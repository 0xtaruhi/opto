// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::TimingOptimizationSession;
use super::buffering::{self, BufferBranchPlan};
use super::forest::{self, EvaluationPolicy};
use crate::OptimizationPhase;
use opto_ir::mapped::{ConnectionSignal, NetId, PinId};
use std::collections::BTreeMap;

#[derive(Debug)]
struct RepairBranch {
    net: NetId,
    sinks: Vec<PinId>,
}

#[derive(Debug)]
struct RankedRepairBranch {
    normalized_violation: f64,
    capacitance: f64,
    fanout: f64,
    sinks: Vec<PinId>,
}

/// Legalizes residual max-fanout, max-capacitance, and max-transition
/// violations after whole-net fanout-tree synthesis.
///
/// One planning generation reads the complete committed DRC set, selects at
/// most one electrically ranked branch per source net, and evaluates the
/// resulting forest transactionally. Rejected forests are bisected only at
/// source-net boundaries. Accepted edits start a new generation from fresh
/// exact STA; there is no serial "repair one branch and rediscover the world"
/// control path and no QoR-budget gate on electrical legality.
pub(super) fn legalize(
    session: &mut TimingOptimizationSession<'_>,
    buffer_candidates: &[usize],
) -> Result<(), crate::SynthError> {
    if buffer_candidates.is_empty() {
        return Ok(());
    }
    let mut generation = 0usize;
    while session.has_design_rule_violations() {
        let branches = repair_generation(session)?;
        if branches.is_empty() {
            break;
        }
        let mut changed = false;
        for &buffer_index in buffer_candidates {
            let plans = branches
                .iter()
                .enumerate()
                .map(|(ordinal, branch)| BufferBranchPlan {
                    net: branch.net,
                    sinks: branch.sinks.clone(),
                    buffer_index,
                    instance_name: format!("U_electrical_buffer_{generation}_{ordinal}"),
                    net_name: format!("_electrical_net_{generation}_{ordinal}"),
                })
                .collect::<Vec<_>>();
            if forest::evaluate(
                &plans,
                OptimizationPhase::DesignRuleRepair,
                EvaluationPolicy::Complete,
                session,
                |mapped, implementations, options, plans| {
                    buffering::buffer_branch_forest_delta(
                        mapped,
                        implementations,
                        &options.target_cells,
                        plans,
                    )
                },
            )? {
                changed = true;
                break;
            }
        }
        if !changed {
            break;
        }
        generation = generation.checked_add(1).ok_or_else(|| {
            crate::SynthError::capacity("electrical legalization generation count exceeds capacity")
        })?;
    }
    Ok(())
}

fn repair_generation(
    session: &TimingOptimizationSession<'_>,
) -> Result<Vec<RepairBranch>, crate::SynthError> {
    let mut branches = BTreeMap::<NetId, RankedRepairBranch>::new();
    for violation in session.design_rules() {
        let Some(net) = violation.mapped_net else {
            continue;
        };
        for mut sinks in
            buffering::buffer_branches(session.mapped, &session.options.target_cells, violation)?
        {
            sinks.sort_unstable();
            sinks.dedup();
            if sinks.is_empty() {
                continue;
            }
            validate_branch_net(session, net, &sinks)?;
            let (capacitance, fanout) = branch_load(session, &sinks)?;
            let ranked = RankedRepairBranch {
                normalized_violation: violation.actual / violation.limit,
                capacitance,
                fanout,
                sinks,
            };
            match branches.entry(net) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(ranked);
                }
                std::collections::btree_map::Entry::Occupied(mut entry)
                    if repair_branch_is_better(&ranked, entry.get()) =>
                {
                    entry.insert(ranked);
                }
                std::collections::btree_map::Entry::Occupied(_) => {}
            }
        }
    }
    Ok(branches
        .into_iter()
        .map(|(net, branch)| RepairBranch {
            net,
            sinks: branch.sinks,
        })
        .collect())
}

fn validate_branch_net(
    session: &TimingOptimizationSession<'_>,
    expected: NetId,
    sinks: &[PinId],
) -> Result<(), crate::SynthError> {
    for &pin in sinks {
        let signal = session
            .mapped
            .connection(pin)
            .ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "electrical repair sink pin {pin:?} disappeared"
                ))
            })?
            .signal;
        if signal != ConnectionSignal::Net(expected) {
            return Err(crate::SynthError::invariant(
                "electrical repair branch does not belong to its violating net",
            ));
        }
    }
    Ok(())
}

fn branch_load(
    session: &TimingOptimizationSession<'_>,
    sinks: &[PinId],
) -> Result<(f64, f64), crate::SynthError> {
    sinks
        .iter()
        .try_fold((0.0, 0.0), |(capacitance, fanout), &pin| {
            let target =
                buffering::library_pin(session.mapped, &session.options.target_cells, pin)?
                    .ok_or_else(|| {
                        crate::SynthError::invariant(
                            "electrical repair sink has no target-library pin identity",
                        )
                    })?;
            Ok((
                capacitance + target.design_input_capacitance(),
                fanout + target.design_fanout_load(),
            ))
        })
}

fn repair_branch_is_better(candidate: &RankedRepairBranch, current: &RankedRepairBranch) -> bool {
    candidate
        .normalized_violation
        .total_cmp(&current.normalized_violation)
        .then_with(|| candidate.capacitance.total_cmp(&current.capacitance))
        .then_with(|| candidate.fanout.total_cmp(&current.fanout))
        .then_with(|| candidate.sinks.len().cmp(&current.sinks.len()))
        .then_with(|| current.sinks.cmp(&candidate.sinks))
        .is_gt()
}
