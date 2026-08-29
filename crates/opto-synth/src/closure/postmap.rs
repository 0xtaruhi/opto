// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::{ImplementationDb, SynthesisOptions, SynthesisProgress, api::types::SynthesisPolicy};
use opto_ir::mapped::{CellId, MappedNetlist, NetId};
use opto_runtime::ExecutionContext;
use opto_timing::ScenarioSet;
#[cfg(test)]
use opto_timing::{
    IncrementalTiming, ReportTimingOptions, TimingContext, TimingLibrary, TimingModel,
};

mod area;
mod buffering;
mod candidate;
mod candidates;
mod cloning;
mod diagnostics;
mod electrical;
mod fanout;
mod forest;
mod mfs;
mod power;
mod region;
mod registers;
mod session;
mod sizing;

use crate::closure::mmmc::MmmcTiming;
#[cfg(test)]
use crate::closure::mmmc::MmmcViewPolicy;
pub(crate) use candidates::PostmapCellCatalog;
pub(crate) use fanout::MappedFanoutLoadProfile;
use power::MmmcPower;
use session::{TimingOptimizationPolicy, TimingOptimizationRequest, TimingOptimizationSession};

const HIGH_EFFORT_POSTMAP_GENERATIONS: usize = 3;

fn generated_name_namespace(
    mapped: &MappedNetlist,
    cell_prefix: &str,
    net_prefix: &str,
) -> Result<u64, crate::SynthError> {
    let object_count = mapped
        .cell_count()
        .checked_add(mapped.design_instance_count())
        .and_then(|count| count.checked_add(mapped.net_count()))
        .ok_or_else(|| crate::SynthError::capacity("mapped object count overflow"))?;
    for offset in 0..=object_count {
        let namespace = mapped
            .edit_revision()
            .checked_add(offset as u64)
            .ok_or_else(|| crate::SynthError::capacity("generated name namespace overflow"))?;
        let cell_stem = format!("{cell_prefix}{namespace}_");
        let net_stem = format!("{net_prefix}{namespace}_");
        let cell_conflict = mapped
            .cell_ids()
            .filter_map(|cell| mapped.cell_name(cell))
            .chain(
                mapped
                    .design_instance_ids()
                    .filter_map(|instance| mapped.design_instance_name(instance)),
            )
            .any(|name| name.starts_with(&cell_stem));
        let net_conflict = mapped
            .net_ids()
            .filter_map(|net| mapped.net_name(net))
            .any(|name| name.starts_with(&net_stem));
        if !cell_conflict && !net_conflict {
            return Ok(namespace);
        }
    }
    Err(crate::SynthError::invariant(
        "mapped object names exhaust the generated namespace search",
    ))
}

fn mapped_cell_nets(
    mapped: &MappedNetlist,
    cells: impl IntoIterator<Item = CellId>,
) -> Result<std::collections::BTreeSet<NetId>, crate::SynthError> {
    let mut nets = std::collections::BTreeSet::new();
    for cell in cells {
        let connections = mapped.connections(cell).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "mapped footprint references removed cell {cell:?}"
            ))
        })?;
        nets.extend(connections.iter().filter_map(|connection| {
            let opto_ir::mapped::ConnectionSignal::Net(net) = connection.signal else {
                return None;
            };
            Some(net)
        }));
    }
    Ok(nets)
}

pub(crate) struct PostmapOutcome {
    pub(crate) timing: Option<MmmcTiming>,
    pub(crate) changed: bool,
    #[cfg(test)]
    pub(crate) replacements: usize,
}

pub(crate) struct PostmapRequest<'a> {
    pub(crate) mapped: &'a mut MappedNetlist,
    pub(crate) implementations: &'a mut ImplementationDb,
    pub(crate) timing: Option<MmmcTiming>,
    pub(crate) options: &'a SynthesisOptions,
    pub(crate) catalog: &'a PostmapCellCatalog,
    pub(crate) scenarios: &'a ScenarioSet,
    pub(crate) fanout_load_profile: &'a MappedFanoutLoadProfile,
    pub(crate) policy: SynthesisPolicy,
    pub(crate) runtime: &'a ExecutionContext,
    pub(crate) power_evaluator: std::sync::Arc<dyn crate::SynthesisPowerEvaluator>,
    pub(crate) connectivity: &'a crate::mapping::materialize::FrozenObservableConnectivity,
}

pub(crate) fn optimize_mapped_netlist(
    request: PostmapRequest<'_>,
    config: crate::SynthesisConfig,
    observer: &mut dyn FnMut(SynthesisProgress),
) -> Result<PostmapOutcome, crate::SynthError> {
    let PostmapRequest {
        mapped,
        implementations,
        timing,
        options,
        catalog,
        scenarios,
        fanout_load_profile,
        policy,
        runtime,
        power_evaluator,
        connectivity,
    } = request;
    let mut cloned_drivers = std::collections::BTreeSet::new();
    let preparation = match timing {
        Some(timing) => optimize_timing(
            PostmapRequest {
                mapped,
                implementations,
                timing: Some(timing),
                options,
                catalog,
                scenarios,
                fanout_load_profile,
                policy,
                runtime,
                power_evaluator: power_evaluator.clone(),
                connectivity,
            },
            config,
            observer,
            &mut cloned_drivers,
        )?,
        None => PostmapOutcome {
            timing: None,
            changed: false,
            #[cfg(test)]
            replacements: 0,
        },
    };
    let mut timing = preparation.timing;
    let mut changed = preparation.changed;
    #[cfg(test)]
    let mut replacements = preparation.replacements;
    let generation_limit = if policy.repeated_timing_passes {
        HIGH_EFFORT_POSTMAP_GENERATIONS
    } else {
        1
    };
    for _ in 0..generation_limit {
        let recovery = area::optimize(
            PostmapRequest {
                mapped,
                implementations,
                timing,
                options,
                catalog,
                scenarios,
                fanout_load_profile,
                policy,
                runtime,
                power_evaluator: power_evaluator.clone(),
                connectivity,
            },
            config,
            observer,
        )?;
        let recovery_changed = recovery.changed;
        timing = recovery.timing;
        changed |= recovery_changed;
        #[cfg(test)]
        {
            replacements = replacements
                .checked_add(recovery.replacements)
                .ok_or_else(|| {
                    crate::SynthError::invariant("post-map replacement count overflow")
                })?;
        }
        if !policy.repeated_timing_passes || !recovery_changed {
            break;
        }
        let refinement_fanout_load_profile =
            MappedFanoutLoadProfile::build(mapped, &options.target_cells)?;
        let refinement = optimize_timing(
            PostmapRequest {
                mapped,
                implementations,
                timing,
                options,
                catalog,
                scenarios,
                fanout_load_profile: &refinement_fanout_load_profile,
                policy,
                runtime,
                power_evaluator: power_evaluator.clone(),
                connectivity,
            },
            config,
            observer,
            &mut cloned_drivers,
        )?;
        timing = refinement.timing;
        changed |= refinement.changed;
        #[cfg(test)]
        {
            replacements = replacements
                .checked_add(refinement.replacements)
                .ok_or_else(|| {
                    crate::SynthError::invariant("post-map replacement count overflow")
                })?;
        }
    }
    Ok(PostmapOutcome {
        timing,
        changed,
        #[cfg(test)]
        replacements,
    })
}

fn optimize_timing(
    request: PostmapRequest<'_>,
    config: crate::SynthesisConfig,
    observer: &mut dyn FnMut(SynthesisProgress),
    cloned_drivers: &mut std::collections::BTreeSet<CellId>,
) -> Result<PostmapOutcome, crate::SynthError> {
    let optimization_started = std::time::Instant::now();
    let diagnostics = config.diagnostics;
    let PostmapRequest {
        mapped,
        implementations,
        timing,
        options,
        catalog,
        scenarios,
        fanout_load_profile,
        policy,
        runtime,
        power_evaluator,
        connectivity,
    } = request;
    let mut session = TimingOptimizationSession::start(TimingOptimizationRequest {
        mapped,
        implementations,
        timing: timing.ok_or_else(|| {
            crate::SynthError::invariant("post-map timing optimization has no timing owner")
        })?,
        options,
        scenarios,
        runtime,
        power_evaluator,
        connectivity,
        diagnostics,
        observer,
    })?;
    fanout_load_profile.validate(session.mapped)?;
    let trace = session.trace();
    if trace.is_enabled() {
        match fanout_load_profile.maximum() {
            Some(maximum) => crate::api::diagnostics::trace!(
                trace,
                "postmap.electrical_profile",
                "multi_sink_nets={} max_net={:?} name={} sinks={} fanout_load={:.6} \
                 pin_capacitance={:.9}",
                fanout_load_profile.len(),
                maximum.net(),
                session
                    .mapped
                    .net_name(maximum.net())
                    .unwrap_or("<unnamed>"),
                maximum.sinks(),
                maximum.fanout_load(),
                maximum.pin_capacitance(),
            ),
            None => {
                crate::api::diagnostics::trace!(
                    trace,
                    "postmap.electrical_profile",
                    "multi_sink_nets=0"
                );
            }
        }
        diagnostics::report_timing_paths(trace, "initial", &session.timing)?;
        let critical_frontier_cells = session.timing.critical_instances()?;
        let analysis = session.analysis();
        crate::api::diagnostics::trace!(
            trace,
            "postmap.timing.start",
            "cells={} critical_frontier_cells={} arrival={:.6} wns={:?} tns={:.6} violations={}",
            session.mapped.cell_count(),
            critical_frontier_cells.len(),
            analysis.arrival(),
            analysis.wns(),
            analysis.tns(),
            analysis.violating_paths(),
        );
    }
    let timing_policy = TimingOptimizationPolicy::new(policy);
    let buffer_candidates = catalog.buffers();
    fanout::synthesize(
        &mut session,
        buffer_candidates,
        fanout_load_profile,
        runtime,
    )?;
    electrical::legalize(&mut session, buffer_candidates)?;
    if trace.is_enabled() {
        diagnostics::report_timing_paths(trace, "after_electrical_legalization", &session.timing)?;
    }
    cloning::optimize(&mut session, policy.critical_fanout_cloning, cloned_drivers)?;
    sizing::optimize(&mut session, catalog, runtime, &timing_policy)?;
    sizing::evaluate_pin_swaps(&mut session, catalog)?;
    session.report_completion(optimization_started.elapsed());
    Ok(session.finish())
}

#[cfg(test)]
mod tests;
