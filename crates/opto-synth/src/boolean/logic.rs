// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use opto_ir::{BitVal, word};
use serde::{Deserialize, Serialize};
use std::ops::Index;

mod balance;
pub(crate) mod cuts;
pub(crate) mod network;
mod pipeline;
mod rewrite;
mod subject;
mod sweep;

#[cfg(test)]
mod proptests;

pub(crate) use rewrite::{
    CoverageCheck, RewriteIncremental, RewriteRecipeCache, projected_cuts, projected_leaves,
    window_cares,
};
pub(crate) use subject::{
    CanonicalRegionLogic, ChoiceGraph, ChoiceScopeId, ChoiceSubject, RegionLogicOptions,
};

pub(crate) fn inverter_truth() -> TruthTable {
    TruthTable {
        input_count: 1,
        bits: 0b01,
    }
}

pub(crate) fn identity_truth() -> TruthTable {
    TruthTable {
        input_count: 1,
        bits: 0b10,
    }
}

pub(crate) const MAX_MATCH_INPUTS: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LogicSignature {
    pub(crate) inputs: LogicInputs,
    pub(crate) truth: TruthTable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct LogicInputs {
    len: u8,
    values: [word::ValueId; MAX_MATCH_INPUTS],
}

impl LogicInputs {
    pub(crate) fn new() -> Self {
        Self {
            len: 0,
            values: [word::ValueId::FIRST; MAX_MATCH_INPUTS],
        }
    }

    pub(crate) fn from_indices(input_count: usize) -> Option<Self> {
        let mut inputs = Self::new();
        for index in 0..input_count {
            inputs.push(word::ValueId::from_index(index).ok()?)?;
        }
        Some(inputs)
    }

    pub(crate) fn from_slice(values: &[word::ValueId]) -> Option<Self> {
        let mut inputs = Self::new();
        for value in values {
            inputs.push(*value)?;
        }
        Some(inputs)
    }

    pub(crate) fn len(self) -> usize {
        self.len as usize
    }

    pub(crate) fn as_slice(&self) -> &[word::ValueId] {
        &self.values[..self.len()]
    }

    fn push(&mut self, value: word::ValueId) -> Option<()> {
        let len = self.len();
        if len == MAX_MATCH_INPUTS {
            return None;
        }
        self.values[len] = value;
        self.len += 1;
        Some(())
    }
}

impl Index<usize> for LogicInputs {
    type Output = word::ValueId;

    fn index(&self, index: usize) -> &Self::Output {
        &self.as_slice()[index]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct TruthTable {
    pub(crate) input_count: usize,
    pub(crate) bits: u64,
}

pub(crate) fn logic_constant(bits: &opto_ir::ConstBits) -> Option<bool> {
    let [bit] = bits.as_slice() else {
        return None;
    };
    match bit {
        BitVal::Zero => Some(false),
        BitVal::One => Some(true),
        BitVal::X | BitVal::Z => None,
    }
}

impl TruthTable {
    pub(crate) fn bit(self, assignment: usize) -> bool {
        debug_assert!(self.input_count <= MAX_MATCH_INPUTS);
        debug_assert!(assignment < (1usize << self.input_count));
        ((self.bits >> assignment) & 1) == 1
    }

    pub(crate) fn with_input_inversions(self, inversions: u8) -> Self {
        assert!(self.input_count <= MAX_MATCH_INPUTS);
        assert!((inversions as usize) < (1usize << self.input_count));
        let mut bits = 0u64;
        for physical_assignment in 0..(1usize << self.input_count) {
            let logical_assignment = physical_assignment ^ inversions as usize;
            if self.bit(logical_assignment) {
                bits |= 1u64 << physical_assignment;
            }
        }
        Self {
            input_count: self.input_count,
            bits,
        }
    }
}
