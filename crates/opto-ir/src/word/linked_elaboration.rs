// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    Annotation, AnnotationTarget, AnnotationValue, AnnotationValueSpec, BitRange, CastKind,
    DefinitionKind, DynamicRange, Enable, InstId, Instance, LValue, LatchOp, MemoryClock, MemoryId,
    MemoryReadPort, MemoryReadPortId, MemoryReadTiming, MemoryWriteMask, MemoryWritePort,
    MemoryWritePortId, OpId, OpKind, Operation, PortDirection, RegisterOp, Reset, SignalId,
    SignalKind, SignalRef, SourceSpan, SynthesisDirectiveKind, Value, ValueId, ValueKind,
    WordError, WordModule, WordType,
};
use crate::{BitVal, ConstBits, NameId};
use std::collections::BTreeMap;

/// Recursively replaces design instances below `root` with their Word IR.
///
/// Definitions absent from `definitions`, and definitions explicitly marked as
/// black boxes, are retained as leaf instances. This is the boundary between
/// design hierarchy and technology/library cells.
///
/// # Errors
///
/// Returns [`WordError`] for invalid or duplicate definitions, recursive
/// hierarchy, incompatible instance bindings, unknown references, or compact
/// arena capacity overflow.
pub fn elaborate_linked_root<'a>(
    root: &'a WordModule,
    definitions: impl IntoIterator<Item = &'a WordModule>,
) -> Result<WordModule, WordError> {
    elaborate_linked_root_with(root, definitions, |_, _| Ok(()))
}

pub(crate) fn elaborate_linked_root_with<'a, E>(
    root: &'a WordModule,
    definitions: impl IntoIterator<Item = &'a WordModule>,
    on_occurrence: impl FnMut(&'a WordModule, &ModuleRemap) -> Result<(), E>,
) -> Result<WordModule, E>
where
    E: From<WordError>,
{
    let definitions = definitions
        .into_iter()
        .filter(|module| module.definition_kind() != DefinitionKind::BlackBox)
        .map(|module| (module.name(), module))
        .collect::<BTreeMap<_, _>>();
    let mut target = WordModule::new(root.name());
    copy_root_metadata(&mut target, root)?;
    let mut inliner = HierarchyInliner {
        target,
        definitions,
        active_definitions: Vec::new(),
        on_occurrence,
    };
    inliner.copy_module(root, "", true)?;
    inliner.target.consolidate_names()?;
    Ok(inliner.target)
}

fn copy_root_metadata(target: &mut WordModule, source: &WordModule) -> Result<(), WordError> {
    target.set_definition_kind(source.definition_kind());
    for annotation in source
        .annotations()
        .iter()
        .filter(|annotation| annotation.target == AnnotationTarget::Module)
    {
        let name = source
            .resolve_name(annotation.name)
            .ok_or_else(|| WordError::new("root annotation name does not resolve"))?;
        let value = annotation_value_spec(source, annotation)?;
        target.add_annotation(
            AnnotationTarget::Module,
            name,
            value,
            annotation.source.clone(),
        )?;
    }
    for directive in source
        .synthesis_directives()
        .iter()
        .filter(|directive| directive.target == AnnotationTarget::Module)
    {
        target.set_synthesis_directive(
            AnnotationTarget::Module,
            directive.kind,
            directive.enabled,
            directive.source.clone(),
        )?;
    }
    Ok(())
}

fn annotation_value_spec(
    source: &WordModule,
    annotation: &Annotation,
) -> Result<AnnotationValueSpec, WordError> {
    Ok(match &annotation.value {
        AnnotationValue::Integer {
            bits,
            width,
            signed,
        } => {
            let text = source
                .resolve_name(*bits)
                .ok_or_else(|| WordError::new("annotation integer bits do not resolve"))?;
            let bits = crate::ConstBits::from_bin_str(text)
                .map_err(|error| WordError::new(error.to_string()))?;
            if bits.width() != *width {
                return Err(WordError::new("annotation integer width is inconsistent"));
            }
            AnnotationValueSpec::Integer {
                bits,
                signed: *signed,
            }
        }
        AnnotationValue::String(value) => AnnotationValueSpec::String(
            source
                .resolve_name(*value)
                .ok_or_else(|| WordError::new("annotation value does not resolve"))?
                .to_string(),
        ),
        AnnotationValue::Other(value) => AnnotationValueSpec::Other(
            source
                .resolve_name(*value)
                .ok_or_else(|| WordError::new("annotation value does not resolve"))?
                .to_string(),
        ),
    })
}

struct HierarchyInliner<'a, F> {
    target: WordModule,
    definitions: BTreeMap<&'a str, &'a WordModule>,
    active_definitions: Vec<&'a str>,
    on_occurrence: F,
}

#[derive(Debug)]
pub(crate) struct ModuleRemap {
    signals: Box<[SignalId]>,
    memories: Box<[MemoryId]>,
    value_base: usize,
    operation_base: usize,
    memory_read_port_base: usize,
    memory_write_port_base: usize,
}

#[derive(Debug)]
struct OutputFragment {
    signal: SignalId,
    lsb: u32,
    width: u32,
    ty: WordType,
}

impl<'a, F> HierarchyInliner<'a, F> {
    fn copy_module<E>(
        &mut self,
        source: &'a WordModule,
        prefix: &str,
        preserve_ports: bool,
    ) -> Result<ModuleRemap, E>
    where
        E: From<WordError>,
        F: FnMut(&'a WordModule, &ModuleRemap) -> Result<(), E>,
    {
        if self.active_definitions.contains(&source.name()) {
            return Err(WordError::new(format!(
                "recursive RTL hierarchy reaches design '{}'",
                source.name()
            ))
            .into());
        }
        self.active_definitions.push(source.name());

        let signals = self.copy_signals(source, prefix, preserve_ports)?;
        let memories = self.copy_memories(source, prefix)?;
        let value_base = self.target.values.len();
        let operation_base = self.target.operations.len();
        let memory_read_port_base = self.target.memory_read_ports.len();
        let memory_write_port_base = self.target.memory_write_ports.len();
        let remap = ModuleRemap {
            signals: signals.into_boxed_slice(),
            memories: memories.into_boxed_slice(),
            value_base,
            operation_base,
            memory_read_port_base,
            memory_write_port_base,
        };
        self.copy_values_and_operations(source, prefix, &remap)?;
        self.copy_memory_ports(source, &remap)?;
        self.copy_connects(source, &remap)?;
        self.copy_annotations(source, preserve_ports, &remap)?;
        self.copy_synthesis_directives(source, preserve_ports, &remap)?;
        (self.on_occurrence)(source, &remap)?;

        for (index, instance) in source.instances().iter().enumerate() {
            self.copy_instance(source, prefix, &remap, InstId::from_index(index)?, instance)?;
        }

        let removed = self.active_definitions.pop();
        debug_assert_eq!(removed, Some(source.name()));
        Ok(remap)
    }

    fn copy_signals(
        &mut self,
        source: &WordModule,
        prefix: &str,
        preserve_ports: bool,
    ) -> Result<Vec<SignalId>, WordError> {
        let mut signals = Vec::with_capacity(source.signals().len());
        for (index, signal) in source.signals().iter().enumerate() {
            let local_name = signal.name.map_or_else(
                || format!("$signal{index}"),
                |name| source.name_str(name).to_string(),
            );
            let name = self.unique_signal_name(&hierarchical_name(prefix, &local_name));
            let copied = match signal.kind {
                SignalKind::Port(port) if preserve_ports => {
                    let port = source.port(port).ok_or_else(|| {
                        WordError::new(format!(
                            "design '{}' signal references a missing port",
                            source.name()
                        ))
                    })?;
                    let port_id = self.target.add_port(
                        &name,
                        port.direction,
                        signal.ty,
                        signal.source.clone(),
                    )?;
                    self.target
                        .port(port_id)
                        .expect("newly inserted port must exist")
                        .signal
                }
                SignalKind::Port(_) | SignalKind::Wire => {
                    self.target
                        .add_wire(&name, signal.ty, signal.source.clone())?
                }
                SignalKind::Register => {
                    self.target
                        .add_register_signal(&name, signal.ty, signal.source.clone())?
                }
                SignalKind::ProcessLocal => {
                    self.target
                        .add_process_local_signal(&name, signal.ty, signal.source.clone())?
                }
            };
            if let Some(layout) = source.signal_type_layout_spec(SignalId::from_index(index)?)? {
                self.target.set_signal_type_layout(copied, &layout)?;
            }
            self.target
                .set_signal_resolution(copied, signal.resolution)?;
            signals.push(copied);
        }
        Ok(signals)
    }

    fn copy_values_and_operations(
        &mut self,
        source: &WordModule,
        prefix: &str,
        remap: &ModuleRemap,
    ) -> Result<(), WordError> {
        self.target.values.reserve(source.values().len());
        for value in source.values() {
            let kind = match &value.kind {
                ValueKind::Signal(reference) => ValueKind::Signal(SignalRef {
                    signal: remap.signal(reference.signal)?,
                    lsb: reference.lsb,
                    width: reference.width,
                }),
                ValueKind::Constant(bits) => ValueKind::Constant(bits.clone()),
                ValueKind::Operation(operation) => {
                    ValueKind::Operation(remap.operation(*operation)?)
                }
            };
            self.target.values.push(Value {
                kind,
                ty: value.ty,
                source: value.source.in_occurrence(prefix),
            });
        }

        self.target.operations.reserve(source.operations().len());
        for operation in source.operations() {
            let kind = self.remap_operation(source, prefix, remap, &operation.kind)?;
            self.target.operations.push(Operation {
                kind,
                result: remap.value(operation.result)?,
                source: operation.source.in_occurrence(prefix),
            });
        }
        Ok(())
    }

    fn copy_memories(
        &mut self,
        source: &WordModule,
        prefix: &str,
    ) -> Result<Vec<MemoryId>, WordError> {
        source
            .memories()
            .iter()
            .map(|memory| {
                let local = source.name_str(memory.name);
                let name = self.unique_memory_name(&hierarchical_name(prefix, local));
                self.target.add_memory(
                    name,
                    memory.element_type,
                    memory.depth,
                    memory.source.clone(),
                )
            })
            .collect()
    }

    fn copy_memory_ports(
        &mut self,
        source: &WordModule,
        remap: &ModuleRemap,
    ) -> Result<(), WordError> {
        for port in source.memory_read_ports() {
            let timing = match port.timing {
                MemoryReadTiming::Asynchronous => MemoryReadTiming::Asynchronous,
                MemoryReadTiming::Synchronous {
                    clock,
                    enable,
                    disabled,
                } => MemoryReadTiming::Synchronous {
                    clock: Self::remap_memory_clock(remap, clock)?,
                    enable: enable
                        .map(|enable| Self::remap_enable(remap, enable))
                        .transpose()?,
                    disabled,
                },
            };
            self.target.add_memory_read_port(MemoryReadPort {
                memory: remap.memory(port.memory)?,
                address: remap.value(port.address)?,
                data: remap.signal(port.data)?,
                timing,
                read_during_write: port.read_during_write,
                source: port.source.clone(),
            })?;
        }
        for port in source.memory_write_ports() {
            self.target.add_memory_write_port(MemoryWritePort {
                memory: remap.memory(port.memory)?,
                address: remap.value(port.address)?,
                data: remap.value(port.data)?,
                clock: Self::remap_memory_clock(remap, port.clock)?,
                enable: port
                    .enable
                    .map(|enable| Self::remap_enable(remap, enable))
                    .transpose()?,
                mask: port
                    .mask
                    .map(|mask| -> Result<_, WordError> {
                        Ok(MemoryWriteMask {
                            value: remap.value(mask.value)?,
                            granularity: mask.granularity,
                            active_high: mask.active_high,
                        })
                    })
                    .transpose()?,
                priority: port.priority,
                source: port.source.clone(),
            })?;
        }
        Ok(())
    }

    fn remap_operation(
        &mut self,
        source: &WordModule,
        prefix: &str,
        remap: &ModuleRemap,
        operation: &OpKind,
    ) -> Result<OpKind, WordError> {
        Ok(match operation {
            OpKind::Unary { op, arg } => OpKind::Unary {
                op: *op,
                arg: remap.value(*arg)?,
            },
            OpKind::Binary { op, left, right } => OpKind::Binary {
                op: *op,
                left: remap.value(*left)?,
                right: remap.value(*right)?,
            },
            OpKind::Mux {
                cond,
                then_value,
                else_value,
            } => OpKind::Mux {
                cond: remap.value(*cond)?,
                then_value: remap.value(*then_value)?,
                else_value: remap.value(*else_value)?,
            },
            OpKind::Concat { parts } => OpKind::Concat {
                parts: parts
                    .iter()
                    .map(|value| remap.value(*value))
                    .collect::<Result<_, _>>()?,
            },
            OpKind::Extract { value, lsb, width } => OpKind::Extract {
                value: remap.value(*value)?,
                lsb: *lsb,
                width: *width,
            },
            OpKind::DynamicExtract {
                value,
                offset,
                width,
            } => OpKind::DynamicExtract {
                value: remap.value(*value)?,
                offset: remap.value(*offset)?,
                width: *width,
            },
            OpKind::DynamicInsert {
                value,
                offset,
                replacement,
            } => OpKind::DynamicInsert {
                value: remap.value(*value)?,
                offset: remap.value(*offset)?,
                replacement: remap.value(*replacement)?,
            },
            OpKind::Cast {
                kind,
                value,
                target,
            } => OpKind::Cast {
                kind: *kind,
                value: remap.value(*value)?,
                target: *target,
            },
            OpKind::Register(register) => OpKind::Register(RegisterOp {
                name: self.remap_optional_name(source, prefix, register.name)?,
                d: remap.value(register.d)?,
                clock: remap.value(register.clock)?,
                edge: register.edge,
                enable: register
                    .enable
                    .map(|enable| Self::remap_enable(remap, enable))
                    .transpose()?,
                resets: register
                    .resets
                    .iter()
                    .copied()
                    .map(|reset| Self::remap_reset(remap, reset))
                    .collect::<Result<_, _>>()?,
            }),
            OpKind::Latch(latch) => OpKind::Latch(LatchOp {
                name: self.remap_optional_name(source, prefix, latch.name)?,
                d: remap.value(latch.d)?,
                enable: Self::remap_enable(remap, latch.enable)?,
                resets: latch
                    .resets
                    .iter()
                    .copied()
                    .map(|reset| Self::remap_reset(remap, reset))
                    .collect::<Result<_, _>>()?,
            }),
        })
    }

    fn remap_optional_name(
        &mut self,
        source: &WordModule,
        prefix: &str,
        name: Option<NameId>,
    ) -> Result<Option<NameId>, WordError> {
        name.map(|name| {
            self.target
                .intern_name(hierarchical_name(prefix, source.name_str(name)))
        })
        .transpose()
    }

    fn remap_enable(remap: &ModuleRemap, enable: Enable) -> Result<Enable, WordError> {
        Ok(Enable {
            value: remap.value(enable.value)?,
            active_high: enable.active_high,
        })
    }

    fn remap_memory_clock(
        remap: &ModuleRemap,
        clock: MemoryClock,
    ) -> Result<MemoryClock, WordError> {
        Ok(MemoryClock {
            value: remap.value(clock.value)?,
            edge: clock.edge,
        })
    }

    fn remap_reset(remap: &ModuleRemap, reset: Reset) -> Result<Reset, WordError> {
        Ok(Reset {
            kind: reset.kind,
            value: remap.value(reset.value)?,
            active_high: reset.active_high,
            reset_value: remap.value(reset.reset_value)?,
        })
    }

    fn copy_connects(&mut self, source: &WordModule, remap: &ModuleRemap) -> Result<(), WordError> {
        for connect in source.connects() {
            let target = Self::remap_lvalue(remap, &connect.target)?;
            let target_ty = self.target.lvalue_ty(&target)?;
            let value =
                self.coerce_value(remap.value(connect.value)?, target_ty, &connect.source)?;
            self.target.connect(target, value, connect.source.clone())?;
        }
        Ok(())
    }

    fn copy_annotations(
        &mut self,
        source: &WordModule,
        preserve_ports: bool,
        remap: &ModuleRemap,
    ) -> Result<(), WordError> {
        for annotation in source.annotations() {
            let target = match annotation.target {
                AnnotationTarget::Module | AnnotationTarget::Instance(_) => continue,
                AnnotationTarget::Port(port) => {
                    let signal = source
                        .port(port)
                        .ok_or_else(|| WordError::new("annotation references unknown port"))?
                        .signal;
                    let signal = remap.signal(signal)?;
                    if preserve_ports {
                        match self.target.signal(signal).map(|signal| signal.kind) {
                            Some(SignalKind::Port(port)) => AnnotationTarget::Port(port),
                            _ => {
                                return Err(WordError::new(
                                    "root port annotation did not remap to a port",
                                ));
                            }
                        }
                    } else {
                        AnnotationTarget::Signal(signal)
                    }
                }
                AnnotationTarget::Signal(signal) => AnnotationTarget::Signal(remap.signal(signal)?),
                AnnotationTarget::Memory(memory) => AnnotationTarget::Memory(remap.memory(memory)?),
                AnnotationTarget::MemoryReadPort(port) => {
                    AnnotationTarget::MemoryReadPort(remap.memory_read_port(port)?)
                }
                AnnotationTarget::MemoryWritePort(port) => {
                    AnnotationTarget::MemoryWritePort(remap.memory_write_port(port)?)
                }
                AnnotationTarget::Value(value) => AnnotationTarget::Value(remap.value(value)?),
                AnnotationTarget::Operation(operation) => {
                    AnnotationTarget::Operation(remap.operation(operation)?)
                }
            };
            let name = source
                .resolve_name(annotation.name)
                .ok_or_else(|| WordError::new("annotation name does not resolve"))?;
            self.target.add_annotation(
                target,
                name,
                annotation_value_spec(source, annotation)?,
                annotation.source.clone(),
            )?;
        }
        Ok(())
    }

    fn copy_synthesis_directives(
        &mut self,
        source: &WordModule,
        preserve_ports: bool,
        remap: &ModuleRemap,
    ) -> Result<(), WordError> {
        for directive in source.synthesis_directives() {
            let target = match directive.target {
                AnnotationTarget::Module | AnnotationTarget::Instance(_) => continue,
                AnnotationTarget::Port(port) => {
                    let signal = source
                        .port(port)
                        .ok_or_else(|| WordError::new("directive references unknown port"))?
                        .signal;
                    let signal = remap.signal(signal)?;
                    if preserve_ports {
                        match self.target.signal(signal).map(|signal| signal.kind) {
                            Some(SignalKind::Port(port)) => AnnotationTarget::Port(port),
                            _ => {
                                return Err(WordError::new(
                                    "root port directive did not remap to a port",
                                ));
                            }
                        }
                    } else {
                        AnnotationTarget::Signal(signal)
                    }
                }
                AnnotationTarget::Signal(signal) => AnnotationTarget::Signal(remap.signal(signal)?),
                AnnotationTarget::Memory(memory) => AnnotationTarget::Memory(remap.memory(memory)?),
                AnnotationTarget::MemoryReadPort(port) => {
                    AnnotationTarget::MemoryReadPort(remap.memory_read_port(port)?)
                }
                AnnotationTarget::MemoryWritePort(port) => {
                    AnnotationTarget::MemoryWritePort(remap.memory_write_port(port)?)
                }
                AnnotationTarget::Value(value) => AnnotationTarget::Value(remap.value(value)?),
                AnnotationTarget::Operation(operation) => {
                    AnnotationTarget::Operation(remap.operation(operation)?)
                }
            };
            self.target.set_synthesis_directive(
                target,
                directive.kind,
                directive.enabled,
                directive.source.clone(),
            )?;
        }
        Ok(())
    }

    fn remap_lvalue(remap: &ModuleRemap, lvalue: &LValue) -> Result<LValue, WordError> {
        Ok(LValue {
            signal: remap.signal(lvalue.signal)?,
            range: lvalue.range,
            dynamic: match lvalue.dynamic {
                Some(range) => Some(DynamicRange {
                    offset: remap.value(range.offset)?,
                    width: range.width,
                }),
                None => None,
            },
        })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "instance elaboration keeps child lookup, binding, remapping, and annotations in one transaction"
    )]
    fn copy_instance<E>(
        &mut self,
        parent: &WordModule,
        prefix: &str,
        parent_remap: &ModuleRemap,
        source_instance: InstId,
        instance: &Instance,
    ) -> Result<(), E>
    where
        E: From<WordError>,
        F: FnMut(&'a WordModule, &ModuleRemap) -> Result<(), E>,
    {
        let instance_name = hierarchical_name(prefix, parent.name_str(instance.name));
        let reference = parent.name_str(instance.module);
        let child = self.definitions.get(reference).copied();
        let instance_target = AnnotationTarget::Instance(source_instance);
        let effective_directive = |kind| {
            parent
                .synthesis_directive(instance_target, kind)
                .or_else(|| {
                    child
                        .and_then(|child| child.synthesis_directive(AnnotationTarget::Module, kind))
                })
        };
        let preserve_hierarchy = effective_directive(SynthesisDirectiveKind::DontTouch)
            == Some(true)
            || effective_directive(SynthesisDirectiveKind::Ungroup) == Some(false);
        let Some(child) = child.filter(|_| !preserve_hierarchy) else {
            let name = self.unique_instance_name(&instance_name);
            let connections = instance
                .connections
                .iter()
                .map(|connection| {
                    Ok((
                        parent.name_str(connection.port).to_string(),
                        parent_remap.value(connection.value)?,
                        connection.source.clone(),
                    ))
                })
                .collect::<Result<_, WordError>>()?;
            let target_instance =
                self.target
                    .add_instance(name, reference, connections, instance.source.clone())?;
            for annotation in parent.annotations().iter().filter(|annotation| {
                annotation.target == AnnotationTarget::Instance(source_instance)
            }) {
                let name = parent
                    .resolve_name(annotation.name)
                    .ok_or_else(|| WordError::new("instance annotation name does not resolve"))?;
                self.target.add_annotation(
                    AnnotationTarget::Instance(target_instance),
                    name,
                    annotation_value_spec(parent, annotation)?,
                    annotation.source.clone(),
                )?;
            }
            if let Some(child) = child {
                for directive in child.synthesis_directives().iter().filter(|directive| {
                    directive.target == AnnotationTarget::Module
                        && matches!(
                            directive.kind,
                            SynthesisDirectiveKind::DontTouch | SynthesisDirectiveKind::Ungroup
                        )
                }) {
                    self.target.set_synthesis_directive(
                        AnnotationTarget::Instance(target_instance),
                        directive.kind,
                        directive.enabled,
                        directive.source.clone(),
                    )?;
                }
            }
            for directive in parent
                .synthesis_directives()
                .iter()
                .filter(|directive| directive.target == AnnotationTarget::Instance(source_instance))
            {
                self.target.set_synthesis_directive(
                    AnnotationTarget::Instance(target_instance),
                    directive.kind,
                    directive.enabled,
                    directive.source.clone(),
                )?;
            }
            return Ok(());
        };

        let child_remap = self.copy_module(child, &instance_name, false)?;
        let connected_ports = instance
            .connections
            .iter()
            .map(|connection| parent.name_str(connection.port))
            .collect::<std::collections::BTreeSet<_>>();
        for child_port in child.ports().iter().filter(|port| {
            port.direction == PortDirection::Input
                && !connected_ports.contains(child.name_str(port.name))
        }) {
            let child_signal = child_remap.signal(child_port.signal)?;
            let bit = if child_port.ty.state() == super::LogicStateKind::FourState {
                BitVal::X
            } else {
                BitVal::Zero
            };
            let bits = ConstBits::from_bits(vec![bit; child_port.ty.width() as usize])
                .map_err(|error| WordError::new(error.to_string()))?;
            let value = self
                .target
                .constant(bits, child_port.ty, instance.source.clone())?;
            self.connect_child_input(child_signal, value, &instance.source)?;
        }
        for connection in &instance.connections {
            let port_name = parent.name_str(connection.port);
            let child_port = child
                .ports()
                .iter()
                .find(|port| child.name_str(port.name) == port_name)
                .ok_or_else(|| {
                    WordError::new(format!(
                        "instance '{instance_name}' of '{reference}' connects unknown port '{port_name}'"
                    ))
                })?;
            let child_signal = child_remap.signal(child_port.signal)?;
            match child_port.direction {
                PortDirection::Input => {
                    let value = parent_remap.value(connection.value)?;
                    let value = self.coerce_value(value, child_port.ty, &connection.source)?;
                    self.connect_child_input(child_signal, value, &connection.source)?;
                }
                PortDirection::Output => self.connect_child_output(
                    parent,
                    parent_remap,
                    connection.value,
                    child_signal,
                    &connection.source,
                )?,
                PortDirection::Inout => {
                    return Err(WordError::new(format!(
                        "linked elaboration does not support inout port '{reference}.{port_name}'"
                    ))
                    .into());
                }
            }
        }
        Ok(())
    }

    fn connect_child_input(
        &mut self,
        signal: SignalId,
        value: ValueId,
        source: &SourceSpan,
    ) -> Result<(), WordError> {
        let target = LValue::signal(signal);
        let ty = self.target.lvalue_ty(&target)?;
        let value = self.coerce_value(value, ty, source)?;
        self.target.connect(target, value, source.clone())
    }

    fn connect_child_output(
        &mut self,
        parent: &WordModule,
        parent_remap: &ModuleRemap,
        connection: ValueId,
        child_signal: SignalId,
        source: &SourceSpan,
    ) -> Result<(), WordError> {
        let parent_ty = parent
            .value(connection)
            .ok_or_else(|| WordError::new("instance output references a missing value"))?
            .ty;
        let child_value = self.target.read_signal(child_signal, source.clone())?;
        let output = self.coerce_value(child_value, parent_ty, source)?;
        let fragments = parent
            .signal_fragments(connection)?
            .into_iter()
            .map(|fragment| {
                Ok(OutputFragment {
                    signal: parent_remap.signal(fragment.reference.signal)?,
                    lsb: fragment.reference.lsb,
                    width: fragment.reference.width(),
                    ty: fragment.ty,
                })
            })
            .collect::<Result<Vec<_>, WordError>>()?;
        let total_width = fragments.iter().try_fold(0u32, |width, fragment| {
            width
                .checked_add(fragment.width)
                .ok_or_else(|| WordError::new("instance output width exceeds 32-bit capacity"))
        })?;
        if total_width != parent_ty.width() {
            return Err(WordError::new(
                "instance output fragments do not cover the connection value",
            ));
        }

        let mut offset = 0u32;
        for fragment in fragments {
            let value = if offset == 0 && fragment.width == parent_ty.width() {
                output
            } else {
                self.target
                    .extract(output, offset, fragment.width, source.clone())?
            };
            let value = self.coerce_value(value, fragment.ty, source)?;
            let target_signal = self.target.signal(fragment.signal).ok_or_else(|| {
                WordError::new("instance output target references a missing signal")
            })?;
            let target = if fragment.lsb == 0 && fragment.width == target_signal.ty.width() {
                LValue::signal(fragment.signal)
            } else {
                let msb = fragment
                    .lsb
                    .checked_add(fragment.width - 1)
                    .ok_or_else(|| {
                        WordError::new("instance output range exceeds 32-bit capacity")
                    })?;
                LValue::signal(fragment.signal).with_range(BitRange {
                    msb,
                    lsb: fragment.lsb,
                })
            };
            self.target.connect(target, value, source.clone())?;
            offset = offset
                .checked_add(fragment.width)
                .ok_or_else(|| WordError::new("instance output offset exceeds 32-bit capacity"))?;
        }
        Ok(())
    }

    fn coerce_value(
        &mut self,
        value: ValueId,
        target: WordType,
        source: &SourceSpan,
    ) -> Result<ValueId, WordError> {
        let current = self.target.value_ty(value)?;
        if current == target {
            return Ok(value);
        }
        let kind = if current.width() > target.width() {
            CastKind::Truncate
        } else if current.is_signed() {
            CastKind::SignExtend
        } else {
            CastKind::ZeroExtend
        };
        self.target.cast(kind, value, target, source.clone())
    }

    fn unique_signal_name(&mut self, base: &str) -> String {
        unique_linked_name(base, |name| {
            self.target.signal_id(name).is_some() || self.target.memory_id(name).is_some()
        })
    }

    fn unique_memory_name(&mut self, base: &str) -> String {
        self.unique_signal_name(base)
    }

    fn unique_instance_name(&mut self, base: &str) -> String {
        unique_linked_name(base, |name| self.target.instance_id(name).is_some())
    }
}

fn unique_linked_name(base: &str, occupied: impl Fn(&str) -> bool) -> String {
    if !occupied(base) {
        return base.to_string();
    }
    (0_u64..=u64::MAX)
        .map(|suffix| format!("{base}$linked${suffix}"))
        .find(|candidate| !occupied(candidate))
        .expect("the linked-name suffix space cannot be exhausted in memory")
}

impl ModuleRemap {
    pub(crate) fn signal(&self, signal: SignalId) -> Result<SignalId, WordError> {
        self.signals.get(signal.index()).copied().ok_or_else(|| {
            WordError::new(format!(
                "linked elaboration references unknown signal {signal:?}"
            ))
        })
    }

    pub(crate) fn value(&self, value: ValueId) -> Result<ValueId, WordError> {
        self.value_base
            .checked_add(value.index())
            .ok_or_else(|| WordError::new("linked elaboration value ID exceeds address capacity"))
            .and_then(ValueId::from_index)
    }

    pub(crate) fn memory(&self, memory: MemoryId) -> Result<MemoryId, WordError> {
        self.memories.get(memory.index()).copied().ok_or_else(|| {
            WordError::new(format!(
                "linked elaboration references unknown memory {memory:?}"
            ))
        })
    }

    fn operation(&self, operation: OpId) -> Result<OpId, WordError> {
        self.operation_base
            .checked_add(operation.index())
            .ok_or_else(|| {
                WordError::new("linked elaboration operation ID exceeds address capacity")
            })
            .and_then(OpId::from_index)
    }

    fn memory_read_port(&self, port: MemoryReadPortId) -> Result<MemoryReadPortId, WordError> {
        self.memory_read_port_base
            .checked_add(port.index())
            .ok_or_else(|| {
                WordError::new("linked elaboration memory read port ID exceeds address capacity")
            })
            .and_then(MemoryReadPortId::from_index)
    }

    fn memory_write_port(&self, port: MemoryWritePortId) -> Result<MemoryWritePortId, WordError> {
        self.memory_write_port_base
            .checked_add(port.index())
            .ok_or_else(|| {
                WordError::new("linked elaboration memory write port ID exceeds address capacity")
            })
            .and_then(MemoryWritePortId::from_index)
    }
}

fn hierarchical_name(prefix: &str, local: &str) -> String {
    if prefix.is_empty() {
        local.to_string()
    } else {
        format!("{prefix}/{local}")
    }
}

#[cfg(test)]
mod tests;
