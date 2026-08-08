// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use opto_db::{AnyObjectId, ObjectIdSet, RevisionId};
use opto_power::SwitchingActivity;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
/// Partial update applied to existing switching activity.
pub struct SwitchingActivityUpdate {
    /// Probability that the signal is at logic one.
    pub static_probability: Option<f64>,
    /// Average transitions per timing unit.
    pub toggle_rate: Option<f64>,
    /// Fraction of transitions that are rising.
    pub rise_ratio: Option<f64>,
}

impl SwitchingActivityUpdate {
    pub(crate) fn apply(
        self,
        current: SwitchingActivity,
    ) -> Result<SwitchingActivity, opto_power::PowerError> {
        SwitchingActivity::new(
            self.static_probability
                .unwrap_or(current.static_probability()),
            self.toggle_rate.unwrap_or(current.toggle_rate()),
            self.rise_ratio.unwrap_or(current.rise_ratio()),
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PowerContext {
    pub(crate) revision: RevisionId,
    pub(crate) activities: BTreeMap<AnyObjectId, SwitchingActivity>,
}

#[derive(Debug)]
pub(crate) struct PreparedPowerObjectRemoval {
    revision: Option<RevisionId>,
    objects: Vec<AnyObjectId>,
}

impl Default for PowerContext {
    fn default() -> Self {
        Self {
            revision: RevisionId::INITIAL,
            activities: BTreeMap::new(),
        }
    }
}

impl PowerContext {
    pub(crate) fn synthesis_fingerprint(&self) -> Option<[u8; 32]> {
        if self.activities.is_empty() {
            return None;
        }
        let mut digest = blake3::Hasher::new();
        digest.update(b"opto/session/synthesis-activity/v1\0");
        for (&object, &activity) in &self.activities {
            let class = match object {
                AnyObjectId::Design(_) => 0,
                AnyObjectId::Port(_) => 1,
                AnyObjectId::Cell(_) => 2,
                AnyObjectId::Pin(_) => 3,
                AnyObjectId::Net(_) => 4,
                AnyObjectId::Clock(_) => 5,
            };
            digest.update(&[class]);
            digest.update(&object.uid().get().get().to_le_bytes());
            digest.update(&activity.static_probability().to_bits().to_le_bytes());
            digest.update(&activity.toggle_rate().to_bits().to_le_bytes());
            digest.update(&activity.rise_ratio().to_bits().to_le_bytes());
        }
        Some(*digest.finalize().as_bytes())
    }

    pub(crate) fn prepare_object_removal(
        &self,
        removed: &impl ObjectIdSet,
    ) -> Result<PreparedPowerObjectRemoval, crate::SessionError> {
        let mut objects = if removed.len() <= self.activities.len() {
            removed
                .iter()
                .filter(|id| self.activities.contains_key(id))
                .collect::<Vec<_>>()
        } else {
            self.activities
                .keys()
                .filter(|id| removed.contains(id))
                .copied()
                .collect::<Vec<_>>()
        };
        objects.sort_unstable();
        let revision = (!objects.is_empty())
            .then(|| self.revision.next())
            .transpose()?;
        Ok(PreparedPowerObjectRemoval { revision, objects })
    }

    pub(crate) fn apply_object_removal(&mut self, prepared: PreparedPowerObjectRemoval) {
        for id in prepared.objects {
            self.activities.remove(&id);
        }
        if let Some(revision) = prepared.revision {
            self.revision = revision;
        }
    }
}
