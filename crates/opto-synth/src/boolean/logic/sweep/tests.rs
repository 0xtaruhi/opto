// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::boolean::logic::pipeline::map_roots;

/// Builds three-input majority twice, through the sum-of-products form and
/// through the factored form. Both are gates, both compute the same function,
/// and neither the builder's local rules nor hash consing merges them.
fn majority_pair() -> (LogicGraph, [LogicNodeId; 2]) {
    let mut network = LogicGraph::new();
    let a = network.variable(0).unwrap();
    let b = network.variable(1).unwrap();
    let c = network.variable(2).unwrap();
    let both_high = network.and(b, c);
    let sum_of_products = {
        let with_b = network.and(a, b);
        let with_c = network.and(a, c);
        let either = network.or(with_b, with_c);
        network.or(either, both_high)
    };
    let factored = {
        let either = network.or(b, c);
        let selected = network.and(a, either);
        network.or(selected, both_high)
    };
    network.freeze();
    (network, [sum_of_products, factored])
}

#[test]
fn merges_a_structurally_distinct_equivalent_node() {
    let (network, roots) = majority_pair();
    let expected = roots.map(|root| network.truth_table(root, 3));
    let mut metrics = SweepMetrics::default();

    let product = reduce(&network, &roots, crate::test_runtime(), &mut metrics)
        .expect("sweep succeeds")
        .expect("sweep finds the duplicate");
    let reduced_roots = map_roots(&product.remap, &roots).unwrap();

    assert!(metrics.proved > 0);
    assert_eq!(reduced_roots[0], reduced_roots[1]);
    for (&actual, expected) in reduced_roots.iter().zip(expected) {
        assert_eq!(product.network.truth_table(actual, 3), expected);
    }
}

#[test]
fn merges_an_inverted_equivalent_node() {
    let (network, [sum_of_products, factored]) = majority_pair();
    // Observing the complement of one form must still merge the two cones and
    // must preserve the observed phase.
    let roots = [sum_of_products, factored.inverted()];
    let expected = roots.map(|root| network.truth_table(root, 3));

    let product = reduce(
        &network,
        &roots,
        crate::test_runtime(),
        &mut SweepMetrics::default(),
    )
    .expect("sweep succeeds")
    .expect("sweep finds the duplicate");
    let reduced_roots = map_roots(&product.remap, &roots).unwrap();

    assert_eq!(reduced_roots[0].positive(), reduced_roots[1].positive());
    assert_ne!(
        reduced_roots[0].is_inverted(),
        reduced_roots[1].is_inverted()
    );
    for (&actual, expected) in reduced_roots.iter().zip(expected) {
        assert_eq!(product.network.truth_table(actual, 3), expected);
    }
}

#[test]
fn folds_a_node_that_is_functionally_constant() {
    let mut network = LogicGraph::new();
    let a = network.variable(0).unwrap();
    let b = network.variable(1).unwrap();
    let c = network.variable(2).unwrap();
    // `(a | b) & (b | c) & !(a | b | c)` is unsatisfiable, but no local rule of
    // the builder observes that.
    let left = network.or(a, b);
    let middle = network.or(b, c);
    let any = {
        let partial = network.or(a, b);
        network.or(partial, c)
    };
    let root = {
        let pair = network.and(left, middle);
        network.and(pair, any.inverted())
    };
    network.freeze();

    let product = reduce(
        &network,
        &[root],
        crate::test_runtime(),
        &mut SweepMetrics::default(),
    )
    .expect("sweep succeeds")
    .expect("sweep folds the constant");
    let reduced = map_roots(&product.remap, &[root]).unwrap();

    assert_eq!(product.network.truth_table(reduced[0], 3).bits, 0);
}

#[test]
fn leaves_inequivalent_nodes_alone() {
    let mut network = LogicGraph::new();
    let a = network.variable(0).unwrap();
    let b = network.variable(1).unwrap();
    let roots = [network.and(a, b), network.xor(a, b)];
    network.freeze();
    let mut metrics = SweepMetrics::default();

    assert!(
        reduce(&network, &roots, crate::test_runtime(), &mut metrics)
            .unwrap()
            .is_none()
    );
    assert_eq!(metrics.proved, 0);
}

#[test]
fn preserves_equivalence_on_a_reconvergent_subject() {
    let (network, roots) = majority_pair();
    let product = reduce(
        &network,
        &roots,
        crate::test_runtime(),
        &mut SweepMetrics::default(),
    )
    .unwrap()
    .unwrap();
    let reduced_roots = map_roots(&product.remap, &roots).unwrap();
    let proof = opto_formal::prove_logic_network_equivalence(
        network.storage_network(),
        &roots.map(LogicNodeId::lit),
        product.network.storage_network(),
        &reduced_roots
            .iter()
            .copied()
            .map(LogicNodeId::lit)
            .collect::<Vec<_>>(),
    )
    .expect("formal engine accepts the sweep miter");

    assert!(proof.require_proved().is_ok());
}

#[test]
fn simulation_stimulus_is_independent_of_node_identity() {
    // Two graphs that declare the same origins in a different node order must
    // simulate the same stimulus, or nomination would depend on construction
    // order rather than on function.
    let build = |swapped: bool| {
        let mut network = LogicGraph::new();
        let (a, b) = if swapped {
            let b = network.variable(1).unwrap();
            (network.variable(0).unwrap(), b)
        } else {
            let a = network.variable(0).unwrap();
            (a, network.variable(1).unwrap())
        };
        let root = network.and(a, b);
        network.freeze();
        (network, root)
    };
    let signature = |network: &LogicGraph, root: LogicNodeId, origin: u32| {
        let live = network.live_nodes(&[root]);
        let stimulus = Stimulus::random();
        let mut signatures = Signatures::new(network.node_count());
        simulate(network, &live, &stimulus, &mut signatures, 0);
        (0..network.node_count())
            .find(|&index| {
                matches!(
                    network.node(LogicNodeId::from_index(index)),
                    LogicNode::Var(found) if found == origin
                )
            })
            .map(|index| signatures.row(index)[0])
            .unwrap()
    };
    let (first, first_root) = build(false);
    let (second, second_root) = build(true);
    for origin in 0..2 {
        assert_eq!(
            signature(&first, first_root, origin),
            signature(&second, second_root, origin)
        );
    }
}

#[test]
fn nominated_classes_are_ordered_by_their_lowest_member() {
    let (network, roots) = majority_pair();
    let live = network.live_nodes(&roots);
    let stimulus = Stimulus::random();
    let mut signatures = Signatures::new(network.node_count());
    simulate(&network, &live, &stimulus, &mut signatures, 0);
    let substitutions = vec![None; network.node_count()];
    let classes = nominate(
        &network,
        &live,
        &substitutions,
        &signatures,
        &mut SweepMetrics::default(),
    );

    assert!(
        classes
            .windows(2)
            .all(|pair| pair[0].members[0].0 < pair[1].members[0].0)
    );
    for class in &classes {
        assert!(class.members.windows(2).all(|pair| pair[0].0 < pair[1].0));
    }
}

#[test]
fn shard_quotas_partition_the_round_budget() {
    for (max_pairs, shard_count) in [
        (4_000, 130),
        (4_000, 834),
        (1, 130),
        (0, 7),
        (7, 1),
        (13, 5),
    ] {
        let quotas = (0..shard_count)
            .map(|shard| super::shard_quota(max_pairs, shard_count, shard))
            .collect::<Vec<_>>();
        let total = quotas.iter().sum::<usize>();
        assert_eq!(
            total, max_pairs,
            "quotas for {shard_count} shards must sum to {max_pairs}"
        );
        let base = max_pairs / shard_count;
        let remainder = max_pairs % shard_count;
        for (shard, quota) in quotas.into_iter().enumerate() {
            assert_eq!(
                quota,
                base + usize::from(shard < remainder),
                "shard {shard} of {shard_count} received the wrong quota"
            );
        }
    }
}
