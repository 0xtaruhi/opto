// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Direct materialization of live state elements into the mapped substrate.
//!
//! Sequential cells are immutable state-region substrate objects: regional
//! epochs replace only combinational covers. This module therefore prepares one
//! deterministic delta from the lowered Word model without first emitting
//! target instances back into that model. Enable polarity is normalized in
//! the private Word model, so any inverter is covered by the ordinary Boolean
//! mapper rather than introduced by this publication layer.

use super::region_delta::{MappedValueSignal, WordMappedSignals};
use super::{
    ArtifactCell, ArtifactNetTable, ArtifactSignal, target_pin_id, validate_artifact_nets,
};
use crate::artifact::MappedCellSource;
use crate::mapping::MappedCell;
use crate::mapping::library::CombinationalCellCatalog;
use crate::mapping::sequential::{SelectedRegisterCell, SequentialCellCatalog};
use opto_ir::mapped::{AppliedRegionDelta, CellId, NetId, RegionDelta, TempCellId};
use opto_ir::word;
use std::collections::BTreeSet;

/// Immutable topology for every live register and latch in one lowered design.
///
/// Existing nets are explicit and sorted.
#[derive(Debug, Clone)]
pub(crate) struct MappedSequentialArtifact {
    nets: ArtifactNetTable,
    cells: Box<[ArtifactCell<MappedCellSource>]>,
    referenced_nets: Box<[NetId]>,
}

/// Delta-local sequential identities retained until mapped/timing commit.
#[derive(Debug)]
pub(crate) struct PendingMappedSequential {
    cells: Box<[(TempCellId, MappedCellSource)]>,
}

/// Stable source state selected before scalar bit lowering.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SourceSequentialBinding {
    operation: word::OpId,
    region: crate::RegionAnchorId,
}

/// One source-visible state bit implemented by a lowered state operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SequentialSourceBit {
    pub(crate) operation: word::OpId,
    pub(crate) bit: u32,
}

/// Exact semantic state relation for one live scalar state operation.
#[derive(Debug, Clone)]
pub(crate) struct SequentialRegionBinding {
    pub(crate) operation: word::OpId,
    pub(crate) region: crate::RegionAnchorId,
    pub(crate) sources: Box<[SequentialSourceBit]>,
}

impl PendingMappedSequential {
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
                    region: region.id(),
                });
            }
        }
    }
    Ok(operations.into_boxed_slice())
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
        (crate::RegionAnchorId, Vec<SequentialSourceBit>),
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
                return Err(crate::SynthError::invariant(
                    "lowered sequential state is produced by combinational logic",
                ));
            }
            let row = lowered
                .entry(operation)
                .or_insert_with(|| (source_binding.region, Vec::new()));
            if row.0 != source_binding.region {
                return Err(crate::SynthError::invariant(
                    "one lowered sequential operation has conflicting region bindings",
                ));
            }
            row.1.push(SequentialSourceBit {
                operation: source_binding.operation,
                bit,
            });
        }
    }
    Ok(lowered
        .into_iter()
        .map(|(operation, (region, mut sources))| {
            sources.sort_unstable();
            sources.dedup();
            SequentialRegionBinding {
                operation,
                region,
                sources: sources.into_boxed_slice(),
            }
        })
        .collect())
}

/// Returns every value needed to materialize a frozen sequential operation set.
pub(crate) fn sequential_binding_values(
    module: &word::WordModule,
    operations: &[SourceSequentialBinding],
) -> Result<Box<[word::ValueId]>, crate::SynthError> {
    collect_sequential_binding_values(module, operations.iter().map(|binding| binding.operation))
}

/// Returns every scalar value needed by the selected lowered state cells.
pub(crate) fn lowered_sequential_binding_values(
    module: &word::WordModule,
    operations: &[SequentialRegionBinding],
) -> Result<Box<[word::ValueId]>, crate::SynthError> {
    collect_sequential_binding_values(module, operations.iter().map(|binding| binding.operation))
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

impl MappedSequentialArtifact {
    #[allow(
        clippy::redundant_closure_for_method_calls,
        reason = "the method's defining module is private, so its method-item path is not nameable here"
    )]
    pub(crate) fn from_module(
        module: &word::WordModule,
        mapped_values: &WordMappedSignals,
        operations: &[SequentialRegionBinding],
        config: &crate::mapping::MappingConfig<'_>,
    ) -> Result<Self, crate::SynthError> {
        let mut artifact = ArtifactBuilder::new(module)?;
        let library = LibraryContext {
            module,
            mapped_values,
            sequential_catalog: &config.mapping_context.sequential_catalog,
            combinational_catalog: &config.mapping_context.combinational_catalog,
            target_cells: &config.options.target_cells,
        };
        for binding in operations {
            let operation_id = binding.operation;
            let operation = module.operation(operation_id).ok_or_else(|| {
                crate::SynthError::invariant("live sequential operation disappeared")
            })?;
            require_scalar(module, operation.result, "sequential result")?;
            let region = binding.region;
            match &operation.kind {
                word::OpKind::Register(register) => {
                    artifact.push_library_register(
                        &library,
                        operation_id,
                        operation.result,
                        region,
                        register,
                    )?;
                }
                word::OpKind::Latch(latch) => {
                    artifact.push_library_latch(
                        &library,
                        operation_id,
                        operation.result,
                        region,
                        latch,
                    )?;
                }
                _ => {
                    return Err(crate::SynthError::invariant(
                        "live sequential set contains a combinational operation",
                    ));
                }
            }
        }
        validate_artifact_nets(
            "sequential artifact",
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
    ) -> Result<PendingMappedSequential, crate::SynthError> {
        if self
            .referenced_nets
            .iter()
            .any(|&net| !delta.snapshot().contains_net(net))
        {
            return Err(crate::SynthError::invariant(
                "sequential substrate net is absent from its transaction snapshot",
            ));
        }
        let (_, cells) = super::append_artifact_cells(
            delta,
            &self.nets,
            &self.cells,
            "sequential artifact references an unknown internal net",
            |cell, &source| (cell, source),
        )?;
        Ok(PendingMappedSequential { cells })
    }
}

struct ArtifactBuilder<'a> {
    module: &'a word::WordModule,
    state_targets: Vec<Option<word::LValue>>,
    cells: Vec<ArtifactCell<MappedCellSource>>,
    nets: ArtifactNetTable,
}

struct LibraryContext<'a> {
    module: &'a word::WordModule,
    mapped_values: &'a WordMappedSignals,
    sequential_catalog: &'a SequentialCellCatalog,
    combinational_catalog: &'a CombinationalCellCatalog,
    target_cells: &'a opto_library::TargetCellSet,
}

impl<'a> ArtifactBuilder<'a> {
    fn new(module: &'a word::WordModule) -> Result<Self, crate::SynthError> {
        let mut state_targets = vec![None; module.values().len()];
        for connect in module.connects() {
            let target = state_targets
                .get_mut(connect.value.index())
                .ok_or_else(|| {
                    crate::SynthError::invariant(
                        "sequential name index references a value outside the Word arena",
                    )
                })?;
            if target.is_none() {
                *target = Some(connect.target.clone());
            }
        }
        Ok(Self {
            module,
            state_targets,
            cells: Vec::new(),
            nets: ArtifactNetTable::default(),
        })
    }

    fn push_library_register(
        &mut self,
        context: &LibraryContext<'_>,
        operation_id: word::OpId,
        result: word::ValueId,
        region: crate::RegionAnchorId,
        register: &word::RegisterOp,
    ) -> Result<(), crate::SynthError> {
        let output = require_output(&mut self.nets, context.mapped_values, result)?;
        let source = MappedCellSource::Value {
            value: result,
            region,
        };
        if let Some(enable) = register.enable {
            let SelectedRegisterCell::Enabled(cell) = context.sequential_catalog.select_register(
                context.module,
                register,
                context.combinational_catalog,
            )?
            else {
                return Err(crate::SynthError::invariant(
                    "enabled register selected a simple DFF",
                ));
            };
            if enable.active_high != cell.enable_active_high() {
                return Err(crate::SynthError::invariant(
                    "enabled register reached publication with unnormalized polarity",
                ));
            }
            let mapped = cell.mapped_cell(
                register.d,
                enable.value,
                register.clock,
                &register
                    .resets
                    .iter()
                    .map(|reset| reset.value)
                    .collect::<Vec<_>>(),
                result,
                None,
            );
            let name = self.name(operation_id, "");
            return self.push_library_cell(context, name, mapped, &[], &[(0, output)], source);
        }
        let SelectedRegisterCell::Simple(cell) = context.sequential_catalog.select_register(
            context.module,
            register,
            context.combinational_catalog,
        )?
        else {
            return Err(crate::SynthError::invariant(
                "simple register selected an enabled DFF",
            ));
        };
        let mapped = cell.mapped_cell(
            register.d,
            register.clock,
            &register
                .resets
                .iter()
                .map(|reset| reset.value)
                .collect::<Vec<_>>(),
            result,
            None,
        );
        let name = self.name(operation_id, "");
        self.push_library_cell(context, name, mapped, &[], &[(0, output)], source)
    }

    fn push_library_latch(
        &mut self,
        context: &LibraryContext<'_>,
        operation_id: word::OpId,
        result: word::ValueId,
        region: crate::RegionAnchorId,
        latch: &word::LatchOp,
    ) -> Result<(), crate::SynthError> {
        let reset_requests =
            crate::mapping::sequential::async_reset_requests(context.module, &latch.resets)?;
        let cell = context
            .sequential_catalog
            .best_latch(
                &reset_requests,
                latch.enable.active_high,
                false,
                crate::mapping::sequential::enable_inverter_cost(
                    context.module,
                    latch.enable.value,
                    context.combinational_catalog,
                ),
            )
            .ok_or_else(|| crate::SynthError::mapping("target library has no compatible latch"))?;
        if latch.enable.active_high != cell.enable_active_high() {
            return Err(crate::SynthError::invariant(
                "latch reached publication with unnormalized enable polarity",
            ));
        }
        let mapped = cell.mapped_cell(
            latch.d,
            latch.enable.value,
            &latch
                .resets
                .iter()
                .map(|reset| reset.value)
                .collect::<Vec<_>>(),
            result,
            None,
        );
        let name = self.name(operation_id, "");
        let output = require_output(&mut self.nets, context.mapped_values, result)?;
        self.push_library_cell(
            context,
            name,
            mapped,
            &[],
            &[(0, output)],
            MappedCellSource::Value {
                value: result,
                region,
            },
        )
    }

    fn push_library_cell(
        &mut self,
        context: &LibraryContext<'_>,
        name: String,
        mapped: MappedCell,
        input_overrides: &[(usize, ArtifactSignal)],
        output_overrides: &[(usize, ArtifactSignal)],
        source: MappedCellSource,
    ) -> Result<(), crate::SynthError> {
        let (library_cell, target) = context
            .target_cells
            .synthesis_cells()
            .find(|(_, cell)| cell.name() == mapped.cell_name)
            .ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "selected sequential cell '{}' disappeared from the target library",
                    mapped.cell_name
                ))
            })?;
        let library_cell = u32::try_from(library_cell)
            .map_err(|_| crate::SynthError::capacity("target cell index exceeds 32 bits"))?;
        let mut connections = Vec::with_capacity(
            mapped
                .input_connections
                .len()
                .saturating_add(mapped.output_connections.len()),
        );
        for (index, connection) in mapped.input_connections.iter().enumerate() {
            let signal = match override_at(input_overrides, index) {
                Some(signal) => signal,
                None => self
                    .nets
                    .signal(context.mapped_values.require(connection.value)?),
            };
            connections.push((
                connection.pin.clone(),
                Some(target_pin_id(target, &connection.pin)?),
                signal,
            ));
        }
        for (index, connection) in mapped.output_connections.iter().enumerate() {
            let signal = match override_at(output_overrides, index) {
                Some(signal) => signal,
                None => require_output(&mut self.nets, context.mapped_values, connection.value)?,
            };
            connections.push((
                connection.pin.clone(),
                Some(target_pin_id(target, &connection.pin)?),
                signal,
            ));
        }
        self.cells.push(ArtifactCell {
            name,
            cell_type: mapped.cell_name,
            library_cell: Some(library_cell),
            connections: connections.into_boxed_slice(),
            metadata: source,
        });
        Ok(())
    }

    /// Names one sequential cell.
    ///
    /// Registers and latches keep the `<state>_reg` name derived from
    /// the state they implement, so a published netlist, its reports, and any
    /// constraint that matches cells by name all refer to the same object a
    /// designer wrote. Only a state with no recoverable name falls back to a
    /// synthetic identifier.
    fn name(&mut self, operation: word::OpId, suffix: &str) -> String {
        let target = self
            .module
            .operation(operation)
            .and_then(|operation| self.state_targets.get(operation.result.index()))
            .and_then(Option::as_ref);
        let base = sequential_state_stem(self.module, operation, target)
            .unwrap_or_else(|| format!("__opto_seq_{}", operation.index()));
        unique_instance_name(self.module, format!("{base}{suffix}"))
    }

    fn finish(self) -> MappedSequentialArtifact {
        let referenced_nets = self.nets.external_nets().collect::<Vec<_>>();
        MappedSequentialArtifact {
            nets: self.nets,
            cells: self.cells.into_boxed_slice(),
            referenced_nets: referenced_nets.into_boxed_slice(),
        }
    }
}

fn require_scalar(
    module: &word::WordModule,
    value: word::ValueId,
    description: &str,
) -> Result<(), crate::SynthError> {
    let width = module
        .value(value)
        .ok_or_else(|| crate::SynthError::invariant(format!("unknown {description} {value:?}")))?
        .ty
        .width();
    if width != 1 {
        return Err(crate::SynthError::invariant(format!(
            "{description} must be scalar, got width {width}"
        )));
    }
    Ok(())
}

fn require_output(
    nets: &mut ArtifactNetTable,
    mapped_values: &WordMappedSignals,
    value: word::ValueId,
) -> Result<ArtifactSignal, crate::SynthError> {
    match mapped_values.require(value)? {
        signal @ MappedValueSignal::Net(_) => {
            let target = nets.signal(signal);
            nets.claim_output(Some(target))
        }
        MappedValueSignal::Constant(_) => Err(crate::SynthError::invariant(
            "sequential output resolved to a constant substrate signal",
        )),
    }
}

fn override_at(overrides: &[(usize, ArtifactSignal)], index: usize) -> Option<ArtifactSignal> {
    overrides
        .iter()
        .find_map(|&(candidate, signal)| (candidate == index).then_some(signal))
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
            regions.operation_region_rows(),
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
                operation: source_operations[0].operation,
                bit: 0,
            }]
        );
        assert!(matches!(
            module.operation(lowered[0].operation).unwrap().kind,
            word::OpKind::Register(_)
        ));
    }
}
