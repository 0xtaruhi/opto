// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Canonical integer division and remainder implementation provider.

use super::{BitBlaster, ImplementationRequest};
use crate::planning::provider::{ImplementationProvider, ProviderRecipeId, StructuralEstimate};
use crate::{OperatorKind, SemanticOperator};

const CANONICAL: ProviderRecipeId = ProviderRecipeId::from_raw(0);

#[derive(Debug)]
struct DivisionProvider;

impl ImplementationProvider for DivisionProvider {
    fn resource_name(&self) -> &'static str {
        "integer division"
    }

    fn enumerate_recipes(
        &self,
        operator: SemanticOperator,
        emit: &mut dyn FnMut(ProviderRecipeId),
    ) {
        if matches!(operator.kind(), OperatorKind::Divide | OperatorKind::Modulo) {
            emit(CANONICAL);
        }
    }

    fn recipe_name(&self, recipe: ProviderRecipeId) -> Option<&'static str> {
        (recipe == CANONICAL).then_some("canonical")
    }

    fn module_name(&self, operator: SemanticOperator) -> Option<&'static str> {
        match operator.kind() {
            OperatorKind::Divide => Some("DW_div"),
            OperatorKind::Modulo => Some("DW_mod"),
            OperatorKind::Add
            | OperatorKind::Sum
            | OperatorKind::Subtract
            | OperatorKind::Increment
            | OperatorKind::Decrement
            | OperatorKind::Multiply
            | OperatorKind::DynamicExtract => None,
        }
    }

    fn operation_mnemonic(&self, operator: SemanticOperator) -> Option<&'static str> {
        match operator.kind() {
            OperatorKind::Divide => Some("div"),
            OperatorKind::Modulo => Some("mod"),
            OperatorKind::Add
            | OperatorKind::Sum
            | OperatorKind::Subtract
            | OperatorKind::Increment
            | OperatorKind::Decrement
            | OperatorKind::Multiply
            | OperatorKind::DynamicExtract => None,
        }
    }

    fn implementation_name(&self, recipe: ProviderRecipeId) -> Option<&'static str> {
        (recipe == CANONICAL).then_some("canonical restoring division")
    }

    fn structural_estimate(
        &self,
        recipe: ProviderRecipeId,
        operator: SemanticOperator,
    ) -> Result<StructuralEstimate, crate::SynthError> {
        if recipe != CANONICAL
            || !matches!(operator.kind(), OperatorKind::Divide | OperatorKind::Modulo)
        {
            return Err(crate::SynthError::invariant(
                "division provider received an unsupported operator or recipe",
            ));
        }
        let width = u64::from(operator.width());
        Ok(StructuralEstimate {
            logic_depth: operator.width().saturating_mul(2),
            logic_units: width.saturating_mul(width).saturating_mul(4),
            wiring_units: width.saturating_mul(width).saturating_mul(6),
        })
    }
}

pub(super) fn implementation_provider() -> &'static dyn ImplementationProvider {
    &DivisionProvider
}

pub(super) fn lower_implementation(
    recipe: ProviderRecipeId,
    blaster: &mut BitBlaster<'_>,
    request: ImplementationRequest<'_>,
) -> Result<Vec<opto_ir::word::ValueId>, crate::SynthError> {
    if recipe != CANONICAL {
        return Err(crate::SynthError::invariant(
            "division lowering received an unknown recipe",
        ));
    }
    let remainder = match request.operator.kind() {
        OperatorKind::Divide => false,
        OperatorKind::Modulo => true,
        _ => {
            return Err(crate::SynthError::invariant(
                "division lowering received a non-division operator",
            ));
        }
    };
    let [left, right] = request.operator.inputs();
    blaster.divide_bits(left, right, remainder, request.result_type, request.source)
}
