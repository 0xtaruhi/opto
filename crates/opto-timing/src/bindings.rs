// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::PortId;

/// Typed object identities in the declaration order of a design's ports.
///
/// IR layers have independent name tables, so names are resolved exactly once
/// at the session boundary. Synthesis, mapped timing, and hierarchy overlays
/// carry this dense table instead of repeatedly joining on owned strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortBindings {
    ids: Box<[PortId]>,
}

impl PortBindings {
    /// Bind port declarations in their stable declaration order.
    #[must_use]
    pub fn new(ids: impl IntoIterator<Item = PortId>) -> Self {
        Self {
            ids: ids.into_iter().collect(),
        }
    }

    /// Return the object identity for one declaration index.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<PortId> {
        self.ids.get(index).copied()
    }

    /// Number of bound port declarations.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Whether no ports are bound.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}
