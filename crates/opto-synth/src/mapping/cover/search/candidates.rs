// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    Candidate, CombinationalCellCatalog, CutDatabase, CutTruthDatabase, DONT_CARE_FILL_CAP,
    HashMap, Joint, KCut, LogicGraph, LogicNodeId, TruthTable, full_truth_mask, slot,
};
use crate::planning::mapping_policy::compare_cell_cost;

#[derive(Clone, Copy)]
pub(crate) struct CandidateContext<'a> {
    pub(crate) network: &'a LogicGraph,
    pub(crate) cuts: &'a CutDatabase,
    pub(crate) truths: &'a CutTruthDatabase,
    pub(crate) catalog: &'a CombinationalCellCatalog,
}

pub(crate) fn node_candidates(
    context: CandidateContext<'_>,
    index: usize,
    cares: Option<&[u64]>,
    candidates: &mut Vec<Candidate>,
    exact_cache: &mut HashMap<TruthTable, Box<[Candidate]>>,
) -> Result<[usize; 2], crate::SynthError> {
    let CandidateContext {
        network,
        cuts,
        truths,
        catalog,
    } = context;
    let node = LogicNodeId::from_index(index);
    if !network.node(node).is_cover_node() {
        return Ok([0, 0]);
    }
    let mut phase_lengths = [0usize; 2];
    for (phase, phase_length) in phase_lengths.iter_mut().enumerate() {
        let start = candidates.len();
        for (cut_index, cut) in cuts.cuts(node).iter().copied().enumerate() {
            if cut.contains(node) {
                continue;
            }
            let compact_cut = u8::try_from(cut_index).map_err(|_| {
                crate::SynthError::capacity("cover cut index exceeds 8-bit capacity")
            })?;
            let assignments = 1usize << cut.len();
            let full = full_truth_mask(assignments);
            let positive = truths.truth(node, cut_index);
            let truth = if phase == 0 {
                positive
            } else {
                TruthTable {
                    input_count: positive.input_count,
                    bits: positive.bits ^ full,
                }
            };
            let exact = match exact_cache.entry(truth) {
                hashbrown::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                hashbrown::hash_map::Entry::Vacant(entry) => {
                    let mut matches = Vec::new();
                    for inversions in 0..1u32 << truth.input_count {
                        let inversions = u8::try_from(inversions)
                            .expect("cover truth tables have at most eight inputs");
                        let cell_truth = truth.with_input_inversions(inversions);
                        append_binding_candidates(
                            catalog,
                            cell_truth,
                            0,
                            inversions,
                            &mut matches,
                        )?;
                    }
                    entry.insert(matches.into_boxed_slice())
                }
            };
            candidates.extend(exact.iter().copied().map(|mut candidate| {
                candidate.cut = compact_cut;
                candidate
            }));
            let Some(cares) = cares else {
                continue;
            };
            // Input inversion permutes the assignments of a truth table, so the
            // number of don't-care assignments is the same for every inversion
            // mask. Deciding once whether this cut has a fillable don't-care set
            // skips the whole permutation loop for the common cut that has none.
            let dont_care_count = (full & !(cares[cut_index] & full)).count_ones();
            if dont_care_count == 0 || dont_care_count > DONT_CARE_FILL_CAP {
                continue;
            }
            for inversions in 0..1u32 << cut.len() {
                let inversions =
                    u8::try_from(inversions).expect("cover cuts have at most eight inputs");
                let cell_truth = truth.with_input_inversions(inversions);
                let care = TruthTable {
                    input_count: cut.len(),
                    bits: cares[cut_index] & full,
                }
                .with_input_inversions(inversions)
                .bits;
                let dont_care = full & !care;
                debug_assert_eq!(dont_care.count_ones(), dont_care_count);
                let base = cell_truth.bits & care;
                let mut filling = 0u64;
                loop {
                    filling = filling.wrapping_sub(dont_care) & dont_care;
                    if filling == 0 {
                        break;
                    }
                    let variant = TruthTable {
                        input_count: cut.len(),
                        bits: base | filling,
                    };
                    if variant.bits != cell_truth.bits {
                        append_binding_candidates(
                            catalog,
                            variant,
                            compact_cut,
                            inversions,
                            candidates,
                        )?;
                    }
                }
                let variant = TruthTable {
                    input_count: cut.len(),
                    bits: base,
                };
                if variant.bits != cell_truth.bits {
                    append_binding_candidates(
                        catalog,
                        variant,
                        compact_cut,
                        inversions,
                        candidates,
                    )?;
                }
            }
        }
        u32::try_from(candidates.len())
            .map_err(|_| crate::SynthError::capacity("cover candidate count"))?;
        *phase_length = candidates.len() - start;
    }
    Ok(phase_lengths)
}

fn append_binding_candidates(
    catalog: &CombinationalCellCatalog,
    truth: TruthTable,
    cut: u8,
    inversions: u8,
    candidates: &mut Vec<Candidate>,
) -> Result<(), crate::SynthError> {
    let truth_input_count = u8::try_from(truth.input_count)
        .map_err(|_| crate::SynthError::capacity("cover truth input count exceeds capacity"))?;
    catalog
        .visit_cover_bindings(truth, |binding_id, binding| {
            candidates.push(Candidate::new(
                truth,
                truth_input_count,
                binding_id,
                cut,
                inversions,
                binding.inverted_input().map(|input| {
                    u8::try_from(input).expect("library binding input fits its compact signature")
                }),
            ));
        })
        .map_err(|_| {
            crate::SynthError::capacity(
                "target library cover binding arena exceeds 32-bit capacity",
            )
        })?;
    Ok(())
}

pub(crate) fn observability_cares(
    network: &LogicGraph,
    cuts: &CutDatabase,
    node_index: usize,
    consumer_index: usize,
) -> Option<Box<[u64]>> {
    let node = LogicNodeId::from_index(node_index);
    let consumer = LogicNodeId::from_index(consumer_index);
    let base = cuts
        .cuts(consumer)
        .iter()
        .copied()
        .filter(|cut| {
            !cut.contains(consumer) && cut.len() >= 2 && cut.len() <= 5 && !cut.contains(node)
        })
        .max_by_key(|cut| cut.len())?;
    let mut inputs = base.leaves().to_vec();
    inputs.push(node);
    let mut coverage = crate::boolean::logic::CoverageCheck::new(network, base.leaves());
    let cut_list = cuts.cuts(node);
    let projected =
        crate::boolean::logic::projected_cuts(&mut coverage, cut_list, |cut| cut.contains(node));
    let observed =
        crate::boolean::logic::projected_leaves(cut_list, &projected).collect::<Vec<_>>();
    let tables = network.truth_tables_for_inputs(consumer, &inputs, &observed);
    let (function, _) = tables.care_projection(consumer, &inputs)?;
    let window = base.len();
    let mut sensitive = 0u64;
    for assignment in 0..1usize << window {
        if function.bit(assignment) != function.bit(assignment | (1 << window)) {
            sensitive |= 1 << assignment;
        }
    }
    let full_window = (1u64 << (1u64 << window)) - 1;
    if sensitive == full_window {
        return None;
    }
    let cares = cut_list
        .iter()
        .zip(projected.iter())
        .map(|(cut, &projected)| {
            if !projected {
                return u64::MAX;
            }
            let mut leaf_functions = Vec::with_capacity(cut.len());
            for leaf in cut.leaves() {
                match tables.care_projection(*leaf, &inputs) {
                    Some((truth, _)) => leaf_functions.push(truth),
                    None => return u64::MAX,
                }
            }
            let mut care = 0u64;
            for assignment in 0..1usize << window {
                if sensitive & (1 << assignment) == 0 {
                    continue;
                }
                let mut pattern = 0usize;
                for (position, leaf) in leaf_functions.iter().enumerate() {
                    if leaf.bit(assignment) {
                        pattern |= 1 << position;
                    }
                }
                care |= 1 << pattern;
            }
            care
        })
        .collect();
    Some(cares)
}

pub(crate) fn enumerate_joints(
    network: &LogicGraph,
    cuts: &CutDatabase,
    truths: &CutTruthDatabase,
    catalog: &CombinationalCellCatalog,
    live_nodes: &[bool],
) -> Vec<Joint> {
    let mut groups = HashMap::<Box<[u32]>, Vec<(usize, usize, KCut)>>::new();
    for (index, &is_live) in live_nodes.iter().enumerate() {
        if !is_live {
            continue;
        }
        let node = LogicNodeId::from_index(index);
        if !network.node(node).is_cover_node() {
            continue;
        }
        for (cut_index, cut) in cuts.cuts(node).iter().copied().enumerate() {
            if cut.contains(node) || !catalog.has_joint_input_count(cut.len()) {
                continue;
            }
            let key = cut
                .leaves()
                .iter()
                .map(|leaf| {
                    u32::try_from(leaf.index())
                        .expect("logic node index is bounded by compact graph storage")
                })
                .collect::<Box<[u32]>>();
            let group = groups.entry(key).or_default();
            group.push((index, cut_index, cut));
        }
    }
    let mut joints = Vec::new();
    let mut seen = hashbrown::HashSet::<(usize, usize, KCut)>::new();
    let mut keys = groups.keys().cloned().collect::<Vec<_>>();
    keys.sort();
    for key in keys {
        let group = &groups[&key];
        let members = group
            .iter()
            .map(|&(index, cut_index, cut)| {
                let positive = truths.truth(LogicNodeId::from_index(index), cut_index);
                let assignments = 1usize << cut.len();
                let negative = TruthTable {
                    input_count: positive.input_count,
                    bits: positive.bits ^ full_truth_mask(assignments),
                };
                (index, cut, [positive, negative])
            })
            .collect::<Vec<_>>();
        for first in 0..members.len() {
            for second in first + 1..members.len() {
                let (first_node, cut, first_truths) = members[first];
                let (second_node, _, second_truths) = members[second];
                let pair = (
                    first_node.min(second_node),
                    first_node.max(second_node),
                    cut,
                );
                if seen.insert(pair) {
                    append_joint_pair(
                        catalog,
                        JointFunctionPair {
                            nodes: [first_node, second_node],
                            cut,
                            truths: [first_truths[0], second_truths[0]],
                        },
                        &mut joints,
                    );
                }
            }
        }
    }
    joints
}

#[derive(Clone, Copy)]
struct JointFunctionPair {
    nodes: [usize; 2],
    cut: KCut,
    truths: [TruthTable; 2],
}

fn append_joint_pair(
    catalog: &CombinationalCellCatalog,
    pair: JointFunctionPair,
    joints: &mut Vec<Joint>,
) {
    let JointFunctionPair {
        nodes: [first_node, second_node],
        cut,
        truths: [first_positive, second_positive],
    } = pair;
    let full = full_truth_mask(1usize << cut.len());
    for phases in 0..4usize {
        let first_root = phased(first_node, phases & 1 != 0);
        let second_root = phased(second_node, phases & 2 != 0);
        let first_truth = TruthTable {
            input_count: cut.len(),
            bits: first_positive.bits ^ if phases & 1 != 0 { full } else { 0 },
        };
        let second_truth = TruthTable {
            input_count: cut.len(),
            bits: second_positive.bits ^ if phases & 2 != 0 { full } else { 0 },
        };
        let mut best: Option<Joint> = None;
        for inversions in 0..1u32 << cut.len() {
            let inversions =
                u8::try_from(inversions).expect("joint cover cuts have at most eight inputs");
            let first_cell = first_truth.with_input_inversions(inversions);
            let second_cell = second_truth.with_input_inversions(inversions);
            let Some(binding) = catalog.best_joint_binding(first_cell, second_cell) else {
                continue;
            };
            let cost = catalog.joint_cost(binding);
            if best
                .as_ref()
                .is_none_or(|current| compare_cell_cost(cost, current.cost).is_lt())
            {
                best = Some(Joint {
                    cut,
                    inversions,
                    binding,
                    cost,
                    slots: [slot(first_root), slot(second_root)],
                    truths: [first_cell, second_cell],
                });
            }
        }
        joints.extend(best);
    }
}

fn phased(index: usize, inverted: bool) -> LogicNodeId {
    let node = LogicNodeId::from_index(index);
    if inverted { node.inverted() } else { node }
}
