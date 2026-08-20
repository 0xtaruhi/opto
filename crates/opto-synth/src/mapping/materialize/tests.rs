// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn cell(
    name: &str,
    dont_use: bool,
    usage: opto_library::TargetCellUsage,
    output_pin: bool,
) -> opto_library::TargetCell {
    opto_library::TargetCell {
        name: name.to_string(),
        area: Some(1.0),
        dont_use,
        usage,
        pins: output_pin
            .then(|| opto_library::TargetPin {
                name: "Y".to_string(),
                direction: opto_library::TargetPinDirection::Output,
                function: None,
                three_state: None,
                capacitance: None,
                rise_capacitance: None,
                fall_capacitance: None,
                receiver_capacitance: None,
                fanout_load: None,
                next_state_type: None,
                timing_arcs: Vec::new(),
                clock_gate_role: None,
            })
            .into_iter()
            .collect(),
        sequential: Vec::new(),
        clock_gate: None,
        memory: None,
    }
}

fn buffer_cell() -> opto_library::TargetCell {
    let mut cell = cell("BUF", false, opto_library::TargetCellUsage::default(), true);
    let mut input = cell.pins[0].clone();
    input.name = "A".to_string();
    input.direction = opto_library::TargetPinDirection::Input;
    cell.pins[0].function = Some(opto_library::BooleanFunction::parse("A").unwrap());
    cell.pins.insert(0, input);
    cell
}

fn input_pin(name: &str) -> opto_library::TargetPin {
    let mut pin = cell("", false, opto_library::TargetCellUsage::default(), true)
        .pins
        .pop()
        .unwrap();
    pin.name = name.to_string();
    pin.direction = opto_library::TargetPinDirection::Input;
    pin
}

#[test]
fn sealed_artifact_allows_one_external_producer_with_independent_consumers() {
    let target_cells: opto_library::TargetCellSet = vec![buffer_cell()].into();
    let mut nets = ArtifactNetTable::default();
    let external = nets.signal(region_delta::MappedValueSignal::Net(
        NetId::from_index(0).unwrap(),
    ));
    let produced = nets.claim_output(Some(external)).unwrap();
    let local = nets.allocate_local().unwrap();
    let cells = [
        ArtifactCell {
            name: "producer".to_string(),
            cell_type: "BUF".to_string(),
            library_cell: Some(0),
            connections: vec![
                ("A".to_string(), Some(0), ArtifactSignal::Constant(false)),
                ("Y".to_string(), Some(1), produced),
            ]
            .into_boxed_slice(),
            metadata: (),
        },
        ArtifactCell {
            name: "consumer".to_string(),
            cell_type: "BUF".to_string(),
            library_cell: Some(0),
            connections: vec![
                ("A".to_string(), Some(0), external),
                ("Y".to_string(), Some(1), local),
            ]
            .into_boxed_slice(),
            metadata: (),
        },
    ];

    validate_artifact_nets("test artifact", &nets, &cells, &target_cells).unwrap();
}

#[test]
fn sealed_artifact_rejects_a_bit_level_external_feedback_cycle() {
    let target_cells: opto_library::TargetCellSet = vec![buffer_cell()].into();
    let mut nets = ArtifactNetTable::default();
    let external = nets.signal(region_delta::MappedValueSignal::Net(
        NetId::from_index(0).unwrap(),
    ));
    let produced = nets.claim_output(Some(external)).unwrap();
    let cells = [ArtifactCell {
        name: "feedback".to_string(),
        cell_type: "BUF".to_string(),
        library_cell: Some(0),
        connections: vec![
            ("A".to_string(), Some(0), external),
            ("Y".to_string(), Some(1), produced),
        ]
        .into_boxed_slice(),
        metadata: (),
    }];

    let error = validate_artifact_nets("test artifact", &nets, &cells, &target_cells).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("physical bit-level combinational cycle"),
        "{error}"
    );
}

fn tri_state_cell(name: &str, area: f64, active_high: bool) -> opto_library::TargetCell {
    let input = |name: &str| opto_library::TargetPin {
        name: name.to_string(),
        direction: opto_library::TargetPinDirection::Input,
        function: None,
        three_state: None,
        capacitance: None,
        rise_capacitance: None,
        fall_capacitance: None,
        receiver_capacitance: None,
        fanout_load: None,
        next_state_type: None,
        timing_arcs: Vec::new(),
        clock_gate_role: None,
    };
    opto_library::TargetCell {
        name: name.to_string(),
        area: Some(area),
        dont_use: false,
        usage: opto_library::TargetCellUsage::default(),
        pins: vec![
            input("A"),
            input("E"),
            opto_library::TargetPin {
                name: "Y".to_string(),
                direction: opto_library::TargetPinDirection::Output,
                function: Some(opto_library::BooleanFunction::parse("A").unwrap()),
                three_state: Some(
                    opto_library::BooleanFunction::parse(if active_high { "!E" } else { "E" })
                        .unwrap(),
                ),
                capacitance: None,
                rise_capacitance: None,
                fall_capacitance: None,
                receiver_capacitance: None,
                fanout_load: None,
                next_state_type: None,
                timing_arcs: Vec::new(),
                clock_gate_role: None,
            },
        ],
        sequential: Vec::new(),
        clock_gate: None,
        memory: None,
    }
}

fn source_provenance(module: &word::WordModule) -> SourceInstanceProvenance {
    SourceInstanceProvenance::capture(module)
}

#[test]
fn lowers_scalar_signedness_casts_as_wire_aliases() {
    let mut module = word::WordModule::new("scalar_cast");
    let signed = word::WordType::new(1, true, word::LogicStateKind::FourState).unwrap();
    let unsigned = word::WordType::new(1, false, word::LogicStateKind::FourState).unwrap();
    let input = module
        .add_port(
            "a",
            word::PortDirection::Input,
            signed,
            word::SourceSpan::default(),
        )
        .unwrap();
    let output = module
        .add_port(
            "y",
            word::PortDirection::Output,
            unsigned,
            word::SourceSpan::default(),
        )
        .unwrap();
    let input = module
        .read_signal(
            module.port(input).unwrap().signal,
            word::SourceSpan::default(),
        )
        .unwrap();
    let cast = module
        .cast(
            word::CastKind::ZeroExtend,
            input,
            unsigned,
            word::SourceSpan::default(),
        )
        .unwrap();
    module
        .connect(
            word::LValue::signal(module.port(output).unwrap().signal),
            cast,
            word::SourceSpan::default(),
        )
        .unwrap();

    let source_instances = source_provenance(&module);
    let mapped = build_test_substrate(
        &module,
        &SynthesisOptions {
            target_cells: opto_library::TargetCellSet::default(),
        },
        &BTreeSet::new(),
        &crate::ReferencePortMap::new(),
        &source_instances,
        opto_ir::RevisionId::INITIAL,
    )
    .unwrap()
    .netlist;

    assert_eq!(mapped.cell_count(), 0);
    let input_net = mapped
        .port_nets(opto_ir::mapped::PortId::from_index(0).unwrap())
        .unwrap()[0];
    let output_net = mapped
        .port_nets(opto_ir::mapped::PortId::from_index(1).unwrap())
        .unwrap()[0];
    assert_eq!(input_net, output_net);
    validate_observable_drivers(
        &mapped,
        &opto_library::TargetCellSet::default(),
        &crate::ReferencePortMap::new(),
    )
    .unwrap();
}

#[test]
fn publication_rejects_an_undriven_observable_output() {
    let mut module = word::WordModule::new("undriven_output");
    module
        .add_port(
            "y",
            word::PortDirection::Output,
            word::WordType::bits(1).unwrap(),
            word::SourceSpan::default(),
        )
        .unwrap();
    let source_instances = source_provenance(&module);
    let mapped = build_test_substrate(
        &module,
        &SynthesisOptions {
            target_cells: opto_library::TargetCellSet::default(),
        },
        &BTreeSet::new(),
        &crate::ReferencePortMap::new(),
        &source_instances,
        opto_ir::RevisionId::INITIAL,
    )
    .unwrap()
    .netlist;

    let error = validate_observable_drivers(
        &mapped,
        &opto_library::TargetCellSet::default(),
        &crate::ReferencePortMap::new(),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("output 'y[0]' has no physical driver")
    );
}

#[test]
fn publication_rejects_an_undriven_consumed_internal_net() {
    let mut sink = cell(
        "SINK",
        false,
        opto_library::TargetCellUsage::default(),
        false,
    );
    sink.pins.push(input_pin("A"));
    let target_cells: opto_library::TargetCellSet = vec![sink].into();
    let mut builder =
        MappedBuilder::new("undriven_internal", opto_ir::RevisionId::INITIAL).unwrap();
    let net = builder.add_net(Some("dangling")).unwrap();
    builder
        .add_cell(
            "U1",
            "SINK",
            Some(0),
            &[("A".to_string(), Some(0), ConnectionSignal::Net(net))],
        )
        .unwrap();
    let mapped = builder.freeze().unwrap();

    let error =
        validate_observable_drivers(&mapped, &target_cells, &crate::ReferencePortMap::new())
            .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("is consumed but has no physical driver"),
        "{error}"
    );
}

#[test]
fn publication_rejects_a_multiply_driven_consumed_internal_net() {
    let mut sink = cell(
        "SINK",
        false,
        opto_library::TargetCellUsage::default(),
        false,
    );
    sink.pins.push(input_pin("A"));
    let driver = cell(
        "DRIVER",
        false,
        opto_library::TargetCellUsage::default(),
        true,
    );
    let target_cells: opto_library::TargetCellSet = vec![sink, driver].into();
    let mut builder =
        MappedBuilder::new("multiply_driven_internal", opto_ir::RevisionId::INITIAL).unwrap();
    let net = builder.add_net(Some("contended")).unwrap();
    for instance in ["U_DRIVER_0", "U_DRIVER_1"] {
        builder
            .add_cell(
                instance,
                "DRIVER",
                Some(1),
                &[("Y".to_string(), Some(0), ConnectionSignal::Net(net))],
            )
            .unwrap();
    }
    builder
        .add_cell(
            "U_SINK",
            "SINK",
            Some(0),
            &[("A".to_string(), Some(0), ConnectionSignal::Net(net))],
        )
        .unwrap();
    let mapped = builder.freeze().unwrap();

    let error =
        validate_observable_drivers(&mapped, &target_cells, &crate::ReferencePortMap::new())
            .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("is consumed but has 2 physical drivers"),
        "{error}"
    );
}

#[test]
fn materializes_a_tri_state_boundary_with_the_smallest_compatible_cell() {
    let mut module = word::WordModule::new("tri_state_boundary");
    let bit = word::WordType::bits(1).unwrap();
    let data_port = module
        .add_port(
            "data",
            word::PortDirection::Input,
            bit,
            word::SourceSpan::default(),
        )
        .unwrap();
    let enable_port = module
        .add_port(
            "enable",
            word::PortDirection::Input,
            bit,
            word::SourceSpan::default(),
        )
        .unwrap();
    let pad_port = module
        .add_port(
            "pad",
            word::PortDirection::Inout,
            bit,
            word::SourceSpan::default(),
        )
        .unwrap();
    let observed_port = module
        .add_port(
            "observed",
            word::PortDirection::Output,
            bit,
            word::SourceSpan::default(),
        )
        .unwrap();
    let pad = module.port(pad_port).unwrap().signal;
    module
        .set_signal_resolution(pad, word::SignalResolution::TriState)
        .unwrap();
    let data = module
        .read_signal(
            module.port(data_port).unwrap().signal,
            word::SourceSpan::default(),
        )
        .unwrap();
    let enable = module
        .read_signal(
            module.port(enable_port).unwrap().signal,
            word::SourceSpan::default(),
        )
        .unwrap();
    let driver = module
        .tri_state(
            data,
            word::Enable {
                value: enable,
                active_high: true,
            },
            word::SourceSpan::default(),
        )
        .unwrap();
    module
        .connect(
            word::LValue::signal(pad),
            driver,
            word::SourceSpan::default(),
        )
        .unwrap();
    let pad_read = module
        .read_signal(pad, word::SourceSpan::default())
        .unwrap();
    module
        .connect(
            word::LValue::signal(module.port(observed_port).unwrap().signal),
            pad_read,
            word::SourceSpan::default(),
        )
        .unwrap();
    let options = SynthesisOptions {
        target_cells: vec![
            tri_state_cell("TBUF_LARGE", 2.0, true),
            tri_state_cell("TBUF_SMALL", 1.0, true),
            tri_state_cell("TBUFN", 0.5, false),
        ]
        .into(),
    };
    let source_instances = source_provenance(&module);

    let mapped = build_test_substrate(
        &module,
        &options,
        &BTreeSet::new(),
        &crate::ReferencePortMap::new(),
        &source_instances,
        opto_ir::RevisionId::INITIAL,
    )
    .unwrap()
    .netlist;

    assert_eq!(mapped.cell_count(), 1);
    let cell = mapped.cell_ids().next().unwrap();
    assert_eq!(mapped.cell_type(cell), Some("TBUF_SMALL"));
    let pad_net = mapped
        .port_nets(opto_ir::mapped::PortId::from_index(2).unwrap())
        .unwrap()[0];
    let output = mapped
        .pin_ids(cell)
        .unwrap()
        .into_iter()
        .find(|&pin| {
            let connection = mapped.connection(pin).unwrap();
            mapped.pin_name(connection) == Some("Y")
        })
        .unwrap();
    assert_eq!(
        mapped.connection(output).unwrap().signal,
        ConnectionSignal::Net(pad_net)
    );
    validate_observable_drivers(
        &mapped,
        &options.target_cells,
        &crate::ReferencePortMap::new(),
    )
    .unwrap();
}

#[test]
fn rejects_a_tri_state_boundary_without_a_polarity_compatible_cell() {
    let mut module = word::WordModule::new("tri_state_missing_cell");
    let bit = word::WordType::bits(1).unwrap();
    let pad_port = module
        .add_port(
            "pad",
            word::PortDirection::Inout,
            bit,
            word::SourceSpan::default(),
        )
        .unwrap();
    let pad = module.port(pad_port).unwrap().signal;
    module
        .set_signal_resolution(pad, word::SignalResolution::TriState)
        .unwrap();
    let data = module
        .constant(
            opto_ir::ConstBits::from_bin_str("1").unwrap(),
            bit,
            word::SourceSpan::default(),
        )
        .unwrap();
    let enable = module
        .constant(
            opto_ir::ConstBits::from_bin_str("1").unwrap(),
            bit,
            word::SourceSpan::default(),
        )
        .unwrap();
    let driver = module
        .tri_state(
            data,
            word::Enable {
                value: enable,
                active_high: true,
            },
            word::SourceSpan::default(),
        )
        .unwrap();
    module
        .connect(
            word::LValue::signal(pad),
            driver,
            word::SourceSpan::default(),
        )
        .unwrap();
    let options = SynthesisOptions {
        target_cells: vec![tri_state_cell("TBUFN", 1.0, false)].into(),
    };
    let source_instances = source_provenance(&module);

    let error = build_test_substrate(
        &module,
        &options,
        &BTreeSet::new(),
        &crate::ReferencePortMap::new(),
        &source_instances,
        opto_ir::RevisionId::INITIAL,
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("no compatible active-high tri-state buffer"),
        "{error}"
    );
}

#[test]
fn publication_resolves_a_target_output_pin_by_name_without_a_dense_pin_id() {
    let target_cells: opto_library::TargetCellSet = vec![cell(
        "BUF",
        false,
        opto_library::TargetCellUsage::default(),
        true,
    )]
    .into();
    let mut builder =
        MappedBuilder::new("pin_name_fallback", opto_ir::RevisionId::INITIAL).unwrap();
    let net = builder.add_net(Some("y")).unwrap();
    builder
        .add_port("y", PortDirection::Output, &[net])
        .unwrap();
    builder
        .add_cell(
            "U1",
            "BUF",
            Some(0),
            &[("Y".to_string(), None, ConnectionSignal::Net(net))],
        )
        .unwrap();
    let mapped = builder.freeze().unwrap();

    validate_observable_drivers(&mapped, &target_cells, &crate::ReferencePortMap::new()).unwrap();
}

#[test]
fn frozen_connectivity_rejects_a_postmap_edit_that_orphans_an_output() {
    let target_cells: opto_library::TargetCellSet = vec![cell(
        "BUF",
        false,
        opto_library::TargetCellUsage::default(),
        true,
    )]
    .into();
    let mut builder = MappedBuilder::new("frozen_output", opto_ir::RevisionId::INITIAL).unwrap();
    let net = builder.add_net(Some("y")).unwrap();
    builder
        .add_port("y", PortDirection::Output, &[net])
        .unwrap();
    let driver = builder
        .add_cell(
            "U1",
            "BUF",
            Some(0),
            &[("Y".to_string(), None, ConnectionSignal::Net(net))],
        )
        .unwrap();
    let mut mapped = builder.freeze().unwrap();
    let connectivity = FrozenObservableConnectivity::capture(
        &mapped,
        &target_cells,
        &crate::ReferencePortMap::new(),
    )
    .unwrap();
    let snapshot = mapped.snapshot_region([driver], [net]).unwrap();
    let mut delta = opto_ir::mapped::RegionDelta::new(snapshot);
    delta.remove_cell(driver).unwrap();
    let edit = mapped.apply_region_delta(delta).unwrap();

    assert!(
        !connectivity
            .preserves_affected(&mapped, &target_cells, edit.affected_nets())
            .unwrap()
    );
    mapped.rollback_region_delta(edit).unwrap();
    connectivity.validate(&mapped, &target_cells).unwrap();
}

#[test]
fn materialization_preserves_preexisting_special_and_dont_use_cells() {
    let mut module = word::WordModule::new("top");
    let bit = word::WordType::bits(1).unwrap();
    let iso_net = module
        .add_wire("iso_unused", bit, word::SourceSpan::default())
        .unwrap();
    let dont_use_net = module
        .add_wire("dont_use_unused", bit, word::SourceSpan::default())
        .unwrap();
    let iso_output = module
        .read_signal(iso_net, word::SourceSpan::default())
        .unwrap();
    let dont_use_output = module
        .read_signal(dont_use_net, word::SourceSpan::default())
        .unwrap();
    module
        .add_instance(
            "u_iso",
            "ISO",
            vec![("Y".to_string(), iso_output, word::SourceSpan::default())],
            word::SourceSpan::default(),
        )
        .unwrap();
    module
        .add_instance(
            "u_dont_use",
            "DONT_USE",
            vec![(
                "Y".to_string(),
                dont_use_output,
                word::SourceSpan::default(),
            )],
            word::SourceSpan::default(),
        )
        .unwrap();
    let options = SynthesisOptions {
        target_cells: vec![
            cell("ISO", false, opto_library::TargetCellUsage::ISOLATION, true),
            cell(
                "DONT_USE",
                true,
                opto_library::TargetCellUsage::default(),
                true,
            ),
        ]
        .into(),
    };
    let references = crate::target_cell_reference_ports(&options.target_cells);
    let source_instances = source_provenance(&module);

    let mapped = build_test_substrate(
        &module,
        &options,
        &BTreeSet::new(),
        &references,
        &source_instances,
        opto_ir::RevisionId::INITIAL,
    )
    .unwrap()
    .netlist;

    assert_eq!(mapped.cell_count(), 2);
    assert_eq!(
        mapped
            .cell_ids()
            .map(|id| mapped.cell_type(id).unwrap())
            .collect::<Vec<_>>(),
        ["ISO", "DONT_USE"]
    );
}

#[test]
fn source_link_only_cell_is_preserved_and_linked_by_timing_name() {
    let mut module = word::WordModule::new("top");
    let bit = word::WordType::bits(1).unwrap();
    let input = module
        .add_wire("macro_input", bit, word::SourceSpan::default())
        .unwrap();
    let output = module
        .add_wire("macro_output", bit, word::SourceSpan::default())
        .unwrap();
    let input = module
        .read_signal(input, word::SourceSpan::default())
        .unwrap();
    let output = module
        .read_signal(output, word::SourceSpan::default())
        .unwrap();
    module
        .add_instance(
            "u_macro",
            "MACRO",
            vec![
                ("A".to_string(), input, word::SourceSpan::default()),
                ("Y".to_string(), output, word::SourceSpan::default()),
            ],
            word::SourceSpan::default(),
        )
        .unwrap();
    let target = cell(
        "BUF",
        false,
        opto_library::TargetCellUsage::default(),
        false,
    );
    let mut link = cell(
        "MACRO",
        false,
        opto_library::TargetCellUsage::default(),
        true,
    );
    link.pins.insert(
        0,
        opto_library::TargetPin {
            name: "A".to_string(),
            direction: opto_library::TargetPinDirection::Input,
            function: None,
            three_state: None,
            capacitance: None,
            rise_capacitance: None,
            fall_capacitance: None,
            receiver_capacitance: None,
            fanout_load: None,
            next_state_type: None,
            timing_arcs: Vec::new(),
            clock_gate_role: None,
        },
    );
    link.pins[1].function = Some(opto_library::BooleanFunction::parse("A").unwrap());
    link.pins[1]
        .timing_arcs
        .push(opto_library::TargetTimingArc {
            related_pin: "A".to_string(),
            timing_type: opto_library::TargetTimingType::Combinational,
            timing_sense: opto_library::TimingSense::PositiveUnate,
            delay_model: None,
            rise_constraint: None,
            fall_constraint: None,
        });
    let options = SynthesisOptions {
        target_cells: vec![target].into(),
    };
    let link_cells: opto_library::TargetCellSet = vec![link].into();
    let references = crate::target_cell_reference_ports(&link_cells);
    let source_instances = source_provenance(&module);

    let mapped = build_test_substrate(
        &module,
        &options,
        &BTreeSet::new(),
        &references,
        &source_instances,
        opto_ir::RevisionId::INITIAL,
    )
    .unwrap()
    .netlist;

    let mapped_cell = mapped.cell_ids().next().unwrap();
    assert_eq!(mapped.cell_type(mapped_cell), Some("MACRO"));
    assert_eq!(mapped.cell(mapped_cell).unwrap().library_cell, None);
    let timing = opto_timing::TimingModel::from_mapped(
        &mapped,
        opto_timing::DesignId::from_uid(
            opto_core::ObjectUid::from_raw(1).expect("test design ID is nonzero"),
        ),
        &opto_timing::PortBindings::new([]),
        opto_timing::TimingLibrary {
            cells: link_cells,
            ..opto_timing::TimingLibrary::default()
        },
    )
    .unwrap();
    assert_eq!(
        timing
            .instance_library_cell_id(opto_timing::TimingInstanceId::from_raw(0))
            .map(opto_timing::LibraryCellId::raw),
        Some(0)
    );
}

#[test]
fn unknown_source_cell_is_rejected_even_when_mapping_library_is_empty() {
    let mut module = word::WordModule::new("top");
    module
        .add_instance(
            "u_unknown",
            "UNKNOWN",
            Vec::new(),
            word::SourceSpan::default(),
        )
        .unwrap();
    let source_instances = source_provenance(&module);

    let error = build_test_substrate(
        &module,
        &SynthesisOptions {
            target_cells: opto_library::TargetCellSet::default(),
        },
        &BTreeSet::new(),
        &crate::ReferencePortMap::new(),
        &source_instances,
        opto_ir::RevisionId::INITIAL,
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("unknown resolution-library cell 'UNKNOWN'")
    );
}

#[test]
fn generated_link_only_and_special_cells_are_rejected() {
    for (cell_type, target_cells, link_cells) in [
        (
            "MACRO",
            vec![cell(
                "BUF",
                false,
                opto_library::TargetCellUsage::default(),
                false,
            )],
            vec![cell(
                "MACRO",
                false,
                opto_library::TargetCellUsage::default(),
                false,
            )],
        ),
        (
            "ISO",
            vec![cell(
                "ISO",
                false,
                opto_library::TargetCellUsage::ISOLATION,
                false,
            )],
            vec![cell(
                "ISO",
                false,
                opto_library::TargetCellUsage::ISOLATION,
                false,
            )],
        ),
    ] {
        let mut module = word::WordModule::new("top");
        let source_instances = source_provenance(&module);
        module
            .add_instance(
                "u_generated",
                cell_type,
                Vec::new(),
                word::SourceSpan::default(),
            )
            .unwrap();
        let options = SynthesisOptions {
            target_cells: target_cells.into(),
        };
        let link_cells: opto_library::TargetCellSet = link_cells.into();
        let references = crate::target_cell_reference_ports(&link_cells);

        let error = build_test_substrate(
            &module,
            &options,
            &BTreeSet::new(),
            &references,
            &source_instances,
            opto_ir::RevisionId::INITIAL,
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("outside the eligible target-cell set"),
            "{error}"
        );
    }
}
