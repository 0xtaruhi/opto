// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

mod catalog;
mod planning;

use crate::mapping::library::CombinationalCellCatalog;
use crate::planning::mapping_policy::CellCost;
use opto_ir::word;

pub(crate) use catalog::cells::AsyncResetRequest;
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
        .copied()
        .map(|reset| {
            if reset.kind != word::ResetKind::Async {
                return Err(crate::SynthError::invariant(
                    "synchronous reset reached library sequential selection",
                ));
            }
            if module
                .value(reset.reset_value)
                .is_none_or(|value| value.ty.width() != 1)
            {
                return Err(crate::SynthError::invariant(
                    "scalar sequential selection received a non-scalar reset value",
                ));
            }
            reset_request(module, reset)?.ok_or_else(|| {
                crate::SynthError::invariant("scalar sequential reset is not a two-state constant")
            })
        })
        .collect()
}

pub(crate) fn uniform_async_reset_requests(
    module: &word::WordModule,
    resets: &[word::Reset],
) -> Result<Option<Vec<AsyncResetRequest>>, crate::SynthError> {
    resets
        .iter()
        .copied()
        .map(|reset| reset_request(module, reset))
        .collect()
}

fn reset_request(
    module: &word::WordModule,
    reset: word::Reset,
) -> Result<Option<AsyncResetRequest>, crate::SynthError> {
    if reset.kind != word::ResetKind::Async {
        return Err(crate::SynthError::invariant(
            "synchronous reset reached library sequential selection",
        ));
    }
    let stored = module
        .value(reset.reset_value)
        .ok_or_else(|| crate::SynthError::invariant("asynchronous reset value disappeared"))?;
    let word::ValueKind::Constant(bits) = &stored.kind else {
        return Err(crate::SynthError::invariant(
            "asynchronous reset value is not constant",
        ));
    };
    let Some(first) = bits.as_slice().first().copied() else {
        return Err(crate::SynthError::invariant(
            "asynchronous reset value is empty",
        ));
    };
    let reset_value = match first {
        opto_ir::BitVal::Zero => false,
        opto_ir::BitVal::One => true,
        opto_ir::BitVal::X | opto_ir::BitVal::Z => return Ok(None),
    };
    if bits.as_slice().iter().any(|&bit| bit != first) {
        return Ok(None);
    }
    Ok(Some(AsyncResetRequest {
        active_high: reset.active_high,
        reset_value,
    }))
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
