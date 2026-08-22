// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use self::search::{CoverTiming, LibraryCover};
pub(crate) use self::search::{LibraryCoverBinding, LibraryCoverSource};
use super::roots::MappingRoot;
use super::{CombinationalCellCatalog, word};
use crate::boolean::logic::network::LogicNodeId;
use crate::boolean::logic::{ChoiceDesign, ChoiceScopeId};
use opto_runtime::{Task, TaskKey};

mod portable;
mod response;
mod search;

pub(crate) use portable::{decode as decode_portable_cover, empty_plan_key};
pub(crate) use response::CoverResponseModels;

pub(crate) struct AnalyzedRegionCover {
    inputs: Box<[word::ValueId]>,
    outputs: Box<[AnalyzedRegionOutput]>,
    cover: LibraryCover,
}

pub(crate) enum RegionCoverAnalysis {
    NoCombinationalLogic,
    Covered(Box<AnalyzedRegionCover>),
}

#[derive(Clone)]
pub(crate) struct AnalyzedRegionOutput {
    node: LogicNodeId,
    values: Box<[word::ValueId]>,
}

/// Borrowed closure domain used to evaluate and seal one regional cover.
#[derive(Clone, Copy)]
pub(crate) struct CoverClosureDomain<'a, 'scenario> {
    pub(crate) contracts: &'a [crate::BoundaryContract],
    pub(crate) catalog: &'a CombinationalCellCatalog,
    pub(crate) response_models: &'a CoverResponseModels<'scenario>,
    pub(crate) timing_tags: &'a crate::TimingTagInterner,
    pub(crate) regional_slice: &'a super::logic_partition::RegionLogicSlice,
}

impl AnalyzedRegionCover {
    pub(crate) fn candidate_binding(
        &mut self,
        domain: crate::mapping::CandidateBindingDomain<'_>,
        catalog: &CombinationalCellCatalog,
    ) -> Result<crate::mapping::CandidateBinding, crate::SynthError> {
        let candidate = crate::mapping::build_candidate_binding(
            domain,
            &self.inputs,
            self.outputs.iter().map(|output| output.values.as_ref()),
        )?;
        if candidate.output_widths.len() != self.outputs.len()
            || self.cover.outputs.len() != self.outputs.len()
            || self.cover.output_costs.len() != self.outputs.len()
        {
            return Err(crate::SynthError::invariant(
                "regional owner bindings and cover metadata do not align with cover outputs",
            ));
        }
        let mut outputs = Vec::with_capacity(candidate.binding.outputs.len());
        let mut cover_outputs = Vec::with_capacity(candidate.binding.outputs.len());
        let mut output_costs = Vec::with_capacity(candidate.binding.outputs.len());
        for (((output, &source), &cost), &width) in self
            .outputs
            .iter()
            .zip(self.cover.outputs.iter())
            .zip(self.cover.output_costs.iter())
            .zip(candidate.output_widths.iter())
        {
            outputs.extend(std::iter::repeat_n(output.clone(), width));
            cover_outputs.extend(std::iter::repeat_n(source, width));
            output_costs.extend(std::iter::repeat_n(cost, width));
        }
        self.outputs = outputs.into_boxed_slice();
        self.cover.outputs = cover_outputs.into_boxed_slice();
        self.cover.output_costs = output_costs.into_boxed_slice();
        self.cover.isolate_outputs(catalog)?;
        Ok(candidate)
    }

    pub(crate) fn compact_plan(
        &self,
        region: crate::SynthesisRegion,
        context: crate::RegionContextKey,
        decision_key: [u8; 32],
        closure: CoverClosureDomain<'_, '_>,
    ) -> Result<crate::RegionCoverPlan, crate::SynthError> {
        let CoverClosureDomain {
            contracts: boundary_response,
            catalog,
            response_models,
            timing_tags,
            regional_slice,
        } = closure;
        let mut payload = b"ORCP\x03".to_vec();
        payload.extend_from_slice(&(self.cover.cells.len() as u64).to_le_bytes());
        for (index, cell) in self.cover.cells.iter().enumerate() {
            let local = u32::try_from(index)
                .map_err(|_| crate::SynthError::capacity("regional cover cell index"))?;
            payload.extend_from_slice(&local.to_le_bytes());
            payload.extend_from_slice(&(cell.binding_identity.len() as u64).to_le_bytes());
            payload.extend_from_slice(&cell.binding_identity);
            payload.extend_from_slice(&cell.truth.bits.to_le_bytes());
            payload.push(u8::try_from(cell.truth.input_count).map_err(|_| {
                crate::SynthError::capacity("regional truth-table input count exceeds 8 bits")
            })?);
            payload.push(match cell.binding {
                LibraryCoverBinding::Single(_) => 0,
                LibraryCoverBinding::Joint(_) => 1,
            });
            match cell.second_truth {
                Some(truth) => {
                    payload.push(1);
                    payload.extend_from_slice(&truth.bits.to_le_bytes());
                    payload.push(u8::try_from(truth.input_count).map_err(|_| {
                        crate::SynthError::capacity(
                            "regional secondary truth-table input count exceeds 8 bits",
                        )
                    })?);
                }
                None => payload.push(0),
            }
            payload.extend_from_slice(&(cell.sources.len() as u64).to_le_bytes());
            for source in &cell.sources {
                let (kind, index) = match source {
                    LibraryCoverSource::Constant(value) => (u8::from(*value), 0usize),
                    LibraryCoverSource::Input(index) => (2, *index),
                    LibraryCoverSource::Cell(index) => (3, *index),
                    LibraryCoverSource::CellSecond(index) => (4, *index),
                };
                payload.push(kind);
                payload.extend_from_slice(&(index as u64).to_le_bytes());
            }
        }
        payload.extend_from_slice(&(self.cover.outputs.len() as u64).to_le_bytes());
        for source in &self.cover.outputs {
            let (kind, index) = match source {
                LibraryCoverSource::Constant(value) => (u8::from(*value), 0usize),
                LibraryCoverSource::Input(index) => (2, *index),
                LibraryCoverSource::Cell(index) => (3, *index),
                LibraryCoverSource::CellSecond(index) => (4, *index),
            };
            payload.push(kind);
            payload.extend_from_slice(&(index as u64).to_le_bytes());
        }
        let local_cell_count = u32::try_from(self.cover.cells.len())
            .map_err(|_| crate::SynthError::capacity("regional cover cell count overflow"))?;
        let local_net_count = self.cover.cells.iter().try_fold(0u32, |count, cell| {
            count
                .checked_add(if cell.second_node.is_some() { 2 } else { 1 })
                .ok_or_else(|| crate::SynthError::capacity("regional cover net count overflow"))
        })?;
        let mut local_pin_count = 0usize;
        let mut implementation_cells = Vec::with_capacity(self.cover.cells.len());
        for cell in &self.cover.cells {
            let (cell_name, connections) = match cell.binding {
                LibraryCoverBinding::Single(binding) => (
                    catalog.binding_cell_name(binding),
                    catalog.binding_connection_count(binding),
                ),
                LibraryCoverBinding::Joint(binding) => (
                    catalog.joint_binding_cell_name(binding),
                    catalog.joint_binding_connection_count(binding),
                ),
            };
            local_pin_count = local_pin_count
                .checked_add(connections)
                .ok_or_else(|| crate::SynthError::capacity("regional cover pin count overflow"))?;
            implementation_cells.push(crate::regional::RegionImplementationCell {
                cell_name: cell_name.into(),
                pin_count: u32::try_from(connections)
                    .map_err(|_| crate::SynthError::capacity("regional target-cell pin count"))?,
            });
        }
        implementation_cells.sort();
        let local_pin_count = u32::try_from(local_pin_count)
            .map_err(|_| crate::SynthError::capacity("regional cover pin count overflow"))?;
        let stable_plan_key =
            portable::stable_plan_key(region.id(), decision_key, &payload, &implementation_cells);
        if self.outputs.len() != self.cover.outputs.len() {
            return Err(crate::SynthError::invariant(
                "regional cover output costs do not align with its roots",
            ));
        }
        let finite = |value| {
            crate::FiniteValue::new(value)
                .map_err(|error| crate::SynthError::invariant(error.to_string()))
        };
        let area = finite(self.cover.total_area)?;
        let leakage_power = response_models
            .regional_leakage(&self.cover, catalog)
            .map(finite)
            .transpose()?;
        let measured_response = response::measure(
            &self.inputs,
            &self.outputs,
            &self.cover,
            boundary_response,
            regional_slice,
            response_models,
        )?;
        let boundary_score = response::score_boundaries(
            boundary_response,
            &measured_response.boundaries,
            timing_tags,
        )?;
        Ok(crate::RegionCoverPlan::new(
            region,
            context,
            crate::RegionPlanCost {
                legal: true,
                worst_normalized_violation: finite(boundary_score.worst_normalized_violation)?,
                minimum_slack: finite(boundary_score.minimum_slack)?,
                total_negative_slack: finite(boundary_score.total_negative_slack)?,
                area,
                leakage_power,
                dynamic_power: measured_response.dynamic_power.map(finite).transpose()?,
                cell_count: local_cell_count,
                stable_plan_key,
            },
            local_net_count,
            local_pin_count,
            boundary_response.to_vec(),
            payload,
        )
        .with_measured_response(measured_response.boundaries)
        .with_implementation_cells(implementation_cells))
    }
}

#[derive(Clone, Copy)]
pub(crate) struct DesignCoverScope<'a> {
    pub(crate) module: &'a word::WordModule,
    pub(crate) roots: &'a [MappingRoot],
    pub(crate) regional_slice: &'a super::logic_partition::RegionLogicSlice,
    pub(crate) scope: ChoiceScopeId,
}

pub(crate) fn analyze_design_cover(
    subject: &ChoiceDesign,
    scopes: &[DesignCoverScope<'_>],
    timing: &opto_timing::TimingContext,
    port_bindings: &opto_timing::PortBindings,
    mapping: &super::TargetMappingContext,
    runtime: &opto_runtime::ExecutionContext,
) -> Result<Vec<RegionCoverAnalysis>, crate::SynthError> {
    const TASK_DOMAIN: u32 = 0x434f_5652;
    if subject.scope_count() != scopes.len() {
        return Err(crate::SynthError::invariant(
            "choice scopes do not align with regional cover scopes",
        ));
    }
    let catalog = &mapping.combinational_catalog;
    let tasks = scopes
        .iter()
        .enumerate()
        .map(|(row, scope)| {
            let outputs = analyzed_outputs(scope.roots, subject, scope.scope)?
                .map_or_else(Vec::new, <[AnalyzedRegionOutput]>::into_vec);
            let roots = merged_output_roots(scope.roots, &outputs)?;
            let required_times = roots
                .iter()
                .map(|root| root.required_time)
                .collect::<Vec<_>>();
            let output_loads = roots
                .iter()
                .map(|root| root.output_load)
                .collect::<Vec<_>>();
            let mut input_transitions = Vec::new();
            let mut input_arrivals = Vec::new();
            for &value in subject.inputs(scope.scope) {
                input_transitions.push(
                    scope
                        .regional_slice
                        .search_input_transition(value)
                        .or_else(|| {
                            let word::ValueKind::Signal(reference) =
                                scope.module.value(value)?.kind
                            else {
                                return None;
                            };
                            let word::SignalKind::Port(port) =
                                scope.module.signal(reference.signal)?.kind
                            else {
                                return None;
                            };
                            port_bindings
                                .get(port.index())
                                .and_then(|port| timing.input_transition_on(port))
                        }),
                );
                input_arrivals.push(scope.regional_slice.search_input_arrival(value));
            }
            let graph = subject.graph(scope.scope);
            Ok(Task::new(
                TaskKey::new(TASK_DOMAIN, row as u64),
                (
                    scope.scope,
                    outputs,
                    required_times,
                    output_loads,
                    input_transitions,
                    input_arrivals,
                ),
            )
            .with_estimated_work(graph.network().node_count().max(1) as u64)
            .with_estimated_memory(graph.network().node_count().max(1) as u64))
        })
        .collect::<Result<Vec<_>, crate::SynthError>>()?;
    runtime.map_ordered_composite(tasks, |task, regional_runtime| {
        let (scope, outputs, required_times, output_loads, input_transitions, input_arrivals) =
            task;
        if outputs.is_empty() {
            return Ok(RegionCoverAnalysis::NoCombinationalLogic);
        }
        let graph = subject.graph(scope);
        let nodes = outputs.iter().map(|output| output.node).collect::<Vec<_>>();
        let compiled =
            search::CompiledMapping::for_choices(graph, &nodes, catalog, regional_runtime)?;
        let cover = search::cover_choice_graph(
            graph,
            &compiled,
            &nodes,
            catalog,
            CoverTiming {
                required_times: &required_times,
                output_loads: &output_loads,
                input_transitions: &input_transitions,
                input_arrivals: &input_arrivals,
            },
            regional_runtime,
        )?
        .ok_or_else(|| crate::SynthError::mapping("regional Boolean graph cannot be covered"))?;
        Ok(RegionCoverAnalysis::Covered(Box::new(
            AnalyzedRegionCover {
                inputs: subject.inputs(scope).into(),
                outputs: outputs.into_boxed_slice(),
                cover,
            },
        )))
    })
}

fn merged_output_roots(
    roots: &[MappingRoot],
    outputs: &[AnalyzedRegionOutput],
) -> Result<Vec<MappingRoot>, crate::SynthError> {
    let mut by_value = std::collections::BTreeMap::new();
    for &root in roots {
        by_value
            .entry(root.value)
            .and_modify(|current| merge_root_constraints(current, root))
            .or_insert(root);
    }
    outputs
        .iter()
        .map(|output| {
            let mut values = output.values.iter();
            let first = values.next().ok_or_else(|| {
                crate::SynthError::invariant("retained design cover output has no Word values")
            })?;
            let mut merged = *by_value.get(first).ok_or_else(|| {
                crate::SynthError::invariant("design cover output is absent from mapping roots")
            })?;
            for value in values {
                merge_root_constraints(
                    &mut merged,
                    *by_value.get(value).ok_or_else(|| {
                        crate::SynthError::invariant(
                            "design cover output is absent from mapping roots",
                        )
                    })?,
                );
            }
            Ok(merged)
        })
        .collect()
}

fn analyzed_outputs(
    roots: &[MappingRoot],
    subject: &ChoiceDesign,
    scope: ChoiceScopeId,
) -> Result<Option<Box<[AnalyzedRegionOutput]>>, crate::SynthError> {
    let mut outputs = Vec::new();
    for &root in roots {
        if !root.requires_combinational_cover {
            continue;
        }
        let Some(node) = subject.node(scope, root.value) else {
            return Err(crate::SynthError::invariant(format!(
                "combinational regional root {:?} has no Boolean subject node",
                root.value
            )));
        };
        if subject.is_dont_care(scope, root.value)
            && node != crate::boolean::logic::network::LogicGraph::constant(false)
        {
            return Err(crate::SynthError::invariant(
                "care-free regional root has no deterministic publication constant",
            ));
        }
        outputs.push(AnalyzedRegionOutput {
            node,
            values: Box::new([root.value]),
        });
    }
    if outputs.is_empty() {
        return Ok(None);
    }
    Ok(Some(outputs.into_boxed_slice()))
}

fn merge_root_constraints(merged: &mut MappingRoot, root: MappingRoot) {
    merged.required_time = match (merged.required_time, root.required_time) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    };
    merged.output_load = match (merged.output_load, root.output_load) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    };
}
