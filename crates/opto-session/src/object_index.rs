// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use opto_db::{Cell, CellConnection, DesignIndex, Direction, NameId, Net, Port};
use opto_ir::proc::{ProcTarget, TargetSelect, TerminatorKind};
use opto_ir::rtl::RtlModule;
use opto_ir::word::{self, OpKind, ValueKind};
use std::collections::BTreeSet;

pub(crate) fn build_object_index(rtl: &RtlModule) -> Result<DesignIndex, crate::SessionError> {
    let module = rtl.word();
    let mut design = DesignIndex::with_name_table(module.name(), module.name_table().clone());
    for port in module.ports() {
        design.add_port(Port {
            name: port.name,
            direction: direction(port.direction),
            width: port.ty.width(),
        });
    }
    for signal in module.signals() {
        if matches!(
            signal.kind,
            word::SignalKind::Wire | word::SignalKind::Register
        ) && let Some(name) = signal.name
        {
            design.add_net(Net {
                name,
                width: signal.ty.width(),
            });
            push_name(&mut design.used_signals, name);
        }
    }
    let mut values = ValueSignals::new(module);
    for instance in module.instances() {
        let mut cell = Cell::new(instance.name, instance.module);
        for connection in &instance.connections {
            let mut signals = Vec::new();
            values.collect(connection.value, &mut signals)?;
            deduplicate_preserving_order(&mut signals);
            for signal in &signals {
                push_name(&mut design.used_signals, *signal);
            }
            cell.connections.push(CellConnection {
                port: connection.port,
                signals,
            });
        }
        design.add_cell(cell);
    }
    for connect in module.connects() {
        push_signal(module, connect.target.signal, &mut design.used_signals)?;
        if let Some(dynamic) = connect.target.dynamic {
            values.collect(dynamic.offset, &mut design.used_signals)?;
        }
        values.collect(connect.value, &mut design.used_signals)?;
    }
    for event in rtl.procedures().events() {
        values.collect(event.value, &mut design.used_signals)?;
        if let Some(qualifier) = event.iff {
            values.collect(qualifier, &mut design.used_signals)?;
        }
    }
    for effect in rtl.procedures().effects() {
        match effect.target {
            ProcTarget::Signal { signal, select } => {
                push_signal(module, signal, &mut design.used_signals)?;
                collect_target_select(&mut values, select, &mut design.used_signals)?;
            }
            ProcTarget::Memory {
                address, select, ..
            } => {
                values.collect(address, &mut design.used_signals)?;
                collect_target_select(&mut values, select, &mut design.used_signals)?;
            }
        }
        values.collect(effect.value, &mut design.used_signals)?;
    }
    for (index, block) in rtl.procedures().blocks().iter().enumerate() {
        match block.terminator.kind {
            TerminatorKind::Return | TerminatorKind::Jump { .. } => {}
            TerminatorKind::Branch { condition, .. } => {
                values.collect(condition, &mut design.used_signals)?;
            }
            TerminatorKind::Switch { selector, .. } => {
                values.collect(selector, &mut design.used_signals)?;
                let block = opto_ir::proc::BlockId::from_index(index)
                    .expect("sealed procedure block count fits its typed ID");
                for (_, arm) in rtl
                    .procedures()
                    .switch_arms(block)
                    .expect("sealed switch owns a valid arm range")
                {
                    values.collect(arm.pattern, &mut design.used_signals)?;
                }
            }
        }
    }
    for port in module.memory_read_ports() {
        values.collect(port.address, &mut design.used_signals)?;
        push_signal(module, port.data, &mut design.used_signals)?;
        if let word::MemoryReadTiming::Synchronous { clock, enable, .. } = port.timing {
            values.collect(clock.value, &mut design.used_signals)?;
            if let Some(enable) = enable {
                values.collect(enable.value, &mut design.used_signals)?;
            }
        }
    }
    for port in module.memory_write_ports() {
        for value in [port.address, port.data, port.clock.value] {
            values.collect(value, &mut design.used_signals)?;
        }
        if let Some(enable) = port.enable {
            values.collect(enable.value, &mut design.used_signals)?;
        }
        if let Some(mask) = port.mask {
            values.collect(mask.value, &mut design.used_signals)?;
        }
    }
    deduplicate_preserving_order(&mut design.used_signals);
    Ok(design)
}

fn collect_target_select(
    values: &mut ValueSignals<'_>,
    select: TargetSelect,
    signals: &mut Vec<NameId>,
) -> Result<(), crate::SessionError> {
    if let TargetSelect::Dynamic { offset, .. } = select {
        values.collect(offset, signals)?;
    }
    Ok(())
}

struct ValueSignals<'a> {
    module: &'a word::WordModule,
    seen: Vec<u32>,
    generation: u32,
    pending: Vec<word::ValueId>,
}

impl<'a> ValueSignals<'a> {
    fn new(module: &'a word::WordModule) -> Self {
        Self {
            module,
            seen: vec![0; module.values().len()],
            generation: 0,
            pending: Vec::new(),
        }
    }

    fn collect(
        &mut self,
        root: word::ValueId,
        signals: &mut Vec<NameId>,
    ) -> Result<(), crate::SessionError> {
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.seen.fill(0);
            self.generation = 1;
        }
        self.pending.clear();
        self.pending.push(root);
        while let Some(value_id) = self.pending.pop() {
            let Some(seen) = self.seen.get_mut(value_id.index()) else {
                return Err(crate::SessionError::state(format!(
                    "object index: unknown semantic value {value_id:?}"
                )));
            };
            if std::mem::replace(seen, self.generation) == self.generation {
                continue;
            }
            let value = &self.module.values()[value_id.index()];
            match &value.kind {
                ValueKind::Signal(reference) => {
                    push_signal(self.module, reference.signal, signals)?;
                }
                ValueKind::Constant(_) => {}
                ValueKind::Operation(operation_id) => {
                    let operation = self.module.operation(*operation_id).ok_or_else(|| {
                        crate::SessionError::state(format!(
                            "object index: unknown semantic operation {operation_id:?}"
                        ))
                    })?;
                    match &operation.kind {
                        OpKind::Unary { arg, .. }
                        | OpKind::Extract { value: arg, .. }
                        | OpKind::Cast { value: arg, .. } => self.pending.push(*arg),
                        OpKind::Binary { left, right, .. } => {
                            self.pending.extend([*right, *left]);
                        }
                        OpKind::Concat { parts } => {
                            self.pending.extend(parts.iter().rev().copied());
                        }
                        OpKind::Mux {
                            cond,
                            then_value,
                            else_value,
                        } => self.pending.extend([*else_value, *then_value, *cond]),
                        OpKind::TriState { data, enable } => {
                            self.pending.extend([enable.value, *data]);
                        }
                        OpKind::DynamicExtract { value, offset, .. } => {
                            self.pending.extend([*offset, *value]);
                        }
                        OpKind::DynamicInsert {
                            value,
                            offset,
                            replacement,
                        } => self.pending.extend([*replacement, *offset, *value]),
                        OpKind::Register(register) => {
                            for reset in register.resets.iter().rev() {
                                self.pending.extend([reset.reset_value, reset.value]);
                            }
                            if let Some(enable) = register.enable {
                                self.pending.push(enable.value);
                            }
                            self.pending.extend([register.clock, register.d]);
                        }
                        OpKind::Latch(latch) => {
                            for reset in latch.resets.iter().rev() {
                                self.pending.extend([reset.reset_value, reset.value]);
                            }
                            self.pending.extend([latch.enable.value, latch.d]);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

fn push_signal(
    module: &word::WordModule,
    signal_id: word::SignalId,
    signals: &mut Vec<NameId>,
) -> Result<(), crate::SessionError> {
    let signal = module.signal(signal_id).ok_or_else(|| {
        crate::SessionError::state(format!(
            "object index: unknown semantic signal {signal_id:?}"
        ))
    })?;
    if let Some(name) = signal.name {
        push_name(signals, name);
    }
    Ok(())
}

fn direction(direction: word::PortDirection) -> Direction {
    match direction {
        word::PortDirection::Input => Direction::Input,
        word::PortDirection::Output => Direction::Output,
        word::PortDirection::Inout => Direction::Inout,
        word::PortDirection::Ref => Direction::Ref,
    }
}

fn push_name(values: &mut Vec<NameId>, value: NameId) {
    values.push(value);
}

fn deduplicate_preserving_order(values: &mut Vec<NameId>) {
    let mut seen = BTreeSet::new();
    values.retain(|value| seen.insert(*value));
}

#[cfg(test)]
mod tests {
    use super::*;
    use opto_ir::proc::{
        AssignmentMode, ProcBuilder, ProcTarget, ProcedureKind, SensitivityEvent, SwitchArmSpec,
    };
    use opto_ir::word::{
        DisabledRead, Edge, Enable, LogicStateKind, MemoryClock, MemoryReadPort, MemoryReadTiming,
        MemoryWriteMask, MemoryWritePort, ReadDuringWrite, SourceSpan, WordType,
    };
    use std::num::NonZeroU32;

    #[test]
    fn indexes_register_dependencies_without_copying_expressions() {
        let mut module = word::WordModule::new("top");
        let bit = WordType::new(1, false, LogicStateKind::FourState).unwrap();
        let d = module
            .add_port("d", word::PortDirection::Input, bit, SourceSpan::default())
            .unwrap();
        let clock = module
            .add_port(
                "clock",
                word::PortDirection::Input,
                bit,
                SourceSpan::default(),
            )
            .unwrap();
        let q = module
            .add_port("q", word::PortDirection::Output, bit, SourceSpan::default())
            .unwrap();
        let d_value = module
            .read_signal(module.port(d).unwrap().signal, SourceSpan::default())
            .unwrap();
        let clock_value = module
            .read_signal(module.port(clock).unwrap().signal, SourceSpan::default())
            .unwrap();
        let register = module
            .register(
                word::RegisterOp {
                    name: None,
                    d: d_value,
                    clock: clock_value,
                    edge: word::Edge::Pos,
                    enable: None,
                    resets: Vec::new(),
                },
                SourceSpan::default(),
            )
            .unwrap();
        module
            .connect(
                word::LValue::signal(module.port(q).unwrap().signal),
                register,
                SourceSpan::default(),
            )
            .unwrap();

        let rtl = RtlModule::structural(module).unwrap();
        let index = build_object_index(&rtl).unwrap();
        assert_eq!(
            index
                .used_signals
                .iter()
                .map(|name| index.name_str(*name))
                .collect::<Vec<_>>(),
            ["q", "d", "clock"]
        );
        assert!(index.cells.is_empty());
    }

    #[test]
    fn indexes_flat_cfg_and_first_class_memory_dependencies() {
        let mut word = word::WordModule::new("top");
        let bit = WordType::bits(1).unwrap();
        let byte = WordType::bits(8).unwrap();
        let address_ty = WordType::bits(2).unwrap();
        let add_port = |word: &mut word::WordModule, name, ty| {
            let port = word
                .add_port(name, word::PortDirection::Input, ty, SourceSpan::default())
                .unwrap();
            word.read_signal(word.port(port).unwrap().signal, SourceSpan::default())
                .unwrap()
        };
        let data = add_port(&mut word, "data", byte);
        let selector = add_port(&mut word, "selector", bit);
        let address = add_port(&mut word, "address", address_ty);
        let clock = add_port(&mut word, "clock", bit);
        let enable = add_port(&mut word, "enable", bit);
        let mask = add_port(&mut word, "mask", bit);
        let output = word
            .add_port(
                "output",
                word::PortDirection::Output,
                byte,
                SourceSpan::default(),
            )
            .unwrap();
        let read_data = word
            .add_wire("read_data", byte, SourceSpan::default())
            .unwrap();
        let memory = word
            .add_memory(
                "memory",
                byte,
                NonZeroU32::new(4).unwrap(),
                SourceSpan::default(),
            )
            .unwrap();
        word.add_memory_read_port(MemoryReadPort {
            memory,
            address,
            data: read_data,
            timing: MemoryReadTiming::Synchronous {
                clock: MemoryClock {
                    value: clock,
                    edge: Edge::Pos,
                },
                enable: Some(Enable {
                    value: enable,
                    active_high: true,
                }),
                disabled: DisabledRead::Hold,
            },
            read_during_write: ReadDuringWrite::OldData,
            source: SourceSpan::default(),
        })
        .unwrap();
        word.add_memory_write_port(MemoryWritePort {
            memory,
            address,
            data,
            clock: MemoryClock {
                value: clock,
                edge: Edge::Pos,
            },
            enable: Some(Enable {
                value: enable,
                active_high: true,
            }),
            mask: Some(MemoryWriteMask {
                value: mask,
                granularity: NonZeroU32::new(8).unwrap(),
                active_high: true,
            }),
            priority: 0,
            source: SourceSpan::default(),
        })
        .unwrap();
        let zero = word
            .constant(
                opto_ir::ConstBits::from_bin_str("0").unwrap(),
                bit,
                SourceSpan::default(),
            )
            .unwrap();
        let mut procedures = ProcBuilder::new();
        let procedure = procedures
            .add_combinational_procedure(ProcedureKind::Combinational, SourceSpan::default())
            .unwrap();
        let entry = procedures
            .add_block(procedure, SourceSpan::default())
            .unwrap();
        let selected = procedures
            .add_block(procedure, SourceSpan::default())
            .unwrap();
        let default = procedures
            .add_block(procedure, SourceSpan::default())
            .unwrap();
        procedures
            .terminate_switch(
                entry,
                selector,
                [SwitchArmSpec {
                    pattern: zero,
                    target: selected,
                    source: SourceSpan::default(),
                }],
                default,
                SourceSpan::default(),
            )
            .unwrap();
        procedures
            .assign(
                selected,
                AssignmentMode::Blocking,
                ProcTarget::signal(word.port(output).unwrap().signal),
                data,
                SourceSpan::default(),
            )
            .unwrap();
        procedures
            .assign(
                selected,
                AssignmentMode::Nonblocking,
                ProcTarget::memory(memory, address),
                data,
                SourceSpan::default(),
            )
            .unwrap();
        procedures
            .terminate_return(selected, SourceSpan::default())
            .unwrap();
        procedures
            .terminate_return(default, SourceSpan::default())
            .unwrap();
        let clocked = procedures
            .add_clocked_procedure(
                [SensitivityEvent {
                    value: clock,
                    edge: Edge::Neg,
                    iff: None,
                }],
                SourceSpan::default(),
            )
            .unwrap();
        let clocked_entry = procedures
            .add_block(clocked, SourceSpan::default())
            .unwrap();
        procedures
            .terminate_return(clocked_entry, SourceSpan::default())
            .unwrap();
        let rtl = RtlModule::new(word, procedures.seal().unwrap()).unwrap();

        let index = build_object_index(&rtl).unwrap();
        let used = index
            .used_signals
            .iter()
            .map(|name| index.name_str(*name))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            used,
            BTreeSet::from([
                "address",
                "clock",
                "data",
                "enable",
                "mask",
                "output",
                "read_data",
                "selector",
            ])
        );
    }
}
