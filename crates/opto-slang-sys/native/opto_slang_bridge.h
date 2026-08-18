// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

#ifndef OPTO_SLANG_BRIDGE_H
#define OPTO_SLANG_BRIDGE_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct OptoSlangCompiler OptoSlangCompiler;
typedef struct OptoSlangAnalysis OptoSlangAnalysis;
typedef struct OptoSlangSnapshot OptoSlangSnapshot;
typedef struct OptoSlangExpr OptoSlangExpr;
typedef struct OptoSlangTypeLayout OptoSlangTypeLayout;
typedef struct OptoSlangTypeLayoutField OptoSlangTypeLayoutField;
typedef struct OptoSlangPortData OptoSlangPortData;
typedef struct OptoSlangNetData OptoSlangNetData;
typedef struct OptoSlangConnectionData OptoSlangConnectionData;
typedef struct OptoSlangInstanceData OptoSlangInstanceData;
typedef struct OptoSlangAssignData OptoSlangAssignData;
typedef struct OptoSlangEventData OptoSlangEventData;
typedef struct OptoSlangProcedureData OptoSlangProcedureData;

typedef enum OptoSlangStatus { OPTO_SLANG_OK = 0, OPTO_SLANG_ERROR = 1 } OptoSlangStatus;

typedef enum OptoSlangDiagnosticSeverity {
    OPTO_SLANG_DIAGNOSTIC_NOTE = 0,
    OPTO_SLANG_DIAGNOSTIC_WARNING = 1,
    OPTO_SLANG_DIAGNOSTIC_ERROR = 2
} OptoSlangDiagnosticSeverity;

typedef enum OptoSlangLoweringFailureCategory {
    OPTO_SLANG_LOWERING_UNSUPPORTED_PROFILE = 0,
    OPTO_SLANG_LOWERING_INVALID_PROJECTION = 1,
    OPTO_SLANG_LOWERING_CAPACITY = 2,
    OPTO_SLANG_LOWERING_INVARIANT = 3,
    OPTO_SLANG_LOWERING_NATIVE = 4
} OptoSlangLoweringFailureCategory;

typedef enum OptoSlangPortDirection {
    OPTO_SLANG_PORT_INPUT = 0,
    OPTO_SLANG_PORT_OUTPUT = 1,
    OPTO_SLANG_PORT_INOUT = 2,
    OPTO_SLANG_PORT_REF = 3
} OptoSlangPortDirection;

typedef enum OptoSlangNetResolution {
    OPTO_SLANG_NET_SINGLE_DRIVER = 0,
    OPTO_SLANG_NET_WIRED_AND = 1,
    OPTO_SLANG_NET_WIRED_OR = 2
} OptoSlangNetResolution;

typedef enum OptoSlangTypeLayoutKind {
    OPTO_SLANG_TYPE_SCALAR = 0,
    OPTO_SLANG_TYPE_ARRAY = 1,
    OPTO_SLANG_TYPE_STRUCT = 2
} OptoSlangTypeLayoutKind;

typedef enum OptoSlangAttributeValueKind {
    OPTO_SLANG_ATTRIBUTE_INTEGER = 0,
    OPTO_SLANG_ATTRIBUTE_STRING = 1,
    OPTO_SLANG_ATTRIBUTE_OTHER = 2
} OptoSlangAttributeValueKind;

typedef enum OptoSlangExprKind {
    OPTO_SLANG_EXPR_SIGNAL = 0,
    OPTO_SLANG_EXPR_CONSTANT = 1,
    OPTO_SLANG_EXPR_UNARY = 2,
    OPTO_SLANG_EXPR_BINARY = 3,
    OPTO_SLANG_EXPR_CONCAT = 4,
    OPTO_SLANG_EXPR_MUX = 5,
    OPTO_SLANG_EXPR_CAST = 6,
    OPTO_SLANG_EXPR_EXTRACT = 7,
    OPTO_SLANG_EXPR_DYNAMIC_EXTRACT = 8
} OptoSlangExprKind;

typedef enum OptoSlangCastKind {
    OPTO_SLANG_CAST_ZERO_EXTEND = 0,
    OPTO_SLANG_CAST_SIGN_EXTEND = 1,
    OPTO_SLANG_CAST_TRUNCATE = 2
} OptoSlangCastKind;

typedef enum OptoSlangUnaryOp {
    OPTO_SLANG_UNARY_LOGICAL_NOT = 0,
    OPTO_SLANG_UNARY_BIT_NOT = 1,
    OPTO_SLANG_UNARY_REDUCTION_AND = 2,
    OPTO_SLANG_UNARY_REDUCTION_OR = 3,
    OPTO_SLANG_UNARY_REDUCTION_XOR = 4
} OptoSlangUnaryOp;

typedef enum OptoSlangBinaryOp {
    OPTO_SLANG_BINARY_ADD = 0,
    OPTO_SLANG_BINARY_SUB = 1,
    OPTO_SLANG_BINARY_MUL = 2,
    OPTO_SLANG_BINARY_BIT_AND = 3,
    OPTO_SLANG_BINARY_BIT_OR = 4,
    OPTO_SLANG_BINARY_BIT_XOR = 5,
    OPTO_SLANG_BINARY_LOGICAL_AND = 6,
    OPTO_SLANG_BINARY_LOGICAL_OR = 7,
    OPTO_SLANG_BINARY_EQ = 8,
    OPTO_SLANG_BINARY_NE = 9,
    OPTO_SLANG_BINARY_LT = 10,
    OPTO_SLANG_BINARY_LE = 11,
    OPTO_SLANG_BINARY_GT = 12,
    OPTO_SLANG_BINARY_GE = 13,
    OPTO_SLANG_BINARY_SHL = 14,
    OPTO_SLANG_BINARY_SHR = 15,
    OPTO_SLANG_BINARY_ASHR = 16,
    OPTO_SLANG_BINARY_DIV = 17,
    OPTO_SLANG_BINARY_MOD = 18
} OptoSlangBinaryOp;

typedef enum OptoSlangProcedureKind {
    OPTO_SLANG_PROCEDURE_COMB = 0,
    OPTO_SLANG_PROCEDURE_LATCH = 1,
    OPTO_SLANG_PROCEDURE_FLOP = 2,
    OPTO_SLANG_PROCEDURE_COMB_OR_LATCH = 3
} OptoSlangProcedureKind;

typedef enum OptoSlangLoopForm {
    OPTO_SLANG_LOOP_PRE_TEST = 0,
    OPTO_SLANG_LOOP_POST_TEST = 1,
    OPTO_SLANG_LOOP_UNCONDITIONAL = 2
} OptoSlangLoopForm;

typedef enum OptoSlangEdge { OPTO_SLANG_EDGE_POS = 0, OPTO_SLANG_EDGE_NEG = 1 } OptoSlangEdge;

typedef enum OptoSlangTerminatorKind {
    OPTO_SLANG_TERMINATOR_RETURN = 0,
    OPTO_SLANG_TERMINATOR_JUMP = 1,
    OPTO_SLANG_TERMINATOR_BRANCH = 2,
    OPTO_SLANG_TERMINATOR_SWITCH = 3
} OptoSlangTerminatorKind;

typedef struct OptoSlangSourceSpanView {
    const char *file;
    uint32_t line;
    uint32_t column;
} OptoSlangSourceSpanView;

typedef struct OptoSlangLoweringFailureView {
    OptoSlangLoweringFailureCategory category;
    uint16_t code;
    const char *message;
    OptoSlangSourceSpanView source;
} OptoSlangLoweringFailureView;

typedef struct OptoSlangEdgeTargetView {
    uint32_t block;
    OptoSlangSourceSpanView source;
} OptoSlangEdgeTargetView;

typedef struct OptoSlangAnalysisView {
    size_t definition_count;
    size_t package_count;
    size_t dependency_count;
} OptoSlangAnalysisView;

typedef struct OptoSlangSourceFileView {
    const char *path;
    const char *text;
} OptoSlangSourceFileView;

typedef struct OptoSlangDiagnosticView {
    OptoSlangDiagnosticSeverity severity;
    uint16_t subsystem;
    uint16_t code;
    const char *message;
    const char *option_name;
    const char *file;
    uint32_t line;
    uint32_t column;
    uint32_t length;
} OptoSlangDiagnosticView;

typedef struct OptoSlangSnapshotView {
    const char *top;
    size_t module_count;
} OptoSlangSnapshotView;

typedef struct OptoSlangModuleInfoView {
    const char *name;
    uint64_t source_order;
} OptoSlangModuleInfoView;

typedef struct OptoSlangModuleView {
    size_t attribute_count;
    size_t port_count;
    size_t net_count;
    size_t instance_count;
    size_t assign_count;
    size_t procedure_count;
} OptoSlangModuleView;

typedef struct OptoSlangAttributeView {
    const char *name;
    OptoSlangAttributeValueKind kind;
    const char *value;
    uint32_t integer_width;
    int integer_signed;
    int is_true;
    OptoSlangSourceSpanView source;
} OptoSlangAttributeView;

typedef struct OptoSlangPortView {
    const OptoSlangPortData *identity;
    const char *name;
    OptoSlangPortDirection direction;
    uint32_t width;
    int is_signed;
    OptoSlangNetResolution resolution;
    const OptoSlangTypeLayout *type_layout;
    size_t attribute_count;
} OptoSlangPortView;

typedef struct OptoSlangNetView {
    const OptoSlangNetData *identity;
    const char *name;
    uint32_t width;
    int is_signed;
    int element_is_signed;
    int is_process_local;
    OptoSlangNetResolution resolution;
    const OptoSlangTypeLayout *type_layout;
    size_t attribute_count;
} OptoSlangNetView;

typedef struct OptoSlangInstanceView {
    const OptoSlangInstanceData *identity;
    const char *name;
    const char *module_name;
    size_t connection_count;
    size_t attribute_count;
} OptoSlangInstanceView;

typedef struct OptoSlangConnectionView {
    const char *port;
    const OptoSlangExpr *expression;
} OptoSlangConnectionView;

typedef struct OptoSlangAssignView {
    const OptoSlangExpr *lhs;
    const OptoSlangExpr *rhs;
} OptoSlangAssignView;

typedef struct OptoSlangProcedureView {
    const OptoSlangProcedureData *identity;
    OptoSlangProcedureKind kind;
    size_t event_count;
    size_t block_count;
    size_t loop_region_count;
    uint32_t entry_block;
    OptoSlangSourceSpanView source;
} OptoSlangProcedureView;

typedef struct OptoSlangLoopRegionView {
    uint32_t header;
    uint32_t body;
    uint32_t latch;
    uint32_t exit;
    OptoSlangLoopForm form;
    int has_parent;
    uint32_t parent;
    OptoSlangSourceSpanView source;
} OptoSlangLoopRegionView;

typedef struct OptoSlangEventView {
    OptoSlangEdge edge;
    const OptoSlangExpr *expression;
    const OptoSlangExpr *qualifier;
    OptoSlangSourceSpanView source;
} OptoSlangEventView;

typedef struct OptoSlangBlockView {
    size_t effect_count;
    OptoSlangTerminatorKind terminator_kind;
    OptoSlangSourceSpanView source;
} OptoSlangBlockView;

typedef struct OptoSlangEffectView {
    const OptoSlangExpr *lhs;
    const OptoSlangExpr *rhs;
    int blocking;
    OptoSlangSourceSpanView source;
} OptoSlangEffectView;

typedef struct OptoSlangTerminatorView {
    OptoSlangTerminatorKind kind;
    const OptoSlangExpr *condition;
    const OptoSlangExpr *selector;
    OptoSlangEdgeTargetView jump_edge;
    OptoSlangEdgeTargetView then_edge;
    OptoSlangEdgeTargetView else_edge;
    OptoSlangEdgeTargetView default_edge;
    size_t arm_count;
    OptoSlangSourceSpanView source;
} OptoSlangTerminatorView;

typedef struct OptoSlangSwitchArmView {
    const OptoSlangExpr *pattern;
    OptoSlangEdgeTargetView edge;
} OptoSlangSwitchArmView;

typedef struct OptoSlangTypeLayoutView {
    OptoSlangTypeLayoutKind kind;
    uint32_t width;
    int32_t array_left;
    int32_t array_right;
    int array_is_packed;
    const OptoSlangTypeLayout *array_element;
    size_t field_count;
} OptoSlangTypeLayoutView;

typedef struct OptoSlangTypeFieldView {
    const char *name;
    uint32_t bit_offset;
    const OptoSlangTypeLayout *layout;
} OptoSlangTypeFieldView;

typedef struct OptoSlangExprView {
    OptoSlangExprKind kind;
    const char *source_file;
    uint32_t source_line;
    uint32_t source_column;
    const char *signal_name;
    int signal_has_range;
    uint32_t signal_msb;
    uint32_t signal_lsb;
    int constant_has_width;
    uint32_t constant_width;
    int constant_signed;
    const char *constant_bits;
    OptoSlangUnaryOp unary_op;
    const OptoSlangExpr *unary_arg;
    OptoSlangBinaryOp binary_op;
    const OptoSlangExpr *binary_left;
    const OptoSlangExpr *binary_right;
    const OptoSlangExpr *const *concat_parts;
    size_t concat_count;
    const OptoSlangExpr *mux_condition;
    const OptoSlangExpr *mux_then;
    const OptoSlangExpr *mux_else;
    OptoSlangCastKind cast_kind;
    const OptoSlangExpr *cast_value;
    uint32_t cast_width;
    int cast_signed;
    const OptoSlangExpr *extract_value;
    uint32_t extract_lsb;
    uint32_t extract_width;
    const OptoSlangExpr *dynamic_extract_value;
    const OptoSlangExpr *dynamic_extract_offset;
    uint32_t dynamic_extract_width;
} OptoSlangExprView;

OptoSlangCompiler *opto_slang_compiler_new(void);
void opto_slang_compiler_free(OptoSlangCompiler *compiler);
OptoSlangStatus opto_slang_compiler_begin_source_unit(OptoSlangCompiler *compiler);
OptoSlangStatus opto_slang_compiler_add_source_file(
    OptoSlangCompiler *compiler, const char *path, const char *text);
OptoSlangStatus opto_slang_compiler_add_source_path(OptoSlangCompiler *compiler, const char *path);
OptoSlangStatus opto_slang_compiler_add_source_dependency(
    OptoSlangCompiler *compiler, const char *path, const char *text);
OptoSlangStatus opto_slang_compiler_add_include_dir(OptoSlangCompiler *compiler, const char *path);
OptoSlangStatus opto_slang_compiler_add_define(
    OptoSlangCompiler *compiler, const char *name, const char *value);
OptoSlangStatus opto_slang_compiler_set_top(OptoSlangCompiler *compiler, const char *top);
OptoSlangStatus opto_slang_compiler_set_language(
    OptoSlangCompiler *compiler, int language);
OptoSlangStatus opto_slang_compiler_set_max_threads(
    OptoSlangCompiler *compiler, uint32_t max_threads);
OptoSlangStatus opto_slang_compiler_compile(
    OptoSlangCompiler *compiler, OptoSlangSnapshot **design);
OptoSlangStatus opto_slang_compiler_analyze(
    OptoSlangCompiler *compiler, OptoSlangAnalysis **analysis);
const char *opto_slang_compiler_last_error(const OptoSlangCompiler *compiler);
size_t opto_slang_compiler_diagnostic_count(const OptoSlangCompiler *compiler);
OptoSlangStatus opto_slang_compiler_diagnostic_view(
    const OptoSlangCompiler *compiler, size_t index, OptoSlangDiagnosticView *view);

void opto_slang_analysis_free(OptoSlangAnalysis *analysis);
OptoSlangStatus opto_slang_analysis_view(
    const OptoSlangAnalysis *analysis, OptoSlangAnalysisView *view);
const char *opto_slang_analysis_definition_name(const OptoSlangAnalysis *analysis, size_t index);
const char *opto_slang_analysis_package_name(const OptoSlangAnalysis *analysis, size_t index);
OptoSlangStatus opto_slang_analysis_dependency_view(
    const OptoSlangAnalysis *analysis, size_t index, OptoSlangSourceFileView *view);
size_t opto_slang_analysis_diagnostic_count(const OptoSlangAnalysis *analysis);
OptoSlangStatus opto_slang_analysis_diagnostic_view(
    const OptoSlangAnalysis *analysis, size_t index, OptoSlangDiagnosticView *view);

void opto_slang_snapshot_free(OptoSlangSnapshot *design);
OptoSlangStatus opto_slang_snapshot_view(
    const OptoSlangSnapshot *design, OptoSlangSnapshotView *view);
size_t opto_slang_snapshot_diagnostic_count(const OptoSlangSnapshot *design);
OptoSlangStatus opto_slang_snapshot_diagnostic_view(
    const OptoSlangSnapshot *design, size_t index, OptoSlangDiagnosticView *view);
OptoSlangStatus opto_slang_module_view(
    const OptoSlangSnapshot *design, size_t module_index, OptoSlangModuleView *view);
OptoSlangStatus opto_slang_module_info(
    const OptoSlangSnapshot *design, size_t module_index, OptoSlangModuleInfoView *view);
OptoSlangStatus opto_slang_module_materialize(OptoSlangSnapshot *design, size_t module_index);
OptoSlangStatus opto_slang_module_materialize_failure(
    const OptoSlangSnapshot *design,
    size_t module_index,
    OptoSlangLoweringFailureView *view);
void opto_slang_module_release(OptoSlangSnapshot *design, size_t module_index);

OptoSlangStatus opto_slang_module_attribute_view(
    const OptoSlangSnapshot *design,
    size_t module_index,
    size_t attribute_index,
    OptoSlangAttributeView *view);

OptoSlangStatus opto_slang_port_view(
    const OptoSlangSnapshot *design,
    size_t module_index,
    size_t port_index,
    OptoSlangPortView *view);
OptoSlangStatus opto_slang_port_attribute_view(
    const OptoSlangPortData *port, size_t attribute_index, OptoSlangAttributeView *view);
OptoSlangStatus opto_slang_net_view(
    const OptoSlangSnapshot *design, size_t module_index, size_t net_index, OptoSlangNetView *view);
OptoSlangStatus opto_slang_net_attribute_view(
    const OptoSlangNetData *net, size_t attribute_index, OptoSlangAttributeView *view);
OptoSlangStatus opto_slang_instance_view(
    const OptoSlangSnapshot *design,
    size_t module_index,
    size_t instance_index,
    OptoSlangInstanceView *view);
OptoSlangStatus opto_slang_instance_attribute_view(
    const OptoSlangInstanceData *instance,
    size_t attribute_index,
    OptoSlangAttributeView *view);
OptoSlangStatus opto_slang_connection_view(
    const OptoSlangInstanceData *instance, size_t connection_index, OptoSlangConnectionView *view);
OptoSlangStatus opto_slang_assign_view(
    const OptoSlangSnapshot *design,
    size_t module_index,
    size_t assign_index,
    OptoSlangAssignView *view);
OptoSlangStatus opto_slang_procedure_view(
    const OptoSlangSnapshot *design,
    size_t module_index,
    size_t procedure_index,
    OptoSlangProcedureView *view);
OptoSlangStatus opto_slang_event_view(
    const OptoSlangProcedureData *procedure, size_t event_index, OptoSlangEventView *view);
OptoSlangStatus opto_slang_block_view(
    const OptoSlangProcedureData *procedure, size_t block_index, OptoSlangBlockView *view);
OptoSlangStatus opto_slang_loop_region_view(
    const OptoSlangProcedureData *procedure,
    size_t region_index,
    OptoSlangLoopRegionView *view);
OptoSlangStatus opto_slang_effect_view(
    const OptoSlangProcedureData *procedure,
    size_t block_index,
    size_t effect_index,
    OptoSlangEffectView *view);
OptoSlangStatus opto_slang_terminator_view(
    const OptoSlangProcedureData *procedure,
    size_t block_index,
    OptoSlangTerminatorView *view);
OptoSlangStatus opto_slang_switch_arm_view(
    const OptoSlangProcedureData *procedure,
    size_t block_index,
    size_t arm_index,
    OptoSlangSwitchArmView *view);
OptoSlangStatus opto_slang_type_layout_view(
    const OptoSlangTypeLayout *layout, OptoSlangTypeLayoutView *view);
OptoSlangStatus opto_slang_type_field_view(
    const OptoSlangTypeLayout *layout, size_t field_index, OptoSlangTypeFieldView *view);
OptoSlangStatus opto_slang_expr_view(const OptoSlangExpr *expression, OptoSlangExprView *view);

#ifdef __cplusplus
}
#endif

#endif
