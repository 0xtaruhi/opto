// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use std::ffi::{c_char, c_int};

pub(crate) const OK: c_int = 0;
pub(crate) const LANGUAGE_VERILOG_2005: c_int = 0;
pub(crate) const LANGUAGE_SYSTEM_VERILOG_2017: c_int = 1;

pub(crate) const PORT_INPUT: c_int = 0;
pub(crate) const PORT_OUTPUT: c_int = 1;
pub(crate) const PORT_INOUT: c_int = 2;

pub(crate) const TYPE_SCALAR: c_int = 0;
pub(crate) const TYPE_ARRAY: c_int = 1;
pub(crate) const TYPE_STRUCT: c_int = 2;

pub(crate) const ATTRIBUTE_INTEGER: c_int = 0;
pub(crate) const ATTRIBUTE_STRING: c_int = 1;
pub(crate) const ATTRIBUTE_OTHER: c_int = 2;

pub(crate) const EXPR_SIGNAL: c_int = 0;
pub(crate) const EXPR_CONSTANT: c_int = 1;
pub(crate) const EXPR_UNARY: c_int = 2;
pub(crate) const EXPR_BINARY: c_int = 3;
pub(crate) const EXPR_CONCAT: c_int = 4;
pub(crate) const EXPR_MUX: c_int = 5;
pub(crate) const EXPR_CAST: c_int = 6;
pub(crate) const EXPR_EXTRACT: c_int = 7;
pub(crate) const EXPR_DYNAMIC_EXTRACT: c_int = 8;

pub(crate) const CAST_ZERO_EXTEND: c_int = 0;
pub(crate) const CAST_SIGN_EXTEND: c_int = 1;
pub(crate) const CAST_TRUNCATE: c_int = 2;

pub(crate) const UNARY_LOGICAL_NOT: c_int = 0;
pub(crate) const UNARY_BIT_NOT: c_int = 1;
pub(crate) const UNARY_REDUCTION_AND: c_int = 2;
pub(crate) const UNARY_REDUCTION_OR: c_int = 3;
pub(crate) const UNARY_REDUCTION_XOR: c_int = 4;

pub(crate) const BINARY_ADD: c_int = 0;
pub(crate) const BINARY_SUB: c_int = 1;
pub(crate) const BINARY_MUL: c_int = 2;
pub(crate) const BINARY_BIT_AND: c_int = 3;
pub(crate) const BINARY_BIT_OR: c_int = 4;
pub(crate) const BINARY_BIT_XOR: c_int = 5;
pub(crate) const BINARY_LOGICAL_AND: c_int = 6;
pub(crate) const BINARY_LOGICAL_OR: c_int = 7;
pub(crate) const BINARY_EQ: c_int = 8;
pub(crate) const BINARY_NE: c_int = 9;
pub(crate) const BINARY_LT: c_int = 10;
pub(crate) const BINARY_LE: c_int = 11;
pub(crate) const BINARY_GT: c_int = 12;
pub(crate) const BINARY_GE: c_int = 13;
pub(crate) const BINARY_SHL: c_int = 14;
pub(crate) const BINARY_SHR: c_int = 15;
pub(crate) const BINARY_ASHR: c_int = 16;
pub(crate) const BINARY_DIV: c_int = 17;
pub(crate) const BINARY_MOD: c_int = 18;

pub(crate) const PROCEDURE_COMB: c_int = 0;
pub(crate) const PROCEDURE_LATCH: c_int = 1;
pub(crate) const PROCEDURE_FLOP: c_int = 2;
pub(crate) const PROCEDURE_COMB_OR_LATCH: c_int = 3;

pub(crate) const EDGE_POS: c_int = 0;
pub(crate) const EDGE_NEG: c_int = 1;

pub(crate) const TERMINATOR_RETURN: c_int = 0;
pub(crate) const TERMINATOR_JUMP: c_int = 1;
pub(crate) const TERMINATOR_BRANCH: c_int = 2;
pub(crate) const TERMINATOR_SWITCH: c_int = 3;

macro_rules! opaque {
    ($($name:ident),+ $(,)?) => {$(
        #[repr(C)]
        pub(crate) struct $name {
            _private: [u8; 0],
        }
    )+};
}

opaque!(
    Compiler, Analysis, Snapshot, Expr, TypeLayout, Port, Net, Instance, Procedure
);

#[repr(C)]
#[derive(Debug, Clone, Copy)]
#[allow(
    clippy::struct_field_names,
    reason = "field names mirror the audited C ABI and distinguish independent arena counts"
)]
pub(crate) struct AnalysisView {
    pub(crate) definition_count: usize,
    pub(crate) package_count: usize,
    pub(crate) dependency_count: usize,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct SourceFileView {
    pub(crate) path: *const c_char,
    pub(crate) text: *const c_char,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct SnapshotView {
    pub(crate) top: *const c_char,
    pub(crate) module_count: usize,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct ModuleInfoView {
    pub(crate) name: *const c_char,
    pub(crate) source_order: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
#[allow(
    clippy::struct_field_names,
    reason = "field names mirror the audited C ABI and distinguish independent module arenas"
)]
pub(crate) struct ModuleView {
    pub(crate) attribute_count: usize,
    pub(crate) port_count: usize,
    pub(crate) net_count: usize,
    pub(crate) instance_count: usize,
    pub(crate) assign_count: usize,
    pub(crate) procedure_count: usize,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct AttributeView {
    pub(crate) name: *const c_char,
    pub(crate) kind: c_int,
    pub(crate) value: *const c_char,
    pub(crate) integer_width: u32,
    pub(crate) integer_signed: c_int,
    pub(crate) is_true: c_int,
    pub(crate) source: SourceSpanView,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct PortView {
    pub(crate) identity: *const Port,
    pub(crate) name: *const c_char,
    pub(crate) direction: c_int,
    pub(crate) width: u32,
    pub(crate) is_signed: c_int,
    pub(crate) resolution: c_int,
    pub(crate) type_layout: *const TypeLayout,
    pub(crate) attribute_count: usize,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct NetView {
    pub(crate) identity: *const Net,
    pub(crate) name: *const c_char,
    pub(crate) width: u32,
    pub(crate) is_signed: c_int,
    pub(crate) element_is_signed: c_int,
    pub(crate) is_process_local: c_int,
    pub(crate) resolution: c_int,
    pub(crate) type_layout: *const TypeLayout,
    pub(crate) attribute_count: usize,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct InstanceView {
    pub(crate) identity: *const Instance,
    pub(crate) name: *const c_char,
    pub(crate) module_name: *const c_char,
    pub(crate) connection_count: usize,
    pub(crate) attribute_count: usize,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct ConnectionView {
    pub(crate) port: *const c_char,
    pub(crate) expression: *const Expr,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct AssignView {
    pub(crate) lhs: *const Expr,
    pub(crate) rhs: *const Expr,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct ProcedureView {
    pub(crate) identity: *const Procedure,
    pub(crate) kind: c_int,
    pub(crate) event_count: usize,
    pub(crate) block_count: usize,
    pub(crate) entry_block: u32,
    pub(crate) source: SourceSpanView,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct EventView {
    pub(crate) edge: c_int,
    pub(crate) signal: *const Expr,
    pub(crate) source: SourceSpanView,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct SourceSpanView {
    pub(crate) file: *const c_char,
    pub(crate) line: u32,
    pub(crate) column: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct EdgeTargetView {
    pub(crate) block: u32,
    pub(crate) source: SourceSpanView,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct BlockView {
    pub(crate) effect_count: usize,
    pub(crate) terminator_kind: c_int,
    pub(crate) source: SourceSpanView,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct EffectView {
    pub(crate) lhs: *const Expr,
    pub(crate) rhs: *const Expr,
    pub(crate) blocking: c_int,
    pub(crate) source: SourceSpanView,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct TerminatorView {
    pub(crate) kind: c_int,
    pub(crate) condition: *const Expr,
    pub(crate) selector: *const Expr,
    pub(crate) jump_edge: EdgeTargetView,
    pub(crate) then_edge: EdgeTargetView,
    pub(crate) else_edge: EdgeTargetView,
    pub(crate) default_edge: EdgeTargetView,
    pub(crate) arm_count: usize,
    pub(crate) source: SourceSpanView,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct SwitchArmView {
    pub(crate) pattern: *const Expr,
    pub(crate) edge: EdgeTargetView,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct TypeLayoutView {
    pub(crate) kind: c_int,
    pub(crate) width: u32,
    pub(crate) array_left: i32,
    pub(crate) array_right: i32,
    pub(crate) array_is_packed: c_int,
    pub(crate) array_element: *const TypeLayout,
    pub(crate) field_count: usize,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct TypeFieldView {
    pub(crate) name: *const c_char,
    pub(crate) bit_offset: u32,
    pub(crate) layout: *const TypeLayout,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct ExprView {
    pub(crate) kind: c_int,
    pub(crate) source_file: *const c_char,
    pub(crate) source_line: u32,
    pub(crate) source_column: u32,
    pub(crate) signal_name: *const c_char,
    pub(crate) signal_has_range: c_int,
    pub(crate) signal_msb: u32,
    pub(crate) signal_lsb: u32,
    pub(crate) constant_has_width: c_int,
    pub(crate) constant_width: u32,
    pub(crate) constant_signed: c_int,
    pub(crate) constant_bits: *const c_char,
    pub(crate) unary_op: c_int,
    pub(crate) unary_arg: *const Expr,
    pub(crate) binary_op: c_int,
    pub(crate) binary_left: *const Expr,
    pub(crate) binary_right: *const Expr,
    pub(crate) concat_parts: *const *const Expr,
    pub(crate) concat_count: usize,
    pub(crate) mux_condition: *const Expr,
    pub(crate) mux_then: *const Expr,
    pub(crate) mux_else: *const Expr,
    pub(crate) cast_kind: c_int,
    pub(crate) cast_value: *const Expr,
    pub(crate) cast_width: u32,
    pub(crate) cast_signed: c_int,
    pub(crate) extract_value: *const Expr,
    pub(crate) extract_lsb: u32,
    pub(crate) extract_width: u32,
    pub(crate) dynamic_extract_value: *const Expr,
    pub(crate) dynamic_extract_offset: *const Expr,
    pub(crate) dynamic_extract_width: u32,
}

unsafe extern "C" {
    pub(crate) fn opto_slang_compiler_new() -> *mut Compiler;
    pub(crate) fn opto_slang_compiler_free(compiler: *mut Compiler);
    pub(crate) fn opto_slang_compiler_begin_source_unit(compiler: *mut Compiler) -> c_int;
    pub(crate) fn opto_slang_compiler_add_source_file(
        compiler: *mut Compiler,
        path: *const c_char,
        text: *const c_char,
    ) -> c_int;
    pub(crate) fn opto_slang_compiler_add_source_path(
        compiler: *mut Compiler,
        path: *const c_char,
    ) -> c_int;
    pub(crate) fn opto_slang_compiler_add_source_dependency(
        compiler: *mut Compiler,
        path: *const c_char,
        text: *const c_char,
    ) -> c_int;
    pub(crate) fn opto_slang_compiler_add_include_dir(
        compiler: *mut Compiler,
        path: *const c_char,
    ) -> c_int;
    pub(crate) fn opto_slang_compiler_add_define(
        compiler: *mut Compiler,
        name: *const c_char,
        value: *const c_char,
    ) -> c_int;
    pub(crate) fn opto_slang_compiler_set_top(compiler: *mut Compiler, top: *const c_char)
    -> c_int;
    pub(crate) fn opto_slang_compiler_set_language(
        compiler: *mut Compiler,
        language: c_int,
    ) -> c_int;
    pub(crate) fn opto_slang_compiler_set_max_threads(
        compiler: *mut Compiler,
        max_threads: u32,
    ) -> c_int;
    pub(crate) fn opto_slang_compiler_compile(
        compiler: *mut Compiler,
        design: *mut *mut Snapshot,
    ) -> c_int;
    pub(crate) fn opto_slang_compiler_analyze(
        compiler: *mut Compiler,
        analysis: *mut *mut Analysis,
    ) -> c_int;
    pub(crate) fn opto_slang_compiler_last_error(compiler: *const Compiler) -> *const c_char;

    pub(crate) fn opto_slang_analysis_free(analysis: *mut Analysis);
    pub(crate) fn opto_slang_analysis_view(
        analysis: *const Analysis,
        view: *mut AnalysisView,
    ) -> c_int;
    pub(crate) fn opto_slang_analysis_definition_name(
        analysis: *const Analysis,
        index: usize,
    ) -> *const c_char;
    pub(crate) fn opto_slang_analysis_package_name(
        analysis: *const Analysis,
        index: usize,
    ) -> *const c_char;
    pub(crate) fn opto_slang_analysis_dependency_view(
        analysis: *const Analysis,
        index: usize,
        view: *mut SourceFileView,
    ) -> c_int;

    pub(crate) fn opto_slang_snapshot_free(design: *mut Snapshot);
    pub(crate) fn opto_slang_snapshot_view(
        design: *const Snapshot,
        view: *mut SnapshotView,
    ) -> c_int;
    pub(crate) fn opto_slang_module_info(
        design: *const Snapshot,
        module_index: usize,
        view: *mut ModuleInfoView,
    ) -> c_int;
    pub(crate) fn opto_slang_module_view(
        design: *const Snapshot,
        module_index: usize,
        view: *mut ModuleView,
    ) -> c_int;
    pub(crate) fn opto_slang_module_materialize(
        design: *mut Snapshot,
        module_index: usize,
    ) -> c_int;
    pub(crate) fn opto_slang_module_materialize_error(
        design: *const Snapshot,
        module_index: usize,
    ) -> *const c_char;
    pub(crate) fn opto_slang_module_release(design: *mut Snapshot, module_index: usize);
    pub(crate) fn opto_slang_module_attribute_view(
        design: *const Snapshot,
        module_index: usize,
        attribute_index: usize,
        view: *mut AttributeView,
    ) -> c_int;

    pub(crate) fn opto_slang_port_view(
        design: *const Snapshot,
        module_index: usize,
        port_index: usize,
        view: *mut PortView,
    ) -> c_int;
    pub(crate) fn opto_slang_port_attribute_view(
        port: *const Port,
        attribute_index: usize,
        view: *mut AttributeView,
    ) -> c_int;
    pub(crate) fn opto_slang_net_view(
        design: *const Snapshot,
        module_index: usize,
        net_index: usize,
        view: *mut NetView,
    ) -> c_int;
    pub(crate) fn opto_slang_net_attribute_view(
        net: *const Net,
        attribute_index: usize,
        view: *mut AttributeView,
    ) -> c_int;
    pub(crate) fn opto_slang_instance_view(
        design: *const Snapshot,
        module_index: usize,
        instance_index: usize,
        view: *mut InstanceView,
    ) -> c_int;
    pub(crate) fn opto_slang_instance_attribute_view(
        instance: *const Instance,
        attribute_index: usize,
        view: *mut AttributeView,
    ) -> c_int;
    pub(crate) fn opto_slang_connection_view(
        instance: *const Instance,
        connection_index: usize,
        view: *mut ConnectionView,
    ) -> c_int;
    pub(crate) fn opto_slang_assign_view(
        design: *const Snapshot,
        module_index: usize,
        assign_index: usize,
        view: *mut AssignView,
    ) -> c_int;
    pub(crate) fn opto_slang_procedure_view(
        design: *const Snapshot,
        module_index: usize,
        procedure_index: usize,
        view: *mut ProcedureView,
    ) -> c_int;
    pub(crate) fn opto_slang_event_view(
        procedure: *const Procedure,
        event_index: usize,
        view: *mut EventView,
    ) -> c_int;
    pub(crate) fn opto_slang_block_view(
        procedure: *const Procedure,
        block_index: usize,
        view: *mut BlockView,
    ) -> c_int;
    pub(crate) fn opto_slang_effect_view(
        procedure: *const Procedure,
        block_index: usize,
        effect_index: usize,
        view: *mut EffectView,
    ) -> c_int;
    pub(crate) fn opto_slang_terminator_view(
        procedure: *const Procedure,
        block_index: usize,
        view: *mut TerminatorView,
    ) -> c_int;
    pub(crate) fn opto_slang_switch_arm_view(
        procedure: *const Procedure,
        block_index: usize,
        arm_index: usize,
        view: *mut SwitchArmView,
    ) -> c_int;
    pub(crate) fn opto_slang_type_layout_view(
        layout: *const TypeLayout,
        view: *mut TypeLayoutView,
    ) -> c_int;
    pub(crate) fn opto_slang_type_field_view(
        layout: *const TypeLayout,
        field_index: usize,
        view: *mut TypeFieldView,
    ) -> c_int;
    pub(crate) fn opto_slang_expr_view(expression: *const Expr, view: *mut ExprView) -> c_int;
}
