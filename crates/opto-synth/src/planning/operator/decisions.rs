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
            crate::boolean::bitblast::implementation_providers().into(),
        )
    }

    #[cfg(test)]
    pub(crate) fn for_unfused_module(module: &word::WordModule) -> Result<Self, crate::SynthError> {
        Self::build(
            module,
            false,
            crate::boolean::bitblast::implementation_providers().into(),
        )
    }

    pub(crate) fn for_private_region(
        module: &word::WordModule,
        providers: Box<[&'static dyn ImplementationProvider]>,
    ) -> Result<Self, crate::SynthError> {
        Self::build(module, true, providers)
    }

    fn build(
        module: &word::WordModule,
        fuse_arithmetic: bool,
        providers: Box<[&'static dyn ImplementationProvider]>,
    ) -> Result<Self, crate::SynthError> {
        let observable = ObservableBits::analyze(module)?;
        let catalog = Arc::new(OperatorCatalog::for_module(
            module,
            &observable,
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

    pub(crate) fn select_for_budget(
        &mut self,
        target: &crate::planning::regional::StructuralTargetModel,
        budget: Option<f64>,
    ) -> Result<(), crate::SynthError> {
        let mut selections = Vec::with_capacity(self.operators().len());
        for operator in self.operators() {
            let mut best = None;
            for &candidate in self.candidates(operator.id()) {
                let key = target.score_for_budget(self.candidate_estimate(candidate)?, budget)?;
                let stable = self.candidate_recipe_name(candidate.id()).unwrap_or("");
                if best.as_ref().is_none_or(|(_, best_key, best_stable)| {
                    (key, stable) < (*best_key, *best_stable)
                }) {
                    best = Some((candidate.id(), key, stable));
                }
            }
            if let Some((candidate, _, _)) = best {
                selections.push(candidate);
            }
        }
        for candidate in selections {
            self.select_candidate(candidate)?;
        }
        Ok(())
    }

    pub(crate) fn operator_for_source_operation(
        &self,
        operation: word::OpId,
    ) -> Option<OperatorId> {
        self.catalog.operator_for_source_operation(operation)
    }
}

#[cfg(test)]
mod tests;
