// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

#pragma once

#include "opto_slang_internal.h"

#include "slang/ast/ASTContext.h"
#include "slang/ast/ASTVisitor.h"
#include "slang/ast/Bitstream.h"
#include "slang/ast/Compilation.h"
#include "slang/ast/EvalContext.h"
#include "slang/ast/Expression.h"
#include "slang/ast/Patterns.h"
#include "slang/ast/SemanticFacts.h"
#include "slang/ast/TimingControl.h"
#include "slang/ast/expressions/AssertionExpr.h"
#include "slang/ast/expressions/AssignmentExpressions.h"
#include "slang/ast/expressions/CallExpression.h"
#include "slang/ast/expressions/ConversionExpression.h"
#include "slang/ast/expressions/LiteralExpressions.h"
#include "slang/ast/expressions/MiscExpressions.h"
#include "slang/ast/expressions/Operator.h"
#include "slang/ast/expressions/OperatorExpressions.h"
#include "slang/ast/expressions/SelectExpressions.h"
#include "slang/ast/statements/ConditionalStatements.h"
#include "slang/ast/statements/LoopStatements.h"
#include "slang/ast/statements/MiscStatements.h"
#include "slang/ast/symbols/BlockSymbols.h"
#include "slang/ast/symbols/CompilationUnitSymbols.h"
#include "slang/ast/symbols/InstanceSymbols.h"
#include "slang/ast/symbols/MemberSymbols.h"
#include "slang/ast/symbols/ParameterSymbols.h"
#include "slang/ast/symbols/PortSymbols.h"
#include "slang/ast/symbols/VariableSymbols.h"
#include "slang/ast/types/AllTypes.h"
#include "slang/ast/types/NetType.h"
#include "slang/ast/types/Type.h"
#include "slang/ast/types/TypePrinter.h"
#include "slang/diagnostics/DiagnosticEngine.h"
#include "slang/driver/Driver.h"
#include "slang/numeric/SVInt.h"
#include "slang/syntax/AllSyntax.h"
#include "slang/text/SourceManager.h"

#include <algorithm>
#include <bit>
#include <cstdint>
#include <filesystem>
#include <iomanip>
#include <iterator>
#include <limits>
#include <map>
#include <memory>
#include <optional>
#include <sstream>
#include <stdexcept>
#include <string>
#include <string_view>
#include <unordered_map>
#include <unordered_set>
#include <utility>
#include <vector>

using namespace slang;
using namespace slang::ast;
using namespace slang::driver;
using namespace slang::syntax;

namespace opto::slang_lower {

template <typename Cleanup> class ScopeExit {
public:
    explicit ScopeExit(Cleanup cleanup) : cleanup(std::move(cleanup)) {}
    ScopeExit(const ScopeExit&) = delete;
    ScopeExit& operator=(const ScopeExit&) = delete;
    ~ScopeExit() noexcept {
        cleanup();
    }

private:
    Cleanup cleanup;
};

template <typename T> class ScopedValue {
public:
    explicit ScopedValue(T& target) : target(target), previous(target) {}
    ScopedValue(T& target, T replacement)
        : target(target), previous(std::exchange(target, std::move(replacement))) {}
    ScopedValue(const ScopedValue&) = delete;
    ScopedValue& operator=(const ScopedValue&) = delete;
    ~ScopedValue() {
        target = std::move(previous);
    }

private:
    T& target;
    T previous;
};

struct FunctionReturnControl {
    const OptoSlangExpr* returned = nullptr;
    const OptoSlangExpr* not_returned = nullptr;
    const OptoSlangExpr* true_value = nullptr;
};

struct LoopControlFlag {
    const OptoSlangExpr* value = nullptr;
    const OptoSlangExpr* inactive = nullptr;
};

struct LoopControl {
    std::optional<uint32_t> break_target;
    std::optional<uint32_t> continue_target;
    std::optional<uint32_t> cyclic_region;
};

// One activation of a named sequential block or inlined task. A disable
// targets symbol identity, while recursive or unrolled activations receive
// distinct flags and resolve the innermost active match.
struct DisableControl {
    const Symbol* target = nullptr;
    LoopControlFlag disabled;
    const OptoSlangExpr* true_value = nullptr;
    const OptoSlangExpr* false_value = nullptr;
};

struct ValueShape {
    uint32_t width = 1;
    bool is_signed = false;

    bool operator==(const ValueShape&) const = default;
};

struct TaggedUnionLayout {
    // Both packed and unpacked tagged unions use [tag | padding | payload].
    // Padding is not part of member identity and the active value is always
    // aligned to bit zero, matching Slang's packed tagged-union convention.
    uint32_t tag_width = 0;
    uint32_t payload_width = 0;
    uint32_t total_width = 0;
};

struct CfgFragment;
class ProcedureBuilder;

struct GuardedEffectData {
    const OptoSlangExpr* condition = nullptr;
    OptoSlangEffectData effect;
};

// A fresh context is created for each materialized module. It encapsulates all
// mutable lowering arenas and scope stacks; the long-lived snapshot retains
// only frozen slang state and the module inventory.
struct ModuleLoweringContext {
    explicit ModuleLoweringContext(
        OptoSlangModulePayload& module,
        const InstanceBodySymbol& body,
        const std::unordered_map<const InstanceBodySymbol*, std::string>& body_names,
        const SourceManager* source_manager)
        : module(module), body(body), body_names(body_names), source_manager(source_manager) {}

    OptoSlangModulePayload& module;
    const InstanceBodySymbol& body;
    std::unordered_set<std::string> net_names;
    std::unordered_map<std::string, ValueShape> value_shapes;
    std::unordered_map<const Type*, const OptoSlangTypeLayout*> type_layout_by_type;
    const OptoSlangTypeLayout* scalar_type_layout = nullptr;
    std::unordered_map<const ValueSymbol*, std::string> value_names;
    std::unordered_map<
        const InstanceBodySymbol*,
        std::unordered_map<const ValueSymbol*, std::string>>
        interface_port_names;
    const std::unordered_map<const InstanceBodySymbol*, std::string>& body_names;
    std::unordered_map<const ValueSymbol*, ConstantValue> procedural_constants;
    std::unordered_set<const VariableSymbol*> procedural_loop_variables;
    std::unordered_map<const ValueSymbol*, OptoSlangExpr*> function_values;
    std::unordered_map<const ValueSymbol*, OptoSlangExpr*> function_lvalues;
    std::vector<const VariableSymbol*> function_returns;
    std::vector<FunctionReturnControl> function_return_controls;
    std::vector<LoopControl> loop_controls;
    std::vector<DisableControl> disable_controls;
    const std::unordered_set<const VariableSymbol*>* loop_live_after = nullptr;
    bool loop_liveness_indexed = false;
    std::unordered_map<const VariableSymbol*, size_t> loop_read_only_process_counts;
    std::unordered_set<const VariableSymbol*> loop_external_references;
    std::unordered_set<const VariableSymbol*> procedure_loop_local_bindings;
    uint32_t cyclic_loop_depth = 0;
    std::vector<OptoSlangExpr*> lvalue_references;
    CfgFragment* active_expression_prelude = nullptr;
    ProcedureBuilder* active_procedure_builder = nullptr;
    std::vector<const SubroutineSymbol*> function_stack;
    uint64_t next_function_instance = 0;
    uint64_t next_loop_instance = 0;
    uint64_t next_disable_instance = 0;
    uint64_t next_lvalue_instance = 0;
    EvalContext* eval_context = nullptr;
    const SourceManager* source_manager = nullptr;
};

template<typename Value>
class ScopedSymbolMapBindings {
public:
    explicit ScopedSymbolMapBindings(std::unordered_map<const ValueSymbol*, Value>& bindings)
        : bindings_(bindings) {}

    ScopedSymbolMapBindings(const ScopedSymbolMapBindings&) = delete;
    ScopedSymbolMapBindings& operator=(const ScopedSymbolMapBindings&) = delete;

    ~ScopedSymbolMapBindings() {
        for (auto& [symbol, previous] : previous_) {
            if (previous) {
                bindings_.insert_or_assign(symbol, std::move(*previous));
            } else {
                bindings_.erase(symbol);
            }
        }
    }

    void track(const ValueSymbol* symbol) {
        if (!symbol || std::ranges::any_of(previous_, [symbol](const auto& entry) {
                return entry.first == symbol;
            })) {
            return;
        }
        auto found = bindings_.find(symbol);
        if (found == bindings_.end()) {
            previous_.emplace_back(symbol, std::nullopt);
        } else {
            previous_.emplace_back(symbol, found->second);
        }
    }

private:
    std::unordered_map<const ValueSymbol*, Value>& bindings_;
    std::vector<std::pair<const ValueSymbol*, std::optional<Value>>> previous_;
};


struct ModuleMembers {
    std::vector<const NetSymbol*> nets;
    std::vector<const VariableSymbol*> variables;
    std::vector<const InstanceSymbol*> instances;
    std::vector<const InstanceSymbol*> interface_instances;
    std::vector<const UninstantiatedDefSymbol*> unresolved_instances;
    std::vector<const PrimitiveInstanceSymbol*> primitives;
    std::vector<const ContinuousAssignSymbol*> assigns;
    std::vector<const ProceduralBlockSymbol*> processes;
};

struct LvalueLeaf {
    const Expression* expression;
    uint32_t width;
};


struct InterfaceSignal {
    std::string_view name;
    const ValueSymbol* value = nullptr;
    const ValueSymbol* reference = nullptr;
    ArgumentDirection direction = ArgumentDirection::InOut;
};

// Cross-component lowering contracts.
ConstantValue evaluate_lowering_constant(
    ModuleLoweringContext& design, const Expression& expression);
std::string copy_string(std::string_view text);
uint32_t checked_width(uint64_t width, std::string_view object_name);
// Returns the one canonical hardware storage width. Unlike Slang's
// getBitstreamWidth(), this includes the out-of-band discriminant of an
// unpacked tagged union and recursively accounts for nested tagged values.
uint64_t lowered_type_width(const Type& source_type);
TaggedUnionLayout tagged_union_layout(const Type& source_type);
const OptoSlangTypeLayout*
store_type_layout(ModuleLoweringContext& design, OptoSlangTypeLayout layout);
const OptoSlangTypeLayout* scalar_type_layout(ModuleLoweringContext& design);
const OptoSlangTypeLayout*
intern_type_layout(ModuleLoweringContext& design, const Type& source_type);
uint32_t aggregate_field_storage_offset(const Type& aggregate_type, const FieldSymbol& field);
OptoSlangPortDirection lower_direction(ArgumentDirection direction);
OptoSlangUnaryOp lower_unary_op(UnaryOperator op);
OptoSlangBinaryOp lower_binary_op(BinaryOperator op);
uint32_t checked_source_coordinate(size_t value, std::string_view coordinate);
const std::string*
interned_source_path(ModuleLoweringContext& design, slang::SourceLocation location);
void set_expr_source(
    ModuleLoweringContext& design, OptoSlangExpr& lowered, const Expression& expression);
template<typename Node>
OptoSlangSourceSpanView source_span(ModuleLoweringContext& design, const Node& node) {
    if (!design.source_manager) {
        throw std::runtime_error("slang source manager is unavailable during lowering");
    }
    auto location = node.sourceRange.start();
    if (!location.valid()) {
        return {};
    }
    location = design.source_manager->getFullyOriginalLoc(location);
    const auto* file = interned_source_path(design, location);
    return {
        file ? file->c_str() : nullptr,
        checked_source_coordinate(design.source_manager->getLineNumber(location), "line"),
        checked_source_coordinate(design.source_manager->getColumnNumber(location), "column"),
    };
}
OptoSlangSourceSpanView
source_span(ModuleLoweringContext& design, slang::SourceLocation location);
std::string expression_location(const ModuleLoweringContext& design, const Expression& expression);
std::string statement_location(const ModuleLoweringContext& design, const Statement& statement);
const std::string* intern_string(ModuleLoweringContext& design, std::string value);
OptoSlangExpr*
make_expr(ModuleLoweringContext& design, OptoSlangExpr expr, const Expression& source);
OptoSlangExpr* make_signal_expr(ModuleLoweringContext& design, std::string name);
bool is_port_backref(const ValueSymbol& symbol);
std::string unsupported_member_message(const InstanceBodySymbol& body, const Symbol& symbol);
void collect_instance_leaf(
    const InstanceBodySymbol& body, const Symbol& symbol, ModuleMembers& members);
void collect_elaborated_members(
    const InstanceBodySymbol& body, const Scope& scope, ModuleMembers& members);
void collect_interface_leaves(const Symbol* symbol, std::vector<const InstanceSymbol*>& leaves);
std::vector<InterfaceSignal>
interface_signals(const InstanceSymbol& instance, std::string_view modport_name);
std::optional<ArgumentDirection>
merge_interface_direction(std::optional<ArgumentDirection> current, ArgumentDirection next);
std::optional<ArgumentDirection> infer_interface_direction_impl(
    const InstanceBodySymbol& body,
    const ValueSymbol& value,
    std::unordered_set<const InstanceBodySymbol*>& visiting);
ArgumentDirection
infer_interface_direction(const InstanceBodySymbol& body, const ValueSymbol& value);
bool same_interface_value(const Symbol* reference, const ValueSymbol& target);
bool expression_references_interface_value(
    const Expression& expression, const ValueSymbol& target);
bool interface_value_is_driven_impl(
    const InstanceBodySymbol& body,
    const ValueSymbol& value,
    std::unordered_set<const InstanceBodySymbol*>& visiting);
bool interface_value_is_driven(const InstanceBodySymbol& body, const ValueSymbol& value);
std::string flattened_interface_port_name(
    std::string_view port, size_t element, size_t element_count, std::string_view signal);
std::string module_relative_name(const InstanceBodySymbol& body, const Symbol& symbol);
bool is_procedural_local(const InstanceBodySymbol& body, const Symbol& symbol);
std::string procedural_local_base_name(const InstanceBodySymbol& body, const ValueSymbol& symbol);
std::string unique_internal_name(std::unordered_set<std::string>& existing, std::string base);
std::string add_internal_net(
    ModuleLoweringContext& design,
    std::string base,
    uint32_t width,
    bool is_signed,
    bool is_process_local = false);
std::string registered_value_name(const ModuleLoweringContext& design, const ValueSymbol& symbol);
bool has_registered_value(const ModuleLoweringContext& design, const ValueSymbol& symbol);
const OptoSlangExpr*
find_function_lvalue(const ModuleLoweringContext& design, const ValueSymbol& symbol);
std::optional<uint32_t> integer_literal_u32(ModuleLoweringContext& design, const Expression& expr);
std::string exact_binary_string(const SVInt& value);
const Expression& require_primitive_port(
    const PrimitiveInstanceSymbol& instance,
    std::span<const Expression* const> ports,
    size_t index);
OptoSlangExpr* make_unary_expr(
    ModuleLoweringContext& design,
    OptoSlangUnaryOp op,
    const OptoSlangExpr* arg,
    const Expression& source);
OptoSlangExpr* make_binary_expr(
    ModuleLoweringContext& design,
    OptoSlangBinaryOp op,
    const OptoSlangExpr* left,
    const OptoSlangExpr* right,
    const Expression& source);
OptoSlangExpr* make_unsigned_constant_expr(
    ModuleLoweringContext& design, uint32_t value, uint32_t width, const Expression& source);
OptoSlangExpr* make_signed_constant_expr(
    ModuleLoweringContext& design, int64_t value, uint32_t width, const Expression& source);
OptoSlangExpr* make_unsigned_cast_expr(
    ModuleLoweringContext& design,
    const OptoSlangExpr* value,
    uint32_t width,
    const Expression& source);
OptoSlangExpr* make_signed_cast_expr(
    ModuleLoweringContext& design,
    const OptoSlangExpr* value,
    uint32_t width,
    const Expression& source);
ValueShape lowered_value_shape(const ModuleLoweringContext& design, const OptoSlangExpr& value);
OptoSlangExpr* cast_to_shape(
    ModuleLoweringContext& design,
    OptoSlangExpr* value,
    uint32_t target_width,
    bool target_signed,
    const Expression& source,
    bool force = false);
OptoSlangExpr* cast_to_type(
    ModuleLoweringContext& design,
    OptoSlangExpr* value,
    const Type& result_type,
    const Expression& source,
    bool force = false);
ValueShape lvalue_shape(const ModuleLoweringContext& design, const Expression& expression);
OptoSlangExpr*
cast_to_lvalue_type(ModuleLoweringContext& design, OptoSlangExpr* value, const Expression& lvalue);
OptoSlangExpr* cast_to_expression_type(
    ModuleLoweringContext& design,
    OptoSlangExpr* value,
    const Expression& result,
    bool force = false);
OptoSlangExpr* make_high_impedance_expr(ModuleLoweringContext& design, const Expression& source);
OptoSlangExpr* make_mux_expr(
    ModuleLoweringContext& design,
    const OptoSlangExpr* condition,
    const OptoSlangExpr* then_value,
    const OptoSlangExpr* else_value,
    const Expression& source);
OptoSlangExpr* lower_boolean_context(ModuleLoweringContext& design, const Expression& expression);
bool is_empty_connection_expression(const Expression& expr);
const Expression& call_output_lvalue(const Expression& actual);
OptoSlangExpr* lower_function_call(ModuleLoweringContext&, const CallExpression&);
OptoSlangExpr* lower_assignment_expression(ModuleLoweringContext&, const AssignmentExpression&);
OptoSlangExpr* lower_update_expression(ModuleLoweringContext&, const UnaryExpression&);
OptoSlangExpr* lower_conditional_expression(ModuleLoweringContext&, const ConditionalExpression&);
OptoSlangExpr* lower_short_circuit_expression(ModuleLoweringContext&, const BinaryExpression&);
bool constant_element_select_is_out_of_range(ModuleLoweringContext&, const Expression&);
OptoSlangExpr* lower_signal_expr(ModuleLoweringContext&, const Expression&);
OptoSlangExpr* apply_rvalue_slice(
    ModuleLoweringContext&, const OptoSlangExpr*, uint64_t, uint32_t, const Expression&);
void collect_lvalue_leaves(const Expression&, std::vector<LvalueLeaf>&);
std::vector<OptoSlangAssignData> lower_continuous_assignment(ModuleLoweringContext&, const AssignmentExpression&);
OptoSlangExpr* lower_inside_item_match(ModuleLoweringContext&, const OptoSlangExpr*, const Expression&);
std::optional<bool>
constant_boolean_value(ModuleLoweringContext&, const Expression&);
OptoSlangExpr* lower_expr(ModuleLoweringContext&, const Expression&);
OptoSlangProcedureData lower_procedure(ModuleLoweringContext&, const InstanceBodySymbol&, const ProceduralBlockSymbol&);
OptoSlangProcedureData make_guarded_procedure(
    std::vector<GuardedEffectData>,
    OptoSlangProcedureKind,
    std::vector<OptoSlangEventData>,
    OptoSlangSourceSpanView);
void validate_initial_process(ModuleLoweringContext&, const InstanceBodySymbol&, const ProceduralBlockSymbol&);
} // namespace opto::slang_lower
