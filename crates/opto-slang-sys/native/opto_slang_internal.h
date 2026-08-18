// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

#pragma once

#include "opto_slang_bridge.h"

#include "slang/ast/Compilation.h"
#include "slang/ast/symbols/InstanceSymbols.h"
#include "slang/text/SourceManager.h"

#include <cstdint>
#include <deque>
#include <memory>
#include <mutex>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <unordered_map>
#include <utility>
#include <vector>

namespace slang {
class ThreadPool;
namespace driver {
class Driver;
}
} // namespace slang

struct OptoSlangExpr {
    OptoSlangExprKind kind = OPTO_SLANG_EXPR_SIGNAL;
    const std::string* source_file = nullptr;
    uint32_t source_line = 0;
    uint32_t source_column = 0;
    const std::string* signal_name = nullptr;
    bool signal_has_range = false;
    uint32_t signal_msb = 0;
    uint32_t signal_lsb = 0;
    bool constant_has_width = false;
    uint32_t constant_width = 0;
    bool constant_signed = false;
    std::string constant_bits;
    OptoSlangUnaryOp unary_op = OPTO_SLANG_UNARY_LOGICAL_NOT;
    OptoSlangBinaryOp binary_op = OPTO_SLANG_BINARY_ADD;
    const OptoSlangExpr* unary_arg = nullptr;
    const OptoSlangExpr* binary_left = nullptr;
    const OptoSlangExpr* binary_right = nullptr;
    std::vector<const OptoSlangExpr*> concat_parts;
    const OptoSlangExpr* mux_condition = nullptr;
    const OptoSlangExpr* mux_then = nullptr;
    const OptoSlangExpr* mux_else = nullptr;
    OptoSlangCastKind cast_kind = OPTO_SLANG_CAST_ZERO_EXTEND;
    const OptoSlangExpr* cast_value = nullptr;
    uint32_t cast_width = 1;
    bool cast_signed = false;
    const OptoSlangExpr* extract_value = nullptr;
    uint32_t extract_lsb = 0;
    uint32_t extract_width = 1;
    const OptoSlangExpr* dynamic_extract_value = nullptr;
    const OptoSlangExpr* dynamic_extract_offset = nullptr;
    uint32_t dynamic_extract_offset_width = 1;
    uint32_t dynamic_extract_width = 1;
};

struct OptoSlangTypeLayoutField {
    std::string name;
    uint32_t bit_offset = 0;
    const OptoSlangTypeLayout* layout = nullptr;
};

struct OptoSlangTypeLayout {
    OptoSlangTypeLayoutKind kind = OPTO_SLANG_TYPE_SCALAR;
    uint32_t width = 1;
    int32_t array_left = 0;
    int32_t array_right = 0;
    bool array_is_packed = false;
    const OptoSlangTypeLayout* array_element = nullptr;
    std::vector<OptoSlangTypeLayoutField> fields;
};

struct OptoSlangAttributeData {
    std::string name;
    OptoSlangAttributeValueKind kind = OPTO_SLANG_ATTRIBUTE_OTHER;
    std::string value;
    uint32_t integer_width = 0;
    bool integer_signed = false;
    bool is_true = false;
    OptoSlangSourceSpanView source{};
};

struct OptoSlangPortData {
    std::string name;
    OptoSlangPortDirection direction = OPTO_SLANG_PORT_INPUT;
    uint32_t width = 1;
    bool is_signed = false;
    OptoSlangNetResolution resolution = OPTO_SLANG_NET_SINGLE_DRIVER;
    const OptoSlangTypeLayout* type_layout = nullptr;
    std::vector<OptoSlangAttributeData> attributes;
};

struct OptoSlangNetData {
    std::string name;
    uint32_t width = 1;
    bool is_signed = false;
    bool element_is_signed = false;
    bool is_process_local = false;
    OptoSlangNetResolution resolution = OPTO_SLANG_NET_SINGLE_DRIVER;
    const OptoSlangTypeLayout* type_layout = nullptr;
    std::vector<OptoSlangAttributeData> attributes;
};

struct OptoSlangConnectionData {
    std::string port;
    const OptoSlangExpr* expr = nullptr;
};

struct OptoSlangInstanceData {
    std::string name;
    std::string module_name;
    std::vector<OptoSlangConnectionData> connections;
    std::vector<OptoSlangAttributeData> attributes;
};

struct OptoSlangAssignData {
    const OptoSlangExpr* lhs = nullptr;
    const OptoSlangExpr* rhs = nullptr;
};

struct OptoSlangEffectData {
    const OptoSlangExpr* lhs = nullptr;
    const OptoSlangExpr* rhs = nullptr;
    bool blocking = true;
    OptoSlangSourceSpanView source{};
};

struct OptoSlangEdgeTargetData {
    uint32_t block = 0;
    OptoSlangSourceSpanView source{};
};

struct OptoSlangSwitchArmData {
    const OptoSlangExpr* pattern = nullptr;
    OptoSlangEdgeTargetData edge;
};

struct OptoSlangTerminatorData {
    OptoSlangTerminatorKind kind = OPTO_SLANG_TERMINATOR_RETURN;
    const OptoSlangExpr* condition = nullptr;
    const OptoSlangExpr* selector = nullptr;
    OptoSlangEdgeTargetData jump_edge;
    OptoSlangEdgeTargetData then_edge;
    OptoSlangEdgeTargetData else_edge;
    OptoSlangEdgeTargetData default_edge;
    std::vector<OptoSlangSwitchArmData> arms;
    OptoSlangSourceSpanView source{};
};

struct OptoSlangBlockData {
    std::vector<OptoSlangEffectData> effects;
    OptoSlangTerminatorData terminator;
    bool terminated = false;
    OptoSlangSourceSpanView source{};
};

struct OptoSlangEventData {
    OptoSlangEdge edge = OPTO_SLANG_EDGE_POS;
    const OptoSlangExpr* expression = nullptr;
    const OptoSlangExpr* qualifier = nullptr;
    OptoSlangSourceSpanView source{};
};

struct OptoSlangLoopRegionData {
    uint32_t header = 0;
    uint32_t body = 0;
    uint32_t latch = 0;
    uint32_t exit = 0;
    OptoSlangLoopForm form = OPTO_SLANG_LOOP_PRE_TEST;
    std::optional<uint32_t> parent;
    OptoSlangSourceSpanView source{};
};

struct OptoSlangProcedureData {
    OptoSlangProcedureKind kind = OPTO_SLANG_PROCEDURE_COMB;
    std::vector<OptoSlangEventData> events;
    std::vector<OptoSlangBlockData> blocks;
    std::vector<OptoSlangLoopRegionData> loop_regions;
    uint32_t entry_block = 0;
    OptoSlangSourceSpanView source{};
};

struct OptoSlangModulePayload {
    std::vector<OptoSlangAttributeData> attributes;
    std::vector<OptoSlangPortData> ports;
    std::vector<OptoSlangNetData> nets;
    std::vector<OptoSlangInstanceData> instances;
    std::vector<OptoSlangAssignData> assigns;
    std::vector<OptoSlangProcedureData> procedures;
    std::deque<OptoSlangExpr> exprs;
    std::deque<std::string> interned_strings;
    std::unordered_map<std::string_view, const std::string*> interned_index;
    std::deque<std::string> source_path_storage;
    std::unordered_map<uint32_t, const std::string*> source_paths_by_buffer;
    std::vector<std::unique_ptr<OptoSlangTypeLayout>> type_layouts;
};

struct OptoSlangLoweringFailure {
    OptoSlangLoweringFailure() = default;
    OptoSlangLoweringFailure(
        OptoSlangLoweringFailureCategory category,
        uint16_t code,
        std::string message)
        : category(category), code(code), message(std::move(message)) {}

    OptoSlangLoweringFailureCategory category = OPTO_SLANG_LOWERING_NATIVE;
    uint16_t code = 1;
    std::string message;
    std::string file;
    uint32_t line = 0;
    uint32_t column = 0;
};

struct OptoSlangModuleData {
    std::string name;
    uint64_t source_order = UINT64_MAX;
    // One allocation owns the complete ephemeral lowering result so the last
    // materialization lease can return all of its capacity to the allocator.
    std::unique_ptr<OptoSlangModulePayload> payload;
    std::mutex materialize_mutex;
    size_t materialize_users = 0;
    std::optional<OptoSlangLoweringFailure> materialize_failure;
};

struct OptoSlangCompilationState;

struct OptoSlangDiagnostic {
    OptoSlangDiagnosticSeverity severity = OPTO_SLANG_DIAGNOSTIC_ERROR;
    uint16_t subsystem = 0;
    uint16_t code = 0;
    std::string message;
    std::string option_name;
    std::string file;
    uint32_t line = 0;
    uint32_t column = 0;
    uint32_t length = 1;
};

struct OptoSlangSnapshot {
    OptoSlangSnapshot();
    ~OptoSlangSnapshot();

    std::vector<std::unique_ptr<OptoSlangModuleData>> modules;
    std::string top;
    std::unique_ptr<OptoSlangCompilationState> compilation_state;
    std::mutex materialization_mutex;
    std::unordered_map<const slang::ast::InstanceBodySymbol*, std::string> body_names;
    const slang::SourceManager* source_manager = nullptr;
    std::vector<OptoSlangDiagnostic> diagnostics;
};

struct OptoSlangSourceUnit {
    struct SourceFile {
        std::string path;
        std::optional<std::string> text;
    };
    std::vector<SourceFile> files;
    std::vector<SourceFile> dependencies;
    std::vector<std::string> include_dirs;
    std::vector<std::pair<std::string, std::optional<std::string>>> defines;
};

struct OptoSlangAnalysis {
    std::vector<std::string> definitions;
    std::vector<std::string> packages;
    std::vector<OptoSlangSourceUnit::SourceFile> dependencies;
    std::vector<OptoSlangDiagnostic> diagnostics;
};

struct OptoSlangCompiler {
    std::vector<OptoSlangSourceUnit> units;
    std::optional<std::string> top;
    slang::LanguageVersion language = slang::LanguageVersion::v1800_2017;
    uint32_t max_threads = 0;
    std::string last_error;
    std::vector<OptoSlangDiagnostic> diagnostics;
};

void opto_slang_collect_modules(
    OptoSlangSnapshot& design,
    std::span<const slang::ast::InstanceSymbol* const> tops,
    std::unique_ptr<slang::driver::Driver> driver,
    std::unique_ptr<slang::ast::Compilation> compilation);

OptoSlangStatus opto_slang_materialize_module(OptoSlangSnapshot& design, size_t module_index);

void opto_slang_release_module(OptoSlangSnapshot& design, size_t module_index);

void opto_slang_prepare_module_names(
    OptoSlangSnapshot& design, std::span<const slang::ast::InstanceSymbol* const> tops);
