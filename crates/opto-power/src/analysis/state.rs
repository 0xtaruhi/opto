// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::activity::{function_variables, propagated_activity};
use super::evaluation::{
    CellCalculationContext, calculate_cell_power, index, pin_net, switching_power,
};
use super::{
    ActivityAnnotations, ActivityOrigin, PowerAnalysis, PowerAnalysisData, PowerLibraryReference,
    PowerSummary, SwitchingActivity,
};
use crate::PowerError;
use opto_library::TargetPinDirection;
use opto_runtime::{
    DependencyActivation, DependencyDirection, DependencyExecution, DependencyPlan,
    DependencyPublication, DependencyPublicationPlan, DependencyRun, DependencyWorklist,
    ExecutionContext,
};
use opto_timing::{
    LibraryCellId, TimingElectricalSnapshot, TimingInstanceRef, TimingModel, TimingNetId,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

const NONE: u32 = u32::MAX;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct NetActivity {
    pub(super) value: SwitchingActivity,
    pub(super) origin: ActivityOrigin,
}

impl Default for NetActivity {
    fn default() -> Self {
        Self {
            value: SwitchingActivity::quiescent(),
            origin: ActivityOrigin::Default,
        }
    }
}

#[derive(Debug)]
pub(crate) struct PowerAnalysisState {
    pub(crate) analysis: PowerAnalysis,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PowerUpdateCounts {
    pub(crate) nets: usize,
    pub(crate) cells: usize,
}

#[derive(Debug)]
pub(super) struct PowerTopology {
    fanout_instances: opto_core::PackedRows<u32>,
    connected_instances: opto_core::PackedRows<u32>,
    instance_inputs: opto_core::PackedRows<TimingNetId>,
    instance_outputs: opto_core::PackedRows<TimingNetId>,
    drivers: Box<[u32]>,
    target_cells: Box<[LibraryCellId]>,
    power_cells: Box<[u32]>,
    pub(super) sequential: Box<[bool]>,
    pub(super) report_nets: Box<[TimingNetId]>,
    propagation_plan: DependencyPlan,
    publication_plan: DependencyPublicationPlan,
}

fn dense_rows<T: Ord>(
    row_count: usize,
    mut entries: Vec<(usize, T)>,
) -> Result<opto_core::PackedRows<T>, PowerError> {
    entries.sort_unstable();
    entries.dedup();
    opto_core::PackedRows::try_from_entries(row_count, entries).map_err(|_| PowerError::Capacity {
        resource: "power row index",
    })
}

impl PowerAnalysisState {
    pub(super) fn analyze(
        runtime: &ExecutionContext,
        model: &TimingModel,
        electrical: &TimingElectricalSnapshot,
        annotations: &ActivityAnnotations,
    ) -> Result<Self, PowerError> {
        validate_inputs(model, electrical, annotations)?;
        let topology = Arc::new(PowerTopology::new(model)?);
        let mut activities = vec![NetActivity::default(); model.net_count()];
        propagate_initial(runtime, model, &topology, annotations, &mut activities)?;

        let units = model.library().power.units;
        let time_unit_seconds = units.time_seconds.ok_or(PowerError::MissingLibraryUnit {
            attribute: "time_unit",
        })?;
        let capacitance_unit_farads =
            units
                .capacitance_farads
                .ok_or(PowerError::MissingLibraryUnit {
                    attribute: "capacitive_load_unit",
                })?;
        let voltage_unit = units.voltage_volts.ok_or(PowerError::MissingLibraryUnit {
            attribute: "voltage_unit",
        })?;
        let nominal_voltage = units
            .nominal_voltage
            .ok_or(PowerError::MissingLibraryUnit {
                attribute: "nom_voltage",
            })?;
        let leakage_power_unit_watts =
            units
                .leakage_power_watts
                .ok_or(PowerError::MissingLibraryUnit {
                    attribute: "leakage_power_unit",
                })?;
        let voltage = voltage_unit * nominal_voltage;
        let dynamic_power_unit_watts =
            capacitance_unit_farads * voltage * voltage / time_unit_seconds;
        let net_switching_watts = model
            .net_ids()
            .map(|net| {
                switching_power(
                    electrical
                        .get(net)
                        .expect("validated electrical snapshot covers every timing net")
                        .capacitance,
                    activities[index(net)].value,
                    capacitance_unit_farads,
                    voltage,
                    time_unit_seconds,
                )
            })
            .collect::<Vec<_>>();
        let calculation = CellCalculationContext {
            activities: &activities,
            electrical,
            net_switching_watts: &net_switching_watts,
            dynamic_power_unit_watts,
            leakage_power_unit_watts,
        };
        let cells = model
            .instances()
            .enumerate()
            .map(|(row, instance)| calculate_cell(model, &topology, row, instance, &calculation))
            .collect::<Result<Vec<_>, _>>()?;
        let summary = PowerSummary {
            internal_watts: cells.iter().map(|cell| cell.internal).sum(),
            switching_watts: cells.iter().map(|cell| cell.switching).sum(),
            leakage_watts: cells.iter().map(|cell| cell.leakage).sum(),
        };
        let analysis = PowerAnalysis {
            data: Arc::new(PowerAnalysisData {
                generation: model.generation(),
                design: model.design().name().to_string(),
                libraries: model
                    .library()
                    .name
                    .iter()
                    .map(|name| PowerLibraryReference {
                        name: name.clone(),
                        source: None,
                    })
                    .collect::<Vec<_>>()
                    .into(),
                operating_conditions: model.library().operating_conditions.clone(),
                wire_load_mode: model.library().wire_load_mode.clone(),
                voltage,
                voltage_unit_volts: voltage_unit,
                time_unit_seconds,
                capacitance_unit_farads,
                dynamic_power_unit_watts,
                leakage_power_unit_watts,
                activities: activities.into(),
                net_switching_watts: net_switching_watts.into(),
                cells: cells.into(),
                topology,
                electrical: electrical.clone(),
                summary,
            }),
        };
        Ok(Self { analysis })
    }

    pub(crate) fn update_activities(
        &mut self,
        runtime: &ExecutionContext,
        model: &TimingModel,
        electrical: &TimingElectricalSnapshot,
        previous: &ActivityAnnotations,
        annotations: &ActivityAnnotations,
    ) -> Result<PowerUpdateCounts, PowerError> {
        self.update_activities_with_hook(runtime, model, electrical, previous, annotations, |_| {})
    }

    #[allow(
        clippy::too_many_lines,
        reason = "incremental activity update atomically reconciles affected cones, net activity, \
                  cell power, summary deltas, and the immutable report snapshot"
    )]
    fn update_activities_with_hook(
        &mut self,
        runtime: &ExecutionContext,
        model: &TimingModel,
        electrical: &TimingElectricalSnapshot,
        previous: &ActivityAnnotations,
        annotations: &ActivityAnnotations,
        worker_hook: impl Fn(usize) + Send + Sync,
    ) -> Result<PowerUpdateCounts, PowerError> {
        validate_inputs(model, electrical, annotations)?;
        if previous.generation() != annotations.generation()
            || self.analysis.generation() != annotations.generation()
            || !self.analysis.data.electrical.is_same_snapshot(electrical)
        {
            return Err(PowerError::GenerationMismatch);
        }
        let changed = previous
            .keys()
            .chain(annotations.keys())
            .filter(|&net| previous.get(net) != annotations.get(net))
            .collect::<BTreeSet<_>>();
        if changed.is_empty() {
            return Ok(PowerUpdateCounts::default());
        }

        // All mutations target detached compact columns. Publishing the new
        // Arc happens only after every fallible propagation and cell
        // evaluation succeeds, preserving the previous cached snapshot on
        // every error path.
        let current = &self.analysis.data;
        let topology = &current.topology;
        let mut activities = clone_column(&current.activities, "power activity column")?;
        let mut net_switching_watts =
            clone_column(&current.net_switching_watts, "power net switching column")?;
        let mut cells = clone_column(&current.cells, "power cell value column")?;
        let mut summary = current.summary;
        let mut affected = vec![false; model.net_count()];
        let mut affected_nets = Vec::new();
        let mut seeds = BTreeSet::new();
        for net in changed {
            let row = index(net);
            let next = annotations.get(net).map_or_else(
                || unannotated_activity(model, topology, &activities, net),
                |value| NetActivity {
                    value,
                    origin: ActivityOrigin::Annotated,
                },
            );
            if annotations.get(net).is_none()
                && topology
                    .driver(net)
                    .is_some_and(|driver| !topology.sequential.get(driver).copied().unwrap_or(true))
            {
                seeds.insert(topology.driver(net).expect("tested typed driver"));
            } else if activities[row] != next {
                activities[row] = next;
                mark_affected(net, &mut affected, &mut affected_nets);
                seed_fanout(topology, net, &mut seeds);
            }
        }

        let scheduled = propagation_closure(topology, seeds.iter().copied(), annotations);
        let worklist = topology.propagation_worklist(scheduled, annotations)?;
        let execution = propagate_instances(
            runtime,
            model,
            topology,
            &mut activities,
            PropagationSchedule {
                worklist,
                annotations,
                activation: DependencyActivation::on_change(
                    model.instance_count(),
                    seeds.iter().copied(),
                )?,
            },
            worker_hook,
        )?;
        for &row in execution.changed_rows() {
            let net = model
                .net_ids()
                .nth(row)
                .ok_or(PowerError::InvalidTimingNetState {
                    net: u32::try_from(row).unwrap_or(u32::MAX),
                })?;
            mark_affected(net, &mut affected, &mut affected_nets);
        }
        affected_nets.sort_unstable();

        let mut affected_cells = vec![false; model.instance_count()];
        for &net in &affected_nets {
            let row = index(net);
            let activity = activities[row];
            net_switching_watts[row] = switching_power(
                electrical
                    .get(net)
                    .expect("validated electrical snapshot covers changed power net")
                    .capacitance,
                activity.value,
                current.capacitance_unit_farads,
                current.voltage,
                current.time_unit_seconds,
            );
            for &instance in topology.connected_instances.row(row) {
                affected_cells[instance as usize] = true;
            }
        }

        let calculation = CellCalculationContext {
            activities: &activities,
            electrical,
            net_switching_watts: &net_switching_watts,
            dynamic_power_unit_watts: current.dynamic_power_unit_watts,
            leakage_power_unit_watts: current.leakage_power_unit_watts,
        };
        let mut cell_count = 0;
        for (row, &is_affected) in affected_cells.iter().enumerate() {
            if !is_affected {
                continue;
            }
            let instance = model
                .instance_at(row)
                .expect("power topology instance rows match the timing model");
            let next = calculate_cell(model, topology, row, instance, &calculation)?;
            let old = cells[row];
            summary.internal_watts += next.internal - old.internal;
            summary.switching_watts += next.switching - old.switching;
            summary.leakage_watts += next.leakage - old.leakage;
            cells[row] = next;
            cell_count += 1;
        }
        let mut next = (**current).clone();
        next.activities = activities.into();
        next.net_switching_watts = net_switching_watts.into();
        next.cells = cells.into();
        next.summary = summary;
        self.analysis = PowerAnalysis {
            data: Arc::new(next),
        };
        Ok(PowerUpdateCounts {
            nets: affected_nets.len(),
            cells: cell_count,
        })
    }
}

fn clone_column<T: Clone>(source: &[T], resource: &'static str) -> Result<Vec<T>, PowerError> {
    let mut clone = Vec::new();
    clone
        .try_reserve_exact(source.len())
        .map_err(|_| PowerError::Capacity { resource })?;
    clone.extend_from_slice(source);
    Ok(clone)
}

fn unannotated_activity(
    model: &TimingModel,
    topology: &PowerTopology,
    activities: &[NetActivity],
    net: TimingNetId,
) -> NetActivity {
    if let Some(value) = model.constant_net_value(net) {
        return NetActivity {
            value: SwitchingActivity::constant(value),
            origin: ActivityOrigin::Default,
        };
    }
    let Some(driver) = topology.driver(net) else {
        return NetActivity::default();
    };
    if topology.sequential[driver] {
        return NetActivity::default();
    }
    activities[index(net)]
}

impl PowerTopology {
    #[allow(
        clippy::too_many_lines,
        reason = "topology construction validates typed bindings and publishes all dense reverse \
                  indices, driver rows, library links, and dependency plans together"
    )]
    fn new(model: &TimingModel) -> Result<Self, PowerError> {
        let net_count = model.net_count();
        let instance_count = model.instance_count();
        let mut fanout = Vec::new();
        let mut connected = Vec::new();
        let mut inputs = Vec::new();
        let mut outputs = Vec::new();
        let mut drivers = vec![NONE; net_count];
        let mut target_cells = Vec::with_capacity(instance_count);
        let mut power_cells = Vec::with_capacity(instance_count);
        let mut sequential = Vec::with_capacity(instance_count);
        let power_by_name = model
            .library()
            .power
            .cells
            .iter()
            .enumerate()
            .map(|(index, cell)| (cell.name.as_str(), index))
            .collect::<BTreeMap<_, _>>();

        for (row, instance) in model.instances().enumerate() {
            let encoded_row = u32::try_from(row).map_err(|_| PowerError::Capacity {
                resource: "power instance index",
            })?;
            let target_id = model
                .instance_library_cell_id(instance.id())
                .ok_or_else(|| PowerError::MissingTargetCell {
                    cell: instance.cell().to_string(),
                })?;
            let target = model
                .library_cell(target_id)
                .expect("timing model library-cell IDs remain valid");
            target_cells.push(target_id);
            power_cells.push(
                power_by_name
                    .get(instance.cell())
                    .copied()
                    .map(|index| {
                        u32::try_from(index).map_err(|_| PowerError::Capacity {
                            resource: "power-cell index",
                        })
                    })
                    .transpose()?
                    .unwrap_or(NONE),
            );
            let is_sequential = target.sequential().next().is_some();
            sequential.push(is_sequential);
            for connection in instance.connections() {
                let net = connection.net;
                let net_row = index(net);
                connected.push((net_row, encoded_row));
                let Some(pin) = target.pins().find(|pin| pin.name() == connection.pin) else {
                    continue;
                };
                if matches!(
                    pin.direction(),
                    TargetPinDirection::Output | TargetPinDirection::Inout
                ) {
                    if drivers[net_row] != NONE {
                        return Err(PowerError::MultipleNetDrivers { net: net.raw() });
                    }
                    drivers[net_row] = encoded_row;
                    outputs.push((row, net));
                }
            }
            if !is_sequential {
                for function in target
                    .pins()
                    .filter(|pin| {
                        matches!(
                            pin.direction(),
                            TargetPinDirection::Output | TargetPinDirection::Inout
                        )
                    })
                    .filter_map(opto_library::TargetPinRef::function)
                {
                    for variable in function_variables(&function) {
                        let net = pin_net(instance, &variable).ok_or_else(|| {
                            PowerError::UnknownFunctionPin {
                                pin: variable.clone(),
                            }
                        })?;
                        fanout.push((index(net), encoded_row));
                        inputs.push((row, net));
                    }
                }
            }
        }
        let fanout_instances = dense_rows(net_count, fanout)?;
        let connected_instances = dense_rows(net_count, connected)?;
        let instance_inputs = dense_rows(instance_count, inputs)?;
        let publication_plan = DependencyPublicationPlan::sparse(
            instance_count,
            net_count,
            outputs.iter().map(|&(row, net)| (row, index(net))),
        )?;
        let instance_outputs = dense_rows(instance_count, outputs)?;
        let propagation_plan = build_propagation_plan(&instance_inputs, &drivers, &sequential)?;
        let mut report_nets = Vec::new();
        report_nets
            .try_reserve_exact(net_count)
            .map_err(|_| PowerError::Capacity {
                resource: "power report net index",
            })?;
        for net in model.net_ids() {
            if model.constant_net_value(net).is_none() {
                report_nets.push(net);
            }
        }
        Ok(Self {
            fanout_instances,
            connected_instances,
            instance_inputs,
            instance_outputs,
            drivers: drivers.into_boxed_slice(),
            target_cells: target_cells.into_boxed_slice(),
            power_cells: power_cells.into_boxed_slice(),
            sequential: sequential.into_boxed_slice(),
            report_nets: report_nets.into_boxed_slice(),
            propagation_plan,
            publication_plan,
        })
    }

    fn driver(&self, net: TimingNetId) -> Option<usize> {
        self.drivers
            .get(index(net))
            .copied()
            .filter(|&row| row != NONE)
            .map(|row| row as usize)
    }

    fn propagation_worklist(
        &self,
        seeds: impl IntoIterator<Item = usize>,
        annotations: &ActivityAnnotations,
    ) -> Result<DependencyWorklist<'_>, PowerError> {
        let mut candidates = BTreeSet::new();
        for net in annotations.keys() {
            let Some(driver) = self.driver(net) else {
                continue;
            };
            if self.sequential[driver] {
                continue;
            }
            candidates.extend(
                self.fanout_instances
                    .row(index(net))
                    .iter()
                    .map(|&row| (row as usize, driver)),
            );
        }
        let disabled = candidates.into_iter().filter(|&(row, driver)| {
            self.instance_inputs
                .row(row)
                .iter()
                .filter(|&&net| self.driver(net) == Some(driver))
                .all(|&net| annotations.get(net).is_some())
        });
        Ok(self
            .propagation_plan
            .worklist_masked(DependencyDirection::Forward, seeds, disabled)?)
    }
}

fn build_propagation_plan(
    inputs: &opto_core::PackedRows<TimingNetId>,
    drivers: &[u32],
    sequential: &[bool],
) -> Result<DependencyPlan, PowerError> {
    let mut edges = Vec::new();
    for (row, &is_sequential) in sequential.iter().enumerate() {
        if is_sequential {
            continue;
        }
        for &net in inputs.row(row) {
            let Some(&driver) = drivers.get(index(net)) else {
                continue;
            };
            let driver_row = driver as usize;
            if driver_row != NONE as usize && !sequential[driver_row] {
                edges.push((row, driver));
            }
        }
    }
    let predecessors = dense_rows(sequential.len(), edges)?;
    let successors = dense_rows(
        sequential.len(),
        (0..sequential.len())
            .flat_map(|row| {
                predecessors
                    .row(row)
                    .iter()
                    .map(move |&dependency| (dependency as usize, compact_row(row)))
            })
            .collect(),
    )?;
    let mut unresolved = (0..sequential.len())
        .map(|row| predecessors.row(row).len())
        .collect::<Vec<_>>();
    let mut ready = unresolved
        .iter()
        .enumerate()
        .filter_map(|(row, &count)| (count == 0).then_some(row))
        .collect::<BTreeSet<_>>();
    let mut order = Vec::with_capacity(sequential.len());
    while let Some(row) = ready.pop_first() {
        order.push(row);
        for &successor in successors.row(row) {
            let pending = &mut unresolved[successor as usize];
            *pending -= 1;
            if *pending == 0 {
                ready.insert(successor as usize);
            }
        }
    }
    if order.len() != sequential.len() {
        return Err(PowerError::ActivityPropagationCycle);
    }
    Ok(DependencyPlan::from_topological_order(
        sequential.len(),
        &order,
        |row| {
            predecessors
                .row(row)
                .iter()
                .map(|&dependency| dependency as usize)
        },
    )?)
}

fn propagate_initial(
    runtime: &ExecutionContext,
    model: &TimingModel,
    topology: &PowerTopology,
    annotations: &ActivityAnnotations,
    activities: &mut [NetActivity],
) -> Result<(), PowerError> {
    propagate_initial_with_hook(runtime, model, topology, annotations, activities, |_| {})
}

fn propagate_initial_with_hook(
    runtime: &ExecutionContext,
    model: &TimingModel,
    topology: &PowerTopology,
    annotations: &ActivityAnnotations,
    activities: &mut [NetActivity],
    worker_hook: impl Fn(usize) + Send + Sync,
) -> Result<(), PowerError> {
    for (raw, net) in model.net_ids().enumerate() {
        if let Some(value) = model.constant_net_value(net) {
            activities[raw].value = SwitchingActivity::constant(value);
        }
    }
    for (net, value) in annotations.iter() {
        activities[index(net)] = NetActivity {
            value,
            origin: ActivityOrigin::Annotated,
        };
    }
    let worklist = topology.propagation_worklist(
        topology
            .sequential
            .iter()
            .enumerate()
            .filter_map(|(row, &sequential)| (!sequential).then_some(row)),
        annotations,
    )?;
    propagate_instances(
        runtime,
        model,
        topology,
        activities,
        PropagationSchedule {
            worklist,
            annotations,
            activation: DependencyActivation::all(),
        },
        worker_hook,
    )
    .map(|_| ())
}

#[derive(Debug)]
struct PreparedInstance {
    row: usize,
    inputs: Box<[NetActivity]>,
}

struct PropagationSchedule<'a> {
    worklist: DependencyWorklist<'a>,
    annotations: &'a ActivityAnnotations,
    activation: DependencyActivation,
}

fn propagate_instances(
    runtime: &ExecutionContext,
    model: &TimingModel,
    topology: &PowerTopology,
    activities: &mut [NetActivity],
    schedule: PropagationSchedule<'_>,
    worker_hook: impl Fn(usize) + Send + Sync,
) -> Result<DependencyExecution, PowerError> {
    let PropagationSchedule {
        worklist,
        annotations,
        activation,
    } = schedule;
    runtime.publish_dependency_rows(
        worklist,
        activities,
        DependencyRun::new(&topology.publication_plan, activation),
        |activities, row| {
            let instance = model
                .instance_at(row)
                .expect("power topology instance rows match the timing model");
            Ok(PreparedInstance {
                row,
                inputs: instance
                    .nets()
                    .iter()
                    .map(|&net| activities[index(net)])
                    .collect(),
            })
        },
        |input| {
            let row = input.row;
            let outputs = instance_outputs(model, topology, annotations, &input)?;
            worker_hook(row);
            Ok(outputs)
        },
    )
}

fn instance_outputs(
    model: &TimingModel,
    topology: &PowerTopology,
    annotations: &ActivityAnnotations,
    prepared: &PreparedInstance,
) -> Result<DependencyPublication<NetActivity>, PowerError> {
    let row = prepared.row;
    if topology.sequential[row] {
        return Ok(DependencyPublication::none());
    }
    let instance = model
        .instance_at(row)
        .expect("power topology instance rows match the timing model");
    let target = model
        .library_cell(topology.target_cells[row])
        .expect("power topology owns valid library-cell IDs");
    let input = |pin: &str| {
        pin_net(instance, pin).and_then(|net| {
            instance
                .nets()
                .iter()
                .position(|&candidate| candidate == net)
                .and_then(|position| prepared.inputs.get(position).map(|input| input.value))
        })
    };
    let current = |net: TimingNetId| {
        instance
            .nets()
            .iter()
            .position(|&candidate| candidate == net)
            .and_then(|position| prepared.inputs.get(position).copied())
            .expect("prepared power instance snapshots every connected net")
    };
    let mut outputs = target
        .pins()
        .filter(|pin| {
            matches!(
                pin.direction(),
                TargetPinDirection::Output | TargetPinDirection::Inout
            )
        })
        .filter_map(|pin| pin_net(instance, pin.name()).map(|net| (pin, net)))
        .map(|(pin, net)| {
            if annotations.get(net).is_some() {
                return Ok((index(net), current(net)));
            }
            pin.function().map_or_else(
                || {
                    Ok((
                        index(net),
                        NetActivity {
                            value: SwitchingActivity::quiescent(),
                            origin: ActivityOrigin::Default,
                        },
                    ))
                },
                |function| {
                    propagated_activity(&function, input).map(|value| {
                        (
                            index(net),
                            NetActivity {
                                value,
                                origin: ActivityOrigin::Propagated,
                            },
                        )
                    })
                },
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    outputs.sort_unstable_by_key(|(row, _)| *row);
    debug_assert!(
        outputs
            .iter()
            .all(|&(net, _)| topology.drivers[net] as usize == row)
    );
    Ok(DependencyPublication::rows(outputs))
}

fn compact_row(row: usize) -> u32 {
    u32::try_from(row).expect("power topology capacity checks keep every row representable as u32")
}

fn calculate_cell(
    model: &TimingModel,
    topology: &PowerTopology,
    row: usize,
    instance: TimingInstanceRef<'_>,
    calculation: &CellCalculationContext<'_>,
) -> Result<super::CellPowerValue, PowerError> {
    let target = model
        .library_cell(topology.target_cells[row])
        .expect("power topology owns valid library-cell IDs");
    let power = topology.power_cells[row]
        .ne(&NONE)
        .then(|| {
            model
                .library()
                .power
                .cells
                .get(topology.power_cells[row] as usize)
        })
        .flatten();
    calculate_cell_power(instance, target, power, calculation)
}

fn validate_inputs(
    model: &TimingModel,
    electrical: &TimingElectricalSnapshot,
    annotations: &ActivityAnnotations,
) -> Result<(), PowerError> {
    if model.generation() != electrical.generation()
        || model.generation() != annotations.generation()
    {
        return Err(PowerError::GenerationMismatch);
    }
    if electrical.len() != model.net_count() {
        return Err(PowerError::InvalidTimingNetState {
            net: u32::try_from(electrical.len()).unwrap_or(u32::MAX),
        });
    }
    if let Some(net) = annotations
        .keys()
        .find(|net| index(*net) >= model.net_count())
    {
        return Err(PowerError::InvalidTimingNetState { net: net.raw() });
    }
    Ok(())
}

fn seed_fanout(topology: &PowerTopology, net: TimingNetId, seeds: &mut BTreeSet<usize>) {
    for &row in topology.fanout_instances.row(index(net)) {
        let row = row as usize;
        if !topology.sequential[row] {
            seeds.insert(row);
        }
    }
}

fn propagation_closure(
    topology: &PowerTopology,
    seeds: impl IntoIterator<Item = usize>,
    annotations: &ActivityAnnotations,
) -> Vec<usize> {
    let mut included = vec![false; topology.sequential.len()];
    let mut pending = VecDeque::new();
    let mut scheduled = Vec::new();
    for row in seeds {
        if !std::mem::replace(&mut included[row], true) {
            pending.push_back(row);
            scheduled.push(row);
        }
    }
    while let Some(row) = pending.pop_front() {
        for &net in topology.instance_outputs.row(row) {
            if annotations.get(net).is_some() {
                continue;
            }
            for &fanout in topology.fanout_instances.row(index(net)) {
                let fanout = fanout as usize;
                if !topology.sequential[fanout] && !std::mem::replace(&mut included[fanout], true) {
                    pending.push_back(fanout);
                    scheduled.push(fanout);
                }
            }
        }
    }
    scheduled
}

fn mark_affected(net: TimingNetId, affected: &mut [bool], rows: &mut Vec<TimingNetId>) {
    if !std::mem::replace(&mut affected[index(net)], true) {
        rows.push(net);
    }
}

#[cfg(test)]
mod tests;
