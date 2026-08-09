// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::TimingOptimizationSession;
use super::candidate::{CandidateDisposition, PostmapCandidate};
use crate::{OptimizationPhase, SynthesisOptions};
use opto_ir::mapped::MappedNetlist;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RejectionPolicy {
    KeepWhole,
    Bisect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EvaluationPolicy {
    /// Finish the finite topology forest independently of the `QoR` search
    /// budget. HFNS and electrical legalization use this policy.
    TopologyComplete,
    /// Charge exact evaluations to the deterministic post-topology `QoR`
    /// budget. Local resynthesis, cloning, sizing, and pin swapping use it.
    QorBudgeted,
}

/// Evaluates a stable ordered forest through the one post-map transaction
/// boundary.
///
/// Domain modules own plan construction and materialization. This executor
/// owns exact evaluation, deterministic rejection splitting, the explicit
/// topology/QoR budget boundary, and the invariant that a freshly materialized
/// transaction cannot be stale.
pub(super) fn evaluate<T, F>(
    plans: &[T],
    phase: OptimizationPhase,
    rejection: RejectionPolicy,
    evaluation: EvaluationPolicy,
    session: &mut TimingOptimizationSession<'_>,
    materialize: F,
) -> Result<bool, crate::SynthError>
where
    F: Fn(
        &MappedNetlist,
        &crate::ImplementationDb,
        &SynthesisOptions,
        &[T],
    ) -> Result<Option<PostmapCandidate>, crate::SynthError>,
{
    if plans.is_empty() {
        return Ok(false);
    }
    let mut accepted = false;
    let mut pending = vec![(0usize, plans.len())];
    while let Some((start, end)) = pending.pop() {
        if evaluation == EvaluationPolicy::QorBudgeted && session.qor_budget_exhausted() {
            break;
        }
        let Some(candidate) = materialize(
            session.mapped,
            session.implementations,
            session.options,
            &plans[start..end],
        )?
        else {
            continue;
        };
        let disposition = match evaluation {
            EvaluationPolicy::TopologyComplete => session.evaluate_topology(candidate, phase)?,
            EvaluationPolicy::QorBudgeted => session.evaluate_qor(candidate, phase)?,
        };
        match disposition {
            CandidateDisposition::Accepted(()) => accepted = true,
            CandidateDisposition::Rejected
                if rejection == RejectionPolicy::Bisect && end - start > 1 =>
            {
                let middle = start + (end - start) / 2;
                pending.push((middle, end));
                pending.push((start, middle));
            }
            CandidateDisposition::Rejected => {}
            CandidateDisposition::Stale => {
                return Err(crate::SynthError::invariant(format!(
                    "fresh {} forest transaction became stale",
                    phase.stage().as_str()
                )));
            }
        }
    }
    Ok(accepted)
}
