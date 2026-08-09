// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn assert_close(actual: f64, expected: f64) {
    let scale = actual.abs().max(expected.abs()).max(1.0);
    assert!(
        (actual - expected).abs() <= 1.0e-12 * scale,
        "expected {expected}, got {actual}"
    );
}

fn input(name: &str, capacitance: f64) -> TargetPin {
    TargetPin {
        name: name.to_string(),
        direction: TargetPinDirection::Input,
        function: None,
        three_state: None,
        capacitance: Some(capacitance),
        rise_capacitance: None,
        fall_capacitance: None,
        receiver_capacitance: None,
        fanout_load: None,
        next_state_type: None,
        timing_arcs: Vec::new(),
        clock_gate_role: None,
    }
}

#[test]
fn mapping_reference_load_is_fo4_per_input() {
    let cells = opto_library::TargetCellSet::from(vec![TargetCell {
        name: "LOADS".to_string(),
        area: None,
        dont_use: false,
        usage: opto_library::TargetCellUsage::default(),
        pins: vec![input("A", 0.002), input("B", 0.004)],
        sequential: Vec::new(),
        clock_gate: None,
        memory: None,
    }]);
    let pins = cells
        .get(0)
        .expect("fixture contains one cell")
        .pins()
        .collect::<Vec<_>>();
    assert_close(reference_fanout_load(&pins), 0.012);
}

#[test]
fn binding_frontier_keeps_tradeoffs_and_removes_dominated_costs() {
    let cost = |area, delay, transition, input_capacitance| CellCost {
        area,
        delay,
        transition,
        input_capacitance,
    };
    let balanced = cost(2.0, 2.0, 2.0, 2.0);

    assert!(cell_cost_dominates(
        cost(1.0, 2.0, 2.0, 2.0),
        balanced,
        false
    ));
    assert!(!cell_cost_dominates(
        cost(1.0, 3.0, 2.0, 2.0),
        balanced,
        false
    ));
    assert!(cell_cost_dominates(balanced, balanced, true));
    assert!(!cell_cost_dominates(balanced, balanced, false));
}

#[test]
fn representative_cost_requires_finite_area_and_delay() {
    let characterized = CellCost {
        area: 2.0,
        delay: 3.0,
        transition: 4.0,
        input_capacitance: 5.0,
    };
    assert!(has_finite_area_delay(characterized));
    assert!(!has_finite_area_delay(CellCost {
        delay: f64::INFINITY,
        ..characterized
    }));
    assert!(!has_finite_area_delay(CellCost {
        area: -1.0,
        ..characterized
    }));
}

#[test]
fn combinational_catalog_excludes_every_forbidden_cell_class() {
    let base = TargetCell {
        name: "GENERAL".to_string(),
        area: Some(1.0),
        dont_use: false,
        usage: opto_library::TargetCellUsage::default(),
        sequential: Vec::new(),
        pins: vec![
            input("A", 1.0),
            TargetPin {
                name: "Y".to_string(),
                direction: TargetPinDirection::Output,
                function: Some(BooleanFunction::parse("A").unwrap()),
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
        ],
        clock_gate: None,
        memory: None,
    };
    let mut dont_use = base.clone();
    dont_use.name = "DONT_USE".to_string();
    dont_use.dont_use = true;
    let mut isolation = base.clone();
    isolation.name = "ISOLATION".to_string();
    isolation.usage = opto_library::TargetCellUsage::ISOLATION;
    let mut level_shifter = base.clone();
    level_shifter.name = "LEVEL_SHIFTER".to_string();
    level_shifter.usage = opto_library::TargetCellUsage::LEVEL_SHIFTER;
    let mut clock_gate = base.clone();
    clock_gate.name = "CLOCK_GATE".to_string();
    clock_gate.usage = opto_library::TargetCellUsage::INTEGRATED_CLOCK_GATING;
    let catalog = CombinationalCellCatalog::new(
        &SynthesisOptions {
            target_cells: vec![base, dont_use, isolation, level_shifter, clock_gate].into(),
        },
        crate::SynthesisDiagnostics::default(),
    );

    assert_eq!(catalog.templates.len(), 1);
    assert_eq!(catalog.templates[0].cell_name, "GENERAL");
}

#[test]
fn unconstrained_catalog_selection_prefers_area() {
    use opto_library::{
        ArcDelayModel, LookupTable, NldmTimingModel, TargetTimingType, TimingSense,
    };

    let cell = |name: &str, area: f64, delay: f64| TargetCell {
        name: name.to_string(),
        area: Some(area),
        dont_use: false,
        usage: opto_library::TargetCellUsage::default(),
        sequential: Vec::new(),
        pins: vec![
            input("A", 1.0),
            TargetPin {
                name: "Y".to_string(),
                direction: TargetPinDirection::Output,
                function: Some(BooleanFunction::parse("A").unwrap()),
                three_state: None,
                capacitance: None,
                rise_capacitance: None,
                fall_capacitance: None,
                receiver_capacitance: None,
                fanout_load: None,
                next_state_type: None,
                timing_arcs: vec![crate::TargetTimingArc {
                    related_pin: "A".to_string(),
                    timing_type: TargetTimingType::Combinational,
                    timing_sense: TimingSense::PositiveUnate,
                    delay_model: Some(ArcDelayModel::Nldm(NldmTimingModel::new(
                        Some(LookupTable::scalar(delay)),
                        Some(LookupTable::scalar(delay)),
                        Some(LookupTable::scalar(delay)),
                        Some(LookupTable::scalar(delay)),
                    ))),
                    rise_constraint: None,
                    fall_constraint: None,
                }],
                clock_gate_role: None,
            },
        ],
        clock_gate: None,
        memory: None,
    };
    let catalog = CombinationalCellCatalog::new(
        &SynthesisOptions {
            target_cells: vec![cell("SMALL", 1.0, 2.0), cell("FAST", 2.0, 1.0)].into(),
        },
        crate::SynthesisDiagnostics::default(),
    );
    let truth = TruthTable {
        input_count: 1,
        bits: 0b10,
    };
    let area = catalog.best_binding_for_truth(truth).unwrap();
    let repeated = catalog.best_binding_for_truth(truth).unwrap();
    assert_close(catalog.cost_for_binding(area).area, 1.0);
    assert_eq!(repeated, area);
}

#[test]
fn permutation_table_covers_each_input_order() {
    let permutations = permutation_table();
    let orders = permutations
        .for_len(4)
        .iter()
        .map(|permutation| permutation.values[..4].to_vec())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(orders.len(), 24);
}

#[test]
fn every_binding_reproduces_its_indexed_truth_through_pin_wiring() {
    use opto_library::{
        ArcDelayModel, LookupTable, NldmTimingModel, TargetTimingType, TimingSense,
    };

    let cell = TargetCell {
        name: "AO21".to_string(),
        area: Some(2.0),
        dont_use: false,
        usage: opto_library::TargetCellUsage::default(),
        sequential: Vec::new(),
        pins: vec![
            input("A", 1.0),
            input("B", 1.0),
            input("C", 1.0),
            TargetPin {
                name: "Y".to_string(),
                direction: TargetPinDirection::Output,
                function: Some(BooleanFunction::parse("(A B) + C").unwrap()),
                three_state: None,
                capacitance: None,
                rise_capacitance: None,
                fall_capacitance: None,
                receiver_capacitance: None,
                fanout_load: None,
                next_state_type: None,
                timing_arcs: vec![crate::TargetTimingArc {
                    related_pin: "A".to_string(),
                    timing_type: TargetTimingType::Combinational,
                    timing_sense: TimingSense::PositiveUnate,
                    delay_model: Some(ArcDelayModel::Nldm(NldmTimingModel::new(
                        Some(LookupTable::scalar(1.0)),
                        Some(LookupTable::scalar(1.0)),
                        Some(LookupTable::scalar(1.0)),
                        Some(LookupTable::scalar(1.0)),
                    ))),
                    rise_constraint: None,
                    fall_constraint: None,
                }],
                clock_gate_role: None,
            },
        ],
        clock_gate: None,
        memory: None,
    };
    let catalog = CombinationalCellCatalog::new(
        &SynthesisOptions {
            target_cells: vec![cell].into(),
        },
        crate::SynthesisDiagnostics::default(),
    );

    let mut checked = 0usize;
    for truth in catalog.bindings_by_truth.keys().copied() {
        for binding in catalog.matching_bindings(truth) {
            let template = &catalog.templates[binding.template];
            let cell_truth = template.outputs[binding.output].truth;
            for assignment in 0..(1usize << truth.input_count) {
                let extra = binding
                    .inverted_input()
                    .map(|signature| ((assignment >> signature) & 1) ^ 1);
                let mut cell_assignment = 0usize;
                for pin_index in 0..cell_truth.input_count {
                    let signature_index = binding.pin_to_signature.signature_index(pin_index);
                    let bit = if signature_index < truth.input_count {
                        (assignment >> signature_index) & 1
                    } else {
                        extra.expect("extra signature slot requires an inverted input")
                    };
                    cell_assignment |= bit << pin_index;
                }
                assert_eq!(
                    truth.bit(assignment),
                    cell_truth.bit(cell_assignment),
                    "binding for {truth:?} diverges at assignment {assignment:#b}"
                );
            }
            checked += 1;
        }
    }
    assert_eq!(checked, 8);
}
