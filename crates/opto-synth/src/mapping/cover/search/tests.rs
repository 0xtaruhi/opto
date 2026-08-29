// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Minimal target-cover search contracts.
//!
//! These tests own library matching, candidate costs, observability care, and
//! deterministic joint selection. Materialization and post-map repair retain
//! their own artifact and transactional oracles.

use super::*;
use crate::boolean::logic::MAX_MATCH_INPUTS;
use crate::planning::mapping_policy::{compare_cell_cost, compare_mapping_cost_with_required_time};
use crate::{BooleanFunction, SynthesisOptions, TargetCell, TargetPin, TargetPinDirection};

#[test]
fn cover_candidate_has_compact_arena_layout() {
    assert_eq!(std::mem::size_of::<Candidate>(), 16);
}

#[test]
fn output_isolation_rejects_misaligned_costs() {
    let mut cover = LibraryCover {
        cells: Box::new([]),
        outputs: Box::new([LibraryCoverSource::Input(0)]),
        total_area: 0.0,
        output_costs: Box::new([]),
    };

    let error = cover.isolate_outputs(&matcher(Vec::new())).unwrap_err();

    assert!(error.to_string().contains("inconsistent lengths"));
}

#[test]
fn output_isolation_keeps_an_artifact_without_a_buffer_cell() {
    let catalog = matcher(vec![target_cell(
        "INV",
        0.6,
        &[in_pin("A"), ("Y", TargetPinDirection::Output, Some("!A"))],
    )]);
    let mut cover = LibraryCover {
        cells: Box::new([]),
        outputs: Box::new([LibraryCoverSource::Input(0)]),
        total_area: 0.0,
        output_costs: Box::new([MappingCost::zero()]),
    };

    cover.isolate_outputs(&catalog).unwrap();

    assert_eq!(cover.cells.len(), 2);
    assert_eq!(cover.outputs.as_ref(), &[LibraryCoverSource::Cell(1)]);
    assert!(!evaluate(&cover, 0, 0));
    assert!(evaluate(&cover, 0, 1));
}

fn target_cell(
    name: &str,
    area: f64,
    pins: &[(&str, TargetPinDirection, Option<&str>)],
) -> TargetCell {
    TargetCell {
        dont_use: false,
        usage: opto_library::TargetCellUsage::default(),
        name: name.to_string(),
        area: Some(area),
        sequential: Vec::new(),
        pins: pins
            .iter()
            .map(|(name, direction, function)| TargetPin {
                name: (*name).to_string(),
                direction: *direction,
                function: function.map(|function| BooleanFunction::parse(function).unwrap()),
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
            .collect(),
        clock_gate: None,
        memory: None,
    }
}

fn matcher(cells: Vec<TargetCell>) -> CombinationalCellCatalog {
    CombinationalCellCatalog::new(
        &SynthesisOptions {
            target_cells: cells.into(),
        },
        crate::SynthesisDiagnostics::default(),
    )
}

fn in_pin(name: &str) -> (&str, TargetPinDirection, Option<&str>) {
    (name, TargetPinDirection::Input, None)
}

fn timed_and_cell(name: &str, area: f64, delay: f64) -> TargetCell {
    let mut cell = target_cell(
        name,
        area,
        &[
            in_pin("A"),
            in_pin("B"),
            ("Y", TargetPinDirection::Output, Some("A&B")),
        ],
    );
    for pin in &mut cell.pins[..2] {
        pin.capacitance = Some(0.1);
    }
    cell.pins[2].timing_arcs = ["A", "B"]
        .into_iter()
        .map(|related_pin| opto_library::TargetTimingArc {
            related_pin: related_pin.to_string(),
            timing_type: opto_library::TargetTimingType::Combinational,
            timing_sense: opto_library::TimingSense::PositiveUnate,
            delay_model: Some(opto_library::ArcDelayModel::Nldm(
                opto_library::NldmTimingModel::new(
                    Some(opto_library::LookupTable::scalar(delay)),
                    Some(opto_library::LookupTable::scalar(delay)),
                    Some(opto_library::LookupTable::scalar(0.1)),
                    Some(opto_library::LookupTable::scalar(0.1)),
                ),
            )),
            rise_constraint: None,
            fall_constraint: None,
        })
        .collect();
    cell
}

fn cover_area(cover: &LibraryCover, catalog: &CombinationalCellCatalog) -> f64 {
    cover
        .cells
        .iter()
        .map(|cell| match cell.binding {
            LibraryCoverBinding::Single(binding) => catalog.cost_for_binding(binding).area,
            LibraryCoverBinding::Joint(binding) => catalog.joint_cost(binding).area,
        })
        .sum()
}

fn normalized_inverter_pair(collapse_inverters: bool) -> LibraryCover {
    let catalog = matcher(vec![target_cell(
        "INV",
        0.6,
        &[in_pin("A"), ("Y", TargetPinDirection::Output, Some("!A"))],
    )]);
    let binding = catalog.best_binding_for_truth(inverter_truth()).unwrap();
    let cell = |source| LibraryCoverCell {
        second_node: None,
        binding: LibraryCoverBinding::Single(binding),
        binding_identity: catalog.binding_identity(binding).into_boxed_slice(),
        truth: inverter_truth(),
        second_truth: None,
        sources: vec![source].into_boxed_slice(),
    };
    LibraryCover {
        cells: vec![
            cell(LibraryCoverSource::Input(0)),
            cell(LibraryCoverSource::Cell(0)),
            cell(LibraryCoverSource::Input(1)),
        ]
        .into_boxed_slice(),
        outputs: vec![LibraryCoverSource::Cell(1)].into_boxed_slice(),
        total_area: 1.8,
        output_costs: Box::new([]),
    }
    .normalize(collapse_inverters)
    .unwrap()
}

fn cost(area: f64, delay: f64) -> MappingCost {
    MappingCost {
        area,
        delay,
        electrical_delay: delay,
        ..MappingCost::zero()
    }
}

fn exact_choice(area: f64, arrival: f64) -> ExactChoice {
    ExactChoice {
        choice: SlotChoice::Constant(false),
        area,
        arrival,
        truth: TruthTable {
            input_count: 0,
            bits: 0,
        },
        order: (0, 0, 0),
    }
}

#[test]
fn exact_recovery_uses_area_delay_only_for_timing_driven_logic() {
    let smaller_slower = exact_choice(9.0, 1.2);
    let larger_faster = exact_choice(10.0, 1.0);

    assert!(smaller_slower.prefers_over(&larger_faster, false));
    assert!(larger_faster.prefers_over(&smaller_slower, true));
}

#[test]
fn compiled_records_reselect_a_faster_cover_when_required_time_tightens() {
    let mut network = LogicGraph::new();
    let a = network.variable(0).unwrap();
    let b = network.variable(1).unwrap();
    let output = network.and(a, b);
    network.freeze();
    let cuts = CutDatabase::build(&network, MAX_MATCH_INPUTS);
    let truths = CutTruthDatabase::build_parallel(&network, &cuts, crate::test_runtime()).unwrap();
    let catalog = matcher(vec![
        timed_and_cell("SMALL_AND", 1.0, 2.0),
        timed_and_cell("FAST_AND", 2.0, 1.0),
    ]);
    let select = |required_time| {
        cover_logic_network_with_truths(
            &network,
            &cuts,
            &truths,
            &[output],
            &catalog,
            CoverTiming {
                required_times: &[Some(required_time)],
                output_loads: &[Some(0.1)],
                input_transitions: &[Some(0.1), Some(0.1)],
                input_arrivals: &[Some(0.0), Some(0.0)],
            },
            crate::test_runtime(),
        )
        .unwrap()
        .unwrap()
    };

    let loose = select(10.0);
    let tight = select(1.5);
    let selected_name = |cover: &LibraryCover| match cover.cells[0].binding {
        LibraryCoverBinding::Single(binding) => catalog.binding_cell_name(binding),
        LibraryCoverBinding::Joint(_) => panic!("single-output AND must not select a joint cell"),
    };

    assert_eq!(selected_name(&loose), "SMALL_AND");
    assert_eq!(selected_name(&tight), "FAST_AND");
}

#[test]
fn recovery_limit_rejects_a_changing_final_round() {
    assert!(!recovery_converged(RECOVERY_ROUND_LIMIT - 1, 1, 0).unwrap());
    assert!(recovery_converged(RECOVERY_ROUND_LIMIT, 0, 0).unwrap());
    assert!(
        recovery_converged(RECOVERY_ROUND_LIMIT, 0, 1)
            .unwrap_err()
            .to_string()
            .contains("did not converge")
    );
}

#[test]
fn joint_recovery_restores_timing_before_recovering_area() {
    assert!(!joint_replacement_is_preferred(
        false, false, 12.0, 0.8, 10.0, 1.2,
    ));
    assert!(joint_replacement_is_preferred(
        true, false, 12.0, 0.8, 10.0, 1.2,
    ));
    assert!(joint_replacement_is_preferred(
        true, true, 12.0, 0.8, 10.0, 1.2,
    ));
}

#[test]
fn required_time_uses_area_delay_after_both_choices_meet_budget() {
    assert_eq!(
        compare_mapping_cost_with_required_time(1.0, cost(4.0, 0.9), cost(1.0, 1.1)),
        std::cmp::Ordering::Less
    );
    assert_eq!(
        compare_mapping_cost_with_required_time(2.0, cost(1.0, 1.5), cost(4.0, 1.0)),
        std::cmp::Ordering::Less
    );
    assert_eq!(
        compare_mapping_cost_with_required_time(0.5, cost(1.0, 1.5), cost(4.0, 1.0)),
        std::cmp::Ordering::Greater
    );
}

#[test]
fn unconstrained_joint_candidate_selection_prefers_area() {
    let small_slow = CellCost {
        area: 1.0,
        delay: 2.0,
        transition: 1.0,
        input_capacitance: 1.0,
    };
    let large_fast = CellCost {
        area: 2.0,
        delay: 1.0,
        transition: 0.5,
        input_capacitance: 1.0,
    };

    assert!(compare_cell_cost(small_slow, large_fast).is_lt());
}

fn evaluate(cover: &LibraryCover, output: usize, assignment: usize) -> bool {
    let mut values = Vec::with_capacity(cover.cells.len());
    for cell in &cover.cells {
        let mut cell_assignment = 0usize;
        for (input, source) in cell
            .sources
            .iter()
            .copied()
            .take(cell.truth.input_count)
            .enumerate()
        {
            if evaluate_source(&values, source, assignment) {
                cell_assignment |= 1 << input;
            }
        }
        values.push((
            cell.truth.bit(cell_assignment),
            cell.second_truth.map(|second| second.bit(cell_assignment)),
        ));
    }
    evaluate_source(&values, cover.outputs[output], assignment)
}

fn evaluate_source(
    values: &[(bool, Option<bool>)],
    source: LibraryCoverSource,
    assignment: usize,
) -> bool {
    match source {
        LibraryCoverSource::Constant(value) => value,
        LibraryCoverSource::Input(input) => (assignment >> input) & 1 == 1,
        LibraryCoverSource::Cell(cell) => values[cell].0,
        LibraryCoverSource::CellSecond(cell) => {
            values[cell].1.expect("joint cells define both outputs")
        }
    }
}

#[test]
fn ignores_choice_graph_fanout_outside_the_active_root_cone() {
    fn build(with_dead_choices: bool) -> (LogicGraph, LogicNodeId) {
        let mut network = LogicGraph::new();
        let a = network.variable(0).unwrap();
        let b = network.variable(1).unwrap();
        let c = network.variable(2).unwrap();
        let output = network.and(a, b);
        if with_dead_choices {
            let mut dead = output;
            for _ in 0..16 {
                let then_value = network.xor(dead, a);
                let else_value = network.and(dead, b);
                dead = network.mux(c, then_value, else_value);
            }
        }
        network.freeze();
        (network, output)
    }

    let catalog = matcher(vec![
        target_cell(
            "AND2",
            1.0,
            &[
                in_pin("A"),
                in_pin("B"),
                ("Y", TargetPinDirection::Output, Some("A&B")),
            ],
        ),
        target_cell(
            "INV",
            0.6,
            &[in_pin("A"), ("Y", TargetPinDirection::Output, Some("!A"))],
        ),
    ]);
    let runtime = ExecutionContext::new(&opto_runtime::ExecutionConfig { max_threads: 2 }).unwrap();
    let map = |network: &LogicGraph, output| {
        let cuts = CutDatabase::build(network, MAX_MATCH_INPUTS);
        cover_logic_network(
            network,
            &cuts,
            &[output],
            &catalog,
            CoverTiming {
                required_times: &[None],
                output_loads: &[None],
                input_transitions: &[None, None, None],
                input_arrivals: &[None, None, None],
            },
            &runtime,
        )
        .unwrap()
        .unwrap()
    };
    let (baseline, baseline_output) = build(false);
    let (choice_graph, choice_output) = build(true);
    let baseline_cover = map(&baseline, baseline_output);
    let choice_cover = map(&choice_graph, choice_output);

    assert!((choice_cover.total_area - baseline_cover.total_area).abs() < f64::EPSILON);
    assert_eq!(choice_cover.cells.len(), baseline_cover.cells.len());
}

#[test]
fn shares_multi_fanout_cones_instead_of_duplicating() {
    let mut network = LogicGraph::new();
    let a = network.variable(0).unwrap();
    let b = network.variable(1).unwrap();
    let c = network.variable(2).unwrap();
    let d = network.variable(3).unwrap();
    let shared = network.and(a, b);
    let left = network.and(shared, c);
    let right = network.and(shared, d);
    network.freeze();
    let cuts = CutDatabase::build(&network, MAX_MATCH_INPUTS);
    let matcher = matcher(vec![
        target_cell(
            "AND2",
            1.0,
            &[
                in_pin("A"),
                in_pin("B"),
                ("Y", TargetPinDirection::Output, Some("A&B")),
            ],
        ),
        target_cell(
            "AND3",
            1.8,
            &[
                in_pin("A"),
                in_pin("B"),
                in_pin("C"),
                ("Y", TargetPinDirection::Output, Some("A&B&C")),
            ],
        ),
        target_cell(
            "INV",
            0.6,
            &[in_pin("A"), ("Y", TargetPinDirection::Output, Some("!A"))],
        ),
    ]);

    let cover = cover_logic_network(
        &network,
        &cuts,
        &[left, right],
        &matcher,
        CoverTiming {
            required_times: &[None, None],
            output_loads: &[None, None],
            input_transitions: &[],
            input_arrivals: &[],
        },
        crate::test_runtime(),
    )
    .unwrap()
    .unwrap();

    let left_truth = network.truth_table(left, 4);
    let right_truth = network.truth_table(right, 4);
    for assignment in 0..16 {
        assert_eq!(evaluate(&cover, 0, assignment), left_truth.bit(assignment));
        assert_eq!(evaluate(&cover, 1, assignment), right_truth.bit(assignment));
    }
    assert!(
        cover_area(&cover, &matcher) < 3.6 - 1e-9,
        "sharing the two-input cone beats duplicated three-input covers, got {}",
        cover_area(&cover, &matcher)
    );
}

#[test]
fn duplicates_multi_fanout_cones_when_crossing_is_cheaper() {
    let mut network = LogicGraph::new();
    let a = network.variable(0).unwrap();
    let b = network.variable(1).unwrap();
    let c = network.variable(2).unwrap();
    let d = network.variable(3).unwrap();
    let shared = network.and(a, b);
    let left = network.and(shared, c);
    let right = network.and(shared, d);
    network.freeze();
    let cuts = CutDatabase::build(&network, MAX_MATCH_INPUTS);
    let matcher = matcher(vec![
        target_cell(
            "AND2",
            1.0,
            &[
                in_pin("A"),
                in_pin("B"),
                ("Y", TargetPinDirection::Output, Some("A&B")),
            ],
        ),
        target_cell(
            "AND3",
            0.7,
            &[
                in_pin("A"),
                in_pin("B"),
                in_pin("C"),
                ("Y", TargetPinDirection::Output, Some("A&B&C")),
            ],
        ),
        target_cell(
            "INV",
            0.6,
            &[in_pin("A"), ("Y", TargetPinDirection::Output, Some("!A"))],
        ),
    ]);

    let cover = cover_logic_network(
        &network,
        &cuts,
        &[left, right],
        &matcher,
        CoverTiming {
            required_times: &[None, None],
            output_loads: &[None, None],
            input_transitions: &[],
            input_arrivals: &[],
        },
        crate::test_runtime(),
    )
    .unwrap()
    .unwrap();

    let left_truth = network.truth_table(left, 4);
    let right_truth = network.truth_table(right, 4);
    for assignment in 0..16 {
        assert_eq!(evaluate(&cover, 0, assignment), left_truth.bit(assignment));
        assert_eq!(evaluate(&cover, 1, assignment), right_truth.bit(assignment));
    }
    assert_eq!(cover.cells.len(), 2);
    assert!((cover_area(&cover, &matcher) - 1.4).abs() < 1e-9);
}

#[test]
fn merges_observability_cares_across_reconvergent_consumers() {
    let mut network = LogicGraph::new();
    let a = network.variable(0).unwrap();
    let b = network.variable(1).unwrap();
    let shared = network.xor(a, b);
    let left = network.and(shared, a.inverted());
    let right = network.and(shared, b.inverted());
    network.freeze();
    let cuts = CutDatabase::build(&network, MAX_MATCH_INPUTS);
    let exact_only = vec![true; network.node_count()];

    let (cares, exact) = planner::analyze_node_cares(
        &network,
        &cuts,
        shared.index(),
        &[
            u32::try_from(left.index()).unwrap(),
            u32::try_from(right.index()).unwrap(),
        ],
        false,
        &exact_only,
    );
    let cut_index = cuts
        .cuts(shared)
        .iter()
        .position(|cut| cut.leaves() == [a, b])
        .unwrap();
    let care = cares.unwrap()[cut_index] & 0b1111;

    assert_eq!(care, 0b0111);
    assert!(!exact);
}

#[test]
fn normalizes_dead_cells_and_redundant_inverters() {
    let cover = normalized_inverter_pair(true);

    assert!(cover.cells.is_empty());
    assert_eq!(cover.outputs.as_ref(), [LibraryCoverSource::Input(0)]);
}

#[test]
fn retains_inverter_pairs_in_timing_covers() {
    let cover = normalized_inverter_pair(false);

    assert_eq!(cover.cells.len(), 2);
    assert_eq!(cover.outputs.as_ref(), [LibraryCoverSource::Cell(1)]);
}

#[test]
fn absorbs_output_inversions_into_matching_cells() {
    let mut network = LogicGraph::new();
    let a = network.variable(0).unwrap();
    let b = network.variable(1).unwrap();
    let and = network.and(a, b);
    let output = LogicGraph::not(and);
    network.freeze();
    let cuts = CutDatabase::build(&network, MAX_MATCH_INPUTS);
    let matcher = matcher(vec![
        target_cell(
            "AND2",
            1.0,
            &[
                in_pin("A"),
                in_pin("B"),
                ("Y", TargetPinDirection::Output, Some("A&B")),
            ],
        ),
        target_cell(
            "NAND2",
            0.9,
            &[
                in_pin("A"),
                in_pin("B"),
                ("Y", TargetPinDirection::Output, Some("!(A&B)")),
            ],
        ),
        target_cell(
            "INV",
            0.6,
            &[in_pin("A"), ("Y", TargetPinDirection::Output, Some("!A"))],
        ),
    ]);

    let cover = cover_logic_network(
        &network,
        &cuts,
        &[output],
        &matcher,
        CoverTiming {
            required_times: &[None],
            output_loads: &[None],
            input_transitions: &[],
            input_arrivals: &[],
        },
        crate::test_runtime(),
    )
    .unwrap()
    .unwrap();

    let expected = network.truth_table(output, 2);
    for assignment in 0..4 {
        assert_eq!(evaluate(&cover, 0, assignment), expected.bit(assignment));
    }
    assert!(
        cover_area(&cover, &matcher) < 1.6 - 1e-9,
        "the inverted phase should map to a single nand, got {}",
        cover_area(&cover, &matcher)
    );
}

#[test]
fn covers_mixed_gates_with_matching_truth_tables() {
    let mut network = LogicGraph::new();
    let s = network.variable(0).unwrap();
    let a = network.variable(1).unwrap();
    let b = network.variable(2).unwrap();
    let c = network.variable(3).unwrap();
    let parity = network.xor(a, b);
    let parity = LogicGraph::not(parity);
    let conjunction = network.and(a, c);
    let output = network.mux(s, parity, conjunction);
    network.freeze();
    let cuts = CutDatabase::build(&network, MAX_MATCH_INPUTS);
    let matcher = matcher(vec![
        target_cell(
            "AND2",
            1.0,
            &[
                in_pin("A"),
                in_pin("B"),
                ("Y", TargetPinDirection::Output, Some("A&B")),
            ],
        ),
        target_cell(
            "OR2",
            1.0,
            &[
                in_pin("A"),
                in_pin("B"),
                ("Y", TargetPinDirection::Output, Some("A|B")),
            ],
        ),
        target_cell(
            "XOR2",
            1.5,
            &[
                in_pin("A"),
                in_pin("B"),
                ("Y", TargetPinDirection::Output, Some("A^B")),
            ],
        ),
        target_cell(
            "MUX2",
            1.6,
            &[
                in_pin("S"),
                in_pin("A"),
                in_pin("B"),
                ("Y", TargetPinDirection::Output, Some("(S&A)|(!S&B)")),
            ],
        ),
        target_cell(
            "INV",
            0.6,
            &[in_pin("A"), ("Y", TargetPinDirection::Output, Some("!A"))],
        ),
    ]);

    let cover = cover_logic_network(
        &network,
        &cuts,
        &[output],
        &matcher,
        CoverTiming {
            required_times: &[None],
            output_loads: &[None],
            input_transitions: &[],
            input_arrivals: &[],
        },
        crate::test_runtime(),
    )
    .unwrap()
    .unwrap();

    let expected = network.truth_table(output, 4);
    for assignment in 0..16 {
        assert_eq!(
            evaluate(&cover, 0, assignment),
            expected.bit(assignment),
            "assignment {assignment:#06b}"
        );
    }
}

#[test]
fn ties_cell_inputs_to_reach_cheaper_implementations() {
    let mut network = LogicGraph::new();
    let selector = network.variable(0).unwrap();
    let first_true = network.variable(1).unwrap();
    let first_false = network.variable(2).unwrap();
    let second_true = network.variable(3).unwrap();
    let second_false = network.variable(4).unwrap();
    let first = network.mux(selector, first_true, first_false);
    let second = network.mux(selector, second_true, second_false);
    network.freeze();
    let cuts = CutDatabase::build(&network, MAX_MATCH_INPUTS);
    let matcher = matcher(vec![
        target_cell(
            "O22AI",
            1.5,
            &[
                in_pin("A1"),
                in_pin("A2"),
                in_pin("B1"),
                in_pin("B2"),
                ("Y", TargetPinDirection::Output, Some("!((A1|A2)&(B1|B2))")),
            ],
        ),
        target_cell(
            "MUX2",
            3.5,
            &[
                in_pin("S"),
                in_pin("A"),
                in_pin("B"),
                ("Y", TargetPinDirection::Output, Some("(S&A)|(!S&B)")),
            ],
        ),
        target_cell(
            "INV",
            0.6,
            &[in_pin("A"), ("Y", TargetPinDirection::Output, Some("!A"))],
        ),
    ]);

    let cover = cover_logic_network(
        &network,
        &cuts,
        &[first, second],
        &matcher,
        CoverTiming {
            required_times: &[None, None],
            output_loads: &[None, None],
            input_transitions: &[],
            input_arrivals: &[],
        },
        crate::test_runtime(),
    )
    .unwrap()
    .unwrap();

    let first_truth = network.truth_table(first, 5);
    let second_truth = network.truth_table(second, 5);
    for assignment in 0..32 {
        assert_eq!(evaluate(&cover, 0, assignment), first_truth.bit(assignment));
        assert_eq!(
            evaluate(&cover, 1, assignment),
            second_truth.bit(assignment)
        );
    }
    assert!(
        cover_area(&cover, &matcher) < 7.0 - 1e-9,
        "tied o22ai with a shared select inverter beats two mux cells, got {}",
        cover_area(&cover, &matcher)
    );
}

#[test]
fn covers_sum_and_carry_with_one_full_adder() {
    let mut network = LogicGraph::new();
    let a = network.variable(0).unwrap();
    let b = network.variable(1).unwrap();
    let carry_in = network.variable(2).unwrap();
    let partial = network.xor(a, b);
    let sum = network.xor(partial, carry_in);
    let generate = network.and(a, b);
    let propagate = network.and(partial, carry_in);
    let carry = network.or(generate, propagate);
    network.freeze();
    let cuts = CutDatabase::build(&network, MAX_MATCH_INPUTS);
    let matcher = matcher(vec![
        target_cell(
            "NAND2",
            1.0,
            &[
                in_pin("A"),
                in_pin("B"),
                ("Y", TargetPinDirection::Output, Some("!(A&B)")),
            ],
        ),
        target_cell(
            "XOR2",
            2.2,
            &[
                in_pin("A"),
                in_pin("B"),
                ("Y", TargetPinDirection::Output, Some("A^B")),
            ],
        ),
        target_cell(
            "INV",
            0.6,
            &[in_pin("A"), ("Y", TargetPinDirection::Output, Some("!A"))],
        ),
        target_cell(
            "FA",
            4.0,
            &[
                in_pin("A"),
                in_pin("B"),
                in_pin("CI"),
                ("S", TargetPinDirection::Output, Some("A^B^CI")),
                (
                    "CO",
                    TargetPinDirection::Output,
                    Some("(A&B)|(A&CI)|(B&CI)"),
                ),
            ],
        ),
    ]);

    let cover = cover_logic_network(
        &network,
        &cuts,
        &[sum, carry],
        &matcher,
        CoverTiming {
            required_times: &[None, None],
            output_loads: &[None, None],
            input_transitions: &[],
            input_arrivals: &[],
        },
        crate::test_runtime(),
    )
    .unwrap()
    .unwrap();

    let sum_truth = network.truth_table(sum, 3);
    let carry_truth = network.truth_table(carry, 3);
    for assignment in 0..8 {
        assert_eq!(evaluate(&cover, 0, assignment), sum_truth.bit(assignment));
        assert_eq!(evaluate(&cover, 1, assignment), carry_truth.bit(assignment));
    }
    assert!(
        cover
            .cells
            .iter()
            .any(|cell| matches!(cell.binding, LibraryCoverBinding::Joint(_))),
        "sum and carry should share one full adder"
    );
    assert!(
        cover_area(&cover, &matcher) < 4.0 + 1e-9,
        "joint full adder beats discrete gates, got {}",
        cover_area(&cover, &matcher)
    );
}

#[test]
fn exact_recovery_scores_shared_full_adder_outputs_without_reference_drift() {
    let mut network = LogicGraph::new();
    let mut carry = network.variable(0).unwrap();
    let mut outputs = Vec::new();
    for bit in 0..32 {
        let a = network.variable(bit * 2 + 1).unwrap();
        let b = network.variable(bit * 2 + 2).unwrap();
        let partial = network.xor(a, b);
        outputs.push(network.xor(partial, carry));
        let generate = network.and(a, b);
        let propagate = network.and(partial, carry);
        carry = network.or(generate, propagate);
    }
    outputs.push(carry);
    network.freeze();
    let cuts = CutDatabase::build(&network, MAX_MATCH_INPUTS);
    let matcher = matcher(vec![
        target_cell(
            "NAND2",
            1.0,
            &[
                in_pin("A"),
                in_pin("B"),
                ("Y", TargetPinDirection::Output, Some("!(A&B)")),
            ],
        ),
        target_cell(
            "XOR2",
            2.2,
            &[
                in_pin("A"),
                in_pin("B"),
                ("Y", TargetPinDirection::Output, Some("A^B")),
            ],
        ),
        target_cell(
            "INV",
            0.6,
            &[in_pin("A"), ("Y", TargetPinDirection::Output, Some("!A"))],
        ),
        target_cell(
            "FA",
            4.0,
            &[
                in_pin("A"),
                in_pin("B"),
                in_pin("CI"),
                ("S", TargetPinDirection::Output, Some("A^B^CI")),
                (
                    "CO",
                    TargetPinDirection::Output,
                    Some("(A&B)|(A&CI)|(B&CI)"),
                ),
            ],
        ),
    ]);

    let cover = cover_logic_network(
        &network,
        &cuts,
        &outputs,
        &matcher,
        CoverTiming {
            required_times: &vec![None; outputs.len()],
            output_loads: &vec![None; outputs.len()],
            input_transitions: &[],
            input_arrivals: &[],
        },
        crate::test_runtime(),
    )
    .unwrap()
    .unwrap();

    assert_eq!(cover.outputs.len(), outputs.len());
    assert_eq!(cover.cells.len(), 32);
    assert!(
        cover
            .cells
            .iter()
            .all(|cell| matches!(cell.binding, LibraryCoverBinding::Joint(_)))
    );
    assert!((cover_area(&cover, &matcher) - 128.0).abs() < 1e-9);
}

#[test]
fn reports_unmatchable_networks_as_uncoverable() {
    let mut network = LogicGraph::new();
    let a = network.variable(0).unwrap();
    let b = network.variable(1).unwrap();
    let output = network.xor(a, b);
    network.freeze();
    let cuts = CutDatabase::build(&network, MAX_MATCH_INPUTS);
    let matcher = matcher(vec![target_cell(
        "AND2",
        1.0,
        &[
            in_pin("A"),
            in_pin("B"),
            ("Y", TargetPinDirection::Output, Some("A&B")),
        ],
    )]);

    assert!(
        cover_logic_network(
            &network,
            &cuts,
            &[output],
            &matcher,
            CoverTiming {
                required_times: &[None],
                output_loads: &[None],
                input_transitions: &[],
                input_arrivals: &[],
            },
            crate::test_runtime(),
        )
        .unwrap()
        .is_none()
    );
}
