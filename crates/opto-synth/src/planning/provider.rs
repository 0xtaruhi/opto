// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::SemanticOperator;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub(crate) struct ImplementationProviderId(u8);

impl ImplementationProviderId {
    pub(crate) const fn from_raw(raw: u8) -> Self {
        Self(raw)
    }

    pub(crate) fn index(self) -> usize {
        usize::from(self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub(crate) struct ProviderRecipeId(u32);

impl ProviderRecipeId {
    pub(crate) const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    pub(crate) const fn raw(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructuralEstimate {
    pub logic_depth: u32,
    pub logic_units: u64,
    pub wiring_units: u64,
}

pub(crate) trait ImplementationProvider: fmt::Debug + Send + Sync {
    fn resource_name(&self) -> &str;

    fn enumerate_recipes(&self, operator: SemanticOperator, emit: &mut dyn FnMut(ProviderRecipeId));

    fn recipe_name(&self, recipe: ProviderRecipeId) -> Option<&str>;

    fn module_name(&self, operator: SemanticOperator) -> Option<&str>;

    fn operation_mnemonic(&self, operator: SemanticOperator) -> Option<&str>;

    fn implementation_name(&self, recipe: ProviderRecipeId) -> Option<&str>;

    fn structural_estimate(
        &self,
        recipe: ProviderRecipeId,
        operator: SemanticOperator,
    ) -> Result<StructuralEstimate, crate::SynthError>;
}
