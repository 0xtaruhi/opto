// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::SessionError;
use opto_db::RevisionId;
use opto_timing::Parasitics;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

    pub(crate) fn validate_publish(&self) -> Result<(), SessionError> {
        self.revision.next().map(|_| ()).map_err(Into::into)
    }

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
