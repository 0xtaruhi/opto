// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::SessionError;
use opto_db::RevisionId;
use opto_timing::Parasitics;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Per-design parasitics under one monotonically increasing session revision.
///
/// Each stored row carries the global revision at which it was published, so
/// synthesis can distinguish a design's exact interconnect generation without
/// hashing unrelated designs.
pub(crate) struct ParasiticsState {
    revision: RevisionId,
    by_design: BTreeMap<String, (RevisionId, Parasitics)>,
}

impl Default for ParasiticsState {
    fn default() -> Self {
        Self {
            revision: RevisionId::INITIAL,
            by_design: BTreeMap::new(),
        }
    }
}

impl ParasiticsState {
    pub(crate) fn revision(&self) -> RevisionId {
        self.revision
    }

    pub(crate) fn get(&self, design: &str) -> Option<&(RevisionId, Parasitics)> {
        self.by_design.get(design)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&String, &(RevisionId, Parasitics))> {
        self.by_design.iter()
    }

    /// Preflights revision capacity for a later publication transaction.
    pub(crate) fn validate_publish(&self) -> Result<(), SessionError> {
        self.revision.next().map(|_| ()).map_err(Into::into)
    }

    /// Atomically replaces one design's parasitics and advances the generation.
    pub(crate) fn publish(
        &mut self,
        design: String,
        parasitics: Parasitics,
    ) -> Result<RevisionId, SessionError> {
        let revision = self.revision.next()?;
        self.by_design.insert(design, (revision, parasitics));
        self.revision = revision;
        Ok(revision)
    }
}
