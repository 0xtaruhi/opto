// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Synthesis-specific analysis over canonical Word IR.
//!
//! This domain owns read-only graph facts shared by the frontend, planning,
//! Boolean lowering, and mapping. It does not mutate Word IR or choose an
//! implementation architecture.

pub(crate) mod bit_connectivity;
pub(crate) mod cycle;
pub(crate) mod instances;
pub(crate) mod signal_driver;
pub(crate) mod uses;

pub(crate) type OperationInputs = smallvec::SmallVec<[opto_ir::word::ValueId; 4]>;

pub(crate) fn operation_inputs(kind: &opto_ir::word::OpKind) -> OperationInputs {
    let mut inputs = OperationInputs::new();
    kind.for_each_input(|input| inputs.push(input));
    inputs
}
