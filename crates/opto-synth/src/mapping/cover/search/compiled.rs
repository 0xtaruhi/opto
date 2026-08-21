// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Compile-once cut, truth, and target-match storage.

use super::{
    Candidate, CandidateContext, CandidateIndex, CandidateRange, CombinationalCellCatalog,
    CompiledMapping, CutDatabase, CutTruthDatabase, ExecutionContext, HashMap, LogicGraph,
    LogicNodeId, enumerate_joints, node_candidates, planner,
};

impl CompiledMapping {
    pub(super) fn for_choices(
        choices: &crate::boolean::logic::ChoiceGraph,
        outputs: &[LogicNodeId],
        catalog: &CombinationalCellCatalog,
        runtime: &ExecutionContext,
    ) -> Result<Self, crate::SynthError> {
        let cuts = {
            let _profile =
                crate::api::diagnostics::ProfileSpan::new(catalog.diagnostics().timing, || {
                    "cover.cut_enumeration".to_string()
                });
            CutDatabase::build_choices_parallel(
                choices,
                crate::boolean::logic::MAX_MATCH_INPUTS,
                runtime,
            )?
        };
        Self::compile(
            choices.network(),
            cuts,
            choices.live_nodes(outputs),
            outputs,
            catalog,
            runtime,
        )
    }

    #[cfg(test)]
    pub(super) fn for_network(
        network: &LogicGraph,
        outputs: &[LogicNodeId],
        catalog: &CombinationalCellCatalog,
        runtime: &ExecutionContext,
    ) -> Result<Self, crate::SynthError> {
        let cuts =
            CutDatabase::build_parallel(network, crate::boolean::logic::MAX_MATCH_INPUTS, runtime)?;
        Self::compile(
            network,
            cuts,
            network.live_nodes(outputs),
            outputs,
            catalog,
            runtime,
        )
    }

    fn compile(
        network: &LogicGraph,
        cuts: CutDatabase,
        live_nodes: Box<[bool]>,
        outputs: &[LogicNodeId],
        catalog: &CombinationalCellCatalog,
        runtime: &ExecutionContext,
    ) -> Result<Self, crate::SynthError> {
        let node_count = network.node_count();
        if live_nodes.len() != node_count || outputs.iter().any(|node| !live_nodes[node.index()]) {
            return Err(crate::SynthError::invariant(
                "compiled mapping live set does not cover its outputs",
            ));
        }
        let truths = {
            let _profile =
                crate::api::diagnostics::ProfileSpan::new(catalog.diagnostics().timing, || {
                    "cover.truth_evaluation".to_string()
                });
            CutTruthDatabase::build_parallel(network, &cuts, runtime)?
        };
        let trace = crate::api::diagnostics::SynthTrace::new(catalog.diagnostics().timing);
        let started = std::time::Instant::now();
        let mut consumer_entries = Vec::new();
        for (index, &is_live) in live_nodes.iter().enumerate() {
            if !is_live {
                continue;
            }
            let consumer = u32::try_from(index).map_err(|_| {
                crate::SynthError::capacity("logic node ID exceeds 32-bit capacity")
            })?;
            let mut unique_fanins = smallvec::SmallVec::<[usize; 3]>::new();
            for fanin in network.node(LogicNodeId::from_index(index)).fanins() {
                if !unique_fanins.contains(&fanin.index()) {
                    unique_fanins.push(fanin.index());
                    consumer_entries.push((fanin.index(), consumer));
                }
            }
        }
        let consumers = opto_core::PackedRows::try_from_entries(node_count, consumer_entries)
            .map_err(|_| crate::SynthError::capacity("logic consumer adjacency"))?;
        let mut output_nodes = vec![false; node_count];
        for &output in outputs {
            output_nodes[output.index()] = true;
        }
        let mut node_cares: Vec<Option<Box<[u64]>>> = vec![None; node_count];
        let mut exact_only = vec![true; node_count];
        let mut levels = vec![Vec::new(); network.max_level() + 1];
        for (index, &is_live) in live_nodes.iter().enumerate() {
            if is_live {
                levels[network.level(LogicNodeId::from_index(index)) as usize].push(index);
            }
        }
        for nodes in levels.into_iter().rev() {
            let analyzed = runtime.analyze_indexed(nodes.len(), |position| {
                let index = nodes[position];
                let (cares, exact) = planner::analyze_node_cares(
                    network,
                    &cuts,
                    index,
                    consumers.row(index),
                    output_nodes[index],
                    &exact_only,
                );
                Ok::<_, crate::SynthError>((index, cares, exact))
            })?;
            for (index, cares, exact) in analyzed {
                node_cares[index] = cares;
                exact_only[index] = exact;
            }
        }
        crate::api::diagnostics::trace!(
            trace,
            "cover.cares",
            "nodes={node_count} wall={:?}",
            started.elapsed()
        );

        let started = std::time::Instant::now();
        let shards = runtime.fold_indexed(
            node_count,
            || (Vec::new(), Vec::new(), HashMap::new()),
            |(candidates, lengths, exact_cache), index| {
                let lengths_for_node = if live_nodes[index] {
                    node_candidates(
                        CandidateContext {
                            network,
                            cuts: &cuts,
                            truths: &truths,
                            catalog,
                        },
                        index,
                        node_cares[index].as_deref(),
                        candidates,
                        exact_cache,
                    )?
                } else {
                    [0, 0]
                };
                lengths.push(lengths_for_node);
                Ok::<_, crate::SynthError>(())
            },
        )?;
        let candidate_count = shards
            .iter()
            .map(|(candidates, _, _)| candidates.len())
            .sum::<usize>();
        let mut arenas = Vec::with_capacity(shards.len());
        let mut ranges = Vec::with_capacity(node_count * 2);
        for (arena_index, (candidates, lengths, _)) in shards.into_iter().enumerate() {
            let arena = u32::try_from(arena_index).map_err(|_| {
                crate::SynthError::capacity("cover candidate shard count exceeds 32-bit capacity")
            })?;
            let mut start = 0usize;
            for phase_lengths in lengths {
                for len in phase_lengths {
                    ranges.push(CandidateRange {
                        arena,
                        start: start.try_into().map_err(|_| {
                            crate::SynthError::capacity(
                                "cover candidate shard exceeds 32-bit capacity",
                            )
                        })?,
                        len: len.try_into().map_err(|_| {
                            crate::SynthError::capacity(
                                "cover candidate range exceeds 32-bit capacity",
                            )
                        })?,
                    });
                    start += len;
                }
            }
            if start != candidates.len() {
                return Err(crate::SynthError::invariant(
                    "cover candidate ranges do not span their shard",
                ));
            }
            arenas.push(candidates);
        }
        if ranges.len() != node_count * 2 {
            return Err(crate::SynthError::invariant(
                "cover candidate ranges do not align with logic phases",
            ));
        }
        let candidates = CandidateIndex {
            arenas: arenas.into_boxed_slice(),
            ranges: ranges.into_boxed_slice(),
        };
        crate::api::diagnostics::trace!(
            trace,
            "cover.candidates",
            "candidates={candidate_count} bytes={} wall={:?}",
            candidate_count * std::mem::size_of::<Candidate>(),
            started.elapsed()
        );

        let started = std::time::Instant::now();
        let joints = enumerate_joints(network, &cuts, &truths, catalog, &live_nodes);
        let mut slot_joint_entries = Vec::with_capacity(joints.len().saturating_mul(2));
        let mut node_joint_entries = Vec::with_capacity(joints.len());
        for (index, joint) in joints.iter().enumerate() {
            let joint_id = u32::try_from(index).map_err(|_| {
                crate::SynthError::capacity("cover joint ID exceeds 32-bit capacity")
            })?;
            for &slot in &joint.slots {
                slot_joint_entries.push((slot, joint_id));
            }
            node_joint_entries.push((joint.slots[0].min(joint.slots[1]) / 2, joint_id));
        }
        let slot_joints =
            opto_core::PackedRows::try_from_entries(node_count * 2, slot_joint_entries)
                .map_err(|_| crate::SynthError::capacity("cover slot-joint adjacency"))?;
        let joints_by_node =
            opto_core::PackedRows::try_from_entries(node_count, node_joint_entries)
                .map_err(|_| crate::SynthError::capacity("cover node-joint adjacency"))?;
        crate::api::diagnostics::trace!(
            trace,
            "cover.joint_enumeration",
            "joints={} wall={:?}",
            joints.len(),
            started.elapsed()
        );
        Ok(Self {
            cuts,
            truths,
            live_nodes,
            candidates,
            joints: joints.into_boxed_slice(),
            slot_joints,
            joints_by_node,
        })
    }
}
