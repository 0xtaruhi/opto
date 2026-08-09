// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Direct materialization of live state elements into the mapped substrate.
//!
//! Sequential cells are immutable state-region substrate objects: regional
//! epochs replace only combinational covers. This module therefore prepares one
//! deterministic delta from the lowered Word model without first emitting
//! target instances back into that model.

use super::region_delta::{MappedValueSignal, WordMappedSignals};
use super::target_pin_id;
use crate::artifact::MappedCellSource;
use crate::mapping::MappedCell;
use crate::mapping::library::CombinationalCellCatalog;
use crate::mapping::sequential::{AsyncResetRequest, SequentialCellCatalog};
use crate::planning::mapping_policy::CellCost;
use opto_ir::mapped::{
    AppliedRegionDelta, CellId, CellSpec, ConnectionRef, NetId, RegionDelta, TempCellId, TempNetId,
};
use opto_ir::word;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArtifactSignal {
    Mapped(MappedValueSignal),
    Internal(usize),
}

impl ArtifactSignal {
    fn connection(self, internal_nets: &[TempNetId]) -> Result<ConnectionRef, crate::SynthError> {
        match self {
            Self::Mapped(signal) => Ok(signal.connection()),
            Self::Internal(index) => internal_nets
                .get(index)
                .copied()
                .map(ConnectionRef::NewNet)
                .ok_or_else(|| {
                    crate::SynthError::invariant(
                        "sequential artifact references an unknown internal net",
                    )
                }),
        }
    }
}

#[derive(Debug, Clone)]
struct ArtifactCell {
    name: String,
    cell_type: String,
    library_cell: Option<u32>,
    connections: Box<[(String, Option<u16>, ArtifactSignal)]>,
    source: MappedCellSource,
}

/// Immutable topology for every live register and latch in one lowered design.
///
/// Existing nets are explicit and sorted.  Nets introduced solely to adapt a
/// library cell's enable polarity stay artifact-local until a caller appends
/// the artifact to its transaction.
#[derive(Debug, Clone)]
pub(crate) struct MappedSequentialArtifact {
    cells: Box<[ArtifactCell]>,
    internal_net_count: usize,
    referenced_nets: Box<[NetId]>,
}

/// Delta-local sequential identities retained until mapped/timing commit.
#[derive(Debug)]
pub(crate) struct PendingMappedSequential {
    cells: Box<[(TempCellId, MappedCellSource)]>,
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

/// Returns the exact lowered values the one-time substrate must resolve before
/// [`MappedSequentialArtifact::from_module`] is called.
pub(crate) fn sequential_binding_values(
    module: &word::WordModule,
) -> Result<Box<[word::ValueId]>, crate::SynthError> {
    let mut values = BTreeSet::new();
    for operation in live_sequential_operations(module)? {
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
        regions: &crate::SynthesisRegionGraph,
        ownership: &crate::boolean::bitblast::LoweredRegionOwnership,
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
        for operation_id in live_sequential_operations(module)? {
            let operation = module.operation(operation_id).ok_or_else(|| {
                crate::SynthError::invariant("live sequential operation disappeared")
            })?;
            require_scalar(module, operation.result, "sequential result")?;
            let owner = ownership
                .owner(operation.result)
                .and_then(|row| regions.region(row))
                .map(|region| region.id())
                .ok_or_else(|| {
                    crate::SynthError::invariant(
                        "sequential artifact has no lowered synthesis-region owner",
                    )
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
        let internal_nets = (0..self.internal_net_count)
            .map(|_| delta.add_net(None).map_err(crate::SynthError::from))
            .collect::<Result<Box<[_]>, _>>()?;
        let cells = self
            .cells
            .iter()
            .map(|cell| {
                let mut spec =
                    CellSpec::new(cell.name.clone(), cell.cell_type.clone(), cell.library_cell);
                for (pin, library_pin, signal) in &cell.connections {
                    spec = spec.connect(
                        pin.clone(),
                        *library_pin,
                        signal.connection(&internal_nets)?,
                    );
                }
                delta
                    .add_cell(spec)
                    .map(|cell_id| (cell_id, cell.source))
                    .map_err(crate::SynthError::from)
            })
            .collect::<Result<Box<[_]>, _>>()?;
        Ok(PendingMappedSequential { cells })
    }
}

struct ArtifactBuilder<'a> {
    module: &'a word::WordModule,
    state_targets: Vec<Option<word::LValue>>,
    cells: Vec<ArtifactCell>,
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
        require_async_resets(&register.resets, "register")?;
        let reset_requests = reset_requests(context.module, &register.resets)?;
        let output = require_output(context.mapped_values, result)?;
        let source = MappedCellSource::Value {
            value: result,
            owner,
        };
        if let Some(enable) = register.enable {
            let enable_signal = context.mapped_values.require(enable.value)?;
            let inverter_cost = inverter_cost(enable_signal, context.combinational_catalog);
            let cell = context
                .sequential_catalog
                .best_enable(
                    register.edge,
                    &reset_requests,
                    enable.active_high,
                    false,
                    inverter_cost,
                )
                .ok_or_else(|| {
                    crate::SynthError::mapping("target library has no compatible enabled DFF")
                })?;
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
        let cell = context
            .sequential_catalog
            .best(register.edge, &reset_requests, false, None)
            .ok_or_else(|| crate::SynthError::mapping("target library has no compatible DFF"))?;
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
        require_async_resets(&latch.resets, "latch")?;
        let reset_requests = reset_requests(context.module, &latch.resets)?;
        let enable_signal = context.mapped_values.require(latch.enable.value)?;
        let cell = context
            .sequential_catalog
            .best_latch(
                &reset_requests,
                latch.enable.active_high,
                false,
                inverter_cost(enable_signal, context.combinational_catalog),
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
            source,
        });
        Ok(())
    }

    fn allocate_internal_net(&mut self) -> Result<ArtifactSignal, crate::SynthError> {
        let index = self.internal_net_count;
        self.internal_net_count = self
            .internal_net_count
            .checked_add(1)
            .ok_or_else(|| crate::SynthError::capacity("sequential internal nets"))?;
        Ok(ArtifactSignal::Internal(index))
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
            .flat_map(|cell| &cell.connections)
            .filter_map(|(_, _, signal)| match signal {
                ArtifactSignal::Mapped(MappedValueSignal::Net(net)) => Some(*net),
                ArtifactSignal::Mapped(MappedValueSignal::Constant(_))
                | ArtifactSignal::Internal(_) => None,
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

fn live_sequential_operations(
    module: &word::WordModule,
) -> Result<Box<[word::OpId]>, crate::SynthError> {
    let live = crate::mapping::word_util::live_operation_mask(module, &[])?;
    module
        .operations()
        .iter()
        .enumerate()
        .filter(|(index, operation)| {
            live[*index]
                && matches!(
                    operation.kind,
                    word::OpKind::Register(_) | word::OpKind::Latch(_)
                )
        })
        .map(|(index, _)| word::OpId::from_index(index).map_err(crate::SynthError::Word))
        .collect::<Result<Vec<_>, _>>()
        .map(Vec::into_boxed_slice)
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

fn reset_requests(
    module: &word::WordModule,
    resets: &[word::Reset],
) -> Result<Vec<AsyncResetRequest>, crate::SynthError> {
    resets
        .iter()
        .map(|reset| {
            let stored = module.value(reset.reset_value).ok_or_else(|| {
                crate::SynthError::invariant("asynchronous reset value disappeared")
            })?;
            let word::ValueKind::Constant(bits) = &stored.kind else {
                return Err(crate::SynthError::invariant(
                    "asynchronous reset value is not constant",
                ));
            };
            let reset_value = crate::boolean::logic::logic_constant(bits).ok_or_else(|| {
                crate::SynthError::invariant("asynchronous reset value is not a two-state scalar")
            })?;
            Ok(AsyncResetRequest {
                active_high: reset.active_high,
                reset_value,
            })
        })
        .collect()
}

fn require_async_resets(resets: &[word::Reset], element: &str) -> Result<(), crate::SynthError> {
    if resets
        .iter()
        .any(|reset| reset.kind != word::ResetKind::Async)
    {
        return Err(crate::SynthError::invariant(format!(
            "synchronous reset reached library {element} materialization"
        )));
    }
    Ok(())
}

fn inverter_cost(
    signal: MappedValueSignal,
    catalog: &CombinationalCellCatalog,
) -> Option<CellCost> {
    if matches!(signal, MappedValueSignal::Constant(_)) {
        return Some(CellCost {
            area: 0.0,
            delay: 0.0,
            transition: 0.0,
            input_capacitance: 0.0,
        });
    }
    let signature = crate::boolean::logic::LogicSignature {
        inputs: crate::boolean::logic::LogicInputs::from_indices(1)
            .expect("one inverter input fits a logic signature"),
        truth: crate::boolean::logic::inverter_truth(),
    };
    catalog.best_cost_for_signature(&signature)
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
