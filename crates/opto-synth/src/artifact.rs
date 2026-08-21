// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Published implementation fragment containment and source provenance.
//!
//! Mapping and closure update this domain transactionally. It contains durable
//! artifact data, not algorithms that choose or optimize an implementation.

pub(crate) mod implementation;
pub(crate) mod provenance;
mod source;

pub(crate) use source::MappedCellSource;

pub use implementation::{
    FragmentFootprint, ImplementationDb, ImplementationRegion, ImplementationRegionId,
    MappedFragmentId,
};
