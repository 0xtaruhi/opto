// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::TimingOptimizationSession;
use super::candidate::{CandidateDisposition, PostmapCandidate};
use crate::{OptimizationPhase, SynthesisOptions};
use opto_ir::mapped::MappedNetlist;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EvaluationPolicy {
    /// Finish the finite forest independently of the `QoR` search budget.
    Complete,
    /// Charge exact evaluations to the deterministic post-topology `QoR`
    /// budget. Local resynthesis, cloning, sizing, and pin swapping use it.
    QorBudgeted,
}

pub(super) trait ForestSession {
    fn mapped(&self) -> &MappedNetlist;
    fn implementations(&self) -> &crate::ImplementationDb;
    fn options(&self) -> &SynthesisOptions;
    fn qor_budget_exhausted(&self) -> bool;
    fn evaluate_forest_candidate(
        &mut self,
        candidate: PostmapCandidate,
        phase: OptimizationPhase,
        policy: EvaluationPolicy,
    ) -> Result<CandidateDisposition<()>, crate::SynthError>;
    fn publish_forest_progress(&mut self, phase: OptimizationPhase);
}

impl ForestSession for TimingOptimizationSession<'_> {
    fn mapped(&self) -> &MappedNetlist {
        self.mapped
    }

    fn implementations(&self) -> &crate::ImplementationDb {
        self.implementations
    }

    fn options(&self) -> &SynthesisOptions {
        self.options
    }

    fn qor_budget_exhausted(&self) -> bool {
        self.qor_budget_exhausted()
    }

    fn evaluate_forest_candidate(
        &mut self,
        candidate: PostmapCandidate,
        phase: OptimizationPhase,
        policy: EvaluationPolicy,
    ) -> Result<CandidateDisposition<()>, crate::SynthError> {
        match policy {
            EvaluationPolicy::Complete => self.evaluate_topology(candidate, phase),
            EvaluationPolicy::QorBudgeted => self.evaluate_qor(candidate, phase),
        }
    }

    fn publish_forest_progress(&mut self, phase: OptimizationPhase) {
        TimingOptimizationSession::publish_progress(self, phase);
    }
}

/// Evaluates a stable ordered forest through the one post-map transaction
/// boundary.
///
/// Domain modules own plan construction and materialization. This executor
/// owns exact evaluation, deterministic rejection splitting, the explicit
/// topology/QoR budget boundary, and the invariant that a freshly materialized
/// transaction cannot be stale.
pub(super) fn evaluate<T, F, S>(
    plans: &[T],
    phase: OptimizationPhase,
    evaluation: EvaluationPolicy,
    session: &mut S,
    materialize: F,
) -> Result<bool, crate::SynthError>
where
    S: ForestSession,
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
            session.mapped(),
            session.implementations(),
            session.options(),
            &plans[start..end],
        )?
        else {
            continue;
        };
        let disposition = session.evaluate_forest_candidate(candidate, phase, evaluation)?;
        match disposition {
            CandidateDisposition::Accepted(()) => accepted = true,
            CandidateDisposition::Rejected if end - start > 1 => {
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
    session.publish_forest_progress(phase);
    Ok(accepted)
}
