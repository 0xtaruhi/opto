// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn structurally_hashes_equivalent_commutative_nodes() {
    let mut cone = LogicGraph::new();
    let a = cone.variable(0).unwrap();
    let b = cone.variable(1).unwrap();

    assert_eq!(cone.and(a, b), cone.and(b, a));
    assert_eq!(cone.node_count(), 4);
}

#[test]
fn evaluates_truth_table_in_topological_order() {
    let mut cone = LogicGraph::new();
    let a = cone.variable(0).unwrap();
    let b = cone.variable(1).unwrap();
    let c = cone.variable(2).unwrap();
    let ab = cone.and(a, b);
    let root = cone.or(ab, c);

    assert_eq!(cone.truth_table(root, 3).bits, 0b1111_1000);
}

#[test]
fn records_compact_node_levels_for_parallel_scheduling() {
    let mut cone = LogicGraph::new();
    let a = cone.variable(0).unwrap();
    let b = cone.variable(1).unwrap();
    let c = cone.variable(2).unwrap();
    let ab = cone.and(a, b);
    let inverted_c = LogicGraph::not(c);
    let root = cone.or(ab, inverted_c);

    assert_eq!(cone.level(a), 0);
    assert_eq!(cone.level(ab), 1);
    assert_eq!(cone.level(inverted_c), 0);
    assert_eq!(cone.level(root), 2);
}

#[test]
fn node_and_cut_ids_remain_compact_beyond_u16_capacity() {
    let mut cone = LogicGraph::new();
    for index in 0..70_000 {
        cone.variable(index).unwrap();
    }

    assert_eq!(cone.node_count(), 70_001);
    assert_eq!(std::mem::size_of::<LogicNodeId>(), 4);
    assert!(std::mem::size_of::<KCut>() <= 28);
    assert!(std::mem::size_of::<CutRange>() <= 12);
}

#[test]
fn enumerates_structural_cuts() {
    let mut cone = LogicGraph::new();
    let a = cone.variable(0).unwrap();
    let b = cone.variable(1).unwrap();
    let c = cone.variable(2).unwrap();
    let ab = cone.and(a, b);
    let root = cone.or(ab, c);

    let cuts = cone.cuts(root, 4);

    assert!(has_cut(&cuts, &[root.positive()]));
    assert!(has_cut(&cuts, &[c, ab]));
    assert!(has_cut(&cuts, &[a, b, c]));
}

#[test]
fn parallel_cut_construction_matches_serial_order() {
    let mut cone = LogicGraph::new();
    let inputs = (0..64)
        .map(|index| cone.variable(index).unwrap())
        .collect::<Vec<_>>();
    let mut level = inputs;
    for round in 0..12 {
        let mut next = Vec::new();
        for (index, pair) in level.chunks(2).enumerate() {
            let right = pair.get(1).copied().unwrap_or(pair[0]);
            next.push(if (round + index) % 2 == 0 {
                cone.and(pair[0], right)
            } else {
                cone.xor(pair[0], right)
            });
        }
        level.extend(next);
    }
    while cone.node_count() < 600 {
        let last = LogicNodeId::from_index(cone.node_count() - 1);
        let input = LogicNodeId::from_index(1 + cone.node_count() % 64);
        level.push(cone.and(last, input));
    }
    cone.freeze();

    let serial = CutDatabase::build(&cone, 6);
    let runtime = ExecutionContext::new(&opto_runtime::ExecutionConfig { max_threads: 4 }).unwrap();
    let parallel = CutDatabase::build_parallel(&cone, 6, &runtime).unwrap();

    for index in 0..cone.node_count() {
        let node = LogicNodeId::from_index(index);
        assert_eq!(parallel.cuts(node), serial.cuts(node));
    }
}

#[test]
fn constant_nodes_have_empty_cuts() {
    let cone = LogicGraph::new();
    let zero = LogicGraph::constant(false);

    let cuts = cone.cuts(zero, 4);

    assert_eq!(cuts.len(), 1);
    assert!(has_cut(&cuts, &[]));
}

#[test]
fn mux_cut_merges_three_fanin_cut_sets() {
    let mut cone = LogicGraph::new();
    let select = cone.variable(0).unwrap();
    let a = cone.variable(1).unwrap();
    let b = cone.variable(2).unwrap();
    let root = cone.mux(select, a, b);

    let cuts = cone.cuts(root, 4);

    assert!(has_cut(&cuts, &[root.positive()]));
    assert!(has_cut(&cuts, &[select, a, b]));
}

#[test]
fn rejects_cuts_larger_than_requested_limit() {
    let mut cone = LogicGraph::new();
    let a = cone.variable(0).unwrap();
    let b = cone.variable(1).unwrap();
    let c = cone.variable(2).unwrap();
    let d = cone.variable(3).unwrap();
    let ab = cone.and(a, b);
    let cd = cone.and(c, d);
    let root = cone.or(ab, cd);

    let cuts = cone.cuts(root, 3);

    assert!(has_cut(&cuts, &[root.positive()]));
    assert!(has_cut(&cuts, &[ab, cd]));
    assert!(!has_cut(&cuts, &[a, b, c, d]));
}

#[test]
fn computes_truth_table_over_internal_cut_leaves() {
    let mut cone = LogicGraph::new();
    let a = cone.variable(0).unwrap();
    let b = cone.variable(1).unwrap();
    let c = cone.variable(2).unwrap();
    let ab = cone.and(a, b);
    let root = cone.or(ab, c);
    let cut = KCut::from_leaves(&[c, ab]).unwrap();

    assert_eq!(cone.truth_table_for_cut(root, cut).bits, 0b1110);
}

#[test]
fn cut_truth_matches_full_cone_truth_for_primary_input_cut() {
    let mut cone = LogicGraph::new();
    let a = cone.variable(0).unwrap();
    let b = cone.variable(1).unwrap();
    let c = cone.variable(2).unwrap();
    let ab = cone.and(a, b);
    let root = cone.or(ab, c);
    let cut = KCut::from_leaves(&[a, b, c]).unwrap();

    assert_eq!(
        cone.truth_table_for_cut(root, cut),
        cone.truth_table(root, 3)
    );
}

#[test]
fn derives_a_function_over_non_structural_inputs() {
    let mut cone = LogicGraph::new();
    let select_0 = cone.variable(0).unwrap();
    let select_1 = cone.variable(1).unwrap();
    let a = cone.variable(2).unwrap();
    let b = cone.variable(3).unwrap();
    let c = cone.variable(4).unwrap();
    let select_or = cone.or(select_0, select_1);
    let no_selector_active = LogicGraph::not(select_or);
    let exactly_one_selector_active = cone.xor(select_0, select_1);
    let inner = cone.mux(exactly_one_selector_active, b, c);
    let root = cone.mux(no_selector_active, a, inner);
    let tables = cone.truth_tables_for_inputs(root, &[select_0, select_1, a, b, c], &[]);

    let truth = tables
        .function_of(root, &[select_0, a, exactly_one_selector_active, b, c])
        .unwrap();

    for assignment in 0..32 {
        let select_0 = assignment & 1 != 0;
        let a = assignment & 2 != 0;
        let exactly_one_selector_active = assignment & 4 != 0;
        let b = assignment & 8 != 0;
        let c = assignment & 16 != 0;
        let expected = if exactly_one_selector_active {
            b
        } else if select_0 {
            c
        } else {
            a
        };
        assert_eq!(truth.bit(assignment), expected);
    }
}

#[test]
fn rejects_inputs_that_do_not_determine_the_root() {
    let mut cone = LogicGraph::new();
    let a = cone.variable(0).unwrap();
    let b = cone.variable(1).unwrap();
    let root = cone.xor(a, b);
    let tables = cone.truth_tables_for_inputs(root, &[a, b], &[]);

    assert!(tables.function_of(root, &[a]).is_none());
}

#[test]
fn validates_that_a_cut_intersects_every_input_path() {
    let mut cone = LogicGraph::new();
    let a = cone.variable(0).unwrap();
    let b = cone.variable(1).unwrap();
    let c = cone.variable(2).unwrap();
    let ab = cone.and(a, b);
    let root = cone.or(ab, c);

    assert!(cone.is_valid_cut(root, KCut::from_leaves(&[ab, c]).unwrap()));
    assert!(cone.is_valid_cut(root, KCut::from_leaves(&[a, b, c]).unwrap()));
    assert!(!cone.is_valid_cut(root, KCut::from_leaves(&[ab]).unwrap()));
    assert!(!cone.is_valid_cut(root, KCut::from_leaves(&[a, c]).unwrap()));
}

fn has_cut(cuts: &CutSet, leaves: &[LogicNodeId]) -> bool {
    cuts.iter().any(|cut| cut.leaves() == leaves)
}
