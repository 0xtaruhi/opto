// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::{DbUpdate, FrontendOptions, HdlError};
use opto_ir::proc::{
    AssignmentMode, BlockId, ProcBuilder, ProcTarget, ProcedureKind, SensitivityEvent,
    SwitchArmSpec, TargetSelect,
};
use opto_ir::rtl::RtlModule;
use opto_ir::word::{
    AnnotationTarget, AnnotationValueSpec, ArrayKind, BinaryOp, BitRange, CastKind, DefinitionKind,
    Edge, IndexRange, LValue, LogicStateKind, MemoryId, MemoryReadPort, MemoryReadTiming,
    PortDirection, ReadDuringWrite, SignalResolution, SourceIdentity, SourceOrigin, SourceSpan,
    SynthesisDirectiveKind, TypeLayoutFieldSpec, TypeLayoutSpec, UnaryOp, ValueId, WordModule,
    WordType,
};
use opto_ir::{BitVal, ConstBits};
use opto_slang_sys::{
    SlangArrayKind, SlangAssignmentMode, SlangAttribute, SlangAttributeValue, SlangBinaryOp,
    SlangBitRange, SlangCastKind, SlangCompilation, SlangEdge, SlangEdgeTarget, SlangExpression,
    SlangExpressionKind, SlangMaterializedModule, SlangNetResolution, SlangPortDirection,
    SlangProcedure, SlangProcedureKind, SlangSensitivityEvent, SlangSignalRef, SlangSourceSpan,
    SlangTerminatorKind, SlangTypeLayout, SlangTypeLayoutKind, SlangUnaryOp,
};
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;

mod memory;
mod module;

use memory::{
    MemorySelection, dynamic_memory_select, memory_address_constant, memory_address_offset,
    read_memory, read_memory_span, read_whole_memory, static_memory_select,
};
pub(crate) use module::compilation;

struct ModuleLowerer {
    module: WordModule,
    procedures: ProcBuilder,
    source_origins: HashMap<PathBuf, HashMap<&'static str, SourceOrigin>>,
    syntax_occurrences: HashMap<[u8; 32], u64>,
}

#[derive(Debug, Clone, Copy)]
struct SyntaxPath([u8; 32]);

impl SyntaxPath {
    fn child(self, role: u32) -> Self {
        let mut digest = blake3::Hasher::new();
        digest.update(b"opto/source-syntax-child/v1\0");
        digest.update(&self.0);
        digest.update(&role.to_le_bytes());
        Self(*digest.finalize().as_bytes())
    }

    fn named_child(self, domain: &[u8], key: &[u8], ordinal: u64) -> Self {
        let mut digest = blake3::Hasher::new();
        digest.update(b"opto/source-syntax-named-child/v1\0");
        digest.update(&self.0);
        append_identity_bytes(&mut digest, domain);
        append_identity_bytes(&mut digest, key);
        digest.update(&ordinal.to_le_bytes());
        Self(*digest.finalize().as_bytes())
    }

    const fn identity(self) -> SourceIdentity {
        SourceIdentity::from_bytes(self.0)
    }
}

impl ModuleLowerer {
    fn new(name: &str) -> Self {
        Self {
            module: WordModule::new(name),
            procedures: ProcBuilder::new(),
            source_origins: HashMap::new(),
            syntax_occurrences: HashMap::new(),
        }
    }

    fn source_span(&mut self, source: SlangSourceSpan<'_>, construct: &'static str) -> SourceSpan {
        let origin = if let Some(file) = source.file {
            if !self.source_origins.contains_key(file) {
                self.source_origins
                    .insert(file.to_path_buf(), HashMap::new());
            }
            self.source_origins
                .get_mut(file)
                .expect("source origin file was inserted")
                .entry(construct)
                .or_insert_with(|| {
                    SourceOrigin::new(
                        Some(file.to_string_lossy().into_owned()),
                        Some(construct.to_string()),
                    )
                })
                .clone()
        } else {
            SourceOrigin::new(None, Some(construct.to_string()))
        };
        SourceSpan::with_origin(origin, source.line, source.column)
    }

    fn identified_span(
        &mut self,
        source: SlangSourceSpan<'_>,
        construct: &'static str,
        path: SyntaxPath,
    ) -> SourceSpan {
        self.source_span(source, construct)
            .with_identity(path.identity())
    }

    fn declaration_span(
        &mut self,
        domain: &[u8],
        key: &[u8],
        construct: &'static str,
    ) -> SourceSpan {
        let path = self.syntax_root(domain, key);
        SourceSpan::construct(construct).with_identity(path.identity())
    }

    fn syntax_root(&mut self, domain: &[u8], key: &[u8]) -> SyntaxPath {
        let mut base = blake3::Hasher::new();
        base.update(b"opto/source-syntax-root/v1\0");
        append_identity_bytes(&mut base, self.module.name().as_bytes());
        append_identity_bytes(&mut base, domain);
        append_identity_bytes(&mut base, key);
        let base = *base.finalize().as_bytes();
        let ordinal = self.syntax_occurrences.entry(base).or_default();
        let mut identity = blake3::Hasher::new();
        identity.update(b"opto/source-syntax-occurrence/v1\0");
        identity.update(&base);
        identity.update(&ordinal.to_le_bytes());
        *ordinal = ordinal
            .checked_add(1)
            .expect("syntax occurrence count exceeds 64-bit capacity");
        SyntaxPath(*identity.finalize().as_bytes())
    }

    fn finish(mut self) -> Result<RtlModule, HdlError> {
        self.module.consolidate_names().map_err(HdlError::Ir)?;
        Ok(RtlModule::new(self.module, self.procedures.seal()?)?)
    }
}

fn append_identity_bytes(digest: &mut blake3::Hasher, bytes: &[u8]) {
    digest.update(&(bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}

impl Deref for ModuleLowerer {
    type Target = WordModule;

    fn deref(&self) -> &Self::Target {
        &self.module
    }
}

impl DerefMut for ModuleLowerer {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.module
    }
}

#[derive(Clone, Copy)]
struct SignalTarget {
    signal: opto_ir::word::SignalId,
    select: TargetSelect,
}

#[derive(Clone, Copy)]
enum AssignmentTarget {
    Signal(SignalTarget),
    Memory {
        memory: MemoryId,
        address: ValueId,
        select: TargetSelect,
    },
    MemorySpan {
        memory: MemoryId,
        address: ValueId,
        elements: NonZeroU32,
    },
    WholeMemory {
        memory: MemoryId,
    },
}

impl AssignmentTarget {
    fn continuous(self) -> Result<LValue, HdlError> {
        let Self::Signal(target) = self else {
            return Err(HdlError::unsupported(
                "verilog frontend: continuous assignments cannot write unpacked memories",
            ));
        };
        Ok(match target.select {
            TargetSelect::Whole => LValue::signal(target.signal),
            TargetSelect::Static(range) => LValue::signal(target.signal).with_range(range),
            TargetSelect::Dynamic { offset, width } => {
                LValue::signal(target.signal).with_dynamic_range(offset, width)
            }
        })
    }

    fn procedural(self) -> ProcTarget {
        match self {
            Self::Signal(target) => ProcTarget::signal(target.signal).with_select(target.select),
            Self::Memory {
                memory,
                address,
                select,
            } => ProcTarget::memory(memory, address).with_select(select),
            Self::MemorySpan { .. } | Self::WholeMemory { .. } => {
                unreachable!(
                    "multi-element memory assignments must be expanded before IR construction"
                )
            }
        }
    }

    fn coerce_value(
        self,
        module: &mut ModuleLowerer,
        value: ValueId,
        source: SourceSpan,
    ) -> Result<ValueId, HdlError> {
        let (memory, select) = match self {
            Self::Signal(_) => return Ok(value),
            Self::Memory { memory, select, .. } => (memory, select),
            Self::MemorySpan {
                memory, elements, ..
            } => {
                let definition = module
                    .memory(memory)
                    .ok_or_else(|| HdlError::invalid("verilog frontend: unknown memory"))?;
                let expected_width = definition
                    .element_type
                    .width()
                    .checked_mul(elements.get())
                    .ok_or_else(|| {
                        HdlError::invalid(
                            "verilog frontend: memory span assignment width exceeds 32-bit capacity",
                        )
                    })?;
                let actual = module
                    .value(value)
                    .ok_or_else(|| HdlError::invalid("verilog frontend: unknown assignment value"))?
                    .ty;
                if actual.width() != expected_width
                    || actual.state() != definition.element_type.state()
                {
                    return Err(HdlError::invalid(
                        "verilog frontend: memory span assignment type does not match its storage",
                    ));
                }
                return Ok(value);
            }
            Self::WholeMemory { memory } => {
                let definition = module
                    .memory(memory)
                    .ok_or_else(|| HdlError::invalid("verilog frontend: unknown memory"))?;
                let expected_width = definition
                    .element_type
                    .width()
                    .checked_mul(definition.depth.get())
                    .ok_or_else(|| {
                        HdlError::invalid(
                            "verilog frontend: whole-memory assignment width exceeds 32-bit capacity",
                        )
                    })?;
                let actual = module
                    .value(value)
                    .ok_or_else(|| HdlError::invalid("verilog frontend: unknown assignment value"))?
                    .ty;
                if actual.width() != expected_width
                    || actual.state() != definition.element_type.state()
                {
                    return Err(HdlError::invalid(
                        "verilog frontend: whole-memory assignment type does not match its storage",
                    ));
                }
                return Ok(value);
            }
        };
        let mut expected = module
            .memory(memory)
            .ok_or_else(|| HdlError::invalid("verilog frontend: unknown memory"))?
            .element_type;
        let width = match select {
            TargetSelect::Whole => expected.width(),
            TargetSelect::Static(range) => range.width(),
            TargetSelect::Dynamic { width, .. } => width.get(),
        };
        expected =
            WordType::new(width, expected.is_signed(), expected.state()).map_err(HdlError::Ir)?;
        let actual = module
            .value(value)
            .ok_or_else(|| HdlError::invalid("verilog frontend: unknown assignment value"))?
            .ty;
        if actual == expected {
            return Ok(value);
        }
        if actual.width() != expected.width() || actual.state() != expected.state() {
            return Err(HdlError::invalid(
                "verilog frontend: memory assignment type does not match its element",
            ));
        }
        module
            .cast(CastKind::ZeroExtend, value, expected, source)
            .map_err(HdlError::Ir)
    }
}

fn lower_signal_target(
    module: &WordModule,
    signal: SlangSignalRef<'_>,
) -> Result<AssignmentTarget, HdlError> {
    if signal.name.trim().is_empty() {
        return Err(HdlError::invalid(
            "verilog frontend: signal reference has empty name",
        ));
    }
    let signal_id = module.signal_id(signal.name).ok_or_else(|| {
        HdlError::invalid(format!(
            "verilog frontend: unknown signal '{}'",
            signal.name
        ))
    })?;
    Ok(AssignmentTarget::Signal(SignalTarget {
        signal: signal_id,
        select: signal.range.map_or(TargetSelect::Whole, |range| {
            TargetSelect::Static(BitRange {
                msb: range.msb,
                lsb: range.lsb,
            })
        }),
    }))
}

fn lower_target(
    module: &mut ModuleLowerer,
    expression: SlangExpression<'_>,
    path: SyntaxPath,
) -> Result<AssignmentTarget, HdlError> {
    let source = module.identified_span(
        expression.source().map_err(frontend_error)?,
        "assignment target",
        path,
    );
    match expression.kind().map_err(frontend_error)? {
        SlangExpressionKind::Signal(signal) => {
            if let Some(memory) = module.memory_id(signal.name) {
                let Some(range) = signal.range else {
                    return Ok(AssignmentTarget::WholeMemory { memory });
                };
                Ok(match static_memory_select(module, memory, range, source)? {
                    MemorySelection::Element { address, select } => AssignmentTarget::Memory {
                        memory,
                        address,
                        select,
                    },
                    MemorySelection::Span { address, elements } => AssignmentTarget::MemorySpan {
                        memory,
                        address,
                        elements,
                    },
                })
            } else {
                lower_signal_target(module, signal)
            }
        }
        SlangExpressionKind::DynamicExtract {
            value,
            offset,
            width,
        } => {
            let SlangExpressionKind::Signal(signal) = value.kind().map_err(frontend_error)? else {
                return Err(HdlError::invalid(
                    "verilog frontend: dynamic assignment target must select a signal",
                ));
            };
            if let Some(memory) = module.memory_id(signal.name) {
                if signal.range.is_some() {
                    return Err(HdlError::unsupported(
                        "verilog frontend: nested dynamic memory assignment target is not supported",
                    ));
                }
                let offset = lower_expression(module, offset, path.child(0))?;
                return Ok(
                    match dynamic_memory_select(module, memory, offset, width, source)? {
                        MemorySelection::Element { address, select } => AssignmentTarget::Memory {
                            memory,
                            address,
                            select,
                        },
                        MemorySelection::Span { address, elements } => {
                            AssignmentTarget::MemorySpan {
                                memory,
                                address,
                                elements,
                            }
                        }
                    },
                );
            }
            let AssignmentTarget::Signal(target) = lower_signal_target(module, signal)? else {
                unreachable!("memory targets were handled before signal lowering");
            };
            if signal.range.is_some() {
                return Err(HdlError::unsupported(
                    "verilog frontend: nested dynamic assignment target is not supported",
                ));
            }
            let offset = lower_expression(module, offset, path.child(0))?;
            let width = NonZeroU32::new(width).ok_or_else(|| {
                HdlError::invalid("verilog frontend: dynamic assignment width must be non-zero")
            })?;
            Ok(AssignmentTarget::Signal(SignalTarget {
                select: TargetSelect::Dynamic { offset, width },
                ..target
            }))
        }
        _ => Err(HdlError::invalid(
            "verilog frontend: assignment target is not a signal selection",
        )),
    }
}

fn target_identity(expression: SlangExpression<'_>) -> Result<Vec<u8>, HdlError> {
    let mut identity = Vec::new();
    match expression.kind().map_err(frontend_error)? {
        SlangExpressionKind::Signal(signal) => {
            identity.push(0);
            identity.extend_from_slice(&(signal.name.len() as u64).to_le_bytes());
            identity.extend_from_slice(signal.name.as_bytes());
            match signal.range {
                Some(range) => {
                    identity.push(1);
                    identity.extend_from_slice(&range.msb.to_le_bytes());
                    identity.extend_from_slice(&range.lsb.to_le_bytes());
                }
                None => identity.push(0),
            }
        }
        SlangExpressionKind::DynamicExtract { value, width, .. } => {
            identity.push(1);
            identity.extend_from_slice(&target_identity(value)?);
            identity.extend_from_slice(&width.to_le_bytes());
        }
        _ => {
            return Err(HdlError::invalid(
                "verilog frontend: assignment target must select a signal or memory",
            ));
        }
    }
    Ok(identity)
}

fn lower_signal_value(
    module: &mut ModuleLowerer,
    signal: SlangSignalRef<'_>,
    source: SourceSpan,
) -> Result<ValueId, HdlError> {
    if signal.name.trim().is_empty() {
        return Err(HdlError::invalid(
            "verilog frontend: signal reference has empty name",
        ));
    }
    let signal_id = module.signal_id(signal.name).ok_or_else(|| {
        HdlError::invalid(format!(
            "verilog frontend: unknown signal '{}'",
            signal.name
        ))
    });
    if let Some(memory) = module.memory_id(signal.name) {
        let Some(range) = signal.range else {
            return read_whole_memory(module, memory, source);
        };
        return match static_memory_select(module, memory, range, source.clone())? {
            MemorySelection::Element { address, select } => {
                read_memory(module, memory, address, select, source)
            }
            MemorySelection::Span { address, elements } => {
                read_memory_span(module, memory, address, elements, source)
            }
        };
    }
    let signal_id = signal_id?;
    if let Some(range) = signal.range {
        let lsb = range.lsb.min(range.msb);
        let width = range.msb.abs_diff(range.lsb) + 1;
        module
            .read_signal_slice(signal_id, lsb, width, source)
            .map_err(HdlError::Ir)
    } else {
        module.read_signal(signal_id, source).map_err(HdlError::Ir)
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "expression lowering is the exhaustive native-expression to Word IR dispatch; keeping \
              all variants together makes unsupported semantics and source attribution auditable"
)]
fn lower_expression(
    module: &mut ModuleLowerer,
    expression: SlangExpression<'_>,
    path: SyntaxPath,
) -> Result<ValueId, HdlError> {
    let source = expression.source().map_err(frontend_error)?;
    match expression.kind().map_err(frontend_error)? {
        SlangExpressionKind::Signal(signal) => {
            let source = module.identified_span(source, "signal read", path);
            lower_signal_value(module, signal, source)
        }
        SlangExpressionKind::Constant(constant) => {
            if constant.bits.is_empty() {
                return Err(HdlError::invalid(
                    "verilog frontend: constant expression has empty value",
                ));
            }
            let bits = ConstBits::from_bin_str(constant.bits).map_err(HdlError::Constant)?;
            let width = constant.width.unwrap_or_else(|| bits.width());
            let ty = WordType::new(width, constant.signed, LogicStateKind::FourState)
                .map_err(HdlError::Ir)?;
            let source = module.identified_span(source, "constant", path);
            module.constant(bits, ty, source).map_err(HdlError::Ir)
        }
        SlangExpressionKind::Unary { op, arg } => {
            let arg = lower_expression(module, arg, path.child(0))?;
            let source = module.identified_span(source, "unary expression", path);
            module
                .unary(lower_unary_op(op), arg, source)
                .map_err(HdlError::Ir)
        }
        SlangExpressionKind::Binary { op, left, right } => {
            let left = lower_expression(module, left, path.child(0))?;
            let right = lower_expression(module, right, path.child(1))?;
            let source = module.identified_span(source, "binary expression", path);
            module
                .binary(lower_binary_op(op), left, right, source)
                .map_err(HdlError::Ir)
        }
        SlangExpressionKind::Mux {
            condition,
            then_value,
            else_value,
        } => {
            let source = module.identified_span(source, "mux expression", path);
            let condition = lower_expression(module, condition, path.child(0))?;
            let then_value = lower_expression(module, then_value, path.child(1))?;
            let else_value = lower_expression(module, else_value, path.child(2))?;
            module
                .mux(condition, then_value, else_value, source.clone())
                .map_err(|source_error| HdlError::IrAt {
                    location: source_location_text(&source),
                    source: source_error,
                })
        }
        SlangExpressionKind::Concat(concat) => {
            if concat.parts().len() == 0 {
                return Err(HdlError::invalid(
                    "verilog frontend: concat expression is empty",
                ));
            }
            let mut parts = Vec::with_capacity(concat.parts().len());
            for (index, part) in concat.parts().enumerate() {
                let role = u32::try_from(index).map_err(|_| {
                    HdlError::invalid("concat operand count exceeds 32-bit capacity")
                })?;
                parts.push(lower_expression(
                    module,
                    part.map_err(frontend_error)?,
                    path.child(role),
                )?);
            }
            let source = module.identified_span(source, "concat expression", path);
            module.concat(parts, source).map_err(HdlError::Ir)
        }
        SlangExpressionKind::Cast {
            kind,
            value,
            width,
            signed,
        } => {
            let value = lower_expression(module, value, path.child(0))?;
            let target =
                WordType::new(width, signed, LogicStateKind::FourState).map_err(HdlError::Ir)?;
            let source = module.identified_span(source, "conversion expression", path);
            module
                .cast(
                    match kind {
                        SlangCastKind::ZeroExtend => CastKind::ZeroExtend,
                        SlangCastKind::SignExtend => CastKind::SignExtend,
                        SlangCastKind::Truncate => CastKind::Truncate,
                    },
                    value,
                    target,
                    source,
                )
                .map_err(HdlError::Ir)
        }
        SlangExpressionKind::Extract { value, lsb, width } => {
            let value = lower_expression(module, value, path.child(0))?;
            let source = module.identified_span(source, "select expression", path);
            module
                .extract(value, lsb, width, source)
                .map_err(HdlError::Ir)
        }
        SlangExpressionKind::DynamicExtract {
            value,
            offset,
            width,
        } => {
            if let SlangExpressionKind::Signal(signal) = value.kind().map_err(frontend_error)?
                && signal.range.is_none()
                && let Some(memory) = module.memory_id(signal.name)
            {
                let offset = lower_expression(module, offset, path.child(1))?;
                let source = module.identified_span(source, "dynamic memory read", path);
                return match dynamic_memory_select(module, memory, offset, width, source.clone())? {
                    MemorySelection::Element { address, select } => {
                        read_memory(module, memory, address, select, source)
                    }
                    MemorySelection::Span { address, elements } => {
                        read_memory_span(module, memory, address, elements, source)
                    }
                };
            }
            let value = lower_expression(module, value, path.child(0))?;
            let offset = lower_expression(module, offset, path.child(1))?;
            let source = module.identified_span(source, "dynamic select expression", path);
            module
                .dynamic_extract(value, offset, width, source)
                .map_err(HdlError::Ir)
        }
    }
}

fn source_location_text(source: &SourceSpan) -> String {
    let file = source.file().unwrap_or("<unknown>");
    match (source.line(), source.column()) {
        (Some(line), Some(column)) => format!("{file}:{line}:{column}"),
        (Some(line), None) => format!("{file}:{line}"),
        _ => file.to_string(),
    }
}

fn lower_procedure(
    module: &mut ModuleLowerer,
    procedure: SlangProcedure<'_>,
) -> Result<(), HdlError> {
    let (kind, construct) = match procedure.kind().map_err(frontend_error)? {
        SlangProcedureKind::Comb => (ProcedureKind::Combinational, "always_comb"),
        SlangProcedureKind::Latch => (ProcedureKind::Latch, "always_latch"),
        SlangProcedureKind::Flop => (ProcedureKind::FlipFlop, "always_ff"),
        SlangProcedureKind::CombOrLatch => (ProcedureKind::CombinationalOrLatch, "always"),
    };
    let events = procedure
        .events()
        .map(|event| lower_sensitivity_event(module, event))
        .collect::<Result<Vec<_>, _>>()?;
    let blocks = procedure.blocks().collect::<Vec<_>>();
    if blocks.is_empty() {
        return Err(HdlError::invalid(
            "verilog frontend: procedural CFG has no blocks",
        ));
    }
    let mut targets = blocks
        .iter()
        .flat_map(|block| block.effects())
        .map(|effect| target_identity(effect.lhs().map_err(frontend_error)?))
        .collect::<Result<Vec<_>, HdlError>>()?;
    targets.sort();
    let mut procedure_key = Vec::new();
    procedure_key.extend_from_slice(&(construct.len() as u64).to_le_bytes());
    procedure_key.extend_from_slice(construct.as_bytes());
    for event in &events {
        let signal = module.signal(event.signal).ok_or_else(|| {
            HdlError::invalid("verilog frontend: procedure event references an unknown signal")
        })?;
        let name = signal.name.map_or("", |name| module.name_str(name));
        procedure_key.extend_from_slice(&(name.len() as u64).to_le_bytes());
        procedure_key.extend_from_slice(name.as_bytes());
        procedure_key.push(match event.edge {
            Edge::Pos => 0,
            Edge::Neg => 1,
        });
    }
    for target in &targets {
        procedure_key.extend_from_slice(&(target.len() as u64).to_le_bytes());
        procedure_key.extend_from_slice(target);
    }
    let procedure_path = module.syntax_root(b"procedure", &procedure_key);
    let source = module.identified_span(
        procedure.source().map_err(frontend_error)?,
        construct,
        procedure_path,
    );
    let procedure_id = if kind == ProcedureKind::FlipFlop {
        module.procedures.add_clocked_procedure(events, source)?
    } else {
        if !events.is_empty() {
            return Err(HdlError::invalid(
                "verilog frontend: combinational procedure has edge sensitivity",
            ));
        }
        module
            .procedures
            .add_combinational_procedure(kind, source)?
    };

    for (index, block) in blocks.iter().enumerate() {
        if block.id().index() != index {
            return Err(HdlError::invalid(
                "verilog frontend: procedural block arena is not dense",
            ));
        }
    }
    let entry = procedure.entry().index();
    if entry >= blocks.len() {
        return Err(HdlError::invalid(
            "verilog frontend: procedural entry block is out of range",
        ));
    }

    let mut block_ids = vec![None; blocks.len()];
    for (index, block) in blocks.iter().enumerate() {
        let role = u32::try_from(index)
            .map_err(|_| HdlError::invalid("procedural block count exceeds 32-bit capacity"))?;
        let source = module.identified_span(
            block.source().map_err(frontend_error)?,
            "procedural block",
            procedure_path.named_child(b"block", &role.to_le_bytes(), 0),
        );
        block_ids[index] = Some(module.procedures.add_block(procedure_id, source)?);
    }
    module
        .procedures
        .set_entry(procedure_id, mapped_block(&block_ids, procedure.entry())?)?;
    let mut effect_ordinals = HashMap::<Vec<u8>, u64>::new();
    for block in blocks {
        let id = mapped_block(&block_ids, block.id())?;
        for effect in block.effects() {
            let mode = match effect.mode() {
                SlangAssignmentMode::Blocking => AssignmentMode::Blocking,
                SlangAssignmentMode::Nonblocking => AssignmentMode::Nonblocking,
            };
            let lhs = effect.lhs().map_err(frontend_error)?;
            let target_key = target_identity(lhs)?;
            let ordinal = effect_ordinals.entry(target_key.clone()).or_default();
            let effect_path = procedure_path.named_child(b"effect", &target_key, *ordinal);
            *ordinal = ordinal
                .checked_add(1)
                .expect("procedural effect count exceeds 64-bit capacity");
            let source = module.identified_span(
                effect.source().map_err(frontend_error)?,
                match mode {
                    AssignmentMode::Blocking => "blocking assignment",
                    AssignmentMode::Nonblocking => "nonblocking assignment",
                },
                effect_path,
            );
            let target = lower_target(module, lhs, effect_path.child(0))?;
            let value = lower_expression(
                module,
                effect.rhs().map_err(frontend_error)?,
                effect_path.child(1),
            )?;
            let value = target.coerce_value(module, value, source.clone())?;
            assign_procedural_target(module, id, mode, target, value, source)?;
        }
        lower_terminator(module, id, &block_ids, block.terminator(), procedure_path)?;
    }
    Ok(())
}

fn assign_procedural_target(
    module: &mut ModuleLowerer,
    block: BlockId,
    mode: AssignmentMode,
    target: AssignmentTarget,
    value: ValueId,
    source: SourceSpan,
) -> Result<(), HdlError> {
    let (memory, base_address, elements) = match target {
        AssignmentTarget::WholeMemory { memory } => {
            let depth = module
                .memory(memory)
                .ok_or_else(|| HdlError::invalid("verilog frontend: unknown memory"))?
                .depth;
            (memory, None, depth)
        }
        AssignmentTarget::MemorySpan {
            memory,
            address,
            elements,
        } => (memory, Some(address), elements),
        _ => {
            module
                .procedures
                .assign(block, mode, target.procedural(), value, source)?;
            return Ok(());
        }
    };
    let definition = module
        .memory(memory)
        .ok_or_else(|| HdlError::invalid("verilog frontend: unknown memory"))?;
    let element_width = definition.element_type.width();
    let depth = definition.depth;
    for offset in 0..elements.get() {
        let lsb = offset.checked_mul(element_width).ok_or_else(|| {
            HdlError::invalid(
                "verilog frontend: memory span assignment offset exceeds 32-bit capacity",
            )
        })?;
        let element = module
            .extract(value, lsb, element_width, source.clone())
            .map_err(HdlError::Ir)?;
        let address_value = match base_address {
            Some(address) => memory_address_offset(module, address, offset, source.clone())?,
            None => memory_address_constant(module, offset, depth, source.clone())?,
        };
        let element_target = AssignmentTarget::Memory {
            memory,
            address: address_value,
            select: TargetSelect::Whole,
        };
        let element = element_target.coerce_value(module, element, source.clone())?;
        module.procedures.assign(
            block,
            mode,
            element_target.procedural(),
            element,
            source.clone(),
        )?;
    }
    Ok(())
}

fn lower_sensitivity_event(
    module: &WordModule,
    event: SlangSensitivityEvent<'_>,
) -> Result<SensitivityEvent, HdlError> {
    let signal_ref = event.signal().map_err(frontend_error)?;
    if signal_ref.range.is_some() {
        return Err(HdlError::unsupported(
            "verilog frontend: always_ff event signal cannot be a range select",
        ));
    }
    let signal = module.signal_id(signal_ref.name).ok_or_else(|| {
        HdlError::invalid(format!(
            "verilog frontend: unknown always_ff event signal '{}'",
            signal_ref.name
        ))
    })?;
    let ty = module
        .signal(signal)
        .ok_or_else(|| {
            HdlError::invalid(format!("verilog frontend: unknown RTL signal {signal:?}"))
        })?
        .ty;
    if ty.width() != 1 {
        return Err(HdlError::invalid(format!(
            "verilog frontend: always_ff event signal '{}' must be 1 bit wide, got {}",
            signal_ref.name,
            ty.width()
        )));
    }
    Ok(SensitivityEvent {
        signal,
        edge: match event.edge().map_err(frontend_error)? {
            SlangEdge::Pos => Edge::Pos,
            SlangEdge::Neg => Edge::Neg,
        },
    })
}

fn mapped_block(
    blocks: &[Option<BlockId>],
    id: opto_slang_sys::SlangBlockId,
) -> Result<BlockId, HdlError> {
    blocks.get(id.index()).copied().flatten().ok_or_else(|| {
        HdlError::invalid("verilog frontend: procedural edge target is out of range")
    })
}

fn lower_terminator(
    module: &mut ModuleLowerer,
    block: BlockId,
    blocks: &[Option<BlockId>],
    terminator: opto_slang_sys::SlangTerminator<'_>,
    procedure_path: SyntaxPath,
) -> Result<(), HdlError> {
    let block_raw = u32::try_from(block.index())
        .map_err(|_| HdlError::invalid("procedural block index exceeds 32-bit capacity"))?;
    let path = procedure_path.named_child(b"terminator", &block_raw.to_le_bytes(), 0);
    match terminator.kind().map_err(frontend_error)? {
        SlangTerminatorKind::Return => {
            let source = module.identified_span(
                terminator.source().map_err(frontend_error)?,
                "procedural return",
                path,
            );
            module.procedures.terminate_return(block, source)?;
        }
        SlangTerminatorKind::Jump(edge) => {
            let (target, source) = lower_edge(module, blocks, edge, "procedural jump", path)?;
            module.procedures.terminate_jump(block, target, source)?;
        }
        SlangTerminatorKind::Branch {
            condition,
            then_edge,
            else_edge,
        } => {
            let condition = lower_expression(module, condition, path.child(0))?;
            let then_target = mapped_block(blocks, then_edge.block)?;
            let else_target = mapped_block(blocks, else_edge.block)?;
            let source = module.identified_span(
                terminator.source().map_err(frontend_error)?,
                "if statement",
                path,
            );
            module.procedures.terminate_branch(
                block,
                condition,
                then_target,
                else_target,
                source,
            )?;
        }
        SlangTerminatorKind::Switch {
            selector,
            arms,
            default,
        } => {
            let selector = lower_expression(module, selector, path.child(0))?;
            let mut lowered_arms = Vec::with_capacity(arms.len());
            for (index, arm) in arms.iter().enumerate() {
                let edge = arm.edge().map_err(frontend_error)?;
                let role = u32::try_from(index).map_err(|_| {
                    HdlError::invalid("case arm count exceeds 32-bit syntax-path capacity")
                })?;
                let arm_role = role.checked_add(1).ok_or_else(|| {
                    HdlError::invalid("case arm count exceeds 32-bit syntax-path capacity")
                })?;
                lowered_arms.push(SwitchArmSpec {
                    pattern: lower_expression(
                        module,
                        arm.pattern().map_err(frontend_error)?,
                        path.child(arm_role),
                    )?,
                    target: mapped_block(blocks, edge.block)?,
                    source: module.identified_span(edge.source, "case item", path.child(arm_role)),
                });
            }
            let source = module.identified_span(
                terminator.source().map_err(frontend_error)?,
                "case statement",
                path,
            );
            module.procedures.terminate_switch(
                block,
                selector,
                lowered_arms,
                mapped_block(blocks, default.block)?,
                source,
            )?;
        }
    }
    Ok(())
}

fn lower_edge(
    module: &mut ModuleLowerer,
    blocks: &[Option<BlockId>],
    edge: SlangEdgeTarget<'_>,
    construct: &'static str,
    path: SyntaxPath,
) -> Result<(BlockId, SourceSpan), HdlError> {
    Ok((
        mapped_block(blocks, edge.block)?,
        module.identified_span(edge.source, construct, path),
    ))
}

fn lower_unary_op(op: SlangUnaryOp) -> UnaryOp {
    match op {
        SlangUnaryOp::LogicalNot => UnaryOp::LogicalNot,
        SlangUnaryOp::BitNot => UnaryOp::BitNot,
        SlangUnaryOp::ReductionAnd => UnaryOp::ReductionAnd,
        SlangUnaryOp::ReductionOr => UnaryOp::ReductionOr,
        SlangUnaryOp::ReductionXor => UnaryOp::ReductionXor,
    }
}

fn lower_binary_op(op: SlangBinaryOp) -> BinaryOp {
    match op {
        SlangBinaryOp::Add => BinaryOp::Add,
        SlangBinaryOp::Sub => BinaryOp::Sub,
        SlangBinaryOp::Mul => BinaryOp::Mul,
        SlangBinaryOp::Div => BinaryOp::Div,
        SlangBinaryOp::Mod => BinaryOp::Mod,
        SlangBinaryOp::BitAnd => BinaryOp::BitAnd,
        SlangBinaryOp::BitOr => BinaryOp::BitOr,
        SlangBinaryOp::BitXor => BinaryOp::BitXor,
        SlangBinaryOp::LogicalAnd => BinaryOp::LogicalAnd,
        SlangBinaryOp::LogicalOr => BinaryOp::LogicalOr,
        SlangBinaryOp::Eq => BinaryOp::Eq,
        SlangBinaryOp::Ne => BinaryOp::Ne,
        SlangBinaryOp::Lt => BinaryOp::Lt,
        SlangBinaryOp::Le => BinaryOp::Le,
        SlangBinaryOp::Gt => BinaryOp::Gt,
        SlangBinaryOp::Ge => BinaryOp::Ge,
        SlangBinaryOp::Shl => BinaryOp::Shl,
        SlangBinaryOp::Shr => BinaryOp::Shr,
        SlangBinaryOp::Ashr => BinaryOp::Ashr,
    }
}

fn frontend_error(error: opto_slang_sys::SlangError) -> HdlError {
    HdlError::Slang(error)
}
