// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use std::num::NonZeroU32;

fn test_span() -> word::SourceSpan {
    word::SourceSpan::stable("test")
}

fn input(module: &mut word::WordModule, name: &str, width: u32) -> word::ValueId {
    let port = module
        .add_port(
            name,
            word::PortDirection::Input,
            word::WordType::bits(width).unwrap(),
            test_span(),
        )
        .unwrap();
    module
        .read_signal(module.port(port).unwrap().signal, test_span())
        .unwrap()
}

fn memory_module() -> (
    word::WordModule,
    word::MemoryId,
    word::ValueId,
    word::ValueId,
    word::ValueId,
) {
    let mut module = word::WordModule::new("top");
    let clock = input(&mut module, "clock", 1);
    let address = input(&mut module, "address", 2);
    let data = input(&mut module, "data", 8);
    let memory = module
        .add_memory(
            "memory",
            word::WordType::bits(8).unwrap(),
            NonZeroU32::new(2).unwrap(),
            test_span(),
        )
        .unwrap();
    (module, memory, clock, address, data)
}

#[test]
fn out_of_range_read_selects_unknown_instead_of_word_zero() {
    let (mut module, memory, clock, address, data) = memory_module();
    let read_data = module
        .add_wire("read_data", word::WordType::bits(8).unwrap(), test_span())
        .unwrap();
    module
        .add_memory_read_port(word::MemoryReadPort {
            memory,
            address,
            data: read_data,
            timing: word::MemoryReadTiming::Asynchronous,
            read_during_write: word::ReadDuringWrite::OldData,
            source: test_span(),
        })
        .unwrap();
    module
        .add_memory_write_port(word::MemoryWritePort {
            memory,
            address,
            data,
            clock: word::MemoryClock {
                value: clock,
                edge: word::Edge::Pos,
            },
            enable: None,
            mask: None,
            priority: 0,
            source: test_span(),
        })
        .unwrap();

    let ownership = lower_memories_to_register_banks(&mut module).unwrap();
    let operations = ownership
        .operations()
        .filter_map(|(operation, owner)| (owner == memory).then_some(operation))
        .collect::<Vec<_>>();
    assert!(!operations.is_empty());
    let states = ownership
        .state_values()
        .filter_map(|(value, owner)| (owner == memory).then_some(value))
        .collect::<Vec<_>>();
    for (ordinal, &value) in states.iter().enumerate() {
        assert_eq!(
            ownership.state_value(memory, u32::try_from(ordinal).unwrap()),
            Some(value)
        );
    }
    assert_eq!(
        ownership.state_value(memory, u32::try_from(states.len()).unwrap()),
        None
    );
    let mut value = module
        .connects()
        .iter()
        .find(|connect| connect.target.signal == read_data)
        .unwrap()
        .value;
    for _ in 0..2 {
        let operation = match &module.value(value).unwrap().kind {
            word::ValueKind::Operation(operation) => module.operation(*operation).unwrap(),
            _ => panic!("memory select must be a mux chain"),
        };
        let &word::OpKind::Mux { else_value, .. } = &operation.kind else {
            panic!("memory select must be a mux chain");
        };
        value = else_value;
    }
    let word::ValueKind::Constant(bits) = &module.value(value).unwrap().kind else {
        panic!("out-of-range memory select must end in an unknown constant");
    };
    assert!(bits.as_slice().iter().all(|bit| *bit == opto_ir::BitVal::X));
}

#[test]
fn disabled_synchronous_read_updates_to_unknown() {
    let (mut module, memory, clock, address, data) = memory_module();
    let enable = input(&mut module, "read_enable", 1);
    let read_data = module
        .add_wire("read_data", word::WordType::bits(8).unwrap(), test_span())
        .unwrap();
    module
        .add_memory_read_port(word::MemoryReadPort {
            memory,
            address,
            data: read_data,
            timing: word::MemoryReadTiming::Synchronous {
                clock: word::MemoryClock {
                    value: clock,
                    edge: word::Edge::Pos,
                },
                enable: Some(word::Enable {
                    value: enable,
                    active_high: true,
                }),
                disabled: word::DisabledRead::Undefined,
            },
            read_during_write: word::ReadDuringWrite::OldData,
            source: test_span(),
        })
        .unwrap();
    module
        .add_memory_write_port(word::MemoryWritePort {
            memory,
            address,
            data,
            clock: word::MemoryClock {
                value: clock,
                edge: word::Edge::Pos,
            },
            enable: None,
            mask: None,
            priority: 0,
            source: test_span(),
        })
        .unwrap();

    lower_memories_to_register_banks(&mut module).unwrap();
    let register = read_register(&module, read_data);
    assert!(register.enable.is_none());
    let word::OpKind::Mux { else_value, .. } = &operation_of(&module, register.d).kind else {
        panic!("disabled-undefined read must select an unknown value");
    };
    assert_unknown(&module, *else_value);
}

#[test]
fn undefined_read_during_write_materializes_an_unknown_collision_path() {
    let (mut module, memory, clock, address, data) = memory_module();
    let read_data = module
        .add_wire("read_data", word::WordType::bits(8).unwrap(), test_span())
        .unwrap();
    module
        .add_memory_read_port(word::MemoryReadPort {
            memory,
            address,
            data: read_data,
            timing: word::MemoryReadTiming::Synchronous {
                clock: word::MemoryClock {
                    value: clock,
                    edge: word::Edge::Pos,
                },
                enable: None,
                disabled: word::DisabledRead::Hold,
            },
            read_during_write: word::ReadDuringWrite::Undefined,
            source: test_span(),
        })
        .unwrap();
    module
        .add_memory_write_port(word::MemoryWritePort {
            memory,
            address,
            data,
            clock: word::MemoryClock {
                value: clock,
                edge: word::Edge::Pos,
            },
            enable: None,
            mask: None,
            priority: 0,
            source: test_span(),
        })
        .unwrap();

    lower_memories_to_register_banks(&mut module).unwrap();
    let register = read_register(&module, read_data);
    let word::OpKind::Mux { then_value, .. } = &operation_of(&module, register.d).kind else {
        panic!("undefined collision must select an unknown value");
    };
    assert_unknown(&module, *then_value);
}

fn read_register(module: &word::WordModule, signal: word::SignalId) -> &word::RegisterOp {
    let value = module
        .connects()
        .iter()
        .find(|connect| connect.target.signal == signal)
        .expect("read data signal must be connected")
        .value;
    let word::ValueKind::Operation(operation) = &module.value(value).unwrap().kind else {
        panic!("synchronous read must be driven by a register");
    };
    let word::OpKind::Register(register) = &module.operation(*operation).unwrap().kind else {
        panic!("synchronous read must be driven by a register");
    };
    register
}

fn operation_of(module: &word::WordModule, value: word::ValueId) -> &word::Operation {
    let word::ValueKind::Operation(operation) = &module.value(value).unwrap().kind else {
        panic!("value must be produced by an operation");
    };
    module.operation(*operation).unwrap()
}

fn assert_unknown(module: &word::WordModule, value: word::ValueId) {
    let word::ValueKind::Constant(bits) = &module.value(value).unwrap().kind else {
        panic!("expected an unknown constant");
    };
    assert!(bits.as_slice().iter().all(|bit| *bit == BitVal::X));
}

#[test]
fn different_write_clocks_fail_before_resource_extraction() {
    let (mut module, memory, first_clock, address, data) = memory_module();
    let second_clock = input(&mut module, "second_clock", 1);
    for (priority, clock) in [first_clock, second_clock].into_iter().enumerate() {
        module
            .add_memory_write_port(word::MemoryWritePort {
                memory,
                address,
                data,
                clock: word::MemoryClock {
                    value: clock,
                    edge: word::Edge::Pos,
                },
                enable: None,
                mask: None,
                priority: u32::try_from(priority).unwrap(),
                source: test_span(),
            })
            .unwrap();
    }

    let error = lower_memories_to_register_banks(&mut module).unwrap_err();
    assert!(error.to_string().contains("multiple write clocks"));
    assert_eq!(module.memories().len(), 1);
    assert_eq!(module.memory_write_ports().len(), 2);
}

#[test]
fn read_only_memory_has_no_implicit_fallback_implementation() {
    let (mut module, _, _, _, _) = memory_module();

    let error = lower_memories_to_register_banks(&mut module).unwrap_err();
    assert!(error.to_string().contains("no writable storage"));
    assert_eq!(module.memories().len(), 1);
}

fn macro_pin(name: &str, direction: opto_library::TargetPinDirection) -> opto_library::TargetPin {
    opto_library::TargetPin {
        name: name.to_string(),
        direction,
        function: None,
        three_state: None,
        capacitance: None,
        rise_capacitance: None,
        fall_capacitance: None,
        receiver_capacitance: None,
        fanout_load: None,
        next_state_type: None,
        clock_gate_role: None,
        timing_arcs: Vec::new(),
    }
}

fn macro_output_pin(name: &str) -> opto_library::TargetPin {
    let mut pin = macro_pin(name, opto_library::TargetPinDirection::Output);
    pin.timing_arcs.push(opto_library::TargetTimingArc {
        related_pin: "A0".to_string(),
        timing_type: opto_library::TargetTimingType::Combinational,
        timing_sense: opto_library::TimingSense::NonUnate,
        delay_model: Some(opto_library::ArcDelayModel::Nldm(
            opto_library::NldmTimingModel::new(
                Some(opto_library::LookupTable::scalar(0.1)),
                Some(opto_library::LookupTable::scalar(0.1)),
                None,
                None,
            ),
        )),
        rise_constraint: None,
        fall_constraint: None,
    });
    pin
}

fn ram_macro() -> opto_library::TargetCell {
    let mut pins = vec![
        macro_pin("A0", opto_library::TargetPinDirection::Input),
        macro_pin("A1", opto_library::TargetPinDirection::Input),
        macro_pin("CLK", opto_library::TargetPinDirection::Input),
    ];
    pins.extend(
        (0..8).map(|bit| macro_pin(&format!("D{bit}"), opto_library::TargetPinDirection::Input)),
    );
    pins.extend((0..8).map(|bit| macro_output_pin(&format!("Q{bit}"))));
    opto_library::TargetCell {
        name: "RAM4X8".to_string(),
        area: Some(1.0),
        dont_use: false,
        usage: opto_library::TargetCellUsage::default(),
        pins,
        sequential: Vec::new(),
        clock_gate: None,
        memory: Some(opto_library::TargetMemory {
            kind: opto_library::TargetMemoryKind::Ram,
            depth: 4,
            word_width: 8,
            read_ports: vec![opto_library::TargetMemoryReadPort {
                address_pins: vec!["A0".to_string(), "A1".to_string()],
                data_pins: (0..8).map(|bit| format!("Q{bit}")).collect(),
                clock: None,
                enable: None,
                disabled: opto_library::TargetMemoryDisabledRead::Undefined,
                read_during_write: opto_library::TargetMemoryReadDuringWrite::OldData,
            }],
            write_ports: vec![opto_library::TargetMemoryWritePort {
                address_pins: vec!["A0".to_string(), "A1".to_string()],
                data_pins: (0..8).map(|bit| format!("D{bit}")).collect(),
                clock: opto_library::TargetMemoryClock {
                    pin: "CLK".to_string(),
                    edge: opto_library::TargetMemoryEdge::Rising,
                },
                enable: None,
                mask_pins: Vec::new(),
                mask_granularity: 0,
                mask_active_high: true,
            }],
        }),
    }
}

#[test]
fn compatible_macro_is_selected_and_materialized_with_exact_scalar_pins() {
    let mut module = word::WordModule::new("top");
    let clock = input(&mut module, "clock", 1);
    let address = input(&mut module, "address", 2);
    let data = input(&mut module, "data", 8);
    let memory = module
        .add_memory(
            "memory",
            word::WordType::bits(8).unwrap(),
            NonZeroU32::new(4).unwrap(),
            test_span(),
        )
        .unwrap();
    let read_data = module
        .add_wire("read_data", word::WordType::bits(8).unwrap(), test_span())
        .unwrap();
    module
        .add_memory_read_port(word::MemoryReadPort {
            memory,
            address,
            data: read_data,
            timing: word::MemoryReadTiming::Asynchronous,
            read_during_write: word::ReadDuringWrite::OldData,
            source: test_span(),
        })
        .unwrap();
    module
        .add_memory_write_port(word::MemoryWritePort {
            memory,
            address,
            data,
            clock: word::MemoryClock {
                value: clock,
                edge: word::Edge::Pos,
            },
            enable: None,
            mask: None,
            priority: 0,
            source: test_span(),
        })
        .unwrap();
    let cells: opto_library::TargetCellSet = vec![ram_macro()].into();

    assert_eq!(
        compatible_memory_macros(&module, memory, &cells).unwrap(),
        [0]
    );
    lower_selected_memories(
        &mut module,
        &[crate::planning::regional::MemoryImplementationCandidate::Macro(0)],
        &cells,
    )
    .unwrap();

    assert!(module.memories().is_empty());
    assert!(module.memory_read_ports().is_empty());
    assert!(module.memory_write_ports().is_empty());
    assert_eq!(module.instances().len(), 1);
    let instance = &module.instances()[0];
    assert_eq!(module.name_str(instance.module), "RAM4X8");
    assert_eq!(instance.connections.len(), 19);
    assert_eq!(
        instance
            .connections
            .iter()
            .map(|connection| module.name_str(connection.port))
            .collect::<Vec<_>>(),
        [
            "A0", "A1", "CLK", "D0", "D1", "D2", "D3", "D4", "D5", "D6", "D7", "Q0", "Q1", "Q2",
            "Q3", "Q4", "Q5", "Q6", "Q7"
        ]
    );
}

#[test]
fn shared_macro_pins_require_identical_logical_bindings() {
    let mut module = word::WordModule::new("top");
    let clock = input(&mut module, "clock", 1);
    let first_address = input(&mut module, "first_address", 2);
    let second_address = input(&mut module, "second_address", 2);
    let data = input(&mut module, "data", 8);
    let memory = module
        .add_memory(
            "memory",
            word::WordType::bits(8).unwrap(),
            NonZeroU32::new(4).unwrap(),
            test_span(),
        )
        .unwrap();
    for (name, address) in [
        ("first_read_data", first_address),
        ("second_read_data", second_address),
    ] {
        let read_data = module
            .add_wire(name, word::WordType::bits(8).unwrap(), test_span())
            .unwrap();
        module
            .add_memory_read_port(word::MemoryReadPort {
                memory,
                address,
                data: read_data,
                timing: word::MemoryReadTiming::Asynchronous,
                read_during_write: word::ReadDuringWrite::OldData,
                source: test_span(),
            })
            .unwrap();
    }
    module
        .add_memory_write_port(word::MemoryWritePort {
            memory,
            address: first_address,
            data,
            clock: word::MemoryClock {
                value: clock,
                edge: word::Edge::Pos,
            },
            enable: None,
            mask: None,
            priority: 0,
            source: test_span(),
        })
        .unwrap();

    let mut target = ram_macro();
    target.pins.extend(
        (0..8).map(|bit| macro_pin(&format!("R{bit}"), opto_library::TargetPinDirection::Output)),
    );
    let contract = target.memory.as_mut().unwrap();
    let mut second_read = contract.read_ports[0].clone();
    second_read.data_pins = (0..8).map(|bit| format!("R{bit}")).collect();
    contract.read_ports.push(second_read);
    let cells: opto_library::TargetCellSet = vec![target].into();

    assert!(
        compatible_memory_macros(&module, memory, &cells)
            .unwrap()
            .is_empty()
    );
    let error = lower_selected_memories(
        &mut module,
        &[crate::planning::regional::MemoryImplementationCandidate::Macro(0)],
        &cells,
    )
    .unwrap_err();
    assert!(error.to_string().contains("incompatible"));
    assert_eq!(module.memories().len(), 1);
    assert_eq!(module.memory_read_ports().len(), 2);
}

#[test]
fn regional_synthesis_selects_characterized_macro_over_register_bank() {
    let mut module = word::WordModule::new("top");
    let clock = input(&mut module, "clock", 1);
    let address = input(&mut module, "address", 2);
    let data = input(&mut module, "data", 8);
    let output = module
        .add_port(
            "q",
            word::PortDirection::Output,
            word::WordType::bits(8).unwrap(),
            test_span(),
        )
        .unwrap();
    let memory = module
        .add_memory(
            "memory",
            word::WordType::bits(8).unwrap(),
            NonZeroU32::new(4).unwrap(),
            test_span(),
        )
        .unwrap();
    let read_data = module
        .add_wire("read_data", word::WordType::bits(8).unwrap(), test_span())
        .unwrap();
    module
        .add_memory_read_port(word::MemoryReadPort {
            memory,
            address,
            data: read_data,
            timing: word::MemoryReadTiming::Asynchronous,
            read_during_write: word::ReadDuringWrite::OldData,
            source: test_span(),
        })
        .unwrap();
    module
        .add_memory_write_port(word::MemoryWritePort {
            memory,
            address,
            data,
            clock: word::MemoryClock {
                value: clock,
                edge: word::Edge::Pos,
            },
            enable: None,
            mask: None,
            priority: 0,
            source: test_span(),
        })
        .unwrap();
    let read = module.read_signal(read_data, test_span()).unwrap();
    module
        .connect(
            word::LValue::signal(module.port(output).unwrap().signal),
            read,
            test_span(),
        )
        .unwrap();

    let source = opto_ir::rtl::RtlModule::structural(module).unwrap();
    let mut uncharacterized = ram_macro();
    for pin in &mut uncharacterized.pins {
        pin.timing_arcs.clear();
    }
    let error = crate::synthesize_rtl_module(
        source.clone(),
        crate::SynthesisOptions {
            target_cells: vec![uncharacterized].into(),
        },
        crate::test_runtime(),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("lack complete early/late output timing characterization"),
        "{error}"
    );

    let result = crate::synthesize_rtl_module(
        source,
        crate::SynthesisOptions {
            target_cells: vec![ram_macro()].into(),
        },
        crate::test_runtime(),
    )
    .unwrap();
    let mut verilog = Vec::new();
    opto_formats::write_mapped_verilog(&mut verilog, result.mapped()).unwrap();
    let verilog = String::from_utf8(verilog).unwrap();

    assert_eq!(verilog.matches("RAM4X8").count(), 1, "{verilog}");
}
