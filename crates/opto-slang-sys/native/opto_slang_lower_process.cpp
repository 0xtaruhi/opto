// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

#include "opto_slang_lower_internal.h"

namespace opto::slang_lower {
struct CfgFragment {
    std::optional<uint32_t> entry;
    std::vector<uint32_t> exits;

    bool empty() const {
        return !entry;
    }
};

class ProcedureBuilder {
public:
    uint32_t add_block(OptoSlangSourceSpanView source) {
        if (procedure_.blocks.size() >= UINT32_MAX) {
            throw std::runtime_error("procedural CFG exceeds 32-bit block capacity");
        }
        const auto index = static_cast<uint32_t>(procedure_.blocks.size());
        procedure_.blocks.push_back(OptoSlangBlockData{{}, {}, false, source});
        return index;
    }

    CfgFragment effects(
        std::vector<OptoSlangEffectData> effects, OptoSlangSourceSpanView source) {
        if (effects.empty()) {
            return {};
        }
        const auto block = add_block(source);
        procedure_.blocks[block].effects = std::move(effects);
        return {block, {block}};
    }

    CfgFragment sequence(
        CfgFragment first, CfgFragment second, OptoSlangSourceSpanView source) {
        if (first.empty()) {
            return second;
        }
        if (second.empty()) {
            return first;
        }
        connect(first.exits, *second.entry, source);
        return {first.entry, std::move(second.exits)};
    }

    CfgFragment guard(
        const OptoSlangExpr* condition,
        CfgFragment body,
        OptoSlangSourceSpanView source) {
        if (body.empty()) {
            return {};
        }
        const auto dispatch = add_block(source);
        const auto join = add_block(source);
        branch(dispatch, condition, *body.entry, join, source);
        connect(body.exits, join, source);
        return {dispatch, {join}};
    }

    void jump(uint32_t from, uint32_t target, OptoSlangSourceSpanView source) {
        OptoSlangTerminatorData terminator;
        terminator.kind = OPTO_SLANG_TERMINATOR_JUMP;
        terminator.jump_edge = {target, source};
        terminator.source = source;
        terminate(from, std::move(terminator));
    }

    void branch(
        uint32_t from,
        const OptoSlangExpr* condition,
        uint32_t then_target,
        uint32_t else_target,
        OptoSlangSourceSpanView source) {
        OptoSlangTerminatorData terminator;
        terminator.kind = OPTO_SLANG_TERMINATOR_BRANCH;
        terminator.condition = condition;
        terminator.then_edge = {then_target, source};
        terminator.else_edge = {else_target, source};
        terminator.source = source;
        terminate(from, std::move(terminator));
    }

    void switch_(
        uint32_t from,
        const OptoSlangExpr* selector,
        std::vector<OptoSlangSwitchArmData> arms,
        uint32_t default_target,
        OptoSlangSourceSpanView source) {
        OptoSlangTerminatorData terminator;
        terminator.kind = OPTO_SLANG_TERMINATOR_SWITCH;
        terminator.selector = selector;
        terminator.arms = std::move(arms);
        terminator.default_edge = {default_target, source};
        terminator.source = source;
        terminate(from, std::move(terminator));
    }

    OptoSlangProcedureData finish(
        CfgFragment body,
        OptoSlangProcedureKind kind,
        std::vector<OptoSlangEventData> events,
        OptoSlangSourceSpanView source) {
        if (body.empty()) {
            return {};
        }
        OptoSlangTerminatorData terminator;
        terminator.kind = OPTO_SLANG_TERMINATOR_RETURN;
        terminator.source = source;
        for (auto exit : body.exits) {
            terminate(exit, terminator);
        }
        for (const auto& block : procedure_.blocks) {
            if (!block.terminated) {
                throw std::runtime_error("procedural CFG contains an unterminated block");
            }
        }
        validate(*body.entry, kind, events);
        procedure_.kind = kind;
        procedure_.events = std::move(events);
        procedure_.entry_block = *body.entry;
        procedure_.source = source;
        return std::move(procedure_);
    }

private:
    void validate(
        uint32_t entry,
        OptoSlangProcedureKind kind,
        const std::vector<OptoSlangEventData>& events) const {
        if ((kind == OPTO_SLANG_PROCEDURE_FLOP) != !events.empty()) {
            throw std::runtime_error("procedure kind and sensitivity events are inconsistent");
        }
        std::vector<bool> reached(procedure_.blocks.size());
        std::vector<uint32_t> pending{entry};
        while (!pending.empty()) {
            const auto block_index = pending.back();
            pending.pop_back();
            if (block_index >= procedure_.blocks.size()) {
                throw std::runtime_error("procedural CFG edge targets an unknown block");
            }
            if (reached[block_index]) {
                continue;
            }
            reached[block_index] = true;
            const auto& block = procedure_.blocks[block_index];
            if (std::ranges::any_of(block.effects, [](const auto& effect) {
                    return !effect.lhs || !effect.rhs;
                })) {
                throw std::runtime_error("procedural CFG contains an incomplete effect");
            }
            const auto& terminator = block.terminator;
            switch (terminator.kind) {
            case OPTO_SLANG_TERMINATOR_RETURN:
                break;
            case OPTO_SLANG_TERMINATOR_JUMP:
                pending.push_back(terminator.jump_edge.block);
                break;
            case OPTO_SLANG_TERMINATOR_BRANCH:
                if (!terminator.condition) {
                    throw std::runtime_error("procedural branch has no condition");
                }
                pending.push_back(terminator.else_edge.block);
                pending.push_back(terminator.then_edge.block);
                break;
            case OPTO_SLANG_TERMINATOR_SWITCH:
                if (!terminator.selector || terminator.arms.empty()) {
                    throw std::runtime_error("procedural switch is incomplete");
                }
                pending.push_back(terminator.default_edge.block);
                for (auto arm = terminator.arms.rbegin(); arm != terminator.arms.rend(); ++arm) {
                    if (!arm->pattern) {
                        throw std::runtime_error("procedural switch arm has no pattern");
                    }
                    pending.push_back(arm->edge.block);
                }
                break;
            }
        }
        if (std::ranges::find(reached, false) != reached.end()) {
            throw std::runtime_error("procedural CFG contains an unreachable block");
        }
    }

    void connect(
        const std::vector<uint32_t>& exits,
        uint32_t target,
        OptoSlangSourceSpanView source) {
        for (auto exit : exits) {
            jump(exit, target, source);
        }
    }

    void terminate(uint32_t block, OptoSlangTerminatorData terminator) {
        if (block >= procedure_.blocks.size() || procedure_.blocks[block].terminated) {
            throw std::runtime_error("procedural CFG block is invalid or already terminated");
        }
        procedure_.blocks[block].terminator = std::move(terminator);
        procedure_.blocks[block].terminated = true;
    }

    OptoSlangProcedureData procedure_;
};

CfgFragment lower_statement(
    ProcedureBuilder& builder,
    ModuleLoweringContext& design,
    const Statement& stmt,
    OptoSlangProcedureKind procedure_kind);

CfgFragment lower_subroutine_call_statement(
    ProcedureBuilder& builder,
    ModuleLoweringContext& design,
    const CallExpression& call,
    OptoSlangProcedureKind procedure_kind);

CfgFragment lower_assignment_statement(
    ProcedureBuilder& builder,
    ModuleLoweringContext& design,
    const AssignmentExpression& assignment,
    OptoSlangProcedureKind procedure_kind) {
    const bool blocking =
        assignment.isBlocking() || procedure_kind == OPTO_SLANG_PROCEDURE_COMB;
    const auto source = source_span(design, assignment);
    if (assignment.timingControl &&
        assignment.timingControl->kind != TimingControlKind::Delay &&
        assignment.timingControl->kind != TimingControlKind::Delay3) {
        throw std::runtime_error(
            "event timing controls are not supported on procedural assignments");
    }

    if (assignment.left().kind != ExpressionKind::Concatenation &&
        constant_element_select_is_out_of_range(design, assignment.left())) {
        return {};
    }

    if (assignment.left().kind == ExpressionKind::Concatenation) {
        if (assignment.isCompound()) {
            throw std::runtime_error(
                "compound assignment to an lvalue concatenation is not supported");
        }
        std::vector<LvalueLeaf> leaves;
        collect_lvalue_leaves(assignment.left(), leaves);
        if (leaves.empty()) {
            throw std::runtime_error("procedural assignment has an empty lvalue");
        }
        const auto total_width = checked_width(
            assignment.left().type->getBitstreamWidth(), "procedural assignment lvalue");
        const auto* rhs =
            cast_to_lvalue_type(design, lower_expr(design, assignment.right()), assignment.left());
        std::vector<OptoSlangEffectData> effects;
        effects.reserve(leaves.size() + 1);
        if (blocking) {
            auto temp_name = add_internal_net(
                design,
                "__opto_lvalue_" + std::to_string(design.next_lvalue_instance++),
                total_width,
                assignment.left().type->isSigned());
            OptoSlangExpr temp;
            temp.kind = OPTO_SLANG_EXPR_SIGNAL;
            temp.signal_name = intern_string(design, std::move(temp_name));
            auto* temp_value = make_expr(design, std::move(temp), assignment.right());
            effects.push_back({temp_value, rhs, true, source});
            rhs = temp_value;
        }

        uint64_t consumed = 0;
        for (const auto& leaf : leaves) {
            consumed += leaf.width;
            if (consumed > total_width) {
                throw std::runtime_error("lvalue concatenation width exceeds its assignment type");
            }
            if (constant_element_select_is_out_of_range(design, *leaf.expression)) {
                continue;
            }
            effects.push_back(
                {
                    lower_signal_expr(design, *leaf.expression),
                    apply_rvalue_slice(
                        design, rhs, total_width - consumed, leaf.width, assignment.right()),
                    blocking,
                    source,
                });
        }
        if (consumed != total_width) {
            throw std::runtime_error(
                "lvalue concatenation width does not match its assignment type");
        }
        return builder.effects(std::move(effects), source);
    }

    const auto* lhs = lower_signal_expr(design, assignment.left());
    if (assignment.isCompound()) {
        design.lvalue_references.push_back(lower_expr(design, assignment.left()));
    }
    ScopeExit release_lvalue_reference([&] {
        if (assignment.isCompound()) {
            design.lvalue_references.pop_back();
        }
    });
    const auto* rhs =
        cast_to_lvalue_type(design, lower_expr(design, assignment.right()), assignment.left());
    return builder.effects({{lhs, rhs, blocking, source}}, source);
}

std::optional<bool>
constant_boolean_value(ModuleLoweringContext& design, const Expression& expression) {
    if (expression.kind == ExpressionKind::Invalid) {
        auto* child = expression.as<InvalidExpression>().child;
        return child ? constant_boolean_value(design, *child) : std::nullopt;
    }
    if (expression.kind == ExpressionKind::Conversion && !expression.type->isIntegral()) {
        return constant_boolean_value(design, expression.as<ConversionExpression>().operand());
    }
    if (expression.kind == ExpressionKind::IntegerLiteral ||
        expression.kind == ExpressionKind::UnbasedUnsizedIntegerLiteral) {
        auto value = expression.kind == ExpressionKind::IntegerLiteral
                         ? expression.as<IntegerLiteral>().getValue()
                         : expression.as<UnbasedUnsizedIntegerLiteral>().getValue();
        auto truth = static_cast<logic_t>(value);
        return truth.isUnknown() ? std::nullopt : std::optional<bool>(static_cast<bool>(truth));
    }
    if (expression.kind == ExpressionKind::MemberAccess) {
        const auto& access = expression.as<MemberAccessExpression>();
        EvalContext context(access.member);
        auto value = expression.eval(context);
        if (value && value.isInteger() && !value.integer().hasUnknown()) {
            return value.isTrue();
        }
    }
    if (expression.kind == ExpressionKind::BinaryOp) {
        const auto& binary = expression.as<BinaryExpression>();
        if (binary.op == BinaryOperator::LogicalAnd || binary.op == BinaryOperator::LogicalOr) {
            auto left = constant_boolean_value(design, binary.left());
            if ((binary.op == BinaryOperator::LogicalAnd && left == false) ||
                (binary.op == BinaryOperator::LogicalOr && left == true)) {
                return left;
            }
            auto right = constant_boolean_value(design, binary.right());
            if ((binary.op == BinaryOperator::LogicalAnd && right == false) ||
                (binary.op == BinaryOperator::LogicalOr && right == true)) {
                return right;
            }
            if (left && right) {
                return binary.op == BinaryOperator::LogicalAnd ? *left && *right : *left || *right;
            }
            return std::nullopt;
        }
    }
    if (auto* value = expression.getConstant();
        value && value->isInteger() && !value->integer().hasUnknown()) {
        return value->isTrue();
    }
    auto value = evaluate_lowering_constant(design, expression);
    if (value && value.isInteger() && !value.integer().hasUnknown()) {
        return value.isTrue();
    }
    return std::nullopt;
}

CfgFragment lower_conditional_statement(
    ProcedureBuilder& builder,
    ModuleLoweringContext& design,
    const ConditionalStatement& statement,
    OptoSlangProcedureKind procedure_kind) {
    if (statement.conditions.empty()) {
        throw std::runtime_error("if statement without a condition is not supported");
    }

    std::vector<const Expression*> runtime_conditions;
    bool all_constant_true = true;
    for (const auto& condition : statement.conditions) {
        if (condition.pattern) {
            throw std::runtime_error("pattern conditions are not supported in procedural if");
        }
        auto constant = constant_boolean_value(design, *condition.expr);
        if (constant == false) {
            if (statement.ifFalse) {
                return lower_statement(builder, design, *statement.ifFalse, procedure_kind);
            }
            return {};
        }
        if (constant == true) {
            continue;
        }
        all_constant_true = false;
        runtime_conditions.push_back(condition.expr);
    }
    if (all_constant_true) {
        return lower_statement(builder, design, statement.ifTrue, procedure_kind);
    }

    const OptoSlangExpr* combined = nullptr;
    for (auto* condition : runtime_conditions) {
        auto* lowered = lower_boolean_context(design, *condition);
        combined = combined
                       ? make_binary_expr(
                             design,
                             OPTO_SLANG_BINARY_LOGICAL_AND,
                             combined,
                             lowered,
                             *condition)
                       : lowered;
    }
    const auto source = source_span(design, statement);
    const auto dispatch = builder.add_block(source);
    auto then_branch = lower_statement(builder, design, statement.ifTrue, procedure_kind);
    CfgFragment else_branch;
    if (statement.ifFalse) {
        else_branch = lower_statement(builder, design, *statement.ifFalse, procedure_kind);
    }
    const auto join = builder.add_block(source);
    builder.branch(
        dispatch,
        combined,
        then_branch.empty() ? join : *then_branch.entry,
        else_branch.empty() ? join : *else_branch.entry,
        source);
    if (!then_branch.empty()) {
        for (auto exit : then_branch.exits) {
            builder.jump(exit, join, source);
        }
    }
    if (!else_branch.empty()) {
        for (auto exit : else_branch.exits) {
            builder.jump(exit, join, source);
        }
    }
    return {dispatch, {join}};
}

struct PriorityArm {
    const OptoSlangExpr* condition;
    uint32_t dispatch;
    CfgFragment body;
};

CfgFragment finish_priority_case(
    ProcedureBuilder& builder,
    std::vector<PriorityArm> arms,
    CfgFragment default_body,
    OptoSlangSourceSpanView source) {
    if (arms.empty()) {
        return default_body;
    }
    const auto join = builder.add_block(source);
    const auto default_target = default_body.empty() ? join : *default_body.entry;
    for (size_t index = 0; index < arms.size(); ++index) {
        const auto false_target =
            index + 1 < arms.size() ? arms[index + 1].dispatch : default_target;
        const auto true_target = arms[index].body.empty() ? join : *arms[index].body.entry;
        builder.branch(
            arms[index].dispatch, arms[index].condition, true_target, false_target, source);
        for (auto exit : arms[index].body.exits) {
            builder.jump(exit, join, source);
        }
    }
    for (auto exit : default_body.exits) {
        builder.jump(exit, join, source);
    }
    return {arms.front().dispatch, {join}};
}

CfgFragment lower_case_statement(
    ProcedureBuilder& builder,
    ModuleLoweringContext& design,
    const CaseStatement& statement,
    OptoSlangProcedureKind procedure_kind) {
    if (statement.condition == CaseStatementCondition::WildcardXOrZ) {
        throw std::runtime_error("casex is not supported for synthesis lowering");
    }
    const auto source = source_span(design, statement);
    if (statement.condition == CaseStatementCondition::Inside) {
        auto* selector = lower_expr(design, statement.expr);
        std::vector<PriorityArm> arms;
        arms.reserve(statement.items.size());
        for (const auto& item : statement.items) {
            OptoSlangExpr* condition = nullptr;
            for (auto* expression : item.expressions) {
                if (!expression) {
                    throw std::runtime_error("case inside item contains a null pattern");
                }
                auto* matched = lower_inside_item_match(design, selector, *expression);
                condition =
                    condition
                        ? make_binary_expr(
                              design, OPTO_SLANG_BINARY_LOGICAL_OR, condition, matched, *expression)
                        : matched;
            }
            if (!condition) {
                throw std::runtime_error("case inside item has no match patterns");
            }
            const auto dispatch = builder.add_block(source_span(design, *item.stmt));
            arms.push_back(
                {
                    condition,
                    dispatch,
                    lower_statement(builder, design, *item.stmt, procedure_kind),
                });
        }
        if (arms.empty()) {
            throw std::runtime_error("case inside statement has no selectable items");
        }
        CfgFragment default_body;
        if (statement.defaultCase) {
            default_body =
                lower_statement(builder, design, *statement.defaultCase, procedure_kind);
        }
        return finish_priority_case(builder, std::move(arms), std::move(default_body), source);
    }
    if (statement.condition == CaseStatementCondition::WildcardJustZ) {
        auto* selector = lower_expr(design, statement.expr);
        const auto selector_width =
            checked_width(statement.expr.type->getBitstreamWidth(), "casez selector");
        std::vector<PriorityArm> arms;
        arms.reserve(statement.items.size());
        for (const auto& item : statement.items) {
            const OptoSlangExpr* condition = nullptr;
            for (auto* expression : item.expressions) {
                if (!expression) {
                    throw std::runtime_error("casez item contains a null pattern");
                }
                auto* pattern = lower_expr(design, *expression);
                if (pattern->kind != OPTO_SLANG_EXPR_CONSTANT || !pattern->constant_has_width ||
                    pattern->constant_width != selector_width ||
                    pattern->constant_bits.size() != selector_width) {
                    throw std::runtime_error(
                        "casez patterns must elaborate to selector-width constants");
                }
                std::string care_mask;
                std::string cared_value;
                care_mask.reserve(selector_width);
                cared_value.reserve(selector_width);
                for (char bit : pattern->constant_bits) {
                    if (bit == 'z' || bit == 'Z') {
                        care_mask.push_back('0');
                        cared_value.push_back('0');
                    } else if (bit == '0' || bit == '1') {
                        care_mask.push_back('1');
                        cared_value.push_back(bit);
                    } else {
                        throw std::runtime_error(
                            "casez pattern contains an X bit that "
                            "cannot be synthesized exactly");
                    }
                }
                OptoSlangExpr mask_expr;
                mask_expr.kind = OPTO_SLANG_EXPR_CONSTANT;
                mask_expr.constant_has_width = true;
                mask_expr.constant_width = selector_width;
                mask_expr.constant_bits = std::move(care_mask);
                auto* mask = make_expr(design, std::move(mask_expr), *expression);
                OptoSlangExpr value_expr;
                value_expr.kind = OPTO_SLANG_EXPR_CONSTANT;
                value_expr.constant_has_width = true;
                value_expr.constant_width = selector_width;
                value_expr.constant_bits = std::move(cared_value);
                auto* value = make_expr(design, std::move(value_expr), *expression);
                auto* masked_selector = make_binary_expr(
                    design, OPTO_SLANG_BINARY_BIT_AND, selector, mask, *expression);
                auto* matched = make_binary_expr(
                    design, OPTO_SLANG_BINARY_EQ, masked_selector, value, *expression);
                condition =
                    condition
                        ? make_binary_expr(
                              design, OPTO_SLANG_BINARY_LOGICAL_OR, condition, matched, *expression)
                        : matched;
            }
            if (!condition) {
                throw std::runtime_error("casez item has no match patterns");
            }
            const auto dispatch = builder.add_block(source_span(design, *item.stmt));
            arms.push_back(
                {
                    condition,
                    dispatch,
                    lower_statement(builder, design, *item.stmt, procedure_kind),
                });
        }
        if (arms.empty()) {
            throw std::runtime_error("casez statement has no selectable items");
        }
        CfgFragment default_body;
        if (statement.defaultCase) {
            default_body =
                lower_statement(builder, design, *statement.defaultCase, procedure_kind);
        }
        return finish_priority_case(builder, std::move(arms), std::move(default_body), source);
    }

    const auto* selector = lower_expr(design, statement.expr);
    const auto dispatch = builder.add_block(source);
    struct SwitchBody {
        std::vector<const OptoSlangExpr*> patterns;
        CfgFragment body;
    };
    std::vector<SwitchBody> bodies;
    bodies.reserve(statement.items.size());
    for (const auto& item : statement.items) {
        auto& patterns = bodies.emplace_back().patterns;
        patterns.reserve(item.expressions.size());
        for (const auto* expression : item.expressions) {
            if (!expression) {
                throw std::runtime_error("case item contains a null match expression");
            }
            patterns.push_back(lower_expr(design, *expression));
        }
        bodies.back().body = lower_statement(builder, design, *item.stmt, procedure_kind);
    }
    CfgFragment default_body;
    if (statement.defaultCase) {
        default_body = lower_statement(builder, design, *statement.defaultCase, procedure_kind);
    }
    const auto join = builder.add_block(source);
    std::vector<OptoSlangSwitchArmData> arms;
    for (auto& body : bodies) {
        const auto target = body.body.empty() ? join : *body.body.entry;
        for (auto* pattern : body.patterns) {
            arms.push_back({pattern, {target, source}});
        }
        for (auto exit : body.body.exits) {
            builder.jump(exit, join, source);
        }
    }
    if (arms.empty()) {
        builder.jump(dispatch, default_body.empty() ? join : *default_body.entry, source);
    } else {
        builder.switch_(
            dispatch,
            selector,
            std::move(arms),
            default_body.empty() ? join : *default_body.entry,
            source);
    }
    for (auto exit : default_body.exits) {
        builder.jump(exit, join, source);
    }
    return {dispatch, {join}};
}

bool statement_contains_return(const Statement& statement) {
    switch (statement.kind) {
    case StatementKind::Return:
        return true;
    case StatementKind::Block:
        return statement_contains_return(statement.as<BlockStatement>().body);
    case StatementKind::List:
        return std::ranges::any_of(statement.as<StatementList>().list, [](const Statement* child) {
            return child && statement_contains_return(*child);
        });
    case StatementKind::Conditional: {
        const auto& conditional = statement.as<ConditionalStatement>();
        return statement_contains_return(conditional.ifTrue) ||
               (conditional.ifFalse && statement_contains_return(*conditional.ifFalse));
    }
    case StatementKind::Case: {
        const auto& case_statement = statement.as<CaseStatement>();
        return std::ranges::any_of(
                   case_statement.items,
                   [](const auto& item) {
                       return item.stmt && statement_contains_return(*item.stmt);
                   }) ||
               (case_statement.defaultCase &&
                statement_contains_return(*case_statement.defaultCase));
    }
    case StatementKind::ForLoop:
        return statement_contains_return(statement.as<ForLoopStatement>().body);
    default:
        return false;
    }
}

bool statement_contains_break(const Statement& statement) {
    switch (statement.kind) {
    case StatementKind::Break:
        return true;
    case StatementKind::Block:
        return statement_contains_break(statement.as<BlockStatement>().body);
    case StatementKind::List:
        return std::ranges::any_of(statement.as<StatementList>().list, [](const Statement* child) {
            return child && statement_contains_break(*child);
        });
    case StatementKind::Conditional: {
        const auto& conditional = statement.as<ConditionalStatement>();
        return statement_contains_break(conditional.ifTrue) ||
               (conditional.ifFalse && statement_contains_break(*conditional.ifFalse));
    }
    case StatementKind::Case: {
        const auto& case_statement = statement.as<CaseStatement>();
        return std::ranges::any_of(
                   case_statement.items,
                   [](const auto& item) {
                       return item.stmt && statement_contains_break(*item.stmt);
                   }) ||
               (case_statement.defaultCase &&
                statement_contains_break(*case_statement.defaultCase));
    }
    case StatementKind::ForLoop:
        return false;
    default:
        return false;
    }
}

std::vector<const VariableSymbol*> procedural_for_variables(const ForLoopStatement& loop) {
    if (!loop.loopVars.empty()) {
        return {loop.loopVars.begin(), loop.loopVars.end()};
    }
    std::vector<const VariableSymbol*> variables;
    variables.reserve(loop.initializers.size());
    for (auto* initializer : loop.initializers) {
        if (!initializer || initializer->kind != ExpressionKind::Assignment) {
            throw std::runtime_error("procedural for initializer must assign a loop variable");
        }
        const auto& lhs = initializer->as<AssignmentExpression>().left();
        if (lhs.kind != ExpressionKind::NamedValue ||
            lhs.as<NamedValueExpression>().symbol.kind != SymbolKind::Variable) {
            throw std::runtime_error("procedural for initializer target must be a variable");
        }
        const auto* variable = &lhs.as<NamedValueExpression>().symbol.as<VariableSymbol>();
        if (std::ranges::find(variables, variable) == variables.end()) {
            variables.push_back(variable);
        }
    }
    return variables;
}

bool statement_requires_return_control(const Statement& statement) {
    switch (statement.kind) {
    case StatementKind::Block:
        return statement_requires_return_control(statement.as<BlockStatement>().body);
    case StatementKind::List: {
        bool prior_return = false;
        for (auto* child : statement.as<StatementList>().list) {
            if (!child || child->kind == StatementKind::Empty) {
                continue;
            }
            if (prior_return || statement_requires_return_control(*child)) {
                return true;
            }
            prior_return = statement_contains_return(*child);
        }
        return false;
    }
    case StatementKind::Conditional: {
        const auto& conditional = statement.as<ConditionalStatement>();
        return statement_requires_return_control(conditional.ifTrue) ||
               (conditional.ifFalse && statement_requires_return_control(*conditional.ifFalse));
    }
    case StatementKind::Case: {
        const auto& case_statement = statement.as<CaseStatement>();
        return std::ranges::any_of(
                   case_statement.items,
                   [](const auto& item) {
                       return item.stmt && statement_requires_return_control(*item.stmt);
                   }) ||
               (case_statement.defaultCase &&
                statement_requires_return_control(*case_statement.defaultCase));
    }
    case StatementKind::ForLoop:
        return statement_contains_return(statement.as<ForLoopStatement>().body) ||
               statement_requires_return_control(statement.as<ForLoopStatement>().body);
    default:
        return false;
    }
}

CfgFragment guard_unreturned_statements(
    ProcedureBuilder& builder,
    ModuleLoweringContext& design,
    CfgFragment body,
    OptoSlangSourceSpanView source) {
    if (body.empty() || design.function_return_controls.empty()) {
        return body;
    }
    const auto& control = design.function_return_controls.back();
    if (!control.not_returned) {
        return body;
    }
    return builder.guard(control.not_returned, std::move(body), source);
}

CfgFragment guard_unbroken_statements(
    ProcedureBuilder& builder,
    ModuleLoweringContext& design,
    CfgFragment body,
    OptoSlangSourceSpanView source) {
    if (body.empty() || design.loop_controls.empty()) {
        return body;
    }
    const auto& control = design.loop_controls.back();
    if (!control.not_broken) {
        return body;
    }
    return builder.guard(control.not_broken, std::move(body), source);
}

CfgFragment lower_statement_list(
    ProcedureBuilder& builder,
    ModuleLoweringContext& design,
    std::span<const Statement* const> statements,
    OptoSlangProcedureKind procedure_kind) {
    std::vector<const VariableSymbol*> registered_loop_variables;
    for (const auto* child : statements) {
        if (!child || child->kind != StatementKind::ForLoop) {
            continue;
        }
        for (auto* variable : procedural_for_variables(child->as<ForLoopStatement>())) {
            if (variable && design.procedural_loop_variables.insert(variable).second) {
                registered_loop_variables.push_back(variable);
            }
        }
    }
    CfgFragment lowered;
    bool prior_return = false;
    bool prior_break = false;
    ScopeExit unregister_loop_variables([&] {
        for (auto* variable : registered_loop_variables) {
            design.procedural_loop_variables.erase(variable);
            design.procedural_constants.erase(variable);
        }
    });
    for (const auto* child : statements) {
        if (!child) {
            continue;
        }
        auto child_lowered = lower_statement(builder, design, *child, procedure_kind);
        const auto source = source_span(design, *child);
        if (prior_return) {
            child_lowered = guard_unreturned_statements(
                builder, design, std::move(child_lowered), source);
        }
        if (prior_break) {
            child_lowered =
                guard_unbroken_statements(builder, design, std::move(child_lowered), source);
        }
        lowered = builder.sequence(std::move(lowered), std::move(child_lowered), source);
        prior_return = prior_return || statement_contains_return(*child);
        prior_break = prior_break || statement_contains_break(*child);
    }
    return lowered;
}

OptoSlangEffectData lower_loop_variable_assignment(
    ModuleLoweringContext& design,
    const VariableSymbol& variable,
    const ConstantValue& value,
    const Expression& source) {
    if (!value.isInteger()) {
        throw std::runtime_error("procedural loop variable is not integral");
    }
    OptoSlangExpr lhs;
    lhs.kind = OPTO_SLANG_EXPR_SIGNAL;
    lhs.signal_name = intern_string(design, registered_value_name(design, variable));
    OptoSlangExpr rhs;
    rhs.kind = OPTO_SLANG_EXPR_CONSTANT;
    auto bits = value.integer().resize(variable.getType().getBitstreamWidth());
    bits.setSigned(variable.getType().isSigned());
    rhs.constant_has_width = true;
    rhs.constant_width = checked_width(bits.getBitWidth(), variable.name);
    rhs.constant_bits = exact_binary_string(bits);
    const auto* lowered_lhs = make_expr(design, std::move(lhs), source);
    auto* lowered_rhs = make_expr(design, std::move(rhs), source);
    lowered_rhs->constant_signed = variable.getType().isSigned();
    return {lowered_lhs, lowered_rhs, true, source_span(design, source)};
}

CfgFragment lower_for_loop(
    ProcedureBuilder& builder,
    ModuleLoweringContext& design,
    const ForLoopStatement& loop,
    OptoSlangProcedureKind procedure_kind) {
    auto variables = procedural_for_variables(loop);
    if (variables.empty() || !loop.stopExpr || loop.steps.empty()) {
        throw std::runtime_error(
            "procedural for loop requires variables, a stop "
            "condition, and steps at " +
            statement_location(design, loop));
    }

    EvalContext context(*variables.front());
    for (const auto& [symbol, value] : design.procedural_constants) {
        context.createLocal(symbol, value);
    }
    if (!loop.loopVars.empty()) {
        for (auto* variable : variables) {
            auto* initializer = variable->getInitializer();
            if (!initializer) {
                throw std::runtime_error(
                    "procedural for loop variable requires a constant initializer");
            }
            auto value = initializer->eval(context);
            if (!value || !value.isInteger() || value.integer().hasUnknown()) {
                throw std::runtime_error(
                    "procedural for loop initializer is not a known integral constant");
            }
            context.createLocal(variable, std::move(value));
        }
    } else {
        for (auto* initializer : loop.initializers) {
            const auto& assignment = initializer->as<AssignmentExpression>();
            const auto* variable =
                &assignment.left().as<NamedValueExpression>().symbol.as<VariableSymbol>();
            auto value = assignment.right().eval(context);
            if (!value || !value.isInteger() || value.integer().hasUnknown()) {
                throw std::runtime_error(
                    "procedural for initializer is not a known integral constant");
            }
            context.createLocal(variable, std::move(value));
        }
    }

    const auto source = source_span(design, loop);
    CfgFragment lowered;
    const bool body_contains_return = statement_contains_return(loop.body);
    bool prior_iteration_may_return = false;
    const bool body_contains_break = statement_contains_break(loop.body);
    const bool preserve_loop_signal = body_contains_break;
    LoopControl loop_control;
    if (body_contains_break) {
        const auto ordinal = design.next_loop_instance++;
        auto flag_name = add_internal_net(
            design, "__opto_loop_" + std::to_string(ordinal) + "_broken", 1, false);
        OptoSlangExpr broken;
        broken.kind = OPTO_SLANG_EXPR_SIGNAL;
        broken.signal_name = intern_string(design, flag_name);
        loop_control.broken = make_expr(design, std::move(broken), *loop.stopExpr);
        loop_control.not_broken = make_unary_expr(
            design, OPTO_SLANG_UNARY_LOGICAL_NOT, loop_control.broken, *loop.stopExpr);
        OptoSlangExpr false_value;
        false_value.kind = OPTO_SLANG_EXPR_CONSTANT;
        false_value.constant_has_width = true;
        false_value.constant_width = 1;
        false_value.constant_bits = "0";
        OptoSlangExpr true_value;
        true_value.kind = OPTO_SLANG_EXPR_CONSTANT;
        true_value.constant_has_width = true;
        true_value.constant_width = 1;
        true_value.constant_bits = "1";
        loop_control.true_value = make_expr(design, std::move(true_value), *loop.stopExpr);
        lowered = builder.effects(
            {{
                loop_control.broken,
                make_expr(design, std::move(false_value), *loop.stopExpr),
                true,
                source,
            }},
            source);
    }
    design.loop_controls.push_back(loop_control);
    ScopedValue evaluation_context(design.eval_context);
    ScopeExit leave_loop([&] {
        if (preserve_loop_signal) {
            for (auto* variable : variables) {
                design.procedural_constants.erase(variable);
            }
        }
        design.loop_controls.pop_back();
    });
    std::unordered_set<std::string> seen_states;
    bool had_iteration = false;
    while (true) {
        auto stop = loop.stopExpr->eval(context);
        if (!stop || !stop.isInteger() || stop.integer().hasUnknown()) {
            throw std::runtime_error(
                "procedural for loop stop condition is not a "
                "known integral constant at " +
                expression_location(design, *loop.stopExpr));
        }
        if (!stop.isTrue()) {
            if (!preserve_loop_signal) {
                for (auto* variable : variables) {
                    auto* value = context.findLocal(variable);
                    if (!value) {
                        throw std::runtime_error("procedural loop variable lost its final value");
                    }
                    design.procedural_constants.insert_or_assign(variable, *value);
                }
            } else if (!loop.initializers.empty()) {
                std::vector<OptoSlangEffectData> final_values;
                for (auto* variable : variables) {
                    auto* value = context.findLocal(variable);
                    if (!value) {
                        throw std::runtime_error("procedural loop variable lost its final value");
                    }
                    final_values.push_back(
                        lower_loop_variable_assignment(design, *variable, *value, *loop.stopExpr));
                }
                auto final_fragment = builder.effects(std::move(final_values), source);
                if (body_contains_break) {
                    final_fragment = guard_unbroken_statements(
                        builder, design, std::move(final_fragment), source);
                }
                lowered =
                    builder.sequence(std::move(lowered), std::move(final_fragment), source);
            }
            return lowered;
        }

        std::string state;
        std::vector<OptoSlangEffectData> iteration_values;
        for (auto* variable : variables) {
            auto* value = context.findLocal(variable);
            if (!value) {
                throw std::runtime_error("procedural for loop variable lost its evaluation value");
            }
            if (!value->isInteger()) {
                throw std::runtime_error("procedural loop variable is not integral");
            }
            state.append(exact_binary_string(value->integer()));
            state.push_back(';');
            design.procedural_constants.insert_or_assign(variable, *value);
            if (preserve_loop_signal && !loop.initializers.empty()) {
                iteration_values.push_back(
                    lower_loop_variable_assignment(design, *variable, *value, *loop.stopExpr));
            }
        }
        if (!seen_states.insert(std::move(state)).second) {
            throw std::runtime_error(
                "procedural for loop repeats an evaluation state and does not terminate at " +
                statement_location(design, loop));
        }
        design.eval_context = &context;
        auto iteration = builder.effects(std::move(iteration_values), source);
        iteration = builder.sequence(
            std::move(iteration),
            lower_statement(builder, design, loop.body, procedure_kind),
            source);
        if (prior_iteration_may_return) {
            iteration =
                guard_unreturned_statements(builder, design, std::move(iteration), source);
        }
        if (body_contains_break && had_iteration) {
            iteration = guard_unbroken_statements(builder, design, std::move(iteration), source);
        }
        lowered = builder.sequence(std::move(lowered), std::move(iteration), source);
        prior_iteration_may_return = prior_iteration_may_return || body_contains_return;
        had_iteration = true;

        for (auto* step : loop.steps) {
            if (!step || !step->eval(context)) {
                throw std::runtime_error("procedural for loop step could not be evaluated");
            }
        }
    }
}

CfgFragment lower_statement_impl(
    ProcedureBuilder& builder,
    ModuleLoweringContext& design,
    const Statement& stmt,
    OptoSlangProcedureKind procedure_kind) {
    switch (stmt.kind) {
    case StatementKind::Invalid: {
        auto* child = stmt.as<InvalidStatement>().child;
        if (!child) {
            throw std::runtime_error("invalid statement at " + statement_location(design, stmt));
        }
        return lower_statement(builder, design, *child, procedure_kind);
    }
    case StatementKind::Empty:
        return {};
    case StatementKind::List:
        return lower_statement_list(
            builder, design, stmt.as<StatementList>().list, procedure_kind);
    case StatementKind::Block:
        return lower_statement(builder, design, stmt.as<BlockStatement>().body, procedure_kind);
    case StatementKind::VariableDeclaration: {
        const auto& symbol = stmt.as<VariableDeclStatement>().symbol;
        if (design.procedural_loop_variables.contains(&symbol)) {
            return {};
        }
        if (auto* initializer = symbol.getInitializer()) {
            OptoSlangExpr lhs;
            lhs.kind = OPTO_SLANG_EXPR_SIGNAL;
            lhs.signal_name = intern_string(design, registered_value_name(design, symbol));
            const auto source = source_span(design, stmt);
            return builder.effects(
                {{
                    make_expr(design, std::move(lhs), *initializer),
                    cast_to_type(
                        design, lower_expr(design, *initializer), symbol.getType(), *initializer),
                    true,
                    source,
                }},
                source);
        }
        return {};
    }
    case StatementKind::ForLoop:
        return lower_for_loop(builder, design, stmt.as<ForLoopStatement>(), procedure_kind);
    case StatementKind::Break: {
        if (design.loop_controls.empty() || !design.loop_controls.back().broken) {
            throw std::runtime_error("break statement has no active synthesizable loop");
        }
        const auto& control = design.loop_controls.back();
        const auto source = source_span(design, stmt);
        return builder.effects(
            {{control.broken, control.true_value, true, source}}, source);
    }
    case StatementKind::Return: {
        if (design.function_returns.empty()) {
            throw std::runtime_error("return statement appears outside an inlined function");
        }
        const auto& returned = stmt.as<ReturnStatement>();
        if (!returned.expr) {
            throw std::runtime_error("void return is not synthesizable as an expression");
        }
        const auto* return_variable = design.function_returns.back();
        OptoSlangExpr lhs;
        lhs.kind = OPTO_SLANG_EXPR_SIGNAL;
        lhs.signal_name = intern_string(design, registered_value_name(design, *return_variable));
        const auto source = source_span(design, stmt);
        std::vector<OptoSlangEffectData> effects;
        effects.push_back(
            {
                make_expr(design, std::move(lhs), *returned.expr),
                lower_expr(design, *returned.expr),
                true,
                source,
            });
        if (!design.function_return_controls.empty()) {
            const auto& control = design.function_return_controls.back();
            if (control.returned) {
                effects.push_back({control.returned, control.true_value, true, source});
            }
        }
        return builder.effects(std::move(effects), source);
    }
    case StatementKind::Conditional:
        return lower_conditional_statement(
            builder, design, stmt.as<ConditionalStatement>(), procedure_kind);
    case StatementKind::Case:
        return lower_case_statement(builder, design, stmt.as<CaseStatement>(), procedure_kind);
    case StatementKind::ExpressionStatement: {
        const auto& expr = stmt.as<ExpressionStatement>().expr;
        if (expr.kind == ExpressionKind::Call && !expr.as<CallExpression>().isSystemCall()) {
            return lower_subroutine_call_statement(
                builder, design, expr.as<CallExpression>(), procedure_kind);
        }
        if (expr.kind == ExpressionKind::UnaryOp) {
            const auto& unary = expr.as<UnaryExpression>();
            const bool increment =
                unary.op == UnaryOperator::Preincrement || unary.op == UnaryOperator::Postincrement;
            const bool decrement =
                unary.op == UnaryOperator::Predecrement || unary.op == UnaryOperator::Postdecrement;
            if (!increment && !decrement) {
                throw std::runtime_error(
                    "unary expression statement '" + copy_string(toString(unary.op)) +
                    "' is not synthesizable at " + expression_location(design, expr));
            }
            const auto width = checked_width(
                unary.operand().type->getBitstreamWidth(), "increment or decrement operand");
            OptoSlangExpr one;
            one.kind = OPTO_SLANG_EXPR_CONSTANT;
            one.constant_has_width = true;
            one.constant_width = width;
            one.constant_bits.assign(width, '0');
            one.constant_bits.back() = '1';
            auto* one_value = make_expr(design, std::move(one), expr);
            const auto source = source_span(design, stmt);
            return builder.effects(
                {{
                    lower_signal_expr(design, unary.operand()),
                    make_binary_expr(
                        design,
                        increment ? OPTO_SLANG_BINARY_ADD : OPTO_SLANG_BINARY_SUB,
                        lower_expr(design, unary.operand()),
                        one_value,
                        expr),
                    true,
                    source,
                }},
                source);
        }
        if (expr.kind != ExpressionKind::Assignment) {
            throw std::runtime_error(
                "expression statement '" + copy_string(toString(expr.kind)) +
                "' is not supported in procedural blocks at " + expression_location(design, expr));
        }
        return lower_assignment_statement(
            builder, design, expr.as<AssignmentExpression>(), procedure_kind);
    }
    default: {
        std::string context;
        if (!design.function_stack.empty()) {
            context = " while inlining function '" +
                      copy_string(design.function_stack.back()->name) + "'";
        } else {
            context = " in module '" + copy_string(design.body.getDefinition().name) + "'";
        }
        throw std::runtime_error(
            "unsupported statement '" + copy_string(toString(stmt.kind)) + "' in procedural block" +
            context + " at " + statement_location(design, stmt));
    }
    }
}

CfgFragment lower_statement(
    ProcedureBuilder& builder,
    ModuleLoweringContext& design,
    const Statement& stmt,
    OptoSlangProcedureKind procedure_kind) {
    CfgFragment prelude;
    ScopedValue expression_prelude(design.active_expression_prelude, &prelude);
    ScopedValue active_builder(design.active_procedure_builder, &builder);
    auto body = lower_statement_impl(builder, design, stmt, procedure_kind);
    return builder.sequence(
        std::move(prelude), std::move(body), source_span(design, stmt));
}

OptoSlangEventData lower_flop_event(ModuleLoweringContext& design, const TimingControl& timing) {
    if (timing.kind != TimingControlKind::SignalEvent) {
        throw std::runtime_error(
            "edge-triggered procedural block event must be a "
            "posedge or negedge signal");
    }
    const auto& event = timing.as<SignalEventControl>();
    if (event.iffCondition) {
        throw std::runtime_error("iff event conditions are not supported for synthesis");
    }

    OptoSlangEdge edge;
    switch (event.edge) {
    case EdgeKind::PosEdge:
        edge = OPTO_SLANG_EDGE_POS;
        break;
    case EdgeKind::NegEdge:
        edge = OPTO_SLANG_EDGE_NEG;
        break;
    default:
        throw std::runtime_error(
            "edge-triggered procedural block event must be a "
            "posedge or negedge signal");
    }
    return OptoSlangEventData{
        edge,
        lower_signal_expr(design, event.expr),
        source_span(design, event.expr),
    };
}

void lower_flop_events(
    ModuleLoweringContext& design,
    const TimingControl& timing,
    std::vector<OptoSlangEventData>& events) {
    if (timing.kind == TimingControlKind::EventList) {
        const auto& list = timing.as<EventListControl>();
        if (list.events.empty()) {
            throw std::runtime_error("edge-triggered procedural block has an empty event list");
        }
        for (auto* event : list.events) {
            if (!event) {
                throw std::runtime_error("edge-triggered procedural block has a null event");
            }
            lower_flop_events(design, *event, events);
        }
        return;
    }
    events.push_back(lower_flop_event(design, timing));
}

bool is_combinational_sensitivity(const TimingControl& timing) {
    switch (timing.kind) {
    case TimingControlKind::ImplicitEvent:
        return true;
    case TimingControlKind::SignalEvent: {
        const auto& event = timing.as<SignalEventControl>();
        return event.edge == EdgeKind::None && !event.iffCondition;
    }
    case TimingControlKind::EventList: {
        const auto& list = timing.as<EventListControl>();
        return !list.events.empty() &&
               std::ranges::all_of(list.events, [](const TimingControl* event) {
                   return event && is_combinational_sensitivity(*event);
               });
    }
    default:
        return false;
    }
}

bool is_edge_sensitivity(const TimingControl& timing) {
    if (timing.kind == TimingControlKind::EventList) {
        const auto& list = timing.as<EventListControl>();
        return !list.events.empty() &&
               std::ranges::all_of(list.events, [](const TimingControl* event) {
                   return event && is_edge_sensitivity(*event);
               });
    }
    if (timing.kind != TimingControlKind::SignalEvent) {
        return false;
    }
    const auto& event = timing.as<SignalEventControl>();
    return !event.iffCondition &&
           (event.edge == EdgeKind::PosEdge || event.edge == EdgeKind::NegEdge);
}

OptoSlangProcedureData lower_procedure(
    ModuleLoweringContext& design,
    const InstanceBodySymbol& body,
    const ProceduralBlockSymbol& process) {
    ProcedureBuilder builder;
    OptoSlangProcedureKind kind;
    std::vector<OptoSlangEventData> events;
    const Statement* statement = nullptr;
    if (process.procedureKind == ProceduralBlockKind::AlwaysComb) {
        kind = OPTO_SLANG_PROCEDURE_COMB;
        statement = &process.getBody();
    } else if (process.procedureKind == ProceduralBlockKind::AlwaysLatch) {
        kind = OPTO_SLANG_PROCEDURE_LATCH;
        statement = &process.getBody();
    } else if (process.procedureKind == ProceduralBlockKind::AlwaysFF) {
        kind = OPTO_SLANG_PROCEDURE_FLOP;
        if (process.getBody().kind != StatementKind::Timed) {
            throw std::runtime_error(
                "edge-triggered procedural block requires a "
                "posedge or negedge event list");
        }
        const auto& timed = process.getBody().as<TimedStatement>();
        lower_flop_events(design, timed.timing, events);
        statement = &timed.stmt;
    } else if (process.procedureKind == ProceduralBlockKind::Always) {
        if (process.getBody().kind != StatementKind::Timed) {
            throw std::runtime_error(
                "always procedural block requires an event control for synthesis");
        }
        const auto& timed = process.getBody().as<TimedStatement>();
        if (is_edge_sensitivity(timed.timing)) {
            kind = OPTO_SLANG_PROCEDURE_FLOP;
            lower_flop_events(design, timed.timing, events);
        } else if (is_combinational_sensitivity(timed.timing)) {
            kind = OPTO_SLANG_PROCEDURE_COMB_OR_LATCH;
        } else {
            throw std::runtime_error(
                "always event control is not a supported "
                "combinational or edge sensitivity list");
        }
        statement = &timed.stmt;
    } else {
        throw std::runtime_error(unsupported_member_message(body, process));
    }
    const auto source = source_span(design, *statement);
    return builder.finish(
        lower_statement(builder, design, *statement, kind), kind, std::move(events), source);
}
void collect_function_locals(const Scope& scope, std::vector<const VariableSymbol*>& locals) {
    for (const auto& member : scope.members()) {
        if (member.kind == SymbolKind::Variable) {
            locals.push_back(&member.as<VariableSymbol>());
        } else if (member.kind == SymbolKind::StatementBlock) {
            collect_function_locals(member.as<StatementBlockSymbol>(), locals);
        }
    }
}

void collect_function_loop_variables(
    const Statement& statement, std::unordered_set<const VariableSymbol*>& variables) {
    switch (statement.kind) {
    case StatementKind::Block:
        collect_function_loop_variables(statement.as<BlockStatement>().body, variables);
        break;
    case StatementKind::List:
        for (auto* child : statement.as<StatementList>().list) {
            if (child) {
                collect_function_loop_variables(*child, variables);
            }
        }
        break;
    case StatementKind::Conditional: {
        const auto& conditional = statement.as<ConditionalStatement>();
        collect_function_loop_variables(conditional.ifTrue, variables);
        if (conditional.ifFalse) {
            collect_function_loop_variables(*conditional.ifFalse, variables);
        }
        break;
    }
    case StatementKind::Case: {
        const auto& case_statement = statement.as<CaseStatement>();
        for (const auto& item : case_statement.items) {
            if (item.stmt) {
                collect_function_loop_variables(*item.stmt, variables);
            }
        }
        if (case_statement.defaultCase) {
            collect_function_loop_variables(*case_statement.defaultCase, variables);
        }
        break;
    }
    case StatementKind::ForLoop: {
        const auto& loop = statement.as<ForLoopStatement>();
        for (auto* variable : loop.loopVars) {
            if (variable) {
                variables.insert(variable);
            }
        }
        collect_function_loop_variables(loop.body, variables);
        break;
    }
    default:
        break;
    }
}

const ValueSymbol* expression_root_value(const Expression& expression) {
    switch (expression.kind) {
    case ExpressionKind::NamedValue:
        return &expression.as<NamedValueExpression>().symbol;
    case ExpressionKind::ElementSelect:
        return expression_root_value(expression.as<ElementSelectExpression>().value());
    case ExpressionKind::RangeSelect:
        return expression_root_value(expression.as<RangeSelectExpression>().value());
    case ExpressionKind::MemberAccess:
        return expression_root_value(expression.as<MemberAccessExpression>().value());
    default:
        return nullptr;
    }
}

bool statement_assigns_value(const Statement& statement, const ValueSymbol& value) {
    switch (statement.kind) {
    case StatementKind::Block:
        return statement_assigns_value(statement.as<BlockStatement>().body, value);
    case StatementKind::List:
        return std::ranges::any_of(statement.as<StatementList>().list, [&](const Statement* child) {
            return child && statement_assigns_value(*child, value);
        });
    case StatementKind::Conditional: {
        const auto& conditional = statement.as<ConditionalStatement>();
        return statement_assigns_value(conditional.ifTrue, value) ||
               (conditional.ifFalse && statement_assigns_value(*conditional.ifFalse, value));
    }
    case StatementKind::Case: {
        const auto& case_statement = statement.as<CaseStatement>();
        return std::ranges::any_of(
                   case_statement.items,
                   [&](const auto& item) {
                       return item.stmt && statement_assigns_value(*item.stmt, value);
                   }) ||
               (case_statement.defaultCase &&
                statement_assigns_value(*case_statement.defaultCase, value));
    }
    case StatementKind::ForLoop:
        return statement_assigns_value(statement.as<ForLoopStatement>().body, value);
    case StatementKind::ExpressionStatement: {
        const auto& expression = statement.as<ExpressionStatement>().expr;
        if (expression.kind == ExpressionKind::Assignment) {
            return expression_root_value(expression.as<AssignmentExpression>().left()) == &value;
        }
        if (expression.kind == ExpressionKind::UnaryOp) {
            const auto& unary = expression.as<UnaryExpression>();
            return (unary.op == UnaryOperator::Preincrement ||
                    unary.op == UnaryOperator::Postincrement ||
                    unary.op == UnaryOperator::Predecrement ||
                    unary.op == UnaryOperator::Postdecrement) &&
                   expression_root_value(unary.operand()) == &value;
        }
        return false;
    }
    default:
        return false;
    }
}

bool module_has_value_name(const OptoSlangModulePayload& module, std::string_view name) {
    return std::ranges::any_of(
               module.ports, [name](const auto& port) { return port.name == name; }) ||
           std::ranges::any_of(module.nets, [name](const auto& net) { return net.name == name; });
}

std::string allocate_function_value_name(
    ModuleLoweringContext& design, const SubroutineSymbol& function, std::string_view local) {
    while (true) {
        const auto ordinal = design.next_function_instance++;
        auto name = "__opto_fn_" + std::to_string(ordinal) + "_" + copy_string(function.name) +
                    "_" + copy_string(local);
        if (!module_has_value_name(design.module, name)) {
            return name;
        }
    }
}

OptoSlangExpr* lower_function_call(ModuleLoweringContext& design, const CallExpression& call) {
    const auto* selected = std::get_if<const SubroutineSymbol*>(&call.subroutine);
    if (!selected || !*selected) {
        throw std::runtime_error(
            "unsupported non-system call '" + copy_string(call.getSubroutineName()) + "'");
    }
    const auto& function = **selected;
    if (function.subroutineKind != SubroutineKind::Function || function.hasOutputArgs() ||
        function.isVirtual() || function.flags.has(MethodFlags::DPIImport | MethodFlags::BuiltIn)) {
        throw std::runtime_error(
            "subroutine '" + copy_string(function.name) +
            "' is not a synthesizable input-only function");
    }
    auto arguments = function.getArguments();
    auto actuals = call.arguments();
    if (arguments.size() != actuals.size()) {
        throw std::runtime_error(
            "function call argument count does not match its "
            "elaborated declaration");
    }
    std::vector<OptoSlangExpr*> lowered_actuals;
    std::vector<ConstantValue> constant_actuals;
    lowered_actuals.reserve(actuals.size());
    constant_actuals.reserve(actuals.size());
    for (auto* actual : actuals) {
        if (!actual) {
            throw std::runtime_error("function call contains an unbound argument");
        }
        lowered_actuals.push_back(lower_expr(design, *actual));
        constant_actuals.push_back(evaluate_lowering_constant(design, *actual));
    }
    constexpr size_t max_recursive_depth = 256;
    if (static_cast<size_t>(std::ranges::count(design.function_stack, &function)) >=
        max_recursive_depth) {
        throw std::runtime_error(
            "recursive synthesizable function '" + copy_string(function.name) +
            "' does not reach a constant base case within " +
            std::to_string(max_recursive_depth) + " calls");
    }
    const bool process_local = design.active_procedure_builder != nullptr;
    const auto source = source_span(design, call);
    const auto* return_variable = function.returnValVar;
    if (!return_variable) {
        throw std::runtime_error(
            "function '" + copy_string(function.name) + "' has no return variable");
    }
    std::vector<const VariableSymbol*> locals;
    collect_function_locals(function, locals);
    std::unordered_set<const VariableSymbol*> loop_variables;
    collect_function_loop_variables(function.getBody(), loop_variables);
    ScopedSymbolMapBindings function_value_bindings(design.function_values);
    ScopedSymbolMapBindings constant_bindings(design.procedural_constants);
    ScopedSymbolMapBindings name_bindings(design.value_names);
    for (auto* argument : arguments) {
        function_value_bindings.track(argument);
        constant_bindings.track(argument);
        name_bindings.track(argument);
    }
    function_value_bindings.track(return_variable);
    constant_bindings.track(return_variable);
    name_bindings.track(return_variable);
    for (auto* local : locals) {
        function_value_bindings.track(local);
        constant_bindings.track(local);
        name_bindings.track(local);
    }
    for (auto* variable : loop_variables) {
        function_value_bindings.track(variable);
        constant_bindings.track(variable);
        name_bindings.track(variable);
    }
    design.function_stack.push_back(&function);

    std::vector<const ValueSymbol*> installed_values;
    std::vector<const ValueSymbol*> installed_constants;
    std::vector<const ValueSymbol*> installed_names;
    std::vector<OptoSlangEffectData> argument_initializers;
    std::string return_name;
    bool return_scope_pushed = false;
    ScopeExit leave_function([&] {
        if (return_scope_pushed) {
            design.function_returns.pop_back();
            design.function_return_controls.pop_back();
        }
        for (auto* symbol : installed_values) {
            design.function_values.erase(symbol);
        }
        for (auto* symbol : installed_constants) {
            design.procedural_constants.erase(symbol);
        }
        for (auto* symbol : installed_names) {
            design.value_names.erase(symbol);
        }
        design.function_stack.pop_back();
    });

    for (size_t index = 0; index < arguments.size(); ++index) {
        auto* argument = arguments[index];
        if (!argument || argument->direction != ArgumentDirection::In) {
            throw std::runtime_error("synthesizable function arguments must all be inputs");
        }
        const bool is_written = statement_assigns_value(function.getBody(), *argument);
        if (is_written) {
            auto name = allocate_function_value_name(design, function, argument->name);
            name = add_internal_net(
                design,
                std::move(name),
                checked_width(argument->getType().getBitstreamWidth(), argument->name),
                argument->getType().isSigned(),
                process_local);
            design.value_names.insert_or_assign(argument, name);
            installed_names.push_back(argument);
            OptoSlangExpr lhs;
            lhs.kind = OPTO_SLANG_EXPR_SIGNAL;
            lhs.signal_name = intern_string(design, std::move(name));
            argument_initializers.push_back(
                {
                    make_expr(design, std::move(lhs), *actuals[index]),
                    lowered_actuals[index],
                    true,
                    source,
                });
        } else {
            design.function_values.insert_or_assign(argument, lowered_actuals[index]);
            installed_values.push_back(argument);
        }
        if (constant_actuals[index] && !is_written) {
            design.procedural_constants.insert_or_assign(argument, constant_actuals[index]);
            installed_constants.push_back(argument);
        }
    }

    return_name = allocate_function_value_name(design, function, "return");
    return_name = add_internal_net(
        design,
        std::move(return_name),
        checked_width(function.getReturnType().getBitstreamWidth(), function.name),
        function.getReturnType().isSigned(),
        process_local);
    design.value_names.insert_or_assign(return_variable, return_name);
    installed_names.push_back(return_variable);

    for (auto* local : locals) {
        if (local == return_variable || std::ranges::find(arguments, local) != arguments.end() ||
            loop_variables.contains(local)) {
            continue;
        }
        auto name = allocate_function_value_name(design, function, local->name);
        name = add_internal_net(
            design,
            std::move(name),
            checked_width(local->getType().getBitstreamWidth(), local->name),
            local->getType().isSigned(),
            process_local);
        design.value_names.insert_or_assign(local, std::move(name));
        installed_names.push_back(local);
    }

    ProcedureBuilder standalone_builder;
    auto& builder = process_local ? *design.active_procedure_builder : standalone_builder;
    OptoSlangExpr return_signal;
    return_signal.kind = OPTO_SLANG_EXPR_SIGNAL;
    return_signal.signal_name = intern_string(design, return_name);
    auto* return_lhs = make_expr(design, std::move(return_signal), call);
    OptoSlangExpr unknown_return;
    unknown_return.kind = OPTO_SLANG_EXPR_CONSTANT;
    unknown_return.constant_has_width = true;
    unknown_return.constant_width =
        checked_width(function.getReturnType().getBitstreamWidth(), function.name);
    unknown_return.constant_bits.assign(unknown_return.constant_width, 'x');
    std::vector<OptoSlangEffectData> initializers;
    initializers.reserve(argument_initializers.size() + 2);
    initializers.push_back(
        {
            return_lhs,
            make_expr(design, std::move(unknown_return), call),
            true,
            source,
        });
    initializers.insert(
        initializers.end(),
        std::make_move_iterator(argument_initializers.begin()),
        std::make_move_iterator(argument_initializers.end()));

    FunctionReturnControl return_control;
    if (statement_requires_return_control(function.getBody())) {
        auto flag_name = allocate_function_value_name(design, function, "returned");
        flag_name = add_internal_net(design, std::move(flag_name), 1, false, process_local);
        OptoSlangExpr returned_signal;
        returned_signal.kind = OPTO_SLANG_EXPR_SIGNAL;
        returned_signal.signal_name = intern_string(design, flag_name);
        return_control.returned = make_expr(design, std::move(returned_signal), call);
        return_control.not_returned =
            make_unary_expr(design, OPTO_SLANG_UNARY_LOGICAL_NOT, return_control.returned, call);
        OptoSlangExpr false_value;
        false_value.kind = OPTO_SLANG_EXPR_CONSTANT;
        false_value.constant_has_width = true;
        false_value.constant_width = 1;
        false_value.constant_bits = "0";
        auto* false_expr = make_expr(design, std::move(false_value), call);
        OptoSlangExpr true_value;
        true_value.kind = OPTO_SLANG_EXPR_CONSTANT;
        true_value.constant_has_width = true;
        true_value.constant_width = 1;
        true_value.constant_bits = "1";
        return_control.true_value = make_expr(design, std::move(true_value), call);
        initializers.push_back({return_control.returned, false_expr, true, source});
    }

    design.function_returns.push_back(return_variable);
    design.function_return_controls.push_back(return_control);
    return_scope_pushed = true;
    auto body = builder.sequence(
        builder.effects(std::move(initializers), source),
        lower_statement(
            builder, design, function.getBody(), OPTO_SLANG_PROCEDURE_COMB),
        source);
    design.function_return_controls.pop_back();
    design.function_returns.pop_back();
    return_scope_pushed = false;
    if (design.active_expression_prelude) {
        auto& prelude = *design.active_expression_prelude;
        prelude = builder.sequence(std::move(prelude), std::move(body), source);
    } else {
        design.module.procedures.push_back(
            builder.finish(
                std::move(body), OPTO_SLANG_PROCEDURE_COMB, {}, source));
    }
    OptoSlangExpr result;
    result.kind = OPTO_SLANG_EXPR_SIGNAL;
    result.signal_name = intern_string(design, std::move(return_name));
    return make_expr(design, std::move(result), call);
}

CfgFragment lower_subroutine_call_statement(
    ProcedureBuilder& builder,
    ModuleLoweringContext& design,
    const CallExpression& call,
    OptoSlangProcedureKind procedure_kind) {
    const auto* selected = std::get_if<const SubroutineSymbol*>(&call.subroutine);
    if (!selected || !*selected) {
        throw std::runtime_error(
            "unsupported non-system call statement '" + copy_string(call.getSubroutineName()) +
            "'");
    }
    const auto& function = **selected;
    const bool synthesizable_kind = function.subroutineKind == SubroutineKind::Task ||
                                    (function.subroutineKind == SubroutineKind::Function &&
                                     function.getReturnType().isVoid());
    if (!synthesizable_kind || function.isVirtual() ||
        function.flags.has(MethodFlags::DPIImport | MethodFlags::BuiltIn)) {
        throw std::runtime_error(
            "call statement '" + copy_string(function.name) +
            "' is not a synthesizable task or void function");
    }
    const auto arguments = function.getArguments();
    const auto actuals = call.arguments();
    if (arguments.size() != actuals.size()) {
        throw std::runtime_error(
            "task or void function call argument count does not match its declaration");
    }
    constexpr size_t max_recursive_depth = 256;
    if (static_cast<size_t>(std::ranges::count(design.function_stack, &function)) >=
        max_recursive_depth) {
        throw std::runtime_error(
            "recursive synthesizable subroutine '" + copy_string(function.name) +
            "' does not reach a constant base case within " +
            std::to_string(max_recursive_depth) + " calls");
    }
    const bool process_local = design.active_procedure_builder != nullptr;
    const auto source = source_span(design, call);
    std::vector<const VariableSymbol*> locals;
    collect_function_locals(function, locals);
    std::unordered_set<const VariableSymbol*> loop_variables;
    collect_function_loop_variables(function.getBody(), loop_variables);
    ScopedSymbolMapBindings function_value_bindings(design.function_values);
    ScopedSymbolMapBindings constant_bindings(design.procedural_constants);
    ScopedSymbolMapBindings name_bindings(design.value_names);
    for (auto* argument : arguments) {
        function_value_bindings.track(argument);
        constant_bindings.track(argument);
        name_bindings.track(argument);
    }
    for (auto* local : locals) {
        function_value_bindings.track(local);
        constant_bindings.track(local);
        name_bindings.track(local);
    }
    for (auto* variable : loop_variables) {
        function_value_bindings.track(variable);
        constant_bindings.track(variable);
        name_bindings.track(variable);
    }
    design.function_stack.push_back(&function);

    struct CopyOut {
        const FormalArgumentSymbol* argument;
        const Expression* actual;
    };
    std::vector<const ValueSymbol*> installed_values;
    std::vector<const ValueSymbol*> installed_constants;
    std::vector<const ValueSymbol*> installed_names;
    std::vector<CopyOut> copy_outs;
    std::vector<OptoSlangEffectData> initializers;
    bool return_control_pushed = false;
    ScopeExit leave_function([&] {
        if (return_control_pushed) {
            design.function_return_controls.pop_back();
        }
        for (auto* symbol : installed_values) {
            design.function_values.erase(symbol);
        }
        for (auto* symbol : installed_constants) {
            design.procedural_constants.erase(symbol);
        }
        for (auto* symbol : installed_names) {
            design.value_names.erase(symbol);
        }
        design.function_stack.pop_back();
    });

    for (size_t index = 0; index < arguments.size(); ++index) {
        auto* argument = arguments[index];
        auto* actual = actuals[index];
        if (!argument || !actual) {
            throw std::runtime_error("subroutine call contains an unbound argument");
        }
        if (argument->direction == ArgumentDirection::Ref) {
            throw std::runtime_error(
                "ref arguments are not supported in synthesizable subroutines");
        }

        auto constant = evaluate_lowering_constant(design, *actual);

        const bool input_only = argument->direction == ArgumentDirection::In;
        const bool is_written =
            !input_only || statement_assigns_value(function.getBody(), *argument);
        if (!is_written) {
            design.function_values.insert_or_assign(argument, lower_expr(design, *actual));
            installed_values.push_back(argument);
            if (constant) {
                design.procedural_constants.insert_or_assign(argument, constant);
                installed_constants.push_back(argument);
            }
            continue;
        }

        auto name = allocate_function_value_name(design, function, argument->name);
        name = add_internal_net(
            design,
            std::move(name),
            checked_width(argument->getType().getBitstreamWidth(), argument->name),
            argument->getType().isSigned(),
            process_local);
        design.value_names.insert_or_assign(argument, name);
        installed_names.push_back(argument);
        OptoSlangExpr local;
        local.kind = OPTO_SLANG_EXPR_SIGNAL;
        local.signal_name = intern_string(design, name);
        const auto* lhs = make_expr(design, std::move(local), *actual);
        const OptoSlangExpr* rhs;
        if (argument->direction == ArgumentDirection::Out) {
            OptoSlangExpr unknown;
            unknown.kind = OPTO_SLANG_EXPR_CONSTANT;
            unknown.constant_has_width = true;
            unknown.constant_width =
                checked_width(argument->getType().getBitstreamWidth(), argument->name);
            unknown.constant_bits.assign(unknown.constant_width, 'x');
            rhs = make_expr(design, std::move(unknown), *actual);
        } else {
            rhs = lower_expr(design, *actual);
        }
        initializers.push_back({lhs, rhs, true, source});
        if (argument->direction != ArgumentDirection::In) {
            copy_outs.push_back(CopyOut{argument, &call_output_lvalue(*actual)});
        }
    }

    for (auto* local : locals) {
        if (std::ranges::find(arguments, local) != arguments.end() ||
            loop_variables.contains(local)) {
            continue;
        }
        auto name = allocate_function_value_name(design, function, local->name);
        name = add_internal_net(
            design,
            std::move(name),
            checked_width(local->getType().getBitstreamWidth(), local->name),
            local->getType().isSigned(),
            process_local);
        design.value_names.insert_or_assign(local, std::move(name));
        installed_names.push_back(local);
    }

    design.function_return_controls.push_back({});
    return_control_pushed = true;
    auto body = builder.sequence(
        builder.effects(std::move(initializers), source),
        lower_statement(builder, design, function.getBody(), procedure_kind),
        source);
    design.function_return_controls.pop_back();
    return_control_pushed = false;
    std::vector<OptoSlangEffectData> copy_effects;
    copy_effects.reserve(copy_outs.size());
    for (const auto& copy : copy_outs) {
        OptoSlangExpr value;
        value.kind = OPTO_SLANG_EXPR_SIGNAL;
        value.signal_name = intern_string(design, registered_value_name(design, *copy.argument));
        copy_effects.push_back(
            {
                lower_signal_expr(design, *copy.actual),
                make_expr(design, std::move(value), *copy.actual),
                true,
                source,
            });
    }
    return builder.sequence(
        std::move(body), builder.effects(std::move(copy_effects), source), source);
}

void validate_initial_process(
    ModuleLoweringContext& design,
    const InstanceBodySymbol& body,
    const ProceduralBlockSymbol& process) {
    ProcedureBuilder builder;
    auto initial = lower_statement(
        builder, design, process.getBody(), OPTO_SLANG_PROCEDURE_COMB);
    if (!initial.empty()) {
        throw std::runtime_error(unsupported_member_message(body, process));
    }
}
}
