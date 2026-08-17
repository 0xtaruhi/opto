// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use opto_ir::proc::{
    AssignmentMode, ProcTarget, ProcedureId, ProcedureKind, Sensitivity, TerminatorKind,
};
use opto_ir::word::{BinaryOp, Edge, OpKind, SignalResolution, TypeSelector, ValueKind};

#[test]
fn verilog_frontend_eliminates_reference_ports_during_linked_elaboration() {
    let source = TestSource::new(
        "reference-port.sv",
        "module child(ref logic [3:0] value, input logic [3:0] data); always_comb value = data; endmodule\nmodule top(input logic [3:0] data, output logic [3:0] y); logic [3:0] shared; child u_child(.value(shared), .data(data)); assign y = shared; endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions {
            top: Some("top".to_string()),
            ..FrontendOptions::default()
        },
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let top = update
        .modules
        .iter()
        .find(|module| module.word().name() == "top")
        .unwrap();
    let flat = opto_ir::rtl::elaborate_linked_root(top, update.modules.iter()).unwrap();
    let shared = flat.word().signal_id("shared").unwrap();
    assert!(flat.word().signal_id("u_child/value").is_none());
    assert!(flat.procedures().effects().iter().any(
        |effect| matches!(effect.target, ProcTarget::Signal { signal, .. } if signal == shared)
    ));
}

#[test]
fn verilog_frontend_composes_dynamic_unpacked_reference_port_aliases() {
    let source = TestSource::new(
        "dynamic-reference-port.sv",
        "module child(ref logic [7:0] value, input logic [7:0] data); always_comb value = data; endmodule\nmodule top(input logic [7:0] data, input logic [1:0] index, output logic [7:0] y); logic [7:0] values [0:3]; child u_child(.value(values[index]), .data(data)); assign y = values[index]; endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions {
            top: Some("top".to_string()),
            ..FrontendOptions::default()
        },
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let top = update
        .modules
        .iter()
        .find(|module| module.word().name() == "top")
        .unwrap();
    let flat = opto_ir::rtl::elaborate_linked_root(top, update.modules.iter()).unwrap();
    let values = flat.word().signal_id("values").unwrap();
    assert!(flat.procedures().effects().iter().any(|effect| matches!(
        effect.target,
        ProcTarget::Signal {
            signal,
            select: opto_ir::proc::TargetSelect::Dynamic { width, .. },
        } if signal == values && width.get() == 8
    )));
}

#[test]
fn verilog_frontend_keeps_continuously_driven_unpacked_arrays_as_signals() {
    let source = TestSource::new(
        "continuous-array.sv",
        "module top(input logic [7:0] a, b, c, d, input logic [1:0] index, output logic [7:0] fixed, dynamic); wire [7:0] values [0:3]; assign values[0] = a; assign values[1] = b; assign values[2] = c; assign values[3] = d; assign fixed = values[2]; assign dynamic = values[index]; endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let module = update.modules[0].word();
    let values = module.signal_id("values").unwrap();

    assert!(module.memory_id("values").is_none());
    assert_eq!(
        module.signal_bit_selectors(values, 0).unwrap(),
        Some(vec![TypeSelector::Index(0), TypeSelector::Index(0)])
    );
    assert_eq!(
        module.signal_bit_selectors(values, 24).unwrap(),
        Some(vec![TypeSelector::Index(3), TypeSelector::Index(0)])
    );
    assert!(module.operations().iter().any(|operation| matches!(
        operation.kind,
        OpKind::DynamicExtract { width, .. } if width.get() == 8
    )));
}

#[test]
fn verilog_frontend_preserves_multidimensional_continuous_array_layout() {
    let source = TestSource::new(
        "continuous-multidimensional-array.sv",
        "module top(input logic [3:0] a, b, c, d, e, f, input logic row, input logic [1:0] column, output logic [3:0] fixed, dynamic); wire [3:0] values [0:1][0:2]; assign values[0][0] = a; assign values[0][1] = b; assign values[0][2] = c; assign values[1][0] = d; assign values[1][1] = e; assign values[1][2] = f; assign fixed = values[1][2]; assign dynamic = values[row][column]; endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let module = update.modules[0].word();
    let values = module.signal_id("values").unwrap();

    assert!(module.memory_id("values").is_none());
    assert_eq!(
        module.signal_bit_selectors(values, 0).unwrap(),
        Some(vec![
            TypeSelector::Index(0),
            TypeSelector::Index(0),
            TypeSelector::Index(0),
        ])
    );
    assert_eq!(
        module.signal_bit_selectors(values, 20).unwrap(),
        Some(vec![
            TypeSelector::Index(1),
            TypeSelector::Index(2),
            TypeSelector::Index(0),
        ])
    );
    assert!(module.operations().iter().any(|operation| matches!(
        operation.kind,
        OpKind::DynamicExtract { width, .. } if width.get() == 4
    )));
}

#[test]
fn verilog_frontend_keeps_always_comb_unpacked_targets_as_signals() {
    let source = TestSource::new(
        "always-comb-array.sv",
        "module top(input logic [7:0] a, b, input logic [1:0] index, output logic [7:0] y); logic [7:0] values [0:3]; always_comb begin values[0] = a; values[index] = b; end assign y = values[1]; endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let rtl = &update.modules[0];
    let module = rtl.word();
    let values = module.signal_id("values").unwrap();
    let effects = rtl.procedures().effects();

    assert!(module.memory_id("values").is_none());
    assert_eq!(
        module.signal_bit_selectors(values, 24).unwrap(),
        Some(vec![TypeSelector::Index(3), TypeSelector::Index(0)])
    );
    assert!(effects.iter().any(|effect| matches!(
        effect.target,
        ProcTarget::Signal {
            signal,
            select: opto_ir::proc::TargetSelect::Static(range),
        } if signal == values && range.width() == 8
    )));
    assert!(effects.iter().any(|effect| matches!(
        effect.target,
        ProcTarget::Signal {
            signal,
            select: opto_ir::proc::TargetSelect::Dynamic { width, .. },
        } if signal == values && width.get() == 8
    )));
}

#[test]
fn verilog_frontend_keeps_always_latch_unpacked_targets_as_signals() {
    let source = TestSource::new(
        "always-latch-array.sv",
        "module top(input logic enable, input logic [7:0] a, b, input logic [1:0] index, output logic [7:0] y); logic [7:0] values [0:3]; always_latch if (enable) begin values[2] <= a; values[index] <= b; end assign y = values[1]; endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let rtl = &update.modules[0];
    let module = rtl.word();
    let values = module.signal_id("values").unwrap();
    let effects = rtl.procedures().effects();

    assert!(module.memory_id("values").is_none());
    assert_eq!(rtl.procedures().procedures()[0].kind, ProcedureKind::Latch);
    assert!(effects.iter().any(|effect| matches!(
        effect.target,
        ProcTarget::Signal {
            signal,
            select: opto_ir::proc::TargetSelect::Static(range),
        } if signal == values && range.width() == 8
    )));
    assert!(effects.iter().any(|effect| matches!(
        effect.target,
        ProcTarget::Signal {
            signal,
            select: opto_ir::proc::TargetSelect::Dynamic { width, .. },
        } if signal == values && width.get() == 8
    )));
}

#[test]
fn verilog_frontend_keeps_async_reset_flop_arrays_as_register_signals() {
    let source = TestSource::new(
        "async-reset-register-array.sv",
        "module top(input logic clk, rst_n, we, input logic [7:0] d[2], \
         output logic [7:0] q[2]); logic [7:0] state[2]; \
         for (genvar i = 0; i < 2; i++) begin \
           always_ff @(posedge clk or negedge rst_n) begin \
             if (!rst_n) state[i] <= '0; else if (we) state[i] <= d[i]; \
           end \
         end \
         assign q = state; endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let rtl = &update.modules[0];
    let module = rtl.word();
    let state = module.signal_id("state").unwrap();

    assert!(module.memory_id("state").is_none());
    assert!(module.memories().is_empty());
    assert_eq!(
        module.signal_bit_selectors(state, 8).unwrap(),
        Some(vec![TypeSelector::Index(1), TypeSelector::Index(0)])
    );
    assert_eq!(rtl.procedures().procedures().len(), 2);
    assert!(rtl.procedures().effects().iter().all(|effect| matches!(
        effect.target,
        ProcTarget::Signal { signal, .. } if signal == state
    )));
}

#[test]
fn verilog_frontend_materializes_automatic_flop_array_locals_for_normalization() {
    let source = TestSource::new(
        "automatic-flop-array.sv",
        "module top(input logic clk, input logic [1:0] index, input logic [7:0] a, b, output logic [7:0] q); always_ff @(posedge clk) begin automatic logic [7:0] temporary [0:3]; temporary[0] = a; temporary[1] = b; temporary[2] = a ^ b; temporary[3] = a + b; q <= temporary[index]; end endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let module = update.modules[0].word();
    assert!(module.memories().is_empty());
    assert!(
        module
            .signals()
            .iter()
            .any(|signal| signal.kind == opto_ir::word::SignalKind::ProcessLocal)
    );
    assert!(
        module
            .operations()
            .iter()
            .any(|operation| matches!(operation.kind, OpKind::DynamicExtract { .. }))
    );
    assert!(module.operations().iter().any(|operation| matches!(
        operation.kind,
        OpKind::DynamicExtract { width, .. } if width.get() == 8
    )));
}

#[test]
fn verilog_frontend_makes_process_local_part_selects_unsigned() {
    let source = TestSource::new(
        "automatic-signed-slice.sv",
        "module top(input logic [4:0] data, output logic [2:0] y); always_comb begin automatic integer temporary; temporary = data; y = temporary[2:0]; end endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let rtl = &update.modules[0];
    let output = rtl.word().signal_id("y").unwrap();
    let effect = rtl
        .procedures()
        .effects()
        .iter()
        .find(
            |effect| matches!(effect.target, ProcTarget::Signal { signal, .. } if signal == output),
        )
        .unwrap();

    assert!(!rtl.word().value(effect.value).unwrap().ty.is_signed());
}

#[test]
fn verilog_frontend_flattens_noncontiguous_unpacked_state() {
    let source = TestSource::new(
        "noncontiguous-unpacked-state.sv",
        "typedef struct { logic [7:0] lanes [0:1]; logic [3:0] tag; } entry_t; module top(input logic clk, row, column, input logic [7:0] data, output logic [7:0] q); entry_t state [0:1]; always_ff @(posedge clk) state[row].lanes[column] <= data; assign q = state[row].lanes[column]; endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let rtl = &update.modules[0];
    let module = rtl.word();
    let state = module
        .signal_id("state")
        .expect("irregular aggregate state must use flattened signal storage");

    assert!(module.memory_id("state").is_none());
    assert_eq!(module.signal(state).unwrap().ty.width(), 40);
    assert!(rtl.procedures().effects().iter().any(|effect| matches!(
        effect.target,
        ProcTarget::Signal {
            signal,
            select: opto_ir::proc::TargetSelect::Dynamic { width, .. },
        } if signal == state && width.get() == 8
    )));
}

#[test]
fn verilog_frontend_flattens_async_reset_array_state() {
    let source = TestSource::new(
        "async-reset-array-state.sv",
        "module top(input logic clk, rst_n, we, input logic [1:0] address, input logic [7:0] data, output logic [7:0] q); logic [7:0] memory [0:3]; integer index; always_ff @(posedge clk or negedge rst_n) begin if (!rst_n) begin for (index = 0; index < 4; index = index + 1) memory[index] <= '0; end else if (we) memory[address] <= data; end assign q = memory[address]; endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let module = update.modules[0].word();
    let memory = module
        .signal_id("memory")
        .expect("asynchronously reset array state must use flattened signal storage");
    let index = module
        .signal_id("index")
        .expect("the source loop index remains a declared module signal");

    assert!(module.memory_id("memory").is_none());
    assert_eq!(module.signal(memory).unwrap().ty.width(), 32);
    assert_eq!(
        update.modules[0]
            .procedures()
            .sensitivity_events(ProcedureId::FIRST)
            .unwrap()
            .len(),
        2
    );
    assert!(
        update.modules[0]
            .procedures()
            .effects()
            .iter()
            .any(|effect| matches!(
                effect.target,
                ProcTarget::Signal {
                    signal,
                    select: opto_ir::proc::TargetSelect::Dynamic { width, .. },
                } if signal == memory && width.get() == 8
            ))
    );
    let memory_effects = update.modules[0]
        .procedures()
        .effects()
        .iter()
        .filter(
            |effect| matches!(effect.target, ProcTarget::Signal { signal, .. } if signal == memory),
        )
        .count();
    assert_eq!(
        memory_effects, 6,
        "joint normalization removes the final value-unreachable reset clone"
    );
    assert!(!update.modules[0].procedures().effects().iter().any(
        |effect| matches!(effect.target, ProcTarget::Signal { signal, .. } if signal == index)
    ));
}

#[test]
fn verilog_frontend_rejects_mixed_unpacked_array_drivers() {
    let source = TestSource::new(
        "mixed-array-drivers.sv",
        "module top(input logic [7:0] a, b, output logic [7:0] y); logic [7:0] values [0:1]; assign values[0] = a; always_comb values[1] = b; assign y = values[0] ^ values[1]; endmodule\n",
    );
    let error = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap_err();

    assert!(error.to_string().contains(
        "unpacked storage 'values' has mixed continuous and combinational/latch procedural drivers"
    ));
}

#[test]
fn verilog_frontend_rejects_comb_and_flop_unpacked_array_drivers() {
    let source = TestSource::new(
        "mixed-procedural-array-drivers.sv",
        "module top(input logic clk, input logic [7:0] a, b, output logic [7:0] y); logic [7:0] values [0:1]; always_comb values[0] = a; always_ff @(posedge clk) values[1] <= b; assign y = values[0] ^ values[1]; endmodule\n",
    );
    let error = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap_err();

    assert!(error.to_string().contains(
        "unpacked storage 'values' has mixed combinational/latch procedural and edge-triggered procedural drivers"
    ));
}

#[test]
fn verilog_frontend_aligns_static_and_dynamic_unpacked_array_indices() {
    let source = TestSource::new(
        "inspect-array-read.sv",
        "module top(input logic clk, input logic [63:0] a, b, c, d, input logic [1:0] index, output logic [31:0] y); logic [63:0] values[4]; always_ff @(posedge clk) begin values[0] <= a; values[1] <= b; values[2] <= c; values[3] <= d; end assign y = values[index][31:0]; endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let rtl = &update.modules[0];
    let module = rtl.word();
    let values = module.memory_id("values").unwrap();
    assert!(module.signal_id("values").is_none());
    assert_eq!(module.memory(values).unwrap().depth.get(), 4);
    assert_eq!(module.memory(values).unwrap().element_type.width(), 64);

    let addresses = rtl
        .procedures()
        .effects()
        .iter()
        .filter_map(|effect| match effect.target {
            ProcTarget::Memory {
                memory,
                address,
                select: opto_ir::proc::TargetSelect::Whole,
            } if memory == values => opto_ir::word::unsigned_value_range(module, address),
            _ => None,
        })
        .map(|range| (range.minimum(), range.maximum()))
        .collect::<Vec<_>>();
    assert_eq!(addresses, [(0, 0), (1, 1), (2, 2), (3, 3)]);

    let read = &module.memory_read_ports()[0];
    assert_eq!(read.memory, values);
    assert_eq!(
        opto_ir::word::unsigned_value_range(module, read.address)
            .unwrap()
            .maximum(),
        3
    );
    assert!(module.operations().iter().any(|operation| matches!(
        operation.kind,
        OpKind::Extract { width, .. } if width.get() == 32
    )));
}

#[test]
fn verilog_frontend_uses_memory_ports_for_static_and_dynamic_accesses() {
    let source = TestSource::new(
        "memory-ports.sv",
        "module top(input logic clk, we, input logic [1:0] address, input logic signed [7:0] data, output logic signed [7:0] dynamic_read, static_read); logic signed [7:0] memory[4]; assign dynamic_read = memory[address]; assign static_read = memory[2]; always_ff @(posedge clk) if (we) memory[address] <= data; endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let rtl = &update.modules[0];
    let module = rtl.word();
    let memory = module.memory_id("memory").unwrap();

    assert!(module.signal_id("memory").is_none());
    assert!(module.memory(memory).unwrap().element_type.is_signed());
    assert_eq!(module.memory_read_ports().len(), 2);
    assert!(value_depends_on_signal_named(
        module,
        module.memory_read_ports()[0].address,
        "address"
    ));
    let ranges = module
        .memory_read_ports()
        .iter()
        .map(|port| {
            let range = opto_ir::word::unsigned_value_range(module, port.address).unwrap();
            (range.minimum(), range.maximum())
        })
        .collect::<Vec<_>>();
    assert_eq!(ranges, [(0, 3), (2, 2)]);
    assert!(rtl.procedures().effects().iter().any(|effect| matches!(
        effect.target,
        ProcTarget::Memory {
            memory: target,
            select: opto_ir::proc::TargetSelect::Whole,
            ..
        } if target == memory
    )));
}

#[test]
fn verilog_frontend_composes_nested_dynamic_memory_bit_targets() {
    let source = TestSource::new(
        "nested-memory-bit-target.sv",
        "module top(input logic clk, we, input logic [1:0] address, bit_index, input logic data, output logic [7:0] q); logic [7:0] memory[4]; always_ff @(posedge clk) begin if (we) begin memory[2][bit_index] <= data; memory[address][bit_index + 2'd1] <= data; end end assign q = memory[address]; endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let rtl = &update.modules[0];
    let module = rtl.word();
    let memory = module.memory_id("memory").unwrap();
    let dynamic_writes = rtl
        .procedures()
        .effects()
        .iter()
        .filter(|effect| {
            matches!(
                effect.target,
                ProcTarget::Memory {
                    memory: target,
                    select: opto_ir::proc::TargetSelect::Dynamic { width, .. },
                    ..
                } if target == memory && width.get() == 1
            )
        })
        .count();

    assert_eq!(dynamic_writes, 2);
}

#[test]
fn verilog_frontend_lowers_wide_memory_addresses_without_overflow() {
    let source = TestSource::new(
        "wide-memory-address.sv",
        "module top(input logic clk, we, input logic [63:0] address, \
         input logic [7:0] data, output logic [7:0] result); \
         logic [7:0] memory[4]; \
         always_ff @(posedge clk) if (we) memory[address] <= data; \
         assign result = memory[address]; endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let module = update.modules[0].word();
    let memory = module.memory_id("memory").unwrap();

    assert_eq!(module.memory_read_ports().len(), 1);
    assert_eq!(module.memory_read_ports()[0].memory, memory);
}

#[test]
fn verilog_frontend_narrows_proven_memory_addresses_to_physical_width() {
    let source = TestSource::new(
        "canonical-memory-address.sv",
        "module top(input logic clk, we, input logic [1:0] address, input logic [7:0] data, output logic [7:0] result); logic [7:0] memory[0:3]; always_ff @(posedge clk) if (we) memory[address] <= data; assign result = memory[address]; endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let rtl = &update.modules[0];
    let module = rtl.word();
    let memory = module.memory_id("memory").unwrap();

    assert_eq!(
        module
            .value(module.memory_read_ports()[0].address)
            .unwrap()
            .ty
            .width(),
        2
    );
    assert!(rtl.procedures().effects().iter().any(|effect| matches!(
        effect.target,
        ProcTarget::Memory {
            memory: target,
            ..
        } if target == memory
    )));
}

#[test]
fn verilog_frontend_flattens_whole_memory_reads_in_storage_order() {
    let source = TestSource::new(
        "whole-memory-read.sv",
        "module top(input logic clk, input logic [7:0] a, b, output logic [7:0] q[2]); \
         logic [7:0] memory[2]; \
         always_ff @(posedge clk) begin memory[0] <= a; memory[1] <= b; end \
         assign q = memory; endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let module = update.modules[0].word();
    let memory = module.memory_id("memory").unwrap();

    assert_eq!(module.memory_read_ports().len(), 2);
    let addresses = module
        .memory_read_ports()
        .iter()
        .map(|port| {
            assert_eq!(port.memory, memory);
            let range = opto_ir::word::unsigned_value_range(module, port.address).unwrap();
            (range.minimum(), range.maximum())
        })
        .collect::<Vec<_>>();
    assert_eq!(addresses, [(1, 1), (0, 0)]);
    assert!(module.operations().iter().any(|operation| matches!(
        &operation.kind,
        OpKind::Concat { parts } if parts.len() == 2
    )));
}

#[test]
fn verilog_frontend_expands_whole_memory_writes_in_storage_order() {
    let source = TestSource::new(
        "whole-memory-write.sv",
        "module top(input logic clk, input logic [7:0] data[2]); \
         logic [7:0] memory[2]; \
         always_ff @(posedge clk) memory <= data; endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let rtl = &update.modules[0];
    let module = rtl.word();
    let memory = module.memory_id("memory").unwrap();
    let addresses = rtl
        .procedures()
        .effects()
        .iter()
        .map(|effect| {
            let ProcTarget::Memory {
                memory: target,
                address,
                ..
            } = effect.target
            else {
                panic!("whole-memory write must expand to memory effects");
            };
            assert_eq!(target, memory);
            let range = opto_ir::word::unsigned_value_range(module, address).unwrap();
            (range.minimum(), range.maximum())
        })
        .collect::<Vec<_>>();

    assert_eq!(addresses, [(0, 0), (1, 1)]);
}

#[test]
fn verilog_frontend_expands_multielement_memory_accesses() {
    let source = TestSource::new(
        "dynamic-memory-span.sv",
        "module top(input logic clk, row, input logic [7:0] data[2], \
         output logic [7:0] dynamic_result[2], static_result[2]); \
         logic [7:0] memory[2][2]; \
         always_ff @(posedge clk) begin memory[row] <= data; memory[1] <= data; end \
         assign dynamic_result = memory[row]; \
         assign static_result = memory[1]; endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let rtl = &update.modules[0];
    let memory = rtl.word().memory_id("memory").unwrap();

    assert_eq!(rtl.word().memory_read_ports().len(), 4);
    assert!(
        rtl.word()
            .memory_read_ports()
            .iter()
            .all(|port| port.memory == memory)
    );
    assert_eq!(
        rtl.procedures()
            .effects()
            .iter()
            .filter(|effect| matches!(
                effect.target,
                ProcTarget::Memory { memory: target, .. } if target == memory
            ))
            .count(),
        4
    );
}

#[test]
fn verilog_frontend_keeps_same_clock_dual_writes_as_distinct_memory_effects() {
    let source = TestSource::new(
        "dual-port-memory.sv",
        "module top(input logic clk, we_a, we_b, input logic [1:0] address_a, address_b, \
         input logic [7:0] data_a, data_b, output logic [7:0] result); \
         logic [7:0] memory[4]; \
         always_ff @(posedge clk) if (we_a) memory[address_a] <= data_a; \
         always_ff @(posedge clk) if (we_b) memory[address_b] <= data_b; \
         assign result = memory[address_a]; endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let rtl = &update.modules[0];
    let memory = rtl.word().memory_id("memory").unwrap();

    assert_eq!(rtl.procedures().procedures().len(), 2);
    assert_eq!(
        rtl.procedures()
            .effects()
            .iter()
            .filter(|effect| matches!(
                effect.target,
                ProcTarget::Memory { memory: target, .. } if target == memory
            ))
            .count(),
        2
    );
    assert_eq!(rtl.word().memory_read_ports().len(), 1);
}

#[test]
fn verilog_requires_input_files() {
    let err = Frontend::read_verilog(
        &[],
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("no input files"));
}

#[test]
fn verilog_frontend_lowers_native_slang_views() {
    let source = TestSource::new(
        "native-bridge.sv",
        "module top(input logic a, output logic y); assign y = ~a; endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();

    assert_eq!(update.top.as_deref(), Some("top"));
    assert_eq!(update.modules.len(), 1);
    assert_eq!(update.modules[0].word().ports().len(), 2);
}

#[test]
fn verilog_frontend_preserves_wired_net_resolution() {
    let source = TestSource::new(
        "wired-resolution.v",
        "module top(input a, b, output wand y_and, output wor y_or); assign y_and = a; assign y_and = b; assign y_or = a; assign y_or = b; endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let module = update.modules[0].word();

    assert_eq!(
        module
            .signal(module.signal_id("y_and").unwrap())
            .unwrap()
            .resolution,
        SignalResolution::WiredAnd
    );
    assert_eq!(
        module
            .signal(module.signal_id("y_or").unwrap())
            .unwrap()
            .resolution,
        SignalResolution::WiredOr
    );
}

#[test]
fn verilog_frontend_materializes_tri_state_driver_contract() {
    let source = TestSource::new(
        "wired-tristate.sv",
        "module top(input logic data, enable, output wand y); bufif1 (y, data, enable); endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let module = update.modules[0].word();
    let connect = module.connects().first().unwrap();
    let ValueKind::Operation(operation) = module.value(connect.value).unwrap().kind else {
        panic!("tri-state assignment must be an explicit operation");
    };
    assert!(matches!(
        module.operation(operation).unwrap().kind,
        OpKind::TriState {
            enable: opto_ir::word::Enable {
                active_high: true,
                ..
            },
            ..
        }
    ));
}

#[test]
fn verilog_frontend_lowers_pull_primitives_to_constant_connections() {
    let source = TestSource::new(
        "pull-primitives.v",
        "module top(output wire high, low); pullup (high); pulldown (low); endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let module = update.modules[0].word();
    let high = module.signal_id("high").unwrap();
    let low = module.signal_id("low").unwrap();

    let driven_bit = |signal| {
        let connect = module
            .connects()
            .iter()
            .find(|connect| connect.target.signal == signal)
            .unwrap();
        let ValueKind::Constant(ref constant) = module.value(connect.value).unwrap().kind else {
            panic!("pull primitive must lower to a constant connection");
        };
        constant.bit_lsb(0)
    };
    assert_eq!(driven_bit(high), Some(opto_ir::BitVal::One));
    assert_eq!(driven_bit(low), Some(opto_ir::BitVal::Zero));
}

#[test]
fn verilog_frontend_marks_inout_tri_state_as_a_physical_boundary() {
    let source = TestSource::new(
        "inout-tristate.sv",
        "module top(input logic data, enable, inout wire pad); assign pad = enable ? data : 1'bz; endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let module = update.modules[0].word();

    assert_eq!(
        module
            .signal(module.signal_id("pad").unwrap())
            .unwrap()
            .resolution,
        SignalResolution::TriState
    );
}

#[test]
fn linked_elaboration_preserves_feedback_tri_state_boundary() {
    let source = TestSource::new(
        "feedback-tristate.v",
        "module top(inout wire pad, input enable); reg captured; always @(pad or enable) if (!enable) captured <= pad; assign pad = enable ? ~captured : 1'bz; endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let flat =
        opto_ir::rtl::elaborate_linked_root(&update.modules[0], update.modules.iter()).unwrap();
    let pad = flat.word().signal_id("pad").unwrap();

    assert_eq!(
        flat.word().signal(pad).unwrap().resolution,
        SignalResolution::TriState
    );
}

#[test]
fn verilog_frontend_marks_procedural_tri_state_as_a_physical_boundary() {
    let source = TestSource::new(
        "procedural-tristate.v",
        "module top(input en, data, output reg y); always @(en or data) y <= en ? data : 1'bz; endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let module = update.modules[0].word();
    let y = module.signal_id("y").unwrap();

    assert_eq!(
        module.signal(y).unwrap().resolution,
        SignalResolution::TriState
    );
    let effect = update.modules[0]
        .procedures()
        .effects()
        .iter()
        .find(|effect| matches!(effect.target, ProcTarget::Signal { signal, .. } if signal == y))
        .unwrap();
    let ValueKind::Operation(operation) = module.value(effect.value).unwrap().kind else {
        panic!("procedural tri-state assignment must remain an explicit operation");
    };
    assert!(matches!(
        module.operation(operation).unwrap().kind,
        OpKind::TriState { .. }
    ));
}

#[test]
fn verilog_frontend_keeps_disjoint_continuous_slices_single_driver() {
    let source = TestSource::new(
        "disjoint-continuous-slices.sv",
        "module top(input logic [2:0] a, output logic [2:0] y); assign y[2] = a[2]; assign y[1] = a[1]; assign y[0] = a[0]; endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let module = update.modules[0].word();

    assert_eq!(
        module
            .signal(module.signal_id("y").unwrap())
            .unwrap()
            .resolution,
        SignalResolution::SingleDriver
    );
}

#[test]
fn verilog_frontend_lowers_blackboxes_to_port_only_modules() {
    let source = TestSource::new(
        "blackbox.sv",
        "(* blackbox *) module macro(input logic clk, input logic a, output logic y); logic state; always_ff @(posedge clk) state <= a; assign y = state; endmodule\nmodule top(input logic clk, input logic a, output logic y); macro u_macro(.clk(clk), .a(a), .y(y)); endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions {
            top: Some("top".to_string()),
            ..FrontendOptions::default()
        },
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let blackbox = update
        .modules
        .iter()
        .find(|module| module.word().name() == "macro")
        .unwrap();
    let word = blackbox.word();

    assert_eq!(
        word.definition_kind(),
        opto_ir::word::DefinitionKind::BlackBox
    );
    assert_eq!(word.annotations().len(), 1);
    assert_eq!(word.name_str(word.annotations()[0].name), "blackbox");
    assert_eq!(word.ports().len(), 3);
    assert_eq!(word.signals().len(), 3);
    assert!(word.values().is_empty());
    assert!(word.operations().is_empty());
    assert!(blackbox.procedures().procedures().is_empty());
    assert!(word.connects().is_empty());
}

#[test]
fn false_blackbox_attribute_preserves_synthesizable_body() {
    let source = TestSource::new(
        "false-blackbox.sv",
        "(* blackbox = 0 *) module top(input logic a, output logic y); assign y = ~a; endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions {
            top: Some("top".to_string()),
            ..FrontendOptions::default()
        },
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let word = update.modules[0].word();

    assert_eq!(
        word.definition_kind(),
        opto_ir::word::DefinitionKind::Synthesizable
    );
    assert_eq!(word.annotations().len(), 1);
    assert!(!word.operations().is_empty());
    assert!(!word.connects().is_empty());
}

#[test]
fn verilog_frontend_retains_structural_object_annotations() {
    let source = TestSource::new(
        "object-annotations.sv",
        "module child(input logic a, output logic y); assign y = a; endmodule\n\
         (* module_tag = \"top\" *) module top(a, y);\n\
           (* port_tag = 2 *) input logic a;\n\
           output logic y;\n\
           (* net_tag = \"middle\" *) logic n;\n\
           (* instance_tag *) child u_child(.a(a), .y(n));\n\
           assign y = n;\n\
         endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions {
            top: Some("top".to_string()),
            ..FrontendOptions::default()
        },
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let module = update
        .modules
        .iter()
        .find(|module| module.word().name() == "top")
        .unwrap()
        .word();
    let a_signal = module.signal_id("a").unwrap();
    let opto_ir::word::SignalKind::Port(a) = module.signal(a_signal).unwrap().kind else {
        panic!("source port must retain its port identity");
    };
    let n = module.signal_id("n").unwrap();
    let child = module.instance_id("u_child").unwrap();

    let names = module
        .annotations()
        .iter()
        .map(|annotation| (annotation.target, module.name_str(annotation.name)))
        .collect::<Vec<_>>();
    assert!(names.contains(&(opto_ir::word::AnnotationTarget::Module, "module_tag")));
    assert!(names.contains(&(opto_ir::word::AnnotationTarget::Port(a), "port_tag")));
    assert!(names.contains(&(opto_ir::word::AnnotationTarget::Signal(n), "net_tag")));
    assert!(names.contains(&(
        opto_ir::word::AnnotationTarget::Instance(child),
        "instance_tag"
    )));
}

#[test]
fn verilog_frontend_decodes_supported_synthesis_annotations() {
    let source = TestSource::new(
        "synthesis-annotations.sv",
        "(* dont_touch = \"TRUE\" *) module child(input logic a, output logic y); assign y = a; endmodule\n\
         module top(input logic a, output logic y);\n\
           (* keep = \"yes\", async_reg = 1, dont_touch = 0 *) logic preserved;\n\
           (* keep_hierarchy = \"on\" *) child u_child(.a(a), .y(preserved));\n\
           assign y = preserved;\n\
         endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions {
            top: Some("top".to_string()),
            ..FrontendOptions::default()
        },
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let child = update
        .modules
        .iter()
        .find(|module| module.word().name() == "child")
        .unwrap()
        .word();
    assert_eq!(
        child.synthesis_directive(
            opto_ir::word::AnnotationTarget::Module,
            opto_ir::word::SynthesisDirectiveKind::DontTouch,
        ),
        Some(true)
    );

    let top = update
        .modules
        .iter()
        .find(|module| module.word().name() == "top")
        .unwrap()
        .word();
    let signal = top.signal_id("preserved").unwrap();
    let instance = top.instance_id("u_child").unwrap();
    assert_eq!(
        top.synthesis_directive(
            opto_ir::word::AnnotationTarget::Signal(signal),
            opto_ir::word::SynthesisDirectiveKind::KeepSignal,
        ),
        Some(true)
    );
    assert_eq!(
        top.synthesis_directive(
            opto_ir::word::AnnotationTarget::Signal(signal),
            opto_ir::word::SynthesisDirectiveKind::AsyncRegister,
        ),
        Some(true)
    );
    assert_eq!(
        top.synthesis_directive(
            opto_ir::word::AnnotationTarget::Signal(signal),
            opto_ir::word::SynthesisDirectiveKind::DontTouch,
        ),
        Some(false)
    );
    assert_eq!(
        top.synthesis_directive(
            opto_ir::word::AnnotationTarget::Instance(instance),
            opto_ir::word::SynthesisDirectiveKind::Ungroup,
        ),
        Some(false)
    );
    assert_eq!(top.annotations().len(), 4);
}

#[test]
fn supported_synthesis_annotation_rejects_ambiguous_boolean_values() {
    let source = TestSource::new(
        "invalid-synthesis-annotation.sv",
        "module top(input logic a, output logic y); (* keep = \"maybe\" *) logic n; assign n = a; assign y = n; endmodule\n",
    );
    let error = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("synthesis attribute 'keep' expects a boolean value")
    );
}

#[test]
fn verilog_frontend_connects_net_declaration_assignments() {
    let source = TestSource::new(
        "net-declaration-assignment.sv",
        "module top(input logic [3:0] a, b, output logic [3:0] y); wire [4:0] sum = {1'b0, a} + {1'b0, b}; assign y = sum[3:0]; endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let module = update.modules[0].word();
    let sum = module.signal_id("sum").unwrap();

    assert!(
        module
            .connects()
            .iter()
            .any(|connect| connect.target.signal == sum)
    );
}

#[test]
fn verilog_frontend_preserves_right_shift_semantics() {
    let source = TestSource::new(
        "right-shifts.sv",
        "module top(input logic signed [7:0] value, input logic [2:0] amount, output logic signed [7:0] logical_result, arithmetic_result); assign logical_result = value >> amount; assign arithmetic_result = value >>> amount; endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let shifts = update.modules[0]
        .word()
        .operations()
        .iter()
        .filter_map(|operation| match operation.kind {
            OpKind::Binary { op, .. } if matches!(op, BinaryOp::Shr | BinaryOp::Ashr) => Some(op),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(shifts, [BinaryOp::Shr, BinaryOp::Ashr]);
}

#[test]
fn verilog_frontend_preserves_multidimensional_source_indices() {
    let source = TestSource::new(
        "source-layout.sv",
        "module top(input logic [31:1] addr, output logic y); logic [31:0] rdata_q [2:0]; assign y = addr[1] ^ rdata_q[1][11]; endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let module = update.modules[0].word();
    let addr = module.signal_id("addr").unwrap();
    let rdata = module.signal_id("rdata_q").unwrap();

    assert_eq!(
        module.signal_bit_selectors(addr, 0).unwrap(),
        Some(vec![TypeSelector::Index(1)])
    );
    assert!(module.memory_id("rdata_q").is_none());
    assert_eq!(
        module.signal_bit_selectors(rdata, 43).unwrap(),
        Some(vec![TypeSelector::Index(1), TypeSelector::Index(11)])
    );
    assert!(module.memory_read_ports().is_empty());
}

#[test]
fn verilog_frontend_uses_one_canonical_unpacked_storage_layout() {
    let source = TestSource::new(
        "unpacked-storage-layout.sv",
        "module top(input logic clk, input logic [33:0] d[2], output logic [33:0] q[2]); always_ff @(posedge clk) begin q[0] <= d[0]; q[1] <= d[1]; end endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let module = update.modules[0].word();
    let q = module.signal_id("q").unwrap();

    assert_eq!(
        module.signal_bit_selectors(q, 0).unwrap(),
        Some(vec![TypeSelector::Index(0), TypeSelector::Index(0)])
    );
    assert_eq!(
        module.signal_bit_selectors(q, 34).unwrap(),
        Some(vec![TypeSelector::Index(1), TypeSelector::Index(0)])
    );
}

#[test]
fn verilog_frontend_preserves_expression_source_locations() {
    let source = TestSource::new(
        "source-location.sv",
        "module top(\n  input logic [3:0] a, b,\n  output logic [3:0] y\n);\n  assign y = a + b;\nendmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let add = update.modules[0]
        .word()
        .operations()
        .iter()
        .find(|operation| {
            matches!(
                operation.kind,
                opto_ir::word::OpKind::Binary {
                    op: BinaryOp::Add,
                    ..
                }
            )
        })
        .unwrap();

    assert_eq!(
        std::fs::canonicalize(add.source.file().unwrap()).unwrap(),
        std::fs::canonicalize(&source.path).unwrap()
    );
    assert_eq!(add.source.line(), Some(5));
    assert!(add.source.column().is_some());
}

#[test]
fn verilog_frontend_lowers_always_comb_control_flow() {
    let source = TestSource::new(
        "always-comb.sv",
        "module top(input logic [1:0] sel, input logic a, b, c, output logic y); always_comb begin if (sel == 0) y = a; else case (sel) 1: y = b; default: y = c; endcase end endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let rtl = &update.modules[0];
    let cfg = rtl.procedures();
    let procedure = &cfg.procedures()[0];

    assert_eq!(procedure.kind, ProcedureKind::Combinational);
    assert_eq!(procedure.sensitivity, Sensitivity::Implicit);
    assert!(
        cfg.blocks()
            .iter()
            .any(|block| matches!(block.terminator.kind, TerminatorKind::Branch { .. }))
    );
    assert!(
        cfg.blocks()
            .iter()
            .any(|block| matches!(block.terminator.kind, TerminatorKind::Switch { .. }))
    );
    let mut incoming = vec![0usize; cfg.blocks().len()];
    for edge in cfg.edges() {
        incoming[edge.target.index()] += 1;
    }
    assert!(incoming.into_iter().any(|count| count >= 2));
    assert_eq!(
        cfg.effects()
            .iter()
            .map(|effect| signal_value_name(rtl.word(), effect.value))
            .collect::<Vec<_>>(),
        ["a", "b", "c"]
    );
    assert!(cfg.effects().iter().all(|effect| {
        effect.mode == AssignmentMode::Blocking
            && matches!(
                effect.target,
                ProcTarget::Signal { signal, .. }
                    if rtl.word().resolve_name(rtl.word().signal(signal).unwrap().name.unwrap())
                        == Some("y")
            )
    }));
}

#[test]
fn verilog_frontend_lowers_bounded_runtime_condition_loops_to_acyclic_cfg() {
    let source = TestSource::new(
        "bounded-runtime-loops.sv",
        "module top(input logic [3:0] keep, output logic [2:0] while_count, do_count); always_comb begin integer i; integer j; i = 0; j = 0; while (i < 4 && keep[i]) i++; do j++; while (j < 4 && keep[j]); while_count = i; do_count = j; end endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let rtl = &update.modules[0];
    let cfg = rtl.procedures();

    assert!(
        cfg.blocks()
            .iter()
            .filter(|block| matches!(block.terminator.kind, TerminatorKind::Branch { .. }))
            .count()
            >= 7
    );
    let assigned_outputs = cfg
        .effects()
        .iter()
        .filter_map(|effect| match effect.target {
            ProcTarget::Signal { signal, .. } => rtl
                .word()
                .signal(signal)
                .and_then(|signal| signal.name)
                .and_then(|name| rtl.word().resolve_name(name)),
            ProcTarget::Memory { .. } => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert!(assigned_outputs.contains("while_count"));
    assert!(assigned_outputs.contains("do_count"));
}

#[test]
fn verilog_frontend_proves_signed_division_loop_progress() {
    let source = TestSource::new(
        "signed-division-loop.sv",
        "module top(output logic signed [7:0] result); always_comb begin int signed value; value = -16; while (value < -1) value = value / 32'sd2; result = value[7:0]; end endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let rtl = &update.modules[0];

    rtl.procedures().validate().unwrap();
    assert!(rtl.procedures().effects().iter().any(|effect| matches!(
        effect.target,
        ProcTarget::Signal { signal, .. }
            if rtl.word().resolve_name(rtl.word().signal(signal).unwrap().name.unwrap())
                == Some("result")
    )));
}

#[test]
fn verilog_frontend_rejects_loops_without_a_finite_rust_proof() {
    let cases = [
        (
            "runtime-while.sv",
            "module top(input logic enable, output logic y); always_comb while (enable) y = 1; endmodule\n",
        ),
        (
            "continue-without-progress.sv",
            "module top(input logic skip, output logic [3:0] y); always_comb begin integer i; i = 0; y = 0; while (i < 4) begin if (skip) continue; i++; y[i - 1] = 1; end end endmodule\n",
        ),
        (
            "repeating-state.sv",
            "module top(output logic y); always_comb begin integer i; i = 0; y = 0; while (i < 2) begin y = ~y; i ^= 1; end end endmodule\n",
        ),
        (
            "runtime-forever-break.sv",
            "module top(input logic stop, output logic y); always_comb begin y = 0; forever begin if (stop) break; y = 1; end end endmodule\n",
        ),
        (
            "nonexhaustive-forever-case.sv",
            "module top(input logic select, output logic y); always_comb forever begin y = 1; case (select) 1'b0: break; endcase end endmodule\n",
        ),
    ];

    for (name, text) in cases {
        let source = TestSource::new(name, text);
        let error = Frontend::read_verilog(
            std::slice::from_ref(&source.path),
            &FrontendOptions::default(),
            &opto_runtime::ExecutionContext::default(),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("cannot prove loop finite"),
            "{name}: {error}"
        );
    }
}

#[test]
fn verilog_frontend_does_not_treat_an_uninitialized_loop_local_as_known() {
    let source = TestSource::new(
        "uninitialized-loop-local.sv",
        "module top(output logic [3:0] y); always_comb begin integer i; y = 0; while (i < 4) begin i++; y = i[3:0]; end end endmodule\n",
    );
    let error = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap_err();

    assert!(
        error.to_string().contains("cannot prove loop finite"),
        "{error}"
    );
}

#[test]
fn verilog_frontend_proves_and_eliminates_cyclic_constant_repeat() {
    let source = TestSource::new(
        "cyclic-constant-repeat.sv",
        "module top(input logic [3:0] a, output logic [3:0] y); always_comb begin y = a; repeat (3) y = y + 1'b1; end endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let rtl = &update.modules[0];
    let y = rtl.word().signal_id("y").unwrap();

    assert_eq!(
        rtl.procedures()
            .effects()
            .iter()
            .filter(|effect| matches!(
                effect.target,
                ProcTarget::Signal { signal, .. } if signal == y
            ))
            .count(),
        5,
        "the acyclic graph retains one unreachable-by-value body clone for joint normalization"
    );
    assert!(rtl.word().signals().iter().all(|signal| {
        signal.name.is_none_or(|name| {
            rtl.word().resolve_name(name).is_none_or(|name| {
                !name.starts_with("__opto_repeat_")
                    || signal.kind == opto_ir::word::SignalKind::ProcessLocal
            })
        })
    }));
    rtl.procedures().validate().unwrap();
}

#[test]
fn verilog_frontend_preserves_repeat_return_and_disable_transfers() {
    let source = TestSource::new(
        "cyclic-repeat-transfers.sv",
        "module top(input logic stop_return, stop_scope, output logic [3:0] returned, scoped); function automatic logic [3:0] run(input logic stop); logic [3:0] value; value = 0; repeat (3) begin value++; if (stop) return value; end return value; endfunction always_comb returned = run(stop_return); always_comb begin scoped = 0; begin : scope repeat (3) begin scoped++; if (stop_scope) disable scope; scoped += 2; end scoped += 4; end scoped += 8; end endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let rtl = &update.modules[0];

    assert_eq!(rtl.procedures().procedures().len(), 2);
    assert!(rtl.word().signals().iter().all(|signal| {
        signal.name.is_none_or(|name| {
            rtl.word().resolve_name(name).is_none_or(|name| {
                (!name.starts_with("__opto_repeat_")
                    && !name.starts_with("__opto_disable_")
                    && !name.ends_with("_returned"))
                    || signal.kind == opto_ir::word::SignalKind::ProcessLocal
            })
        })
    }));
    rtl.procedures().validate().unwrap();
}

#[test]
fn verilog_frontend_eliminates_repeat_break_and_continue_edges() {
    let source = TestSource::new(
        "cyclic-repeat-loop-transfers.sv",
        "module top(input logic stop, skip, output logic [3:0] y); always_comb begin y = 0; repeat (3) begin y++; if (stop) break; if (skip) continue; y += 2; end end endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let rtl = &update.modules[0];

    assert!(rtl.word().signals().iter().all(|signal| {
        signal.name.is_none_or(|name| {
            rtl.word().resolve_name(name).is_none_or(|name| {
                (!name.starts_with("__opto_repeat_") && !name.starts_with("__opto_loop_"))
                    || signal.kind == opto_ir::word::SignalKind::ProcessLocal
            })
        })
    }));
    rtl.procedures().validate().unwrap();
}

#[test]
fn verilog_frontend_eliminates_nested_cyclic_repeat_regions() {
    let source = TestSource::new(
        "nested-cyclic-repeat.sv",
        "module top(output logic [3:0] y); always_comb begin y = 0; repeat (2) repeat (3) y++; end endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let rtl = &update.modules[0];

    assert!(rtl.word().signals().iter().all(|signal| {
        signal.name.is_none_or(|name| {
            rtl.word()
                .resolve_name(name)
                .is_none_or(|name| !name.starts_with("__opto_repeat_"))
        })
    }));
    rtl.procedures().validate().unwrap();
}

#[test]
fn verilog_frontend_lowers_bounded_forever_loops_to_acyclic_cfg() {
    let source = TestSource::new(
        "bounded-forever-loop.sv",
        "module top(input logic [3:0] stop, skip, output logic [3:0] mask, output logic [2:0] count); always_comb begin integer i; i = 0; mask = '0; forever begin if (stop[i] || i == 4) break; i++; if (skip[i - 1]) continue; mask[i - 1] = 1'b1; end count = i[2:0]; end endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let rtl = &update.modules[0];

    assert!(
        rtl.procedures()
            .blocks()
            .iter()
            .filter(|block| matches!(block.terminator.kind, TerminatorKind::Branch { .. }))
            .count()
            >= 8
    );
    assert!(rtl.word().signals().iter().all(|signal| {
        signal.name.is_none_or(|name| {
            rtl.word()
                .resolve_name(name)
                .is_none_or(|name| !name.ends_with("_broken") && !name.ends_with("_continued"))
        })
    }));
}

#[test]
fn verilog_frontend_lowers_scoped_disable_to_acyclic_cfg() {
    let source = TestSource::new(
        "scoped-disable.sv",
        "module top(input logic stop_inner, stop_outer, stop_loop, stop_task, output logic [7:0] block_value, loop_value, task_value); task automatic leave(output logic [7:0] value, input logic stop); value = 1; if (stop) disable leave; value = value + 2; endtask always_comb begin block_value = 0; begin : outer block_value = block_value + 1; begin : inner block_value = block_value + 2; if (stop_inner) disable inner; block_value = block_value + 4; if (stop_outer) disable outer; block_value = block_value + 8; end block_value = block_value + 16; end block_value = block_value + 32; end always_comb begin loop_value = 0; begin : loop_scope for (int i = 0; i < 4; i++) begin loop_value = loop_value + 1; if (stop_loop && i == 1) disable loop_scope; loop_value = loop_value + 2; end loop_value = loop_value + 16; end loop_value = loop_value + 32; end always_comb begin leave(task_value, stop_task); task_value = task_value + 4; end endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let rtl = &update.modules[0];

    assert_eq!(rtl.procedures().procedures().len(), 3);
    assert!(
        rtl.procedures()
            .blocks()
            .iter()
            .filter(|block| matches!(block.terminator.kind, TerminatorKind::Branch { .. }))
            .count()
            >= 8
    );
    assert_eq!(
        rtl.word()
            .signals()
            .iter()
            .filter(|signal| signal.name.is_some_and(|name| {
                rtl.word()
                    .resolve_name(name)
                    .is_some_and(|name| name.starts_with("__opto_disable_"))
            }))
            .count(),
        4,
        "lexical-disable flags remain typed process locals until joint normalization"
    );
}

#[test]
fn verilog_frontend_lowers_pattern_conditions_to_acyclic_cfg() {
    let source = TestSource::new(
        "pattern-conditions.sv",
        "module top(input logic [1:0] opcode, input logic [3:0] payload, output logic [3:0] y); typedef struct packed { logic [1:0] opcode; logic [3:0] payload; } packet_t; packet_t packet; always_comb begin packet = '{opcode: opcode, payload: payload}; if (packet matches '{opcode: 2'b01, payload: .captured} &&& captured[3]) y = captured; else y = 4'h0; end endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let rtl = &update.modules[0];

    assert!(
        rtl.procedures()
            .blocks()
            .iter()
            .any(|block| matches!(block.terminator.kind, TerminatorKind::Branch { .. }))
    );
    assert!(rtl.word().signals().iter().all(|signal| {
        signal.name.is_none_or(|name| {
            rtl.word().resolve_name(name).is_none_or(|name| {
                !name.starts_with("__opto_pattern_")
                    || signal.kind == opto_ir::word::SignalKind::ProcessLocal
            })
        })
    }));
}

#[test]
fn verilog_frontend_preserves_tagged_union_discriminants() {
    let source = TestSource::new(
        "tagged-union.sv",
        "module top(input logic [7:0] data, output logic [7:0] y); typedef union tagged { void Empty; logic [3:0] Small; logic [7:0] Large; } value_t; value_t value; always_comb begin value = tagged Large data; y = '0; if (value matches tagged Large .captured) y = captured; end endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let rtl = &update.modules[0];
    let value = rtl.word().signal_id("value").unwrap();

    assert_eq!(rtl.word().signal(value).unwrap().ty.width(), 10);
    assert!(
        rtl.procedures()
            .blocks()
            .iter()
            .any(|block| matches!(block.terminator.kind, TerminatorKind::Branch { .. }))
    );
}

#[test]
fn verilog_frontend_converts_untyped_parameter_slices_to_lvalue_type() {
    let source = TestSource::new(
        "assignment-signedness.sv",
        "module top(output logic [2:0] value); localparam MaxNumWords = $clog2(64 / 8); always_comb value = MaxNumWords[2:0]; endmodule\n",
    );

    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();

    assert_eq!(update.modules[0].procedures().procedures().len(), 1);
}

#[test]
fn verilog_frontend_lowers_always_ff_clock_enable() {
    let source = TestSource::new(
        "always-ff.sv",
        "module top(input logic clk, en, d, output logic q); always_ff @(negedge clk) begin if (en) q <= d; end endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let rtl = &update.modules[0];
    let cfg = rtl.procedures();
    let process = &cfg.procedures()[0];
    let events = cfg.sensitivity_events(ProcedureId::FIRST).unwrap();

    assert_eq!(process.kind, ProcedureKind::FlipFlop);
    assert_eq!(events.len(), 1);
    for (_, event) in events {
        assert_eq!(event.edge, Edge::Neg);
        assert_eq!(
            rtl.word()
                .resolve_name(rtl.word().signal(event.signal).unwrap().name.unwrap()),
            Some("clk")
        );
    }
    assert!(
        cfg.blocks()
            .iter()
            .any(|block| matches!(block.terminator.kind, TerminatorKind::Branch { .. }))
    );
    assert!(
        cfg.effects()
            .iter()
            .all(|effect| effect.mode == AssignmentMode::Nonblocking)
    );
}

#[test]
fn verilog_frontend_lowers_single_event_iff_as_register_enable() {
    let source = TestSource::new(
        "event-iff.sv",
        "module top(input logic clk, en, d, output logic q); always_ff @(posedge clk iff en) q <= d; endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let rtl = &update.modules[0];
    let cfg = rtl.procedures();
    let process = &cfg.procedures()[0];
    let events = cfg.sensitivity_events(ProcedureId::FIRST).unwrap();

    assert_eq!(process.kind, ProcedureKind::FlipFlop);
    assert_eq!(events.len(), 1);
    assert!(
        cfg.blocks()
            .iter()
            .any(|block| matches!(block.terminator.kind, TerminatorKind::Branch { .. }))
    );
}

#[test]
fn verilog_frontend_preserves_always_latch_process_kind() {
    let source = TestSource::new(
        "always-latch.sv",
        "module top(input logic en, d, output logic q); always_latch begin if (en) q <= d; end endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let cfg = update.modules[0].procedures();
    let process = &cfg.procedures()[0];

    assert_eq!(process.kind, ProcedureKind::Latch);
    assert_eq!(process.sensitivity, Sensitivity::Implicit);
    assert!(
        cfg.blocks()
            .iter()
            .any(|block| matches!(block.terminator.kind, TerminatorKind::Branch { .. }))
    );
}

#[test]
fn verilog_frontend_defers_classic_always_latch_inference() {
    let source = TestSource::new(
        "classic-latch.v",
        "module top(input wire en, d, output reg q); always @* if (en) q = d; endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();

    assert_eq!(
        update.modules[0].procedures().procedures()[0].kind,
        ProcedureKind::CombinationalOrLatch
    );
}

#[test]
fn verilog_frontend_lowers_classic_verilog_always() {
    let source = TestSource::new(
        "classic-always.v",
        "module top(input wire clk, input wire d, input wire a, output reg q, y); always @(posedge clk) q <= d; always @* y = a; endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let processes = update.modules[0].procedures().procedures();

    assert_eq!(processes[0].kind, ProcedureKind::FlipFlop);
    assert_eq!(processes[1].kind, ProcedureKind::CombinationalOrLatch);
}

#[test]
fn verilog_frontend_retains_unresolved_instances_for_hierarchy_resolution() {
    let source = TestSource::new(
        "unresolved-instance.sv",
        "module top(input logic a, output logic y); INVX1 u_inv(.A(a), .Y(y)); endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions::default(),
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let module = update.modules[0].word();
    let instance = &module.instances()[0];

    assert_eq!(module.resolve_name(instance.name), Some("u_inv"));
    assert_eq!(module.resolve_name(instance.module), Some("INVX1"));
    assert_eq!(module.resolve_name(instance.connections[0].port), Some("A"));
    assert_eq!(module.resolve_name(instance.connections[1].port), Some("Y"));
}

#[test]
fn top_option_selects_requested_module() {
    let source = TestSource::new(
        "top-option.sv",
        "module unused; endmodule\nmodule chosen; endmodule\n",
    );
    let update = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions {
            top: Some("chosen".to_string()),
            ..FrontendOptions::default()
        },
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();

    assert_eq!(update.top.as_deref(), Some("chosen"));
}

#[test]
fn analysis_owns_primary_and_include_source_snapshots() {
    let dir = TestDirectory::new("analysis-snapshot");
    let include = dir.join("width.svh");
    let source = dir.join("top.sv");
    std::fs::write(&include, "`define WIDTH 4\n").unwrap();
    std::fs::write(
        &source,
        "`include \"width.svh\"\nmodule top(input logic [`WIDTH-1:0] a, output logic [`WIDTH-1:0] y); assign y = a; endmodule\n",
    )
    .unwrap();

    let analysis = Frontend::ingest_verilog(
        std::slice::from_ref(&source),
        &FrontendOptions {
            include_paths: vec![dir.clone()],
            ..FrontendOptions::default()
        },
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    std::fs::remove_file(source).unwrap();
    std::fs::remove_file(include).unwrap();

    let update = Frontend::elaborate_verilog(
        &[analysis],
        "top",
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    assert_eq!(update.modules[0].word().ports()[0].ty.width(), 4);
    assert_eq!(update.modules[0].word().ports()[1].ty.width(), 4);
}

#[test]
fn primary_files_are_independent_source_units_in_serial_and_parallel() {
    let first = TestSource::new(
        "source-unit-first.sv",
        "`define PRIVATE_WIDTH 2\n\
         module width_two(input logic [`PRIVATE_WIDTH-1:0] a, \
         output logic [`PRIVATE_WIDTH-1:0] y); assign y = a; endmodule\n",
    );
    let second = TestSource::new(
        "source-unit-second.sv",
        "`ifdef PRIVATE_WIDTH\n`define SECOND_WIDTH 1\n`else\n`define SECOND_WIDTH 3\n`endif\n\
         module width_three(input logic [`SECOND_WIDTH-1:0] a, \
         output logic [`SECOND_WIDTH-1:0] y); assign y = a; endmodule\n",
    );
    let top = TestSource::new(
        "source-unit-top.sv",
        "module top(input logic [1:0] a, input logic [2:0] b, \
         output logic [1:0] y, output logic [2:0] z); \
         width_two u_two(.a(a), .y(y)); width_three u_three(.a(b), .y(z)); endmodule\n",
    );
    let files = vec![first.path.clone(), second.path.clone(), top.path.clone()];
    let options = FrontendOptions {
        top: Some("top".to_string()),
        ..FrontendOptions::default()
    };
    let run_analysis = |threads| {
        let runtime = opto_runtime::ExecutionContext::new(&opto_runtime::ExecutionConfig {
            max_threads: threads,
        })
        .unwrap();
        Frontend::read_verilog(&files, &options, &runtime)
            .unwrap()
            .modules
            .into_iter()
            .map(|module| {
                (
                    module.word().name().to_string(),
                    module
                        .word()
                        .ports()
                        .iter()
                        .map(|port| port.ty.width())
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>()
    };

    let serial = run_analysis(1);
    let parallel = run_analysis(4);
    assert_eq!(parallel, serial);
    assert!(serial.contains(&("width_two".to_string(), vec![2, 2])));
    assert!(serial.contains(&("width_three".to_string(), vec![3, 3])));
}

#[test]
fn missing_top_is_an_error() {
    let source = TestSource::new("missing-top.sv", "module actual; endmodule\n");
    let err = Frontend::read_verilog(
        std::slice::from_ref(&source.path),
        &FrontendOptions {
            top: Some("missing".to_string()),
            ..FrontendOptions::default()
        },
        &opto_runtime::ExecutionContext::default(),
    )
    .unwrap_err();

    assert!(err.to_string().contains("missing"));
}

fn signal_value_name(module: &opto_ir::word::WordModule, value: opto_ir::word::ValueId) -> &str {
    let ValueKind::Signal(signal) = module.value(value).unwrap().kind else {
        panic!("expected a signal value")
    };
    module
        .resolve_name(module.signal(signal.signal).unwrap().name.unwrap())
        .unwrap()
}

fn value_depends_on_signal_named(
    module: &opto_ir::word::WordModule,
    value: opto_ir::word::ValueId,
    expected: &str,
) -> bool {
    match module.value(value).unwrap().kind {
        ValueKind::Signal(_) => signal_value_name(module, value) == expected,
        ValueKind::Constant(_) => false,
        ValueKind::Operation(operation) => {
            let mut found = false;
            module
                .operation(operation)
                .unwrap()
                .kind
                .for_each_input(|input| {
                    found |= value_depends_on_signal_named(module, input, expected);
                });
            found
        }
    }
}

struct TestSource {
    path: PathBuf,
}

static TEST_SOURCE_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[derive(Debug)]
struct TestDirectory {
    path: PathBuf,
}

impl TestDirectory {
    fn new(name: &str) -> Self {
        let sequence = TEST_SOURCE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "{}-{}-{sequence}-{name}",
            env!("CARGO_PKG_NAME"),
            std::process::id(),
        ));
        std::fs::create_dir(&path).unwrap();
        Self { path }
    }
}

impl std::ops::Deref for TestDirectory {
    type Target = PathBuf;

    fn deref(&self) -> &Self::Target {
        &self.path
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

impl TestSource {
    fn new(name: &str, text: &str) -> Self {
        let sequence = TEST_SOURCE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "{}-{}-{sequence}-{name}",
            env!("CARGO_PKG_NAME"),
            std::process::id()
        ));
        std::fs::write(&path, text).unwrap();
        Self { path }
    }
}

impl Drop for TestSource {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
