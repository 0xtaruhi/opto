// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::{TargetPin, TargetSequential};

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
