// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::ConstBits;
use std::num::NonZeroU32;

#[test]
fn source_spans_remain_compact_for_large_designs() {
    assert_eq!(std::mem::size_of::<SourceSpan>(), 16);
}

#[test]
fn generated_source_identity_is_stable_role_separated_and_location_independent() {
    let identity = SourceIdentity::from_bytes([7; 32]);
    let first_parent =
        SourceSpan::located("top.sv", Some(10), Some(4), "assignment").with_identity(identity);
    let moved_parent =
        SourceSpan::located("top.sv", Some(80), Some(12), "assignment").with_identity(identity);

    let first = first_parent.derived("procedural read", b"slice-0").unwrap();
    let moved = moved_parent.derived("procedural read", b"slice-0").unwrap();
    let other_role = first_parent.derived("procedural read", b"slice-1").unwrap();
    let other_transform = first_parent.derived("predicate", b"slice-0").unwrap();

    assert_eq!(first.identity(), moved.identity());
    assert_ne!(first.identity(), other_role.identity());
    assert_ne!(first.identity(), other_transform.identity());
    assert_eq!(first.line(), Some(10));
    assert_eq!(moved.line(), Some(80));
    assert!(
        SourceSpan::default()
            .derived("procedural read", b"slice-0")
            .is_none()
    );
}

#[test]
fn module_annotations_are_interned_validated_and_serialized() {
    let mut module = WordModule::new("macro");
    module
        .add_annotation(
            AnnotationTarget::Module,
            "black_box",
            AnnotationValueSpec::Integer {
                bits: crate::ConstBits::from_bin_str("1").unwrap(),
                signed: false,
            },
            SourceSpan::construct("module attribute"),
        )
        .unwrap();
    module
        .add_annotation(
            AnnotationTarget::Module,
            "implementation",
            AnnotationValueSpec::String("memory macro".to_string()),
            SourceSpan::default(),
        )
        .unwrap();
    module.set_definition_kind(DefinitionKind::BlackBox);
    module.consolidate_names().unwrap();
    module.validate().unwrap();

    let encoded = serde_json::to_string(&module).unwrap();
    let decoded: WordModule = serde_json::from_str(&encoded).unwrap();

    decoded.validate().unwrap();
    assert_eq!(decoded, module);
    assert_eq!(decoded.definition_kind(), DefinitionKind::BlackBox);
    assert_eq!(decoded.name_str(decoded.annotations()[0].name), "black_box");
}

#[test]
fn synthesis_directives_are_typed_unique_and_serialized() {
    let mut module = WordModule::new("top");
    let signal = module
        .add_wire("state", WordType::bits(1).unwrap(), SourceSpan::default())
        .unwrap();
    module
        .set_synthesis_directive(
            AnnotationTarget::Signal(signal),
            SynthesisDirectiveKind::KeepSignal,
            true,
            SourceSpan::construct("first"),
        )
        .unwrap();
    module
        .set_synthesis_directive(
            AnnotationTarget::Signal(signal),
            SynthesisDirectiveKind::KeepSignal,
            false,
            SourceSpan::construct("replacement"),
        )
        .unwrap();
    module.validate().unwrap();

    assert_eq!(module.synthesis_directives().len(), 1);
    assert_eq!(
        module.synthesis_directive(
            AnnotationTarget::Signal(signal),
            SynthesisDirectiveKind::KeepSignal
        ),
        Some(false)
    );
    let encoded = serde_json::to_string(&module).unwrap();
    let decoded: WordModule = serde_json::from_str(&encoded).unwrap();
    decoded.validate().unwrap();
    assert_eq!(decoded, module);
}

#[test]
fn synthesis_directive_scope_is_validated() {
    let mut module = WordModule::new("top");
    let port = module
        .add_port(
            "a",
            PortDirection::Input,
            WordType::bits(1).unwrap(),
            SourceSpan::default(),
        )
        .unwrap();

    assert_eq!(
        module
            .set_synthesis_directive(
                AnnotationTarget::Port(port),
                SynthesisDirectiveKind::DontTouch,
                true,
                SourceSpan::default(),
            )
            .unwrap_err()
            .to_string(),
        format!(
            "synthesis directive DontTouch is not valid on {:?}",
            AnnotationTarget::Port(port)
        )
    );
}

#[test]
fn black_box_definition_rejects_synthesizable_body_state() {
    let mut module = WordModule::new("macro");
    module.set_definition_kind(DefinitionKind::BlackBox);
    module
        .add_wire(
            "internal",
            WordType::bits(1).unwrap(),
            SourceSpan::default(),
        )
        .unwrap();

    assert_eq!(
        module.validate().unwrap_err().to_string(),
        "black-box definition must contain only its declared ports"
    );
}

fn bits(width: u32) -> WordType {
    WordType::bits(width).unwrap()
}

fn constant(module: &mut WordModule, text: &str) -> ValueId {
    let ty = bits(u32::try_from(text.len()).unwrap());
    module
        .constant(
            ConstBits::from_bin_str(text).unwrap(),
            ty,
            SourceSpan::default(),
        )
        .unwrap()
}

#[test]
fn speculative_rollback_is_atomic_when_an_annotation_retains_the_suffix() {
    let mut module = WordModule::new("top");
    module
        .add_annotation(
            AnnotationTarget::Module,
            "temporary",
            AnnotationValueSpec::String("candidate".to_string()),
            SourceSpan::default(),
        )
        .unwrap();
    let checkpoint = module.speculation_checkpoint();
    let speculative = constant(&mut module, "0");
    module.annotations[0].target = AnnotationTarget::Value(speculative);

    let error = module.rollback_speculation(checkpoint).unwrap_err();

    assert!(error.to_string().contains("would strand"));
    assert!(module.value(speculative).is_some());
    module.annotations[0].target = AnnotationTarget::Module;
    module.rollback_speculation(checkpoint).unwrap();
    assert!(module.value(speculative).is_none());
    module.validate().unwrap();
}

#[test]
fn speculative_rollback_is_atomic_when_a_retained_operation_reads_the_suffix() {
    let mut module = WordModule::new("top");
    let zero = constant(&mut module, "0");
    let result = module
        .unary(UnaryOp::BitNot, zero, SourceSpan::default())
        .unwrap();
    let ValueKind::Operation(operation) = module.value(result).unwrap().kind else {
        panic!("unary result must name its operation");
    };
    let checkpoint = module.speculation_checkpoint();
    let speculative = constant(&mut module, "1");
    module.operation_mut(operation).unwrap().kind = OpKind::Unary {
        op: UnaryOp::BitNot,
        arg: speculative,
    };

    let error = module.rollback_speculation(checkpoint).unwrap_err();

    assert!(error.to_string().contains("would strand"));
    assert!(module.value(speculative).is_some());
    module.operation_mut(operation).unwrap().kind = OpKind::Unary {
        op: UnaryOp::BitNot,
        arg: zero,
    };
    module.rollback_speculation(checkpoint).unwrap();
    assert!(module.value(speculative).is_none());
    module.validate().unwrap();
}

#[test]
fn speculative_rollback_is_atomic_when_a_memory_port_retains_the_suffix() {
    let mut module = WordModule::new("top");
    let memory = module
        .add_memory(
            "mem",
            bits(1),
            NonZeroU32::new(2).unwrap(),
            SourceSpan::default(),
        )
        .unwrap();
    let data = module
        .add_wire("read_data", bits(1), SourceSpan::default())
        .unwrap();
    let address = constant(&mut module, "0");
    let read_port = module
        .add_memory_read_port(MemoryReadPort {
            memory,
            address,
            data,
            timing: MemoryReadTiming::Asynchronous,
            read_during_write: ReadDuringWrite::OldData,
            source: SourceSpan::default(),
        })
        .unwrap();
    let checkpoint = module.speculation_checkpoint();
    let speculative = constant(&mut module, "1");
    module.memory_read_ports[read_port.index()].address = speculative;

    let error = module.rollback_speculation(checkpoint).unwrap_err();

    assert!(error.to_string().contains("would strand"));
    assert!(module.value(speculative).is_some());
    module.memory_read_ports[read_port.index()].address = address;
    module.rollback_speculation(checkpoint).unwrap();
    assert!(module.value(speculative).is_none());
    module.validate().unwrap();
}

#[test]
fn speculative_rollback_removes_generated_signals_and_memory_reads() {
    let mut module = WordModule::new("top");
    let memory = module
        .add_memory(
            "mem",
            bits(1),
            NonZeroU32::new(2).unwrap(),
            SourceSpan::default(),
        )
        .unwrap();
    let address = constant(&mut module, "0");
    let signal_count = module.signals().len();
    let value_count = module.values().len();
    let checkpoint = module.speculation_checkpoint();
    let data = module
        .add_generated_wire(bits(1), SourceSpan::default())
        .unwrap();
    module
        .add_memory_read_port(MemoryReadPort {
            memory,
            address,
            data,
            timing: MemoryReadTiming::Asynchronous,
            read_during_write: ReadDuringWrite::OldData,
            source: SourceSpan::default(),
        })
        .unwrap();
    let read = module.read_signal(data, SourceSpan::default()).unwrap();

    module.rollback_speculation(checkpoint).unwrap();

    assert_eq!(module.signals().len(), signal_count);
    assert_eq!(module.values().len(), value_count);
    assert!(module.memory_read_ports().is_empty());
    assert!(module.signal(data).is_none());
    assert!(module.value(read).is_none());
    module.validate().unwrap();
}

#[test]
fn speculative_rollback_rejects_a_checkpoint_from_another_module() {
    let source = WordModule::new("source");
    let checkpoint = source.speculation_checkpoint();
    let mut target = WordModule::new("target");
    let speculative = constant(&mut target, "0");

    let error = target.rollback_speculation(checkpoint).unwrap_err();

    assert!(error.to_string().contains("different module"));
    assert!(target.value(speculative).is_some());
    target.validate().unwrap();
}

#[test]
fn bit_type_rejects_zero_width() {
    let err = WordType::bits(0).unwrap_err();
    assert_eq!(err.to_string(), "RTL bit type width must be non-zero");
}

#[test]
fn module_assigns_stable_arena_ids() {
    let mut module = WordModule::new("top");
    let a = module
        .add_port("a", PortDirection::Input, bits(1), SourceSpan::default())
        .unwrap();
    let y = module
        .add_port("y", PortDirection::Output, bits(1), SourceSpan::default())
        .unwrap();

    assert_eq!(a.index(), 0);
    assert_eq!(y.index(), 1);
    assert_eq!(module.port(a).unwrap().signal.index(), 0);
    assert_eq!(module.port(y).unwrap().signal.index(), 1);
    assert_eq!(module.signal_id("y").unwrap().index(), 1);
}

#[test]
fn cloned_modules_can_be_renamed_and_retarget_instances() {
    let mut original = WordModule::new("child");
    original
        .add_instance("u_leaf", "leaf", Vec::new(), SourceSpan::default())
        .unwrap();
    original.consolidate_names().unwrap();

    let mut clone = original.clone();
    clone.rename("child_0").unwrap();
    let instance = clone.instance_id("u_leaf").unwrap();
    clone.set_instance_module(instance, "leaf_0").unwrap();
    clone.consolidate_names().unwrap();

    assert_eq!(original.name(), "child");
    assert_eq!(original.name_str(original.instances()[0].module), "leaf");
    assert_eq!(clone.name(), "child_0");
    assert_eq!(clone.name_str(clone.instances()[0].module), "leaf_0");
}

#[test]
fn signal_references_are_zero_cost_typed_slices() {
    let mut module = WordModule::new("top");
    let data = module
        .add_port("data", PortDirection::Input, bits(8), SourceSpan::default())
        .unwrap();
    let signal = module.port(data).unwrap().signal;

    let slice = module
        .read_signal_slice(signal, 2, 3, SourceSpan::default())
        .unwrap();
    let value = module.value(slice).unwrap();

    assert_eq!(value.ty.width(), 3);
    assert!(!value.ty.is_signed());
    assert!(matches!(
        value.kind,
        ValueKind::Signal(SignalRef {
            signal: referenced,
            lsb: 2,
            ..
        }) if referenced == signal
    ));
    assert!(
        module
            .read_signal_slice(signal, 7, 2, SourceSpan::default())
            .unwrap_err()
            .to_string()
            .contains("exceeds signal width")
    );
}

#[test]
fn structural_signal_fragments_are_lsb_first() {
    let mut module = WordModule::new("top");
    let high = module
        .add_wire("high", bits(2), SourceSpan::default())
        .unwrap();
    let low = module
        .add_wire("low", bits(3), SourceSpan::default())
        .unwrap();
    let high_value = module.read_signal(high, SourceSpan::default()).unwrap();
    let low_value = module.read_signal(low, SourceSpan::default()).unwrap();
    let connection = module
        .concat(vec![high_value, low_value], SourceSpan::default())
        .unwrap();

    let fragments = module.signal_fragments(connection).unwrap();

    assert_eq!(fragments.len(), 2);
    assert_eq!(fragments[0].reference.signal, low);
    assert_eq!(fragments[0].reference.width(), 3);
    assert_eq!(fragments[1].reference.signal, high);
    assert_eq!(fragments[1].reference.width(), 2);
}

#[test]
fn module_names_use_compact_storage_and_parallel_reads() {
    const SIGNALS: u32 = 10_000;

    let mut module = WordModule::new("top");
    let mut expected_bytes = "top".len();
    for index in 0..SIGNALS {
        let name = format!("signal_{index}");
        expected_bytes += name.len();
        let id = module
            .add_wire(&name, bits(1), SourceSpan::default())
            .unwrap();
        assert_eq!(id.index(), index as usize);
    }
    module.consolidate_names().unwrap();

    assert_eq!(module.name_count(), SIGNALS as usize + 1);
    assert_eq!(module.name_storage_bytes(), expected_bytes);
    let names_before_miss = module.name_count();
    assert_eq!(module.signal_id("missing"), None);
    assert_eq!(module.instance_id("missing"), None);
    assert_eq!(module.name_count(), names_before_miss);

    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                for index in 0..SIGNALS {
                    let name = format!("signal_{index}");
                    assert_eq!(module.signal_id(&name).unwrap().index(), index as usize);
                }
            });
        }
    });
}

#[test]
fn duplicate_signal_names_are_rejected() {
    let mut module = WordModule::new("top");
    module
        .add_wire("n", bits(1), SourceSpan::default())
        .unwrap();
    let err = module
        .add_wire("n", bits(1), SourceSpan::default())
        .unwrap_err();
    assert_eq!(err.to_string(), "duplicate RTL signal 'n'");
}

#[test]
fn generated_wires_do_not_require_a_mutable_name_table() {
    let mut module = WordModule::new("top");
    module.consolidate_names().unwrap();

    let signal = module
        .add_generated_wire(bits(3), SourceSpan::default())
        .unwrap();

    let signal = module.signal(signal).unwrap();
    assert_eq!(signal.name, None);
    assert_eq!(signal.kind, SignalKind::Wire);
    assert_eq!(signal.ty, bits(3));
}

#[test]
fn mux_batch_resolves_local_rows_and_rejects_forward_references_atomically() {
    let mut module = WordModule::new("top");
    let zero = constant(&mut module, "0");
    let one = constant(&mut module, "1");
    let source = SourceSpan::default();
    let values = module
        .append_mux_batch(vec![
            MuxBatchOperation {
                cond: BatchValue::Existing(one),
                then_value: BatchValue::Existing(one),
                else_value: BatchValue::Existing(zero),
                source: source.clone(),
            },
            MuxBatchOperation {
                cond: BatchValue::Existing(zero),
                then_value: BatchValue::Generated(0),
                else_value: BatchValue::Existing(one),
                source: source.clone(),
            },
        ])
        .unwrap();
    assert_eq!(values.len(), 2);
    module.validate().unwrap();

    let value_count = module.values().len();
    let operation_count = module.operations().len();
    let error = module
        .append_mux_batch(vec![MuxBatchOperation {
            cond: BatchValue::Existing(one),
            then_value: BatchValue::Generated(0),
            else_value: BatchValue::Existing(zero),
            source,
        }])
        .unwrap_err();
    assert!(error.to_string().contains("not earlier"));
    assert_eq!(module.values().len(), value_count);
    assert_eq!(module.operations().len(), operation_count);
}

#[test]
fn memories_have_typed_ports_and_deterministic_ids() {
    assert_eq!(std::mem::size_of::<MemoryId>(), 4);
    assert_eq!(std::mem::size_of::<MemoryReadPortId>(), 4);
    assert_eq!(std::mem::size_of::<MemoryWritePortId>(), 4);

    let mut module = WordModule::new("top");
    let memory = module
        .add_memory(
            "mem",
            bits(16),
            NonZeroU32::new(256).unwrap(),
            SourceSpan::default(),
        )
        .unwrap();
    let address = constant(&mut module, "00000000");
    let data = module
        .add_wire("read_data", bits(16), SourceSpan::default())
        .unwrap();
    let read = module
        .add_memory_read_port(MemoryReadPort {
            memory,
            address,
            data,
            timing: MemoryReadTiming::Asynchronous,
            read_during_write: ReadDuringWrite::OldData,
            source: SourceSpan::default(),
        })
        .unwrap();
    let write_data = constant(&mut module, "0000000000000000");
    let clock = constant(&mut module, "0");
    let mask = constant(&mut module, "00");
    let write = module
        .add_memory_write_port(MemoryWritePort {
            memory,
            address,
            data: write_data,
            clock: MemoryClock {
                value: clock,
                edge: Edge::Pos,
            },
            enable: None,
            mask: Some(MemoryWriteMask {
                value: mask,
                granularity: NonZeroU32::new(8).unwrap(),
                active_high: true,
            }),
            priority: 0,
            source: SourceSpan::default(),
        })
        .unwrap();

    assert_eq!(memory.index(), 0);
    assert_eq!(read.index(), 0);
    assert_eq!(write.index(), 0);
    assert_eq!(module.memory_id("mem"), Some(memory));
    module.validate_memories().unwrap();

    let json = serde_json::to_string(&module).unwrap();
    let mut decoded: WordModule = serde_json::from_str(&json).unwrap();
    decoded.validate_memories().unwrap();
    assert_eq!(decoded, module);

    let resources = decoded.take_memory_resources();
    assert_eq!(resources.memories.len(), 1);
    assert_eq!(resources.reads.len(), 1);
    assert_eq!(resources.writes.len(), 1);
    assert_eq!(decoded.memory_id("mem"), None);
    decoded.validate_memories().unwrap();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the test exercises one memory-port validation matrix with shared fixtures"
)]
fn memory_ports_reject_invalid_widths_drivers_and_priorities() {
    let mut module = WordModule::new("top");
    let memory = module
        .add_memory(
            "mem",
            bits(8),
            NonZeroU32::new(17).unwrap(),
            SourceSpan::default(),
        )
        .unwrap();
    let narrow_address = constant(&mut module, "0000");
    let address = constant(&mut module, "00000");
    let data = module
        .add_wire("read_data", bits(8), SourceSpan::default())
        .unwrap();
    let error = module
        .add_memory_read_port(MemoryReadPort {
            memory,
            address: narrow_address,
            data,
            timing: MemoryReadTiming::Asynchronous,
            read_during_write: ReadDuringWrite::Undefined,
            source: SourceSpan::default(),
        })
        .unwrap_err();
    assert!(error.to_string().contains("at least 5 bits"));

    module
        .add_memory_read_port(MemoryReadPort {
            memory,
            address,
            data,
            timing: MemoryReadTiming::Asynchronous,
            read_during_write: ReadDuringWrite::Undefined,
            source: SourceSpan::default(),
        })
        .unwrap();
    let duplicate = module
        .add_memory_read_port(MemoryReadPort {
            memory,
            address,
            data,
            timing: MemoryReadTiming::Asynchronous,
            read_during_write: ReadDuringWrite::Undefined,
            source: SourceSpan::default(),
        })
        .unwrap_err();
    assert!(duplicate.to_string().contains("exactly one"));

    let write_data = constant(&mut module, "00000000");
    let clock = constant(&mut module, "0");
    let wide_control = constant(&mut module, "00");
    let clock_error = module
        .add_memory_write_port(MemoryWritePort {
            memory,
            address,
            data: write_data,
            clock: MemoryClock {
                value: wide_control,
                edge: Edge::Pos,
            },
            enable: None,
            mask: None,
            priority: 6,
            source: SourceSpan::default(),
        })
        .unwrap_err();
    assert!(clock_error.to_string().contains("clock must be 1 bit"));

    let enabled_data = module
        .add_wire("enabled_read", bits(8), SourceSpan::default())
        .unwrap();
    let enable_error = module
        .add_memory_read_port(MemoryReadPort {
            memory,
            address,
            data: enabled_data,
            timing: MemoryReadTiming::Synchronous {
                clock: MemoryClock {
                    value: clock,
                    edge: Edge::Pos,
                },
                enable: Some(Enable {
                    value: wide_control,
                    active_high: true,
                }),
                disabled: DisabledRead::Hold,
            },
            read_during_write: ReadDuringWrite::OldData,
            source: SourceSpan::default(),
        })
        .unwrap_err();
    assert!(enable_error.to_string().contains("enable must be 1 bit"));

    let bad_mask = constant(&mut module, "00");
    let bad_mask = module
        .add_memory_write_port(MemoryWritePort {
            memory,
            address,
            data: write_data,
            clock: MemoryClock {
                value: clock,
                edge: Edge::Pos,
            },
            enable: None,
            mask: Some(MemoryWriteMask {
                value: bad_mask,
                granularity: NonZeroU32::new(2).unwrap(),
                active_high: true,
            }),
            priority: 7,
            source: SourceSpan::default(),
        })
        .unwrap_err();
    assert!(bad_mask.to_string().contains("covers 4 bits"));

    let port = MemoryWritePort {
        memory,
        address,
        data: write_data,
        clock: MemoryClock {
            value: clock,
            edge: Edge::Pos,
        },
        enable: None,
        mask: None,
        priority: 7,
        source: SourceSpan::default(),
    };
    module.add_memory_write_port(port.clone()).unwrap();
    let duplicate = module.add_memory_write_port(port).unwrap_err();
    assert!(duplicate.to_string().contains("priority 7 is not unique"));
}

#[test]
fn mux_requires_one_bit_condition_and_matching_branch_types() {
    let mut module = WordModule::new("top");
    let cond = constant(&mut module, "10");
    let one = constant(&mut module, "1");
    let zero = constant(&mut module, "0");

    let err = module
        .mux(cond, one, zero, SourceSpan::default())
        .unwrap_err();
    assert_eq!(err.to_string(), "mux condition must be 1 bit wide, got 2");

    let cond = constant(&mut module, "1");
    let wide = constant(&mut module, "10");
    let err = module
        .mux(cond, wide, zero, SourceSpan::default())
        .unwrap_err();
    assert!(err.to_string().contains("mux branch types differ"));
}

#[test]
fn tri_state_preserves_data_type_enable_polarity_and_validation() {
    let mut module = WordModule::new("top");
    let data = constant(&mut module, "1010");
    let enable = constant(&mut module, "1");
    let driver = module
        .tri_state(
            data,
            Enable {
                value: enable,
                active_high: false,
            },
            SourceSpan::default(),
        )
        .unwrap();

    assert_eq!(
        module.value(driver).unwrap().ty,
        module.value(data).unwrap().ty
    );
    assert!(matches!(
        module
            .operation(match module.value(driver).unwrap().kind {
                ValueKind::Operation(operation) => operation,
                _ => unreachable!(),
            })
            .unwrap()
            .kind,
        OpKind::TriState {
            data: stored,
            enable: Enable {
                value: stored_enable,
                active_high: false,
            },
        } if stored == data && stored_enable == enable
    ));
    module.validate().unwrap();

    let wide_enable = constant(&mut module, "11");
    let error = module
        .tri_state(
            data,
            Enable {
                value: wide_enable,
                active_high: true,
            },
            SourceSpan::default(),
        )
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "tri-state enable must be 1 bit wide, got 2"
    );
}

#[test]
fn shift_result_type_follows_left_operand() {
    let mut module = WordModule::new("top");
    let value = module
        .constant(
            ConstBits::from_bin_str("111111111111111111111111111111111").unwrap(),
            WordType::new(33, true, LogicStateKind::FourState).unwrap(),
            SourceSpan::default(),
        )
        .unwrap();
    let amount = constant(&mut module, "10101");
    for op in [BinaryOp::Shl, BinaryOp::Shr, BinaryOp::Ashr] {
        let shifted = module
            .binary(op, value, amount, SourceSpan::default())
            .unwrap();
        let ty = module.value(shifted).unwrap().ty;
        assert_eq!(ty.width(), 33);
        assert!(ty.is_signed());
    }
}

#[test]
fn concat_extract_and_connect_check_widths() {
    let mut module = WordModule::new("top");
    let wide = constant(&mut module, "1010");
    let low = module.extract(wide, 0, 2, SourceSpan::default()).unwrap();
    let high = module.extract(wide, 2, 2, SourceSpan::default()).unwrap();
    let combined = module
        .concat(vec![high, low], SourceSpan::construct("concat"))
        .unwrap();
    assert_eq!(module.value(combined).unwrap().ty.width(), 4);

    let dst = module
        .add_wire("dst", bits(4), SourceSpan::default())
        .unwrap();
    module
        .connect(LValue::signal(dst), combined, SourceSpan::default())
        .unwrap();
    assert_eq!(module.connects().len(), 1);

    let err = module
        .extract(wide, 3, 2, SourceSpan::default())
        .unwrap_err();
    assert_eq!(err.to_string(), "extract [3 +: 2] exceeds value width 4");
}

#[test]
fn dynamic_extract_preserves_runtime_offset_and_checks_types() {
    let mut module = WordModule::new("top");
    let value = constant(&mut module, "10101010");
    let offset = constant(&mut module, "101");
    let selected = module
        .dynamic_extract(value, offset, 1, SourceSpan::default())
        .unwrap();

    assert_eq!(module.value(selected).unwrap().ty.width(), 1);
    assert!(matches!(
        module.operations().last().unwrap().kind,
        OpKind::DynamicExtract {
            value: actual_value,
            offset: actual_offset,
            ..
        } if actual_value == value && actual_offset == offset
    ));

    let err = module
        .dynamic_extract(value, offset, 9, SourceSpan::default())
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "dynamic extract width 9 exceeds value width 8"
    );
}

#[test]
fn unsigned_range_proves_scaled_dynamic_offsets() {
    let mut module = WordModule::new("top");
    let index_port = module
        .add_port(
            "index",
            PortDirection::Input,
            bits(5),
            SourceSpan::default(),
        )
        .unwrap();
    let index = module
        .read_signal(
            module.port(index_port).unwrap().signal,
            SourceSpan::default(),
        )
        .unwrap();
    let widened = module
        .cast(CastKind::ZeroExtend, index, bits(11), SourceSpan::default())
        .unwrap();
    let scale = constant(&mut module, "00001000000");
    let offset = module
        .binary(BinaryOp::Mul, widened, scale, SourceSpan::default())
        .unwrap();

    assert_eq!(
        unsigned_value_range(&module, offset),
        Some(UnsignedValueRange {
            minimum: 0,
            maximum: 1984,
        })
    );
}

#[test]
fn dynamic_insert_preserves_base_type_and_runtime_operands() {
    let mut module = WordModule::new("top");
    let value = constant(&mut module, "10101010");
    let offset = constant(&mut module, "101");
    let replacement = constant(&mut module, "11");
    let updated = module
        .dynamic_insert(value, offset, replacement, SourceSpan::default())
        .unwrap();

    assert_eq!(
        module.value(updated).unwrap().ty,
        module.value(value).unwrap().ty
    );
    assert!(matches!(
        module.operations().last().unwrap().kind,
        OpKind::DynamicInsert {
            value: actual_value,
            offset: actual_offset,
            replacement: actual_replacement,
        } if actual_value == value
            && actual_offset == offset
            && actual_replacement == replacement
    ));
}

#[test]
fn register_checks_controls_and_preserves_type() {
    let mut module = WordModule::new("top");
    let d = constant(&mut module, "1010");
    let clk = constant(&mut module, "1");
    let rst = constant(&mut module, "0");
    let rst_value = constant(&mut module, "0000");
    let q_name = module.names.intern("q").unwrap();
    let q = module
        .register(
            RegisterOp {
                name: Some(q_name),
                d,
                clock: clk,
                edge: Edge::Pos,
                enable: None,
                resets: vec![Reset {
                    kind: ResetKind::Async,
                    value: rst,
                    active_high: true,
                    reset_value: rst_value,
                }],
            },
            SourceSpan::construct("always_ff"),
        )
        .unwrap();

    assert_eq!(module.value(q).unwrap().ty.width(), 4);
    assert!(matches!(
        &module.operations()[0].kind,
        OpKind::Register(RegisterOp {
            edge: Edge::Pos,
            ..
        })
    ));
}

#[test]
fn register_rejects_mismatched_reset_type() {
    let mut module = WordModule::new("top");
    let d = constant(&mut module, "1010");
    let clk = constant(&mut module, "1");
    let rst = constant(&mut module, "0");
    let rst_value = constant(&mut module, "00");
    let err = module
        .register(
            RegisterOp {
                name: None,
                d,
                clock: clk,
                edge: Edge::Pos,
                enable: None,
                resets: vec![Reset {
                    kind: ResetKind::Async,
                    value: rst,
                    active_high: true,
                    reset_value: rst_value,
                }],
            },
            SourceSpan::construct("always_ff"),
        )
        .unwrap_err();

    assert!(err.to_string().contains("register reset value type"));
}

#[test]
fn instance_connections_are_validated() {
    let mut module = WordModule::new("top");
    let a = module
        .add_wire("a", bits(1), SourceSpan::default())
        .unwrap();
    let value = module.read_signal(a, SourceSpan::default()).unwrap();
    let instance_id = module
        .add_instance(
            "u_child",
            "child",
            vec![("in".to_string(), value, SourceSpan::default())],
            SourceSpan::default(),
        )
        .unwrap();
    assert_eq!(instance_id.index(), 0);
    assert_eq!(module.instances().len(), 1);

    let err = module
        .add_instance(
            "u_bad",
            "child",
            vec![
                ("in".to_string(), value, SourceSpan::default()),
                ("in".to_string(), value, SourceSpan::default()),
            ],
            SourceSpan::default(),
        )
        .unwrap_err();
    assert!(err.to_string().contains("duplicate connection port"));
}

#[test]
fn compact_type_layouts_have_an_allocation_free_structural_view() {
    let mut module = WordModule::new("top");
    let signal = module
        .add_wire("value", bits(3), SourceSpan::default())
        .unwrap();
    module
        .set_signal_type_layout(
            signal,
            &TypeLayoutSpec::Struct {
                fields: vec![
                    TypeLayoutFieldSpec {
                        name: "flag".to_string(),
                        bit_offset: 0,
                        layout: TypeLayoutSpec::Scalar,
                    },
                    TypeLayoutFieldSpec {
                        name: "payload".to_string(),
                        bit_offset: 1,
                        layout: TypeLayoutSpec::Array {
                            kind: ArrayKind::Packed,
                            range: IndexRange { left: 1, right: 0 },
                            element: Box::new(TypeLayoutSpec::Scalar),
                        },
                    },
                ],
            },
        )
        .unwrap();

    let mut events = Vec::new();
    let traversal = module.visit_signal_type_layout(signal, |event| events.push(event));

    assert_eq!(traversal, TypeLayoutTraversal::Complete);
    assert_eq!(
        events,
        [
            TypeLayoutEvent::Struct { field_count: 2 },
            TypeLayoutEvent::Field {
                name: "flag",
                bit_offset: 0,
            },
            TypeLayoutEvent::Scalar,
            TypeLayoutEvent::Field {
                name: "payload",
                bit_offset: 1,
            },
            TypeLayoutEvent::Array {
                kind: ArrayKind::Packed,
                range: IndexRange { left: 1, right: 0 },
            },
            TypeLayoutEvent::Scalar,
        ]
    );
}
