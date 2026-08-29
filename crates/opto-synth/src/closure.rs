// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Timing, power, and post-map physical-quality closure.
//!
//! This domain consumes a mapped artifact and performs bounded transactional
//! repairs. It must not define frontend or technology-mapping semantics.

mod boundary_measurement;
pub(crate) mod mapped_timing;
pub(crate) mod mmmc;
pub(crate) mod objective;
pub(crate) mod postmap;
pub(crate) mod power;

pub(crate) use boundary_measurement::{
    BoundaryNetObservation, GlobalBoundaryRequest, measure_global_boundaries,
    validated_dynamic_power,
};
pub use power::{NoPowerEvaluation, SynthesisPowerEvaluator};

/// Whether a timing library characterizes any arc at all.
pub(crate) fn library_has_timing_arcs(library: &opto_timing::TimingLibrary) -> bool {
    library
        .cells
        .iter()
        .any(|cell| cell.pins().any(|pin| pin.timing_arcs().next().is_some()))
}
