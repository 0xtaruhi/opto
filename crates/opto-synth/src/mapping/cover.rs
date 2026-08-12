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

#[derive(Clone, Copy)]
pub(crate) struct CompactPlanInputs<'a, 'scenario> {
    pub(crate) region: crate::SynthesisRegion,
    pub(crate) context: crate::RegionContextKey,
    pub(crate) boundary_response: &'a [crate::BoundaryContract],
    pub(crate) decision_key: [u8; 32],
    pub(crate) catalog: &'a CombinationalCellCatalog,
    pub(crate) response_models: &'a CoverResponseModels<'scenario>,
    pub(crate) timing_tags: &'a crate::TimingTagInterner,
    pub(crate) regional_slice: &'a super::logic_partition::RegionLogicSlice,
}

impl AnalyzedRegionCover {
    pub(crate) fn candidate_binding(
        &mut self,
        inputs: crate::mapping::CandidateBindingInputs<'_>,
        catalog: &CombinationalCellCatalog,
    ) -> Result<RegionPlanBinding, crate::SynthError> {
        let candidate = crate::mapping::build_candidate_binding(
            inputs,
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
        inputs: CompactPlanInputs<'_, '_>,
    ) -> Result<crate::RegionCoverPlan, crate::SynthError> {
        let CompactPlanInputs {
            region,
            context,
            boundary_response,
            decision_key,
            catalog,
            response_models,
            timing_tags,
            regional_slice,
        } = inputs;
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
            crate::RegionPlanIdentity {
                region: region.id(),
                revision: region.revision(),
                context_key: context,
            },
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
            crate::RegionPlanSize {
                local_net_count,
                local_cell_count,
                local_pin_count,
            },
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
    let subject =
        RegionLogicGraph::new_cached(module, &root_values, &root_requirements, request.options)?;
    let inputs = subject.inputs().to_vec().into_boxed_slice();
    let cuts =
        CutDatabase::build_parallel(subject.network(), MAX_MATCH_INPUTS, request.options.runtime)?;
    let truths =
        CutTruthDatabase::build_parallel(subject.network(), &cuts, request.options.runtime)?;
    let Some((outputs, cover)) =
        select_subject_cover(&subject, &cuts, &truths, &inputs, module, &request)?
    else {
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
    let mut implementations = subject.implementations().iter();
    let timing_driven = request
        .roots
        .iter()
        .any(|root| root.required_time.is_some_and(f64::is_finite));
    let portfolio_search = if timing_driven {
        CoverSearch::Estimate
    } else {
        CoverSearch::Exact
    };
    let baseline = implementations.next().ok_or_else(|| {
        crate::SynthError::invariant("regional Boolean subject has no baseline implementation")
    })?;
    let Some(mut outputs) = analyzed_outputs(request.roots, |value| baseline.node(value))? else {
        return Ok(None);
    };
    let mut selected = selector
        .select(&outputs, portfolio_search)?
        .ok_or_else(|| crate::SynthError::mapping("regional Boolean network cannot be covered"))?;
    let mut selected_pass = baseline.pass();
    for implementation in implementations {
        let candidate_outputs =
            analyzed_outputs(request.roots, |value| implementation.node(value))?.ok_or_else(
                || crate::SynthError::invariant("AXM alternative has no analyzed outputs"),
            )?;
        let Some(candidate) = selector.select(&candidate_outputs, portfolio_search)? else {
            crate::api::diagnostics::trace!(
                crate::api::diagnostics::SynthTrace::timing(request.options.config.diagnostics),
                "cover.logic_alternative",
                "pass={} coverable=false selected=false",
                implementation.pass()
            );
            continue;
        };
        let preferred = prefer_cover_rank(candidate.rank, selected.rank, timing_driven);
        crate::api::diagnostics::trace!(
            crate::api::diagnostics::SynthTrace::timing(request.options.config.diagnostics),
            "cover.logic_alternative",
            "pass={} area={:.3} incumbent={:.3} selected={preferred}",
            implementation.pass(),
            candidate.cover.total_area,
            selected.cover.total_area
        );
        if preferred {
            outputs = candidate_outputs;
            selected = candidate;
            selected_pass = implementation.pass();
        }
    }
    let portfolio_area = selected.cover.total_area;
    let selected = if portfolio_search == CoverSearch::Exact {
        selected
    } else {
        selector
            .select(&outputs, CoverSearch::Exact)?
            .ok_or_else(|| {
                crate::SynthError::mapping("selected AXM implementation cannot be covered")
            })?
    };
    crate::api::diagnostics::trace!(
        crate::api::diagnostics::SynthTrace::timing(request.options.config.diagnostics),
        "cover.logic_portfolio",
        "pass={selected_pass} portfolio={portfolio_area:.3} exact={:.3}",
        selected.cover.total_area
    );
    Ok(Some((outputs, selected.cover)))
}

fn analyzed_outputs(
    roots: &[MappingRoot],
    mut node: impl FnMut(word::ValueId) -> Option<LogicNodeId>,
) -> Result<Option<Box<[AnalyzedRegionOutput]>>, crate::SynthError> {
    let mut outputs = Vec::new();
    for &root in roots {
        if !root.requires_combinational_cover {
            continue;
        }
        let Some(node) = node(root.value) else {
            return Err(crate::SynthError::invariant(format!(
                "combinational regional root {:?} has no Boolean subject node",
                root.value
            )));
        };
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
        search_kind: CoverSearch,
    ) -> Result<Option<RankedCover>, crate::SynthError> {
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
        let cover = match search_kind {
            CoverSearch::Estimate => search::estimate_logic_network_with_truths(
                subject.network(),
                cuts,
                truths,
                &nodes,
                request.catalog,
                timing,
                request.options.runtime,
            )?,
            CoverSearch::Exact => search::cover_logic_network_with_truths(
                subject.network(),
                cuts,
                truths,
                &nodes,
                request.catalog,
                timing,
                request.options.runtime,
            )?,
        };
        let mut cover = cover;
        if let Some(cover) = &mut cover {
            cover.isolate_outputs(request.catalog).map_err(|error| {
                crate::SynthError::mapping(format!(
                    "{error}; {} frozen regional output obligations",
                    outputs.len()
                ))
            })?;
        }
        Ok(cover.map(|cover| RankedCover {
            rank: cover_rank(&cover, &required_times),
            cover,
        }))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CoverSearch {
    Estimate,
    Exact,
}

struct RankedCover {
    cover: LibraryCover,
    rank: CoverRank,
}

fn prefer_cover_rank(candidate: CoverRank, current: CoverRank, timing_driven: bool) -> bool {
    if timing_driven {
        candidate < current
    } else {
        candidate
            .area
            .total_cmp(&current.area)
            .then_with(|| candidate.delay.total_cmp(&current.delay))
            .then_with(|| candidate.cells.cmp(&current.cells))
            .is_lt()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct CoverRank {
    violating: bool,
    worst_violation: f64,
    total_violation: f64,
    area: f64,
    delay: f64,
    cells: usize,
}

impl Eq for CoverRank {}

impl PartialOrd for CoverRank {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CoverRank {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.violating
            .cmp(&other.violating)
            .then_with(|| self.worst_violation.total_cmp(&other.worst_violation))
            .then_with(|| self.total_violation.total_cmp(&other.total_violation))
            .then_with(|| (self.area * self.delay).total_cmp(&(other.area * other.delay)))
            .then_with(|| self.area.total_cmp(&other.area))
            .then_with(|| self.delay.total_cmp(&other.delay))
            .then_with(|| self.cells.cmp(&other.cells))
    }
}

fn cover_rank(cover: &LibraryCover, requirements: &[Option<f64>]) -> CoverRank {
    let mut worst_violation = 0.0f64;
    let mut total_violation = 0.0f64;
    let mut delay = 0.0f64;
    for (cost, required) in cover.output_costs.iter().zip(requirements) {
        delay = delay.max(cost.electrical_delay);
        if let Some(required) = required.filter(|required| required.is_finite()) {
            let violation = (cost.electrical_delay - required).max(0.0);
            worst_violation = worst_violation.max(violation);
            total_violation += violation;
        }
    }
    CoverRank {
        violating: worst_violation > 0.0,
        worst_violation,
        total_violation,
        area: cover.total_area,
        delay,
        cells: cover.cells.len(),
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

#[cfg(test)]
mod tests {
    use super::{CoverRank, prefer_cover_rank};

    fn rank(area: f64, delay: f64) -> CoverRank {
        CoverRank {
            violating: false,
            worst_violation: 0.0,
            total_violation: 0.0,
            area,
            delay,
            cells: 1,
        }
    }

    #[test]
    fn feasible_alternative_minimizes_area_delay_product() {
        assert!(!prefer_cover_rank(rank(99.8, 1.1), rank(100.0, 1.0), true));
        assert!(prefer_cover_rank(rank(99.8, 0.9), rank(100.0, 1.0), true));
        assert!(prefer_cover_rank(rank(100.0, 0.9), rank(100.0, 1.0), true));
        assert!(prefer_cover_rank(rank(110.0, 0.9), rank(100.0, 1.0), true));
    }

    #[test]
    fn constraint_feasibility_precedes_area_delay_product() {
        let mut violating = rank(1.0, 1.0);
        violating.violating = true;
        violating.worst_violation = 0.1;
        violating.total_violation = 0.1;

        assert!(!prefer_cover_rank(violating, rank(100.0, 100.0), true));
        assert!(prefer_cover_rank(rank(100.0, 100.0), violating, true));
    }

    #[test]
    fn unconstrained_alternative_minimizes_area_before_delay() {
        assert!(prefer_cover_rank(rank(99.8, 10.0), rank(100.0, 1.0), false));
        assert!(!prefer_cover_rank(
            rank(100.1, 0.1),
            rank(100.0, 1.0),
            false
        ));
    }
}
