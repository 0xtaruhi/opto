// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use opto_ir::word;

#[derive(Debug, Default)]
pub(super) struct RewriteScratch {
    cache: Vec<Option<word::ValueId>>,
    touched: Vec<usize>,
    visited: Vec<bool>,
    visit_touched: Vec<usize>,
}

impl RewriteScratch {
    fn begin(&mut self, values: usize) {
        for index in self.touched.drain(..) {
            self.cache[index] = None;
        }
        self.cache.resize(values, None);
    }

    pub(super) fn begin_visit(&mut self, values: usize) {
        for index in self.visit_touched.drain(..) {
            self.visited[index] = false;
        }
        self.visited.resize(values, false);
    }

    pub(super) fn visit(&mut self, value: word::ValueId) -> Result<bool, crate::SynthError> {
        let reached = self.visited.get_mut(value.index()).ok_or_else(|| {
            crate::SynthError::invariant("procedural expression references an unknown value")
        })?;
        if *reached {
            return Ok(false);
        }
        *reached = true;
        self.visit_touched.push(value.index());
        Ok(true)
    }
}

pub(super) fn rewrite_value(
    module: &mut word::WordModule,
    value: word::ValueId,
    scratch: &mut RewriteScratch,
    resolve: impl FnMut(
        &mut word::WordModule,
        word::ValueId,
        word::SignalRef,
    ) -> Result<Option<word::ValueId>, crate::SynthError>,
) -> Result<word::ValueId, crate::SynthError> {
    scratch.begin(module.values().len());
    Rewriter {
        module,
        cache: &mut scratch.cache,
        touched: &mut scratch.touched,
        resolve,
    }
    .rewrite(value)
}

struct Rewriter<'a, F> {
    module: &'a mut word::WordModule,
    cache: &'a mut Vec<Option<word::ValueId>>,
    touched: &'a mut Vec<usize>,
    resolve: F,
}

#[derive(Clone, Copy)]
enum ValueNode {
    Signal(word::SignalRef),
    Constant,
    Operation(word::OpId),
}

#[derive(Clone, Copy)]
enum OperationNode {
    Unary(word::UnaryOp, word::ValueId),
    Binary(word::BinaryOp, word::ValueId, word::ValueId),
    Mux(word::ValueId, word::ValueId, word::ValueId),
    TriState(word::ValueId, word::Enable),
    Concat(usize),
    Extract(word::ValueId, u32, std::num::NonZeroU32),
    DynamicExtract(word::ValueId, word::ValueId, std::num::NonZeroU32),
    DynamicInsert(word::ValueId, word::ValueId, word::ValueId),
    Cast(word::CastKind, word::ValueId, word::WordType),
    Sequential,
}

impl<F> Rewriter<'_, F>
where
    F: FnMut(
        &mut word::WordModule,
        word::ValueId,
        word::SignalRef,
    ) -> Result<Option<word::ValueId>, crate::SynthError>,
{
    fn rewrite(&mut self, id: word::ValueId) -> Result<word::ValueId, crate::SynthError> {
        if let Some(value) = self.cache.get(id.index()).copied().flatten() {
            return Ok(value);
        }
        let node = match &self
            .module
            .value(id)
            .ok_or_else(|| crate::SynthError::invariant(format!("unknown RTL value {id:?}")))?
            .kind
        {
            word::ValueKind::Signal(reference) => ValueNode::Signal(*reference),
            word::ValueKind::Constant(_) => ValueNode::Constant,
            word::ValueKind::Operation(operation) => ValueNode::Operation(*operation),
        };
        let rewritten = match node {
            ValueNode::Signal(reference) => {
                (self.resolve)(self.module, id, reference)?.unwrap_or(id)
            }
            ValueNode::Constant => id,
            ValueNode::Operation(operation) => self.operation(id, operation)?,
        };
        let slot = &mut self.cache[id.index()];
        debug_assert!(slot.is_none());
        *slot = Some(rewritten);
        self.touched.push(id.index());
        Ok(rewritten)
    }

    fn operation_record(
        &self,
        operation: word::OpId,
    ) -> Result<&word::Operation, crate::SynthError> {
        self.module.operation(operation).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "unknown procedural expression operation {operation:?}"
            ))
        })
    }

    fn operation(
        &mut self,
        original: word::ValueId,
        operation: word::OpId,
    ) -> Result<word::ValueId, crate::SynthError> {
        use word::OpKind;
        let node = match &self
            .module
            .operation(operation)
            .ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "unknown procedural expression operation {operation:?}"
                ))
            })?
            .kind
        {
            OpKind::Unary { op, arg } => OperationNode::Unary(*op, *arg),
            OpKind::Binary { op, left, right } => OperationNode::Binary(*op, *left, *right),
            OpKind::Mux {
                cond,
                then_value,
                else_value,
            } => OperationNode::Mux(*cond, *then_value, *else_value),
            OpKind::TriState { data, enable } => OperationNode::TriState(*data, *enable),
            OpKind::Concat { parts } => OperationNode::Concat(parts.len()),
            OpKind::Extract { value, lsb, width } => OperationNode::Extract(*value, *lsb, *width),
            OpKind::DynamicExtract {
                value,
                offset,
                width,
            } => OperationNode::DynamicExtract(*value, *offset, *width),
            OpKind::DynamicInsert {
                value,
                offset,
                replacement,
            } => OperationNode::DynamicInsert(*value, *offset, *replacement),
            OpKind::Cast {
                kind,
                value,
                target,
            } => OperationNode::Cast(*kind, *value, *target),
            OpKind::Register(_) | OpKind::Latch(_) => OperationNode::Sequential,
        };
        match node {
            OperationNode::Unary(op, arg) => {
                let rewritten = self.rewrite(arg)?;
                self.changed(original, operation, rewritten == arg, |module, source| {
                    module.unary(op, rewritten, source)
                })
            }
            OperationNode::Binary(op, left, right) => {
                let rewritten_left = self.rewrite(left)?;
                let rewritten_right = self.rewrite(right)?;
                self.changed(
                    original,
                    operation,
                    rewritten_left == left && rewritten_right == right,
                    |module, source| module.binary(op, rewritten_left, rewritten_right, source),
                )
            }
            OperationNode::Mux(cond, then_value, else_value) => {
                let rewritten_cond = self.rewrite(cond)?;
                let rewritten_then = self.rewrite(then_value)?;
                let rewritten_else = self.rewrite(else_value)?;
                self.changed(
                    original,
                    operation,
                    rewritten_cond == cond
                        && rewritten_then == then_value
                        && rewritten_else == else_value,
                    |module, source| {
                        module.mux(rewritten_cond, rewritten_then, rewritten_else, source)
                    },
                )
            }
            OperationNode::TriState(data, enable) => {
                let rewritten_data = self.rewrite(data)?;
                let rewritten_enable = self.rewrite(enable.value)?;
                self.changed(
                    original,
                    operation,
                    rewritten_data == data && rewritten_enable == enable.value,
                    |module, source| {
                        module.tri_state(
                            rewritten_data,
                            word::Enable {
                                value: rewritten_enable,
                                active_high: enable.active_high,
                            },
                            source,
                        )
                    },
                )
            }
            OperationNode::Concat(len) => {
                let mut rewritten = Vec::with_capacity(len);
                let mut unchanged = true;
                for index in 0..len {
                    let part = match &self.operation_record(operation)?.kind {
                        OpKind::Concat { parts } => parts[index],
                        _ => unreachable!("operation kind is immutable"),
                    };
                    let value = self.rewrite(part)?;
                    unchanged &= value == part;
                    rewritten.push(value);
                }
                self.changed(original, operation, unchanged, |module, source| {
                    module.concat(rewritten, source)
                })
            }
            OperationNode::Extract(value, lsb, width) => {
                let rewritten = self.rewrite(value)?;
                self.changed(original, operation, rewritten == value, |module, source| {
                    module.extract(rewritten, lsb, width.get(), source)
                })
            }
            OperationNode::DynamicExtract(value, offset, width) => {
                let rewritten_value = self.rewrite(value)?;
                let rewritten_offset = self.rewrite(offset)?;
                self.changed(
                    original,
                    operation,
                    rewritten_value == value && rewritten_offset == offset,
                    |module, source| {
                        module.dynamic_extract(
                            rewritten_value,
                            rewritten_offset,
                            width.get(),
                            source,
                        )
                    },
                )
            }
            OperationNode::DynamicInsert(value, offset, replacement) => {
                let rewritten_value = self.rewrite(value)?;
                let rewritten_offset = self.rewrite(offset)?;
                let rewritten_replacement = self.rewrite(replacement)?;
                self.changed(
                    original,
                    operation,
                    rewritten_value == value
                        && rewritten_offset == offset
                        && rewritten_replacement == replacement,
                    |module, source| {
                        module.dynamic_insert(
                            rewritten_value,
                            rewritten_offset,
                            rewritten_replacement,
                            source,
                        )
                    },
                )
            }
            OperationNode::Cast(kind, value, target) => {
                let rewritten = self.rewrite(value)?;
                self.changed(original, operation, rewritten == value, |module, source| {
                    module.cast(kind, rewritten, target, source)
                })
            }
            OperationNode::Sequential => Err(crate::SynthError::unsupported(
                "sequential operation inside a procedural expression",
            )),
        }
    }

    fn changed(
        &mut self,
        original: word::ValueId,
        operation: word::OpId,
        unchanged: bool,
        build: impl FnOnce(
            &mut word::WordModule,
            word::SourceSpan,
        ) -> Result<word::ValueId, word::WordError>,
    ) -> Result<word::ValueId, crate::SynthError> {
        if unchanged {
            Ok(original)
        } else {
            let source = self.operation_record(operation)?.source.clone();
            build(self.module, source).map_err(crate::SynthError::from)
        }
    }
}
