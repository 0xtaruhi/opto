// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! State owner and checked construction API for one word-level definition.
//!
//! All names and typed IDs are module-local. Mutating constructors validate
//! operand types and ranges before appending records; whole-module validation
//! remains the publication and checkpoint boundary.

use super::{
    Annotation, AnnotationTarget, AnnotationValue, AnnotationValueSpec, BatchValue, BinaryOp,
    CastKind, Connect, DefinitionKind, InstId, Instance, InstanceConnection, LValue, LatchOp,
    LogicStateKind, Memory, MemoryClock, MemoryId, MemoryReadPort, MemoryReadPortId,
    MemoryReadTiming, MemoryResources, MemoryWritePort, MemoryWritePortId, MuxBatchOperation, OpId,
    OpKind, Operation, PackedInstanceSpec, Port, PortDirection, PortId, RegisterOp, ResetKind,
    Signal, SignalFragment, SignalId, SignalKind, SignalRef, SignalResolution, SourceSpan,
    SynthesisDirective, SynthesisDirectiveKind, TypeLayout, UnaryOp, Value, ValueId, ValueKind,
    WordError, WordType,
};

mod builders;
use crate::value::{BitVal, ConstBits};
use crate::{NameId, NameTable};
use serde::{Deserialize, Serialize};
use std::num::{NonZeroU32, NonZeroU64};
use std::sync::atomic::{AtomicU64, Ordering};

mod instance;
mod rewrite;
mod validation;

static NEXT_SPECULATION_IDENTITY: AtomicU64 = AtomicU64::new(1);

/// Runtime-only owner identity for speculation checkpoints.
///
/// Clones receive a fresh identity, while equality ignores it so structural
/// module comparisons and deterministic serialization remain unchanged.
#[derive(Debug)]
struct SpeculationIdentity(NonZeroU64);

impl SpeculationIdentity {
    fn fresh() -> Self {
        let raw = NEXT_SPECULATION_IDENTITY
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
            .expect("Word module speculation identity space is exhausted");
        Self(NonZeroU64::new(raw).expect("speculation identities start at one"))
    }
}

impl Default for SpeculationIdentity {
    fn default() -> Self {
        Self::fresh()
    }
}

impl Clone for SpeculationIdentity {
    fn clone(&self) -> Self {
        Self::fresh()
    }
}

impl PartialEq for SpeculationIdentity {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for SpeculationIdentity {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Validated word-level definition with dense, owner-local arenas.
///
/// Values and operations form an SSA-like dataflow graph. Signals, ports,
/// memories, continuous connections, and instances remain explicit structural
/// objects. Every typed ID is valid only in the module that created it.
pub struct WordModule {
    pub(super) names: NameTable,
    pub(super) name: NameId,
    pub(super) definition_kind: DefinitionKind,
    pub(super) annotations: Vec<Annotation>,
    pub(super) synthesis_directives: Vec<SynthesisDirective>,
    pub(super) ports: Vec<Port>,
    pub(super) signals: Vec<Signal>,
    pub(super) memories: Vec<Memory>,
    pub(super) memory_read_ports: Vec<MemoryReadPort>,
    pub(super) memory_write_ports: Vec<MemoryWritePort>,
    pub(super) type_layouts: Vec<TypeLayout>,
    pub(super) values: Vec<Value>,
    pub(super) operations: Vec<Operation>,
    pub(super) connects: Vec<Connect>,
    pub(super) instances: Vec<Instance>,
    pub(super) named_signals: Vec<Option<SignalId>>,
    pub(super) named_memories: Vec<Option<MemoryId>>,
    pub(super) named_instances: Vec<Option<InstId>>,
    #[serde(skip, default)]
    speculation_identity: SpeculationIdentity,
}

/// One module's arena boundary, taken before a speculative construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpeculationCheckpoint {
    identity: NonZeroU64,
    names: crate::NameCheckpoint,
    annotations: usize,
    synthesis_directives: usize,
    ports: usize,
    values: usize,
    operations: usize,
    signals: usize,
    memories: usize,
    memory_read_ports: usize,
    memory_write_ports: usize,
    type_layouts: usize,
    connects: usize,
    instances: usize,
    named_signals: usize,
}

fn dense_id<T: Copy>(index: &[Option<T>], name: NameId) -> Option<T> {
    index.get(name.raw() as usize).copied().flatten()
}

fn insert_dense_id<T: Copy>(
    index: &mut Vec<Option<T>>,
    name: NameId,
    id: T,
) -> Result<(), WordError> {
    let slot = name.raw() as usize;
    if index.len() <= slot {
        index.resize(slot + 1, None);
    }
    if index[slot].is_some() {
        return Err(WordError::new("dense name index insertion is not unique"));
    }
    index[slot] = Some(id);
    Ok(())
}

impl WordModule {
    ///
    /// # Panics
    ///
    /// Panics only if the initial name cannot fit in the 32-bit name table.
    ///
    /// # Examples
    ///
    /// ```
    /// use opto_ir::word::WordModule;
    ///
    /// let module = WordModule::new("alu");
    /// assert_eq!(module.name(), "alu");
    /// assert!(module.ports().is_empty());
    /// ```
    pub fn new(name: impl AsRef<str>) -> Self {
        let mut names = NameTable::new();
        let name = names
            .intern(name.as_ref())
            .expect("RTL module name must fit in the name table");
        Self {
            names,
            name,
            definition_kind: DefinitionKind::Synthesizable,
            annotations: Vec::new(),
            synthesis_directives: Vec::new(),
            ports: Vec::new(),
            signals: Vec::new(),
            memories: Vec::new(),
            memory_read_ports: Vec::new(),
            memory_write_ports: Vec::new(),
            type_layouts: Vec::new(),
            values: Vec::new(),
            operations: Vec::new(),
            connects: Vec::new(),
            instances: Vec::new(),
            named_signals: Vec::new(),
            named_memories: Vec::new(),
            named_instances: Vec::new(),
            speculation_identity: SpeculationIdentity::fresh(),
        }
    }

    /// Returns the RTL module name.
    ///
    /// # Panics
    ///
    /// Panics only if the module's private name ID no longer resolves in its
    /// owned table; checked construction and deserialization preserve it.
    #[must_use]
    pub fn name(&self) -> &str {
        self.resolve_name(self.name)
            .expect("RTL module name ID must resolve")
    }

    /// Returns whether this definition contains RTL or is an external black box.
    #[must_use]
    pub fn definition_kind(&self) -> DefinitionKind {
        self.definition_kind
    }

    /// Sets the semantic definition classification.
    pub fn set_definition_kind(&mut self, kind: DefinitionKind) {
        self.definition_kind = kind;
    }

    /// Returns sparse source annotations in deterministic insertion order.
    #[must_use]
    pub fn annotations(&self) -> &[Annotation] {
        &self.annotations
    }

    /// Returns strongly typed synthesis directives in deterministic insertion order.
    #[must_use]
    pub fn synthesis_directives(&self) -> &[SynthesisDirective] {
        &self.synthesis_directives
    }

    /// Returns the explicit value of a typed directive on one object.
    #[must_use]
    pub fn synthesis_directive(
        &self,
        target: AnnotationTarget,
        kind: SynthesisDirectiveKind,
    ) -> Option<bool> {
        self.synthesis_directives
            .iter()
            .find(|directive| directive.target == target && directive.kind == kind)
            .map(|directive| directive.enabled)
    }

    /// Sets or replaces one explicit typed synthesis directive.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] if `target` is unknown or `kind` is incompatible
    /// with that target.
    pub fn set_synthesis_directive(
        &mut self,
        target: AnnotationTarget,
        kind: SynthesisDirectiveKind,
        enabled: bool,
        source: SourceSpan,
    ) -> Result<(), WordError> {
        self.validate_synthesis_directive(target, kind)?;
        if let Some(directive) = self
            .synthesis_directives
            .iter_mut()
            .find(|directive| directive.target == target && directive.kind == kind)
        {
            directive.enabled = enabled;
            directive.source = source;
        } else {
            self.synthesis_directives.push(SynthesisDirective {
                target,
                kind,
                enabled,
                source,
            });
        }
        Ok(())
    }

    /// Returns whether a signal must remain materialized through optimization.
    #[must_use]
    pub fn signal_is_preserved(&self, signal: SignalId) -> bool {
        let target = AnnotationTarget::Signal(signal);
        [
            SynthesisDirectiveKind::DontTouch,
            SynthesisDirectiveKind::KeepSignal,
            SynthesisDirectiveKind::AsyncRegister,
        ]
        .into_iter()
        .any(|kind| self.synthesis_directive(target, kind) == Some(true))
    }

    /// Iterates signal IDs selected by any enabled preservation directive.
    pub fn preserved_signals(&self) -> impl Iterator<Item = SignalId> + '_ {
        self.synthesis_directives.iter().filter_map(|directive| {
            let AnnotationTarget::Signal(signal) = directive.target else {
                return None;
            };
            (directive.enabled
                && matches!(
                    directive.kind,
                    SynthesisDirectiveKind::DontTouch
                        | SynthesisDirectiveKind::KeepSignal
                        | SynthesisDirectiveKind::AsyncRegister
                ))
            .then_some(signal)
        })
    }

    /// Appends one evaluated source annotation and interns its textual payload.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] for an unknown target, empty annotation name,
    /// frozen/full name table, or a textual payload that cannot be interned.
    pub fn add_annotation(
        &mut self,
        target: AnnotationTarget,
        name: impl AsRef<str>,
        value: AnnotationValueSpec,
        source: SourceSpan,
    ) -> Result<(), WordError> {
        self.validate_annotation_target(target)?;
        let name = name.as_ref();
        if name.is_empty() {
            return Err(WordError::new("annotation name cannot be empty"));
        }
        let name = self.names.intern(name)?;
        let value = match value {
            AnnotationValueSpec::Integer { bits, signed } => {
                if bits.width() == 0 {
                    return Err(WordError::new("annotation integer cannot have zero width"));
                }
                let width = bits.width();
                let bits = self.names.intern(&bits.to_string())?;
                AnnotationValue::Integer {
                    bits,
                    width,
                    signed,
                }
            }
            AnnotationValueSpec::String(value) => {
                AnnotationValue::String(self.names.intern(&value)?)
            }
            AnnotationValueSpec::Other(value) => AnnotationValue::Other(self.names.intern(&value)?),
        };
        self.annotations.push(Annotation {
            target,
            name,
            value,
            source,
        });
        Ok(())
    }

    pub(super) fn validate_annotation_target(
        &self,
        target: AnnotationTarget,
    ) -> Result<(), WordError> {
        let valid = match target {
            AnnotationTarget::Module => true,
            AnnotationTarget::Port(id) => id.index() < self.ports.len(),
            AnnotationTarget::Signal(id) => id.index() < self.signals.len(),
            AnnotationTarget::Memory(id) => id.index() < self.memories.len(),
            AnnotationTarget::MemoryReadPort(id) => id.index() < self.memory_read_ports.len(),
            AnnotationTarget::MemoryWritePort(id) => id.index() < self.memory_write_ports.len(),
            AnnotationTarget::Value(id) => id.index() < self.values.len(),
            AnnotationTarget::Operation(id) => id.index() < self.operations.len(),
            AnnotationTarget::Instance(id) => id.index() < self.instances.len(),
        };
        if valid {
            Ok(())
        } else {
            Err(WordError::new(
                "annotation target is not owned by this module",
            ))
        }
    }

    pub(super) fn validate_synthesis_directive(
        &self,
        target: AnnotationTarget,
        kind: SynthesisDirectiveKind,
    ) -> Result<(), WordError> {
        self.validate_annotation_target(target)?;
        let valid = match kind {
            SynthesisDirectiveKind::DontTouch => matches!(
                target,
                AnnotationTarget::Module
                    | AnnotationTarget::Signal(_)
                    | AnnotationTarget::Instance(_)
            ),
            SynthesisDirectiveKind::Ungroup => {
                matches!(
                    target,
                    AnnotationTarget::Module | AnnotationTarget::Instance(_)
                )
            }
            SynthesisDirectiveKind::KeepSignal | SynthesisDirectiveKind::AsyncRegister => {
                matches!(target, AnnotationTarget::Signal(_))
            }
        };
        if valid {
            Ok(())
        } else {
            Err(WordError::new(format!(
                "synthesis directive {kind:?} is not valid on {target:?}"
            )))
        }
    }

    /// Replaces the definition name without changing object identities.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] when `name` is empty or the name table is at capacity.
    pub fn rename(&mut self, name: impl AsRef<str>) -> Result<(), WordError> {
        let name = name.as_ref();
        if name.trim().is_empty() {
            return Err(WordError::new("RTL module name cannot be empty"));
        }
        self.name = self.names.intern(name)?;
        Ok(())
    }

    /// Resolves a module-local interned name.
    #[must_use]
    pub fn resolve_name(&self, id: NameId) -> Option<&str> {
        self.names.resolve(id)
    }

    /// Resolves a name ID known to belong to this module.
    ///
    /// # Panics
    ///
    /// Panics when `id` is foreign to this module.
    #[must_use]
    pub fn name_str(&self, id: NameId) -> &str {
        self.resolve_name(id)
            .expect("RTL name ID must resolve in its owning module")
    }

    /// Consolidates mutable names into immutable shared storage.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] if the underlying compact table cannot be rebuilt.
    pub fn consolidate_names(&mut self) -> Result<(), WordError> {
        self.names.freeze().map_err(Into::into)
    }

    /// Returns the number of user-visible interned names.
    #[must_use]
    pub fn name_count(&self) -> usize {
        self.names.entry_count() - 1
    }

    /// Returns bytes occupied by interned string contents.
    #[must_use]
    pub fn name_storage_bytes(&self) -> usize {
        self.names.stored_bytes()
    }

    /// Returns the module-local interned-name table.
    #[must_use]
    pub fn name_table(&self) -> &NameTable {
        &self.names
    }

    /// Interns a module-local name.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] when the table is at capacity.
    pub fn intern_name(&mut self, name: impl AsRef<str>) -> Result<NameId, WordError> {
        self.names.intern(name.as_ref()).map_err(Into::into)
    }

    /// Returns ports in stable insertion order.
    #[must_use]
    pub fn ports(&self) -> &[Port] {
        &self.ports
    }

    /// Returns signals in dense ID order.
    #[must_use]
    pub fn signals(&self) -> &[Signal] {
        &self.signals
    }

    /// Returns memory declarations in dense ID order.
    #[must_use]
    pub fn memories(&self) -> &[Memory] {
        &self.memories
    }

    /// Returns memory read ports in stable insertion order.
    #[must_use]
    pub fn memory_read_ports(&self) -> &[MemoryReadPort] {
        &self.memory_read_ports
    }

    /// Retargets one memory read port to an equivalent procedural address.
    ///
    /// This operation is used while procedural SSA replaces activation-local
    /// address operands. The port identity and data signal remain stable.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] for an unknown port or an address whose type is
    /// incompatible with the referenced memory.
    pub fn set_memory_read_port_address(
        &mut self,
        port: MemoryReadPortId,
        address: ValueId,
    ) -> Result<(), WordError> {
        let mut candidate = self
            .memory_read_ports
            .get(port.index())
            .cloned()
            .ok_or_else(|| WordError::new(format!("unknown memory read port {port:?}")))?;
        candidate.address = address;
        self.validate_memory_read_port(&candidate, Some(port.index()))?;
        self.memory_read_ports[port.index()].address = address;
        Ok(())
    }

    /// Returns memory write ports in stable insertion order.
    #[must_use]
    pub fn memory_write_ports(&self) -> &[MemoryWritePort] {
        &self.memory_write_ports
    }

    /// Atomically transfers every first-class memory resource to resource
    /// inference. The remaining module contains no memory identity or port
    /// that a structural lowering pass could silently ignore.
    pub fn take_memory_resources(&mut self) -> MemoryResources {
        self.named_memories.clear();
        MemoryResources {
            memories: std::mem::take(&mut self.memories),
            reads: std::mem::take(&mut self.memory_read_ports),
            writes: std::mem::take(&mut self.memory_write_ports),
        }
    }

    /// Returns values in dense ID order.
    #[must_use]
    pub fn values(&self) -> &[Value] {
        &self.values
    }

    /// Returns operations in dense ID order.
    #[must_use]
    pub fn operations(&self) -> &[Operation] {
        &self.operations
    }

    /// Returns continuous structural assignments in insertion order.
    #[must_use]
    pub fn connects(&self) -> &[Connect] {
        &self.connects
    }

    /// Returns child instances in dense ID order.
    #[must_use]
    pub fn instances(&self) -> &[Instance] {
        &self.instances
    }

    /// Adds a named port and its uniquely associated signal.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] for an empty or duplicate name, a name-table or
    /// arena capacity failure, or a collision with an existing memory.
    ///
    /// # Panics
    ///
    /// Panics only if the successfully inserted named signal immediately loses
    /// its name; the insertion helper establishes that private invariant.
    pub fn add_port(
        &mut self,
        name: impl AsRef<str>,
        direction: PortDirection,
        ty: WordType,
        source: SourceSpan,
    ) -> Result<PortId, WordError> {
        let port_id = PortId::from_index(self.ports.len())?;
        let signal =
            self.add_named_signal(name.as_ref(), SignalKind::Port(port_id), ty, source.clone())?;
        let name = self
            .signal(signal)
            .and_then(|signal| signal.name)
            .expect("a named port signal must retain its name");
        self.ports.push(Port {
            name,
            direction,
            signal,
            ty,
            source,
        });
        Ok(port_id)
    }

    /// Adds a named continuously driven signal.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] for an invalid or duplicate name, a name collision,
    /// or a capacity failure.
    pub fn add_wire(
        &mut self,
        name: impl AsRef<str>,
        ty: WordType,
        source: SourceSpan,
    ) -> Result<SignalId, WordError> {
        self.add_named_signal(name.as_ref(), SignalKind::Wire, ty, source)
    }

    /// Adds an unnamed wire for a generated implementation detail.
    ///
    /// Unlike a named signal, this remains available after the source name
    /// table has been frozen and does not become a user-visible design object.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] when the signal arena is at capacity.
    pub fn add_generated_wire(
        &mut self,
        ty: WordType,
        source: SourceSpan,
    ) -> Result<SignalId, WordError> {
        let id = SignalId::from_index(self.signals.len())?;
        self.signals.push(Signal {
            name: None,
            kind: SignalKind::Wire,
            ty,
            resolution: SignalResolution::SingleDriver,
            type_layout: None,
            source,
        });
        Ok(id)
    }

    /// Reserves dense arena space for a known batch of generated operations.
    ///
    /// Each operation owns exactly one result value, so reserving both arenas
    /// together prevents their capacities from drifting during deterministic
    /// bulk implementation emission.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] if either arena cannot reserve the requested
    /// additional capacity.
    pub fn reserve_generated_operations(&mut self, additional: usize) -> Result<(), WordError> {
        self.values.try_reserve(additional).map_err(|error| {
            WordError::new(format!("cannot reserve generated value arena: {error}"))
        })?;
        self.operations.try_reserve(additional).map_err(|error| {
            WordError::new(format!("cannot reserve generated operation arena: {error}"))
        })?;
        Ok(())
    }

    /// Appends a preplanned scalar mux DAG as one deterministic arena batch.
    ///
    /// Generated operands may reference only earlier rows in `operations`.
    /// The complete batch, arena capacities, and dense-ID range are preflighted
    /// before the first row is committed.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] for an unknown or forward operand, a non-scalar
    /// condition, incompatible branch types, or typed-ID capacity exhaustion.
    ///
    /// # Panics
    ///
    /// Panics only if internal batch preflight and commit invariants diverge.
    /// The method owns the batch and exclusively borrows both dense arenas, so
    /// safe callers cannot trigger this condition.
    pub fn append_mux_batch(
        &mut self,
        operations: Vec<MuxBatchOperation>,
    ) -> Result<Box<[ValueId]>, WordError> {
        let value_base = self.values.len();
        let operation_base = self.operations.len();
        let final_value_count = value_base
            .checked_add(operations.len())
            .ok_or_else(|| WordError::new("generated value arena length overflow"))?;
        let final_operation_count = operation_base
            .checked_add(operations.len())
            .ok_or_else(|| WordError::new("generated operation arena length overflow"))?;
        if final_value_count > value_base {
            let _ = ValueId::from_index(final_value_count - 1)?;
            let _ = OpId::from_index(final_operation_count - 1)?;
        }

        let mut result_types = Vec::new();
        result_types
            .try_reserve_exact(operations.len())
            .map_err(|error| {
                WordError::new(format!("cannot reserve batch type preflight: {error}"))
            })?;
        let type_of = |reference: BatchValue,
                       row: usize,
                       generated: &[WordType]|
         -> Result<WordType, WordError> {
            match reference {
                BatchValue::Existing(value) => self.value_ty(value),
                BatchValue::Generated(ordinal) => {
                    let ordinal = ordinal as usize;
                    if ordinal >= row {
                        return Err(WordError::new(format!(
                            "generated batch operand {ordinal} is not earlier than row {row}"
                        )));
                    }
                    generated.get(ordinal).copied().ok_or_else(|| {
                        WordError::new(format!(
                            "generated batch operand {ordinal} has no preflighted type"
                        ))
                    })
                }
            }
        };
        for (row, operation) in operations.iter().enumerate() {
            if type_of(operation.cond, row, &result_types)?.width() != 1 {
                return Err(WordError::new("batch mux condition is not scalar"));
            }
            let ty = type_of(operation.then_value, row, &result_types)?;
            if type_of(operation.else_value, row, &result_types)? != ty {
                return Err(WordError::new("batch mux branch types differ"));
            }
            result_types.push(ty);
        }

        let mut results = Vec::new();
        results
            .try_reserve_exact(operations.len())
            .map_err(|error| WordError::new(format!("cannot reserve batch result IDs: {error}")))?;
        for row in 0..operations.len() {
            results.push(
                ValueId::from_index(value_base + row)
                    .expect("preflighted generated value ID must fit"),
            );
        }
        self.reserve_generated_operations(operations.len())?;
        let resolve = |reference: BatchValue| match reference {
            BatchValue::Existing(value) => value,
            BatchValue::Generated(ordinal) => results[ordinal as usize],
        };
        for (row, (operation, ty)) in operations.into_iter().zip(result_types).enumerate() {
            let operation_id = OpId::from_index(operation_base + row)
                .expect("preflighted generated operation ID must fit");
            let result = results[row];
            let source = operation.source;
            self.values.push(Value {
                kind: ValueKind::Operation(operation_id),
                ty,
                source: source.clone(),
            });
            self.operations.push(Operation {
                kind: OpKind::Mux {
                    cond: resolve(operation.cond),
                    then_value: resolve(operation.then_value),
                    else_value: resolve(operation.else_value),
                },
                result,
                source,
            });
        }
        Ok(results.into_boxed_slice())
    }

    /// Adds a named procedurally assigned signal.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] for an invalid or duplicate name, a name collision,
    /// or a capacity failure.
    pub fn add_register_signal(
        &mut self,
        name: impl AsRef<str>,
        ty: WordType,
        source: SourceSpan,
    ) -> Result<SignalId, WordError> {
        self.add_named_signal(name.as_ref(), SignalKind::Register, ty, source)
    }

    /// Adds a temporary signal used during procedural normalization.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] for an invalid or duplicate name, a name collision,
    /// or a capacity failure.
    pub fn add_process_local_signal(
        &mut self,
        name: impl AsRef<str>,
        ty: WordType,
        source: SourceSpan,
    ) -> Result<SignalId, WordError> {
        self.add_named_signal(name.as_ref(), SignalKind::ProcessLocal, ty, source)
    }

    /// Adds a technology-independent memory declaration.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] when the name is empty, duplicated, conflicts with
    /// a signal, cannot be interned, or the memory arena is at capacity.
    pub fn add_memory(
        &mut self,
        name: impl AsRef<str>,
        element_type: WordType,
        depth: NonZeroU32,
        source: SourceSpan,
    ) -> Result<MemoryId, WordError> {
        let name = name.as_ref();
        if name.trim().is_empty() {
            return Err(WordError::new("RTL memory name cannot be empty"));
        }
        if let Some(id) = self.names.get(name) {
            if dense_id(&self.named_memories, id).is_some() {
                return Err(WordError::new(format!(
                    "duplicate RTL memory name '{name}'"
                )));
            }
            if dense_id(&self.named_signals, id).is_some() {
                return Err(WordError::new(format!(
                    "RTL memory name '{name}' conflicts with a signal"
                )));
            }
        }
        let name = self.names.intern(name)?;
        let id = MemoryId::from_index(self.memories.len())?;
        self.memories.push(Memory {
            name,
            element_type,
            depth,
            source,
        });
        insert_dense_id(&mut self.named_memories, name, id)?;
        Ok(id)
    }

    /// Validates and appends one memory read port.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] when any referenced ID is invalid or the address,
    /// data, clock, enable, or collision policy is incompatible with the memory.
    pub fn add_memory_read_port(
        &mut self,
        port: MemoryReadPort,
    ) -> Result<MemoryReadPortId, WordError> {
        self.validate_memory_read_port(&port, None)?;
        let id = MemoryReadPortId::from_index(self.memory_read_ports.len())?;
        self.memory_read_ports.push(port);
        Ok(id)
    }

    /// Validates and appends one memory write port.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] when any referenced ID is invalid or the address,
    /// data, clock, enable, mask, or width is incompatible with the memory.
    pub fn add_memory_write_port(
        &mut self,
        port: MemoryWritePort,
    ) -> Result<MemoryWritePortId, WordError> {
        self.validate_memory_write_port(&port, None)?;
        let id = MemoryWritePortId::from_index(self.memory_write_ports.len())?;
        self.memory_write_ports.push(port);
        Ok(id)
    }

    /// Changes the driver-resolution policy of a signal.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] for a foreign signal ID or when wired resolution
    /// is requested for a register or process-local signal.
    pub fn set_signal_resolution(
        &mut self,
        signal: SignalId,
        resolution: SignalResolution,
    ) -> Result<(), WordError> {
        let signal = self
            .signals
            .get_mut(signal.index())
            .ok_or_else(|| WordError::new(format!("unknown RTL signal {signal:?}")))?;
        if resolution != SignalResolution::SingleDriver
            && matches!(signal.kind, SignalKind::Register | SignalKind::ProcessLocal)
        {
            return Err(WordError::new(
                "wired resolution is valid only for nets and ports",
            ));
        }
        signal.resolution = resolution;
        Ok(())
    }

    ///
    /// # Errors
    ///
    /// Returns [`WordError`] when `signal` is foreign or the value arena is at
    /// capacity.
    pub fn read_signal(
        &mut self,
        signal: SignalId,
        source: SourceSpan,
    ) -> Result<ValueId, WordError> {
        let width = self.signal_ty(signal)?.width();
        self.read_signal_slice(signal, 0, width, source)
    }

    /// Creates a value that reads a static signal slice.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] when the signal is foreign, `width` is zero, the
    /// selected range exceeds the signal, or the value arena is at capacity.
    pub fn read_signal_slice(
        &mut self,
        signal: SignalId,
        lsb: u32,
        width: u32,
        source: SourceSpan,
    ) -> Result<ValueId, WordError> {
        let signal_ty = self.signal_ty(signal)?;
        let width = NonZeroU32::new(width)
            .ok_or_else(|| WordError::new("signal reference width must be non-zero"))?;
        let end = lsb
            .checked_add(width.get())
            .ok_or_else(|| WordError::new("signal reference range exceeds 32-bit capacity"))?;
        if end > signal_ty.width() {
            return Err(WordError::new(format!(
                "signal reference [{} +: {}] exceeds signal width {}",
                lsb,
                width.get(),
                signal_ty.width()
            )));
        }
        let ty = if lsb == 0 && width.get() == signal_ty.width() {
            signal_ty
        } else {
            WordType::new(width.get(), false, signal_ty.state())?
        };
        self.push_value(
            ValueKind::Signal(SignalRef { signal, lsb, width }),
            ty,
            source,
        )
    }

    /// Adds a type-checked continuous structural assignment.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] for invalid IDs or ranges, type mismatch, or an
    /// attempt to add a second structural driver to memory read data.
    pub fn connect(
        &mut self,
        target: LValue,
        value: ValueId,
        source: SourceSpan,
    ) -> Result<(), WordError> {
        if self
            .memory_read_ports
            .iter()
            .any(|port| port.data == target.signal)
        {
            return Err(WordError::new(
                "memory read data signal cannot have another structural driver",
            ));
        }
        let target_ty = self.lvalue_ty(&target)?;
        let value_ty = self.value_ty(value)?;
        if target_ty != value_ty {
            return Err(WordError::new(format!(
                "connection type mismatch: target {target_ty:?}, value {value_ty:?}"
            )));
        }
        self.connects.push(Connect {
            target,
            value,
            source,
        });
        Ok(())
    }

    /// Records the arena boundary a speculative construction starts from.
    ///
    /// A pass that has to build an expression before it can decide whether to
    /// keep it takes one of these first and rolls back on every path that
    /// declines. See [`WordModule::rollback_speculation`].
    #[must_use]
    pub fn speculation_checkpoint(&self) -> SpeculationCheckpoint {
        SpeculationCheckpoint {
            identity: self.speculation_identity.0,
            names: self.names.checkpoint(),
            annotations: self.annotations.len(),
            synthesis_directives: self.synthesis_directives.len(),
            ports: self.ports.len(),
            values: self.values.len(),
            operations: self.operations.len(),
            signals: self.signals.len(),
            memories: self.memories.len(),
            memory_read_ports: self.memory_read_ports.len(),
            memory_write_ports: self.memory_write_ports.len(),
            type_layouts: self.type_layouts.len(),
            connects: self.connects.len(),
            instances: self.instances.len(),
            named_signals: self.named_signals.len(),
        }
    }

    /// Discards values, operations, generated signals, and asynchronous memory
    /// read ports appended since `checkpoint`.
    ///
    /// The rollback is atomic: it rejects changes to any other arena, then
    /// validates the prospective prefix before discarding the suffix. Existing
    /// objects therefore cannot retain a speculative value or operation ID.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] without mutation when another arena changed, the
    /// name-table revision is incompatible, or the retained module would refer
    /// to an ID in the discarded suffix.
    pub fn rollback_speculation(
        &mut self,
        checkpoint: SpeculationCheckpoint,
    ) -> Result<(), WordError> {
        if checkpoint.identity != self.speculation_identity.0 {
            return Err(WordError::new(
                "speculation checkpoint belongs to a different module",
            ));
        }
        self.names
            .validate_checkpoint(checkpoint.names)
            .map_err(WordError::from)?;
        if checkpoint.values > self.values.len()
            || checkpoint.operations > self.operations.len()
            || checkpoint.signals > self.signals.len()
            || checkpoint.memory_read_ports > self.memory_read_ports.len()
            || checkpoint.named_signals > self.named_signals.len()
        {
            return Err(WordError::new(
                "speculation checkpoint is ahead of the module arenas",
            ));
        }
        if checkpoint.annotations != self.annotations.len()
            || checkpoint.synthesis_directives != self.synthesis_directives.len()
            || checkpoint.ports != self.ports.len()
            || checkpoint.memories != self.memories.len()
            || checkpoint.memory_write_ports != self.memory_write_ports.len()
            || checkpoint.type_layouts != self.type_layouts.len()
            || checkpoint.connects != self.connects.len()
            || checkpoint.instances != self.instances.len()
        {
            return Err(WordError::new(
                "speculation changed a non-value arena that rollback cannot undo",
            ));
        }
        if self.speculation_prefix_retains_suffix(checkpoint) {
            return Err(WordError::new(
                "speculation rollback would strand a generated object ID",
            ));
        }

        self.values.truncate(checkpoint.values);
        self.operations.truncate(checkpoint.operations);
        self.memory_read_ports
            .truncate(checkpoint.memory_read_ports);
        self.signals.truncate(checkpoint.signals);
        self.named_signals.truncate(checkpoint.named_signals);
        self.names
            .rollback(checkpoint.names)
            .map_err(WordError::from)?;
        Ok(())
    }

    fn speculation_prefix_retains_suffix(&self, checkpoint: SpeculationCheckpoint) -> bool {
        let target_retains_suffix = |target: AnnotationTarget| match target {
            AnnotationTarget::Value(value) => value.index() >= checkpoint.values,
            AnnotationTarget::Operation(operation) => operation.index() >= checkpoint.operations,
            AnnotationTarget::Signal(signal) => signal.index() >= checkpoint.signals,
            _ => false,
        };
        if self
            .annotations
            .iter()
            .any(|annotation| target_retains_suffix(annotation.target))
            || self
                .synthesis_directives
                .iter()
                .any(|directive| target_retains_suffix(directive.target))
        {
            return true;
        }

        let value_retains_suffix = |value: ValueId| value.index() >= checkpoint.values;
        let signal_retains_suffix = |signal: SignalId| signal.index() >= checkpoint.signals;
        if self.ports[..checkpoint.ports]
            .iter()
            .any(|port| signal_retains_suffix(port.signal))
            || self.memory_read_ports[..checkpoint.memory_read_ports]
                .iter()
                .any(|port| {
                    signal_retains_suffix(port.data)
                        || value_retains_suffix(port.address)
                        || match port.timing {
                            MemoryReadTiming::Asynchronous => false,
                            MemoryReadTiming::Synchronous { clock, enable, .. } => {
                                value_retains_suffix(clock.value)
                                    || enable
                                        .is_some_and(|enable| value_retains_suffix(enable.value))
                            }
                        }
                })
            || self.memory_write_ports[..checkpoint.memory_write_ports]
                .iter()
                .any(|port| {
                    value_retains_suffix(port.address)
                        || value_retains_suffix(port.data)
                        || value_retains_suffix(port.clock.value)
                        || port
                            .enable
                            .is_some_and(|enable| value_retains_suffix(enable.value))
                        || port
                            .mask
                            .is_some_and(|mask| value_retains_suffix(mask.value))
                })
        {
            return true;
        }
        if self.values[..checkpoint.values]
            .iter()
            .any(|value| match value.kind {
                ValueKind::Operation(operation) => operation.index() >= checkpoint.operations,
                ValueKind::Signal(reference) => signal_retains_suffix(reference.signal),
                ValueKind::Constant(_) => false,
            })
            || self.operations[..checkpoint.operations]
                .iter()
                .any(|operation| {
                    if value_retains_suffix(operation.result) {
                        return true;
                    }
                    let mut retains_suffix = false;
                    operation.kind.for_each_input(|value| {
                        retains_suffix |= value_retains_suffix(value);
                    });
                    retains_suffix
                })
        {
            return true;
        }

        self.connects[..checkpoint.connects].iter().any(|connect| {
            signal_retains_suffix(connect.target.signal)
                || value_retains_suffix(connect.value)
                || connect
                    .target
                    .dynamic
                    .is_some_and(|range| value_retains_suffix(range.offset))
        }) || self.instances.iter().any(|instance| {
            instance
                .connections
                .iter()
                .any(|connection| value_retains_suffix(connection.value))
        })
    }

    /// Removes and returns all continuous assignments in insertion order.
    pub fn take_connects(&mut self) -> Vec<Connect> {
        std::mem::take(&mut self.connects)
    }

    /// Looks up a port by ID.
    #[must_use]
    pub fn port(&self, id: PortId) -> Option<&Port> {
        self.ports.get(id.index())
    }

    /// Looks up a signal by ID.
    #[must_use]
    pub fn signal(&self, id: SignalId) -> Option<&Signal> {
        self.signals.get(id.index())
    }

    /// Looks up a memory by ID.
    #[must_use]
    pub fn memory(&self, id: MemoryId) -> Option<&Memory> {
        self.memories.get(id.index())
    }

    /// Looks up a memory read port by ID.
    #[must_use]
    pub fn memory_read_port(&self, id: MemoryReadPortId) -> Option<&MemoryReadPort> {
        self.memory_read_ports.get(id.index())
    }

    /// Looks up a memory write port by ID.
    #[must_use]
    pub fn memory_write_port(&self, id: MemoryWritePortId) -> Option<&MemoryWritePort> {
        self.memory_write_ports.get(id.index())
    }

    /// Looks up a word value by ID.
    #[must_use]
    pub fn value(&self, id: ValueId) -> Option<&Value> {
        self.values.get(id.index())
    }

    /// Looks up a word operation by ID.
    #[must_use]
    pub fn operation(&self, id: OpId) -> Option<&Operation> {
        self.operations.get(id.index())
    }

    /// Resolves an operation ID for controlled in-place rewriting.
    ///
    /// Call [`Self::validate`] after mutating an operation.
    pub fn operation_mut(&mut self, id: OpId) -> Option<&mut Operation> {
        self.operations.get_mut(id.index())
    }

    fn add_named_signal(
        &mut self,
        name: &str,
        kind: SignalKind,
        ty: WordType,
        source: SourceSpan,
    ) -> Result<SignalId, WordError> {
        if let Some(id) = self.names.get(name) {
            if dense_id(&self.named_signals, id).is_some() {
                return Err(WordError::new(format!("duplicate RTL signal '{name}'")));
            }
            if dense_id(&self.named_memories, id).is_some() {
                return Err(WordError::new(format!(
                    "RTL signal name '{name}' conflicts with a memory"
                )));
            }
        }
        let name = self.names.intern(name)?;
        let id = SignalId::from_index(self.signals.len())?;
        self.signals.push(Signal {
            name: Some(name),
            kind,
            ty,
            resolution: SignalResolution::SingleDriver,
            type_layout: None,
            source,
        });
        insert_dense_id(&mut self.named_signals, name, id)?;
        Ok(id)
    }

    fn push_value(
        &mut self,
        kind: ValueKind,
        ty: WordType,
        source: SourceSpan,
    ) -> Result<ValueId, WordError> {
        let id = ValueId::from_index(self.values.len())?;
        self.values.push(Value { kind, ty, source });
        Ok(id)
    }

    fn push_operation(
        &mut self,
        kind: OpKind,
        ty: WordType,
        source: SourceSpan,
    ) -> Result<ValueId, WordError> {
        let op_id = OpId::from_index(self.operations.len())?;
        let result = self.push_value(ValueKind::Operation(op_id), ty, source.clone())?;
        self.operations.push(Operation {
            kind,
            result,
            source,
        });
        Ok(result)
    }
}
