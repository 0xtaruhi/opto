// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Typed intermediate representations used throughout Opto.
//!
//! The representations form explicit phase boundaries:
//!
//! - [`proc`] models ordered procedural control and effects.
//! - [`word`] stores typed word-level operations, signals, and memories.
//! - [`logic`] stores Boolean networks with complemented edges.
//! - [`mapped`] stores target-library cells, pins, and canonical nets.
//! - [`rtl`] binds source definitions and linked design occurrences.
//!
//! IDs are compact and local to their owning module or netlist. Builders permit
//! mutation, but consumers receive validated sealed values. Cross-phase
//! provenance is carried in dedicated tables instead of pointers between
//! arenas, which keeps snapshots serializable and parallel reads lock-free.

pub mod logic;
pub mod mapped;
pub mod proc;
pub mod rtl;
pub mod value;
pub mod word;

pub use opto_core::{NameCheckpoint, NameError, NameId, NameTable, RevisionId};
pub use value::{BitVal, ConstBits, ValueError, Width};
