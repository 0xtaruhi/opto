// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::planning::{added_cost, collect_divisors, mffc_weight};
use super::recipe::PAIR_TRUTHS;
use super::*;

fn gate_count(network: &LogicGraph) -> usize {
    (0..network.node_count())
        .filter(|&index| network.node(LogicNodeId::from_index(index)).is_gate())
        .count()
}

fn evaluate_plan(plan: &Plan, assignment: usize) -> bool {
    match plan {
        Plan::Constant(value) => *value,
        Plan::Literal { var, inverted } => {
            (((assignment >> usize::from(*var)) & 1) != 0) ^ *inverted
        }
        Plan::And(left, right) => {
            evaluate_plan(left, assignment) && evaluate_plan(right, assignment)
        }
        Plan::Or(left, right) => {
            evaluate_plan(left, assignment) || evaluate_plan(right, assignment)
        }
        Plan::Xor(left, right) => {
            evaluate_plan(left, assignment) ^ evaluate_plan(right, assignment)
        }
        Plan::Mux {
            select,
            then_plan,
            else_plan,
        } => {
            if (assignment >> usize::from(*select)) & 1 != 0 {
                evaluate_plan(then_plan, assignment)
            } else {
                evaluate_plan(else_plan, assignment)
            }
        }
    }
}

#[test]
fn six_input_plain_and_timing_plans_are_exact() {
    let truth = TruthTable {
        input_count: WINDOW_CUT_LEAVES,
        bits: 1 << 63,
    };
    let mut synthesizer = Synthesizer::default();
    let (_, plain) = synthesizer.plan(truth);
    let arrivals = [0, 0, 0, 0, 0, 10];
    let (_, timed) = synthesizer.timing_plan(truth, &arrivals);

    for assignment in 0..64 {
        let expected = truth.bit(assignment);
        assert_eq!(evaluate_plan(&plain, assignment), expected);
        assert_eq!(evaluate_plan(&timed, assignment), expected);
    }
    assert!(plan_level(&timed, &arrivals) < plan_level(&plain, &arrivals));
    let recipe = PlanRecipe::from_plan(&timed).unwrap();
    assert!(recipe.proves(truth, u64::MAX, &[]));
}

#[test]
fn structural_budget_includes_absolute_input_arrival() {
    let mut network = LogicGraph::new();
    let a = network.variable(0).unwrap();
    let b = network.variable(1).unwrap();
    let root = network.and(a, b);
    network.freeze();

    let requirements = [Some(2.0)];
    let early_inputs = [Some(0.0), Some(0.0)];
    let late_inputs = [Some(1.2), Some(0.0)];
    let early = TimingBudget::for_roots(
        &network,
        &[root],
        StructuralTiming::new(&requirements, &early_inputs, Some(1.0)),
    )
    .unwrap()
    .unwrap();
    let late = TimingBudget::for_roots(
        &network,
        &[root],
        StructuralTiming::new(&requirements, &late_inputs, Some(1.0)),
    )
    .unwrap()
    .unwrap();

    assert_eq!(early.required(root), Some(2));
    assert_eq!(early.arrival(root), 1);
    assert_eq!(early.violation(root), 0);
    assert_eq!(late.arrival(root), 3);
    assert_eq!(late.violation(root), 1);
}

#[test]
fn structural_critical_paths_exclude_early_fanins_and_less_violating_endpoints() {
    let mut network = LogicGraph::new();
    let early = network.variable(0).unwrap();
    let late = network.variable(1).unwrap();
    let other = network.variable(2).unwrap();
    let first = network.and(late, early);
    let worst = network.xor(first, other);
    let secondary = network.and(late, other);
    network.freeze();
    let timing = TimingBudget::for_roots(
        &network,
        &[worst, secondary],
        StructuralTiming::new(
            &[Some(2.0), Some(2.0)],
            &[Some(0.0), Some(3.0), Some(0.0)],
            Some(1.0),
        ),
    )
    .unwrap()
    .unwrap();
    assert_eq!(timing.violation(worst), 3);
    assert_eq!(timing.violation(secondary), 2);
    for node in [late, first, worst] {
        assert!(timing.critical[node.index()]);
    }
    for node in [early, other, secondary] {
        assert!(!timing.critical[node.index()]);
    }
}

#[test]
fn compact_recipe_proof_substitutes_divisors_and_honors_care() {
    let plan = Plan::Xor(
        Arc::new(Plan::Literal {
            var: 0,
            inverted: false,
        }),
        Arc::new(Plan::Literal {
            var: 2,
            inverted: false,
        }),
    );
    let truth = TruthTable {
        input_count: 2,
        bits: 0b0010,
    };
    let recipe = PlanRecipe::from_plan(&plan).unwrap();
    assert!(recipe.proves(truth, 0b0011, &[0b1100]));
    assert!(!recipe.proves(truth, u64::MAX, &[0b1100]));
}

#[test]
fn mffc_search_is_region_bounded() {
    let mut network = LogicGraph::new();
    let mut root = network.variable(0).unwrap();
    for input in 1..=MFFC_NODE_BUDGET + 1 {
        let next = network.variable(input).unwrap();
        root = network.and(root, next);
    }
    network.freeze();
    let mut references = vec![0; network.node_count()];
    for index in 0..network.node_count() {
        for fanin in network.node(LogicNodeId::from_index(index)).fanins() {
            references[fanin.index()] += 1;
        }
    }
    references[root.index()] += 1;
    let cut = KCut::from_leaves(&[]).unwrap();
    let mut scratch = MffcScratch::new();

    assert!(mffc_weight(&network, &references, root, cut, &mut scratch).is_none());
    assert_eq!(scratch.deltas.len(), MFFC_TABLE_CAPACITY);
    assert_eq!(scratch.touched.capacity(), MFFC_NODE_BUDGET);
    assert_eq!(scratch.stack.capacity(), MFFC_NODE_BUDGET);
    assert_eq!(scratch.dying.capacity(), MFFC_NODE_BUDGET);
}

#[test]
fn remaps_virtual_divisor_input_phases_and_order() {
    let left = LogicNodeId::from_index(1);
    let right = LogicNodeId::from_index(2);

    assert_eq!(remap_pair_truth(0b1000, left, right), 0b1000);
    assert_eq!(remap_pair_truth(0b1000, left.inverted(), right), 0b0100);
    assert_eq!(remap_pair_truth(0b1000, left, right.inverted()), 0b0010);
    assert_eq!(
        remap_pair_truth(0b1000, left.inverted(), right.inverted()),
        0b0001
    );
    assert_eq!(remap_pair_truth(0b0010, right, left), 0b0100);
    assert_eq!(remap_pair_truth(0b0110, left.inverted(), right), 0b1001);
}

#[test]
fn virtual_divisors_saturate_the_shared_capacity_without_overflow() {
    let mut network = LogicGraph::new();
    let a = network.variable(0).unwrap();
    let b = network.variable(1).unwrap();
    let c = network.variable(2).unwrap();
    let d = network.variable(3).unwrap();
    let real_divisor = network.xor(a, d);
    network.freeze();

    let mut virtuals = ApprovedDivisors::default();
    let mut add_virtual = |left, right, truth| {
        let id = u32::try_from(virtuals.definitions.len()).unwrap();
        virtuals.definitions.push((left, right, truth));
        virtuals
            .by_pair
            .entry((
                u32::try_from(left.index()).unwrap(),
                u32::try_from(right.index()).unwrap(),
            ))
            .or_default()
            .push((id, truth));
    };
    for (left, right) in [(a, b), (a, c), (b, c)] {
        for truth in PAIR_TRUTHS {
            add_virtual(left, right, truth);
        }
    }
    add_virtual(a, d, 0b1000);

    let cuts = CutDatabase::build(&network, WINDOW_CUT_LEAVES);
    let mut references = vec![0; network.node_count()];
    references[real_divisor.index()] = 1;
    let support_index =
        build_support_index(&network, &cuts, &references, crate::test_runtime()).unwrap();
    let cut = KCut::from_leaves(&[a, b, c, d]).unwrap();
    let divisors = collect_divisors(&support_index, &virtuals, network.node_count(), cut, &[]);

    assert_eq!(divisors.len(), DIVISOR_CAP);
    assert!(
        divisors
            .iter()
            .all(|(divisor, _)| matches!(divisor, DivisorRef::Virtual(_)))
    );
}

#[test]
fn rediscovers_mux_structure_from_and_or_form() {
    let mut network = LogicGraph::new();
    let s = network.variable(0).unwrap();
    let a = network.variable(1).unwrap();
    let b = network.variable(2).unwrap();
    let then_arm = network.and(s, a);
    let else_arm = network.and(s.inverted(), b);
    let root = network.or(then_arm, else_arm);
    network.freeze();
    let before = network.truth_table(root, 3);
    let before_gates = gate_count(&network);

    let outcome = optimize_network(
        &network,
        &[root],
        &[None],
        crate::SynthesisDiagnostics::default(),
        crate::test_runtime(),
    )
    .unwrap();

    let mapped = remap_literal(&outcome.remap, root).unwrap();
    let after = outcome.network.truth_table(mapped, 3);
    assert_eq!(after, before);
    assert!(gate_count(&outcome.network) < before_gates);
}

#[test]
fn preserves_functions_across_mixed_windows() {
    let mut network = LogicGraph::new();
    let inputs = (0..5)
        .map(|index| network.variable(index).unwrap())
        .collect::<Vec<_>>();
    let parity = network.xor(inputs[0], inputs[1]);
    let parity = network.xor(parity, inputs[2]);
    let guard = network.and(inputs[3], parity.inverted());
    let pick = network.mux(inputs[4], guard, parity);
    let redundant_arm = network.and(inputs[3], parity.inverted());
    let redundant = network.or(pick, redundant_arm);
    network.freeze();
    let roots = [pick, redundant, guard];
    let before = roots
        .iter()
        .map(|&root| network.truth_table(root, 5))
        .collect::<Vec<_>>();

    let outcome = optimize_network(
        &network,
        &roots,
        &vec![None; roots.len()],
        crate::SynthesisDiagnostics::default(),
        crate::test_runtime(),
    )
    .unwrap();

    for (root, expected) in roots.iter().zip(before) {
        let mapped = remap_literal(&outcome.remap, *root).unwrap();
        assert_eq!(outcome.network.truth_table(mapped, 5), expected);
    }
}

#[test]
fn warm_recipe_cache_matches_cold_boolean_synthesis() {
    let mut network = LogicGraph::new();
    let inputs = (0..5)
        .map(|index| network.variable(index).unwrap())
        .collect::<Vec<_>>();
    let left = network.xor(inputs[0], inputs[1]);
    let right = network.mux(inputs[2], inputs[3], inputs[4]);
    let root = network.and(left, right);
    network.freeze();
    let cache = RewriteRecipeCache::default();
    let cold_metrics = crate::incremental::IncrementalRunMetrics::default();
    let warm_metrics = crate::incremental::IncrementalRunMetrics::default();
    let diagnostics = crate::SynthesisDiagnostics {
        check_incremental: true,
        ..crate::SynthesisDiagnostics::default()
    };

    let cold = optimize_network_cached(
        &network,
        &[root],
        &[None],
        diagnostics,
        crate::test_runtime(),
        RewriteIncremental::new(&cache, &cold_metrics),
    )
    .unwrap();
    let warm = optimize_network_cached(
        &network,
        &[root],
        &[None],
        diagnostics,
        crate::test_runtime(),
        RewriteIncremental::new(&cache, &warm_metrics),
    )
    .unwrap();

    assert!(warm_metrics.snapshot().boolean_recipe_hits > 0);
    assert_eq!(warm.remap, cold.remap);
    assert_eq!(warm.network.node_count(), cold.network.node_count());
    for index in 0..cold.network.node_count() {
        let node = LogicNodeId::from_index(index);
        assert_eq!(warm.network.node(node), cold.network.node(node));
    }
}

#[test]
fn resubstitutes_xor_from_existing_divisors() {
    let mut network = LogicGraph::new();
    let a = network.variable(0).unwrap();
    let b = network.variable(1).unwrap();
    let union = network.or(a, b);
    let overlap = network.and(a, b);
    let parity = network.xor(a, b);
    network.freeze();
    let roots = [union, overlap, parity];
    let before = roots
        .iter()
        .map(|&root| network.truth_table(root, 2))
        .collect::<Vec<_>>();
    let before_weight = network_size(&network).0;

    let outcome = optimize_network(
        &network,
        &roots,
        &vec![None; roots.len()],
        crate::SynthesisDiagnostics::default(),
        crate::test_runtime(),
    )
    .unwrap();

    for (root, expected) in roots.iter().zip(before) {
        let mapped = remap_literal(&outcome.remap, *root).unwrap();
        assert_eq!(outcome.network.truth_table(mapped, 2), expected);
    }
    assert!(
        network_size(&outcome.network).0 < before_weight,
        "xor should reuse the union and overlap divisors"
    );
}

#[test]
fn absolute_budget_selects_distinct_equivalent_structures() {
    let mut network = LogicGraph::new();
    let inputs = (0..6)
        .map(|origin| network.variable(origin).unwrap())
        .collect::<Vec<_>>();
    let root = inputs[1..]
        .iter()
        .fold(inputs[0], |root, &input| network.and(root, input));
    network.freeze();

    let run = |requirement| {
        optimize_network(
            &network,
            &[root],
            &[Some(requirement)],
            crate::SynthesisDiagnostics::default(),
            crate::test_runtime(),
        )
        .unwrap()
    };
    let tight = run(3.0);
    let relaxed = run(10.0);
    let tight_root = remap_literal(&tight.remap, root).unwrap();
    let relaxed_root = remap_literal(&relaxed.remap, root).unwrap();

    assert_eq!(tight.network.level(tight_root), 3);
    assert_eq!(relaxed.network.level(relaxed_root), 5);
    assert_eq!(network_size(&relaxed.network), network_size(&tight.network));
    for (outcome, mapped) in [(&tight, tight_root), (&relaxed, relaxed_root)] {
        let proof = opto_formal::prove_logic_network_equivalence(
            network.storage_network(),
            &[root.lit()],
            outcome.network.storage_network(),
            &[mapped.lit()],
        )
        .unwrap();
        assert!(proof.require_proved().is_ok());
    }
}

#[test]
fn eliminates_logic_over_unreachable_window_patterns() {
    let mut network = LogicGraph::new();
    let a = network.variable(0).unwrap();
    let b = network.variable(1).unwrap();
    let both = network.and(a, b);
    let either = network.or(a, b);
    let impossible = network.and(both, either.inverted());
    network.freeze();
    let roots = [both, either, impossible];
    let before = roots
        .iter()
        .map(|&root| network.truth_table(root, 2))
        .collect::<Vec<_>>();

    let outcome = optimize_network(
        &network,
        &roots,
        &vec![None; roots.len()],
        crate::SynthesisDiagnostics::default(),
        crate::test_runtime(),
    )
    .unwrap();

    for (root, expected) in roots.iter().zip(before) {
        let mapped = remap_literal(&outcome.remap, *root).unwrap();
        assert_eq!(outcome.network.truth_table(mapped, 2), expected);
    }
    assert_eq!(
        gate_count(&outcome.network),
        2,
        "the contradiction over both/either collapses to a constant"
    );
}

#[test]
fn drops_logic_outside_the_root_cones() {
    let mut network = LogicGraph::new();
    let a = network.variable(0).unwrap();
    let b = network.variable(1).unwrap();
    let c = network.variable(2).unwrap();
    let kept = network.and(a, b);
    let dead_inner = network.xor(b, c);
    let _dead = network.mux(a, dead_inner, c);
    network.freeze();

    let outcome = optimize_network(
        &network,
        &[kept],
        &[None],
        crate::SynthesisDiagnostics::default(),
        crate::test_runtime(),
    )
    .unwrap();

    assert_eq!(gate_count(&outcome.network), 1);
}

#[test]
fn rewrite_is_deterministic_across_worker_counts() {
    let mut network = LogicGraph::new();
    let inputs = (0..6)
        .map(|index| network.variable(index).unwrap())
        .collect::<Vec<_>>();
    let mut current = network.xor(inputs[0], inputs[1]);
    let mut roots = Vec::new();
    for index in 0..600 {
        current = match index % 3 {
            0 => network.and(current, inputs[index % inputs.len()]),
            1 => network.xor(current, inputs[index % inputs.len()]),
            _ => network.mux(
                inputs[index % inputs.len()],
                current,
                inputs[(index + 1) % inputs.len()],
            ),
        };
        if index % 100 == 99 {
            roots.push(current);
        }
    }
    network.freeze();
    let runtime = |max_threads| {
        ExecutionContext::new(&opto_runtime::ExecutionConfig { max_threads }).unwrap()
    };

    let serial = optimize_network(
        &network,
        &roots,
        &vec![None; roots.len()],
        crate::SynthesisDiagnostics::default(),
        &runtime(1),
    )
    .unwrap();
    let parallel = optimize_network(
        &network,
        &roots,
        &vec![None; roots.len()],
        crate::SynthesisDiagnostics::default(),
        &runtime(4),
    )
    .unwrap();

    assert_eq!(
        network_size(&parallel.network),
        network_size(&serial.network)
    );
    assert_eq!(parallel.network.node_count(), serial.network.node_count());
    for index in 0..serial.network.node_count() {
        let node = LogicNodeId::from_index(index);
        assert_eq!(parallel.network.node(node), serial.network.node(node));
    }
    for root in roots {
        let serial_root = remap_literal(&serial.remap, root).unwrap();
        let parallel_root = remap_literal(&parallel.remap, root).unwrap();
        assert_eq!(
            parallel.network.truth_table(parallel_root, 6),
            serial.network.truth_table(serial_root, 6)
        );
    }
}

#[test]
fn replacement_cost_charges_only_nodes_the_network_does_not_already_hold() {
    // (a & b) feeds two consumers, so the sharing landscape a rewrite of one
    // consumer sees contains it; (c & d) is private to the rewritten cone.
    let mut network = LogicGraph::new();
    let inputs = (0..4)
        .map(|origin| network.variable(origin).unwrap())
        .collect::<Vec<_>>();
    let [a, b, c, d] = [inputs[0], inputs[1], inputs[2], inputs[3]];
    let ab = network.and(a, b);
    let cd = network.and(c, d);
    let root = network.and(ab, cd);
    network.freeze();

    let index = opto_ir::logic::StructuralIndex::of(network.storage_network());
    let recipe = PlanRecipe::from_plan(&Plan::And(
        Arc::new(Plan::And(
            Arc::new(Plan::Literal {
                var: 0,
                inverted: false,
            }),
            Arc::new(Plan::Literal {
                var: 1,
                inverted: false,
            }),
        )),
        Arc::new(Plan::And(
            Arc::new(Plan::Literal {
                var: 2,
                inverted: false,
            }),
            Arc::new(Plan::Literal {
                var: 3,
                inverted: false,
            }),
        )),
    ))
    .unwrap();
    let leaves = [a, b, c, d];
    let probe = |dying: &[u32]| {
        added_cost(
            &recipe,
            &leaves,
            dying,
            &mut opto_ir::logic::LogicProbe::new(network.storage_network(), &index),
        )
    };

    // Rebuilding the whole cone costs nothing: every node is already there.
    assert_eq!(probe(&[]), (0, 0));

    // Replacing `root` removes `cd` with it, so the plan has to build `cd`
    // again, and `root` above it. Crediting the removal and the reuse of the
    // same node at once is what this exclusion prevents.
    let dying = [
        u32::try_from(root.index()).unwrap(),
        u32::try_from(cd.index()).unwrap(),
    ];
    assert_eq!(probe(&dying), (2, 2 * AND_WEIGHT));

    // The shared node stays visible even while the private one dies.
    assert!(!dying.contains(&u32::try_from(ab.index()).unwrap()));
}
