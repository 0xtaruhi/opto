// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Provenance and fragment containment carried during mapped construction.

use super::implementation::OriginSetId;
use opto_ir::word;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MappedCellSource {
    Instance(word::InstId),
    StructuralValue(word::ValueId),
    Value {
        value: word::ValueId,
        region: crate::RegionAnchorId,
    },
    Region {
        origins: OriginSetId,
        region: crate::RegionAnchorId,
    },
}
