// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

mod catalog;
mod planning;

use crate::mapping::library::CombinationalCellCatalog;
use crate::planning::mapping_policy::CellCost;
use opto_ir::word;

pub(crate) use catalog::cells::{AsyncResetRequest, AsyncResetRequests};
pub(crate) use catalog::*;
pub(crate) use planning::{
    expand_unsupported_enables, lower_controls, normalize_enable_polarities,
    normalize_sequential_controls,
};

pub(crate) fn async_reset_requests(
    module: &word::WordModule,
    resets: &[word::Reset],
) -> Result<Vec<AsyncResetRequest>, crate::SynthError> {
    resets
        .iter()
        .map(|reset| {
            if reset.kind != word::ResetKind::Async {
                return Err(crate::SynthError::invariant(
                    "synchronous reset reached library sequential selection",
                ));
            }
            let stored = module.value(reset.reset_value).ok_or_else(|| {
                crate::SynthError::invariant("asynchronous reset value disappeared")
            })?;
            let word::ValueKind::Constant(bits) = &stored.kind else {
                return Err(crate::SynthError::invariant(
                    "asynchronous reset value is not constant",
                ));
            };
            let reset_value = crate::boolean::logic::logic_constant(bits).ok_or_else(|| {
                crate::SynthError::invariant("asynchronous reset value is not a two-state scalar")
            })?;
            Ok(AsyncResetRequest {
                active_high: reset.active_high,
                reset_value,
            })
        })
        .collect()
}

pub(crate) fn enable_inverter_cost(
    module: &word::WordModule,
    value: word::ValueId,
    catalog: &CombinationalCellCatalog,
) -> Option<CellCost> {
    if module.value(value).is_some_and(|stored| {
        matches!(
            &stored.kind,
            word::ValueKind::Constant(bits) if crate::boolean::logic::logic_constant(bits).is_some()
        )
    }) {
        return Some(CellCost {
            area: 0.0,
            delay: 0.0,
            transition: 0.0,
            input_capacitance: 0.0,
        });
    }
    let signature = crate::boolean::logic::LogicSignature {
        inputs: crate::boolean::logic::LogicInputs::from_indices(1)
            .expect("one inverter input fits a logic signature"),
        truth: crate::boolean::logic::inverter_truth(),
    };
    catalog.best_cost_for_signature(&signature)
}
