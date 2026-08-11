// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{SequentialCellCatalog, sequential, word};

pub(crate) struct PrivateRegionPreparation<'a> {
    pub(crate) module: &'a mut word::WordModule,
    pub(crate) sequential_catalog: &'a SequentialCellCatalog,
    pub(crate) clock_gating: Option<crate::ClockGatingStyle>,
    pub(crate) clock_gating_catalog: &'a crate::mapping::clock_gating::ClockGatingCatalog,
    pub(crate) target_mapping: bool,
    pub(crate) timing_diagnostics: bool,
    pub(crate) ownership: &'a mut crate::regional::StructuralOwnershipProvenance,
}

pub(crate) fn prepare_private_region(
    request: PrivateRegionPreparation<'_>,
) -> Result<(), crate::SynthError> {
    let PrivateRegionPreparation {
        module,
        sequential_catalog,
        clock_gating,
        clock_gating_catalog,
        target_mapping,
        timing_diagnostics,
        ownership,
    } = request;
    let trace = crate::api::diagnostics::SynthTrace::new(timing_diagnostics);
    let mut stage_started = std::time::Instant::now();
    let mut finish_stage = |stage: &str| {
        crate::api::diagnostics::trace!(
            trace,
            "mapping.prepare",
            "stage={stage} wall={:?}",
            stage_started.elapsed()
        );
        stage_started = std::time::Instant::now();
    };
    sequential::normalize_sequential_controls(module, ownership)?;
    finish_stage("normalize controls");
    if target_mapping {
        sequential::lower_controls(module, sequential_catalog, ownership)?;
        finish_stage("lower controls");
    }
    let gating_edges = |edge: word::Edge| {
        clock_gating.is_some_and(|style| {
            clock_gating_catalog
                .gate_for(edge, style.latch_based)
                .is_some()
        })
    };
    if target_mapping {
        sequential::recover_feedback_enables(module, sequential_catalog, &gating_edges, ownership)?;
        finish_stage("recover enables");
        if let Some(style) = clock_gating {
            crate::mapping::clock_gating::gate_register_clocks_in_regions(
                module,
                clock_gating_catalog,
                style,
                ownership,
            )?;
            finish_stage("gate clocks");
            sequential::expand_unsupported_enables(module, sequential_catalog, ownership)?;
            finish_stage("expand enables");
        }
    }
    Ok(())
}
