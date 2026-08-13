// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use opto_ir::proc::{
    AssignmentMode, ProcTarget, ProcedureId, ProcedureKind, Sensitivity, TerminatorKind,
};
use opto_ir::word::{BinaryOp, Edge, OpKind, SignalResolution, TypeSelector, ValueKind};

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
    assert_eq!(rtl.procedures().effects().len(), 4);
    assert!(rtl.procedures().effects().iter().all(|effect| matches!(
        effect.target,
        ProcTarget::Memory {
            memory: target,
            ..
        } if target == memory
    )));
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
