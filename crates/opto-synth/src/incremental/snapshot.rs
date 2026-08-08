// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Artifact-owned state consumed by the next incremental synthesis.

use super::{Deserialize, Serialize, SourceSnapshot};
use std::mem::size_of;
use std::sync::Arc;

/// Canonical incremental state retained by a synthesis artifact or a session.
#[derive(Debug, Serialize, Deserialize)]
pub struct IncrementalSnapshot {
    source: SourceSnapshot,
    regional_cache_records: Arc<[crate::incremental::RegionalCacheRecord]>,
}

impl IncrementalSnapshot {
    pub(crate) fn new(
        source: SourceSnapshot,
        regional_cache_records: Box<[crate::incremental::RegionalCacheRecord]>,
    ) -> Self {
        Self {
            source,
            regional_cache_records: regional_cache_records.into(),
        }
    }

    /// Borrow the retained source identity.
    #[must_use]
    pub const fn source(&self) -> &SourceSnapshot {
        &self.source
    }

    pub(crate) fn regional_cache_records(&self) -> Arc<[crate::incremental::RegionalCacheRecord]> {
        Arc::clone(&self.regional_cache_records)
    }

    pub(crate) fn owned_memory_bytes(&self) -> usize {
        let records = size_of::<usize>().saturating_mul(2).saturating_add(
            size_of::<crate::incremental::RegionalCacheRecord>()
                .saturating_mul(self.regional_cache_records.len()),
        );
        self.source
            .owned_memory_bytes()
            .saturating_add(opto_core::resident::allocation_bytes(records))
            .saturating_add(crate::incremental::RegionalCacheRecord::owned_memory_bytes(
                &self.regional_cache_records,
            ))
    }

    /// Validate state restored from a checkpoint before incremental reuse.
    ///
    /// # Errors
    ///
    /// Returns an invariant or capacity error when source or regional cache
    /// records violate their canonical serialized representation.
    pub fn validate_checkpoint(&self) -> Result<(), crate::SynthError> {
        self.source.validate_checkpoint()?;
        crate::incremental::RegionalCacheRecord::validate_all(&self.regional_cache_records)
    }
}
