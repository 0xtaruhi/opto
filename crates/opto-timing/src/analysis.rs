// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::constraints::{
    ClockSlot, ExceptionCandidate, advance_candidates, bus_base_name, initial_candidates,
    resolve_multicycle, resolve_path_exception,
};
use crate::{
    Arrival, CheckTimingAnalysis, Clock, ClockId, DelayType, ExceptionCorner, LaunchClock,
    NetTimingState, PathExceptionKind, PathStep, PortId, ReportTimingOptions, TargetCellRef,
    TargetPinRef, TargetTimingArcRef, TargetTimingType, TimingAnalysis, TimingCheckKind,
    TimingContext, TimingDerateKind, TimingEdge, TimingEndpoint, TimingInstanceId, TimingLibrary,
    TimingLibraryMetadata, TimingModel, TimingPort, TimingPortDirection, TimingRequirement,
};
use smallvec::SmallVec;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

type ArrivalJournalEntry = (usize, ArrivalRow);
type RequiredJournalEntry = (usize, RequiredRow);
type RequiredWorklistUpdate = (usize, Vec<RequiredJournalEntry>);

mod arrival;
mod checks;
mod closure;
mod latch;
mod paths;
mod required;
mod state;
mod support;
mod topology;

use arrival::{
    ArrivalTask, arrival_slots_match, propagate_summary_slots, recompute_net,
    recompute_net_changed, seed_net, seed_summary_slots,
};
pub(super) use checks::check_timing;
use checks::{
    check_is_upper_bound, enabled_timing_check_kinds, pulse_width_arcs, pulse_width_related_pin,
    timing_check_arcs,
};
pub(crate) use closure::{ClosureEdit, ClosureIndex, ClosureUpdateContext};
use latch::{latch_data_is_transparent, sequential_description};
use paths::{
    EndpointSlacks, collect_output_candidates, collect_pulse_width_candidates,
    collect_timing_check_candidates, select_worse_path,
};
use required::{
    RequiredEndpoints, RequiredSources, RequiredTask, recompute_required, required_slots_match,
};
pub(super) use state::PropagationState;
use state::*;
use support::*;

use topology::{GraphArcKind, SequentialElement, connection_map_ref};
pub(crate) use topology::{
    InstanceNetArena, InstanceRegionGraphEdit, SharedNetNames, TimingGraph, TimingNetNamesBuilder,
};

#[cfg(test)]
use crate::TimingDesign;

#[cfg(test)]
pub(super) fn analyze_timing(
    timing: &TimingContext,
    model: &TimingModel,
    options: &ReportTimingOptions,
) -> Result<TimingAnalysis, crate::TimingError> {
    worst_analysis(analyze_timing_paths(timing, model, options)?)
}

pub(super) fn analyze_timing_paths(
    timing: &TimingContext,
    model: &TimingModel,
    options: &ReportTimingOptions,
) -> Result<Vec<TimingAnalysis>, crate::TimingError> {
    let propagation = propagate_all(timing, model, options)?;
    analyze_propagation_paths(timing, model, options, &propagation)
}

pub(super) fn analyze_timing_quality(
    timing: &TimingContext,
    model: &TimingModel,
    options: &ReportTimingOptions,
) -> Result<crate::TimingQuality, crate::TimingError> {
    let propagation = propagate_all(timing, model, options)?;
    analyze_propagation_quality(timing, model, options, &propagation)
}

pub(super) fn propagate_all(
    timing: &TimingContext,
    model: &TimingModel,
    options: &ReportTimingOptions,
) -> Result<PropagationState, crate::TimingError> {
    propagate_all_with_path_tracking(timing, model, options, true, None)
}

pub(super) fn propagate_all_with_path_tracking(
    timing: &TimingContext,
    model: &TimingModel,
    options: &ReportTimingOptions,
    track_paths: bool,
    runtime: Option<&opto_runtime::ExecutionContext>,
) -> Result<PropagationState, crate::TimingError> {
    let graph = &model.graph;
    let inputs = PropagationInputs {
        timing,
        model,
        design: &model.design,
        library: &model.library,
        options,
        graph,
    };
    let mut propagation = PropagationState {
        arrivals: ArrivalSlotStore::new(graph.net_count(), track_paths)?,
        requireds: RequiredSlotStore::new(graph.net_count())?,
        paths: track_paths.then(PathArena::default),
        origins: OriginArena::default(),
        tags: TagArena::default(),
    };
    if track_paths {
        for &net in &graph.topological_order {
            recompute_net(&inputs, net, &mut propagation)?;
        }
    } else {
        for net in graph.launch_nets() {
            seed_net(&inputs, net, &mut propagation)?;
        }
        let mut worklist = graph.propagation_worklist(
            opto_runtime::DependencyDirection::Forward,
            0..graph.net_count(),
        )?;
        match runtime {
            Some(runtime) => {
                let origins = &propagation.origins;
                let tags = &propagation.tags;
                let publication =
                    opto_runtime::DependencyPublicationPlan::identity(graph.net_count());
                runtime.publish_dependency_rows(
                    worklist,
                    &mut propagation.arrivals,
                    opto_runtime::DependencyRun::new(
                        &publication,
                        opto_runtime::DependencyActivation::all(),
                    ),
                    |arrivals, net| ArrivalTask::prepare(&inputs, arrivals, origins, net),
                    |task| {
                        let net = task.net();
                        task.analyze(&inputs, tags)
                            .map(|slots| opto_runtime::DependencyPublication::row(net, slots))
                    },
                )?;
            }
            None => {
                while let Some(ready) = worklist.claim_ready()? {
                    let computed = analyze_arrivals(&inputs, &propagation, &ready)?;
                    for (net, slots) in computed {
                        propagation
                            .arrivals
                            .replace_row(net, slots)
                            .expect("timing worklists only publish live net rows");
                        worklist.finish(net)?;
                    }
                }
            }
        }
    }
    let dirty = vec![true; graph.net_count()];
    recompute_required(&inputs, &dirty, &mut propagation, runtime)?;
    Ok(propagation)
}

fn analyze_arrivals(
    inputs: &PropagationInputs<'_, '_>,
    propagation: &PropagationState,
    nets: &[usize],
) -> Result<Vec<ArrivalJournalEntry>, crate::TimingError> {
    let analyze = |position: usize| {
        let net = nets[position];
        let slots = propagation
            .arrivals
            .row(net)
            .ok_or(crate::TimingAnalysisError::DirtyNetOutOfRange { index: net })?;
        propagate_summary_slots(inputs, net, propagation, slots).map(|slots| (net, slots))
    };
    (0..nets.len()).map(analyze).collect()
}

pub(super) fn analyze_propagation_paths(
    timing: &TimingContext,
    model: &TimingModel,
    options: &ReportTimingOptions,
    propagation: &PropagationState,
) -> Result<Vec<TimingAnalysis>, crate::TimingError> {
    analyze_propagation(timing, model, options, propagation).map(|analysis| analysis.paths)
}

pub(super) fn analyze_propagation_quality(
    timing: &TimingContext,
    model: &TimingModel,
    options: &ReportTimingOptions,
    propagation: &PropagationState,
) -> Result<crate::TimingQuality, crate::TimingError> {
    let analysis = analyze_propagation(timing, model, options, propagation)?;
    crate::TimingQuality::from_endpoint_slacks(
        model.generation(),
        analysis.paths,
        analysis.endpoint_slacks.values(),
    )
}

struct PropagationAnalysis {
    paths: Vec<TimingAnalysis>,
    endpoint_slacks: EndpointSlacks,
}

fn analyze_propagation(
    timing: &TimingContext,
    model: &TimingModel,
    options: &ReportTimingOptions,
    propagation: &PropagationState,
) -> Result<PropagationAnalysis, crate::TimingError> {
    if options.max_paths == 0 {
        return Err(crate::TimingAnalysisError::InvalidMaxPaths.into());
    }
    let mut best = Vec::new();
    let mut endpoint_slacks = EndpointSlacks::default();
    let paths = propagation
        .paths
        .as_ref()
        .ok_or(crate::TimingAnalysisError::EmptyPath {
            operation: "analyze",
        })?;
    let candidates = CandidateInputs {
        timing,
        model,
        design: &model.design,
        library: &model.library,
        options,
        graph: &model.graph,
        arrivals: &propagation.arrivals,
        paths,
        origins: &propagation.origins,
        tags: &propagation.tags,
    };
    collect_output_candidates(&candidates, &mut best, &mut endpoint_slacks)?;
    collect_timing_check_candidates(&candidates, &mut best, &mut endpoint_slacks)?;
    collect_pulse_width_candidates(&candidates, &mut best, &mut endpoint_slacks);
    if best.is_empty() {
        Err(crate::TimingAnalysisError::NoTimingPaths.into())
    } else {
        Ok(PropagationAnalysis {
            paths: best,
            endpoint_slacks,
        })
    }
}

pub(super) fn worst_analysis(
    analyses: impl IntoIterator<Item = TimingAnalysis>,
) -> Result<TimingAnalysis, crate::TimingError> {
    let mut best = None;
    for analysis in analyses {
        select_worse_path(&mut best, analysis);
    }
    best.ok_or_else(|| crate::TimingAnalysisError::NoTimingPaths.into())
}

pub(super) fn update_propagation(
    previous: &TimingContext,
    timing: &TimingContext,
    model: &TimingModel,
    options: &ReportTimingOptions,
    propagation: &mut PropagationState,
) -> Result<usize, crate::TimingError> {
    if previous == timing {
        return Ok(0);
    }
    let graph = &model.graph;
    let mut dirty = vec![false; graph.net_count()];
    let mut pending = VecDeque::new();
    let exception_changed = previous.path_exceptions() != timing.path_exceptions();
    if exception_changed {
        propagation.tags = TagArena::default();
    }
    for (net, is_dirty) in dirty.iter_mut().enumerate() {
        let transition_changed =
            explicit_transition(previous, graph, net) != explicit_transition(timing, graph, net);
        let load_changed = explicit_load(previous, graph, net) != explicit_load(timing, graph, net);
        let launch_changed =
            previous.clocks() != timing.clocks() && !graph.sequential_outputs[net].is_empty();
        if transition_changed || load_changed || launch_changed || exception_changed {
            *is_dirty = true;
            pending.push_back(net);
        }
    }
    while let Some(net) = pending.pop_front() {
        for &arc in &graph.outgoing[net] {
            let to = graph.arc(arc).to.index();
            if !dirty[to] {
                dirty[to] = true;
                pending.push_back(to);
            }
        }
    }

    let dirty_count = dirty.iter().filter(|&&is_dirty| is_dirty).count();
    // Clock edits move capture edges, so required times can change on nets
    // whose arrivals are untouched (for example purely combinational cones
    // into a check pin); recompute the whole backward side in that case.
    let clocks_changed = previous.clocks() != timing.clocks();
    if dirty_count == 0 && !clocks_changed {
        return Ok(0);
    }
    let inputs = PropagationInputs {
        timing,
        model,
        design: &model.design,
        library: &model.library,
        options,
        graph,
    };
    for &net in &graph.topological_order {
        if dirty[net] {
            recompute_net(&inputs, net, propagation)?;
        }
    }
    if let Some(paths) = &mut propagation.paths {
        paths.compact(&mut propagation.arrivals)?;
    }
    let mut required_dirty = if clocks_changed {
        vec![true; graph.net_count()]
    } else {
        dirty
    };
    backward_closure(graph, &mut required_dirty);
    recompute_required(&inputs, &required_dirty, propagation, None)?;
    Ok(dirty_count)
}

#[allow(
    clippy::too_many_lines,
    reason = "incremental propagation is one atomic row-journal transaction; splitting it would \
              separate rollback ordering from the mutations it protects"
)]
pub(super) fn update_propagation_from_nets(
    timing: &TimingContext,
    model: &TimingModel,
    options: &ReportTimingOptions,
    propagation: &mut PropagationState,
    seeds: &[usize],
    defer_required: bool,
    runtime: Option<&opto_runtime::ExecutionContext>,
) -> Result<(usize, PropagationEdit), crate::TimingError> {
    let graph = &model.graph;
    let mut edit = PropagationEdit {
        paths_len: propagation
            .paths
            .as_ref()
            .map_or(0, |paths| paths.nodes.len()),
        origins_len: propagation.origins.values.len(),
        tags_len: propagation.tags.keys.len(),
        origins: Vec::new(),
        arrivals: Vec::new(),
        requireds: Vec::new(),
        deferred_required: Vec::new(),
    };
    for &net in seeds {
        if net >= graph.net_count() {
            return Err(crate::TimingAnalysisError::DirtyNetOutOfRange { index: net }.into());
        }
    }
    let inputs = PropagationInputs {
        timing,
        model,
        design: &model.design,
        library: &model.library,
        options,
        graph,
    };
    let dependency_items = graph.propagation_closure(
        opto_runtime::DependencyDirection::Forward,
        seeds.iter().copied(),
    )?;
    let dependency_set = dependency_items.iter().copied().collect::<BTreeSet<_>>();
    edit.origins.extend(
        propagation
            .origins
            .ids
            .iter()
            .filter(|(key, _)| match key {
                OriginKey::PrimaryInput { port, .. } => graph
                    .port_net(*port)
                    .is_some_and(|net| dependency_set.contains(&net.index())),
                OriginKey::Sequential { net, .. } => dependency_set.contains(net),
            })
            .map(|(_, &id)| (id, propagation.origins.values[id.raw() as usize].clone())),
    );
    let mut worklist = graph.propagation_worklist(
        opto_runtime::DependencyDirection::Forward,
        dependency_items.iter().copied(),
    )?;
    let active = ActiveClosure::seeded(graph.net_count(), seeds);
    let mut changed = BTreeSet::new();
    let mut dirty_count = 0usize;
    let result = (|| {
        if !propagation.tracks_paths()
            && let Some(runtime) = runtime
        {
            let mut seeded = vec![None; graph.net_count()];
            for &net in &dependency_items {
                seeded[net] = Some(seed_summary_slots(
                    &inputs,
                    net,
                    &mut propagation.origins,
                    &mut propagation.tags,
                )?);
            }
            let publication = opto_runtime::DependencyPublicationPlan::identity(graph.net_count());
            let mut effects = opto_runtime::DependencyEffects::new();
            let tags = &propagation.tags;
            let execution = runtime.publish_dependency_rows(
                worklist,
                &mut propagation.arrivals,
                opto_runtime::DependencyRun::new(
                    &publication,
                    opto_runtime::DependencyActivation::on_change(
                        graph.net_count(),
                        seeds.iter().copied(),
                    )?,
                )
                .record_effects(&mut effects),
                |arrivals, net| {
                    ArrivalTask::prepare_with_slots(
                        &inputs,
                        arrivals,
                        &propagation.origins,
                        net,
                        seeded[net]
                            .clone()
                            .expect("scheduled timing net owns precomputed seed slots"),
                    )
                },
                |task| {
                    let net = task.net();
                    task.analyze(&inputs, tags)
                        .map(|slots| opto_runtime::DependencyPublication::row(net, slots))
                },
            )?;
            dirty_count += execution.published_items().len();
            changed.extend(execution.changed_items().iter().copied());
            edit.arrivals.extend(
                effects
                    .into_entries()
                    .map(|(_, net, previous)| (net, previous)),
            );
        } else {
            let mut active = active;
            while let Some(nets) = worklist.claim_ready()? {
                if propagation.tracks_paths() {
                    for net in nets {
                        if !active.contains(net) {
                            worklist.finish(net)?;
                            continue;
                        }
                        dirty_count += 1;
                        edit.arrivals.push((
                            net,
                            propagation.arrivals.row(net).ok_or(
                                crate::TimingAnalysisError::DirtyNetOutOfRange { index: net },
                            )?,
                        ));
                        if recompute_net_changed(&inputs, net, propagation)? {
                            changed.insert(net);
                            for &arc in &graph.outgoing[net] {
                                active.activate(graph.arc(arc).to.index());
                            }
                        }
                        worklist.finish(net)?;
                    }
                    continue;
                }
                let active_nets = nets
                    .iter()
                    .copied()
                    .filter(|&net| active.contains(net))
                    .collect::<Vec<_>>();
                dirty_count += active_nets.len();
                let edit_offset = edit.arrivals.len();
                for &net in &active_nets {
                    edit.arrivals.push((
                        net,
                        propagation
                            .arrivals
                            .row(net)
                            .ok_or(crate::TimingAnalysisError::DirtyNetOutOfRange { index: net })?,
                    ));
                    seed_net(&inputs, net, propagation)?;
                }
                let computed = analyze_arrivals(&inputs, propagation, &active_nets)?;
                for (position, (net, slots)) in computed.into_iter().enumerate() {
                    let did_change =
                        !arrival_slots_match(&edit.arrivals[edit_offset + position].1, &slots);
                    propagation
                        .arrivals
                        .replace_row(net, slots)
                        .expect("timing worklists only publish live net rows");
                    if did_change {
                        changed.insert(net);
                        for &arc in &graph.outgoing[net] {
                            active.activate(graph.arc(arc).to.index());
                        }
                    }
                }
                for net in nets {
                    worklist.finish(net)?;
                }
            }
        }
        edit.arrivals.sort_unstable_by_key(|(net, _)| *net);
        if defer_required {
            let mut required_dirty = seeds.iter().copied().collect::<BTreeSet<_>>();
            required_dirty.extend(changed.iter().copied());
            edit.deferred_required.extend(required_dirty);
        } else {
            let required_seeds = seeds
                .iter()
                .copied()
                .chain(changed.iter().copied())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            let endpoints = RequiredEndpoints::build(&inputs)?;
            let (_, requireds) = update_required_worklist(
                &inputs,
                &endpoints,
                propagation,
                &required_seeds,
                runtime,
            )?;
            edit.requireds = requireds;
        }
        Ok::<(), crate::TimingError>(())
    })();
    if let Err(error) = result {
        restore_propagation(propagation, edit);
        return Err(error);
    }
    Ok((dirty_count, edit))
}

#[derive(Debug)]
pub(super) struct PropagationEdit {
    paths_len: usize,
    origins_len: usize,
    tags_len: usize,
    origins: Vec<(OriginId, ArrivalOrigin)>,
    arrivals: Vec<ArrivalJournalEntry>,
    requireds: Vec<RequiredJournalEntry>,
    deferred_required: Vec<usize>,
}

impl PropagationEdit {
    pub(super) fn changed_nets(&self) -> Vec<usize> {
        let mut nets = self
            .arrivals
            .iter()
            .map(|(net, _)| *net)
            .collect::<Vec<_>>();
        nets.sort_unstable();
        nets.dedup();
        nets
    }

    pub(super) fn deferred_required(&self) -> &[usize] {
        &self.deferred_required
    }
}

pub(super) fn synchronize_required_from_nets(
    timing: &TimingContext,
    model: &TimingModel,
    options: &ReportTimingOptions,
    propagation: &mut PropagationState,
    seeds: &[usize],
    runtime: Option<&opto_runtime::ExecutionContext>,
) -> Result<usize, crate::TimingError> {
    let graph = &model.graph;
    let inputs = PropagationInputs {
        timing,
        model,
        design: &model.design,
        library: &model.library,
        options,
        graph,
    };
    let endpoints = RequiredEndpoints::build(&inputs)?;
    for &net in seeds {
        if net >= graph.net_count() {
            return Err(crate::TimingAnalysisError::DirtyNetOutOfRange { index: net }.into());
        }
    }
    update_required_worklist(&inputs, &endpoints, propagation, seeds, runtime)
        .map(|(recomputed, _)| recomputed)
}

fn update_required_worklist(
    inputs: &PropagationInputs<'_, '_>,
    endpoints: &RequiredEndpoints<'_>,
    propagation: &mut PropagationState,
    seeds: &[usize],
    runtime: Option<&opto_runtime::ExecutionContext>,
) -> Result<RequiredWorklistUpdate, crate::TimingError> {
    let dependency_items = inputs.graph.propagation_closure(
        opto_runtime::DependencyDirection::Reverse,
        seeds.iter().copied(),
    )?;
    let mut worklist = inputs
        .graph
        .propagation_worklist(opto_runtime::DependencyDirection::Reverse, dependency_items)?;
    let active = ActiveClosure::seeded(inputs.graph.net_count(), seeds);
    let sources = RequiredSources::new(
        &propagation.arrivals,
        &propagation.origins,
        &propagation.tags,
    );
    let mut previous = Vec::new();
    let mut recomputed = 0usize;
    let execution = if let Some(runtime) = runtime {
        let publication =
            opto_runtime::DependencyPublicationPlan::identity(inputs.graph.net_count());
        let mut effects = opto_runtime::DependencyEffects::new();
        let result = runtime.publish_dependency_rows(
            worklist,
            &mut propagation.requireds,
            opto_runtime::DependencyRun::new(
                &publication,
                opto_runtime::DependencyActivation::on_change(
                    inputs.graph.net_count(),
                    seeds.iter().copied(),
                )?,
            )
            .record_effects(&mut effects),
            |requireds, net| Ok(RequiredTask::prepare(inputs.graph, requireds, net)),
            |task| {
                let net = task.net();
                task.analyze(inputs, endpoints, sources)
                    .map(|slots| opto_runtime::DependencyPublication::row(net, slots))
            },
        );
        if let Ok(execution) = &result {
            recomputed = execution.published_items().len();
            previous.extend(
                std::mem::take(&mut effects)
                    .into_entries()
                    .map(|(_, net, old)| (net, old)),
            );
        }
        result.map(|_| ())
    } else {
        (|| {
            let mut active = active;
            while let Some(nets) = worklist.claim_ready()? {
                for net in nets {
                    if !active.contains(net) {
                        worklist.finish(net)?;
                        continue;
                    }
                    recomputed += 1;
                    let old = propagation
                        .requireds
                        .row(net)
                        .ok_or(crate::TimingAnalysisError::DirtyNetOutOfRange { index: net })?;
                    let slots = RequiredTask::prepare(inputs.graph, &propagation.requireds, net)
                        .analyze(inputs, endpoints, sources)?;
                    let did_change = !required_slots_match(&old, &slots);
                    previous.push((net, old));
                    propagation
                        .requireds
                        .replace_row(net, slots)
                        .expect("timing worklists only publish live net rows");
                    if did_change {
                        for &arc in &inputs.graph.incoming[net] {
                            active.activate(inputs.graph.arc(arc).from.index());
                        }
                    }
                    worklist.finish(net)?;
                }
            }
            Ok(())
        })()
    };
    if let Err(error) = execution {
        for (net, slots) in previous.into_iter().rev() {
            propagation
                .requireds
                .replace_row(net, slots)
                .expect("timing journals only reference live net rows");
        }
        return Err(error);
    }
    previous.sort_unstable_by_key(|(net, _)| *net);
    Ok((recomputed, previous))
}

struct ActiveClosure(Box<[bool]>);

impl ActiveClosure {
    fn seeded(len: usize, seeds: &[usize]) -> Self {
        let mut active = vec![false; len].into_boxed_slice();
        for &seed in seeds {
            active[seed] = true;
        }
        Self(active)
    }

    fn contains(&self, item: usize) -> bool {
        self.0[item]
    }

    fn activate(&mut self, item: usize) {
        self.0[item] = true;
    }
}

/// Restores required rows before arrival rows, then rewinds path and identity arenas.
pub(super) fn restore_propagation(state: &mut PropagationState, edit: PropagationEdit) {
    for (net, slots) in edit.requireds.into_iter().rev() {
        state
            .requireds
            .replace_row(net, slots)
            .expect("timing journals only reference live net rows");
    }
    for (net, slots) in edit.arrivals.into_iter().rev() {
        state
            .arrivals
            .replace_row(net, slots)
            .expect("timing journals only reference live net rows");
    }
    if let Some(paths) = &mut state.paths {
        paths.nodes.truncate(edit.paths_len);
    }
    state.origins.restore(edit.origins_len, edit.origins);
    state.tags.truncate(edit.tags_len);
}

/// Extend a forward-closed dirty set with every fanin cone, covering the
/// nets whose required times depend on the recomputed arrivals.
fn backward_closure(graph: &TimingGraph, dirty: &mut [bool]) {
    let mut pending = dirty
        .iter()
        .enumerate()
        .filter(|(_, is_dirty)| **is_dirty)
        .map(|(net, _)| net)
        .collect::<VecDeque<_>>();
    while let Some(net) = pending.pop_front() {
        for &arc in &graph.incoming[net] {
            let from = graph.arc(arc).from.index();
            if !dirty[from] {
                dirty[from] = true;
                pending.push_back(from);
            }
        }
    }
}

pub(super) fn append_propagation_net(
    propagation: &mut PropagationState,
) -> Result<(), crate::TimingError> {
    propagation.arrivals.push_empty()?;
    if let Err(error) = propagation.requireds.push_empty() {
        propagation.arrivals.pop();
        return Err(error);
    }
    Ok(())
}

pub(super) fn remove_last_propagation_net(propagation: &mut PropagationState) {
    propagation.arrivals.pop();
    propagation.requireds.pop();
}

/// Bounds retained predecessor storage relative to the dense arrival arena.
///
/// The factor is a deterministic structural threshold rather than an RSS- or
/// allocator-dependent admission policy.
pub(super) fn compact_paths_if_needed(
    propagation: &mut PropagationState,
) -> Result<(), crate::TimingError> {
    let Some(paths) = &mut propagation.paths else {
        return Ok(());
    };
    let live_scale = propagation.arrivals.len().saturating_mul(8);
    if paths.nodes.len() > live_scale {
        paths.compact(&mut propagation.arrivals)?;
    }
    Ok(())
}

pub(super) fn net_timing_state(
    timing: &TimingContext,
    model: &TimingModel,
    propagation: &PropagationState,
    delay_type: DelayType,
    name: &str,
) -> Option<NetTimingState> {
    let net = model.net_id(name)?;
    net_timing_state_by_index(timing, model, propagation, delay_type, net.index())
}

pub(crate) fn net_timing_state_by_index(
    timing: &TimingContext,
    model: &TimingModel,
    propagation: &PropagationState,
    delay_type: DelayType,
    net: usize,
) -> Option<NetTimingState> {
    let id = crate::TimingNetId::from_index(net).ok()?;
    let name = model.net_name(id)?;
    let state = TimingEdge::ALL
        .into_iter()
        .flat_map(|edge| propagation.arrivals.states(net, edge.index()))
        .reduce(|left, right| match delay_type {
            DelayType::Max if right.delay > left.delay => right,
            DelayType::Min if right.delay < left.delay => right,
            _ => left,
        });
    let transition = TimingEdge::ALL
        .into_iter()
        .flat_map(|edge| propagation.arrivals.states(net, edge.index()))
        .filter_map(|state| state.transition)
        .max_by(f64::total_cmp);
    // Slack pairs each arrival with the required time of its own tag; the
    // net reports the tightest pairing across both edges.
    let (required, slack) = net_required_and_slack(propagation, delay_type, net);
    Some(NetTimingState {
        id,
        name: name.into_owned(),
        arrival: state.map(|state| state.delay),
        required,
        slack,
        transition,
        capacitance: TimingEdge::ALL
            .into_iter()
            .filter_map(|edge| timing_load(timing, &model.graph, net, edge, delay_type))
            .max_by(f64::total_cmp)
            .unwrap_or(0.0),
        fanout: model.graph.fanout_loads[net],
    })
}

fn net_required_and_slack(
    propagation: &PropagationState,
    delay_type: DelayType,
    net: usize,
) -> (Option<f64>, Option<f64>) {
    let mut required = None;
    let mut slack: Option<f64> = None;
    for edge in TimingEdge::ALL {
        for arrival in propagation.arrivals.states(net, edge.index()) {
            let Some(matched) = propagation
                .requireds
                .states(net, edge.index())
                .find(|candidate| candidate.tag == arrival.tag)
            else {
                continue;
            };
            let candidate = match delay_type {
                DelayType::Max => matched.required - arrival.delay,
                DelayType::Min => arrival.delay - matched.required,
            };
            if slack.is_none_or(|current| candidate < current) {
                slack = Some(candidate);
                required = Some(matched.required);
            }
        }
    }
    (required, slack)
}

pub(super) fn nets_with_slack_at_most(
    model: &TimingModel,
    propagation: &PropagationState,
    delay_type: DelayType,
    slack_limit: f64,
) -> std::collections::BTreeSet<crate::TimingNetId> {
    (0..model.net_count())
        .filter(|net| {
            net_required_and_slack(propagation, delay_type, *net)
                .1
                .is_some_and(|slack| slack <= slack_limit)
        })
        .filter_map(|net| crate::TimingNetId::from_index(net).ok())
        .collect()
}

pub(super) fn all_net_timing_states(
    timing: &TimingContext,
    model: &TimingModel,
    propagation: &PropagationState,
    delay_type: DelayType,
) -> Vec<NetTimingState> {
    (0..model.net_count())
        .filter_map(|net| net_timing_state_by_index(timing, model, propagation, delay_type, net))
        .collect()
}

pub(super) fn electrical_snapshot(
    timing: &TimingContext,
    model: &TimingModel,
    propagation: &PropagationState,
    delay_type: DelayType,
) -> Result<crate::TimingElectricalSnapshot, crate::TimingError> {
    crate::TimingElectricalSnapshot::try_from_dense(
        model.generation(),
        timing.revision(),
        delay_type,
        model.net_count(),
        |net| {
            let transition = TimingEdge::ALL
                .into_iter()
                .flat_map(|edge| propagation.arrivals.states(net, edge.index()))
                .filter_map(|state| state.transition)
                .max_by(f64::total_cmp);
            let capacitance = TimingEdge::ALL
                .into_iter()
                .filter_map(|edge| timing_load(timing, &model.graph, net, edge, delay_type))
                .max_by(f64::total_cmp)
                .unwrap_or(0.0);
            crate::TimingElectricalState {
                capacitance,
                transition,
            }
        },
    )
}

pub(super) fn propagation_net_count(model: &TimingModel) -> usize {
    model.net_count()
}

#[cfg(test)]
mod tests;
