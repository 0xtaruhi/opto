// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Source and interface fingerprints with their change metrics.

use super::{
    AtomicUsize, Deserialize, Fingerprint, HIERARCHY_FINGERPRINT_DOMAIN, Hash,
    INTERFACE_FINGERPRINT_DOMAIN, IncrementalReuseMetrics, Ordering, RtlModule, Serialize,
    canonical_len, capture_source, hash_signal_type_layout,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
/// Versioned 256-bit digest of a module's semantic RTL content.
pub struct SourceFingerprint(pub(crate) [u8; 32]);

impl SourceFingerprint {
    /// Compute a deterministic digest of values, operations, and boundaries.
    #[must_use]
    pub fn capture(rtl: &RtlModule) -> Self {
        capture_source(rtl).semantic_fingerprint
    }

    /// Captures a linked hierarchy without first materializing its flat RTL.
    ///
    /// Definition fingerprints include instance topology and connections;
    /// occurrence counts distinguish otherwise identical definition sets. The
    /// caller supplies definitions in its stable semantic order.
    #[must_use]
    pub fn capture_hierarchy<'a>(
        root: &str,
        definitions: impl IntoIterator<Item = (&'a RtlModule, u64)>,
    ) -> Self {
        let mut digest = blake3::Hasher::new();
        digest.update(HIERARCHY_FINGERPRINT_DOMAIN);
        digest.update(&canonical_len(root.len()).to_le_bytes());
        digest.update(root.as_bytes());
        for (module, occurrences) in definitions {
            let name = module.word().name();
            digest.update(&canonical_len(name.len()).to_le_bytes());
            digest.update(name.as_bytes());
            digest.update(&occurrences.to_le_bytes());
            digest.update(&Self::capture(module).bytes());
        }
        Self(*digest.finalize().as_bytes())
    }

    #[must_use]
    /// Return the digest bytes in canonical order.
    pub const fn bytes(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
/// Versioned digest of a module's externally visible port contract.
pub struct InterfaceFingerprint([u8; 32]);

impl InterfaceFingerprint {
    /// Compute a deterministic digest of port order, names, directions, and
    /// complete signal-type layouts.
    #[must_use]
    pub fn capture(module: &RtlModule) -> Self {
        let module = module.word();
        let mut digest = blake3::Hasher::new();
        digest.update(INTERFACE_FINGERPRINT_DOMAIN);
        digest.update(&canonical_len(module.ports().len()).to_le_bytes());
        for port in module.ports() {
            let name = module.name_str(port.name).as_bytes();
            digest.update(&canonical_len(name.len()).to_le_bytes());
            digest.update(name);
            digest.update(&[port.direction as u8]);
            digest.update(&port.ty.width().to_le_bytes());
            digest.update(&[u8::from(port.ty.is_signed())]);
            digest.update(&[port.ty.state() as u8]);
            let mut layout_fingerprint = Fingerprint::new();
            hash_signal_type_layout(module, port.signal, &mut layout_fingerprint);
            digest.update(layout_fingerprint.bytes().as_slice());
        }
        Self(*digest.finalize().as_bytes())
    }

    #[must_use]
    /// Return the digest bytes in canonical order.
    pub const fn bytes(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
/// Counts semantic objects in the current source and their changes from a
/// compatible prior snapshot.
pub struct SourceChangeMetrics {
    /// Values in the current normalized source.
    pub values: usize,
    /// Current values whose identity or transitive semantics changed.
    pub changed_values: usize,
    /// Values present only in the prior snapshot.
    pub removed_values: usize,
    /// Operations in the current normalized source.
    pub operations: usize,
    /// Current operations whose result fingerprint changed.
    pub changed_operations: usize,
    /// Operations present only in the prior snapshot.
    pub removed_operations: usize,
    /// Procedural and module boundaries in the current source.
    pub boundaries: usize,
    /// Current boundaries whose structural fingerprint changed.
    pub changed_boundaries: usize,
    /// Boundaries present only in the prior snapshot.
    pub removed_boundaries: usize,
    /// Semantic operation fingerprint groups in the current source.
    ///
    /// These metrics predate and do not identify target `SynthesisRegion`s.
    pub regions: usize,
    /// Current fingerprint groups that could not be matched to the prior source.
    pub rebuilt_regions: usize,
    /// Current fingerprint groups matched by content, independently of arena IDs.
    pub reused_regions: usize,
}

#[derive(Debug, Default)]
pub(crate) struct IncrementalRunMetrics {
    boolean_recipe_hits: AtomicUsize,
    boolean_recipe_misses: AtomicUsize,
    regional_decision_hits: AtomicUsize,
    regional_decision_misses: AtomicUsize,
}

impl IncrementalRunMetrics {
    pub(crate) fn boolean_recipe_hit(&self) {
        self.boolean_recipe_hits.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn boolean_recipe_miss(&self) {
        self.boolean_recipe_misses.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn regional_decision_hit(&self) {
        self.regional_decision_hits.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn regional_decision_miss(&self) {
        self.regional_decision_misses
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn snapshot(&self) -> IncrementalReuseMetrics {
        IncrementalReuseMetrics {
            boolean_recipe_hits: self.boolean_recipe_hits.load(Ordering::Relaxed),
            boolean_recipe_misses: self.boolean_recipe_misses.load(Ordering::Relaxed),
            regional_decision_hits: self.regional_decision_hits.load(Ordering::Relaxed),
            regional_decision_misses: self.regional_decision_misses.load(Ordering::Relaxed),
        }
    }
}
