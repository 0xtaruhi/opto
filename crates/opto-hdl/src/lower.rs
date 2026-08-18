// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::{DbUpdate, FrontendOptions, HdlError};
use opto_ir::proc::{
    AssignmentMode, BlockId, LoopAnalysisLimits, LoopForm, LoopRegion, LoopRegionId, ProcExprId,
    ProcExprKind, ProcLocal, ProcLocalId, ProcedureKind, SensitivityEvent, SwitchArmSpec,
    TargetSelect, TransientProcBuilder, TransientTarget, TransientTargetSelect,
};
use opto_ir::rtl::RtlModule;
use opto_ir::word::{
    AnnotationTarget, AnnotationValueSpec, ArrayKind, BinaryOp, BitRange, CastKind, DefinitionKind,
    Edge, Enable, IndexRange, LValue, LogicStateKind, MemoryId, MemoryReadPort, MemoryReadTiming,
    PortDirection, ReadDuringWrite, SignalResolution, SourceIdentity, SourceOrigin, SourceSpan,
    SynthesisDirectiveKind, TypeLayoutFieldSpec, TypeLayoutSpec, UnaryOp, ValueId, WordModule,
    WordType,
};
use opto_ir::{BitVal, ConstBits};
use opto_slang_sys::{
    SlangArrayKind, SlangAssignmentMode, SlangAttribute, SlangAttributeValue, SlangBinaryOp,
    SlangBitRange, SlangCastKind, SlangCompilation, SlangEdge, SlangEdgeTarget, SlangExpression,
    SlangExpressionKind, SlangLoopForm, SlangMaterializedModule, SlangNetResolution,
    SlangPortDirection, SlangProcedure, SlangProcedureKind, SlangSensitivityEvent, SlangSignalRef,
    SlangSourceSpan, SlangTerminatorKind, SlangTypeLayout, SlangTypeLayoutKind, SlangUnaryOp,
};
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;

mod memory;
mod module;

use memory::{
    MemorySelection, dynamic_memory_select, read_memory, read_memory_span, read_whole_memory,
    static_memory_select,
};
pub(crate) use module::compilation;

struct ModuleLowerer {
    module: WordModule,
    procedures: TransientProcBuilder,
    process_locals: HashMap<String, ProcLocalId>,
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
            procedures: TransientProcBuilder::new(),
            process_locals: HashMap::new(),
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
        let procedures = std::mem::take(&mut self.procedures)
            .seal()?
            .promote_loop_signal_state(&self.module)?
            .prove_and_eliminate_loops(&self.module, LoopAnalysisLimits::default())?
            .materialize_locals(&mut self.module)?
            .materialize_acyclic(&mut self.module)?;
        self.module.consolidate_names().map_err(HdlError::Ir)?;
        Ok(RtlModule::new(self.module, procedures)?)
    }

    fn add_process_local(
        &mut self,
        name: &str,
        ty: WordType,
        source: SourceSpan,
    ) -> Result<ProcLocalId, HdlError> {
        if self.process_locals.contains_key(name) {
            return Err(HdlError::invalid(format!(
                "verilog frontend: duplicate process-local name '{name}'"
            )));
        }
        let local = self.procedures.add_local(ProcLocal {
            name: name.into(),
            ty,
            source,
        })?;
        self.process_locals.insert(name.to_string(), local);
        Ok(local)
    }

    fn process_local(&self, name: &str) -> Option<ProcLocalId> {
        self.process_locals.get(name).copied()
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
enum ProceduralAssignmentTarget {
    Single(TransientTarget),
    MemorySpan {
        memory: MemoryId,
        address: ProcExprId,
        elements: NonZeroU32,
    },
    WholeMemory {
        memory: MemoryId,
    },
}

impl ProceduralAssignmentTarget {
    fn coerce_value(
        self,
        module: &mut ModuleLowerer,
        value: ProcExprId,
        source: SourceSpan,
    ) -> Result<ProcExprId, HdlError> {
        let actual = module
            .procedures
            .expression_type(value)
            .ok_or_else(|| HdlError::invalid("verilog frontend: unknown assignment value"))?;
        let expected = match self {
            Self::Single(TransientTarget::Signal { .. } | TransientTarget::Local { .. }) => {
                return Ok(value);
            }
            Self::Single(TransientTarget::Memory { memory, select, .. }) => {
                let element = module
                    .memory(memory)
                    .ok_or_else(|| HdlError::invalid("verilog frontend: unknown memory"))?
                    .element_type;
                let width = match select {
                    TransientTargetSelect::Whole => element.width(),
                    TransientTargetSelect::Static(range) => range.width(),
                    TransientTargetSelect::Dynamic { width, .. } => width.get(),
                };
                WordType::new(width, element.is_signed(), element.state()).map_err(HdlError::Ir)?
            }
            Self::MemorySpan {
                memory, elements, ..
            } => {
                let element = module
                    .memory(memory)
                    .ok_or_else(|| HdlError::invalid("verilog frontend: unknown memory"))?
                    .element_type;
                let width = element.width().checked_mul(elements.get()).ok_or_else(|| {
                    HdlError::invalid(
                        "verilog frontend: memory span assignment width exceeds capacity",
                    )
                })?;
                WordType::new(width, false, element.state()).map_err(HdlError::Ir)?
            }
            Self::WholeMemory { memory } => {
                let definition = module
                    .memory(memory)
                    .ok_or_else(|| HdlError::invalid("verilog frontend: unknown memory"))?;
                let width = definition
                    .element_type
                    .width()
                    .checked_mul(definition.depth.get())
                    .ok_or_else(|| {
                        HdlError::invalid(
                            "verilog frontend: whole-memory assignment width exceeds capacity",
                        )
                    })?;
                WordType::new(width, false, definition.element_type.state())
                    .map_err(HdlError::Ir)?
            }
        };
        if actual == expected {
            return Ok(value);
        }
        if actual.width() != expected.width() || actual.state() != expected.state() {
            return Err(HdlError::invalid(
                "verilog frontend: procedural memory assignment type does not match storage",
            ));
        }
        Ok(module
            .procedures
            .cast(CastKind::ZeroExtend, value, expected, source)?)
    }
}

impl SignalTarget {
    fn continuous(self) -> LValue {
        let target = self;
        match target.select {
            TargetSelect::Whole => LValue::signal(target.signal),
            TargetSelect::Static(range) => LValue::signal(target.signal).with_range(range),
            TargetSelect::Dynamic { offset, width } => {
                LValue::signal(target.signal).with_dynamic_range(offset, width)
            }
        }
    }
}

fn import_proc_value(
    module: &mut ModuleLowerer,
    value: ValueId,
    source: SourceSpan,
) -> Result<ProcExprId, HdlError> {
    let ty = module
        .value(value)
        .ok_or_else(|| HdlError::invalid("verilog frontend: unknown procedural expression value"))?
        .ty;
    Ok(module.procedures.add_module_value(value, ty, source)?)
}

fn import_proc_select(
    module: &mut ModuleLowerer,
    select: TargetSelect,
    source: SourceSpan,
) -> Result<TransientTargetSelect, HdlError> {
    Ok(match select {
        TargetSelect::Whole => TransientTargetSelect::Whole,
        TargetSelect::Static(range) => TransientTargetSelect::Static(range),
        TargetSelect::Dynamic { offset, width } => TransientTargetSelect::Dynamic {
            offset: import_proc_value(module, offset, source)?,
            width,
        },
    })
}

fn lower_signal_target(
    module: &WordModule,
    signal: SlangSignalRef<'_>,
) -> Result<SignalTarget, HdlError> {
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
    Ok(SignalTarget {
        signal: signal_id,
        select: signal.range.map_or(TargetSelect::Whole, |range| {
            TargetSelect::Static(BitRange {
                msb: range.msb,
                lsb: range.lsb,
            })
        }),
    })
}

fn lower_target(
    module: &mut ModuleLowerer,
    expression: SlangExpression<'_>,
    path: SyntaxPath,
) -> Result<SignalTarget, HdlError> {
    match expression.kind().map_err(frontend_error)? {
        SlangExpressionKind::Signal(signal) => {
            if module.memory_id(signal.name).is_some() {
                return Err(HdlError::unsupported(
                    "verilog frontend: continuous assignments cannot write unpacked memories",
                ));
            }
            lower_signal_target(module, signal)
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
            if module.memory_id(signal.name).is_some() {
                return Err(HdlError::unsupported(
                    "verilog frontend: continuous assignments cannot write unpacked memories",
                ));
            }
            let target = lower_signal_target(module, signal)?;
            if signal.range.is_some() {
                return Err(HdlError::unsupported(
                    "verilog frontend: nested dynamic assignment target is not supported",
                ));
            }
            let offset = lower_expression(module, offset, path.child(0))?;
            let width = NonZeroU32::new(width).ok_or_else(|| {
                HdlError::invalid("verilog frontend: dynamic assignment width must be non-zero")
            })?;
            Ok(SignalTarget {
                select: TargetSelect::Dynamic { offset, width },
                ..target
            })
        }
        _ => Err(HdlError::invalid(
            "verilog frontend: assignment target is not a signal selection",
        )),
    }
}

fn lower_procedural_target(
    module: &mut ModuleLowerer,
    expression: SlangExpression<'_>,
    path: SyntaxPath,
) -> Result<ProceduralAssignmentTarget, HdlError> {
    let source = module.identified_span(
        expression.source().map_err(frontend_error)?,
        "procedural assignment target",
        path,
    );
    match expression.kind().map_err(frontend_error)? {
        SlangExpressionKind::Signal(signal) => {
            if let Some(local) = module.process_local(signal.name) {
                let select = signal.range.map_or(TransientTargetSelect::Whole, |range| {
                    TransientTargetSelect::Static(BitRange {
                        msb: range.msb,
                        lsb: range.lsb,
                    })
                });
                return Ok(ProceduralAssignmentTarget::Single(
                    TransientTarget::local(local).with_select(select),
                ));
            }
            if let Some(memory) = module.memory_id(signal.name) {
                let Some(range) = signal.range else {
                    return Ok(ProceduralAssignmentTarget::WholeMemory { memory });
                };
                return Ok(
                    match procedural_static_memory_select(module, memory, range, &source)? {
                        ProceduralMemorySelection::Element { address, select } => {
                            ProceduralAssignmentTarget::Single(TransientTarget::Memory {
                                memory,
                                address,
                                select,
                            })
                        }
                        ProceduralMemorySelection::Span { address, elements } => {
                            ProceduralAssignmentTarget::MemorySpan {
                                memory,
                                address,
                                elements,
                            }
                        }
                    },
                );
            }
            let target = lower_signal_target(module, signal)?;
            Ok(ProceduralAssignmentTarget::Single(
                TransientTarget::signal(target.signal).with_select(import_proc_select(
                    module,
                    target.select,
                    source,
                )?),
            ))
        }
        SlangExpressionKind::DynamicExtract {
            value,
            offset,
            width,
        } => {
            let SlangExpressionKind::Signal(signal) = value.kind().map_err(frontend_error)? else {
                return Err(HdlError::invalid(
                    "verilog frontend: dynamic procedural target must select a signal",
                ));
            };
            if signal.range.is_some() {
                return Err(HdlError::unsupported(
                    "verilog frontend: nested dynamic procedural target is not supported",
                ));
            }
            let offset = lower_procedural_expression(module, offset, path.child(0))?;
            if let Some(memory) = module.memory_id(signal.name) {
                return Ok(
                    match procedural_dynamic_memory_select(module, memory, offset, width, source)? {
                        ProceduralMemorySelection::Element { address, select } => {
                            ProceduralAssignmentTarget::Single(TransientTarget::Memory {
                                memory,
                                address,
                                select,
                            })
                        }
                        ProceduralMemorySelection::Span { address, elements } => {
                            ProceduralAssignmentTarget::MemorySpan {
                                memory,
                                address,
                                elements,
                            }
                        }
                    },
                );
            }
            let width = NonZeroU32::new(width).ok_or_else(|| {
                HdlError::invalid("verilog frontend: dynamic assignment width must be non-zero")
            })?;
            let select = TransientTargetSelect::Dynamic { offset, width };
            if let Some(local) = module.process_local(signal.name) {
                return Ok(ProceduralAssignmentTarget::Single(
                    TransientTarget::local(local).with_select(select),
                ));
            }
            let signal_id = module.signal_id(signal.name).ok_or_else(|| {
                HdlError::invalid(format!(
                    "verilog frontend: unknown signal '{}'",
                    signal.name
                ))
            })?;
            Ok(ProceduralAssignmentTarget::Single(
                TransientTarget::signal(signal_id).with_select(select),
            ))
        }
        _ => Err(HdlError::invalid(
            "verilog frontend: procedural target is not a signal selection",
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
            let then_is_high_impedance = is_high_impedance_expression(then_value)?;
            let else_is_high_impedance = is_high_impedance_expression(else_value)?;
            if then_is_high_impedance ^ else_is_high_impedance {
                let source = module.identified_span(source, "tri-state expression", path);
                let condition = lower_expression(module, condition, path.child(0))?;
                let (data, active_high) = if else_is_high_impedance {
                    (lower_expression(module, then_value, path.child(1))?, true)
                } else {
                    (lower_expression(module, else_value, path.child(2))?, false)
                };
                return module
                    .tri_state(
                        data,
                        Enable {
                            value: condition,
                            active_high,
                        },
                        source.clone(),
                    )
                    .map_err(|source_error| HdlError::IrAt {
                        location: source_location_text(&source),
                        source: source_error,
                    });
            }
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

#[derive(Clone, Copy)]
enum ProceduralMemorySelection {
    Element {
        address: ProcExprId,
        select: TransientTargetSelect,
    },
    Span {
        address: ProcExprId,
        elements: NonZeroU32,
    },
}

#[allow(
    clippy::too_many_lines,
    reason = "this exhaustive dispatch owns the native-expression to transient Proc expression boundary"
)]
fn lower_procedural_expression(
    module: &mut ModuleLowerer,
    expression: SlangExpression<'_>,
    path: SyntaxPath,
) -> Result<ProcExprId, HdlError> {
    let source_view = expression.source().map_err(frontend_error)?;
    match expression.kind().map_err(frontend_error)? {
        SlangExpressionKind::Signal(signal) => {
            let source = module.identified_span(source_view, "procedural signal read", path);
            lower_procedural_signal_value(module, signal, source)
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
            let source = module.identified_span(source_view, "procedural constant", path);
            Ok(module.procedures.constant(bits, ty, source)?)
        }
        SlangExpressionKind::Unary { op, arg } => {
            let arg = lower_procedural_expression(module, arg, path.child(0))?;
            let source = module.identified_span(source_view, "procedural unary expression", path);
            Ok(module.procedures.unary(lower_unary_op(op), arg, source)?)
        }
        SlangExpressionKind::Binary { op, left, right } => {
            let left = lower_procedural_expression(module, left, path.child(0))?;
            let right = lower_procedural_expression(module, right, path.child(1))?;
            let source = module.identified_span(source_view, "procedural binary expression", path);
            Ok(module
                .procedures
                .binary(lower_binary_op(op), left, right, source)?)
        }
        SlangExpressionKind::Mux {
            condition,
            then_value,
            else_value,
        } => {
            let source = module.identified_span(source_view, "procedural mux expression", path);
            let condition = lower_procedural_expression(module, condition, path.child(0))?;
            let then_is_high_impedance = is_high_impedance_expression(then_value)?;
            let else_is_high_impedance = is_high_impedance_expression(else_value)?;
            if then_is_high_impedance ^ else_is_high_impedance {
                let (data, active_high) = if else_is_high_impedance {
                    (
                        lower_procedural_expression(module, then_value, path.child(1))?,
                        true,
                    )
                } else {
                    (
                        lower_procedural_expression(module, else_value, path.child(2))?,
                        false,
                    )
                };
                return Ok(module
                    .procedures
                    .tri_state(data, condition, active_high, source)?);
            }
            let then_value = lower_procedural_expression(module, then_value, path.child(1))?;
            let else_value = lower_procedural_expression(module, else_value, path.child(2))?;
            Ok(module
                .procedures
                .mux(condition, then_value, else_value, source)?)
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
                parts.push(lower_procedural_expression(
                    module,
                    part.map_err(frontend_error)?,
                    path.child(role),
                )?);
            }
            let source = module.identified_span(source_view, "procedural concat expression", path);
            Ok(module.procedures.concat(parts, source)?)
        }
        SlangExpressionKind::Cast {
            kind,
            value,
            width,
            signed,
        } => {
            let value = lower_procedural_expression(module, value, path.child(0))?;
            let target =
                WordType::new(width, signed, LogicStateKind::FourState).map_err(HdlError::Ir)?;
            let source =
                module.identified_span(source_view, "procedural conversion expression", path);
            Ok(module.procedures.cast(
                match kind {
                    SlangCastKind::ZeroExtend => CastKind::ZeroExtend,
                    SlangCastKind::SignExtend => CastKind::SignExtend,
                    SlangCastKind::Truncate => CastKind::Truncate,
                },
                value,
                target,
                source,
            )?)
        }
        SlangExpressionKind::Extract { value, lsb, width } => {
            let value = lower_procedural_expression(module, value, path.child(0))?;
            let source = module.identified_span(source_view, "procedural select expression", path);
            Ok(module.procedures.extract(value, lsb, width, source)?)
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
                let offset = lower_procedural_expression(module, offset, path.child(1))?;
                let source =
                    module.identified_span(source_view, "procedural dynamic memory read", path);
                let selection = procedural_dynamic_memory_select(
                    module,
                    memory,
                    offset,
                    width,
                    source.clone(),
                )?;
                return lower_procedural_memory_selection(module, memory, selection, source);
            }
            let value = lower_procedural_expression(module, value, path.child(0))?;
            let offset = lower_procedural_expression(module, offset, path.child(1))?;
            let source =
                module.identified_span(source_view, "procedural dynamic select expression", path);
            Ok(module
                .procedures
                .dynamic_extract(value, offset, width, source)?)
        }
    }
}

fn lower_procedural_signal_value(
    module: &mut ModuleLowerer,
    signal: SlangSignalRef<'_>,
    source: SourceSpan,
) -> Result<ProcExprId, HdlError> {
    if let Some(local) = module.process_local(signal.name) {
        let value = module.procedures.read_local(local, source.clone())?;
        let Some(range) = signal.range else {
            return Ok(value);
        };
        let value = module.procedures.extract(
            value,
            range.msb.min(range.lsb),
            range.msb.abs_diff(range.lsb) + 1,
            source.clone(),
        )?;
        let ty = module
            .procedures
            .expression_type(value)
            .ok_or_else(|| HdlError::invalid("verilog frontend: unknown process-local slice"))?;
        if !ty.is_signed() {
            return Ok(value);
        }
        let unsigned = WordType::new(ty.width(), false, ty.state()).map_err(HdlError::Ir)?;
        return Ok(module
            .procedures
            .cast(CastKind::ZeroExtend, value, unsigned, source)?);
    }
    if let Some(memory) = module.memory_id(signal.name) {
        let selection = if let Some(range) = signal.range {
            procedural_static_memory_select(module, memory, range, &source)?
        } else {
            let depth = module
                .memory(memory)
                .ok_or_else(|| HdlError::invalid("verilog frontend: unknown memory"))?
                .depth;
            let address = procedural_memory_address_constant(module, 0, depth, source.clone())?;
            ProceduralMemorySelection::Span {
                address,
                elements: depth,
            }
        };
        return lower_procedural_memory_selection(module, memory, selection, source);
    }
    let value = lower_signal_value(module, signal, source.clone())?;
    import_proc_value(module, value, source)
}

fn lower_procedural_memory_selection(
    module: &mut ModuleLowerer,
    memory: MemoryId,
    selection: ProceduralMemorySelection,
    source: SourceSpan,
) -> Result<ProcExprId, HdlError> {
    let element_type = module
        .memory(memory)
        .ok_or_else(|| HdlError::invalid("verilog frontend: unknown memory"))?
        .element_type;
    match selection {
        ProceduralMemorySelection::Element { address, select } => {
            let width = match select {
                TransientTargetSelect::Whole => element_type.width(),
                TransientTargetSelect::Static(range) => range.width(),
                TransientTargetSelect::Dynamic { width, .. } => width.get(),
            };
            let ty = WordType::new(width, element_type.is_signed(), element_type.state())
                .map_err(HdlError::Ir)?;
            Ok(module
                .procedures
                .memory_read(memory, address, select, ty, source)?)
        }
        ProceduralMemorySelection::Span { address, elements } => {
            let mut values = Vec::with_capacity(elements.get() as usize);
            for offset in (0..elements.get()).rev() {
                let address =
                    procedural_memory_address_offset(module, address, offset, source.clone())?;
                values.push(module.procedures.memory_read(
                    memory,
                    address,
                    TransientTargetSelect::Whole,
                    element_type,
                    source.clone(),
                )?);
            }
            Ok(module.procedures.concat(values, source)?)
        }
    }
}

fn procedural_static_memory_select(
    module: &mut ModuleLowerer,
    memory: MemoryId,
    range: SlangBitRange,
    source: &SourceSpan,
) -> Result<ProceduralMemorySelection, HdlError> {
    let definition = module
        .memory(memory)
        .ok_or_else(|| HdlError::invalid("verilog frontend: unknown memory"))?;
    let element_width = definition.element_type.width();
    let depth = definition.depth;
    let lsb = range.lsb.min(range.msb);
    let width = range.msb.abs_diff(range.lsb) + 1;
    let address_value = lsb / element_width;
    let element_lsb = lsb % element_width;
    let end = lsb
        .checked_add(width)
        .ok_or_else(|| HdlError::invalid("verilog frontend: memory selection range overflows"))?;
    if address_value >= depth.get()
        || end
            > element_width.checked_mul(depth.get()).ok_or_else(|| {
                HdlError::invalid("verilog frontend: flattened memory width exceeds capacity")
            })?
    {
        return Err(HdlError::unsupported(
            "verilog frontend: memory selection is outside storage bounds",
        ));
    }
    let address = procedural_memory_address_constant(module, address_value, depth, source.clone())?;
    if width > element_width {
        if element_lsb != 0 || !width.is_multiple_of(element_width) {
            return Err(HdlError::unsupported(
                "verilog frontend: a static memory span must contain whole elements",
            ));
        }
        return Ok(ProceduralMemorySelection::Span {
            address,
            elements: NonZeroU32::new(width / element_width)
                .expect("a wider aligned memory span contains at least one element"),
        });
    }
    if element_lsb
        .checked_add(width)
        .is_none_or(|selection_end| selection_end > element_width)
    {
        return Err(HdlError::unsupported(
            "verilog frontend: a static memory selection cannot cross an element boundary",
        ));
    }
    let select = if element_lsb == 0 && width == element_width {
        TransientTargetSelect::Whole
    } else {
        TransientTargetSelect::Static(BitRange {
            msb: element_lsb + width - 1,
            lsb: element_lsb,
        })
    };
    Ok(ProceduralMemorySelection::Element { address, select })
}

fn procedural_dynamic_memory_select(
    module: &mut ModuleLowerer,
    memory: MemoryId,
    offset: ProcExprId,
    width: u32,
    source: SourceSpan,
) -> Result<ProceduralMemorySelection, HdlError> {
    let definition = module
        .memory(memory)
        .ok_or_else(|| HdlError::invalid("verilog frontend: unknown memory"))?;
    let element_width = definition.element_type.width();
    let depth = definition.depth;
    let offset_type = module
        .procedures
        .expression_type(offset)
        .ok_or_else(|| HdlError::invalid("verilog frontend: unknown memory offset"))?;
    if offset_type.is_signed() {
        return Err(HdlError::invalid(
            "verilog frontend: memory offsets must be unsigned",
        ));
    }
    let width = NonZeroU32::new(width).ok_or_else(|| {
        HdlError::invalid("verilog frontend: dynamic memory selection width must be non-zero")
    })?;
    let scale = procedural_unsigned_constant(module, element_width, offset_type, source.clone())?;
    let address = if element_width == 1 {
        offset
    } else if let Some(address) = scaled_procedural_memory_address(module, offset, element_width) {
        address
    } else {
        module
            .procedures
            .binary(BinaryOp::Div, offset, scale, source.clone())?
    };
    let address = canonical_procedural_memory_address(module, address, depth, source.clone())?;
    if width.get() > element_width {
        if !width.get().is_multiple_of(element_width) {
            return Err(HdlError::unsupported(
                "verilog frontend: a dynamic memory span must contain whole elements",
            ));
        }
        return Ok(ProceduralMemorySelection::Span {
            address,
            elements: NonZeroU32::new(width.get() / element_width)
                .expect("a wider aligned memory span contains at least one element"),
        });
    }
    let select = if width.get() == element_width {
        TransientTargetSelect::Whole
    } else {
        TransientTargetSelect::Dynamic {
            offset: module
                .procedures
                .binary(BinaryOp::Mod, offset, scale, source)?,
            width,
        }
    };
    Ok(ProceduralMemorySelection::Element { address, select })
}

fn scaled_procedural_memory_address(
    module: &ModuleLowerer,
    offset: ProcExprId,
    element_width: u32,
) -> Option<ProcExprId> {
    let ProcExprKind::Binary {
        op: BinaryOp::Mul,
        left,
        right,
    } = module.procedures.expression(offset)?.kind
    else {
        return None;
    };
    if procedural_unsigned_constant_value(module, right) == Some(element_width) {
        Some(left)
    } else if procedural_unsigned_constant_value(module, left) == Some(element_width) {
        Some(right)
    } else {
        None
    }
}

fn canonical_procedural_memory_address(
    module: &mut ModuleLowerer,
    address: ProcExprId,
    depth: NonZeroU32,
    source: SourceSpan,
) -> Result<ProcExprId, HdlError> {
    let address_type = module
        .procedures
        .expression_type(address)
        .ok_or_else(|| HdlError::invalid("verilog frontend: unknown procedural memory address"))?;
    let width = (u32::BITS - (depth.get() - 1).leading_zeros()).max(1);
    if address_type.width() <= width {
        return Ok(address);
    }
    let maximum = procedural_unsigned_maximum(module, address);
    if maximum.is_none_or(|maximum| maximum >= u128::from(depth.get())) {
        return Ok(address);
    }
    let ty = WordType::new(width, false, address_type.state()).map_err(HdlError::Ir)?;
    Ok(module
        .procedures
        .cast(CastKind::Truncate, address, ty, source)?)
}

fn procedural_unsigned_maximum(module: &ModuleLowerer, value: ProcExprId) -> Option<u128> {
    let expression = module.procedures.expression(value)?;
    match &expression.kind {
        ProcExprKind::ModuleValue(value) => opto_ir::word::unsigned_value_range(module, *value)
            .map(opto_ir::word::UnsignedValueRange::maximum),
        ProcExprKind::Constant(_) => {
            procedural_unsigned_constant_value(module, value).map(u128::from)
        }
        ProcExprKind::Cast { kind, value } => {
            let maximum = procedural_unsigned_maximum(module, *value)?;
            match kind {
                CastKind::ZeroExtend => Some(maximum),
                CastKind::SignExtend => module
                    .procedures
                    .expression_type(*value)
                    .is_some_and(|ty| !ty.is_signed())
                    .then_some(maximum),
                CastKind::Truncate => Some(maximum.min(if expression.ty.width() >= u128::BITS {
                    u128::MAX
                } else {
                    (1u128 << expression.ty.width()) - 1
                })),
            }
        }
        _ => None,
    }
}

fn procedural_unsigned_constant_value(module: &ModuleLowerer, value: ProcExprId) -> Option<u32> {
    let expression = module.procedures.expression(value)?;
    let bits = match &expression.kind {
        ProcExprKind::Constant(bits) => bits,
        ProcExprKind::ModuleValue(value) => {
            let opto_ir::word::ValueKind::Constant(bits) = &module.value(*value)?.kind else {
                return None;
            };
            bits
        }
        _ => return None,
    };
    bits.as_slice().iter().try_fold(0u32, |value, bit| {
        let bit = match bit {
            BitVal::Zero => 0,
            BitVal::One => 1,
            BitVal::X | BitVal::Z => return None,
        };
        value.checked_mul(2)?.checked_add(bit)
    })
}

fn procedural_memory_address_constant(
    module: &mut ModuleLowerer,
    address: u32,
    depth: NonZeroU32,
    source: SourceSpan,
) -> Result<ProcExprId, HdlError> {
    let width = (u32::BITS - (depth.get() - 1).leading_zeros()).max(1);
    let ty = WordType::new(width, false, LogicStateKind::FourState).map_err(HdlError::Ir)?;
    procedural_unsigned_constant(module, address, ty, source)
}

fn procedural_memory_address_offset(
    module: &mut ModuleLowerer,
    address: ProcExprId,
    offset: u32,
    source: SourceSpan,
) -> Result<ProcExprId, HdlError> {
    if offset == 0 {
        return Ok(address);
    }
    let ty = module
        .procedures
        .expression_type(address)
        .ok_or_else(|| HdlError::invalid("verilog frontend: unknown memory address"))?;
    let offset = procedural_unsigned_constant(module, offset, ty, source.clone())?;
    Ok(module
        .procedures
        .binary(BinaryOp::Add, address, offset, source)?)
}

fn procedural_unsigned_constant(
    module: &mut ModuleLowerer,
    value: u32,
    ty: WordType,
    source: SourceSpan,
) -> Result<ProcExprId, HdlError> {
    if ty.is_signed() || value.checked_shr(ty.width()).unwrap_or(0) != 0 {
        return Err(HdlError::invalid(
            "verilog frontend: procedural constant exceeds its unsigned type",
        ));
    }
    let bits = (0..ty.width())
        .rev()
        .map(|bit| {
            if value.checked_shr(bit).unwrap_or(0) & 1 == 0 {
                BitVal::Zero
            } else {
                BitVal::One
            }
        })
        .collect();
    Ok(module.procedures.constant(
        ConstBits::from_bits(bits).map_err(HdlError::Constant)?,
        ty,
        source,
    )?)
}

fn is_high_impedance_expression(expression: SlangExpression<'_>) -> Result<bool, HdlError> {
    Ok(matches!(
        expression.kind().map_err(frontend_error)?,
        SlangExpressionKind::Constant(constant)
            if !constant.bits.is_empty() && constant.bits.bytes().all(|bit| bit == b'z')
    ))
}

fn is_tri_state_expression(expression: SlangExpression<'_>) -> Result<bool, HdlError> {
    let SlangExpressionKind::Mux {
        then_value,
        else_value,
        ..
    } = expression.kind().map_err(frontend_error)?
    else {
        return Ok(false);
    };
    Ok(is_high_impedance_expression(then_value)? ^ is_high_impedance_expression(else_value)?)
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
    let source_events = procedure.events().collect::<Vec<_>>();
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
    for event in &source_events {
        let expression = event.expression().map_err(frontend_error)?;
        let identity = target_identity(expression)?;
        procedure_key.extend_from_slice(&(identity.len() as u64).to_le_bytes());
        procedure_key.extend_from_slice(&identity);
        procedure_key.push(match event.edge().map_err(frontend_error)? {
            SlangEdge::Pos => 0,
            SlangEdge::Neg => 1,
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
    let events = source_events
        .into_iter()
        .enumerate()
        .map(|(index, event)| {
            let ordinal = u64::try_from(index).map_err(|_| {
                HdlError::invalid("sensitivity event count exceeds 64-bit capacity")
            })?;
            lower_sensitivity_event(
                module,
                event,
                procedure_path.named_child(b"event", &[], ordinal),
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
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
    let mut loop_region_ids = Vec::<Option<LoopRegionId>>::new();
    for (index, region) in procedure.loop_regions().enumerate() {
        let role = u32::try_from(index)
            .map_err(|_| HdlError::invalid("loop-region count exceeds 32-bit capacity"))?;
        let source = module.identified_span(
            region.source().map_err(frontend_error)?,
            "procedural loop region",
            procedure_path.named_child(b"loop-region", &role.to_le_bytes(), 0),
        );
        let parent = region
            .parent()
            .map(|parent| {
                loop_region_ids
                    .get(parent.index())
                    .copied()
                    .flatten()
                    .ok_or_else(|| {
                        HdlError::invalid(
                            "verilog frontend: loop parent must precede its child region",
                        )
                    })
            })
            .transpose()?;
        let id = module.procedures.add_loop_region(LoopRegion {
            procedure: procedure_id,
            header: mapped_block(&block_ids, region.header())?,
            body: mapped_block(&block_ids, region.body())?,
            latch: mapped_block(&block_ids, region.latch())?,
            exit: mapped_block(&block_ids, region.exit())?,
            form: match region.form().map_err(frontend_error)? {
                SlangLoopForm::PreTest => LoopForm::PreTest,
                SlangLoopForm::PostTest => LoopForm::PostTest,
                SlangLoopForm::Unconditional => LoopForm::Unconditional,
            },
            parent,
            source,
        })?;
        loop_region_ids.push(Some(id));
    }
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
            let target = lower_procedural_target(module, lhs, effect_path.child(0))?;
            let value = lower_procedural_expression(
                module,
                effect.rhs().map_err(frontend_error)?,
                effect_path.child(1),
            )?;
            let value = target.coerce_value(module, value, source.clone())?;
            assign_owned_procedural_target(module, id, mode, target, value, source)?;
        }
        lower_terminator(module, id, &block_ids, block.terminator(), procedure_path)?;
    }
    Ok(())
}

fn assign_owned_procedural_target(
    module: &mut ModuleLowerer,
    block: BlockId,
    mode: AssignmentMode,
    target: ProceduralAssignmentTarget,
    value: ProcExprId,
    source: SourceSpan,
) -> Result<(), HdlError> {
    let (memory, base_address, elements) = match target {
        ProceduralAssignmentTarget::WholeMemory { memory } => {
            let depth = module
                .memory(memory)
                .ok_or_else(|| HdlError::invalid("verilog frontend: unknown memory"))?
                .depth;
            (memory, None, depth)
        }
        ProceduralAssignmentTarget::MemorySpan {
            memory,
            address,
            elements,
        } => (memory, Some(address), elements),
        ProceduralAssignmentTarget::Single(target) => {
            module
                .procedures
                .assign(block, mode, target, value, source)?;
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
            .procedures
            .extract(value, lsb, element_width, source.clone())?;
        let address_value = match base_address {
            Some(address) => {
                procedural_memory_address_offset(module, address, offset, source.clone())?
            }
            None => procedural_memory_address_constant(module, offset, depth, source.clone())?,
        };
        let element_target = ProceduralAssignmentTarget::Single(TransientTarget::Memory {
            memory,
            address: address_value,
            select: TransientTargetSelect::Whole,
        });
        let element = element_target.coerce_value(module, element, source.clone())?;
        let ProceduralAssignmentTarget::Single(element_target) = element_target else {
            unreachable!("an expanded memory element is a single procedural target");
        };
        module
            .procedures
            .assign(block, mode, element_target, element, source.clone())?;
    }
    Ok(())
}

fn lower_sensitivity_event(
    module: &mut ModuleLowerer,
    event: SlangSensitivityEvent<'_>,
    path: SyntaxPath,
) -> Result<SensitivityEvent, HdlError> {
    let expression = lower_expression(
        module,
        event.expression().map_err(frontend_error)?,
        path.child(0),
    )?;
    let ty = module
        .value(expression)
        .map(|value| value.ty)
        .ok_or_else(|| {
            HdlError::invalid("verilog frontend: sensitivity event expression disappeared")
        })?;
    if ty.width() != 1 {
        return Err(HdlError::invalid(format!(
            "verilog frontend: always_ff event expression must be 1 bit wide, got {}",
            ty.width()
        )));
    }
    let qualifier = event
        .qualifier()
        .map_err(frontend_error)?
        .map(|qualifier| lower_expression(module, qualifier, path.child(1)))
        .transpose()?;
    if qualifier.is_some_and(|value| {
        module
            .value(value)
            .is_none_or(|stored| stored.ty.width() != 1)
    }) {
        return Err(HdlError::invalid(
            "verilog frontend: sensitivity event iff qualifier must be 1 bit wide",
        ));
    }
    Ok(SensitivityEvent {
        value: expression,
        edge: match event.edge().map_err(frontend_error)? {
            SlangEdge::Pos => Edge::Pos,
            SlangEdge::Neg => Edge::Neg,
        },
        iff: qualifier,
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
            let condition = lower_procedural_expression(module, condition, path.child(0))?;
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
            let selector = lower_procedural_expression(module, selector, path.child(0))?;
            let mut lowered_arms = Vec::with_capacity(arms.len());
            for (index, arm) in arms.iter().enumerate() {
                let edge = arm.edge().map_err(frontend_error)?;
                let role = u32::try_from(index).map_err(|_| {
                    HdlError::invalid("case arm count exceeds 32-bit syntax-path capacity")
                })?;
                let arm_role = role.checked_add(1).ok_or_else(|| {
                    HdlError::invalid("case arm count exceeds 32-bit syntax-path capacity")
                })?;
                let arm_source =
                    module.identified_span(edge.source, "case item", path.child(arm_role));
                let pattern = lower_procedural_expression(
                    module,
                    arm.pattern().map_err(frontend_error)?,
                    path.child(arm_role),
                )?;
                lowered_arms.push(SwitchArmSpec {
                    pattern,
                    target: mapped_block(blocks, edge.block)?,
                    source: arm_source,
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
