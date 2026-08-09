// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Provenance and ownership carried by a cell during mapped construction.

use super::implementation::OriginSetId;
use opto_ir::word;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MappedCellSource {
    Instance(word::InstId),
    Value {
        value: word::ValueId,
        owner: crate::RegionAnchorId,
    },
    Region {
        origins: OriginSetId,
        owner: crate::RegionAnchorId,
    },
    Boundary {
        origins: OriginSetId,
        driver: crate::RegionAnchorId,
        sink: crate::RegionAnchorId,
    },
}
