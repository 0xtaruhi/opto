// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::{TargetPin, TargetSequential};

#[test]
fn projects_selected_flip_flop_timing_onto_register_boundaries() {
    let mut dff = flip_flop("DFF", 1.0, "CP");
    dff.pins[0].timing_arcs.push(crate::TargetTimingArc {
        related_pin: "CP".to_string(),
        timing_type: TargetTimingType::Check {
            kind: TimingCheckKind::Setup,
            clock_edge: TimingEdge::Rise,
        },
        timing_sense: opto_library::TimingSense::NonUnate,
        delay_model: None,
        rise_constraint: Some(opto_library::LookupTable::scalar(0.2)),
        fall_constraint: Some(opto_library::LookupTable::scalar(0.15)),
    });
    dff.pins[2].timing_arcs.push(crate::TargetTimingArc {
        related_pin: "CP".to_string(),
        timing_type: TargetTimingType::ClockToQ(TimingEdge::Rise),
        timing_sense: opto_library::TimingSense::PositiveUnate,
        delay_model: Some(opto_library::ArcDelayModel::Nldm(
            opto_library::NldmTimingModel::new(
                Some(opto_library::LookupTable::scalar(0.3)),
                Some(opto_library::LookupTable::scalar(0.25)),
                Some(opto_library::LookupTable::scalar(0.04)),
                Some(opto_library::LookupTable::scalar(0.03)),
            ),
        )),
        rise_constraint: None,
        fall_constraint: None,
    });
    let catalog = SequentialCellCatalog::new(&SynthesisOptions {
        target_cells: vec![dff].into(),
    });
    let combinational = crate::mapping::library::CombinationalCellCatalog::default();
    let mut module = word::WordModule::new("top");
    let bit = word::WordType::bits(1).unwrap();
    let source = word::SourceSpan::default();
    let clock_port = module
        .add_port("clk", word::PortDirection::Input, bit, source.clone())
        .unwrap();
    let output_port = module
        .add_port("q", word::PortDirection::Output, bit, source.clone())
        .unwrap();
    let data = module
        .constant(
            opto_ir::ConstBits::from_bin_str("0").unwrap(),
            bit,
            source.clone(),
        )
        .unwrap();
    let clock = module
        .read_signal(module.port(clock_port).unwrap().signal, source.clone())
        .unwrap();
    let result = module
        .register(
            word::RegisterOp {
                name: None,
                d: data,
                clock,
                edge: word::Edge::Pos,
                enable: None,
                resets: Vec::new(),
            },
            source.clone(),
        )
        .unwrap();
    module
        .connect(
            word::LValue::signal(module.port(output_port).unwrap().signal),
            result,
            source,
        )
        .unwrap();

    let projection = SequentialTimingProjection::build(&module, &catalog, &combinational).unwrap();
    assert_eq!(projection.clock_to_q(result), Some(0.3));
    assert_eq!(projection.output_transition(result), Some(0.04));
    assert_eq!(projection.setup(result), Some(0.2));
    let uncharacterized = SequentialCellCatalog::new(&SynthesisOptions {
        target_cells: vec![flip_flop("DFF", 1.0, "CP")].into(),
    });
    let absent =
        SequentialTimingProjection::build(&module, &uncharacterized, &combinational).unwrap();
    assert_eq!(absent.clock_to_q(result), None);
    assert_eq!(absent.output_transition(result), None);
    assert_eq!(absent.setup(result), None);

    let clock_id = opto_timing::PortId::from_uid(opto_core::ObjectUid::from_raw(2).unwrap());
    let output_id = opto_timing::PortId::from_uid(opto_core::ObjectUid::from_raw(3).unwrap());
    let mut timing = opto_timing::TimingContext::new();
    timing
        .create_clock(
            opto_timing::ClockId::from_uid(opto_core::ObjectUid::from_raw(4).unwrap()),
            opto_timing::ClockSpec::new("clk", 1.0, vec![clock_id], None).unwrap(),
        )
        .unwrap();
    let roots = crate::mapping::roots::mapping_roots(
        &module,
        &timing,
        &opto_timing::PortBindings::new([clock_id, output_id]),
        Some(&projection),
    )
    .unwrap();
    assert_eq!(
        roots
            .iter()
            .find(|root| root.value == data)
            .and_then(|root| root.required_time),
        Some(0.8)
    );
}

#[test]
fn recognizes_enable_flip_flops_in_either_polarity() {
    let mut active_high = flip_flop("EDFF", 3.0, "CP");
    active_high
        .pins
        .insert(1, pin("E", TargetPinDirection::Input, None));
    active_high.sequential[0].next_state =
        Some(crate::BooleanFunction::parse("(D*E)+(IQ*!E)").unwrap());
    let mut active_low = flip_flop("EDFFN", 3.5, "CP");
    active_low
        .pins
        .insert(1, pin("EN", TargetPinDirection::Input, None));
    active_low.sequential[0].next_state =
        Some(crate::BooleanFunction::parse("(D*!EN)+(IQ*EN)").unwrap());
    let options = SynthesisOptions {
        target_cells: vec![active_high, active_low].into(),
    };
    let catalog = SequentialCellCatalog::new(&options);

    assert!(catalog.has_enable_cell(word::Edge::Pos, &[]));
    assert!(!catalog.has_enable_cell(word::Edge::Neg, &[]));
    let high = catalog
        .best_enable(word::Edge::Pos, &[], true, false, None)
        .unwrap();
    assert_eq!(high.cell_name, "EDFF");
    assert!(high.enable_active_high());
    let low = catalog
        .best_enable(word::Edge::Pos, &[], false, false, None)
        .unwrap();
    assert_eq!(low.cell_name, "EDFFN");
    assert!(!low.enable_active_high());
}

#[test]
fn timing_projection_ignores_word_state_before_bit_lowering() {
    let mut dff = flip_flop("DFFR", 1.0, "CP");
    dff.pins.push(pin("R", TargetPinDirection::Input, None));
    dff.sequential[0].clear = Some(crate::BooleanFunction::parse("R").unwrap());
    let options = SynthesisOptions {
        target_cells: vec![dff].into(),
    };
    let catalog = SequentialCellCatalog::new(&options);
    let mut module = word::WordModule::new("top");
    let vector = word::WordType::bits(4).unwrap();
    let source = word::SourceSpan::default();
    let clock = module
        .add_wire("clk", word::WordType::bits(1).unwrap(), source.clone())
        .unwrap();
    let reset = module
        .add_wire("reset", word::WordType::bits(1).unwrap(), source.clone())
        .unwrap();
    let data = module.add_wire("d", vector, source.clone()).unwrap();
    let output = module
        .add_port("q", word::PortDirection::Output, vector, source.clone())
        .unwrap();
    let [clock, reset, data] =
        [clock, reset, data].map(|signal| module.read_signal(signal, source.clone()).unwrap());
    let zero = module
        .constant(
            opto_ir::ConstBits::from_bits(vec![opto_ir::BitVal::Zero; 4]).unwrap(),
            vector,
            source.clone(),
        )
        .unwrap();
    let result = module
        .register(
            word::RegisterOp {
                name: None,
                d: data,
                clock,
                edge: word::Edge::Pos,
                enable: None,
                resets: vec![word::Reset {
                    kind: word::ResetKind::Async,
                    value: reset,
                    active_high: true,
                    reset_value: zero,
                }],
            },
            source,
        )
        .unwrap();
    module
        .connect(
            word::LValue::signal(module.port(output).unwrap().signal),
            result,
            word::SourceSpan::default(),
        )
        .unwrap();

    let projection = SequentialTimingProjection::build(
        &module,
        &catalog,
        &crate::mapping::library::CombinationalCellCatalog::default(),
    )
    .unwrap();
    assert_eq!(projection.clock_to_q(result), None);
}

#[test]
fn indexes_smallest_simple_flip_flop_by_edge() {
    let options = SynthesisOptions {
        target_cells: vec![
            flip_flop("Z_DFF", 2.0, "CP"),
            flip_flop("A_DFF", 1.0, "CP"),
            flip_flop("N_DFF", 1.5, "(!CPN)"),
        ]
        .into(),
    };
    let catalog = SequentialCellCatalog::new(&options);

    assert_eq!(
        catalog
            .best(word::Edge::Pos, &[], false, None)
            .unwrap()
            .cell_name,
        "A_DFF"
    );
    assert_eq!(
        catalog
            .best(word::Edge::Neg, &[], false, None)
            .unwrap()
            .cell_name,
        "N_DFF"
    );
}

#[test]
fn indexes_and_connects_active_low_asynchronous_clear() {
    let mut cell = flip_flop("DFFRN", 1.25, "CP");
    cell.pins.push(pin("CDN", TargetPinDirection::Input, None));
    cell.sequential[0].clear = Some(crate::BooleanFunction::parse("!CDN").unwrap());
    let options = SynthesisOptions {
        target_cells: vec![cell].into(),
    };
    let catalog = SequentialCellCatalog::new(&options);
    let request = AsyncResetRequest {
        active_high: false,
        reset_value: false,
    };
    let cell = catalog
        .best(word::Edge::Pos, &[request], false, None)
        .unwrap();
    let mapped = cell.mapped_cell(
        word::ValueId::from_index(1).unwrap(),
        word::ValueId::from_index(2).unwrap(),
        &[word::ValueId::from_index(3).unwrap()],
        word::ValueId::from_index(4).unwrap(),
        None,
    );

    assert_eq!(cell.cell_name, "DFFRN");
    assert!(mapped.input_connections.iter().any(|connection| {
        connection.pin == "CDN" && connection.value == word::ValueId::from_index(3).unwrap()
    }));
}

#[test]
fn indexes_and_connects_independent_clear_and_preset() {
    let mut cell = flip_flop("DFFSR", 1.75, "CP");
    cell.pins.push(pin("CLR", TargetPinDirection::Input, None));
    cell.pins.push(pin("PREN", TargetPinDirection::Input, None));
    cell.sequential[0].clear = Some(crate::BooleanFunction::parse("CLR").unwrap());
    cell.sequential[0].preset = Some(crate::BooleanFunction::parse("!PREN").unwrap());
    let catalog = SequentialCellCatalog::new(&SynthesisOptions {
        target_cells: vec![cell].into(),
    });
    let requests = [
        AsyncResetRequest {
            active_high: true,
            reset_value: false,
        },
        AsyncResetRequest {
            active_high: false,
            reset_value: true,
        },
    ];

    let cell = catalog
        .best(word::Edge::Pos, &requests, false, None)
        .unwrap();
    let controls = [
        word::ValueId::from_index(3).unwrap(),
        word::ValueId::from_index(4).unwrap(),
    ];
    let mapped = cell.mapped_cell(
        word::ValueId::from_index(1).unwrap(),
        word::ValueId::from_index(2).unwrap(),
        &controls,
        word::ValueId::from_index(5).unwrap(),
        None,
    );

    assert_eq!(cell.cell_name, "DFFSR");
    assert!(
        mapped
            .input_connections
            .iter()
            .any(|connection| { connection.pin == "CLR" && connection.value == controls[0] })
    );
    assert!(
        mapped
            .input_connections
            .iter()
            .any(|connection| { connection.pin == "PREN" && connection.value == controls[1] })
    );
}

#[test]
fn excludes_scan_only_next_state_pins_from_functional_mapping() {
    let mut cell = flip_flop("SCAN_DFF", 1.0, "CP");
    cell.pins.push(pin("SE", TargetPinDirection::Input, None));
    cell.pins.last_mut().unwrap().next_state_type = Some(crate::TargetNextStateType::ScanEnable);
    cell.pins.push(pin("SI", TargetPinDirection::Input, None));
    cell.pins.last_mut().unwrap().next_state_type = Some(crate::TargetNextStateType::ScanIn);
    cell.sequential[0].next_state =
        Some(crate::BooleanFunction::parse("(SE SI) + (!SE D)").unwrap());

    let catalog = SequentialCellCatalog::new(&SynthesisOptions {
        target_cells: vec![cell].into(),
    });

    assert!(catalog.cells.is_empty());
}

#[test]
fn unconstrained_sequential_selection_prefers_area() {
    let options = SynthesisOptions {
        target_cells: vec![
            flip_flop("SMALL_DFF", 1.0, "CP"),
            flip_flop("FAST_DFF", 2.0, "CP"),
        ]
        .into(),
    };
    let mut catalog = SequentialCellCatalog::new(&options);
    catalog
        .cells
        .iter_mut()
        .find(|cell| cell.cell_name == "SMALL_DFF")
        .unwrap()
        .cost
        .delay = 2.0;
    catalog
        .cells
        .iter_mut()
        .find(|cell| cell.cell_name == "FAST_DFF")
        .unwrap()
        .cost
        .delay = 1.0;

    let area = catalog.best(word::Edge::Pos, &[], false, None).unwrap();
    let repeated = catalog.best(word::Edge::Pos, &[], false, None).unwrap();
    assert_eq!(area.cell_name, "SMALL_DFF");
    assert_eq!(repeated.cell_name, area.cell_name);
}

#[test]
fn excludes_special_purpose_sequential_cells() {
    let mut isolation = flip_flop("ISO_LATCH", 0.5, "CP");
    isolation.usage = opto_library::TargetCellUsage::ISOLATION;
    let catalog = SequentialCellCatalog::new(&SynthesisOptions {
        target_cells: vec![isolation].into(),
    });

    assert!(catalog.cells.is_empty());
    assert!(catalog.enable_cells.is_empty());
    assert!(catalog.latch_cells.is_empty());
}

fn flip_flop(name: &str, area: f64, clocked_on: &str) -> TargetCell {
    let clock_pin = if clocked_on.contains("CPN") {
        "CPN"
    } else {
        "CP"
    };
    TargetCell {
        dont_use: false,
        usage: opto_library::TargetCellUsage::default(),
        name: name.to_string(),
        area: Some(area),
        pins: vec![
            pin("D", TargetPinDirection::Input, None),
            pin(clock_pin, TargetPinDirection::Input, None),
            pin("Q", TargetPinDirection::Output, Some("IQ")),
        ],
        sequential: vec![TargetSequential {
            kind: TargetSequentialKind::FlipFlop,
            state_variables: vec!["IQ".to_string(), "IQN".to_string()],
            clocked_on: Some(crate::BooleanFunction::parse(clocked_on).unwrap()),
            next_state: Some(crate::BooleanFunction::parse("D").unwrap()),
            enable: None,
            clear: None,
            preset: None,
        }],
        clock_gate: None,
        memory: None,
    }
}

fn pin(name: &str, direction: TargetPinDirection, function: Option<&str>) -> TargetPin {
    TargetPin {
        name: name.to_string(),
        direction,
        function: function.map(|function| crate::BooleanFunction::parse(function).unwrap()),
        three_state: None,
        capacitance: None,
        rise_capacitance: None,
        fall_capacitance: None,
        receiver_capacitance: None,
        fanout_load: None,
        next_state_type: None,
        timing_arcs: Vec::new(),
        clock_gate_role: None,
    }
}
