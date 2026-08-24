// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::catalog::{ImplementationCandidate, ImplementationCandidateId, OperatorCatalog};
use super::demand::ObservableBits;
use super::{OperatorId, SemanticOperator};
use crate::planning::architecture::ArithmeticTerm;
use crate::planning::provider::ImplementationProvider;
use crate::planning::provider::StructuralEstimate;
use opto_ir::word;
use std::sync::Arc;

const GLOBAL_SELECTION_ROUNDS: usize = 4;

#[derive(Clone)]
struct CandidateSummary {
    candidate: ImplementationCandidateId,
    violation: u64,
    physical: u64,
    depth: u64,
    stable: Box<str>,
}

struct RegionSummary {
    row: usize,
    groups: Vec<Vec<CandidateSummary>>,
}

#[derive(Clone, Copy)]
struct FusionSelection {
    left: (usize, usize, usize),
    right: (usize, usize, usize),
}

#[derive(Debug, Clone)]
pub(crate) struct ArchitectureDecisions {
    catalog: Arc<OperatorCatalog>,
    selections: Vec<Selection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Selection {
    operator: OperatorId,
    candidate: ImplementationCandidateId,
}

impl ArchitectureDecisions {
    pub(crate) fn for_regional_shell(module: &word::WordModule) -> Self {
        Self {
            catalog: Arc::new(OperatorCatalog::regional_shell(module.operations().len())),
            selections: Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_module(module: &word::WordModule) -> Result<Self, crate::SynthError> {
        Self::for_private_region(
            module,
            &[],
            crate::boolean::bitblast::implementation_providers().into(),
        )
    }

    #[cfg(test)]
    pub(crate) fn for_unfused_module(module: &word::WordModule) -> Result<Self, crate::SynthError> {
        Self::build(
            module,
            &[],
            false,
            crate::boolean::bitblast::implementation_providers().into(),
        )
    }

    pub(crate) fn for_private_region(
        module: &word::WordModule,
        observed_values: &[word::ValueId],
        providers: Box<[&'static dyn ImplementationProvider]>,
    ) -> Result<Self, crate::SynthError> {
        Self::build(module, observed_values, true, providers)
    }

    fn build(
        module: &word::WordModule,
        observed_values: &[word::ValueId],
        fuse_arithmetic: bool,
        providers: Box<[&'static dyn ImplementationProvider]>,
    ) -> Result<Self, crate::SynthError> {
        let observable = ObservableBits::analyze_with_values(module, observed_values)?;
        let catalog = Arc::new(OperatorCatalog::for_module(
            module,
            &observable,
            observed_values,
            fuse_arithmetic,
            providers,
        )?);
        if let Some(operator) = catalog
            .operators()
            .iter()
            .find(|operator| catalog.candidates(operator.id()).is_empty())
        {
            return Err(crate::SynthError::invariant(format!(
                "operator {} has no implementation candidates",
                operator.id().raw()
            )));
        }
        Ok(Self {
            catalog,
            selections: Vec::new(),
        })
    }

    pub(crate) fn operators(&self) -> &[SemanticOperator] {
        self.catalog.operators()
    }

    pub(crate) fn operator(&self, id: OperatorId) -> Option<SemanticOperator> {
        self.catalog.operator(id)
    }

    pub(crate) fn source_operations(&self, id: OperatorId) -> &[word::OpId] {
        self.catalog.source_operations(id)
    }

    pub(crate) fn arithmetic_terms(&self, id: OperatorId) -> &[ArithmeticTerm] {
        self.catalog.arithmetic_terms(id)
    }

    pub(crate) fn operator_inputs(
        &self,
        operator: SemanticOperator,
    ) -> impl Iterator<Item = word::ValueId> + '_ {
        let terms = self.arithmetic_terms(operator.id());
        terms
            .iter()
            .copied()
            .flat_map(ArithmeticTerm::inputs)
            .chain(
                operator
                    .inputs()
                    .into_iter()
                    .filter(move |_| terms.is_empty()),
            )
    }

    pub(crate) fn candidates(&self, operator: OperatorId) -> &[ImplementationCandidate] {
        self.catalog.candidates(operator)
    }

    pub(crate) fn selected_candidate(
        &self,
        operator: OperatorId,
    ) -> Option<ImplementationCandidate> {
        let candidate = match self
            .selections
            .binary_search_by_key(&operator, |selection| selection.operator)
        {
            Ok(index) => self.selections[index].candidate,
            Err(_) => self.catalog.candidates(operator).first()?.id(),
        };
        self.catalog.candidate(candidate)
    }

    pub(crate) fn candidate_recipe_name(
        &self,
        candidate: ImplementationCandidateId,
    ) -> Option<&str> {
        self.catalog.candidate_recipe_name(candidate)
    }

    pub(crate) fn candidate_implementation_name(
        &self,
        candidate: ImplementationCandidateId,
    ) -> Option<&str> {
        self.catalog.candidate_implementation_name(candidate)
    }

    pub(crate) fn candidate_module_name(
        &self,
        candidate: ImplementationCandidateId,
    ) -> Option<&str> {
        self.catalog.candidate_module_name(candidate)
    }

    pub(crate) fn candidate_operation_mnemonic(
        &self,
        candidate: ImplementationCandidateId,
    ) -> Option<&str> {
        self.catalog.candidate_operation_mnemonic(candidate)
    }

    pub(crate) fn candidate_estimate(
        &self,
        candidate: ImplementationCandidate,
    ) -> Result<StructuralEstimate, crate::SynthError> {
        self.catalog.candidate_estimate(candidate)
    }

    pub(crate) fn select_candidate(
        &mut self,
        candidate: ImplementationCandidateId,
    ) -> Result<(), crate::SynthError> {
        let candidate = self.catalog.candidate(candidate).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "unknown implementation candidate {}",
                candidate.raw()
            ))
        })?;
        let default = self
            .catalog
            .candidates(candidate.operator())
            .first()
            .ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "operator {} has no implementation candidates",
                    candidate.operator().raw()
                ))
            })?
            .id();
        match self
            .selections
            .binary_search_by_key(&candidate.operator(), |selection| selection.operator)
        {
            Ok(index) if candidate.id() == default => {
                self.selections.remove(index);
            }
            Ok(index) => self.selections[index].candidate = candidate.id(),
            Err(_) if candidate.id() == default => {}
            Err(index) => self.selections.insert(
                index,
                Selection {
                    operator: candidate.operator(),
                    candidate: candidate.id(),
                },
            ),
        }
        Ok(())
    }

    pub(crate) fn select_design_for_work(
        decisions: &mut [&mut Self],
        target: &crate::planning::regional::StructuralTargetModel,
        budgets: &[Option<f64>],
        work: &crate::regional::WorkGraph,
        runtime: &opto_runtime::ExecutionContext,
    ) -> Result<(), crate::SynthError> {
        if decisions.len() != budgets.len() {
            return Err(crate::SynthError::invariant(
                "architecture decision groups do not align with design timing budgets",
            ));
        }
        let views = decisions
            .iter()
            .map(|decisions| &**decisions)
            .collect::<Vec<_>>();
        let mut reduced = work.map_reduce(
            runtime,
            |row, _, _| {
                let groups = views[row]
                    .operators()
                    .iter()
                    .map(|operator| {
                        views[row]
                            .candidates(operator.id())
                            .iter()
                            .map(|&candidate| {
                                let (violation, physical, depth) = target.score_for_budget(
                                    views[row].candidate_estimate(candidate)?,
                                    budgets[row],
                                )?;
                                Ok(CandidateSummary {
                                    candidate: candidate.id(),
                                    violation,
                                    physical,
                                    depth,
                                    stable: views[row]
                                        .candidate_recipe_name(candidate.id())
                                        .unwrap_or("")
                                        .into(),
                                })
                            })
                            .collect::<Result<Vec<_>, crate::SynthError>>()
                    })
                    .collect::<Result<Vec<_>, crate::SynthError>>()?;
                Ok((0_u8, RegionSummary { row, groups }))
            },
            |_, rows, _| {
                let mut regions = rows
                    .into_iter()
                    .map(|(_, summary)| summary)
                    .collect::<Vec<_>>();
                regions.sort_by_key(|region| region.row);
                Ok(regions)
            },
        )?;
        let summaries = reduced
            .pop()
            .map(|(_, summaries)| summaries)
            .unwrap_or_default();
        if !reduced.is_empty()
            || summaries.len() != decisions.len()
            || summaries
                .iter()
                .enumerate()
                .any(|(row, summary)| summary.row != row)
        {
            return Err(crate::SynthError::invariant(
                "global architecture reduction does not cover every work item",
            ));
        }
        let mut selected = summaries
            .iter()
            .map(|region| {
                region
                    .groups
                    .iter()
                    .map(|candidates| {
                        best_candidate(candidates, 1).ok_or_else(|| {
                            crate::SynthError::invariant(
                                "architecture decision group has no candidate",
                            )
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut prices = vec![1; summaries.len()].into_boxed_slice();
        for _ in 0..GLOBAL_SELECTION_ROUNDS {
            let local = summaries
                .iter()
                .zip(&selected)
                .map(|(region, selected)| {
                    region
                        .groups
                        .iter()
                        .zip(selected)
                        .map(|(candidates, &candidate)| candidates[candidate].violation)
                        .max()
                        .unwrap_or(0)
                        .saturating_add(1)
                })
                .collect::<Vec<_>>();
            prices = work.backward_prices(&local, &prices)?;
            for (row, region) in summaries.iter().enumerate() {
                for (group, candidates) in region.groups.iter().enumerate() {
                    selected[row][group] =
                        best_candidate(candidates, prices[row]).ok_or_else(|| {
                            crate::SynthError::invariant(
                                "architecture decision group has no priced candidate",
                            )
                        })?;
                }
            }
        }
        let fusion = work.fusion_plan()?;
        for wave in 0..fusion.wave_count() {
            let proposals = fusion.execute_wave(wave, runtime, |item, _| {
                let [left, right] = item.members();
                Ok(joint_selection(&summaries, &selected, &prices, left, right))
            })?;
            for proposal in proposals.into_iter().flatten() {
                selected[proposal.left.0][proposal.left.1] = proposal.left.2;
                selected[proposal.right.0][proposal.right.1] = proposal.right.2;
            }
        }
        for (row, groups) in selected.into_iter().enumerate() {
            for (group, candidate) in groups.into_iter().enumerate() {
                decisions[row]
                    .select_candidate(summaries[row].groups[group][candidate].candidate)?;
            }
        }
        Ok(())
    }

    pub(crate) fn operator_for_source_operation(
        &self,
        operation: word::OpId,
    ) -> Option<OperatorId> {
        self.catalog.operator_for_source_operation(operation)
    }

    pub(crate) fn is_operation_elided(&self, operation: word::OpId) -> bool {
        self.catalog.is_operation_elided(operation)
    }
}

fn best_candidate(candidates: &[CandidateSummary], price: u64) -> Option<usize> {
    candidates
        .iter()
        .enumerate()
        .min_by_key(|(_, candidate)| candidate_key(candidate, price))
        .map(|(index, _)| index)
}

fn candidate_key(candidate: &CandidateSummary, price: u64) -> (u64, u128, u64, &str) {
    (
        candidate.violation,
        u128::from(candidate.depth)
            .saturating_mul(u128::from(price))
            .saturating_add(u128::from(candidate.physical)),
        candidate.physical,
        &candidate.stable,
    )
}

fn joint_selection(
    summaries: &[RegionSummary],
    selected: &[Vec<usize>],
    prices: &[u64],
    left: usize,
    right: usize,
) -> Option<FusionSelection> {
    let critical_group = |row: usize| {
        summaries[row]
            .groups
            .iter()
            .zip(&selected[row])
            .enumerate()
            .max_by_key(|(_, (candidates, selected))| candidates[**selected].depth)
            .map(|(group, _)| group)
    };
    let left_group = critical_group(left)?;
    let right_group = critical_group(right)?;
    let mut best = None;
    for (left_candidate, left_summary) in summaries[left].groups[left_group].iter().enumerate() {
        for (right_candidate, right_summary) in
            summaries[right].groups[right_group].iter().enumerate()
        {
            let key = (
                left_summary.violation.max(right_summary.violation),
                left_summary
                    .violation
                    .saturating_add(right_summary.violation),
                u128::from(left_summary.depth)
                    .saturating_mul(u128::from(prices[left]))
                    .saturating_add(
                        u128::from(right_summary.depth).saturating_mul(u128::from(prices[right])),
                    )
                    .saturating_add(u128::from(left_summary.physical))
                    .saturating_add(u128::from(right_summary.physical)),
                left_summary.physical.saturating_add(right_summary.physical),
                left_summary.stable.as_ref(),
                right_summary.stable.as_ref(),
            );
            if best.as_ref().is_none_or(|(best_key, _, _)| key < *best_key) {
                best = Some((key, left_candidate, right_candidate));
            }
        }
    }
    best.map(|(_, left_candidate, right_candidate)| FusionSelection {
        left: (left, left_group, left_candidate),
        right: (right, right_group, right_candidate),
    })
}

#[cfg(test)]
mod tests;
