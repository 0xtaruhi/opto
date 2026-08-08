// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::api::diagnostics::{SynthTrace, trace};
use crate::closure::mmmc::MmmcTiming;

/// Emits every diagnostic timing path, its steps, and its interconnect detail.
///
/// Callers gate on [`SynthTrace::is_enabled`] before calling: assembling the
/// diagnostic analyses is expensive on its own.
pub(super) fn report_timing_paths(
    trace: SynthTrace,
    stage: &str,
    incremental: &MmmcTiming,
) -> Result<(), crate::SynthError> {
    for (view, analysis) in incremental.diagnostic_analyses()? {
        let mut cell_arc = 0.0;
        let mut interconnect = 0.0;
        let mut boundary = 0.0;
        for step in analysis.steps() {
            match step.kind() {
                opto_timing::PathStepKind::CellArc => cell_arc += step.increment(),
                opto_timing::PathStepKind::Interconnect => interconnect += step.increment(),
                opto_timing::PathStepKind::Clock
                | opto_timing::PathStepKind::InputDelay
                | opto_timing::PathStepKind::Point
                | opto_timing::PathStepKind::TimingCheck => boundary += step.increment(),
            }
        }
        trace!(
            trace,
            "postmap.path",
            "stage={stage} view={view} type={} start={} endpoint={} steps={} arrival={:.6} \
             cell_arc={cell_arc:.6} interconnect={interconnect:.6} boundary={boundary:.6} \
             required={:?} slack={:?}",
            analysis.delay_type().report_name(),
            analysis.startpoint(),
            analysis.endpoint(),
            analysis.steps().len(),
            analysis.arrival(),
            analysis.required(),
            analysis.slack(),
        );
        for (index, step) in analysis.steps().iter().enumerate() {
            trace!(
                trace,
                "postmap.path_step",
                "stage={stage} view={view} index={index} kind={} increment={:.6} \
                 cumulative={:.6} edge={} point={}",
                step.kind().report_name(),
                step.increment(),
                step.path(),
                step.edge().report_suffix(),
                step.point(),
            );
            if let Some(interconnect) = step.interconnect() {
                trace!(
                    trace,
                    "postmap.path_wire",
                    "stage={stage} view={view} index={index} net={:?} fanout={:.6} load={:.9} \
                     resistance={:.9} wire_delay={:.6} parasitic_delay={:.6} derate={:.6}",
                    interconnect.net(),
                    interconnect.fanout(),
                    interconnect.load(),
                    interconnect.resistance(),
                    interconnect.wire_delay(),
                    interconnect.parasitic_delay(),
                    interconnect.derate(),
                );
            }
        }
    }
    Ok(())
}
