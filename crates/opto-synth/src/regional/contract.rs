// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    BoundaryCheckKind, BoundaryContract, BoundaryContractRow, BoundaryInputContract,
    BoundaryOutputContract, EarlyLate, FiniteValue, TimingTag, TimingTagId, TimingTagInterner,
    check_value_lane, input_transition_lane, path_timing_lane,
};
use opto_ir::word;
use opto_timing::{Scenario, ScenarioCheckSet, ScenarioSet, TimingEdge};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

mod exceptions;

use exceptions::{WordTimingEndpointIndex, filter_matches_boundary, regional_exception_classes};

type PathBudget = (Option<f64>, Option<f64>);

#[derive(Debug, Clone)]
pub(crate) struct RegionContractSet {
    contracts: Box<[Box<[BoundaryContract]>]>,
    delay_budgets: Box<[Option<f64>]>,
    timing_tags: TimingTagInterner,
}

impl RegionContractSet {
    pub(crate) fn allocate(
        module: &word::WordModule,
        regions: &crate::SynthesisRegionGraph,
        weights: &[Box<[f64]>],
        scenarios: &ScenarioSet,
        port_bindings: &opto_timing::PortBindings,
        object_bindings: &opto_timing::TimingObjectBindings,
        epoch: u32,
    ) -> Result<Self, crate::SynthError> {
        let mut timing_tags = TimingTagInterner::new();
        let budgets = scenarios
            .scenarios()
            .iter()
            .enumerate()
            .map(|(scenario_index, scenario)| {
                allocate_path_budgets(
                    regions,
                    weights,
                    scenario_index,
                    scenario.constraints().minimum_synthesis_delay(),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let delay_budgets = (0..regions.regions().len())
            .map(|row| {
                budgets
                    .iter()
                    .filter_map(|scenario| {
                        let (arrival, required) = scenario[row];
                        Some((required? - arrival?).max(0.0))
                    })
                    .min_by(f64::total_cmp)
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let clock_domains = scenarios
            .scenarios()
            .iter()
            .map(|scenario| region_clock_domains(module, regions, scenario, port_bindings))
            .collect::<Result<Vec<_>, _>>()?;
        let endpoint_index = WordTimingEndpointIndex::build(module, object_bindings);
        let exception_classes = scenarios
            .scenarios()
            .iter()
            .map(|scenario| {
                regional_exception_classes(regions, &endpoint_index, scenario.constraints())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut rows = Vec::with_capacity(regions.regions().len());
        for region in regions.regions() {
            let mut contracts = Vec::new();
            for &port in regions
                .input_ports(*region)
                .iter()
                .chain(regions.output_ports(*region))
            {
                let port = regions.port(port).ok_or_else(|| {
                    crate::SynthError::invariant("region contract references an unknown port")
                })?;
                let timing_port = timing_port(module, port.value(), port_bindings);
                let timing_endpoints = endpoint_index.endpoints(port.value(), timing_port);
                let mut contract_rows = Vec::new();
                for (scenario_index, scenario) in scenarios.scenarios().iter().enumerate() {
                    let clock_contexts = boundary_clock_contexts(
                        port,
                        &clock_domains[scenario_index],
                        scenario,
                        timing_port,
                    );
                    let tags = contract_tags(
                        &mut timing_tags,
                        scenario,
                        port.direction(),
                        &timing_endpoints,
                        &clock_contexts,
                        &exception_classes[scenario_index][region.row().index()],
                    )?;
                    let limits = PortScenarioLimits::read(
                        module,
                        port.value(),
                        timing_port,
                        scenario,
                        object_bindings,
                        budgets[scenario_index][region.row().index()],
                    )?;
                    for timing_tag in tags {
                        let check = timing_tags
                            .get(timing_tag)
                            .ok_or_else(|| {
                                crate::SynthError::invariant(
                                    "regional contract references an unknown timing tag",
                                )
                            })?
                            .check;
                        contract_rows.push(limits.contract_row(
                            scenario.id(),
                            timing_tag,
                            check,
                            port.direction(),
                        ));
                    }
                }
                contracts.push(
                    BoundaryContract::new(port, epoch, scenarios.generation(), contract_rows)
                        .map_err(|error| super::boundary::synthesis_error(&error))?,
                );
            }
            contracts.sort_by_key(|contract| contract.port().id());
            rows.push(contracts.into_boxed_slice());
        }
        Ok(Self {
            contracts: rows.into_boxed_slice(),
            delay_budgets,
            timing_tags,
        })
    }

    pub(crate) fn delay_budget(&self, row: crate::RegionRowId) -> Option<f64> {
        self.delay_budgets[row.index()]
    }

    pub(crate) fn contracts(&self, row: crate::RegionRowId) -> &[BoundaryContract] {
        &self.contracts[row.index()]
    }

    pub(crate) fn timing_tags(&self) -> &TimingTagInterner {
        &self.timing_tags
    }

    /// Folds the measured boundary responses back into every contract and
    /// widens the required time of the rows the coordinator marked dirty.
    ///
    /// Updates in place and returns the rows whose contracts actually changed;
    /// unchanged rows keep their frozen plan and footprint.
    pub(crate) fn reallocate_dirty<'a>(
        &mut self,
        dirty: &[crate::RegionRowId],
        plans: impl ExactSizeIterator<Item = &'a crate::RegionCoverPlan> + Clone,
        epoch: u32,
    ) -> Result<Box<[crate::RegionRowId]>, crate::SynthError> {
        if plans.len() != self.contracts.len() {
            return Err(crate::SynthError::invariant(
                "regional plans do not align with boundary contracts",
            ));
        }
        let dirty = dirty.iter().copied().collect::<BTreeSet<_>>();
        let mut upstream = BTreeMap::new();
        let mut downstream = BTreeMap::new();
        for plan in plans.clone() {
            for contract in plan.boundary_response() {
                let Some(response) = plan
                    .measured_response()
                    .iter()
                    .find(|response| response.port_semantic_key == contract.port().semantic_key())
                else {
                    continue;
                };
                let target = match contract.port().direction() {
                    crate::RegionPortDirection::Input => &mut downstream,
                    crate::RegionPortDirection::Output => &mut upstream,
                };
                for response in &response.rows {
                    target.insert(
                        (
                            contract.port().semantic_key(),
                            response.scenario,
                            response.timing_tag,
                        ),
                        *response,
                    );
                }
            }
        }
        let mut changed_rows = BTreeSet::new();
        for (row_index, (current, plan)) in self.contracts.iter_mut().zip(plans).enumerate() {
            let row = crate::RegionRowId::from_index(row_index)?;
            let extra = if dirty.contains(&row) {
                (-plan.cost().minimum_slack.get()).max(0.0)
            } else {
                0.0
            };
            let extra = finite(extra)?;
            let mut reallocated = Vec::with_capacity(current.len());
            for contract in current.iter() {
                let contract_rows = contract
                    .rows()
                    .iter()
                    .copied()
                    .map(|mut contract_row| {
                        let key = (
                            contract.port().semantic_key(),
                            contract_row.scenario,
                            contract_row.timing_tag,
                        );
                        if let Some(mut input) = contract_row.input
                            && let Some(response) = upstream.get(&key)
                        {
                            update_present_timing_lanes(&mut input.arrival, response.arrival);
                            update_present_timing_lanes(&mut input.transition, response.transition);
                            input.activity = response.activity;
                            contract_row.input = Some(input);
                        }
                        if let Some(mut output) = contract_row.output {
                            if extra.get() > 0.0 {
                                for value in [
                                    &mut output.required.late.rise,
                                    &mut output.required.late.fall,
                                ] {
                                    *value = value
                                        .map(|required| {
                                            finite((required.get() + extra.get()).min(f64::MAX))
                                        })
                                        .transpose()?;
                                }
                                for value in [
                                    &mut output.required.early.rise,
                                    &mut output.required.early.fall,
                                ] {
                                    *value = value
                                        .map(|required| {
                                            finite((required.get() - extra.get()).max(-f64::MAX))
                                        })
                                        .transpose()?;
                                }
                            }
                            if let Some(response) = downstream.get(&key) {
                                output.capacitance = response.input_capacitance;
                            }
                            contract_row.output = Some(output);
                        }
                        Ok::<_, crate::SynthError>(contract_row)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if contract_rows.as_slice() == contract.rows() {
                    reallocated.push(contract.clone());
                    continue;
                }
                changed_rows.insert(row);
                reallocated.push(
                    BoundaryContract::new(
                        contract.port(),
                        epoch,
                        contract.scenario_generation(),
                        contract_rows,
                    )
                    .map_err(|error| super::boundary::synthesis_error(&error))?,
                );
            }
            *current = reallocated.into_boxed_slice();
        }
        Ok(changed_rows.into_iter().collect())
    }
}

/// Every constraint one boundary port draws from one scenario.
///
/// These are read once per (port, scenario) and then projected onto each timing
/// tag, which is what actually varies per contract row.
#[derive(Debug, Clone, Copy)]
struct PortScenarioLimits {
    transition: Option<FiniteValue>,
    activity: Option<opto_timing::ScenarioSwitchingActivity>,
    load: Option<FiniteValue>,
    arrival: Option<FiniteValue>,
    required: Option<FiniteValue>,
    minimum_delay: Option<FiniteValue>,
    maximum_transition: Option<FiniteValue>,
    maximum_capacitance: Option<FiniteValue>,
    maximum_fanout: Option<FiniteValue>,
}

impl PortScenarioLimits {
    fn read(
        module: &word::WordModule,
        value: word::ValueId,
        timing_port: Option<opto_timing::PortId>,
        scenario: &opto_timing::Scenario,
        object_bindings: &opto_timing::TimingObjectBindings,
        budget: (Option<f64>, Option<f64>),
    ) -> Result<Self, crate::SynthError> {
        let constraints = scenario.constraints();
        let rule = |kind| {
            design_rule_limit(constraints, kind, timing_port)
                .map(finite)
                .transpose()
        };
        let required = budget.1.map(finite).transpose()?;
        Ok(Self {
            transition: timing_port
                .and_then(|port| constraints.input_transition_on(port))
                .map(finite)
                .transpose()?,
            activity: boundary_activity(module, value, timing_port, scenario, object_bindings),
            load: timing_port
                .and_then(|port| constraints.load_on(port))
                .map(finite)
                .transpose()?,
            arrival: budget.0.map(finite).transpose()?,
            required,
            minimum_delay: required.map(|_| finite(0.0)).transpose()?,
            maximum_transition: rule(opto_timing::DesignRuleKind::MaxTransition)?,
            maximum_capacitance: rule(opto_timing::DesignRuleKind::MaxCapacitance)?,
            maximum_fanout: rule(opto_timing::DesignRuleKind::MaxFanout)?,
        })
    }

    /// Projects these limits onto one timing tag. Only the lanes the tag's
    /// check actually constrains are populated.
    fn contract_row(
        self,
        scenario: opto_timing::ScenarioId,
        timing_tag: TimingTagId,
        check: BoundaryCheckKind,
        direction: crate::RegionPortDirection,
    ) -> BoundaryContractRow {
        let (input, output) = match direction {
            crate::RegionPortDirection::Input => (
                Some(BoundaryInputContract {
                    arrival: path_timing_lane(check, self.minimum_delay, self.arrival),
                    transition: input_transition_lane(check, self.transition),
                    activity: self.activity,
                }),
                None,
            ),
            crate::RegionPortDirection::Output => (
                None,
                Some(BoundaryOutputContract {
                    required: path_timing_lane(check, self.minimum_delay, self.required),
                    capacitance: EarlyLate::new(self.load, self.load),
                    fanout_load: EarlyLate::new(None, None),
                    maximum_transition: check_value_lane(
                        check,
                        BoundaryCheckKind::MaxTransition,
                        self.maximum_transition,
                    ),
                    maximum_capacitance: check_value_lane(
                        check,
                        BoundaryCheckKind::MaxCapacitance,
                        self.maximum_capacitance,
                    ),
                    maximum_fanout: (check == BoundaryCheckKind::MaxFanout)
                        .then_some(self.maximum_fanout)
                        .flatten(),
                }),
            ),
        };
        BoundaryContractRow {
            scenario,
            timing_tag,
            input,
            output,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct TagClockContext {
    launch_clock: Option<opto_timing::ClockId>,
    capture_clock: Option<opto_timing::ClockId>,
    launch_edge: TimingEdge,
    capture_edge: TimingEdge,
}

impl Default for TagClockContext {
    fn default() -> Self {
        Self {
            launch_clock: None,
            capture_clock: None,
            launch_edge: TimingEdge::Rise,
            capture_edge: TimingEdge::Rise,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct RegionClockDomains {
    launch: BTreeSet<opto_timing::ClockId>,
    capture: BTreeSet<opto_timing::ClockId>,
}

fn region_clock_domains(
    module: &word::WordModule,
    regions: &crate::SynthesisRegionGraph,
    scenario: &Scenario,
    port_bindings: &opto_timing::PortBindings,
) -> Result<Vec<RegionClockDomains>, crate::SynthError> {
    let mut own = vec![BTreeSet::new(); regions.regions().len()];
    for region in regions.regions() {
        for &operation in regions.operations(*region) {
            let operation = module.operation(operation).ok_or_else(|| {
                crate::SynthError::invariant("clock-domain region references an unknown operation")
            })?;
            let clock = match &operation.kind {
                word::OpKind::Register(register) => Some(register.clock),
                word::OpKind::Latch(latch) => Some(latch.enable.value),
                _ => None,
            };
            if let Some(clock) = clock {
                own[region.row().index()].extend(clocks_on_value(
                    module,
                    clock,
                    scenario.constraints(),
                    port_bindings,
                ));
            }
        }
        for &memory in regions.memories(*region) {
            for clock in module
                .memory_read_ports()
                .iter()
                .filter(|port| port.memory == memory)
                .filter_map(|port| match port.timing {
                    word::MemoryReadTiming::Asynchronous => None,
                    word::MemoryReadTiming::Synchronous { clock, .. } => Some(clock.value),
                })
                .chain(
                    module
                        .memory_write_ports()
                        .iter()
                        .filter(|port| port.memory == memory)
                        .map(|port| port.clock.value),
                )
            {
                own[region.row().index()].extend(clocks_on_value(
                    module,
                    clock,
                    scenario.constraints(),
                    port_bindings,
                ));
            }
        }
    }
    let mut domains = own
        .iter()
        .map(|clocks| RegionClockDomains {
            launch: clocks.clone(),
            capture: clocks.clone(),
        })
        .collect::<Vec<_>>();
    for _ in 0..regions.regions().len() {
        let mut changed = false;
        for region in regions.regions().iter().filter(|region| {
            !matches!(
                region.kind(),
                crate::SynthesisRegionKind::State | crate::SynthesisRegionKind::Memory
            )
        }) {
            let row = region.row();
            let before = domains[row.index()].launch.len();
            for &predecessor in regions.predecessors(*region) {
                let clocks = domains[predecessor.index()].launch.clone();
                domains[row.index()].launch.extend(clocks);
            }
            changed |= domains[row.index()].launch.len() != before;
        }
        if !changed {
            break;
        }
    }
    for _ in 0..regions.regions().len() {
        let mut changed = false;
        for region in regions.regions().iter().rev().filter(|region| {
            !matches!(
                region.kind(),
                crate::SynthesisRegionKind::State | crate::SynthesisRegionKind::Memory
            )
        }) {
            let row = region.row();
            let before = domains[row.index()].capture.len();
            for &successor in regions.successors(*region) {
                let clocks = domains[successor.index()].capture.clone();
                domains[row.index()].capture.extend(clocks);
            }
            changed |= domains[row.index()].capture.len() != before;
        }
        if !changed {
            break;
        }
    }
    Ok(domains)
}

fn clocks_on_value(
    module: &word::WordModule,
    value: word::ValueId,
    constraints: &opto_timing::TimingContext,
    bindings: &opto_timing::PortBindings,
) -> BTreeSet<opto_timing::ClockId> {
    let Some(port) = timing_port(module, value, bindings) else {
        return BTreeSet::new();
    };
    constraints
        .clocks()
        .iter()
        .filter(|clock| clock.sources.contains(&port))
        .map(|clock| clock.id)
        .collect()
}

fn boundary_clock_contexts(
    port: crate::RegionBoundaryPort,
    domains: &[RegionClockDomains],
    scenario: &Scenario,
    timing_port: Option<opto_timing::PortId>,
) -> Vec<TagClockContext> {
    let owner = &domains[port.region().index()];
    let peer = port.peer().map(|peer| &domains[peer.index()]);
    let io = timing_port.map_or(&[] as &[_], |timing_port| match port.direction() {
        crate::RegionPortDirection::Input => scenario.constraints().input_delays(timing_port),
        crate::RegionPortDirection::Output => scenario.constraints().output_delays(timing_port),
    });
    let launch = match port.direction() {
        crate::RegionPortDirection::Input => peer.map_or(&owner.launch, |peer| &peer.launch),
        crate::RegionPortDirection::Output => &owner.launch,
    };
    let capture = match port.direction() {
        crate::RegionPortDirection::Input => &owner.capture,
        crate::RegionPortDirection::Output => peer.map_or(&owner.capture, |peer| &peer.capture),
    };
    let launches =
        if peer.is_none() && matches!(port.direction(), crate::RegionPortDirection::Input) {
            io.iter()
                .map(|delay| (delay.clock, delay.clock_edge))
                .collect::<Vec<_>>()
        } else if launch.is_empty() {
            vec![(None, TimingEdge::Rise)]
        } else {
            launch
                .iter()
                .copied()
                .map(|clock| (Some(clock), TimingEdge::Rise))
                .collect()
        };
    let captures =
        if peer.is_none() && matches!(port.direction(), crate::RegionPortDirection::Output) {
            io.iter()
                .map(|delay| (delay.clock, delay.clock_edge))
                .collect::<Vec<_>>()
        } else if capture.is_empty() {
            vec![(None, TimingEdge::Rise)]
        } else {
            capture
                .iter()
                .copied()
                .map(|clock| (Some(clock), TimingEdge::Rise))
                .collect()
        };
    let launches = if launches.is_empty() {
        vec![(None, TimingEdge::Rise)]
    } else {
        launches
    };
    let captures = if captures.is_empty() {
        vec![(None, TimingEdge::Rise)]
    } else {
        captures
    };
    launches
        .into_iter()
        .flat_map(|(launch_clock, launch_edge)| {
            captures
                .iter()
                .copied()
                .map(move |(capture_clock, capture_edge)| TagClockContext {
                    launch_clock,
                    capture_clock,
                    launch_edge,
                    capture_edge,
                })
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn clock_name(
    constraints: &opto_timing::TimingContext,
    clock: Option<opto_timing::ClockId>,
) -> Option<&str> {
    clock.and_then(|clock| {
        constraints
            .clocks()
            .iter()
            .find(|candidate| candidate.id == clock)
            .map(|clock| clock.name.as_str())
    })
}

fn contract_tags(
    interner: &mut TimingTagInterner,
    scenario: &Scenario,
    direction: crate::RegionPortDirection,
    endpoints: &[opto_timing::TimingEndpoint],
    clock_contexts: &[TagClockContext],
    regional_exception_classes: &[u32],
) -> Result<Vec<TimingTagId>, crate::SynthError> {
    let constraints = scenario.constraints();
    let mut exception_classes = BTreeSet::from([0u32]);
    exception_classes.extend(regional_exception_classes.iter().copied());
    for (index, exception) in constraints.path_exceptions().iter().enumerate() {
        let endpoint_filter = match direction {
            crate::RegionPortDirection::Input => &exception.from,
            crate::RegionPortDirection::Output => &exception.to,
        };
        let matches = filter_matches_boundary(endpoint_filter, endpoints)
            || exception
                .through
                .iter()
                .any(|filter| filter_matches_boundary(filter, endpoints));
        if matches {
            exception_classes.insert(u32::try_from(index + 1).map_err(|_| {
                crate::SynthError::capacity("timing exception class exceeds 32-bit capacity")
            })?);
        }
    }
    let mut tags = Vec::new();
    for check in enabled_checks(scenario.checks()) {
        let path_check = matches!(
            check,
            BoundaryCheckKind::Setup
                | BoundaryCheckKind::Hold
                | BoundaryCheckKind::Recovery
                | BoundaryCheckKind::Removal
                | BoundaryCheckKind::PulseWidth
        );
        let contexts = if path_check && !clock_contexts.is_empty() {
            clock_contexts.to_vec()
        } else {
            vec![TagClockContext::default()]
        };
        for context in contexts {
            let launch_name = clock_name(constraints, context.launch_clock);
            let capture_name = clock_name(constraints, context.capture_clock);
            for &exception_class in &exception_classes {
                tags.push(
                    interner
                        .intern(TimingTag {
                            launch_clock: context.launch_clock,
                            capture_clock: context.capture_clock,
                            launch_edge: context.launch_edge,
                            capture_edge: context.capture_edge,
                            check,
                            path_group: Arc::from(format!(
                                "{}:{}->{}:{check:?}",
                                scenario.name(),
                                launch_name.unwrap_or("unclocked"),
                                capture_name.unwrap_or("unclocked")
                            )),
                            exception_class,
                        })
                        .map_err(|error| super::boundary::synthesis_error(&error))?,
                );
            }
        }
    }
    Ok(tags)
}

fn allocate_path_budgets(
    regions: &crate::SynthesisRegionGraph,
    weights: &[Box<[f64]>],
    scenario: usize,
    total_budget: Option<f64>,
) -> Result<Vec<PathBudget>, crate::SynthError> {
    let Some(total_budget) = total_budget else {
        return Ok(vec![(None, None); regions.regions().len()]);
    };
    let mut indegree = regions
        .regions()
        .iter()
        .map(|region| {
            if matches!(
                region.kind(),
                crate::SynthesisRegionKind::State | crate::SynthesisRegionKind::Memory
            ) {
                0
            } else {
                regions.predecessors(*region).len()
            }
        })
        .collect::<Vec<_>>();
    let mut ready = regions
        .regions()
        .iter()
        .filter(|region| indegree[region.row().index()] == 0)
        .map(|region| (region.id(), region.row()))
        .collect::<BTreeSet<_>>();
    let mut arrival = vec![0.0f64; regions.regions().len()];
    let mut delay = vec![0.0f64; regions.regions().len()];
    let mut order = Vec::with_capacity(regions.regions().len());
    while let Some(&(id, row)) = ready.first() {
        ready.remove(&(id, row));
        let weight = weights
            .get(row.index())
            .and_then(|weights| weights.get(scenario))
            .copied()
            .ok_or_else(|| {
                crate::SynthError::invariant(
                    "regional budget weights do not match the region graph",
                )
            })?;
        if !weight.is_finite() || weight < 0.0 {
            return Err(crate::SynthError::invariant(
                "regional absolute delay estimate is invalid",
            ));
        }
        delay[row.index()] = weight;
        let completion = arrival[row.index()] + weight;
        order.push(row);
        let region = regions.region(row).ok_or_else(|| {
            crate::SynthError::invariant("budget row is outside the region graph")
        })?;
        for &successor in regions.successors(region) {
            if regions.region(successor).is_some_and(|region| {
                matches!(
                    region.kind(),
                    crate::SynthesisRegionKind::State | crate::SynthesisRegionKind::Memory
                )
            }) {
                continue;
            }
            arrival[successor.index()] = arrival[successor.index()].max(completion);
            indegree[successor.index()] = indegree[successor.index()].saturating_sub(1);
            if indegree[successor.index()] == 0 {
                let successor_region = regions.region(successor).ok_or_else(|| {
                    crate::SynthError::invariant("region successor row is out of range")
                })?;
                ready.insert((successor_region.id(), successor));
            }
        }
    }
    if order.len() != regions.regions().len() {
        return Err(crate::SynthError::invalid(
            "synthesis-region dependency graph contains a combinational cycle",
        ));
    }
    let mut required = vec![total_budget; regions.regions().len()];
    for &row in order.iter().rev() {
        let region = regions.region(row).ok_or_else(|| {
            crate::SynthError::invariant("regional required-time row is out of range")
        })?;
        if matches!(
            region.kind(),
            crate::SynthesisRegionKind::State | crate::SynthesisRegionKind::Memory
        ) {
            continue;
        }
        if let Some(candidate) = regions
            .successors(region)
            .iter()
            .filter(|&&successor| {
                regions.region(successor).is_some_and(|successor| {
                    !matches!(
                        successor.kind(),
                        crate::SynthesisRegionKind::State | crate::SynthesisRegionKind::Memory
                    )
                })
            })
            .map(|&successor| required[successor.index()] - delay[successor.index()])
            .min_by(f64::total_cmp)
        {
            required[row.index()] = candidate;
        }
    }
    Ok(arrival
        .into_iter()
        .zip(required)
        .map(|(arrival, required)| (Some(arrival), Some(required)))
        .collect())
}

fn design_rule_limit(
    constraints: &opto_timing::TimingContext,
    kind: opto_timing::DesignRuleKind,
    port: Option<opto_timing::PortId>,
) -> Option<f64> {
    constraints
        .design_rule_constraints(kind)
        .into_iter()
        .filter(|constraint| {
            constraint.objects.is_empty()
                || port.is_some_and(|port| {
                    constraint.objects.iter().any(|object| {
                        matches!(object, opto_timing::TimingObject::Port { id, .. } if id == &port)
                    })
                })
        })
        .map(|constraint| constraint.limit)
        .min_by(f64::total_cmp)
}

#[track_caller]
fn finite(value: f64) -> Result<FiniteValue, crate::SynthError> {
    let caller = std::panic::Location::caller();
    FiniteValue::new(value).map_err(|error| {
        crate::SynthError::invariant(format!(
            "regional contract value {value:?} at {}:{} is invalid: {error}",
            caller.file(),
            caller.line()
        ))
    })
}

fn update_present_timing_lanes(
    target: &mut super::TimingCorners<Option<FiniteValue>>,
    source: super::TimingCorners<Option<FiniteValue>>,
) {
    for (target, source) in [
        (&mut target.early.rise, source.early.rise),
        (&mut target.early.fall, source.early.fall),
        (&mut target.late.rise, source.late.rise),
        (&mut target.late.fall, source.late.fall),
    ] {
        if target.is_some() {
            *target = source;
        }
    }
}

fn timing_port(
    module: &word::WordModule,
    value: word::ValueId,
    bindings: &opto_timing::PortBindings,
) -> Option<opto_timing::PortId> {
    let word::ValueKind::Signal(reference) = module.value(value)?.kind else {
        return None;
    };
    let word::SignalKind::Port(port) = module.signal(reference.signal)?.kind else {
        return None;
    };
    bindings.get(port.index())
}

fn boundary_activity(
    module: &word::WordModule,
    value: word::ValueId,
    port: Option<opto_timing::PortId>,
    scenario: &Scenario,
    object_bindings: &opto_timing::TimingObjectBindings,
) -> Option<opto_timing::ScenarioSwitchingActivity> {
    if let Some(port) = port
        && let Some(activity) = scenario
            .power()
            .activity(&opto_timing::ScenarioActivityTarget::Port(port))
    {
        return Some(activity);
    }
    let word::ValueKind::Signal(reference) = module.value(value)?.kind else {
        return None;
    };
    let signal = module.signal(reference.signal)?;
    let name = signal.name.map(|name| module.name_str(name))?;
    let opto_timing::TimingEndpoint::Net(net) = object_bindings.net_endpoint(name)? else {
        return None;
    };
    scenario
        .power()
        .activity(&opto_timing::ScenarioActivityTarget::Net(net))
}

fn enabled_checks(checks: ScenarioCheckSet) -> impl Iterator<Item = BoundaryCheckKind> {
    [
        (checks.setup, BoundaryCheckKind::Setup),
        (checks.hold, BoundaryCheckKind::Hold),
        (checks.recovery, BoundaryCheckKind::Recovery),
        (checks.removal, BoundaryCheckKind::Removal),
        (checks.pulse_width, BoundaryCheckKind::PulseWidth),
        (checks.max_transition, BoundaryCheckKind::MaxTransition),
        (checks.max_capacitance, BoundaryCheckKind::MaxCapacitance),
        (checks.max_fanout, BoundaryCheckKind::MaxFanout),
    ]
    .into_iter()
    .filter_map(|(enabled, check)| enabled.then_some(check))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn internal_word_values_bind_stable_net_cell_and_pin_endpoints() {
        let mut module = word::WordModule::new("top");
        let source = word::SourceSpan::default();
        let signal = module
            .add_wire("mid", word::WordType::bits(1).unwrap(), source.clone())
            .unwrap();
        let value = module.read_signal(signal, source.clone()).unwrap();
        module
            .add_instance(
                "U0",
                "BUF",
                vec![("A".to_string(), value, source.clone())],
                source,
            )
            .unwrap();

        let mut bindings = opto_timing::TimingObjectBindings::builder();
        let uid = |raw| opto_core::ObjectUid::from_raw(raw).unwrap();
        let net = opto_timing::NetId::from_uid(uid(1));
        let cell = opto_timing::CellId::from_uid(uid(2));
        let pin = opto_timing::PinId::from_uid(uid(3));
        bindings.bind_net("mid", net).unwrap();
        bindings.bind_cell("U0", cell).unwrap();
        bindings.bind_pin("U0/A", pin).unwrap();
        let bindings = bindings.finish().unwrap();

        let index = WordTimingEndpointIndex::build(&module, &bindings);
        let endpoints = index.endpoints(value, None);
        assert_eq!(
            endpoints,
            vec![
                opto_timing::TimingEndpoint::Cell(cell),
                opto_timing::TimingEndpoint::Pin(pin),
                opto_timing::TimingEndpoint::Net(net),
            ]
        );
        assert!(filter_matches_boundary(
            &opto_timing::ExceptionFilter::new([opto_timing::TimingEndpoint::Pin(pin)]),
            &endpoints,
        ));
    }

    #[test]
    fn boundary_activity_resolves_persistent_nets_and_prefers_ports() {
        let mut module = word::WordModule::new("top");
        let source = word::SourceSpan::default();
        let signal = module
            .add_wire("mid", word::WordType::bits(1).unwrap(), source.clone())
            .unwrap();
        let value = module.read_signal(signal, source).unwrap();
        let uid = |raw| opto_core::ObjectUid::from_raw(raw).unwrap();
        let net = opto_timing::NetId::from_uid(uid(1));
        let port = opto_timing::PortId::from_uid(uid(2));
        let net_activity = opto_timing::ScenarioSwitchingActivity::new(0.25, 0.1, 0.5).unwrap();
        let port_activity = opto_timing::ScenarioSwitchingActivity::new(0.75, 0.3, 0.5).unwrap();
        let power = opto_timing::ScenarioPowerView::new(
            Arc::new(opto_library::PowerLibrary::default()),
            vec![
                (opto_timing::ScenarioActivityTarget::Net(net), net_activity),
                (
                    opto_timing::ScenarioActivityTarget::Port(port),
                    port_activity,
                ),
            ],
        )
        .unwrap();
        let scenario = opto_timing::Scenario::single(
            Arc::new(opto_timing::TimingContext::default()),
            Arc::new(opto_timing::TimingLibrary::default()),
            opto_timing::Parasitics::default(),
        )
        .with_power(power);
        let mut bindings = opto_timing::TimingObjectBindings::builder();
        bindings.bind_net("mid", net).unwrap();
        let bindings = bindings.finish().unwrap();

        assert_eq!(
            boundary_activity(&module, value, None, &scenario, &bindings),
            Some(net_activity)
        );
        assert_eq!(
            boundary_activity(&module, value, Some(port), &scenario, &bindings),
            Some(port_activity),
            "a bound port is the boundary's authoritative activity source"
        );
    }
}
