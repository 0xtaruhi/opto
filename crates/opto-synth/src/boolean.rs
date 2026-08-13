// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Word-to-Boolean lowering and technology-independent Boolean optimization.
//!
//! Planning decisions enter through [`bitblast`]. The resulting canonical
//! subject is analyzed and rewritten here before technology mapping consumes
//! it. This domain does not own mapped-netlist closure policy.

pub(crate) mod bitblast;
pub(crate) mod logic;

/// Resolves one four-state HDL constant at final physical publication.
///
/// `X` is a synthesis don't-care and is filled deterministically with zero.
/// `Z` requires a real tri-state implementation and therefore remains an
/// explicit unsupported construct instead of being silently weakened.
pub(crate) fn resolve_publication_bit(
    bit: opto_ir::BitVal,
    design: &str,
    source: &opto_ir::word::SourceSpan,
) -> Result<opto_ir::BitVal, crate::SynthError> {
    match bit {
        opto_ir::BitVal::Zero | opto_ir::BitVal::One => Ok(bit),
        opto_ir::BitVal::X => Ok(opto_ir::BitVal::Zero),
        opto_ir::BitVal::Z => Err(crate::SynthError::invalid(format!(
            "tri-state constant in design '{design}' at {source:?} is not supported"
        ))),
    }
}
