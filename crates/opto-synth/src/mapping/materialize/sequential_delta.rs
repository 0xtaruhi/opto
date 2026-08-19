// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Direct materialization of live state elements into the mapped substrate.
//!
//! Sequential cells are immutable state-region substrate objects: regional
//! epochs replace only combinational covers. This module therefore prepares one
//! deterministic delta from the lowered Word model without first emitting
//! target instances back into that model.

use super::region_delta::{MappedValueSignal, WordMappedSignals};
use super::{ArtifactCell, ArtifactSignal, target_pin_id};
use crate::artifact::MappedCellSource;
use crate::mapping::MappedCell;
use crate::mapping::library::CombinationalCellCatalog;
use crate::mapping::sequential::{SelectedRegisterCell, SequentialCellCatalog};
use opto_ir::mapped::{AppliedRegionDelta, CellId, NetId, RegionDelta, TempCellId};
use opto_ir::word;
use std::collections::BTreeSet;

/// Immutable topology for every live register and latch in one lowered design.
///
/// Existing nets are explicit and sorted.  Nets introduced solely to adapt a
/// library cell's enable polarity stay artifact-local until a caller appends
/// the artifact to its transaction.
#[derive(Debug, Clone)]
pub(crate) struct MappedSequentialArtifact {
    cells: Box<[ArtifactCell<MappedCellSource>]>,
    internal_net_count: usize,
    referenced_nets: Box<[NetId]>,
}

/// Delta-local sequential identities retained until mapped/timing commit.
#[derive(Debug)]
pub(crate) struct PendingMappedSequential {
    cells: Box<[(TempCellId, MappedCellSource)]>,
}

/// One live state operation and its owner frozen before bit lowering.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FrozenSequentialOperation {
    operation: word::OpId,
    owner: crate::RegionRowId,
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
pub(crate) fn frozen_sequential_operations(
    module: &word::WordModule,
    operation_regions: &[Option<crate::RegionRowId>],
) -> Result<Box<[FrozenSequentialOperation]>, crate::SynthError> {
    if operation_regions.len() != module.operations().len() {
        return Err(crate::SynthError::invariant(
            "frozen sequential projection does not cover the Word operation arena",
        ));
    }
    let mut operations = Vec::new();
    for (index, operation) in module.operations().iter().enumerate() {
        if matches!(
            operation.kind,
            word::OpKind::Register(_) | word::OpKind::Latch(_)
        ) && let Some(owner) = operation_regions[index]
        {
            operations.push(FrozenSequentialOperation {
                operation: word::OpId::from_index(index).map_err(crate::SynthError::Word)?,
                owner,
            });
        }
    }
    Ok(operations.into_boxed_slice())
}

/// Resolves frozen source state operations to the scalar state operations
/// emitted by global bit lowering while preserving their frozen region owner.
pub(crate) fn lowered_sequential_operations(
    module: &word::WordModule,
    ownership: &crate::boolean::bitblast::LoweredRegionOwnership,
    source_operations: &[FrozenSequentialOperation],
) -> Result<Box<[FrozenSequentialOperation]>, crate::SynthError> {
    let mut lowered = std::collections::BTreeMap::new();
    for frozen in source_operations {
        let source = module.operation(frozen.operation).ok_or_else(|| {
            crate::SynthError::invariant("frozen source sequential operation disappeared")
        })?;
        let values = ownership.lowered_bits(source.result).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "frozen sequential operation {:?} has no lowered state values",
                frozen.operation
            ))
        })?;
        for &value in values {
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
            if lowered
                .insert(operation, frozen.owner)
                .is_some_and(|owner| owner != frozen.owner)
            {
                return Err(crate::SynthError::invariant(
                    "one lowered sequential operation has conflicting frozen owners",
                ));
            }
        }
    }
    Ok(lowered
        .into_iter()
        .map(|(operation, owner)| FrozenSequentialOperation { operation, owner })
        .collect())
}

/// Returns every value needed to materialize a frozen sequential operation set.
pub(crate) fn sequential_binding_values(
    module: &word::WordModule,
    operations: &[FrozenSequentialOperation],
) -> Result<Box<[word::ValueId]>, crate::SynthError> {
    let mut values = BTreeSet::new();
    for frozen in operations {
        let operation = module
            .operation(frozen.operation)
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
        regions: &crate::SynthesisRegionGraph,
        operations: &[FrozenSequentialOperation],
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
        for frozen in operations {
            let operation_id = frozen.operation;
            let operation = module.operation(operation_id).ok_or_else(|| {
                crate::SynthError::invariant("live sequential operation disappeared")
            })?;
            require_scalar(module, operation.result, "sequential result")?;
            let owner = regions
                .region(frozen.owner)
                .map(|region| region.id())
                .ok_or_else(|| {
                    crate::SynthError::invariant(format!(
                        "sequential artifact for {operation_id:?} at {:?} references unknown frozen region {:?}",
                        operation.source, frozen.owner
                    ))
                })?;
            match &operation.kind {
                word::OpKind::Register(register) => {
                    artifact.push_library_register(
                        &library,
                        operation_id,
                        operation.result,
                        owner,
                        register,
                    )?;
                }
                word::OpKind::Latch(latch) => {
                    artifact.push_library_latch(
                        &library,
                        operation_id,
                        operation.result,
                        owner,
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
            self.internal_net_count,
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
    internal_net_count: usize,
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
            internal_net_count: 0,
        })
    }

    fn push_library_register(
        &mut self,
        context: &LibraryContext<'_>,
        operation_id: word::OpId,
        result: word::ValueId,
        owner: crate::RegionAnchorId,
        register: &word::RegisterOp,
    ) -> Result<(), crate::SynthError> {
        let output = require_output(context.mapped_values, result)?;
        let source = MappedCellSource::Value {
            value: result,
            owner,
        };
        if let Some(enable) = register.enable {
            let enable_signal = context.mapped_values.require(enable.value)?;
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
            let enable_signal = self.adapt_enable(
                operation_id,
                enable.value,
                owner,
                enable_signal,
                enable.active_high != cell.enable_active_high(),
                context,
            )?;
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
            return self.push_library_cell(
                context,
                name,
                mapped,
                &[(1, enable_signal)],
                &[(0, output)],
                source,
            );
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
        owner: crate::RegionAnchorId,
        latch: &word::LatchOp,
    ) -> Result<(), crate::SynthError> {
        let reset_requests =
            crate::mapping::sequential::async_reset_requests(context.module, &latch.resets)?;
        let enable_signal = context.mapped_values.require(latch.enable.value)?;
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
        let enable_signal = self.adapt_enable(
            operation_id,
            latch.enable.value,
            owner,
            enable_signal,
            latch.enable.active_high != cell.enable_active_high(),
            context,
        )?;
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
        self.push_library_cell(
            context,
            name,
            mapped,
            &[(1, enable_signal)],
            &[(0, require_output(context.mapped_values, result)?)],
            MappedCellSource::Value {
                value: result,
                owner,
            },
        )
    }

    fn adapt_enable(
        &mut self,
        operation_id: word::OpId,
        value: word::ValueId,
        owner: crate::RegionAnchorId,
        signal: MappedValueSignal,
        invert: bool,
        context: &LibraryContext<'_>,
    ) -> Result<ArtifactSignal, crate::SynthError> {
        if !invert {
            return Ok(ArtifactSignal::Mapped(signal));
        }
        if let MappedValueSignal::Constant(value) = signal {
            return Ok(ArtifactSignal::Mapped(MappedValueSignal::Constant(!value)));
        }
        let binding = context
            .combinational_catalog
            .best_binding_for_truth(crate::boolean::logic::inverter_truth())
            .ok_or_else(|| crate::SynthError::mapping("target library has no inverter"))?;
        let signature = crate::boolean::logic::LogicSignature {
            inputs: crate::boolean::logic::LogicInputs::from_slice(&[value])
                .expect("one inverter input fits a logic signature"),
            truth: crate::boolean::logic::inverter_truth(),
        };
        let mapped = context
            .combinational_catalog
            .cell_for_binding(binding, &signature, value);
        let internal = self.allocate_internal_net()?;
        let name = self.name(operation_id, "_enable_inv");
        self.push_library_cell(
            context,
            name,
            mapped,
            &[],
            &[(0, internal)],
            MappedCellSource::Value { value, owner },
        )?;
        Ok(internal)
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
            let signal = override_at(input_overrides, index).map_or_else(
                || {
                    context
                        .mapped_values
                        .require(connection.value)
                        .map(ArtifactSignal::Mapped)
                },
                Ok,
            )?;
            connections.push((
                connection.pin.clone(),
                Some(target_pin_id(target, &connection.pin)?),
                signal,
            ));
        }
        for (index, connection) in mapped.output_connections.iter().enumerate() {
            let signal = override_at(output_overrides, index).map_or_else(
                || require_output(context.mapped_values, connection.value),
                Ok,
            )?;
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

    fn allocate_internal_net(&mut self) -> Result<ArtifactSignal, crate::SynthError> {
        let index = self.internal_net_count;
        self.internal_net_count = self
            .internal_net_count
            .checked_add(1)
            .ok_or_else(|| crate::SynthError::capacity("sequential internal nets"))?;
        Ok(ArtifactSignal::LocalNet(index))
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
        let mut referenced_nets = self
            .cells
            .iter()
            .flat_map(|cell| cell.connections.iter())
            .filter_map(|(_, _, signal)| match signal {
                ArtifactSignal::Mapped(MappedValueSignal::Net(net)) => Some(*net),
                ArtifactSignal::Mapped(MappedValueSignal::Constant(_))
                | ArtifactSignal::LocalNet(_) => None,
            })
            .collect::<Vec<_>>();
        referenced_nets.sort_unstable();
        referenced_nets.dedup();
        MappedSequentialArtifact {
            cells: self.cells.into_boxed_slice(),
            internal_net_count: self.internal_net_count,
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
    mapped_values: &WordMappedSignals,
    value: word::ValueId,
) -> Result<ArtifactSignal, crate::SynthError> {
    match mapped_values.require(value)? {
        MappedValueSignal::Net(net) => Ok(ArtifactSignal::Mapped(MappedValueSignal::Net(net))),
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
    fn frozen_state_owner_follows_the_scalar_operation_emitted_by_bit_lowering() {
        let mut module = word::WordModule::new("frozen_state_owner");
        let bit = WordType::bits(1).unwrap();
        let clock = module
            .add_port("clock", PortDirection::Input, bit, SourceSpan::default())
            .unwrap();
        let data = module
            .add_port("data", PortDirection::Input, bit, SourceSpan::default())
            .unwrap();
        let output = module
            .add_port("q", PortDirection::Output, bit, SourceSpan::default())
            .unwrap();
        let clock = module
            .read_signal(module.port(clock).unwrap().signal, SourceSpan::default())
            .unwrap();
        let data = module
            .read_signal(module.port(data).unwrap().signal, SourceSpan::default())
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
                SourceSpan::default(),
            )
            .unwrap();
        module
            .connect(
                LValue::signal(module.port(output).unwrap().signal),
                state,
                SourceSpan::default(),
            )
            .unwrap();
        let owner = crate::RegionRowId::from_index(0).unwrap();
        let source_operations = frozen_sequential_operations(&module, &[Some(owner)]).unwrap();
        let required = sequential_binding_values(&module, &source_operations).unwrap();
        let shell = crate::planning::operator::ArchitectureDecisions::for_regional_shell(&module);
        let mut provenance =
            crate::artifact::provenance::ProvenanceBuilder::new(&module, &shell).unwrap();

        let ownership = crate::boolean::bitblast::bitblast_module_with_regions(
            &mut module,
            &shell,
            &mut provenance,
            &[Some(owner)],
            &required,
            &[],
            crate::boolean::bitblast::GlobalBitblastScope::RegionalShell,
        )
        .unwrap();
        let lowered =
            lowered_sequential_operations(&module, &ownership, &source_operations).unwrap();

        assert_eq!(lowered.len(), 1);
        assert_eq!(lowered[0].owner, owner);
        assert_ne!(lowered[0].operation, source_operations[0].operation);
        assert!(matches!(
            module.operation(lowered[0].operation).unwrap().kind,
            word::OpKind::Register(_)
        ));
    }
}
