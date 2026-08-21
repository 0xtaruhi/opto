// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use self::search::{CoverTiming, LibraryCover};
pub(crate) use self::search::{LibraryCoverBinding, LibraryCoverSource};
use super::roots::MappingRoot;
use super::{CombinationalCellCatalog, word};
use crate::boolean::logic::network::LogicNodeId;
use crate::boolean::logic::{ChoiceGraph, ChoiceScopeId};

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

pub(crate) struct CompiledChoiceMapping(search::CompiledMapping);

pub(crate) fn compile_choice_mapping(
    choices: &ChoiceGraph,
    outputs: &[LogicNodeId],
    catalog: &CombinationalCellCatalog,
    runtime: &opto_runtime::ExecutionContext,
) -> Result<CompiledChoiceMapping, crate::SynthError> {
    search::CompiledMapping::for_choices(choices, outputs, catalog, runtime)
        .map(CompiledChoiceMapping)
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
    subject: &ChoiceGraph,
    compiled: &CompiledChoiceMapping,
    scopes: &[DesignCoverScope<'_>],
    timing: &opto_timing::TimingContext,
    port_bindings: &opto_timing::PortBindings,
    mapping: &super::TargetMappingContext,
    runtime: &opto_runtime::ExecutionContext,
) -> Result<Vec<RegionCoverAnalysis>, crate::SynthError> {
    let catalog = &mapping.combinational_catalog;
    let mut outputs = Vec::new();
    let mut output_ranges = Vec::with_capacity(scopes.len());
    let mut required_times = Vec::new();
    let mut output_loads = Vec::new();
    let mut input_transitions = Vec::new();
    let mut input_arrivals = Vec::new();
    for scope in scopes {
        let start = outputs.len();
        let analyzed = analyzed_outputs(scope.roots, subject, scope.scope)?
            .map_or_else(Vec::new, <[AnalyzedRegionOutput]>::into_vec);
        let roots = merged_output_roots(scope.roots, &analyzed)?;
        required_times.extend(roots.iter().map(|root| root.required_time));
        output_loads.extend(roots.iter().map(|root| root.output_load));
        outputs.extend(analyzed);
        output_ranges.push(start..outputs.len());
        for &value in subject.inputs(scope.scope) {
            input_transitions.push(scope.regional_slice.search_input_transition(value).or_else(
                || {
                    let word::ValueKind::Signal(reference) = scope.module.value(value)?.kind else {
                        return None;
                    };
                    let word::SignalKind::Port(port) = scope.module.signal(reference.signal)?.kind
                    else {
                        return None;
                    };
                    port_bindings
                        .get(port.index())
                        .and_then(|port| timing.input_transition_on(port))
                },
            ));
            input_arrivals.push(scope.regional_slice.search_input_arrival(value));
        }
    }
    if outputs.is_empty() {
        return Ok(scopes
            .iter()
            .map(|_| RegionCoverAnalysis::NoCombinationalLogic)
            .collect());
    }
    let nodes = outputs.iter().map(|output| output.node).collect::<Vec<_>>();
    let _profile =
        crate::api::diagnostics::ProfileSpan::new(mapping.config.diagnostics.timing, || {
            "cover.design_wide_selection".to_string()
        });
    let cover = search::cover_choice_graph(
        subject,
        &compiled.0,
        &nodes,
        catalog,
        CoverTiming {
            required_times: &required_times,
            output_loads: &output_loads,
            input_transitions: &input_transitions,
            input_arrivals: &input_arrivals,
        },
        runtime,
    )?
    .ok_or_else(|| crate::SynthError::mapping("design-wide Boolean graph cannot be covered"))?;
    let covers = split_design_cover(cover, subject, scopes, &output_ranges, catalog)?;
    Ok(covers
        .into_iter()
        .zip(output_ranges)
        .enumerate()
        .map(|(scope, (cover, range))| match cover {
            Some(cover) => RegionCoverAnalysis::Covered(Box::new(AnalyzedRegionCover {
                inputs: subject.inputs(scopes[scope].scope).into(),
                outputs: outputs[range].to_vec().into_boxed_slice(),
                cover,
            })),
            None => RegionCoverAnalysis::NoCombinationalLogic,
        })
        .collect())
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
    subject: &ChoiceGraph,
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

fn split_design_cover(
    cover: LibraryCover,
    subject: &ChoiceGraph,
    scopes: &[DesignCoverScope<'_>],
    output_ranges: &[std::ops::Range<usize>],
    catalog: &CombinationalCellCatalog,
) -> Result<Vec<Option<LibraryCover>>, crate::SynthError> {
    let mut cell_scopes = vec![None; cover.cells.len()];
    for (scope, range) in output_ranges.iter().enumerate() {
        let mut pending = cover.outputs[range.clone()].to_vec();
        while let Some(source) = pending.pop() {
            let cell = match source {
                LibraryCoverSource::Cell(cell) | LibraryCoverSource::CellSecond(cell) => cell,
                LibraryCoverSource::Constant(_) | LibraryCoverSource::Input(_) => continue,
            };
            let slot = cell_scopes.get_mut(cell).ok_or_else(|| {
                crate::SynthError::invariant("design cover references an unknown selected cell")
            })?;
            match slot {
                Some(current) if *current != scope => {
                    return Err(crate::SynthError::invariant(
                        "one selected cover cell spans distinct choice scopes",
                    ));
                }
                Some(_) => continue,
                None => *slot = Some(scope),
            }
            pending.extend(cover.cells[cell].sources.iter().copied());
        }
    }
    let mut cells = (0..scopes.len()).map(|_| Vec::new()).collect::<Vec<_>>();
    let mut areas = vec![0.0; scopes.len()];
    let mut remap = vec![None; cover.cells.len()];
    for (old, mut cell) in cover.cells.into_vec().into_iter().enumerate() {
        let Some(scope) = cell_scopes[old] else {
            continue;
        };
        for source in &mut cell.sources {
            rebase_cover_source(
                source,
                scope,
                subject.input_range(scopes[scope].scope),
                &remap,
            )?;
        }
        let local = cells[scope].len();
        areas[scope] += match cell.binding {
            LibraryCoverBinding::Single(binding) => catalog.cost_for_binding(binding).area,
            LibraryCoverBinding::Joint(binding) => catalog.joint_cost(binding).area,
        };
        remap[old] = Some((scope, local));
        cells[scope].push(cell);
    }
    let mut results = Vec::with_capacity(scopes.len());
    for (scope, range) in output_ranges.iter().enumerate() {
        if range.is_empty() {
            results.push(None);
            continue;
        }
        let input_range = subject.input_range(scopes[scope].scope);
        let mut outputs = cover.outputs[range.clone()].to_vec();
        for source in &mut outputs {
            rebase_cover_source(source, scope, input_range.clone(), &remap)?;
        }
        let mut local = LibraryCover {
            cells: std::mem::take(&mut cells[scope]).into_boxed_slice(),
            outputs: outputs.into_boxed_slice(),
            total_area: areas[scope],
            output_costs: cover.output_costs[range.clone()].into(),
        };
        local.isolate_outputs(catalog)?;
        results.push(Some(local));
    }
    Ok(results)
}

fn rebase_cover_source(
    source: &mut LibraryCoverSource,
    scope: usize,
    inputs: std::ops::Range<usize>,
    cells: &[Option<(usize, usize)>],
) -> Result<(), crate::SynthError> {
    let (cell, second) = match *source {
        LibraryCoverSource::Input(input) => {
            if !inputs.contains(&input) {
                return Err(crate::SynthError::invariant(
                    "selected cover references an input from another choice scope",
                ));
            }
            *source = LibraryCoverSource::Input(input - inputs.start);
            return Ok(());
        }
        LibraryCoverSource::Cell(cell) => (cell, false),
        LibraryCoverSource::CellSecond(cell) => (cell, true),
        LibraryCoverSource::Constant(_) => return Ok(()),
    };
    let (cell_scope, local) = cells.get(cell).copied().flatten().ok_or_else(|| {
        crate::SynthError::invariant("selected cover references an unavailable local cell")
    })?;
    if cell_scope != scope {
        return Err(crate::SynthError::invariant(
            "selected cover crosses a choice-scope boundary",
        ));
    }
    *source = if second {
        LibraryCoverSource::CellSecond(local)
    } else {
        LibraryCoverSource::Cell(local)
    };
    Ok(())
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
