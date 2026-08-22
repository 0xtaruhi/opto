// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Direct materialization of fixed regional cells into the mapped substrate.
//!
//! Sequential cells are immutable state-region substrate objects: regional
//! epochs replace only combinational covers. This module therefore prepares one
//! deterministic delta from the lowered Word model without first emitting
//! target instances back into that model. Enable polarity is normalized in
//! the private Word model, so any inverter is covered by the ordinary Boolean
//! mapper rather than introduced by this publication layer.

use super::region_delta::{MappedValueSignal, WordMappedSignals};
use super::{ArtifactCell, ArtifactNetTable, target_pin_id, validate_artifact_nets};
use crate::artifact::MappedCellSource;
use crate::mapping::sequential::SelectedRegisterCell;
use opto_ir::mapped::{AppliedRegionDelta, CellId, NetId, RegionDelta, TempCellId};
use opto_ir::word;
use std::collections::BTreeSet;
use std::fmt::Write as _;

/// Immutable topology for state cells and generated target cells in one lowered design.
///
/// Existing nets are explicit and sorted.
#[derive(Debug, Clone)]
pub(crate) struct MappedFixedSubstrateArtifact {
    nets: ArtifactNetTable,
    cells: Box<[ArtifactCell<MappedCellSource>]>,
    referenced_nets: Box<[NetId]>,
}

/// Delta-local sequential identities retained until mapped/timing commit.
#[derive(Debug)]
pub(crate) struct PendingMappedFixedSubstrate {
    cells: Box<[(TempCellId, MappedCellSource)]>,
}

/// Stable source state selected before scalar bit lowering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SequentialStateSource {
    Operation {
        cell: opto_ir::design::CellId,
        value: word::ValueId,
    },
    Memory {
        cell: opto_ir::design::CellId,
        memory: word::MemoryId,
        ordinal: u32,
        width: u32,
    },
}

impl SequentialStateSource {
    const fn cell(self) -> opto_ir::design::CellId {
        match self {
            Self::Operation { cell, .. } | Self::Memory { cell, .. } => cell,
        }
    }

    const fn source_value(self) -> Option<word::ValueId> {
        match self {
            Self::Operation { value, .. } => Some(value),
            Self::Memory { .. } => None,
        }
    }

    fn semantic_bit(self, bit: u32) -> Result<u32, crate::SynthError> {
        match self {
            Self::Operation { .. } => Ok(bit),
            Self::Memory { ordinal, width, .. } => ordinal
                .checked_mul(width)
                .and_then(|base| base.checked_add(bit))
                .ok_or_else(|| crate::SynthError::capacity("memory state bit index")),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SourceSequentialBinding {
    operation: word::OpId,
    states: Box<[SequentialStateSource]>,
    region: crate::RegionAnchorId,
    state_relation: Option<[u8; 32]>,
}

/// One source-visible state bit implemented by a lowered state operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SequentialSourceBit {
    source: SequentialStateSource,
    pub(crate) bit: u32,
    pub(crate) state_relation: Option<[u8; 32]>,
}

impl SequentialSourceBit {
    pub(crate) const fn cell(&self) -> opto_ir::design::CellId {
        self.source.cell()
    }

    pub(crate) fn semantic_bit(&self) -> Result<u32, crate::SynthError> {
        self.source.semantic_bit(self.bit)
    }

    pub(crate) const fn source_value(&self) -> Option<word::ValueId> {
        self.source.source_value()
    }

    pub(crate) const fn semantic_binding(&self) -> crate::mapping::RegionPlanValueBinding {
        match self.source {
            SequentialStateSource::Operation { value, .. } => {
                crate::mapping::RegionPlanValueBinding::SourceBit {
                    value,
                    bit: self.bit,
                }
            }
            SequentialStateSource::Memory {
                memory, ordinal, ..
            } => crate::mapping::RegionPlanValueBinding::MemoryStateBit {
                memory,
                ordinal,
                bit: self.bit,
            },
        }
    }
}

/// Exact semantic state relation for one live scalar state operation.
#[derive(Debug, Clone)]
pub(crate) struct SequentialRegionBinding {
    pub(crate) operation: word::OpId,
    pub(crate) region: crate::RegionAnchorId,
    pub(crate) sources: Box<[SequentialSourceBit]>,
    pub(crate) lowering_sources: Box<[word::OpId]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RegionalSequentialCellPlan {
    pub(crate) sources: Box<[SequentialSourceBit]>,
    pub(crate) region: crate::RegionAnchorId,
    pub(crate) instance_name: Box<str>,
    pub(crate) cell_name: Box<str>,
    pub(crate) inputs: Box<[(Box<str>, crate::mapping::RegionalEndpoint)]>,
    pub(crate) output: (Box<str>, crate::mapping::RegionalEndpoint),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RegionalSubstrateConnection {
    pub(crate) pin: Box<str>,
    pub(crate) output: bool,
    pub(crate) endpoint: crate::mapping::RegionalEndpoint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RegionalSubstrateCellPlan {
    pub(crate) source: word::ValueId,
    pub(crate) region: crate::RegionAnchorId,
    pub(crate) instance_name: Box<str>,
    pub(crate) cell_name: Box<str>,
    pub(crate) connections: Box<[RegionalSubstrateConnection]>,
}

pub(crate) struct SequentialPublicationPlan {
    aliases: Box<[(word::ValueId, word::ValueId)]>,
}

impl SequentialPublicationPlan {
    pub(crate) fn aliases(&self) -> &[(word::ValueId, word::ValueId)] {
        &self.aliases
    }
}

impl PendingMappedFixedSubstrate {
    pub(crate) fn resolve(
        self,
        applied: &AppliedRegionDelta,
    ) -> Result<Box<[(CellId, MappedCellSource)]>, crate::SynthError> {
        self.cells
            .iter()
            .map(|&(cell, source)| {
                applied
                    .added_cell(cell)
                    .map(|cell| (cell, source))
                    .ok_or_else(|| {
                        crate::SynthError::invariant(
                            "applied substrate delta lost a sequential cell",
                        )
                    })
            })
            .collect()
    }
}

/// Projects the frozen region-graph membership onto sequential operations
/// before bit lowering changes the representation of state boundaries.
pub(crate) fn sequential_region_bindings(
    module: &word::WordModule,
    regions: &crate::SynthesisRegionGraph,
) -> Result<Box<[SourceSequentialBinding]>, crate::SynthError> {
    let mut operations = Vec::new();
    for &region in regions.regions() {
        for &operation in regions.operations(region) {
            let stored = module.operation(operation).ok_or_else(|| {
                crate::SynthError::invariant(
                    "sequential region binding references an unknown operation",
                )
            })?;
            if matches!(
                stored.kind,
                word::OpKind::Register(_) | word::OpKind::Latch(_)
            ) {
                operations.push(SourceSequentialBinding {
                    operation,
                    states: Box::new([SequentialStateSource::Operation {
                        cell: crate::regional::logical_operation_cell_id(regions, operation)?,
                        value: stored.result,
                    }]),
                    region: region.id(),
                    state_relation: None,
                });
            }
        }
    }
    Ok(operations.into_boxed_slice())
}

pub(crate) fn local_sequential_bindings(
    module: &word::WordModule,
    source_module: &word::WordModule,
    region: crate::RegionAnchorId,
    provenance: &crate::planning::regional::LocalOperationSemantics,
    source_cells: &std::collections::BTreeMap<word::OpId, opto_ir::design::CellId>,
    state_relations: &std::collections::BTreeMap<word::OpId, [u8; 32]>,
) -> Result<Box<[SourceSequentialBinding]>, crate::SynthError> {
    module
        .operations()
        .iter()
        .enumerate()
        .filter_map(|(index, operation)| {
            matches!(
                operation.kind,
                word::OpKind::Register(_) | word::OpKind::Latch(_)
            )
            .then_some(index)
        })
        .map(|index| {
            let operation = word::OpId::from_index(index).map_err(crate::SynthError::from)?;
            let semantics = provenance.states(operation).ok_or_else(|| {
                crate::SynthError::invariant("private state has no semantic binding")
            })?;
            let relations = semantics
                .iter()
                .filter_map(|source| match source {
                    crate::planning::regional::LocalStateSource::Operation(source) => {
                        Some(state_relations.get(source).copied())
                    }
                    crate::planning::regional::LocalStateSource::Memory { .. } => None,
                })
                .collect::<Vec<_>>();
            if relations.iter().any(Option::is_some) && relations.iter().any(Option::is_none) {
                return Err(crate::SynthError::invariant(
                    "encoded state was merged with a direct source state",
                ));
            }
            let mut relation = None;
            for proof in relations.into_iter().flatten() {
                if relation.replace(proof).is_some_and(|old| old != proof) {
                    return Err(crate::SynthError::invariant(
                        "private state has conflicting sequential relations",
                    ));
                }
            }
            let mut states = semantics
                .iter()
                .map(|source| match *source {
                    crate::planning::regional::LocalStateSource::Operation(source) => {
                        let cell = source_cells.get(&source).copied().ok_or_else(|| {
                            crate::SynthError::invariant(
                                "private state has no stable logical cell binding",
                            )
                        })?;
                        let value = source_module
                            .operation(source)
                            .ok_or_else(|| {
                                crate::SynthError::invariant("private state source is not live")
                            })?
                            .result;
                        Ok::<_, crate::SynthError>(SequentialStateSource::Operation { cell, value })
                    }
                    crate::planning::regional::LocalStateSource::Memory { memory, ordinal } => {
                        let stored = source_module.memory(memory).ok_or_else(|| {
                            crate::SynthError::invariant("private memory source is not live")
                        })?;
                        Ok(SequentialStateSource::Memory {
                            cell: crate::regional::logical_memory_cell_id(source_module, memory)?,
                            memory,
                            ordinal,
                            width: stored.element_type.width(),
                        })
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            states.sort_unstable();
            states.dedup();
            if states.is_empty() {
                return Err(crate::SynthError::invariant(
                    "private state has no stable source relation",
                ));
            }
            Ok(SourceSequentialBinding {
                operation,
                states: states.into_boxed_slice(),
                region,
                state_relation: relation,
            })
        })
        .collect()
}

pub(crate) fn plan_regional_sequential_cells(
    module: &word::WordModule,
    source_module: &word::WordModule,
    operations: &[SequentialRegionBinding],
    mapping: &crate::mapping::TargetMappingContext,
    endpoints: &std::collections::BTreeMap<
        crate::mapping::RegionalPinKey,
        crate::mapping::RegionalEndpoint,
    >,
) -> Result<Box<[RegionalSequentialCellPlan]>, crate::SynthError> {
    operations
        .iter()
        .map(|binding| {
            let operation = module.operation(binding.operation).ok_or_else(|| {
                crate::SynthError::invariant("private lowered state operation disappeared")
            })?;
            let (mapped, roles) = match &operation.kind {
                word::OpKind::Register(register) => {
                    let selected = mapping.sequential_catalog.select_scalar_register(
                        module,
                        register,
                        &mapping.combinational_catalog,
                    )?;
                    let resets = register
                        .resets
                        .iter()
                        .map(|reset| reset.value)
                        .collect::<Vec<_>>();
                    let reset_roles = reset_control_roles(resets.len())?;
                    match selected {
                        SelectedRegisterCell::Simple(cell) => (
                            cell.mapped_cell(
                                register.d,
                                register.clock,
                                &resets,
                                operation.result,
                                None,
                            ),
                            std::iter::once(crate::mapping::SequentialPinRole::Data)
                                .chain(std::iter::once(crate::mapping::SequentialPinRole::Clock))
                                .chain(reset_roles)
                                .collect::<Vec<_>>(),
                        ),
                        SelectedRegisterCell::Enabled(cell) => {
                            let enable = register.enable.ok_or_else(|| {
                                crate::SynthError::invariant(
                                    "enabled state-cell plan has no semantic enable",
                                )
                            })?;
                            if enable.active_high != cell.enable_active_high() {
                                return Err(crate::SynthError::invariant(
                                    "private state-cell plan has unnormalized enable polarity",
                                ));
                            }
                            (
                                cell.mapped_cell(
                                    register.d,
                                    enable.value,
                                    register.clock,
                                    &resets,
                                    operation.result,
                                    None,
                                ),
                                std::iter::once(crate::mapping::SequentialPinRole::Data)
                                    .chain(std::iter::once(
                                        crate::mapping::SequentialPinRole::Enable,
                                    ))
                                    .chain(std::iter::once(
                                        crate::mapping::SequentialPinRole::Clock,
                                    ))
                                    .chain(reset_roles)
                                    .collect::<Vec<_>>(),
                            )
                        }
                    }
                }
                word::OpKind::Latch(latch) => {
                    let requests =
                        crate::mapping::sequential::async_reset_requests(module, &latch.resets)?;
                    let cell = mapping
                        .sequential_catalog
                        .best_latch(
                            &requests,
                            latch.enable.active_high,
                            false,
                            crate::mapping::sequential::enable_inverter_cost(
                                module,
                                latch.enable.value,
                                &mapping.combinational_catalog,
                            ),
                        )
                        .ok_or_else(|| {
                            crate::SynthError::mapping("target library has no compatible latch")
                        })?;
                    if latch.enable.active_high != cell.enable_active_high() {
                        return Err(crate::SynthError::invariant(
                            "private latch plan has unnormalized enable polarity",
                        ));
                    }
                    let resets = latch
                        .resets
                        .iter()
                        .map(|reset| reset.value)
                        .collect::<Vec<_>>();
                    let reset_roles = reset_control_roles(resets.len())?;
                    (
                        cell.mapped_cell(
                            latch.d,
                            latch.enable.value,
                            &resets,
                            operation.result,
                            None,
                        ),
                        std::iter::once(crate::mapping::SequentialPinRole::Data)
                            .chain(std::iter::once(crate::mapping::SequentialPinRole::Enable))
                            .chain(reset_roles)
                            .collect::<Vec<_>>(),
                    )
                }
                _ => {
                    return Err(crate::SynthError::invariant(
                        "regional state-cell plan contains combinational logic",
                    ));
                }
            };
            if mapped.input_connections.len() != roles.len() || mapped.output_connections.len() != 1
            {
                return Err(crate::SynthError::invariant(
                    "selected regional state cell has an unsupported pin shape",
                ));
            }
            let source = binding.sources.first().ok_or_else(|| {
                crate::SynthError::invariant("selected state cell has no semantic source")
            })?;
            let endpoint = |role| {
                let bit = source.semantic_bit()?;
                endpoints
                    .get(&crate::mapping::RegionalPinKey::Sequential(
                        crate::mapping::SequentialPinKey {
                            state: source.cell(),
                            role,
                            bit,
                        },
                    ))
                    .copied()
                    .ok_or_else(|| {
                        crate::SynthError::invariant(
                            "selected state cell pin has no stable endpoint",
                        )
                    })
            };
            let instance_name = ArtifactBuilder::source_name(source_module, source)?;
            Ok(RegionalSequentialCellPlan {
                sources: binding.sources.clone(),
                region: binding.region,
                instance_name: instance_name.into_boxed_str(),
                cell_name: mapped.cell_name.into_boxed_str(),
                inputs: mapped
                    .input_connections
                    .into_iter()
                    .zip(roles)
                    .map(|(connection, role)| {
                        endpoint(role).map(|endpoint| (connection.pin.into_boxed_str(), endpoint))
                    })
                    .collect::<Result<_, _>>()?,
                output: (
                    mapped.output_connections[0].pin.clone().into_boxed_str(),
                    endpoint(crate::mapping::SequentialPinRole::StateOutput)?,
                ),
            })
        })
        .collect()
}

fn reset_control_roles(
    count: usize,
) -> Result<Vec<crate::mapping::SequentialPinRole>, crate::SynthError> {
    (0..count)
        .map(|index| {
            u32::try_from(index)
                .map(crate::mapping::SequentialPinRole::ResetControl)
                .map_err(|_| crate::SynthError::capacity("regional sequential reset index"))
        })
        .collect()
}

/// Resolves source state operations to scalar state operations while retaining
/// the stable region identity established by the sealed work graph.
pub(crate) fn lowered_sequential_operations(
    module: &word::WordModule,
    binding: &crate::boolean::bitblast::LoweredRegionBinding,
    source_operations: &[SourceSequentialBinding],
) -> Result<Box<[SequentialRegionBinding]>, crate::SynthError> {
    let mut lowered = std::collections::BTreeMap::<
        word::OpId,
        (
            crate::RegionAnchorId,
            Vec<SequentialSourceBit>,
            Vec<word::OpId>,
        ),
    >::new();
    for source_binding in source_operations {
        let source = module.operation(source_binding.operation).ok_or_else(|| {
            crate::SynthError::invariant("source sequential operation disappeared")
        })?;
        let values = binding.lowered_bits(source.result).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "frozen sequential operation {:?} has no lowered state values",
                source_binding.operation
            ))
        })?;
        for (bit, &value) in values.iter().enumerate() {
            let bit = u32::try_from(bit)
                .map_err(|_| crate::SynthError::capacity("sequential source bit index"))?;
            let operation = module
                .value(value)
                .and_then(|value| match value.kind {
                    word::ValueKind::Operation(operation) => Some(operation),
                    word::ValueKind::Signal(_) | word::ValueKind::Constant(_) => None,
                })
                .ok_or_else(|| {
                    crate::SynthError::invariant(
                        "lowered sequential state is not produced by an operation",
                    )
                })?;
            let stored = module.operation(operation).ok_or_else(|| {
                crate::SynthError::invariant("lowered sequential operation disappeared")
            })?;
            if !matches!(
                stored.kind,
                word::OpKind::Register(_) | word::OpKind::Latch(_)
            ) {
                return Err(crate::SynthError::invariant(format!(
                    "lowered sequential state {value:?} is produced by combinational operation {operation:?}: {:?}",
                    stored.kind
                )));
            }
            let row = lowered
                .entry(operation)
                .or_insert_with(|| (source_binding.region, Vec::new(), Vec::new()));
            if row.0 != source_binding.region {
                return Err(crate::SynthError::invariant(
                    "one lowered sequential operation has conflicting region bindings",
                ));
            }
            row.1.extend(
                source_binding
                    .states
                    .iter()
                    .copied()
                    .map(|source| SequentialSourceBit {
                        source,
                        bit,
                        state_relation: source_binding.state_relation,
                    }),
            );
            row.2.push(source_binding.operation);
        }
    }
    Ok(lowered
        .into_iter()
        .map(|(operation, (region, mut sources, mut lowering_sources))| {
            sources.sort_unstable();
            sources.dedup();
            lowering_sources.sort_unstable();
            lowering_sources.dedup();
            SequentialRegionBinding {
                operation,
                region,
                sources: sources.into_boxed_slice(),
                lowering_sources: lowering_sources.into_boxed_slice(),
            }
        })
        .collect())
}

pub(crate) fn reconcile_sequential_publication<'a>(
    module: &word::WordModule,
    region_binding: &crate::boolean::bitblast::LoweredRegionBinding,
    plans: impl IntoIterator<Item = &'a RegionalSequentialCellPlan>,
    expected_cells: impl IntoIterator<Item = opto_ir::design::CellId>,
) -> Result<SequentialPublicationPlan, crate::SynthError> {
    let mut aliases = Vec::new();
    let mut sources = std::collections::BTreeSet::new();
    for plan in plans {
        for source in &plan.sources {
            if !sources.insert((source.source, source.semantic_bit()?)) {
                return Err(crate::SynthError::invariant(
                    "source state bit has multiple regional cell plans",
                ));
            }
        }
        let mut members = plan
            .sources
            .iter()
            .filter(|source| source.state_relation.is_none())
            .filter_map(|source| source.source_value().map(|value| (source, value)))
            .map(|(source, value)| {
                let stored = module.value(value).ok_or_else(|| {
                    crate::SynthError::invariant("regional state source is not live")
                })?;
                region_binding
                    .lowered_bits(value)
                    .and_then(|bits| bits.get(source.bit as usize))
                    .copied()
                    .or_else(|| (stored.ty.width() == 1 && source.bit == 0).then_some(value))
                    .ok_or_else(|| {
                        crate::SynthError::invariant("regional state source bit was not lowered")
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        members.sort_unstable();
        members.dedup();
        if let Some(&representative) = members.first() {
            aliases.extend(
                members[1..]
                    .iter()
                    .copied()
                    .map(|value| (value, representative)),
            );
        }
    }
    let expected = expected_cells.into_iter().collect::<BTreeSet<_>>();
    let published = sources
        .iter()
        .filter_map(|(source, _)| match *source {
            SequentialStateSource::Operation { cell, .. } => Some(cell),
            SequentialStateSource::Memory { .. } => None,
        })
        .collect::<BTreeSet<_>>();
    if published != expected {
        return Err(crate::SynthError::invariant(
            "canonical state cells do not have exact regional publication coverage",
        ));
    }
    aliases.sort_unstable();
    Ok(SequentialPublicationPlan {
        aliases: aliases.into_boxed_slice(),
    })
}

/// Returns every value needed to materialize a frozen sequential operation set.
pub(crate) fn sequential_binding_values(
    module: &word::WordModule,
    operations: &[SourceSequentialBinding],
) -> Result<Box<[word::ValueId]>, crate::SynthError> {
    collect_sequential_binding_values(module, operations.iter().map(|binding| binding.operation))
}

pub(crate) fn sequential_plan_values<'a>(
    module: &word::WordModule,
    binding: &crate::boolean::bitblast::LoweredRegionBinding,
    plans: impl IntoIterator<Item = &'a RegionalSequentialCellPlan>,
) -> Result<Box<[word::ValueId]>, crate::SynthError> {
    let mut values = BTreeSet::new();
    for endpoint in plans.into_iter().flat_map(|plan| {
        plan.inputs
            .iter()
            .map(|(_, endpoint)| endpoint)
            .chain(std::iter::once(&plan.output.1))
    }) {
        let crate::mapping::RegionalEndpoint::SourceBit { value, bit } = *endpoint else {
            continue;
        };
        let stored = module
            .value(value)
            .ok_or_else(|| crate::SynthError::invariant("state endpoint source is not live"))?;
        let lowered = binding
            .lowered_bits(value)
            .and_then(|bits| bits.get(bit as usize))
            .copied()
            .or_else(|| (stored.ty.width() == 1 && bit == 0).then_some(value))
            .ok_or_else(|| {
                crate::SynthError::invariant("state endpoint source bit was not lowered")
            })?;
        values.insert(lowered);
    }
    Ok(values.into_iter().collect())
}

fn collect_sequential_binding_values(
    module: &word::WordModule,
    operations: impl IntoIterator<Item = word::OpId>,
) -> Result<Box<[word::ValueId]>, crate::SynthError> {
    let mut values = BTreeSet::new();
    for operation in operations {
        let operation = module
            .operation(operation)
            .ok_or_else(|| crate::SynthError::invariant("live sequential operation disappeared"))?;
        values.insert(operation.result);
        values.extend(crate::word::operation_inputs(&operation.kind));
    }
    Ok(values.into_iter().collect())
}

impl MappedFixedSubstrateArtifact {
    #[allow(
        clippy::redundant_closure_for_method_calls,
        reason = "the method's defining module is private, so its method-item path is not nameable here"
    )]
    pub(crate) fn from_module<'a>(
        module: &word::WordModule,
        region_binding: &crate::boolean::bitblast::LoweredRegionBinding,
        mapped_values: &WordMappedSignals,
        plans: impl IntoIterator<Item = &'a RegionalSequentialCellPlan>,
        substrate: impl IntoIterator<Item = &'a RegionalSubstrateCellPlan>,
        regional_pins: &super::RegionalMappedPins,
        config: &crate::mapping::MappingConfig<'_>,
    ) -> Result<Self, crate::SynthError> {
        let mut artifact = ArtifactBuilder::new();
        let mut seen = std::collections::BTreeSet::new();
        for plan in plans {
            if !seen.insert((plan.region, plan.sources.clone())) {
                return Err(crate::SynthError::invariant(
                    "duplicate regional state-cell plan",
                ));
            }
            artifact.push_planned_cell(
                module,
                region_binding,
                mapped_values,
                regional_pins,
                &config.options.target_cells,
                plan,
            )?;
        }
        for plan in substrate {
            artifact.push_substrate_cell(
                module,
                region_binding,
                mapped_values,
                regional_pins,
                &config.options.target_cells,
                plan,
            )?;
        }
        validate_artifact_nets(
            "fixed regional substrate",
            &artifact.nets,
            &artifact.cells,
            &config.options.target_cells,
        )?;
        Ok(artifact.finish())
    }

    pub(crate) fn required_nets(&self) -> &[NetId] {
        &self.referenced_nets
    }

    pub(crate) fn append_to_delta(
        &self,
        delta: &mut RegionDelta,
    ) -> Result<PendingMappedFixedSubstrate, crate::SynthError> {
        if self
            .referenced_nets
            .iter()
            .any(|&net| !delta.snapshot().contains_net(net))
        {
            return Err(crate::SynthError::invariant(
                "fixed regional substrate net is absent from its transaction snapshot",
            ));
        }
        let (_, cells) = super::append_artifact_cells(
            delta,
            &self.nets,
            &self.cells,
            "fixed regional substrate references an unknown internal net",
            |cell, &source| (cell, source),
        )?;
        Ok(PendingMappedFixedSubstrate { cells })
    }
}

struct ArtifactBuilder {
    cells: Vec<ArtifactCell<MappedCellSource>>,
    nets: ArtifactNetTable,
}

impl ArtifactBuilder {
    fn new() -> Self {
        Self {
            cells: Vec::new(),
            nets: ArtifactNetTable::default(),
        }
    }

    fn target_cell<'a>(
        target_cells: &'a opto_library::TargetCellSet,
        name: &str,
    ) -> Result<(u32, opto_library::TargetCellRef<'a>), crate::SynthError> {
        let (index, cell) = target_cells
            .iter()
            .enumerate()
            .find(|(_, cell)| cell.name() == name)
            .ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "fixed regional cell '{name}' disappeared from the target library"
                ))
            })?;
        let index = u32::try_from(index)
            .map_err(|_| crate::SynthError::capacity("target cell index exceeds 32 bits"))?;
        Ok((index, cell))
    }

    fn push_cell(
        &mut self,
        instance_name: &str,
        cell_name: &str,
        library_cell: u32,
        connections: Box<[(String, Option<u16>, super::ArtifactSignal)]>,
        metadata: MappedCellSource,
    ) {
        self.cells.push(ArtifactCell {
            name: instance_name.to_owned(),
            cell_type: cell_name.to_owned(),
            library_cell: Some(library_cell),
            connections,
            metadata,
        });
    }

    fn push_planned_cell(
        &mut self,
        module: &word::WordModule,
        region_binding: &crate::boolean::bitblast::LoweredRegionBinding,
        mapped_values: &WordMappedSignals,
        regional_pins: &super::RegionalMappedPins,
        target_cells: &opto_library::TargetCellSet,
        plan: &RegionalSequentialCellPlan,
    ) -> Result<(), crate::SynthError> {
        let (library_cell, target) = Self::target_cell(target_cells, &plan.cell_name)?;
        let mut connections = Vec::with_capacity(plan.inputs.len().saturating_add(1));
        for (pin, endpoint) in &plan.inputs {
            let signal = resolve_endpoint(
                module,
                region_binding,
                mapped_values,
                regional_pins,
                *endpoint,
            )?;
            connections.push((
                pin.to_string(),
                Some(target_pin_id(target, pin)?),
                self.nets.signal(signal),
            ));
        }
        let output = resolve_endpoint(
            module,
            region_binding,
            mapped_values,
            regional_pins,
            plan.output.1,
        )?;
        let output = self.nets.signal(output);
        let output = self.nets.claim_output(Some(output))?;
        connections.push((
            plan.output.0.to_string(),
            Some(target_pin_id(target, &plan.output.0)?),
            output,
        ));
        let source = plan.sources.first().ok_or_else(|| {
            crate::SynthError::invariant("regional state-cell plan has no semantic source")
        })?;
        let metadata = match source.source {
            SequentialStateSource::Operation { value, .. } => MappedCellSource::Value {
                value,
                region: plan.region,
            },
            SequentialStateSource::Memory {
                memory, ordinal, ..
            } => MappedCellSource::Memory {
                memory,
                ordinal,
                region: plan.region,
            },
        };
        self.push_cell(
            &plan.instance_name,
            &plan.cell_name,
            library_cell,
            connections.into_boxed_slice(),
            metadata,
        );
        Ok(())
    }

    fn push_substrate_cell(
        &mut self,
        module: &word::WordModule,
        region_binding: &crate::boolean::bitblast::LoweredRegionBinding,
        mapped_values: &WordMappedSignals,
        regional_pins: &super::RegionalMappedPins,
        target_cells: &opto_library::TargetCellSet,
        plan: &RegionalSubstrateCellPlan,
    ) -> Result<(), crate::SynthError> {
        let (library_cell, target) = Self::target_cell(target_cells, &plan.cell_name)?;
        let connections = plan
            .connections
            .iter()
            .map(|connection| {
                let signal = resolve_endpoint(
                    module,
                    region_binding,
                    mapped_values,
                    regional_pins,
                    connection.endpoint,
                )?;
                let signal = self.nets.signal(signal);
                let signal = if connection.output {
                    self.nets.claim_output(Some(signal))?
                } else {
                    signal
                };
                Ok((
                    connection.pin.to_string(),
                    Some(target_pin_id(target, &connection.pin)?),
                    signal,
                ))
            })
            .collect::<Result<Box<[_]>, crate::SynthError>>()?;
        let instance_name =
            super::regional_substrate_instance_name(plan.region, &plan.instance_name);
        self.push_cell(
            &instance_name,
            &plan.cell_name,
            library_cell,
            connections,
            MappedCellSource::Value {
                value: plan.source,
                region: plan.region,
            },
        );
        Ok(())
    }

    /// Names one sequential cell.
    ///
    /// Registers and latches keep the `<state>_reg` name derived from
    /// the state they implement, so a published netlist, its reports, and any
    /// constraint that matches cells by name all refer to the same object a
    /// designer wrote. Only a state with no recoverable name falls back to a
    /// synthetic identifier.
    fn source_name(
        module: &word::WordModule,
        source: &SequentialSourceBit,
    ) -> Result<String, crate::SynthError> {
        let Some(source_value) = source.source_value() else {
            let SequentialStateSource::Memory {
                memory, ordinal, ..
            } = source.source
            else {
                unreachable!("a source without a value is a memory state")
            };
            let memory = module.memory(memory).ok_or_else(|| {
                crate::SynthError::invariant("state-cell memory source is not live")
            })?;
            let name = legalize_identifier(module.name_str(memory.name));
            return Ok(unique_instance_name(
                module,
                format!("{name}_reg_{ordinal}_{}_", source.bit),
            ));
        };
        let stored = module
            .value(source_value)
            .ok_or_else(|| crate::SynthError::invariant("state-cell source value is not live"))?;
        let word::ValueKind::Operation(operation) = stored.kind else {
            return Err(crate::SynthError::invariant(
                "state-cell source value is not sequential",
            ));
        };
        let target = module
            .connects()
            .iter()
            .find(|connect| connect.value == source_value)
            .map(|connect| &connect.target);
        let projected = if target.is_none() {
            let semantics = crate::mapping::roots::FullDomainRootSemantics::new(module)?;
            module.connects().iter().find_map(|connect| {
                let signal = module.signal(connect.target.signal)?;
                signal.name?;
                (0..signal.ty.width()).find_map(|target_bit| {
                    matches!(
                        semantics
                            .canonical_publication_bit(connect.value, target_bit)
                            .ok()?,
                        crate::mapping::roots::CanonicalPublicationBit::Value { value, bit }
                            if value == source_value && bit == 0
                    )
                    .then_some((connect.target.signal, target_bit))
                })
            })
        } else {
            None
        };
        let mut base = sequential_state_stem(module, operation, target)
            .or_else(|| {
                let (signal, bit) = projected?;
                let signal = module.signal(signal)?;
                let name = legalize_identifier(module.name_str(signal.name?));
                Some(if signal.ty.width() == 1 {
                    format!("{name}_reg")
                } else {
                    format!("{name}_reg_{bit}_")
                })
            })
            .unwrap_or_else(|| format!("__opto_seq_{}", operation.index()));
        if stored.ty.width() > 1 && target.is_none_or(|target| target.range.is_none()) {
            write!(&mut base, "_{}_", source.bit).expect("writing to a String cannot fail");
        }
        Ok(unique_instance_name(module, base))
    }

    fn finish(self) -> MappedFixedSubstrateArtifact {
        let referenced_nets = self.nets.external_nets().collect::<Vec<_>>();
        MappedFixedSubstrateArtifact {
            nets: self.nets,
            cells: self.cells.into_boxed_slice(),
            referenced_nets: referenced_nets.into_boxed_slice(),
        }
    }
}

fn resolve_endpoint(
    module: &word::WordModule,
    region_binding: &crate::boolean::bitblast::LoweredRegionBinding,
    mapped_values: &WordMappedSignals,
    regional_pins: &super::RegionalMappedPins,
    endpoint: crate::mapping::RegionalEndpoint,
) -> Result<MappedValueSignal, crate::SynthError> {
    match endpoint {
        crate::mapping::RegionalEndpoint::Pin(pin) => {
            regional_pins.require(pin).map(MappedValueSignal::Net)
        }
        crate::mapping::RegionalEndpoint::Constant(value) => Ok(MappedValueSignal::Constant(value)),
        crate::mapping::RegionalEndpoint::SourceBit { value, bit } => {
            let stored = module
                .value(value)
                .ok_or_else(|| crate::SynthError::invariant("state endpoint source is not live"))?;
            let lowered = region_binding
                .lowered_bits(value)
                .and_then(|bits| bits.get(bit as usize))
                .copied()
                .or_else(|| (stored.ty.width() == 1 && bit == 0).then_some(value))
                .ok_or_else(|| {
                    crate::SynthError::invariant("state endpoint source bit was not lowered")
                })?;
            mapped_values.require(lowered)
        }
    }
}

/// Recovers the source-visible state name one sequential operation implements.
///
/// The operation's own interned state name wins. Otherwise the name is taken
/// from the signal its result drives, which is what the source actually
/// declared.
fn sequential_state_stem(
    module: &word::WordModule,
    operation: word::OpId,
    target: Option<&word::LValue>,
) -> Option<String> {
    let stored = module.operation(operation)?;
    let state_name = match &stored.kind {
        word::OpKind::Register(register) => register.name,
        word::OpKind::Latch(latch) => latch.name,
        _ => None,
    };
    let stem = if let Some(name) = state_name {
        legalize_identifier(module.name_str(name))
    } else {
        let signal = module.signal(target?.signal)?;
        legalize_identifier(module.name_str(signal.name?))
    };
    // A vector state lowers to one cell per bit, all deriving the same stem.
    // The bit index follows `_reg`, matching how the source bit is written and
    // how every other tool in this flow names a bit-blasted register.
    match target.and_then(|target| target.range) {
        Some(range) => Some(format!("{stem}_reg_{}_", range.lsb)),
        None => Some(format!("{stem}_reg")),
    }
}

/// Rewrites a source identifier into one that is legal in every mapped output.
fn legalize_identifier(name: &str) -> String {
    let mut result = String::with_capacity(name.len());
    for character in name.chars() {
        if character.is_ascii_alphanumeric() || character == '_' {
            result.push(character);
        } else {
            result.push('_');
        }
    }
    if result.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        result.insert(0, '_');
    }
    result
}

fn unique_instance_name(module: &word::WordModule, base: String) -> String {
    if module.instance_id(&base).is_none() {
        return base;
    }
    let mut suffix = 1u32;
    loop {
        let candidate = format!("{base}_{suffix}");
        if module.instance_id(&candidate).is_none() {
            return candidate;
        }
        suffix = suffix
            .checked_add(1)
            .expect("mapped instance count fits within 32-bit naming space");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opto_ir::word::{Edge, LValue, PortDirection, RegisterOp, SourceSpan, WordType};

    #[test]
    fn memory_register_bank_uses_memory_state_semantics() {
        let source_span = SourceSpan::stable("memory state binding test");
        let mut source = word::WordModule::new("source");
        let memory = source
            .add_memory(
                "memory",
                WordType::bits(8).unwrap(),
                std::num::NonZeroU32::new(2).unwrap(),
                source_span.clone(),
            )
            .unwrap();
        let mut local = word::WordModule::new("local");
        let data = local
            .constant(
                opto_ir::ConstBits::from_bin_str("00000000").unwrap(),
                WordType::bits(8).unwrap(),
                source_span.clone(),
            )
            .unwrap();
        let clock = local
            .constant(
                opto_ir::ConstBits::from_bin_str("0").unwrap(),
                WordType::bits(1).unwrap(),
                source_span.clone(),
            )
            .unwrap();
        let state = local
            .register(
                RegisterOp {
                    name: None,
                    d: data,
                    clock,
                    edge: Edge::Pos,
                    enable: None,
                    resets: vec![],
                },
                source_span,
            )
            .unwrap();
        let word::ValueKind::Operation(operation) = local.value(state).unwrap().kind else {
            unreachable!()
        };
        let mut semantics = crate::planning::regional::LocalOperationSemantics::default();
        semantics.record_generated(operation, []).unwrap();
        semantics.record_memory_state(operation, memory, 1).unwrap();

        let bindings = local_sequential_bindings(
            &local,
            &source,
            crate::RegionAnchorId::from_bytes_for_test([1; 32]),
            &semantics,
            &std::collections::BTreeMap::new(),
            &std::collections::BTreeMap::new(),
        )
        .unwrap();

        assert_eq!(bindings.len(), 1);
        assert!(matches!(
            bindings[0].states[0],
            SequentialStateSource::Memory {
                memory: bound,
                ordinal: 1,
                width: 8,
                ..
            } if bound == memory
        ));
    }

    #[test]
    fn stable_state_region_follows_the_scalar_operation_emitted_by_bit_lowering() {
        let mut module = word::WordModule::new("stable_state_region");
        let bit = WordType::bits(1).unwrap();
        let source = SourceSpan::stable("stable state region test");
        let clock = module
            .add_port("clock", PortDirection::Input, bit, source.clone())
            .unwrap();
        let data = module
            .add_port("data", PortDirection::Input, bit, source.clone())
            .unwrap();
        let output = module
            .add_port("q", PortDirection::Output, bit, source.clone())
            .unwrap();
        let clock = module
            .read_signal(module.port(clock).unwrap().signal, source.clone())
            .unwrap();
        let data = module
            .read_signal(module.port(data).unwrap().signal, source.clone())
            .unwrap();
        let state = module
            .register(
                RegisterOp {
                    name: None,
                    d: data,
                    clock,
                    edge: Edge::Pos,
                    enable: None,
                    resets: Vec::new(),
                },
                source.clone(),
            )
            .unwrap();
        module
            .connect(
                LValue::signal(module.port(output).unwrap().signal),
                state,
                source,
            )
            .unwrap();
        let regions = crate::regional::region_graph::partition::build(
            &module,
            crate::regional::region_graph::RegionPartitionPolicy::default(),
        )
        .unwrap();
        let source_operations = sequential_region_bindings(&module, &regions).unwrap();
        let required = sequential_binding_values(&module, &source_operations).unwrap();
        let shell = crate::planning::operator::ArchitectureDecisions::for_regional_shell(&module);
        let mut provenance =
            crate::artifact::provenance::ProvenanceBuilder::new(&module, &shell).unwrap();

        let binding = crate::boolean::bitblast::bitblast_module_with_regions(
            &mut module,
            &shell,
            &mut provenance,
            &regions.operation_region_rows(),
            &required,
            &[],
            crate::boolean::bitblast::GlobalBitblastScope::RegionalShell,
        )
        .unwrap();
        let lowered = lowered_sequential_operations(&module, &binding, &source_operations).unwrap();

        assert_eq!(lowered.len(), 1);
        assert_eq!(lowered[0].region, source_operations[0].region);
        assert_ne!(lowered[0].operation, source_operations[0].operation);
        assert_eq!(
            lowered[0].sources.as_ref(),
            &[SequentialSourceBit {
                source: source_operations[0].states[0],
                bit: 0,
                state_relation: None,
            }]
        );
        assert!(matches!(
            module.operation(lowered[0].operation).unwrap().kind,
            word::OpKind::Register(_)
        ));
    }
}
