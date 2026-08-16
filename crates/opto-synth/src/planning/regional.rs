// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Region-aware architecture selection and local Word construction.

mod envelope;
mod lowering;
mod private;
mod search;

pub(crate) use envelope::{RegionCostEnvelopeSet, StructuralTargetModel};
pub(crate) use lowering::{
    RegionalMemoryLogicBinding, RegionalMemoryStateBinding, RegionalWordCone,
    RegionalWordConeRequest,
};
pub(crate) use private::optimize_structure as optimize_private_structure;
pub(crate) use search::{RegionalSearchRequest, select_architectures};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemoryImplementationCandidate {
    RegisterBank,
    Macro(u32),
}

impl MemoryImplementationCandidate {
    pub(crate) const fn raw(self) -> u32 {
        match self {
            Self::RegisterBank => 0,
            Self::Macro(cell) => cell.saturating_add(1),
        }
    }

    pub(crate) const fn from_raw(raw: u32) -> Self {
        match raw {
            0 => Self::RegisterBank,
            raw => Self::Macro(raw - 1),
        }
    }
}

pub(crate) fn decode_memory_implementations(
    encoded: &[u8],
) -> Result<Box<[MemoryImplementationCandidate]>, crate::SynthError> {
    let (records, remainder) = encoded.as_chunks::<4>();
    if !remainder.is_empty() {
        return Err(crate::SynthError::invariant(
            "regional memory implementation payload is not 32-bit aligned",
        ));
    }
    Ok(records
        .iter()
        .map(|bytes| MemoryImplementationCandidate::from_raw(u32::from_le_bytes(*bytes)))
        .collect())
}

pub(crate) fn decision_key(implementations: &[MemoryImplementationCandidate]) -> [u8; 32] {
    let mut digest = blake3::Hasher::new();
    digest.update(b"opto/regional/decision-vector/v3\0");
    digest.update(&(implementations.len() as u64).to_le_bytes());
    for implementation in implementations {
        digest.update(&implementation.raw().to_le_bytes());
    }
    *digest.finalize().as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_implementation_participates_in_the_decision_key() {
        let without_memory = [];
        let with_bank = [MemoryImplementationCandidate::RegisterBank];

        assert_ne!(decision_key(&without_memory), decision_key(&with_bank));
        assert_eq!(
            decode_memory_implementations(&[0; 4]).unwrap().as_ref(),
            &with_bank
        );
    }

    #[test]
    fn persisted_macro_cell_index_is_reconstructed() {
        assert_eq!(
            MemoryImplementationCandidate::from_raw(7),
            MemoryImplementationCandidate::Macro(6)
        );
    }
}
