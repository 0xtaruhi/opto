// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use self::search::{CoverTiming, LibraryCover};
pub(crate) use self::search::{LibraryCoverBinding, LibraryCoverSource};
use super::roots::MappingRoot;
use super::{CombinationalCellCatalog, word};
use crate::boolean::logic::cuts::{CutDatabase, CutTruthDatabase};
use crate::boolean::logic::network::LogicNodeId;
use crate::boolean::logic::{MAX_MATCH_INPUTS, RegionLogicGraph};

mod portable;
mod response;
mod search;

use crate::mapping::RegionPlanBinding;
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

type SelectedSubjectCover = (Box<[AnalyzedRegionOutput]>, LibraryCover);

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
    ) -> Result<RegionPlanBinding, crate::SynthError> {
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
        Ok(candidate.binding)
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
pub(crate) struct RegionCoverRequest<'a> {
    pub(crate) roots: &'a [MappingRoot],
    pub(crate) timing: &'a opto_timing::TimingContext,
    pub(crate) port_bindings: &'a opto_timing::PortBindings,
    pub(crate) catalog: &'a super::library::CombinationalCellCatalog,
    pub(crate) options: crate::boolean::logic::RegionLogicOptions<'a>,
    pub(crate) regional_slice: &'a super::logic_partition::RegionLogicSlice,
}

pub(crate) fn analyze_region_cover(
    module: &word::WordModule,
    request: RegionCoverRequest<'_>,
    canonical: crate::boolean::logic::CanonicalRegionLogic,
) -> Result<RegionCoverAnalysis, crate::SynthError> {
    if request.roots.is_empty() {
        return Ok(RegionCoverAnalysis::NoCombinationalLogic);
    }
    let root_values = request
        .roots
        .iter()
        .map(|root| root.value)
        .collect::<Vec<_>>();
    let root_requirements = request
        .roots
        .iter()
        .map(|root| root.required_time)
        .collect::<Vec<_>>();
    let profiling = request.options.config.diagnostics.timing;
    let subject = {
        let _profile = crate::api::diagnostics::ProfileSpan::new(profiling, || {
            "cover.subject_optimization".to_string()
        });
        RegionLogicGraph::from_canonical(
            canonical,
            &root_values,
            &root_requirements,
            request.options,
        )?
    };
    let inputs = subject.inputs().to_vec().into_boxed_slice();
    let cuts = {
        let _profile = crate::api::diagnostics::ProfileSpan::new(profiling, || {
            "cover.cut_enumeration".to_string()
        });
        CutDatabase::build_parallel(subject.network(), MAX_MATCH_INPUTS, request.options.runtime)?
    };
    let truths = {
        let _profile = crate::api::diagnostics::ProfileSpan::new(profiling, || {
            "cover.truth_evaluation".to_string()
        });
        CutTruthDatabase::build_parallel(subject.network(), &cuts, request.options.runtime)?
    };
    let Some((outputs, cover)) = ({
        let _profile = crate::api::diagnostics::ProfileSpan::new(profiling, || {
            "cover.library_selection".to_string()
        });
        select_subject_cover(&subject, &cuts, &truths, &inputs, module, &request)?
    }) else {
        return Ok(RegionCoverAnalysis::NoCombinationalLogic);
    };
    Ok(RegionCoverAnalysis::Covered(Box::new(
        AnalyzedRegionCover {
            inputs,
            outputs,
            cover,
        },
    )))
}

fn select_subject_cover(
    subject: &RegionLogicGraph,
    cuts: &CutDatabase,
    truths: &CutTruthDatabase,
    inputs: &[word::ValueId],
    module: &word::WordModule,
    request: &RegionCoverRequest<'_>,
) -> Result<Option<SelectedSubjectCover>, crate::SynthError> {
    let selector = CoverSelector {
        subject,
        cuts,
        truths,
        inputs,
        module,
        request,
    };
    let Some(outputs) = analyzed_outputs(request.roots, subject)? else {
        return Ok(None);
    };
    let cover = selector
        .select(&outputs, request.options.runtime)?
        .ok_or_else(|| crate::SynthError::mapping("regional Boolean network cannot be covered"))?;
    crate::api::diagnostics::trace!(
        crate::api::diagnostics::SynthTrace::timing(request.options.config.diagnostics),
        "cover.logic",
        "area={:.3}",
        cover.total_area
    );
    Ok(Some((outputs, cover)))
}

fn analyzed_outputs(
    roots: &[MappingRoot],
    subject: &RegionLogicGraph,
) -> Result<Option<Box<[AnalyzedRegionOutput]>>, crate::SynthError> {
    let mut outputs = Vec::new();
    for &root in roots {
        if !root.requires_combinational_cover {
            continue;
        }
        let Some(node) = subject.node(root.value) else {
            return Err(crate::SynthError::invariant(format!(
                "combinational regional root {:?} has no Boolean subject node",
                root.value
            )));
        };
        if subject.is_dont_care(root.value)
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

struct CoverSelector<'a, 'request> {
    subject: &'a RegionLogicGraph,
    cuts: &'a CutDatabase,
    truths: &'a CutTruthDatabase,
    inputs: &'a [word::ValueId],
    module: &'a word::WordModule,
    request: &'a RegionCoverRequest<'request>,
}

impl CoverSelector<'_, '_> {
    fn select(
        &self,
        outputs: &[AnalyzedRegionOutput],
        runtime: &opto_runtime::ExecutionContext,
    ) -> Result<Option<LibraryCover>, crate::SynthError> {
        let Self {
            subject,
            cuts,
            truths,
            inputs,
            module,
            request,
        } = self;
        let mut roots_by_value = std::collections::BTreeMap::new();
        for &root in request.roots {
            roots_by_value
                .entry(root.value)
                .and_modify(|current| merge_root_constraints(current, root))
                .or_insert(root);
        }
        let output_roots = outputs
            .iter()
            .map(|output| {
                let mut values = output.values.iter();
                let first = values.next().ok_or_else(|| {
                    crate::SynthError::invariant(
                        "retained regional cover output has no Word values",
                    )
                })?;
                let mut merged = roots_by_value.get(first).copied().ok_or_else(|| {
                    crate::SynthError::invariant(
                        "retained regional cover output is absent from the active mapping roots",
                    )
                })?;
                for value in values {
                    let root = roots_by_value.get(value).copied().ok_or_else(|| {
                    crate::SynthError::invariant(
                        "retained regional cover output is absent from the active mapping roots",
                    )
                })?;
                    merge_root_constraints(&mut merged, root);
                }
                Ok::<MappingRoot, crate::SynthError>(merged)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let required_times = output_roots
            .iter()
            .map(|root| root.required_time)
            .collect::<Vec<_>>();
        let output_loads = output_roots
            .iter()
            .map(|root| root.output_load)
            .collect::<Vec<_>>();
        let input_transitions = inputs
            .iter()
            .map(|&value| {
                if let Some(transition) = request.regional_slice.search_input_transition(value) {
                    return Some(transition);
                }
                let word::ValueKind::Signal(reference) = module.value(value)?.kind else {
                    return None;
                };
                let word::SignalKind::Port(port) = module.signal(reference.signal)?.kind else {
                    return None;
                };
                request
                    .port_bindings
                    .get(port.index())
                    .and_then(|port| request.timing.input_transition_on(port))
            })
            .collect::<Vec<_>>();
        let input_arrivals = inputs
            .iter()
            .map(|&value| request.regional_slice.search_input_arrival(value))
            .collect::<Vec<_>>();
        let nodes = outputs.iter().map(|output| output.node).collect::<Vec<_>>();
        let timing = CoverTiming {
            required_times: &required_times,
            output_loads: &output_loads,
            input_transitions: &input_transitions,
            input_arrivals: &input_arrivals,
        };
        let mut cover = search::cover_logic_network_with_truths(
            subject.network(),
            cuts,
            truths,
            &nodes,
            request.catalog,
            timing,
            runtime,
        )?;
        if let Some(cover) = &mut cover {
            cover.isolate_outputs(request.catalog).map_err(|error| {
                crate::SynthError::mapping(format!(
                    "{error}; {} frozen regional output obligations",
                    outputs.len()
                ))
            })?;
        }
        Ok(cover)
    }
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
