// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Frontend boundary combining structural word IR and procedural CFGs.
//!
//! [`RtlModule`] owns one [`WordModule`] plus procedures whose effects target
//! its signals and memories. Frontends may build procedures incrementally, but
//! publication seals their control-flow graphs and validates every cross-IR
//! type and ID.
//!
//! Linked elaboration recursively instantiates design definitions while
//! preserving library leaves as structural instances. Repeated occurrences are
//! remapped into one root-owned arena; source definition order and explicit CFG
//! entry blocks remain stable.

use crate::proc::{
    BlockId, Effect, ProcBuilder, ProcError, ProcModule, ProcTarget, ProcedureId, Sensitivity,
    SensitivityEvent, SwitchArmSpec, TargetSelect, TerminatorKind,
};
use crate::word::{
    AnnotationTarget, BinaryOp, BitRange, CastKind, InstId, Memory, MemoryId, ModuleRemap, Signal,
    SignalBindingOffset, SourceSpan, SynthesisDirectiveKind, ValueId, WordModule,
    elaborate_linked_root_with,
};
use crate::word::{WordError, WordType};
use crate::{BitVal, ConstBits};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
/// Validation or construction failure at the procedural/structural RTL boundary.
pub enum RtlError {
    /// The procedural control-flow graph is invalid.
    #[error(transparent)]
    Procedural(#[from] ProcError),
    /// The structural word-level graph is invalid.
    #[error(transparent)]
    Word(#[from] WordError),
    /// A cross-IR invariant is violated.
    #[error("{0}")]
    Invariant(String),
}

impl RtlError {
    fn new(message: impl Into<String>) -> Self {
        Self::Invariant(message.into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// One RTL definition containing structural word IR and sealed procedures.
pub struct RtlModule {
    word: WordModule,
    procedures: ProcModule,
}

/// Borrowed checkpoint view that encodes repeated source origins once.
///
/// The wrapper scopes its reference table to exactly one RTL owner. Ordinary
/// `RtlModule` serde remains self-contained and preserves its public wire.
#[derive(Debug, Clone, Copy)]
pub struct RtlModuleCheckpointRef<'a>(&'a RtlModule);

impl<'a> RtlModuleCheckpointRef<'a> {
    /// Creates a compact checkpoint view of one RTL module.
    #[must_use]
    pub const fn new(module: &'a RtlModule) -> Self {
        Self(module)
    }
}

impl Serialize for RtlModuleCheckpointRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        crate::word::with_source_origin_serialization(|| self.0.serialize(serializer))
    }
}

/// RTL module decoded from a compact checkpoint source-origin stream.
#[derive(Debug)]
pub struct RtlModuleCheckpoint(RtlModule);

impl RtlModuleCheckpoint {
    /// Returns the decoded RTL owner.
    #[must_use]
    pub fn into_inner(self) -> RtlModule {
        self.0
    }
}

impl<'de> Deserialize<'de> for RtlModuleCheckpoint {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        crate::word::with_source_origin_deserialization(|| {
            RtlModule::deserialize(deserializer).map(Self)
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct OwnerClaim {
    signal: crate::word::SignalId,
    start: u32,
    end: u32,
    owner: ProcedureId,
}

impl RtlModule {
    /// Creates an RTL module with no procedural behavior.
    ///
    /// # Errors
    ///
    /// Returns [`RtlError`] if the structural module violates a word-IR or
    /// cross-phase invariant.
    pub fn structural(word: WordModule) -> Result<Self, RtlError> {
        Self::new(word, ProcModule::default())
    }

    /// Creates and validates an RTL module from its two owned phase models.
    ///
    /// # Errors
    ///
    /// Returns [`RtlError`] for an invalid word module, procedural CFG, or
    /// structural/procedural reference contract.
    pub fn new(word: WordModule, procedures: ProcModule) -> Result<Self, RtlError> {
        let module = Self { word, procedures };
        module.validate()?;
        Ok(module)
    }

    /// Returns the sealed structural word IR.
    #[must_use]
    pub fn word(&self) -> &WordModule {
        &self.word
    }

    /// Returns the sealed procedural control-flow graph.
    #[must_use]
    pub fn procedures(&self) -> &ProcModule {
        &self.procedures
    }

    /// Renames this definition while preserving all typed references.
    ///
    /// # Errors
    ///
    /// Returns [`RtlError`] for an empty name or a full name table.
    pub fn rename(&mut self, name: impl AsRef<str>) -> Result<(), RtlError> {
        self.word.rename(name).map_err(Into::into)
    }

    /// Retargets one design instance without exposing mutable structural IR.
    ///
    /// # Errors
    ///
    /// Returns [`RtlError`] if `instance` is unknown, the module name is empty,
    /// or the shared name table cannot intern it.
    pub fn set_instance_module(
        &mut self,
        instance: InstId,
        module: impl AsRef<str>,
    ) -> Result<(), RtlError> {
        self.word
            .set_instance_module(instance, module)
            .map_err(Into::into)
    }

    /// Sets one typed synthesis directive without exposing mutable structural IR.
    ///
    /// # Errors
    ///
    /// Returns [`RtlError`] when `target` is unknown or the directive is not
    /// valid for that target kind.
    pub fn set_synthesis_directive(
        &mut self,
        target: AnnotationTarget,
        kind: SynthesisDirectiveKind,
        enabled: bool,
        source: SourceSpan,
    ) -> Result<(), RtlError> {
        self.word
            .set_synthesis_directive(target, kind, enabled, source)
            .map_err(Into::into)
    }

    /// Consolidates mutable structural names into immutable shared storage.
    ///
    /// # Errors
    ///
    /// Returns [`RtlError`] if the compact name table cannot be rebuilt.
    pub fn consolidate_names(&mut self) -> Result<(), RtlError> {
        self.word.consolidate_names().map_err(Into::into)
    }

    /// Consumes the module and returns its structural and procedural owners.
    #[must_use]
    pub fn into_parts(self) -> (WordModule, ProcModule) {
        (self.word, self.procedures)
    }

    /// Validates both owned IRs and every cross-IR reference and type contract.
    ///
    /// Validation also rejects overlapping procedural ownership of non-local
    /// signal bits by different procedures.
    ///
    /// # Errors
    ///
    /// Returns [`RtlError`] on the first structural, CFG, typing, ownership, or
    /// cross-reference invariant violation.
    ///
    /// # Panics
    ///
    /// Panics only if a validated compact procedural range cannot be converted
    /// back to its typed ID; safe constructors preserve this invariant.
    pub fn validate(&self) -> Result<(), RtlError> {
        self.procedures.validate()?;
        self.word.validate()?;
        if self.word.definition_kind() == crate::word::DefinitionKind::BlackBox
            && !self.procedures.procedures().is_empty()
        {
            return Err(RtlError::new(
                "black-box definition cannot contain procedural behavior",
            ));
        }
        for (procedure_index, procedure) in self.procedures.procedures().iter().enumerate() {
            let procedure_id = crate::proc::ProcedureId::from_index(procedure_index)?;
            if let Sensitivity::Edges(_) = procedure.sensitivity {
                for (_, event) in self
                    .procedures
                    .sensitivity_events(procedure_id)
                    .expect("edge-sensitive procedure has an event range")
                {
                    let signal = self.signal(event.signal, "sensitivity event")?;
                    require_width(signal.ty, 1, "sensitivity event signal")?;
                }
            }
        }
        for (index, block) in self.procedures.blocks().iter().enumerate() {
            let block_id = crate::proc::BlockId::from_index(index)?;
            for (_, effect) in self
                .procedures
                .block_effects(block_id)
                .expect("sealed block has an effect range")
            {
                self.validate_effect(effect)?;
            }
            match block.terminator.kind {
                TerminatorKind::Return | TerminatorKind::Jump { .. } => {}
                TerminatorKind::Branch { condition, .. } => {
                    require_width(
                        self.value_type(condition, "branch condition")?,
                        1,
                        "branch condition",
                    )?;
                }
                TerminatorKind::Switch { selector, .. } => {
                    let selector_type = self.value_type(selector, "switch selector")?;
                    for (_, arm) in self
                        .procedures
                        .switch_arms(block_id)
                        .expect("sealed switch has an arm range")
                    {
                        let pattern_type = self.value_type(arm.pattern, "switch pattern")?;
                        if pattern_type != selector_type {
                            return Err(RtlError::new(format!(
                                "switch pattern type {pattern_type:?} does not match selector type {selector_type:?}"
                            )));
                        }
                    }
                }
            }
        }
        self.validate_procedural_owners()?;
        Ok(())
    }

    /// Deterministic upper bound for temporary arenas used by
    /// [`Self::validate`].
    #[must_use]
    pub fn validation_memory_bytes(&self) -> usize {
        self.procedures
            .validation_memory_bytes()
            .max(self.word.validation_memory_bytes())
            .max(opto_core::resident::slice_bytes::<OwnerClaim>(
                self.owner_claim_capacity(),
            ))
    }

    fn owner_claim_capacity(&self) -> usize {
        self.procedures
            .effects()
            .iter()
            .filter(|effect| matches!(effect.target, ProcTarget::Signal { .. }))
            .count()
    }

    fn validate_procedural_owners(&self) -> Result<(), RtlError> {
        let mut claims = Vec::with_capacity(self.owner_claim_capacity());
        for (procedure_index, _) in self.procedures.procedures().iter().enumerate() {
            let procedure = ProcedureId::from_index(procedure_index)?;
            for block in self
                .procedures
                .procedure_blocks(procedure)
                .expect("validated procedure owns a block range")
            {
                for (_, effect) in self
                    .procedures
                    .block_effects(block)
                    .expect("validated block owns an effect range")
                {
                    let ProcTarget::Signal { signal, select } = effect.target else {
                        continue;
                    };
                    let stored = self.signal(signal, "procedural assignment target")?;
                    if stored.kind == crate::word::SignalKind::ProcessLocal {
                        continue;
                    }
                    let (start, end) = match select {
                        TargetSelect::Whole | TargetSelect::Dynamic { .. } => {
                            (0, stored.ty.width())
                        }
                        TargetSelect::Static(range) => {
                            let start = range.lsb.min(range.msb);
                            (start, start + range.width())
                        }
                    };
                    claims.push(OwnerClaim {
                        signal,
                        start,
                        end,
                        owner: procedure,
                    });
                }
            }
        }
        claims.sort_unstable();
        let mut active: Option<OwnerClaim> = None;
        for claim in claims {
            match &mut active {
                Some(previous) if previous.signal == claim.signal && claim.start < previous.end => {
                    if previous.owner != claim.owner {
                        let name = self
                            .word
                            .signal(claim.signal)
                            .and_then(|signal| signal.name)
                            .map_or("<unnamed>", |name| self.word.name_str(name));
                        return Err(RtlError::new(format!(
                            "signal '{name}' bit {} has multiple drivers",
                            claim.start
                        )));
                    }
                    previous.end = previous.end.max(claim.end);
                }
                _ => active = Some(claim),
            }
        }
        Ok(())
    }

    fn validate_effect(&self, effect: &Effect) -> Result<(), RtlError> {
        let value_type = self.value_type(effect.value, "procedural assignment value")?;
        let target_type = self.target_type(effect.target)?;
        if value_type != target_type {
            return Err(RtlError::new(format!(
                "procedural assignment type mismatch: target {target_type:?}, value {value_type:?}{}",
                source_location(&effect.source)
            )));
        }
        Ok(())
    }

    fn target_type(&self, target: ProcTarget) -> Result<WordType, RtlError> {
        match target {
            ProcTarget::Signal { signal, select } => {
                let signal = self.signal(signal, "procedural assignment target")?;
                self.selected_type(signal.ty, select)
            }
            ProcTarget::Memory {
                memory,
                address,
                select,
            } => {
                let memory = self.memory(memory)?;
                let address_type = self.value_type(address, "procedural memory address")?;
                let minimum_width = (u32::BITS - (memory.depth.get() - 1).leading_zeros()).max(1);
                if address_type.is_signed() || address_type.width() < minimum_width {
                    return Err(RtlError::new(format!(
                        "procedural memory address must be unsigned and at least {minimum_width} bits wide"
                    )));
                }
                self.selected_type(memory.element_type, select)
            }
        }
    }

    fn selected_type(&self, base: WordType, select: TargetSelect) -> Result<WordType, RtlError> {
        match select {
            TargetSelect::Whole => Ok(base),
            TargetSelect::Static(range) => {
                if range.msb.max(range.lsb) >= base.width() {
                    return Err(RtlError::new(format!(
                        "procedural target range [{}:{}] exceeds width {}",
                        range.msb,
                        range.lsb,
                        base.width()
                    )));
                }
                selected_word_type(base, range.width())
            }
            TargetSelect::Dynamic { offset, width } => {
                let offset_type = self.value_type(offset, "dynamic target offset")?;
                if offset_type.is_signed() || width.get() > base.width() {
                    return Err(RtlError::new(format!(
                        "dynamic target selection must have an unsigned offset and width at most {}",
                        base.width()
                    )));
                }
                selected_word_type(base, width.get())
            }
        }
    }

    fn value_type(&self, value: ValueId, kind: &str) -> Result<WordType, RtlError> {
        self.word
            .value(value)
            .map(|value| value.ty)
            .ok_or_else(|| RtlError::new(format!("{kind} references unknown value {value:?}")))
    }

    fn signal(&self, signal: crate::word::SignalId, kind: &str) -> Result<&Signal, RtlError> {
        self.word
            .signal(signal)
            .ok_or_else(|| RtlError::new(format!("{kind} references unknown signal {signal:?}")))
    }

    fn memory(&self, memory: MemoryId) -> Result<&Memory, RtlError> {
        self.word.memory(memory).ok_or_else(|| {
            RtlError::new(format!(
                "procedural assignment references unknown memory {memory:?}"
            ))
        })
    }
}

/// Recursively replaces design instances with structural Word IR and flat CFGs.
///
/// Definitions absent from `definitions` remain external leaf instances. Every
/// occurrence receives its own remapped signal, value, memory, block, and effect
/// identities; no hierarchical path is stored in procedural IR.
///
/// # Errors
///
/// Returns [`RtlError`] for invalid or duplicate definitions, recursive
/// hierarchy, incompatible instance bindings, capacity overflow, or an invalid
/// elaborated structural/procedural module.
///
/// # Panics
///
/// Panics only if the word-level elaborator reports a definition occurrence not
/// present in the index built from the same validated input set.
pub fn elaborate_linked_root<'a>(
    root: &'a RtlModule,
    definitions: impl IntoIterator<Item = &'a RtlModule>,
) -> Result<RtlModule, RtlError> {
    let definitions = definitions.into_iter().collect::<Vec<_>>();
    let mut by_name = BTreeMap::new();
    insert_definition(&mut by_name, root)?;
    for &definition in &definitions {
        insert_definition(&mut by_name, definition)?;
    }

    let mut occurrences = Vec::new();
    let mut word = elaborate_linked_root_with(
        root.word(),
        definitions.iter().map(|definition| definition.word()),
        |source, remap| {
            let definition = by_name
                .get(source.name())
                .expect("word hierarchy occurrences originate from the definition index");
            occurrences.push((definition.procedures(), remap.clone()));
            Ok::<(), RtlError>(())
        },
    )?;
    let mut procedures = ProcBuilder::new();
    for (source, remap) in occurrences {
        append_procedures(&mut procedures, &mut word, source, &remap)?;
    }
    RtlModule::new(word, procedures.seal()?)
}

fn insert_definition<'a>(
    definitions: &mut BTreeMap<&'a str, &'a RtlModule>,
    definition: &'a RtlModule,
) -> Result<(), RtlError> {
    definition.validate()?;
    match definitions.insert(definition.word().name(), definition) {
        Some(previous) if !std::ptr::eq(previous, definition) => Err(RtlError::new(format!(
            "multiple RTL definitions are named '{}'",
            definition.word().name()
        ))),
        _ => Ok(()),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "procedure cloning preserves one shared ID remap across blocks, effects, and edges"
)]
fn append_procedures(
    target: &mut ProcBuilder,
    word: &mut WordModule,
    source: &ProcModule,
    remap: &ModuleRemap,
) -> Result<(), RtlError> {
    for (procedure_index, procedure) in source.procedures().iter().enumerate() {
        let old_procedure = ProcedureId::from_index(procedure_index)?;
        let new_procedure = match procedure.sensitivity {
            Sensitivity::Implicit => {
                target.add_combinational_procedure(procedure.kind, procedure.source.clone())?
            }
            Sensitivity::Edges(_) => target.add_clocked_procedure(
                source
                    .sensitivity_events(old_procedure)
                    .expect("validated edge-sensitive procedure owns events")
                    .map(|(_, event)| {
                        Ok(SensitivityEvent {
                            signal: remap.signal(event.signal)?,
                            edge: event.edge,
                        })
                    })
                    .collect::<Result<Vec<_>, WordError>>()?,
                procedure.source.clone(),
            )?,
        };

        let mut old_blocks = source
            .procedure_blocks(old_procedure)
            .expect("validated procedure owns blocks");
        let first_old = old_blocks
            .next()
            .expect("validated procedure has at least one block");
        let first_new = target.add_block(
            new_procedure,
            source
                .block(first_old)
                .expect("validated block ID resolves")
                .source
                .clone(),
        )?;
        for old_block in old_blocks {
            target.add_block(
                new_procedure,
                source
                    .block(old_block)
                    .expect("validated block ID resolves")
                    .source
                    .clone(),
            )?;
        }
        let blocks = BlockRemap {
            source_start: first_old.index(),
            target_start: first_new.index(),
            len: procedure.block_count(),
        };
        target.set_entry(new_procedure, blocks.map(procedure.entry)?)?;

        for old_block in source
            .procedure_blocks(old_procedure)
            .expect("validated procedure owns blocks")
        {
            let new_block = blocks.map(old_block)?;
            for (_, effect) in source
                .block_effects(old_block)
                .expect("validated block owns effects")
            {
                target.assign(
                    new_block,
                    effect.mode,
                    remap_target(effect.target, remap, word, &effect.source)?,
                    remap.value(effect.value)?,
                    effect.source.clone(),
                )?;
            }
            let terminator = &source
                .block(old_block)
                .expect("validated block ID resolves")
                .terminator;
            match terminator.kind {
                TerminatorKind::Return => {
                    target.terminate_return(new_block, terminator.source.clone())?;
                }
                TerminatorKind::Jump { edge } => target.terminate_jump(
                    new_block,
                    blocks.map(edge_target(source, edge)?)?,
                    terminator.source.clone(),
                )?,
                TerminatorKind::Branch {
                    condition,
                    then_edge,
                    else_edge,
                } => target.terminate_branch(
                    new_block,
                    remap.value(condition)?,
                    blocks.map(edge_target(source, then_edge)?)?,
                    blocks.map(edge_target(source, else_edge)?)?,
                    terminator.source.clone(),
                )?,
                TerminatorKind::Switch {
                    selector, default, ..
                } => {
                    let arms = source
                        .switch_arms(old_block)
                        .expect("validated switch owns arms")
                        .map(|(_, arm)| {
                            let edge = source
                                .edge(arm.edge)
                                .expect("validated switch edge resolves");
                            Ok(SwitchArmSpec {
                                pattern: remap.value(arm.pattern)?,
                                target: blocks.map(edge.target)?,
                                source: edge.source_span.clone(),
                            })
                        })
                        .collect::<Result<Vec<_>, RtlError>>()?;
                    target.terminate_switch(
                        new_block,
                        remap.value(selector)?,
                        arms,
                        blocks.map(edge_target(source, default)?)?,
                        terminator.source.clone(),
                    )?;
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct BlockRemap {
    source_start: usize,
    target_start: usize,
    len: usize,
}

impl BlockRemap {
    fn map(self, block: BlockId) -> Result<BlockId, RtlError> {
        let offset = block
            .index()
            .checked_sub(self.source_start)
            .filter(|&offset| offset < self.len)
            .ok_or_else(|| RtlError::new("procedural edge escapes its owning procedure"))?;
        let index = self
            .target_start
            .checked_add(offset)
            .ok_or_else(|| RtlError::new("procedural block remap exceeds address capacity"))?;
        BlockId::from_index(index).map_err(Into::into)
    }
}

fn edge_target(source: &ProcModule, edge: crate::proc::EdgeId) -> Result<BlockId, RtlError> {
    source
        .edge(edge)
        .map(|edge| edge.target)
        .ok_or_else(|| RtlError::new(format!("unknown procedural edge {edge:?}")))
}

fn remap_target(
    target: ProcTarget,
    remap: &ModuleRemap,
    word: &mut WordModule,
    source: &SourceSpan,
) -> Result<ProcTarget, RtlError> {
    Ok(match target {
        ProcTarget::Signal {
            signal,
            select: old,
        } => {
            let (signal, binding, binding_width) = remap.signal_range(signal)?;
            let signal_width = word
                .signal(signal)
                .ok_or_else(|| RtlError::new("reference-port actual signal disappeared"))?
                .ty
                .width();
            let select = match binding {
                SignalBindingOffset::Static(base) => match old {
                    TargetSelect::Whole if base == 0 && binding_width == signal_width => {
                        TargetSelect::Whole
                    }
                    TargetSelect::Whole => TargetSelect::Static(BitRange {
                        msb: base.checked_add(binding_width - 1).ok_or_else(|| {
                            RtlError::new("reference-port target range exceeds 32-bit capacity")
                        })?,
                        lsb: base,
                    }),
                    TargetSelect::Static(range) => TargetSelect::Static(BitRange {
                        msb: base.checked_add(range.msb).ok_or_else(|| {
                            RtlError::new("reference-port target range exceeds 32-bit capacity")
                        })?,
                        lsb: base.checked_add(range.lsb).ok_or_else(|| {
                            RtlError::new("reference-port target range exceeds 32-bit capacity")
                        })?,
                    }),
                    TargetSelect::Dynamic { offset, width } => TargetSelect::Dynamic {
                        offset: add_reference_offset(word, remap.value(offset)?, base, source)?,
                        width,
                    },
                },
                SignalBindingOffset::Dynamic { offset, base } => {
                    let offset = add_reference_offset(word, offset, base, source)?;
                    match old {
                        TargetSelect::Whole => TargetSelect::Dynamic {
                            offset,
                            width: std::num::NonZeroU32::new(binding_width)
                                .expect("reference-port binding width is nonzero"),
                        },
                        TargetSelect::Static(range) => TargetSelect::Dynamic {
                            offset: add_reference_offset(word, offset, range.lsb, source)?,
                            width: std::num::NonZeroU32::new(range.width())
                                .expect("static target range is nonzero"),
                        },
                        TargetSelect::Dynamic {
                            offset: relative,
                            width,
                        } => TargetSelect::Dynamic {
                            offset: add_reference_offsets(
                                word,
                                offset,
                                remap.value(relative)?,
                                source,
                            )?,
                            width,
                        },
                    }
                }
            };
            ProcTarget::Signal { signal, select }
        }
        ProcTarget::Memory {
            memory,
            address,
            select: old,
        } => ProcTarget::Memory {
            memory: remap.memory(memory)?,
            address: remap.value(address)?,
            select: match old {
                TargetSelect::Whole => TargetSelect::Whole,
                TargetSelect::Static(range) => TargetSelect::Static(range),
                TargetSelect::Dynamic { offset, width } => TargetSelect::Dynamic {
                    offset: remap.value(offset)?,
                    width,
                },
            },
        },
    })
}

fn add_reference_offset(
    word: &mut WordModule,
    offset: ValueId,
    base: u32,
    source: &SourceSpan,
) -> Result<ValueId, RtlError> {
    if base == 0 {
        return Ok(offset);
    }
    let offset_ty = word
        .value(offset)
        .ok_or_else(|| RtlError::new("reference-port dynamic offset disappeared"))?
        .ty;
    let base_width = u32::BITS - base.leading_zeros();
    let width = offset_ty
        .width()
        .max(base_width)
        .checked_add(1)
        .ok_or_else(|| RtlError::new("reference-port dynamic offset is too wide"))?;
    let ty = WordType::new(width, false, offset_ty.state())?;
    let offset = word.cast(CastKind::ZeroExtend, offset, ty, source.clone())?;
    let bits = ConstBits::from_bits(
        (0..width)
            .rev()
            .map(|bit| {
                if bit < u32::BITS && base & (1_u32 << bit) != 0 {
                    BitVal::One
                } else {
                    BitVal::Zero
                }
            })
            .collect(),
    )
    .map_err(|error| RtlError::new(error.to_string()))?;
    let base = word.constant(bits, ty, source.clone())?;
    word.binary(BinaryOp::Add, offset, base, source.clone())
        .map_err(Into::into)
}

fn add_reference_offsets(
    word: &mut WordModule,
    left: ValueId,
    right: ValueId,
    source: &SourceSpan,
) -> Result<ValueId, RtlError> {
    let left_ty = word
        .value(left)
        .ok_or_else(|| RtlError::new("reference-port dynamic offset disappeared"))?
        .ty;
    let right_ty = word
        .value(right)
        .ok_or_else(|| RtlError::new("reference-port dynamic offset disappeared"))?
        .ty;
    let width = left_ty
        .width()
        .max(right_ty.width())
        .checked_add(1)
        .ok_or_else(|| RtlError::new("reference-port dynamic offset is too wide"))?;
    let state = if left_ty.state() == crate::word::LogicStateKind::FourState
        || right_ty.state() == crate::word::LogicStateKind::FourState
    {
        crate::word::LogicStateKind::FourState
    } else {
        crate::word::LogicStateKind::TwoState
    };
    let ty = WordType::new(width, false, state)?;
    let left = word.cast(CastKind::ZeroExtend, left, ty, source.clone())?;
    let right = word.cast(CastKind::ZeroExtend, right, ty, source.clone())?;
    word.binary(BinaryOp::Add, left, right, source.clone())
        .map_err(Into::into)
}

fn selected_word_type(base: WordType, width: u32) -> Result<WordType, RtlError> {
    WordType::new(width, false, base.state()).map_err(Into::into)
}

fn require_width(ty: WordType, width: u32, kind: &str) -> Result<(), RtlError> {
    if ty.width() != width {
        return Err(RtlError::new(format!(
            "{kind} must be {width} bit wide, got {}",
            ty.width()
        )));
    }
    Ok(())
}

fn source_location(source: &SourceSpan) -> String {
    source.file().map_or_else(String::new, |file| {
        let mut location = format!(" at {file}");
        if let Some(line) = source.line() {
            write!(location, ":{line}").expect("writing to String cannot fail");
            if let Some(column) = source.column() {
                write!(location, ":{column}").expect("writing to String cannot fail");
            }
        }
        location
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ConstBits;
    use crate::proc::{AssignmentMode, ProcBuilder, ProcedureKind};
    use crate::word::{PortDirection, SourceSpan};
    use std::num::NonZeroU32;

    #[test]
    fn validates_cross_ir_types_and_round_trips() {
        let mut word = WordModule::new("top");
        let input = word
            .add_port(
                "a",
                PortDirection::Input,
                WordType::bits(2).unwrap(),
                SourceSpan::default(),
            )
            .unwrap();
        let target = word
            .add_register_signal("q", WordType::bits(2).unwrap(), SourceSpan::default())
            .unwrap();
        let value = word
            .read_signal(word.port(input).unwrap().signal, SourceSpan::default())
            .unwrap();
        let mut procedures = ProcBuilder::new();
        let procedure = procedures
            .add_combinational_procedure(ProcedureKind::Combinational, SourceSpan::default())
            .unwrap();
        let block = procedures
            .add_block(procedure, SourceSpan::default())
            .unwrap();
        procedures
            .assign(
                block,
                AssignmentMode::Blocking,
                ProcTarget::signal(target),
                value,
                SourceSpan::default(),
            )
            .unwrap();
        procedures
            .terminate_return(block, SourceSpan::default())
            .unwrap();
        let module = RtlModule::new(word, procedures.seal().unwrap()).unwrap();

        let json = serde_json::to_string(&module).unwrap();
        let decoded: RtlModule = serde_json::from_str(&json).unwrap();
        decoded.validate().unwrap();
        assert_eq!(decoded, module);
    }

    #[test]
    fn rejects_assignment_type_mismatch() {
        let mut word = WordModule::new("top");
        let target = word
            .add_wire("q", WordType::bits(2).unwrap(), SourceSpan::default())
            .unwrap();
        let value = word
            .constant(
                ConstBits::from_bin_str("1").unwrap(),
                WordType::bits(1).unwrap(),
                SourceSpan::default(),
            )
            .unwrap();
        let mut procedures = ProcBuilder::new();
        let procedure = procedures
            .add_combinational_procedure(ProcedureKind::Combinational, SourceSpan::default())
            .unwrap();
        let block = procedures
            .add_block(procedure, SourceSpan::default())
            .unwrap();
        procedures
            .assign(
                block,
                AssignmentMode::Blocking,
                ProcTarget::signal(target),
                value,
                SourceSpan::default(),
            )
            .unwrap();
        procedures
            .terminate_return(block, SourceSpan::default())
            .unwrap();
        let error = RtlModule::new(word, procedures.seal().unwrap()).unwrap_err();
        assert!(error.to_string().contains("type mismatch"));
    }

    #[test]
    fn rejects_overlapping_persistent_targets_from_distinct_procedures() {
        let mut word = WordModule::new("top");
        let target = word
            .add_register_signal("q", WordType::bits(2).unwrap(), SourceSpan::default())
            .unwrap();
        let value = word
            .constant(
                ConstBits::from_bin_str("1").unwrap(),
                WordType::bits(1).unwrap(),
                SourceSpan::default(),
            )
            .unwrap();
        let mut procedures = ProcBuilder::new();
        for bit in [0, 0] {
            let procedure = procedures
                .add_combinational_procedure(ProcedureKind::Combinational, SourceSpan::default())
                .unwrap();
            let block = procedures
                .add_block(procedure, SourceSpan::default())
                .unwrap();
            procedures
                .assign(
                    block,
                    AssignmentMode::Blocking,
                    ProcTarget::signal(target).with_select(TargetSelect::Static(
                        crate::word::BitRange { msb: bit, lsb: bit },
                    )),
                    value,
                    SourceSpan::default(),
                )
                .unwrap();
            procedures
                .terminate_return(block, SourceSpan::default())
                .unwrap();
        }

        let error = RtlModule::new(word, procedures.seal().unwrap()).unwrap_err();
        assert_eq!(error.to_string(), "signal 'q' bit 0 has multiple drivers");
    }

    fn procedural_child() -> RtlModule {
        let mut word = WordModule::new("child");
        let memory = word
            .add_memory(
                "mem",
                WordType::bits(8).unwrap(),
                NonZeroU32::new(4).unwrap(),
                SourceSpan::default(),
            )
            .unwrap();
        let constant = |word: &mut WordModule, bits: &str| {
            word.constant(
                ConstBits::from_bin_str(bits).unwrap(),
                WordType::bits(u32::try_from(bits.len()).unwrap()).unwrap(),
                SourceSpan::default(),
            )
            .unwrap()
        };
        let condition = constant(&mut word, "1");
        let selector = constant(&mut word, "10");
        let pattern = constant(&mut word, "01");
        let address = constant(&mut word, "00");
        let value = constant(&mut word, "10100101");

        let mut procedures = ProcBuilder::new();
        let procedure = procedures
            .add_combinational_procedure(ProcedureKind::Combinational, SourceSpan::default())
            .unwrap();
        // Deliberately store the return block before the entry block. Linked elaboration
        // must preserve arena order while remapping the explicit entry.
        let return_block = procedures
            .add_block(procedure, SourceSpan::default())
            .unwrap();
        let entry = procedures
            .add_block(procedure, SourceSpan::default())
            .unwrap();
        let switch = procedures
            .add_block(procedure, SourceSpan::default())
            .unwrap();
        let assign = procedures
            .add_block(procedure, SourceSpan::default())
            .unwrap();
        procedures.set_entry(procedure, entry).unwrap();
        procedures
            .terminate_return(return_block, SourceSpan::default())
            .unwrap();
        procedures
            .terminate_branch(entry, condition, switch, assign, SourceSpan::default())
            .unwrap();
        procedures
            .terminate_switch(
                switch,
                selector,
                [SwitchArmSpec {
                    pattern,
                    target: assign,
                    source: SourceSpan::default(),
                }],
                return_block,
                SourceSpan::default(),
            )
            .unwrap();
        procedures
            .assign(
                assign,
                AssignmentMode::Nonblocking,
                ProcTarget::memory(memory, address),
                value,
                SourceSpan::default(),
            )
            .unwrap();
        procedures
            .terminate_jump(assign, return_block, SourceSpan::default())
            .unwrap();
        RtlModule::new(word, procedures.seal().unwrap()).unwrap()
    }

    #[test]
    fn linked_elaboration_remaps_repeated_cfg_occurrences_and_memories() {
        let child = procedural_child();
        let mut top = WordModule::new("top");
        top.add_instance("left", "child", Vec::new(), SourceSpan::default())
            .unwrap();
        top.add_instance("right", "child", Vec::new(), SourceSpan::default())
            .unwrap();
        let top = RtlModule::structural(top).unwrap();

        let flat = elaborate_linked_root(&top, [&top, &child]).unwrap();
        flat.validate().unwrap();
        assert!(flat.word().instances().is_empty());
        assert_eq!(flat.word().memories().len(), 2);
        assert_eq!(flat.procedures().procedures().len(), 2);
        assert_eq!(flat.procedures().blocks().len(), 8);
        assert_eq!(flat.procedures().effects().len(), 2);

        let left_memory = flat.word().memory_id("left/mem").unwrap();
        let right_memory = flat.word().memory_id("right/mem").unwrap();
        let assigned_memories = flat
            .procedures()
            .effects()
            .iter()
            .map(|effect| match effect.target {
                ProcTarget::Memory { memory, .. } => memory,
                ProcTarget::Signal { .. } => panic!("expected a memory effect"),
            })
            .collect::<Vec<_>>();
        assert_eq!(assigned_memories, [left_memory, right_memory]);

        assert_eq!(flat.procedures().procedures()[0].entry.index(), 1);
        assert_eq!(flat.procedures().procedures()[1].entry.index(), 5);
        assert!(matches!(
            flat.procedures().blocks()[1].terminator.kind,
            TerminatorKind::Branch { .. }
        ));
        assert!(matches!(
            flat.procedures().blocks()[2].terminator.kind,
            TerminatorKind::Switch { .. }
        ));
        assert!(matches!(
            flat.procedures().blocks()[5].terminator.kind,
            TerminatorKind::Branch { .. }
        ));
        assert!(matches!(
            flat.procedures().blocks()[6].terminator.kind,
            TerminatorKind::Switch { .. }
        ));
    }

    #[test]
    fn linked_elaboration_connects_procedural_child_inputs() {
        let mut child_word = WordModule::new("child");
        let bit = WordType::bits(1).unwrap();
        let input = child_word
            .add_port("input", PortDirection::Input, bit, SourceSpan::default())
            .unwrap();
        let output = child_word
            .add_port("output", PortDirection::Output, bit, SourceSpan::default())
            .unwrap();
        let input_value = child_word
            .read_signal(
                child_word.port(input).unwrap().signal,
                SourceSpan::default(),
            )
            .unwrap();
        let mut child_procedures = ProcBuilder::new();
        let procedure = child_procedures
            .add_combinational_procedure(ProcedureKind::Combinational, SourceSpan::default())
            .unwrap();
        let block = child_procedures
            .add_block(procedure, SourceSpan::default())
            .unwrap();
        child_procedures
            .assign(
                block,
                AssignmentMode::Blocking,
                ProcTarget::signal(child_word.port(output).unwrap().signal),
                input_value,
                SourceSpan::default(),
            )
            .unwrap();
        child_procedures
            .terminate_return(block, SourceSpan::default())
            .unwrap();
        let child = RtlModule::new(child_word, child_procedures.seal().unwrap()).unwrap();

        let mut top = WordModule::new("top");
        let input = top
            .add_port("input", PortDirection::Input, bit, SourceSpan::default())
            .unwrap();
        let output = top
            .add_port("output", PortDirection::Output, bit, SourceSpan::default())
            .unwrap();
        let input_value = top
            .read_signal(top.port(input).unwrap().signal, SourceSpan::default())
            .unwrap();
        let output_value = top
            .read_signal(top.port(output).unwrap().signal, SourceSpan::default())
            .unwrap();
        top.add_instance(
            "u_child",
            "child",
            vec![
                ("input".to_string(), input_value, SourceSpan::default()),
                ("output".to_string(), output_value, SourceSpan::default()),
            ],
            SourceSpan::default(),
        )
        .unwrap();
        let top = RtlModule::structural(top).unwrap();

        let flat = elaborate_linked_root(&top, [&top, &child]).unwrap();
        flat.validate().unwrap();
        let child_input = flat.word().signal_id("u_child/input").unwrap();
        assert!(
            flat.word()
                .connects()
                .iter()
                .any(|connect| connect.target.signal == child_input)
        );
        let effect_value = flat.procedures().effects()[0].value;
        assert!(matches!(
            flat.word().value(effect_value).map(|value| &value.kind),
            Some(crate::word::ValueKind::Signal(reference)) if reference.signal == child_input
        ));
    }

    #[test]
    fn safe_definition_edits_do_not_expose_mutable_word_ir() {
        let mut word = WordModule::new("child");
        word.add_instance("u", "leaf", Vec::new(), SourceSpan::default())
            .unwrap();
        let mut rtl = RtlModule::structural(word).unwrap();
        rtl.rename("child_variant").unwrap();
        rtl.set_instance_module(rtl.word().instance_id("u").unwrap(), "leaf_variant")
            .unwrap();
        rtl.consolidate_names().unwrap();

        assert_eq!(rtl.word().name(), "child_variant");
        let instance = &rtl.word().instances()[0];
        assert_eq!(rtl.word().name_str(instance.module), "leaf_variant");
    }
}
