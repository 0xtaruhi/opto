// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::TimingOptimizationSession;
use super::buffering::{self, FanoutTreePlan};
use super::forest::{self, EvaluationPolicy};
use crate::OptimizationPhase;
use opto_ir::mapped::{MappedGenerationId, MappedNetlist, NetId};
use opto_library::{TargetCellSet, TargetPinDirection};
use opto_runtime::{ExecutionContext, Task, TaskKey};

const PLAN_TASK_DOMAIN: u32 = 0x4846_4e53;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MappedFanoutLoad {
    net: NetId,
    sinks: u32,
    fanout_load: f64,
    pin_capacitance: f64,
}

const _: () = assert!(std::mem::size_of::<MappedFanoutLoad>() <= 24);

impl MappedFanoutLoad {
    pub(super) const fn net(self) -> NetId {
        self.net
    }

    pub(super) const fn sinks(self) -> u32 {
        self.sinks
    }

    pub(super) const fn fanout_load(self) -> f64 {
        self.fanout_load
    }

    pub(super) const fn pin_capacitance(self) -> f64 {
        self.pin_capacitance
    }
}

/// Compact, generation-stamped electrical summary produced immediately after
/// initial mapping and consumed by whole-net HFNS planning.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MappedFanoutLoadProfile {
    mapped_generation: MappedGenerationId,
    mapped_revision: u64,
    rows: Box<[MappedFanoutLoad]>,
}

impl MappedFanoutLoadProfile {
    pub(crate) fn build(
        mapped: &MappedNetlist,
        library: &TargetCellSet,
    ) -> Result<Self, crate::SynthError> {
        let mut rows = Vec::new();
        for net in mapped.net_ids() {
            let Some(pins) = mapped.pins_on_net(net) else {
                continue;
            };
            let mut sinks = 0u32;
            let mut fanout_load = 0.0;
            let mut pin_capacitance = 0.0;
            for pin in pins {
                let Some(target) = buffering::library_pin(mapped, library, pin)? else {
                    continue;
                };
                if !matches!(
                    target.direction(),
                    TargetPinDirection::Input | TargetPinDirection::Inout
                ) {
                    continue;
                }
                sinks = sinks.checked_add(1).ok_or_else(|| {
                    crate::SynthError::capacity("mapped net sink count exceeds capacity")
                })?;
                fanout_load += target.design_fanout_load();
                pin_capacitance += target.design_input_capacitance();
            }
            if sinks >= 2 {
                rows.push(MappedFanoutLoad {
                    net,
                    sinks,
                    fanout_load,
                    pin_capacitance,
                });
            }
        }
        Ok(Self {
            mapped_generation: mapped.generation_id(),
            mapped_revision: mapped.edit_revision(),
            rows: rows.into_boxed_slice(),
        })
    }

    pub(super) fn validate(&self, mapped: &MappedNetlist) -> Result<(), crate::SynthError> {
        if self.mapped_generation != mapped.generation_id() {
            return Err(crate::SynthError::invariant(
                "mapped fanout/load profile belongs to another netlist generation",
            ));
        }
        if self.mapped_revision != mapped.edit_revision() {
            return Err(crate::SynthError::invariant(
                "mapped fanout/load profile does not describe the current netlist generation",
            ));
        }
        Ok(())
    }

    pub(super) fn row(&self, net: NetId) -> Option<MappedFanoutLoad> {
        self.rows
            .binary_search_by_key(&net, |row| row.net)
            .ok()
            .map(|index| self.rows[index])
    }

    pub(super) fn len(&self) -> usize {
        self.rows.len()
    }

    pub(super) fn maximum(&self) -> Option<MappedFanoutLoad> {
        self.rows.iter().copied().max_by(|left, right| {
            left.sinks
                .cmp(&right.sinks)
                .then_with(|| left.fanout_load.total_cmp(&right.fanout_load))
                .then_with(|| left.pin_capacitance.total_cmp(&right.pin_capacitance))
                .then_with(|| right.net.cmp(&left.net))
        })
    }
}

pub(super) fn synthesize(
    session: &mut TimingOptimizationSession<'_>,
    buffer_candidates: &[usize],
    fanout_load_profile: &MappedFanoutLoadProfile,
    runtime: &ExecutionContext,
) -> Result<(), crate::SynthError> {
    if buffer_candidates.is_empty() {
        return Ok(());
    }
    let mut candidate_nets = session
        .timing
        .mapped_nets_with_slack_at_most_all(0.0)?
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    candidate_nets.extend(
        session
            .design_rules()
            .iter()
            .filter_map(|violation| violation.mapped_net),
    );
    let tasks = candidate_nets
        .into_iter()
        .filter(|&net| {
            fanout_load_profile
                .row(net)
                .is_some_and(|load| load.sinks() >= 3)
        })
        .map(|net| {
            Ok(Task::new(
                TaskKey::new(PLAN_TASK_DOMAIN, net.index() as u64),
                (net, session.timing.mapped_net_states(net)?),
            ))
        })
        .collect::<Result<Vec<_>, crate::SynthError>>()?;
    let planned = {
        let mapped = &*session.mapped;
        let target_cells = &session.options.target_cells;
        let scenarios = session.scenarios();
        runtime.map_ordered(tasks, |(net, net_states)| {
            let mut sinks = buffering::net_sink_pins(mapped, target_cells, net)?
                .into_iter()
                .map(|(pin, _)| pin)
                .collect::<Vec<_>>();
            sinks.sort_unstable();
            let selection = buffering::select_fanout_tree_strategy(
                mapped,
                target_cells,
                scenarios,
                buffer_candidates,
                &sinks,
                &net_states,
            )?;
            Ok::<_, crate::SynthError>(selection.map(|selection| FanoutTreePlan {
                net,
                leaf_groups: selection.leaf_groups,
                strategy: selection.strategy,
                ordinal: 0,
            }))
        })?
    };
    let mut plans = planned.into_iter().flatten().collect::<Vec<_>>();
    for (ordinal, plan) in plans.iter_mut().enumerate() {
        plan.ordinal = ordinal;
    }
    if plans.is_empty() {
        return Ok(());
    }
    let trace = session.trace();
    if trace.is_enabled() {
        report_plan(trace, session.mapped, fanout_load_profile, &plans)?;
    }
    forest::evaluate(
        &plans,
        OptimizationPhase::FanoutTreeSynthesis,
        EvaluationPolicy::Complete,
        session,
        |mapped, implementations, options, plans| {
            buffering::fanout_forest_delta(mapped, implementations, &options.target_cells, plans)
        },
    )?;
    Ok(())
}

fn report_plan(
    trace: crate::api::diagnostics::SynthTrace,
    mapped: &MappedNetlist,
    fanout_load_profile: &MappedFanoutLoadProfile,
    plans: &[FanoutTreePlan],
) -> Result<(), crate::SynthError> {
    let sinks = plans.iter().try_fold(0usize, |total, plan| {
        total
            .checked_add(plan.sink_count())
            .ok_or_else(|| crate::SynthError::capacity("fanout-forest sink count exceeds capacity"))
    })?;
    let buffers = plans.iter().try_fold(0usize, |total, plan| {
        let count = buffering::fanout_tree_buffer_count(plan.sink_count(), plan.strategy)?;
        total.checked_add(count).ok_or_else(|| {
            crate::SynthError::capacity("fanout-forest buffer count exceeds capacity")
        })
    })?;
    let largest = plans
        .iter()
        .max_by(|left, right| {
            left.sink_count()
                .cmp(&right.sink_count())
                .then_with(|| right.net.cmp(&left.net))
        })
        .ok_or_else(|| crate::SynthError::invariant("fanout forest has no plans"))?;
    let load = fanout_load_profile.row(largest.net);
    let largest_name = mapped
        .net_name(largest.net)
        .map_or("<unnamed>", |name| name);
    crate::api::diagnostics::trace!(
        trace,
        "postmap.fanout_forest",
        "trees={} sinks={} buffers={} largest_net={:?} name={} largest_sinks={} \
         fanout_load={:.6} pin_capacitance={:.9}",
        plans.len(),
        sinks,
        buffers,
        largest.net,
        largest_name,
        largest.sink_count(),
        load.map_or(0.0, MappedFanoutLoad::fanout_load),
        load.map_or(0.0, MappedFanoutLoad::pin_capacitance),
    );
    Ok(())
}
