// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Activity-aware power analysis for mapped designs.
//!
//! [`PowerEngine`] combines switching annotations, mapped connectivity, and
//! Liberty power models to produce per-net, per-cell, and design summaries.
//! Activity carries an [`ActivityOrigin`] so reports can distinguish explicit
//! user data from propagated or default assumptions.
//!
//! Analysis consumes sealed models and returns an owned [`PowerAnalysis`].
//! Missing library power data is preserved in diagnostics and references rather
//! than silently converted into characterized power.

#![cfg_attr(
    test,
    allow(
        clippy::float_cmp,
        reason = "power tests assert bit-stable closed-form watt totals from deterministic fixtures"
    )
)]

mod analysis;
mod engine;
mod error;

pub use analysis::{
    ActivityAnnotations, ActivityOrigin, CellPower, NetPower, PowerAnalysis, PowerLibraryReference,
    PowerSummary, SwitchingActivity,
};
pub use engine::{PowerEngine, PowerEngineMetrics};
pub use error::PowerError;
