// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Shared multi-mode, multi-corner timing ownership and aggregation.

use opto_ir::mapped::MappedNetlist;
use opto_runtime::{ExecutionContext, Task, TaskKey};
use opto_timing::{
    AnalysisViewId, CellTimingEstimate, DelayType, DesignRuleSummary, DesignRuleViolation,
    IncrementalTiming, ReportTimingOptions, ScenarioSet, TimingAnalysis, TimingInstanceId,
    TimingModel, TimingQualitySummary,
};
use std::collections::BTreeSet;
use std::num::NonZeroUsize;
use std::sync::Arc;

const MMMC_VIEW_BUILD_TASK_DOMAIN: u32 = 0x4d4d_4d43;
const MAX_PARALLEL_VIEW_BUILDS: usize = 2;

pub(crate) struct MmmcMetrics {
    pub(crate) analysis: TimingQualitySummary,
    pub(crate) design_rule_summary: DesignRuleSummary,
    pub(crate) design_rules: Vec<DesignRuleViolation>,
}

pub(crate) struct MmmcTiming {
    owners: Vec<IncrementalTiming>,
    views: Box<[MmmcView]>,
    scenario_owners: Box<[MmmcScenarioOwners]>,
    construction_scratch_high_water_bytes: usize,
    construction_high_water_bytes: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_field_names,
    reason = "the byte suffix makes the unit explicit on every independently reported memory metric"
)]
pub(crate) struct MmmcTimingMemory {
    pub(crate) resident_bytes: usize,
    pub(crate) construction_scratch_high_water_bytes: usize,
    pub(crate) construction_high_water_bytes: usize,
}

#[derive(Debug, Clone, Copy)]
struct MmmcScenarioOwners {
    id: opto_timing::ScenarioId,
    late: AnalysisViewId,
    early: AnalysisViewId,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct MmmcView {
    pub(crate) id: AnalysisViewId,
    pub(crate) scenario: opto_timing::ScenarioId,
    pub(crate) delay_type: DelayType,
    pub(crate) policy: MmmcViewPolicy,
}

/// The two canonical timing owners for one explicit scenario.
///
/// Owner ordering is a service detail: callers name the scenario and then
/// consume its late/max and early/min views explicitly.
pub(crate) struct MmmcScenarioViews<'a> {
    pub(crate) late: Option<MmmcViewRef<'a>>,
    pub(crate) early: Option<MmmcViewRef<'a>>,
}

#[derive(Clone, Copy)]
pub(crate) struct MmmcViewRef<'a> {
    pub(crate) id: AnalysisViewId,
    pub(crate) timing: &'a IncrementalTiming,
}

pub(crate) struct MmmcNetState {
    pub(crate) view: AnalysisViewId,
    pub(crate) state: Option<opto_timing::NetTimingState>,
}

pub(crate) struct CriticalTimingFrontier {
    pub(crate) instances: Vec<TimingInstanceId>,
    pub(crate) mapped_nets: Vec<opto_ir::mapped::NetId>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct MmmcViewPolicy {
    pub(crate) timing: bool,
    pub(crate) checks: opto_timing::ScenarioCheckSet,
}

#[derive(Clone, Copy)]
struct MmmcViewBuild<'a> {
    id: AnalysisViewId,
    scenario: &'a opto_timing::Scenario,
    library: &'a opto_timing::TimingLibrary,
    parasitics: &'a opto_timing::Parasitics,
    delay_type: DelayType,
    policy: MmmcViewPolicy,
}

impl MmmcTiming {
    pub(crate) fn new(
        mapped: &MappedNetlist,
        design_id: opto_timing::DesignId,
        port_bindings: &opto_timing::PortBindings,
        object_bindings: &Arc<opto_timing::TimingObjectBindings>,
        scenarios: &ScenarioSet,
        runtime: &ExecutionContext,
    ) -> Result<Option<Self>, crate::SynthError> {
        let mut scenario_owners = Vec::with_capacity(scenarios.scenarios().len());
        let mut builds = Vec::with_capacity(scenarios.analysis_views().len());
        for scenario in scenarios.scenarios() {
            let checks = scenario.checks();
            let max_checks = checks_for_view(checks, DelayType::Max);
            let min_checks = checks_for_view(checks, DelayType::Min);
            let max_policy = MmmcViewPolicy {
                timing: timing_checks_enabled(max_checks),
                checks: max_checks,
            };
            let min_policy = MmmcViewPolicy {
                timing: timing_checks_enabled(min_checks),
                checks: min_checks,
            };
            let late = scenarios
                .analysis_view_id(scenario.id(), DelayType::Max)
                .ok_or_else(|| crate::SynthError::invariant("scenario has no max analysis view"))?;
            let early = scenarios
                .analysis_view_id(scenario.id(), DelayType::Min)
                .ok_or_else(|| crate::SynthError::invariant("scenario has no min analysis view"))?;
            scenario_owners.push(MmmcScenarioOwners {
                id: scenario.id(),
                late,
                early,
            });
            for (view, library, parasitics, delay_type, policy) in [
                (
                    late,
                    scenario.late_library(),
                    scenario.late_parasitics(),
                    DelayType::Max,
                    max_policy,
                ),
                (
                    early,
                    scenario.early_library(),
                    scenario.early_parasitics(),
                    DelayType::Min,
                    min_policy,
                ),
            ] {
                if !crate::closure::library_has_timing_arcs(library) {
                    continue;
                }
                builds.push(MmmcViewBuild {
                    id: view,
                    scenario,
                    library,
                    parasitics,
                    delay_type,
                    policy,
                });
            }
        }
        if builds.is_empty() {
            return Ok(None);
        }
        let build_descriptor_bytes =
            opto_core::resident::slice_bytes::<MmmcViewBuild<'_>>(builds.len());
        let scenario_owner_bytes =
            opto_core::resident::slice_bytes::<MmmcScenarioOwners>(scenario_owners.len());
        let build_runtime = runtime.with_parallelism_limit(
            NonZeroUsize::new(MAX_PARALLEL_VIEW_BUILDS)
                .expect("MMMC parallel view-build limit is nonzero"),
        );
        let mut groups = Vec::<(opto_timing::TimingTopologySchema, Vec<MmmcViewBuild<'_>>)>::new();
        for build in builds {
            let schema = build.library.topology_schema();
            if let Some(group) = groups
                .iter_mut()
                .find(|(candidate, _)| candidate == &schema)
            {
                group.1.push(build);
            } else {
                groups.push((schema, vec![build]));
            }
        }
        let group_descriptor_bytes = topology_group_descriptor_memory_bytes(&groups);
        let grouping_scratch_high_water_bytes = build_descriptor_bytes
            .checked_add(group_descriptor_bytes)
            .ok_or_else(|| crate::SynthError::capacity("MMMC topology grouping memory"))?;
        let leader_tasks = groups
            .iter()
            .map(|(_, group)| {
                let build = group[0];
                Task::new(
                    TaskKey::new(MMMC_VIEW_BUILD_TASK_DOMAIN, u64::from(build.id.raw())),
                    build,
                )
            })
            .collect();
        let leader_task_count = groups.len();
        let mut leaders = build_runtime.map_ordered_nested(leader_tasks, |build, nested| {
            let mut model = TimingModel::from_mapped_with_parasitics(
                mapped,
                design_id,
                port_bindings,
                materialize_view_library(build),
                build.parasitics.clone(),
            )?;
            model.set_object_bindings(Arc::clone(object_bindings));
            build_view_owner(build, model, nested, runtime.clone())
        })?;
        let leader_build_scratch_high_water_bytes =
            parallel_build_scratch_high_water_bytes(&leaders)?;
        let leader_scratch_high_water_bytes = checked_memory_sum(
            [
                group_descriptor_bytes,
                scheduler_scratch_bytes::<MmmcViewBuild<'_>>(leader_task_count),
                leader_build_scratch_high_water_bytes,
            ],
            "MMMC leader scratch memory",
        )?;
        let leader_resident_bytes = owner_rows_resident_memory_bytes(&leaders);
        let leader_construction_high_water_bytes = checked_memory_sum(
            [
                scenario_owner_bytes,
                leader_resident_bytes,
                leader_scratch_high_water_bytes,
            ],
            "MMMC leader construction memory",
        )?;
        let mut tasks = Vec::new();
        for ((_, group), (_, leader)) in groups.iter().zip(&leaders) {
            tasks.extend(group[1..].iter().copied().map(|build| {
                Task::new(
                    TaskKey::new(MMMC_VIEW_BUILD_TASK_DOMAIN, u64::from(build.id.raw())),
                    (build, leader.model()),
                )
            }));
        }
        let follower_task_count = tasks.len();
        let followers = build_runtime.map_ordered_nested(tasks, |(build, leader), nested| {
            let prepared = leader.prepared_topology();
            let model = TimingModel::fork_prepared_view(
                &prepared,
                materialize_view_library(build),
                build.parasitics.clone(),
            )?;
            build_view_owner(build, model, nested, runtime.clone())
        })?;
        let follower_build_scratch_high_water_bytes =
            parallel_build_scratch_high_water_bytes(&followers)?;
        let follower_scratch_high_water_bytes = checked_memory_sum(
            [
                group_descriptor_bytes,
                scheduler_scratch_bytes::<(MmmcViewBuild<'_>, &TimingModel)>(follower_task_count),
                follower_build_scratch_high_water_bytes,
            ],
            "MMMC follower scratch memory",
        )?;
        let follower_resident_bytes = split_owner_rows_resident_memory_bytes(&leaders, &followers);
        let follower_construction_high_water_bytes = checked_memory_sum(
            [
                scenario_owner_bytes,
                follower_resident_bytes,
                follower_scratch_high_water_bytes,
            ],
            "MMMC follower construction memory",
        )?;
        let construction_scratch_high_water_bytes = grouping_scratch_high_water_bytes
            .max(leader_scratch_high_water_bytes)
            .max(follower_scratch_high_water_bytes);
        leaders.extend(followers);
        leaders.sort_unstable_by_key(|(view, _)| view.id);
        let (views, owners): (Vec<_>, Vec<_>) = leaders.into_iter().unzip();
        let mut timing = Self {
            owners,
            views: views.into_boxed_slice(),
            scenario_owners: scenario_owners.into_boxed_slice(),
            construction_scratch_high_water_bytes,
            construction_high_water_bytes: 0,
        };
        timing.validate_views(scenarios)?;
        timing.construction_high_water_bytes = timing
            .resident_memory_bytes()
            .max(
                scenario_owner_bytes
                    .checked_add(grouping_scratch_high_water_bytes)
                    .ok_or_else(|| {
                        crate::SynthError::capacity("MMMC topology grouping high-water")
                    })?,
            )
            .max(leader_construction_high_water_bytes)
            .max(follower_construction_high_water_bytes);
        Ok(Some(timing))
    }

    pub(crate) fn owners_mut(&mut self) -> &mut [IncrementalTiming] {
        &mut self.owners
    }

    pub(crate) fn owners(&self) -> &[IncrementalTiming] {
        &self.owners
    }

    pub(crate) fn memory_usage(&self) -> MmmcTimingMemory {
        let resident_bytes = self.resident_memory_bytes();
        MmmcTimingMemory {
            resident_bytes,
            construction_scratch_high_water_bytes: self.construction_scratch_high_water_bytes,
            construction_high_water_bytes: self
                .construction_high_water_bytes
                .max(resident_bytes)
                .max(self.construction_scratch_high_water_bytes),
        }
    }

    pub(crate) fn resident_memory_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            .saturating_add(opto_core::resident::slice_bytes::<IncrementalTiming>(
                self.owners.len(),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<MmmcView>(
                self.views.len(),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<MmmcScenarioOwners>(
                self.scenario_owners.len(),
            ))
            .saturating_add(owner_payload_resident_memory_bytes(
                self.owners.len(),
                |owner| &self.owners[owner],
            ))
    }

    pub(crate) fn owners_and_views(&mut self) -> (&mut [IncrementalTiming], &[MmmcView]) {
        (&mut self.owners, &self.views)
    }

    pub(crate) fn view_ids(&self) -> impl ExactSizeIterator<Item = AnalysisViewId> + '_ {
        self.views.iter().map(|view| view.id)
    }

    pub(crate) fn power_owner_ids(&self) -> impl Iterator<Item = Option<AnalysisViewId>> + '_ {
        self.scenario_owners.iter().map(|owners| {
            self.views
                .binary_search_by_key(&owners.late, |view| view.id)
                .is_ok()
                .then_some(owners.late)
                .or_else(|| {
                    self.views
                        .binary_search_by_key(&owners.early, |view| view.id)
                        .is_ok()
                        .then_some(owners.early)
                })
        })
    }

    /// Resolves a scenario by stable identity into its canonical late/max and
    /// early/min owners.
    pub(crate) fn scenario_views(
        &self,
        scenario: opto_timing::ScenarioId,
    ) -> Result<MmmcScenarioViews<'_>, crate::SynthError> {
        let owners = self
            .scenario_owners
            .iter()
            .find(|candidate| candidate.id == scenario)
            .ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "MMMC timing has no owner for scenario {scenario:?}"
                ))
            })?;
        let resolve = |view: AnalysisViewId| {
            self.owner(view)
                .map(|timing| timing.map(|timing| MmmcViewRef { id: view, timing }))
        };
        let late = resolve(owners.late)?;
        let early = resolve(owners.early)?;
        Ok(MmmcScenarioViews { late, early })
    }

    pub(crate) fn diagnostic_analyses(
        &self,
    ) -> Result<Vec<(AnalysisViewId, TimingAnalysis)>, crate::SynthError> {
        self.owners
            .iter()
            .zip(&self.views)
            .filter(|(_, view)| view.policy.timing)
            .map(|(owner, view)| Ok((view.id, owner.analyze()?)))
            .collect()
    }

    pub(crate) fn mapped_net_states(
        &self,
        net: opto_ir::mapped::NetId,
    ) -> Result<Vec<MmmcNetState>, crate::SynthError> {
        let state = |view: AnalysisViewId| {
            Ok(MmmcNetState {
                view,
                state: self
                    .owner(view)?
                    .and_then(|owner| owner.mapped_net_state(net)),
            })
        };
        self.scenario_owners
            .iter()
            .flat_map(|owners| [owners.late, owners.early])
            .map(state)
            .collect()
    }

    /// Conservatively admits a replacement when any available MMMC view can
    /// improve one of its characterized timing metrics.
    pub(crate) fn replacement_can_improve_timing(
        &self,
        instance: TimingInstanceId,
        candidate: opto_library::TargetCellRef<'_>,
    ) -> bool {
        self.owners
            .iter()
            .any(|owner| owner.replacement_can_improve_timing(instance, candidate))
    }

    /// Reduces one local replacement estimate across every available MMMC
    /// view. Missing characterization in any view makes the aggregate
    /// inexact, so callers retain their conservative fallback estimate.
    pub(crate) fn estimate_cell(
        &self,
        instance: TimingInstanceId,
        candidate: opto_library::TargetCellRef<'_>,
    ) -> Option<CellTimingEstimate> {
        let mut estimates = self
            .owners
            .iter()
            .map(|owner| owner.estimate_cell(instance, candidate));
        let mut aggregate = estimates.next()??;
        for estimate in estimates {
            let estimate = estimate?;
            aggregate.delay = aggregate.delay.max(estimate.delay);
            aggregate.transition = aggregate.transition.max(estimate.transition);
            aggregate.input_capacitance =
                aggregate.input_capacitance.max(estimate.input_capacitance);
        }
        Some(aggregate)
    }

    pub(crate) fn metrics(&mut self) -> Result<MmmcMetrics, crate::SynthError> {
        aggregate_timing_owners(&mut self.owners, &self.views)
    }

    pub(crate) fn summary(&mut self) -> Result<crate::TimingSummary, crate::SynthError> {
        let metrics = self.metrics()?;
        Ok(crate::TimingSummary {
            arrival: metrics.analysis.arrival(),
            slack: metrics.analysis.wns(),
            tns: metrics.analysis.tns(),
            violating_paths: metrics.analysis.violating_paths(),
            worst_design_rule_ratio: metrics.design_rule_summary.worst_ratio(),
            design_rule_violations: metrics.design_rule_summary.violations(),
        })
    }

    /// Returns the union of each enabled view's own worst-slack frontier.
    ///
    /// A single aggregate WNS threshold is incorrect for MMMC: a very bad
    /// path in one corner would hide the locally worst path in every less-bad
    /// corner.  Candidate generation must therefore discover a frontier per
    /// owner and only then fold the stable timing-instance identities.
    pub(crate) fn critical_instances(
        &mut self,
    ) -> Result<Vec<TimingInstanceId>, crate::SynthError> {
        Ok(self.critical_frontier()?.instances)
    }

    /// Returns the union of each timing view's locally critical instances and
    /// mapped nets. Both sets use the same per-view slack threshold so
    /// post-map transforms cannot accidentally widen a timing frontier by
    /// walking every pin incident to a critical instance.
    pub(crate) fn critical_frontier(
        &mut self,
    ) -> Result<CriticalTimingFrontier, crate::SynthError> {
        let mut instances = BTreeSet::new();
        let mut mapped_nets = BTreeSet::new();
        for (owner, view) in self.owners.iter_mut().zip(&self.views) {
            if !view.policy.timing {
                continue;
            }
            let Some(quality) = owner.quality_summary() else {
                continue;
            };
            let threshold = quality.wns().map_or(0.0, |wns| wns.min(0.0));
            instances.extend(owner.instances_with_slack_at_most(threshold)?);
            mapped_nets.extend(owner.mapped_nets_with_slack_at_most(threshold)?);
        }
        Ok(CriticalTimingFrontier {
            instances: instances.into_iter().collect(),
            mapped_nets: mapped_nets.into_iter().collect(),
        })
    }

    /// Returns the union of timing instances at or below `threshold` in every
    /// enabled max/min view.
    pub(crate) fn instances_with_slack_at_most_all(
        &mut self,
        threshold: f64,
    ) -> Result<Vec<TimingInstanceId>, crate::SynthError> {
        let mut instances = BTreeSet::new();
        for (owner, view) in self.owners.iter_mut().zip(&self.views) {
            if view.policy.timing {
                instances.extend(owner.instances_with_slack_at_most(threshold)?);
            }
        }
        Ok(instances.into_iter().collect())
    }

    /// Returns the stable union of mapped nets at or below `threshold` in
    /// every enabled timing view.
    ///
    /// Unlike [`Self::critical_frontier`], this does not stop at each view's
    /// worst path. Global topology transforms such as HFNS must see every
    /// currently violating net before constructing one deterministic forest.
    pub(crate) fn mapped_nets_with_slack_at_most_all(
        &mut self,
        threshold: f64,
    ) -> Result<Vec<opto_ir::mapped::NetId>, crate::SynthError> {
        let mut mapped_nets = BTreeSet::new();
        for (owner, view) in self.owners.iter_mut().zip(&self.views) {
            if view.policy.timing {
                mapped_nets.extend(owner.mapped_nets_with_slack_at_most(threshold)?);
            }
        }
        Ok(mapped_nets.into_iter().collect())
    }

    #[cfg(test)]
    pub(crate) fn from_owner_for_test(owner: IncrementalTiming, policy: MmmcViewPolicy) -> Self {
        Self {
            owners: vec![owner],
            views: Box::new([MmmcView {
                id: AnalysisViewId::from_raw(0),
                scenario: opto_timing::ScenarioId::from_raw(0),
                delay_type: DelayType::Max,
                policy,
            }]),
            // Power resolves its owner through this table, so a test service
            // that reports no scenario owners is indistinguishable from one
            // whose scenarios and views disagree.
            scenario_owners: Box::new([MmmcScenarioOwners {
                id: opto_timing::ScenarioId::from_raw(0),
                late: AnalysisViewId::from_raw(0),
                early: AnalysisViewId::from_raw(0),
            }]),
            construction_scratch_high_water_bytes: 0,
            construction_high_water_bytes: 0,
        }
    }

    fn owner(&self, view: AnalysisViewId) -> Result<Option<&IncrementalTiming>, crate::SynthError> {
        let Ok(owner) = self.views.binary_search_by_key(&view, |entry| entry.id) else {
            return Ok(None);
        };
        self.owners.get(owner).map(Some).ok_or_else(|| {
            crate::SynthError::invariant("MMMC view metadata and owners are misaligned")
        })
    }

    fn validate_views(&self, scenarios: &ScenarioSet) -> Result<(), crate::SynthError> {
        if self.owners.len() != self.views.len()
            || self.views.windows(2).any(|pair| pair[0].id >= pair[1].id)
        {
            return Err(crate::SynthError::invariant(
                "MMMC owners are not in unique canonical analysis-view order",
            ));
        }
        for view in &self.views {
            let Some((scenario, delay_type)) = scenarios.analysis_view(view.id) else {
                return Err(crate::SynthError::invariant(
                    "MMMC owner references a foreign analysis-view ID",
                ));
            };
            if scenario.id() != view.scenario || delay_type != view.delay_type {
                return Err(crate::SynthError::invariant(
                    "MMMC analysis-view metadata disagrees with its scenario set",
                ));
            }
        }
        Ok(())
    }
}

fn parallel_build_scratch_high_water_bytes(
    rows: &[(MmmcView, IncrementalTiming)],
) -> Result<usize, crate::SynthError> {
    let mut scratch = rows
        .iter()
        .map(|(_, owner)| owner.memory_usage().construction_scratch_high_water_bytes)
        .collect::<Vec<_>>();
    scratch.sort_unstable_by_key(|&bytes| std::cmp::Reverse(bytes));
    scratch
        .into_iter()
        .take(MAX_PARALLEL_VIEW_BUILDS)
        .try_fold(0usize, usize::checked_add)
        .ok_or_else(|| crate::SynthError::capacity("parallel MMMC construction scratch memory"))
}

fn topology_group_descriptor_memory_bytes(
    groups: &[(opto_timing::TimingTopologySchema, Vec<MmmcViewBuild<'_>>)],
) -> usize {
    groups.iter().fold(
        opto_core::resident::slice_bytes::<(
            opto_timing::TimingTopologySchema,
            Vec<MmmcViewBuild<'_>>,
        )>(groups.len()),
        |bytes, (_, group)| {
            bytes.saturating_add(opto_core::resident::slice_bytes::<MmmcViewBuild<'_>>(
                group.len(),
            ))
        },
    )
}

fn scheduler_scratch_bytes<I>(task_count: usize) -> usize {
    let result_envelope = opto_core::resident::slice_bytes::<
        Result<(MmmcView, IncrementalTiming), crate::SynthError>,
    >(task_count)
    .saturating_sub(opto_core::resident::slice_bytes::<(
        MmmcView,
        IncrementalTiming,
    )>(task_count));
    opto_core::resident::slice_bytes::<Task<I>>(task_count).saturating_add(result_envelope)
}

fn checked_memory_sum(
    values: impl IntoIterator<Item = usize>,
    resource: &'static str,
) -> Result<usize, crate::SynthError> {
    values
        .into_iter()
        .try_fold(0usize, usize::checked_add)
        .ok_or_else(|| crate::SynthError::capacity(resource))
}

fn owner_rows_resident_memory_bytes(rows: &[(MmmcView, IncrementalTiming)]) -> usize {
    opto_core::resident::slice_bytes::<(MmmcView, IncrementalTiming)>(rows.len()).saturating_add(
        owner_payload_resident_memory_bytes(rows.len(), |owner| &rows[owner].1),
    )
}

fn split_owner_rows_resident_memory_bytes(
    leaders: &[(MmmcView, IncrementalTiming)],
    followers: &[(MmmcView, IncrementalTiming)],
) -> usize {
    let leader_count = leaders.len();
    opto_core::resident::slice_bytes::<(MmmcView, IncrementalTiming)>(leader_count)
        .saturating_add(opto_core::resident::slice_bytes::<(
            MmmcView,
            IncrementalTiming,
        )>(followers.len()))
        .saturating_add(owner_payload_resident_memory_bytes(
            leader_count + followers.len(),
            |owner| {
                if owner < leader_count {
                    &leaders[owner].1
                } else {
                    &followers[owner - leader_count].1
                }
            },
        ))
}

fn owner_payload_resident_memory_bytes<'a>(
    owner_count: usize,
    owner_at: impl Copy + Fn(usize) -> &'a IncrementalTiming,
) -> usize {
    let mut bytes = 0usize;
    let mut model_components = BTreeSet::new();
    for owner in 0..owner_count {
        let row = owner_at(owner);
        bytes = bytes.saturating_add(
            row.resident_memory_bytes()
                .saturating_sub(std::mem::size_of::<IncrementalTiming>()),
        );
        for component in row.shared_model_components() {
            if !model_components.insert((component.kind, component.identity)) {
                bytes = bytes.saturating_sub(component.bytes);
            }
        }
        if (0..owner).any(|previous| row.shares_timing_context(owner_at(previous))) {
            bytes = bytes.saturating_sub(row.shared_timing_context_resident_memory_bytes());
        }
        if (0..owner).any(|previous| row.shares_object_bindings(owner_at(previous))) {
            bytes = bytes.saturating_sub(row.shared_object_bindings_resident_memory_bytes());
        }
    }
    bytes
}

fn materialize_view_library(build: MmmcViewBuild<'_>) -> opto_timing::TimingLibrary {
    let mut library = build.library.clone();
    library.power = (**build.scenario.power().library()).clone();
    library
}

fn build_view_owner(
    build: MmmcViewBuild<'_>,
    model: TimingModel,
    build_context: &ExecutionContext,
    runtime: ExecutionContext,
) -> Result<(MmmcView, IncrementalTiming), crate::SynthError> {
    let owner = IncrementalTiming::new_for_optimization_with_build_context(
        Arc::clone(build.scenario.constraints()),
        model,
        ReportTimingOptions {
            delay_type: build.delay_type,
            checks: build.policy.checks,
            ..ReportTimingOptions::default()
        },
        build_context,
        runtime,
    )?;
    Ok((
        MmmcView {
            id: build.id,
            scenario: build.scenario.id(),
            delay_type: build.delay_type,
            policy: build.policy,
        },
        owner,
    ))
}

fn checks_for_view(
    checks: opto_timing::ScenarioCheckSet,
    delay_type: DelayType,
) -> opto_timing::ScenarioCheckSet {
    opto_timing::ScenarioCheckSet {
        setup: checks.setup && delay_type == DelayType::Max,
        hold: checks.hold && delay_type == DelayType::Min,
        recovery: checks.recovery && delay_type == DelayType::Max,
        removal: checks.removal && delay_type == DelayType::Min,
        pulse_width: checks.pulse_width,
        max_transition: checks.max_transition,
        max_capacitance: checks.max_capacitance,
        max_fanout: checks.max_fanout,
    }
}

fn timing_checks_enabled(checks: opto_timing::ScenarioCheckSet) -> bool {
    checks.setup || checks.hold || checks.recovery || checks.removal || checks.pulse_width
}

pub(crate) fn aggregate_timing_owners(
    owners: &mut [IncrementalTiming],
    views: &[MmmcView],
) -> Result<MmmcMetrics, crate::SynthError> {
    let mut arrival = f64::NEG_INFINITY;
    let mut wns: Option<f64> = None;
    let mut tns = 0.0;
    let mut violating_paths = 0usize;
    let mut worst_ratio = 0.0f64;
    let mut total_excess = 0.0;
    let mut rule_violations = 0usize;
    let mut design_rules = Vec::new();
    if owners.len() != views.len() {
        return Err(crate::SynthError::invariant(
            "MMMC timing owners and view policies are misaligned",
        ));
    }
    for (owner, view) in owners.iter_mut().zip(views) {
        if let Some(quality) = owner.quality_summary().filter(|_| view.policy.timing) {
            arrival = arrival.max(quality.arrival());
            if let Some(view_wns) = quality.wns() {
                wns = Some(wns.map_or(view_wns, |current| current.min(view_wns)));
            }
            tns += quality.tns();
            violating_paths = violating_paths
                .checked_add(quality.violating_paths())
                .ok_or_else(|| crate::SynthError::capacity("MMMC timing violation count"))?;
        }
        let mut owner_rules = owner
            .design_rule_violations()
            .into_iter()
            .filter(|violation| design_rule_enabled(view.policy.checks, violation.kind))
            .collect::<Vec<_>>();
        for violation in &owner_rules {
            worst_ratio = worst_ratio.max(violation.actual / violation.limit);
            total_excess += (violation.actual - violation.limit).max(0.0);
        }
        rule_violations = rule_violations
            .checked_add(owner_rules.len())
            .ok_or_else(|| crate::SynthError::capacity("MMMC design-rule violation count"))?;
        design_rules.append(&mut owner_rules);
    }
    if arrival == f64::NEG_INFINITY {
        arrival = 0.0;
    }
    Ok(MmmcMetrics {
        analysis: TimingQualitySummary::aggregate(arrival, wns, tns, violating_paths),
        design_rule_summary: DesignRuleSummary::aggregate(
            worst_ratio,
            total_excess,
            rule_violations,
        ),
        design_rules,
    })
}

fn design_rule_enabled(
    checks: opto_timing::ScenarioCheckSet,
    kind: opto_timing::DesignRuleKind,
) -> bool {
    match kind {
        opto_timing::DesignRuleKind::MaxTransition => checks.max_transition,
        opto_timing::DesignRuleKind::MaxCapacitance => checks.max_capacitance,
        opto_timing::DesignRuleKind::MaxFanout => checks.max_fanout,
    }
}

impl MmmcTiming {
    /// Compacts retained path storage in *every* view.
    pub(crate) fn compact_every_view(&mut self) -> Result<(), crate::SynthError> {
        for owner in &mut self.owners {
            owner
                .compact_paths_if_needed()
                .map_err(crate::SynthError::from)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenario_checks_are_partitioned_by_analysis_polarity() {
        let max = checks_for_view(opto_timing::ScenarioCheckSet::ALL, DelayType::Max);
        assert!(max.setup);
        assert!(max.recovery);
        assert!(!max.hold);
        assert!(!max.removal);
        assert!(max.pulse_width);
        assert!(max.max_transition && max.max_capacitance && max.max_fanout);

        let min = checks_for_view(opto_timing::ScenarioCheckSet::ALL, DelayType::Min);
        assert!(!min.setup);
        assert!(!min.recovery);
        assert!(min.hold);
        assert!(min.removal);
        assert!(min.pulse_width);
        assert!(min.max_transition && min.max_capacitance && min.max_fanout);
    }
}
