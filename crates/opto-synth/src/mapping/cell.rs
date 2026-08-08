// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Target-cell bindings selected before direct artifact materialization.

use opto_ir::word;
use smallvec::SmallVec;

#[derive(Debug, Clone)]
pub(crate) struct MappedCell {
    pub(crate) cell_name: String,
    pub(crate) input_connections: SmallVec<[MappedInputConnection; 4]>,
    pub(crate) output_connections: SmallVec<[MappedOutputConnection; 2]>,
}

#[derive(Debug, Clone)]
pub(crate) struct MappedInputConnection {
    pub(crate) pin: String,
    pub(crate) value: word::ValueId,
}

#[derive(Debug, Clone)]
pub(crate) struct MappedOutputConnection {
    pub(crate) pin: String,
    pub(crate) value: word::ValueId,
}
