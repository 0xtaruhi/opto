// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! One deterministic construction vector for each synthesis region.

use crate::regional::RegionCoverPlanRecord;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RegionalDecisionVector {
    memory_implementations: Box<[MemoryImplementationCandidate]>,
    retained_plan: Option<RegionCoverPlanRecord>,
}

impl RegionalDecisionVector {
    pub(super) fn new(memory_implementations: Vec<MemoryImplementationCandidate>) -> Self {
        Self {
            memory_implementations: memory_implementations.into_boxed_slice(),
            retained_plan: None,
        }
    }

    pub(super) fn with_retained_plan(mut self, plan: Option<RegionCoverPlanRecord>) -> Self {
        self.retained_plan = plan;
        self
    }

    pub(crate) fn memory_implementations(&self) -> &[MemoryImplementationCandidate] {
        &self.memory_implementations
    }

    pub(crate) fn portable_memory_implementations(&self) -> Box<[u8]> {
        self.memory_implementations
            .iter()
            .flat_map(|implementation| implementation.raw().to_le_bytes())
            .collect()
    }

    pub(crate) fn stable_key(&self) -> [u8; 32] {
        let mut digest = blake3::Hasher::new();
        digest.update(b"opto/regional/decision-vector/v3\0");
        digest.update(&(self.memory_implementations.len() as u64).to_le_bytes());
        for implementation in &self.memory_implementations {
            digest.update(&implementation.raw().to_le_bytes());
        }
        *digest.finalize().as_bytes()
    }

    pub(crate) fn retained_plan(&self) -> Option<&RegionCoverPlanRecord> {
        self.retained_plan.as_ref()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RegionalDecisionPlan {
    rows: Box<[RegionalDecisionVector]>,
}

impl RegionalDecisionPlan {
    pub(super) fn new(rows: Vec<RegionalDecisionVector>) -> Self {
        Self {
            rows: rows.into_boxed_slice(),
        }
    }

    pub(crate) fn vector(&self, row: crate::RegionRowId) -> &RegionalDecisionVector {
        &self.rows[row.index()]
    }

    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_implementation_participates_in_the_stable_key() {
        let without_memory = RegionalDecisionVector::new(Vec::new());
        let with_bank =
            RegionalDecisionVector::new(vec![MemoryImplementationCandidate::RegisterBank]);

        assert_ne!(without_memory.stable_key(), with_bank.stable_key());
        assert_eq!(
            with_bank.portable_memory_implementations().as_ref(),
            &[0; 4]
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
