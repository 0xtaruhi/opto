// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

#[cfg(test)]
use super::SourceSpan;
use super::{
    AnnotationValue, BinaryOp, CastKind, DefinitionKind, Enable, InstId, LogicStateKind, OpId,
    OpKind, PortId, Reset, ResetKind, SignalId, SignalKind, SignalResolution, TypeLayoutId,
    TypeLayoutKind, UnaryOp, ValueId, ValueKind, WordError, WordModule, WordType,
};
use crate::value::BitVal;

#[derive(Clone, Copy)]
struct ValidationEntry {
    name: crate::NameId,
    start: u32,
    end: u32,
}

impl WordModule {
    /// Validates every serialized structural invariant without rebuilding IR.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] on the first invalid name, typed ID, arena range,
    /// type contract, topological edge, instance binding, annotation, or memory
    /// port invariant.
    pub fn validate(&self) -> Result<(), WordError> {
        let mut scratch = Vec::with_capacity(self.validation_scratch_entries());
        self.validate_names_and_layouts(&mut scratch)?;
        self.validate_definition_and_annotations()?;
        self.validate_ports_and_signals()?;
        self.validate_values_and_operations()?;
        self.validate_connects_and_instances(&mut scratch)?;
        self.validate_memories()
    }

    fn validate_definition_and_annotations(&self) -> Result<(), WordError> {
        if self.definition_kind == DefinitionKind::BlackBox
            && (self.signals.len() != self.ports.len()
                || self
                    .signals
                    .iter()
                    .any(|signal| !matches!(signal.kind, SignalKind::Port(_)))
                || !self.memories.is_empty()
                || !self.memory_read_ports.is_empty()
                || !self.memory_write_ports.is_empty()
                || !self.values.is_empty()
                || !self.operations.is_empty()
                || !self.connects.is_empty()
                || !self.instances.is_empty())
        {
            return Err(WordError::new(
                "black-box definition must contain only its declared ports",
            ));
        }
        for annotation in &self.annotations {
            self.validate_annotation_target(annotation.target)?;
            let name = self
                .resolve_name(annotation.name)
                .ok_or_else(|| WordError::new("annotation name does not resolve"))?;
            if name.is_empty() {
                return Err(WordError::new("annotation name cannot be empty"));
            }
            match annotation.value {
                AnnotationValue::Integer { bits, width, .. } => {
                    let bits = self
                        .resolve_name(bits)
                        .ok_or_else(|| WordError::new("annotation integer bits do not resolve"))?;
                    if width == 0
                        || bits.len() != width as usize
                        || !bits
                            .bytes()
                            .all(|bit| matches!(bit, b'0' | b'1' | b'x' | b'z'))
                    {
                        return Err(WordError::new(
                            "annotation integer has an invalid bit representation",
                        ));
                    }
                }
                AnnotationValue::String(value) | AnnotationValue::Other(value) => {
                    if self.resolve_name(value).is_none() {
                        return Err(WordError::new("annotation value does not resolve"));
                    }
                }
            }
        }
        for directive in &self.synthesis_directives {
            self.validate_synthesis_directive(directive.target, directive.kind)?;
        }
        for (index, directive) in self.synthesis_directives.iter().enumerate() {
            if self.synthesis_directives[..index].iter().any(|previous| {
                previous.target == directive.target && previous.kind == directive.kind
            }) {
                return Err(WordError::new(
                    "synthesis directive target and kind must be unique",
                ));
            }
        }
        Ok(())
    }

    /// Deterministic upper bound for the sole temporary arena used by
    /// [`Self::validate`].
    #[must_use]
    pub fn validation_memory_bytes(&self) -> usize {
        opto_core::resident::slice_bytes::<ValidationEntry>(self.validation_scratch_entries())
    }

    fn validation_scratch_entries(&self) -> usize {
        self.type_layouts
            .iter()
            .filter_map(|layout| match &layout.kind {
                TypeLayoutKind::Struct { fields } => Some(fields.len()),
                TypeLayoutKind::Scalar | TypeLayoutKind::Array { .. } => None,
            })
            .chain(
                self.instances
                    .iter()
                    .map(|instance| instance.connections.len()),
            )
            .max()
            .unwrap_or(0)
    }

    fn validate_names_and_layouts(
        &self,
        scratch: &mut Vec<ValidationEntry>,
    ) -> Result<(), WordError> {
        let name = self
            .resolve_name(self.name)
            .ok_or_else(|| WordError::new("module name does not resolve"))?;
        if name.trim().is_empty() {
            return Err(WordError::new("module name cannot be empty"));
        }
        for (index, layout) in self.type_layouts.iter().enumerate() {
            let child_width = |id: TypeLayoutId| {
                if id.index() >= index {
                    return Err(WordError::new(
                        "type layouts must reference an earlier arena entry",
                    ));
                }
                Ok(self.type_layouts[id.index()].width)
            };
            let expected = match &layout.kind {
                TypeLayoutKind::Scalar => 1,
                TypeLayoutKind::Array { range, element, .. } => range
                    .width()?
                    .checked_mul(child_width(*element)?)
                    .ok_or_else(|| WordError::new("array type layout width overflow"))?,
                TypeLayoutKind::Struct { fields } => {
                    if fields.is_empty() {
                        return Err(WordError::new("struct type layout has no fields"));
                    }
                    scratch.clear();
                    for field in fields {
                        let name = self.resolve_name(field.name).ok_or_else(|| {
                            WordError::new("struct type layout field name does not resolve")
                        })?;
                        if name.is_empty() {
                            return Err(WordError::new(
                                "struct type layout field names must be nonempty",
                            ));
                        }
                        let end = field
                            .bit_offset
                            .checked_add(child_width(field.layout)?)
                            .ok_or_else(|| WordError::new("struct type layout width overflow"))?;
                        scratch.push(ValidationEntry {
                            name: field.name,
                            start: field.bit_offset,
                            end,
                        });
                    }
                    scratch.sort_unstable_by_key(|entry| entry.name);
                    if scratch.windows(2).any(|pair| pair[0].name == pair[1].name) {
                        return Err(WordError::new(
                            "struct type layout field names must be unique",
                        ));
                    }
                    scratch.sort_unstable_by_key(|entry| entry.start);
                    if scratch[0].start != 0
                        || scratch.windows(2).any(|pair| pair[0].end != pair[1].start)
                    {
                        return Err(WordError::new(
                            "struct type layout fields must be contiguous and non-overlapping",
                        ));
                    }
                    scratch.last().expect("nonempty layout spans").end
                }
            };
            if layout.width == 0 || layout.width != expected {
                return Err(WordError::new(format!(
                    "type layout {index} stores width {}, expected {expected}",
                    layout.width
                )));
            }
        }
        Ok(())
    }

    fn validate_ports_and_signals(&self) -> Result<(), WordError> {
        for (index, port) in self.ports.iter().enumerate() {
            let id = PortId::from_index(index)?;
            let signal = self
                .signal(port.signal)
                .ok_or_else(|| WordError::new(format!("port {id:?} has an unknown signal")))?;
            if self.resolve_name(port.name).is_none()
                || signal.name != Some(port.name)
                || signal.kind != SignalKind::Port(id)
                || signal.ty != port.ty
            {
                return Err(WordError::new(format!(
                    "port {id:?} disagrees with its signal"
                )));
            }
        }
        for (index, signal) in self.signals.iter().enumerate() {
            let id = SignalId::from_index(index)?;
            if let Some(name) = signal.name
                && (self.resolve_name(name).is_none()
                    || self
                        .named_signals
                        .get(name.raw() as usize)
                        .copied()
                        .flatten()
                        != Some(id)
                    || self
                        .named_memories
                        .get(name.raw() as usize)
                        .copied()
                        .flatten()
                        .is_some())
            {
                return Err(WordError::new(format!(
                    "signal {id:?} has an invalid or conflicting name"
                )));
            }
            if let SignalKind::Port(port) = signal.kind
                && self.port(port).is_none_or(|port| port.signal != id)
            {
                return Err(WordError::new(format!(
                    "signal {id:?} references an invalid port"
                )));
            }
            if signal.resolution != SignalResolution::SingleDriver
                && matches!(signal.kind, SignalKind::Register | SignalKind::ProcessLocal)
            {
                return Err(WordError::new(format!(
                    "signal {id:?} has invalid wired resolution"
                )));
            }
            if let Some(layout) = signal.type_layout {
                let layout = self.type_layouts.get(layout.index()).ok_or_else(|| {
                    WordError::new(format!("signal {id:?} has an unknown type layout"))
                })?;
                if layout.width != signal.ty.width() {
                    return Err(WordError::new(format!(
                        "signal {id:?} type layout width disagrees with its type"
                    )));
                }
            }
        }
        for (slot, signal) in self.named_signals.iter().enumerate() {
            if let Some(signal) = signal {
                let stored = self.signal(*signal).ok_or_else(|| {
                    WordError::new("signal name index references an unknown signal")
                })?;
                if stored.name.is_none_or(|name| name.raw() as usize != slot) {
                    return Err(WordError::new(
                        "signal name index disagrees with the stored signal",
                    ));
                }
            }
        }
        Ok(())
    }

    fn validate_values_and_operations(&self) -> Result<(), WordError> {
        for (index, value) in self.values.iter().enumerate() {
            let id = ValueId::from_index(index)?;
            match &value.kind {
                ValueKind::Signal(reference) => {
                    let signal = self.signal(reference.signal).ok_or_else(|| {
                        WordError::new(format!("value {id:?} references an unknown signal"))
                    })?;
                    let end = reference
                        .lsb
                        .checked_add(reference.width())
                        .ok_or_else(|| WordError::new("signal reference range overflow"))?;
                    let expected = if reference.lsb == 0 && end == signal.ty.width() {
                        signal.ty
                    } else {
                        if end > signal.ty.width() {
                            return Err(WordError::new(format!(
                                "value {id:?} signal reference exceeds its signal"
                            )));
                        }
                        WordType::new(reference.width(), false, signal.ty.state())?
                    };
                    if value.ty != expected {
                        return Err(WordError::new(format!(
                            "value {id:?} signal reference has the wrong type"
                        )));
                    }
                }
                ValueKind::Constant(bits) => {
                    if bits.width() != value.ty.width()
                        || value.ty.state() == LogicStateKind::TwoState
                            && bits
                                .as_slice()
                                .iter()
                                .any(|bit| matches!(bit, BitVal::X | BitVal::Z))
                    {
                        return Err(WordError::new(format!(
                            "value {id:?} contains an invalid constant"
                        )));
                    }
                }
                ValueKind::Operation(operation) => {
                    let operation = self.operation(*operation).ok_or_else(|| {
                        WordError::new(format!("value {id:?} references an unknown operation"))
                    })?;
                    if operation.result != id {
                        return Err(WordError::new(format!(
                            "value {id:?} disagrees with its producing operation"
                        )));
                    }
                }
            }
        }
        for (index, operation) in self.operations.iter().enumerate() {
            let id = OpId::from_index(index)?;
            let result = self
                .value(operation.result)
                .ok_or_else(|| WordError::new(format!("operation {id:?} has an unknown result")))?;
            if result.kind != ValueKind::Operation(id) {
                return Err(WordError::new(format!(
                    "operation {id:?} does not own its result value"
                )));
            }
            operation.kind.try_for_each_input(|input| {
                self.value(input).ok_or_else(|| {
                    WordError::new(format!("operation {id:?} has an unknown input"))
                })?;
                if input.index() >= operation.result.index() {
                    return Err(WordError::new(format!(
                        "operation {id:?} is not topologically ordered"
                    )));
                }
                Ok(())
            })?;
            let expected = self.operation_type(&operation.kind)?;
            if result.ty != expected {
                return Err(WordError::new(format!(
                    "operation {id:?} result type {:?} differs from {:?}",
                    result.ty, expected
                )));
            }
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the exhaustive operation typing table is clearer and safer as one match"
    )]
    fn operation_type(&self, kind: &OpKind) -> Result<WordType, WordError> {
        Ok(match kind {
            OpKind::Unary { op, arg } => {
                let ty = self.value_ty(*arg)?;
                match op {
                    UnaryOp::LogicalNot
                    | UnaryOp::ReductionAnd
                    | UnaryOp::ReductionOr
                    | UnaryOp::ReductionXor => WordType::new(1, false, ty.state())?,
                    UnaryOp::BitNot => ty,
                }
            }
            OpKind::Binary { op, left, right } => {
                let left = self.value_ty(*left)?;
                let right = self.value_ty(*right)?;
                let state = left.merged_state(right);
                match op {
                    BinaryOp::LogicalAnd
                    | BinaryOp::LogicalOr
                    | BinaryOp::Eq
                    | BinaryOp::Ne
                    | BinaryOp::Lt
                    | BinaryOp::Le
                    | BinaryOp::Gt
                    | BinaryOp::Ge => WordType::new(1, false, state)?,
                    BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Ashr => {
                        WordType::new(left.width(), left.is_signed(), state)?
                    }
                    _ => WordType::new(
                        left.width().max(right.width()),
                        left.is_signed() && right.is_signed(),
                        state,
                    )?,
                }
            }
            OpKind::Mux {
                cond,
                then_value,
                else_value,
            } => {
                self.require_value_width(*cond, 1, "mux condition")?;
                let then_ty = self.value_ty(*then_value)?;
                let else_ty = self.value_ty(*else_value)?;
                if then_ty != else_ty {
                    return Err(WordError::new("mux branch types differ"));
                }
                then_ty
            }
            OpKind::TriState { data, enable } => {
                self.require_value_width(enable.value, 1, "tri-state enable")?;
                self.value_ty(*data)?
            }
            OpKind::Concat { parts } => {
                if parts.is_empty() {
                    return Err(WordError::new("concat has no parts"));
                }
                let (width, state) = parts.iter().try_fold(
                    (0u32, LogicStateKind::TwoState),
                    |(width, state), part| {
                        let ty = self.value_ty(*part)?;
                        Ok::<_, WordError>((
                            width
                                .checked_add(ty.width())
                                .ok_or_else(|| WordError::new("concat width overflow"))?,
                            state.merge(ty.state()),
                        ))
                    },
                )?;
                WordType::new(width, false, state)?
            }
            OpKind::Extract { value, lsb, width } => {
                let ty = self.value_ty(*value)?;
                if lsb
                    .checked_add(width.get())
                    .is_none_or(|end| end > ty.width())
                {
                    return Err(WordError::new("extract range exceeds its input"));
                }
                ty.with_width(width.get())?
            }
            OpKind::DynamicExtract {
                value,
                offset,
                width,
            } => {
                let ty = self.value_ty(*value)?;
                if width.get() > ty.width() || self.value_ty(*offset)?.is_signed() {
                    return Err(WordError::new("dynamic extract has invalid bounds"));
                }
                ty.with_width(width.get())?
            }
            OpKind::DynamicInsert {
                value,
                offset,
                replacement,
            } => {
                let ty = self.value_ty(*value)?;
                let replacement = self.value_ty(*replacement)?;
                if self.value_ty(*offset)?.is_signed()
                    || replacement.width() > ty.width()
                    || replacement.state() != ty.state()
                {
                    return Err(WordError::new("dynamic insert has invalid operands"));
                }
                ty
            }
            OpKind::Cast {
                kind,
                value,
                target,
            } => {
                let source = self.value_ty(*value)?;
                if matches!(kind, CastKind::ZeroExtend | CastKind::SignExtend)
                    && target.width() < source.width()
                    || *kind == CastKind::Truncate && target.width() > source.width()
                {
                    return Err(WordError::new("cast direction disagrees with its widths"));
                }
                *target
            }
            OpKind::Register(register) => {
                self.validate_state_name(register.name)?;
                self.require_value_width(register.clock, 1, "register clock")?;
                self.validate_enable(register.enable, "register")?;
                self.validate_resets(register.d, &register.resets, false)?;
                self.value_ty(register.d)?
            }
            OpKind::Latch(latch) => {
                self.validate_state_name(latch.name)?;
                self.validate_enable(Some(latch.enable), "latch")?;
                self.validate_resets(latch.d, &latch.resets, true)?;
                self.value_ty(latch.d)?
            }
        })
    }

    fn validate_state_name(&self, name: Option<crate::NameId>) -> Result<(), WordError> {
        if name.is_some_and(|name| self.resolve_name(name).is_none()) {
            return Err(WordError::new("state operation name does not resolve"));
        }
        Ok(())
    }

    fn validate_enable(&self, enable: Option<Enable>, kind: &str) -> Result<(), WordError> {
        if let Some(enable) = enable {
            self.require_value_width(enable.value, 1, &format!("{kind} enable"))?;
        }
        Ok(())
    }

    fn validate_resets(
        &self,
        data: ValueId,
        resets: &[Reset],
        latch: bool,
    ) -> Result<(), WordError> {
        let data = self.value_ty(data)?;
        for reset in resets {
            if latch && reset.kind != ResetKind::Async {
                return Err(WordError::new("latch reset must be asynchronous"));
            }
            self.require_value_width(reset.value, 1, "state reset")?;
            if self.value_ty(reset.reset_value)? != data {
                return Err(WordError::new("state reset value type differs from data"));
            }
        }
        Ok(())
    }

    fn validate_connects_and_instances(
        &self,
        scratch: &mut Vec<ValidationEntry>,
    ) -> Result<(), WordError> {
        for connect in &self.connects {
            if self.lvalue_ty(&connect.target)? != self.value_ty(connect.value)? {
                return Err(WordError::new("connection target and value types differ"));
            }
        }
        for (index, instance) in self.instances.iter().enumerate() {
            let id = InstId::from_index(index)?;
            let name = self
                .resolve_name(instance.name)
                .ok_or_else(|| WordError::new(format!("instance {id:?} name does not resolve")))?;
            let module = self.resolve_name(instance.module).ok_or_else(|| {
                WordError::new(format!("instance {id:?} module does not resolve"))
            })?;
            if name.trim().is_empty()
                || module.trim().is_empty()
                || self
                    .named_instances
                    .get(instance.name.raw() as usize)
                    .copied()
                    .flatten()
                    != Some(id)
            {
                return Err(WordError::new(format!(
                    "instance {id:?} has an invalid identity"
                )));
            }
            scratch.clear();
            for connection in &instance.connections {
                let port = self.resolve_name(connection.port).ok_or_else(|| {
                    WordError::new(format!("instance {id:?} port does not resolve"))
                })?;
                if port.trim().is_empty() {
                    return Err(WordError::new(format!(
                        "instance {id:?} has an invalid port"
                    )));
                }
                scratch.push(ValidationEntry {
                    name: connection.port,
                    start: 0,
                    end: 0,
                });
                self.value_ty(connection.value)?;
            }
            scratch.sort_unstable_by_key(|entry| entry.name);
            if scratch.windows(2).any(|pair| pair[0].name == pair[1].name) {
                return Err(WordError::new(format!(
                    "instance {id:?} has a duplicate port"
                )));
            }
        }
        for (slot, instance) in self.named_instances.iter().enumerate() {
            if let Some(instance) = instance {
                let stored = self.instances.get(instance.index()).ok_or_else(|| {
                    WordError::new("instance name index references an unknown instance")
                })?;
                if stored.name.raw() as usize != slot {
                    return Err(WordError::new(
                        "instance name index disagrees with the stored instance",
                    ));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unary_module() -> (WordModule, OpId, ValueId) {
        let mut module = WordModule::new("top");
        let signal = module
            .add_wire("a", WordType::bits(1).unwrap(), SourceSpan::default())
            .unwrap();
        let input = module.read_signal(signal, SourceSpan::default()).unwrap();
        let result = module
            .unary(UnaryOp::BitNot, input, SourceSpan::default())
            .unwrap();
        let ValueKind::Operation(operation) = module.value(result).unwrap().kind else {
            unreachable!()
        };
        (module, operation, result)
    }

    #[test]
    fn validates_complete_structural_graph_and_rejects_corruption() {
        let (module, operation, result) = unary_module();
        module.validate().unwrap();

        let mut wrong_type = module.clone();
        wrong_type.values[result.index()].ty = WordType::bits(2).unwrap();
        assert!(
            wrong_type
                .validate()
                .unwrap_err()
                .to_string()
                .contains("result type")
        );

        let mut cyclic = module;
        cyclic.operation_mut(operation).unwrap().kind = OpKind::Unary {
            op: UnaryOp::BitNot,
            arg: result,
        };
        assert!(
            cyclic
                .validate()
                .unwrap_err()
                .to_string()
                .contains("topologically ordered")
        );
    }
}
