// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Reachability compaction and topological restoration for Word SSA arenas.
//!
//! Structural outputs, instance bindings, and memory controls are roots.
//! Compaction computes one complete old-to-new mapping, restores the value
//! arena's topological order, and rewrites every stored reference atomically.

use super::{
    AnnotationTarget, MemoryReadPort, MemoryReadTiming, MemoryWritePort, OpId, OpKind, SignalId,
    SignalKind, ValueId, ValueKind, WordError, WordModule,
};
#[cfg(test)]
use super::{AnnotationValueSpec, BinaryOp, LValue, PortDirection, SourceSpan, UnaryOp, WordType};

#[derive(Debug, Clone)]
/// Old-to-new ID mapping produced by [`WordModule::compact_netlist`].
///
/// An entry is `None` when the corresponding old value or operation was dead.
pub struct NetlistRemap {
    values: Box<[Option<ValueId>]>,
    operations: Box<[Option<OpId>]>,
    value_count: usize,
    operation_count: usize,
}

impl NetlistRemap {
    /// Maps an old value ID to its compacted ID.
    #[must_use]
    pub fn value(&self, old: ValueId) -> Option<ValueId> {
        self.values.get(old.index()).copied().flatten()
    }

    /// Maps an old operation ID to its compacted ID.
    #[must_use]
    pub fn operation(&self, old: OpId) -> Option<OpId> {
        self.operations.get(old.index()).copied().flatten()
    }

    /// Returns the value-arena length before compaction.
    #[must_use]
    pub fn old_value_count(&self) -> usize {
        self.values.len()
    }

    /// Returns the number of retained values.
    #[must_use]
    pub fn value_count(&self) -> usize {
        self.value_count
    }

    /// Returns the operation-arena length before compaction.
    #[must_use]
    pub fn old_operation_count(&self) -> usize {
        self.operations.len()
    }

    /// Returns the number of retained operations.
    #[must_use]
    pub fn operation_count(&self) -> usize {
        self.operation_count
    }
}

impl WordModule {
    /// Removes unreachable values and topologically renumbers survivors.
    ///
    /// Roots include structural connects, instance inputs, memory addresses,
    /// clocks, enables, data, and masks. Stable topological renumbering restores
    /// the SSA arena contract after a transaction appends replacements. Every
    /// stored reference is rewritten before the old arenas are discarded.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] if a stored reference is invalid or the compacted
    /// value/operation arenas exceed their typed-ID capacity.
    pub fn compact_netlist(&mut self) -> Result<NetlistRemap, WordError> {
        self.compact_netlist_with_roots(&[])
    }

    /// Removes unreachable values while retaining additional semantic roots.
    ///
    /// The additional roots are for side databases whose references are not
    /// stored in [`WordModule`] itself, such as equivalent implementation
    /// choices consumed by technology mapping. They participate in the same
    /// reachability walk and are returned through the ordinary dense remap.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] for an unknown additional root, an invalid stored
    /// reference, or compact-ID capacity overflow. No old arena is replaced
    /// until every remap and rewritten record has been built successfully.
    #[allow(
        clippy::too_many_lines,
        reason = "compaction computes and commits all dense-ID remaps as one atomic transformation"
    )]
    pub fn compact_netlist_with_roots(
        &mut self,
        additional_roots: &[ValueId],
    ) -> Result<NetlistRemap, WordError> {
        let mut reachable_values = vec![false; self.values.len()];
        let mut reachable_operations = vec![false; self.operations.len()];
        let mut pending = self
            .connects
            .iter()
            .map(|connect| connect.value)
            .chain(
                self.connects
                    .iter()
                    .filter_map(|connect| connect.target.dynamic.map(|range| range.offset)),
            )
            .chain(
                self.instances
                    .iter()
                    .flat_map(|instance| instance.connections.iter())
                    .map(|connection| connection.value),
            )
            .chain(self.memory_read_ports.iter().flat_map(read_port_values))
            .chain(self.memory_write_ports.iter().flat_map(write_port_values))
            .chain(additional_roots.iter().copied())
            .collect::<Vec<_>>();
        // Mark from all externally observable roots. Operation traversal is
        // iterative so deeply generated arithmetic cannot overflow the stack.
        while let Some(value_id) = pending.pop() {
            let value_index = value_id.index();
            let reachable = reachable_values.get_mut(value_index).ok_or_else(|| {
                WordError::new(format!(
                    "netlist root references unknown RTL value {value_id:?}"
                ))
            })?;
            if *reachable {
                continue;
            }
            *reachable = true;
            let ValueKind::Operation(operation_id) = self.values[value_index].kind else {
                continue;
            };
            let operation_index = operation_id.index();
            let operation_reachable =
                reachable_operations
                    .get_mut(operation_index)
                    .ok_or_else(|| {
                        WordError::new(format!(
                            "RTL value {value_id:?} references unknown operation {operation_id:?}"
                        ))
                    })?;
            if *operation_reachable {
                continue;
            }
            *operation_reachable = true;
            self.operations[operation_index]
                .kind
                .for_each_input(|input| pending.push(input));
        }

        let DenseTopologicalRemap {
            value_remap,
            operation_remap,
            value_order,
            operation_order,
        } = dense_topological_remap(self, &reachable_values, &reachable_operations)?;
        let mut old_values = std::mem::take(&mut self.values)
            .into_iter()
            .map(Some)
            .collect::<Vec<_>>();
        let mut values = Vec::with_capacity(value_order.len());
        for index in value_order {
            let mut value = old_values[index]
                .take()
                .ok_or_else(|| WordError::new("topological value order contains a duplicate"))?;
            if let ValueKind::Operation(operation) = &mut value.kind {
                *operation = remap_operation(*operation, &operation_remap)?;
            }
            values.push(value);
        }

        let mut old_operations = std::mem::take(&mut self.operations)
            .into_iter()
            .map(Some)
            .collect::<Vec<_>>();
        let mut operations = Vec::with_capacity(operation_order.len());
        for index in operation_order {
            let mut operation = old_operations[index].take().ok_or_else(|| {
                WordError::new("topological operation order contains a duplicate")
            })?;
            remap_operation_kind(&mut operation.kind, &value_remap)?;
            operation.result = remap_value(operation.result, &value_remap)?;
            operations.push(operation);
        }

        for connect in &mut self.connects {
            connect.value = remap_value(connect.value, &value_remap)?;
            if let Some(dynamic) = &mut connect.target.dynamic {
                dynamic.offset = remap_value(dynamic.offset, &value_remap)?;
            }
        }
        for connection in self
            .instances
            .iter_mut()
            .flat_map(|instance| &mut instance.connections)
        {
            connection.value = remap_value(connection.value, &value_remap)?;
        }
        for port in &mut self.memory_read_ports {
            port.address = remap_value(port.address, &value_remap)?;
            if let MemoryReadTiming::Synchronous { clock, enable, .. } = &mut port.timing {
                clock.value = remap_value(clock.value, &value_remap)?;
                if let Some(enable) = enable {
                    enable.value = remap_value(enable.value, &value_remap)?;
                }
            }
        }
        for port in &mut self.memory_write_ports {
            port.address = remap_value(port.address, &value_remap)?;
            port.data = remap_value(port.data, &value_remap)?;
            port.clock.value = remap_value(port.clock.value, &value_remap)?;
            if let Some(enable) = &mut port.enable {
                enable.value = remap_value(enable.value, &value_remap)?;
            }
            if let Some(mask) = &mut port.mask {
                mask.value = remap_value(mask.value, &value_remap)?;
            }
        }
        self.annotations = std::mem::take(&mut self.annotations)
            .into_iter()
            .filter_map(|mut annotation| {
                let remapped = match annotation.target {
                    AnnotationTarget::Value(value) => value_remap
                        .get(value.index())
                        .copied()
                        .flatten()
                        .map(AnnotationTarget::Value),
                    AnnotationTarget::Operation(operation) => operation_remap
                        .get(operation.index())
                        .copied()
                        .flatten()
                        .map(AnnotationTarget::Operation),
                    target => Some(target),
                };
                remapped.map(|target| {
                    annotation.target = target;
                    annotation
                })
            })
            .collect();
        self.synthesis_directives = std::mem::take(&mut self.synthesis_directives)
            .into_iter()
            .filter_map(|mut directive| {
                let remapped = match directive.target {
                    AnnotationTarget::Value(value) => value_remap
                        .get(value.index())
                        .copied()
                        .flatten()
                        .map(AnnotationTarget::Value),
                    AnnotationTarget::Operation(operation) => operation_remap
                        .get(operation.index())
                        .copied()
                        .flatten()
                        .map(AnnotationTarget::Operation),
                    target => Some(target),
                };
                remapped.map(|target| {
                    directive.target = target;
                    directive
                })
            })
            .collect();
        self.values = values;
        self.operations = operations;
        Ok(NetlistRemap {
            value_count: self.values.len(),
            operation_count: self.operations.len(),
            values: value_remap.into_boxed_slice(),
            operations: operation_remap.into_boxed_slice(),
        })
    }

    /// Ends the procedural phase by removing values and signals that exist
    /// only while CFG effects are normalized.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] if netlist compaction fails or a surviving
    /// structural reference points at a removed process-local signal.
    pub fn remove_process_locals(&mut self) -> Result<(), WordError> {
        self.compact_netlist()?;
        let mut remap = Vec::with_capacity(self.signals.len());
        let mut next = 0usize;
        for signal in &self.signals {
            remap.push(if signal.kind == SignalKind::ProcessLocal {
                None
            } else {
                let id = SignalId::from_index(next)?;
                next += 1;
                Some(id)
            });
        }
        let map = |signal: SignalId| {
            remap.get(signal.index()).copied().flatten().ok_or_else(|| {
                WordError::new("process normalization left a live process-local signal")
            })
        };

        for port in &mut self.ports {
            port.signal = map(port.signal)?;
        }
        for value in &mut self.values {
            if let ValueKind::Signal(reference) = &mut value.kind {
                reference.signal = map(reference.signal)?;
            }
        }
        for connect in &mut self.connects {
            connect.target.signal = map(connect.target.signal)?;
        }
        for port in &mut self.memory_read_ports {
            port.data = map(port.data)?;
        }
        for signal in &mut self.named_signals {
            *signal = signal.and_then(|signal| remap[signal.index()]);
        }
        self.annotations = std::mem::take(&mut self.annotations)
            .into_iter()
            .filter_map(|mut annotation| {
                let remapped = match annotation.target {
                    AnnotationTarget::Signal(signal) => remap
                        .get(signal.index())
                        .copied()
                        .flatten()
                        .map(AnnotationTarget::Signal),
                    target => Some(target),
                };
                remapped.map(|target| {
                    annotation.target = target;
                    annotation
                })
            })
            .collect();
        self.synthesis_directives = std::mem::take(&mut self.synthesis_directives)
            .into_iter()
            .filter_map(|mut directive| {
                let remapped = match directive.target {
                    AnnotationTarget::Signal(signal) => remap
                        .get(signal.index())
                        .copied()
                        .flatten()
                        .map(AnnotationTarget::Signal),
                    target => Some(target),
                };
                remapped.map(|target| {
                    directive.target = target;
                    directive
                })
            })
            .collect();
        self.signals = std::mem::take(&mut self.signals)
            .into_iter()
            .filter(|signal| signal.kind != SignalKind::ProcessLocal)
            .collect();
        self.validate()
    }
}

fn read_port_values(port: &MemoryReadPort) -> impl Iterator<Item = ValueId> {
    let (clock, enable) = match port.timing {
        MemoryReadTiming::Asynchronous => (None, None),
        MemoryReadTiming::Synchronous { clock, enable, .. } => {
            (Some(clock.value), enable.map(|enable| enable.value))
        }
    };
    [Some(port.address), clock, enable].into_iter().flatten()
}

fn write_port_values(port: &MemoryWritePort) -> impl Iterator<Item = ValueId> {
    [
        Some(port.address),
        Some(port.data),
        Some(port.clock.value),
        port.enable.map(|enable| enable.value),
        port.mask.map(|mask| mask.value),
    ]
    .into_iter()
    .flatten()
}

struct DenseTopologicalRemap {
    value_remap: Vec<Option<ValueId>>,
    operation_remap: Vec<Option<OpId>>,
    value_order: Vec<usize>,
    operation_order: Vec<usize>,
}

fn dense_topological_remap(
    module: &WordModule,
    reachable_values: &[bool],
    reachable_operations: &[bool],
) -> Result<DenseTopologicalRemap, WordError> {
    let mut value_remap = vec![None; module.values.len()];
    let mut operation_remap = vec![None; module.operations.len()];
    let mut value_order = Vec::with_capacity(reachable_values.iter().filter(|&&keep| keep).count());
    let mut operation_order =
        Vec::with_capacity(reachable_operations.iter().filter(|&&keep| keep).count());
    let mut state = vec![0u8; module.values.len()];
    let mut stack = Vec::new();
    for start in 0..module.values.len() {
        if !reachable_values[start] || state[start] == 2 {
            continue;
        }
        stack.push((start, false));
        while let Some((index, finish)) = stack.pop() {
            if finish {
                if state[index] != 1 {
                    return Err(WordError::new("RTL topological traversal lost its state"));
                }
                state[index] = 2;
                value_remap[index] = Some(ValueId::from_index(value_order.len())?);
                value_order.push(index);
                if let ValueKind::Operation(operation) = module.values[index].kind {
                    operation_remap[operation.index()] =
                        Some(OpId::from_index(operation_order.len())?);
                    operation_order.push(operation.index());
                }
                continue;
            }
            match state[index] {
                2 => continue,
                1 => return Err(WordError::new("RTL value dependencies contain a cycle")),
                0 => state[index] = 1,
                _ => return Err(WordError::new("RTL topological state is invalid")),
            }
            stack.push((index, true));
            let ValueKind::Operation(operation) = module.values[index].kind else {
                continue;
            };
            let stored = module.operations.get(operation.index()).ok_or_else(|| {
                WordError::new("reachable RTL value references an unknown operation")
            })?;
            if stored.result.index() != index
                || !reachable_operations
                    .get(operation.index())
                    .copied()
                    .unwrap_or(false)
            {
                return Err(WordError::new(
                    "reachable RTL value disagrees with its producing operation",
                ));
            }
            let mut inputs = Vec::new();
            stored
                .kind
                .for_each_input(|input| inputs.push(input.index()));
            for input in inputs.into_iter().rev() {
                if !reachable_values.get(input).copied().unwrap_or(false) {
                    return Err(WordError::new(
                        "reachable RTL operation has an unreachable input",
                    ));
                }
                stack.push((input, false));
            }
        }
    }
    if operation_order.len() != reachable_operations.iter().filter(|&&keep| keep).count() {
        return Err(WordError::new(
            "reachable operation arena disagrees with reachable values",
        ));
    }
    Ok(DenseTopologicalRemap {
        value_remap,
        operation_remap,
        value_order,
        operation_order,
    })
}

fn remap_id<T: Copy>(index: usize, remap: &[Option<T>], kind: &str) -> Result<T, WordError> {
    remap
        .get(index)
        .copied()
        .flatten()
        .ok_or_else(|| WordError::new(format!("reachable netlist references dead {kind} {index}")))
}

fn remap_value(value: ValueId, remap: &[Option<ValueId>]) -> Result<ValueId, WordError> {
    remap_id(value.index(), remap, "RTL value")
}

fn remap_operation(operation: OpId, remap: &[Option<OpId>]) -> Result<OpId, WordError> {
    remap_id(operation.index(), remap, "RTL operation")
}

fn remap_operation_kind(kind: &mut OpKind, remap: &[Option<ValueId>]) -> Result<(), WordError> {
    let remap_one = |value: &mut ValueId| -> Result<(), WordError> {
        *value = remap_value(*value, remap)?;
        Ok(())
    };
    match kind {
        OpKind::Unary { arg, .. } => remap_one(arg)?,
        OpKind::Binary { left, right, .. } => {
            remap_one(left)?;
            remap_one(right)?;
        }
        OpKind::Mux {
            cond,
            then_value,
            else_value,
        } => {
            remap_one(cond)?;
            remap_one(then_value)?;
            remap_one(else_value)?;
        }
        OpKind::TriState { data, enable } => {
            remap_one(data)?;
            remap_one(&mut enable.value)?;
        }
        OpKind::Concat { parts } => {
            for part in parts {
                remap_one(part)?;
            }
        }
        OpKind::Extract { value, .. } | OpKind::Cast { value, .. } => remap_one(value)?,
        OpKind::DynamicExtract { value, offset, .. } => {
            remap_one(value)?;
            remap_one(offset)?;
        }
        OpKind::DynamicInsert {
            value,
            offset,
            replacement,
        } => {
            remap_one(value)?;
            remap_one(offset)?;
            remap_one(replacement)?;
        }
        OpKind::Register(register) => {
            remap_one(&mut register.d)?;
            remap_one(&mut register.clock)?;
            if let Some(enable) = &mut register.enable {
                remap_one(&mut enable.value)?;
            }
            for reset in &mut register.resets {
                remap_one(&mut reset.value)?;
                remap_one(&mut reset.reset_value)?;
            }
        }
        OpKind::Latch(latch) => {
            remap_one(&mut latch.d)?;
            remap_one(&mut latch.enable.value)?;
            for reset in &mut latch.resets {
                remap_one(&mut reset.value)?;
                remap_one(&mut reset.reset_value)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_dead_values_and_rewrites_dense_ids() {
        let mut module = WordModule::new("top");
        let ty = WordType::bits(1).unwrap();
        let a = module
            .add_port("a", PortDirection::Input, ty, SourceSpan::default())
            .unwrap();
        let y = module
            .add_port("y", PortDirection::Output, ty, SourceSpan::default())
            .unwrap();
        let a = module
            .read_signal(module.port(a).unwrap().signal, SourceSpan::default())
            .unwrap();
        let dead = module
            .unary(UnaryOp::BitNot, a, SourceSpan::default())
            .unwrap();
        let live = module
            .binary(BinaryOp::BitAnd, a, a, SourceSpan::default())
            .unwrap();
        module
            .unary(UnaryOp::BitNot, dead, SourceSpan::default())
            .unwrap();
        module
            .connect(
                LValue::signal(module.port(y).unwrap().signal),
                live,
                SourceSpan::default(),
            )
            .unwrap();
        let ValueKind::Operation(dead_operation) = module.value(dead).unwrap().kind else {
            panic!("unary result must reference an operation");
        };
        let ValueKind::Operation(live_operation) = module.value(live).unwrap().kind else {
            panic!("binary result must reference an operation");
        };
        for (target, name) in [
            (AnnotationTarget::Value(dead), "dead_value"),
            (
                AnnotationTarget::Operation(dead_operation),
                "dead_operation",
            ),
            (AnnotationTarget::Value(live), "live_value"),
            (
                AnnotationTarget::Operation(live_operation),
                "live_operation",
            ),
        ] {
            module
                .add_annotation(
                    target,
                    name,
                    AnnotationValueSpec::Other("tag".to_string()),
                    SourceSpan::default(),
                )
                .unwrap();
        }

        let remap = module.compact_netlist().unwrap();

        assert_eq!(module.operations.len(), 1);
        assert_eq!(module.values.len(), 2);
        assert_eq!(module.connects[0].value.index(), 1);
        assert_eq!(module.operations[0].result.index(), 1);
        assert!(matches!(
            module.values[1].kind,
            ValueKind::Operation(operation) if operation.index() == 0
        ));
        assert_eq!(remap.old_value_count(), 4);
        assert_eq!(remap.value_count(), 2);
        assert_eq!(remap.old_operation_count(), 3);
        assert_eq!(remap.operation_count(), 1);
        assert_eq!(remap.value(a).unwrap().index(), 0);
        assert!(remap.value(dead).is_none());
        assert_eq!(module.annotations().len(), 2);
        assert!(module.annotations().iter().any(|annotation| {
            annotation.target == AnnotationTarget::Value(remap.value(live).unwrap())
                && module.name_str(annotation.name) == "live_value"
        }));
        assert!(module.annotations().iter().any(|annotation| {
            annotation.target
                == AnnotationTarget::Operation(remap.operation(live_operation).unwrap())
                && module.name_str(annotation.name) == "live_operation"
        }));
    }

    #[test]
    fn restores_topological_order_after_transactional_forward_rewrite() {
        let mut module = WordModule::new("top");
        let ty = WordType::bits(1).unwrap();
        let input = module
            .add_port("input", PortDirection::Input, ty, SourceSpan::default())
            .unwrap();
        let output = module
            .add_port("output", PortDirection::Output, ty, SourceSpan::default())
            .unwrap();
        let input = module
            .read_signal(module.port(input).unwrap().signal, SourceSpan::default())
            .unwrap();
        let earlier = module
            .unary(UnaryOp::BitNot, input, SourceSpan::default())
            .unwrap();
        let later = module
            .unary(UnaryOp::LogicalNot, input, SourceSpan::default())
            .unwrap();
        let ValueKind::Operation(earlier_operation) = module.value(earlier).unwrap().kind else {
            panic!("unary result must reference an operation");
        };
        let OpKind::Unary { arg, .. } = &mut module.operation_mut(earlier_operation).unwrap().kind
        else {
            panic!("unary operation changed kind");
        };
        *arg = later;
        module
            .connect(
                LValue::signal(module.port(output).unwrap().signal),
                earlier,
                SourceSpan::default(),
            )
            .unwrap();
        assert!(module.validate().is_err());

        let remap = module.compact_netlist().unwrap();

        module.validate().unwrap();
        assert!(remap.value(later).unwrap().index() < remap.value(earlier).unwrap().index());
        assert_eq!(remap.operation(earlier_operation).unwrap().index(), 1);
    }
}
