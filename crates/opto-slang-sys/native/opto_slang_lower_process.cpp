// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

#include "opto_slang_lower_internal.h"
#include "opto_slang_procedure_cfg.h"

namespace opto::slang_lower {
// Source adapters encode repeat and foreach bounds in compact unsigned fields.
// Rust alone owns the smaller source-profile structural expansion limit.
constexpr uint64_t PROCEDURAL_LOOP_COUNT_CAPACITY = UINT32_MAX;

OptoSlangProcedureData make_guarded_procedure(
    std::vector<GuardedEffectData> effects, OptoSlangProcedureKind kind,
    std::vector<OptoSlangEventData> events, OptoSlangSourceSpanView source) {
  ProcedureBuilder builder;
  CfgFragment body;
  if (auto unconditional =
          std::ranges::find(effects, nullptr, &GuardedEffectData::condition);
      unconditional != effects.end()) {
    effects.erase(unconditional + 1, effects.end());
  }
  for (auto guarded = effects.rbegin(); guarded != effects.rend(); ++guarded) {
    auto fragment = builder.effects({std::move(guarded->effect)}, source);
    if (guarded->condition) {
      fragment = builder.conditional(guarded->condition, std::move(fragment),
                                     std::move(body), source);
    }
    body = std::move(fragment);
  }
  return builder.finish(std::move(body), kind, std::move(events), source);
}

CfgFragment lower_statement(ProcedureBuilder &builder,
                            ModuleLoweringContext &design,
                            const Statement &stmt,
                            OptoSlangProcedureKind procedure_kind);

CfgFragment lower_subroutine_call_statement(
    ProcedureBuilder &builder, ModuleLoweringContext &design,
    const CallExpression &call, OptoSlangProcedureKind procedure_kind);

bool statement_assigns_value(const Statement &statement,
                             const ValueSymbol &value);

const ValueSymbol *expression_root_value(const Expression &expression);

std::string allocate_function_value_name(ModuleLoweringContext &design,
                                         const SubroutineSymbol &function,
                                         std::string_view local);

void append_expression_fragment(ModuleLoweringContext &design,
                                CfgFragment fragment,
                                OptoSlangSourceSpanView source) {
  if (!design.active_expression_prelude || !design.active_procedure_builder) {
    throw std::runtime_error(
        "side-effecting expression requires a procedural lowering context");
  }
  auto &prelude = *design.active_expression_prelude;
  prelude = design.active_procedure_builder->sequence(
      std::move(prelude), std::move(fragment), source);
}

OptoSlangExpr *freeze_dynamic_lvalue(ModuleLoweringContext &design,
                                     const Expression &source_expression,
                                     OptoSlangExpr *lvalue) {
  if (lvalue->kind != OPTO_SLANG_EXPR_DYNAMIC_EXTRACT) {
    return lvalue;
  }
  if (!design.active_procedure_builder) {
    throw std::runtime_error(
        "dynamic procedural lvalue requires a procedure-local selector");
  }
  const auto source = source_span(design, source_expression);
  auto selector_name = add_internal_net(
      design,
      "__opto_lvalue_" + std::to_string(design.next_lvalue_instance++) +
          "_selector",
      lvalue->dynamic_extract_offset_width, false, true);
  OptoSlangExpr selector_lhs;
  selector_lhs.kind = OPTO_SLANG_EXPR_SIGNAL;
  selector_lhs.signal_name = intern_string(design, selector_name);
  auto *lowered_selector_lhs =
      make_expr(design, std::move(selector_lhs), source_expression);
  append_expression_fragment(
      design,
      design.active_procedure_builder->effects(
          {{lowered_selector_lhs, lvalue->dynamic_extract_offset, true,
            source}},
          source),
      source);

  OptoSlangExpr selector_value;
  selector_value.kind = OPTO_SLANG_EXPR_SIGNAL;
  selector_value.signal_name = intern_string(design, std::move(selector_name));
  auto frozen = *lvalue;
  frozen.dynamic_extract_offset =
      make_expr(design, std::move(selector_value), source_expression);
  return make_expr(design, std::move(frozen), source_expression);
}

bool expression_may_produce_procedural_effects(const Expression &expression) {
  bool found = false;
  expression.visit(
      makeVisitor([&](auto &self, const AssignmentExpression &nested) {
        found = true;
        self.visitDefault(nested);
      }));
  if (found) {
    return true;
  }
  expression.visit(makeVisitor([&](auto &self, const UnaryExpression &unary) {
    found = unary.op == UnaryOperator::Preincrement ||
            unary.op == UnaryOperator::Postincrement ||
            unary.op == UnaryOperator::Predecrement ||
            unary.op == UnaryOperator::Postdecrement;
    if (!found) {
      self.visitDefault(unary);
    }
  }));
  if (found) {
    return true;
  }
  expression.visit(
      makeVisitor([&](auto &, const CallExpression &) { found = true; }));
  return found;
}

OptoSlangExpr *
make_assignment_result_temp(ModuleLoweringContext &design,
                            const AssignmentExpression &assignment) {
  const auto width = checked_width(lowered_type_width(*assignment.type),
                                   "assignment expression result");
  auto name = add_internal_net(
      design,
      "__opto_assignment_" + std::to_string(design.next_lvalue_instance++),
      width, assignment.type->isSigned(), true);
  OptoSlangExpr value;
  value.kind = OPTO_SLANG_EXPR_SIGNAL;
  value.signal_name = intern_string(design, std::move(name));
  return make_expr(design, std::move(value), assignment);
}

OptoSlangExpr *snapshot_compound_lvalue(ModuleLoweringContext &design,
                                        const AssignmentExpression &assignment,
                                        const OptoSlangExpr *value) {
  const auto source = source_span(design, assignment.left());
  const auto width = checked_width(lowered_type_width(*assignment.left().type),
                                   "compound assignment lvalue");
  auto name = add_internal_net(
      design,
      "__opto_compound_" + std::to_string(design.next_lvalue_instance++) +
          "_old",
      width, assignment.left().type->isSigned(), true);
  OptoSlangExpr snapshot;
  snapshot.kind = OPTO_SLANG_EXPR_SIGNAL;
  snapshot.signal_name = intern_string(design, std::move(name));
  auto *result = make_expr(design, std::move(snapshot), assignment.left());
  append_expression_fragment(design,
                             design.active_procedure_builder->effects(
                                 {{result, value, true, source}}, source),
                             source);
  return result;
}

OptoSlangExpr *
concatenated_lvalue_value(ModuleLoweringContext &design,
                          const Expression &concatenation,
                          const std::vector<LvalueLeaf> &leaves,
                          const std::vector<OptoSlangExpr *> &targets) {
  if (leaves.size() != targets.size()) {
    throw std::runtime_error(
        "lvalue concatenation target storage is inconsistent");
  }
  OptoSlangExpr value;
  value.kind = OPTO_SLANG_EXPR_CONCAT;
  value.concat_parts.reserve(leaves.size());
  for (size_t index = 0; index < leaves.size(); ++index) {
    value.concat_parts.push_back(
        targets[index] ? targets[index]
                       : lower_expr(design, *leaves[index].expression));
  }
  return make_expr(design, std::move(value), concatenation);
}

OptoSlangExpr *
lower_assignment_expression(ModuleLoweringContext &design,
                            const AssignmentExpression &assignment) {
  if (!assignment.isBlocking()) {
    throw std::runtime_error(
        "nonblocking assignments are not permitted inside expressions");
  }
  if (assignment.timingControl) {
    throw std::runtime_error(
        "timing controls are not supported in assignment expressions");
  }
  if (!design.active_expression_prelude || !design.active_procedure_builder) {
    throw std::runtime_error(
        "assignment expressions are only supported in procedural code");
  }

  const auto source = source_span(design, assignment);
  auto *result = make_assignment_result_temp(design, assignment);
  if (assignment.left().kind == ExpressionKind::Concatenation) {
    std::vector<LvalueLeaf> leaves;
    collect_lvalue_leaves(assignment.left(), leaves);
    if (leaves.empty()) {
      throw std::runtime_error("assignment expression has an empty lvalue");
    }
    std::vector<OptoSlangExpr *> targets;
    targets.reserve(leaves.size());
    for (const auto &leaf : leaves) {
      if (constant_element_select_is_out_of_range(design, *leaf.expression)) {
        targets.push_back(nullptr);
        continue;
      }
      targets.push_back(
          freeze_dynamic_lvalue(design, *leaf.expression,
                                lower_signal_expr(design, *leaf.expression)));
    }
    if (assignment.isCompound()) {
      auto *old_value = snapshot_compound_lvalue(
          design, assignment,
          concatenated_lvalue_value(design, assignment.left(), leaves,
                                    targets));
      design.lvalue_references.push_back(old_value);
    }
    ScopeExit release_lvalue_reference([&] {
      if (assignment.isCompound()) {
        design.lvalue_references.pop_back();
      }
    });
    auto *rhs = cast_to_lvalue_type(
        design, lower_expr(design, assignment.right()), assignment.left());
    std::vector<OptoSlangEffectData> effects;
    effects.reserve(leaves.size() + 1);
    effects.push_back({result, rhs, true, source});
    const auto total_width =
        checked_width(lowered_type_width(*assignment.left().type),
                      "assignment expression lvalue");
    uint64_t consumed = 0;
    for (size_t index = 0; index < leaves.size(); ++index) {
      consumed += leaves[index].width;
      if (consumed > total_width) {
        throw std::runtime_error(
            "lvalue concatenation width exceeds its assignment type");
      }
      if (!targets[index]) {
        continue;
      }
      effects.push_back({
          targets[index],
          apply_rvalue_slice(design, result, total_width - consumed,
                             leaves[index].width, assignment.right()),
          true,
          source,
      });
    }
    if (consumed != total_width) {
      throw std::runtime_error(
          "lvalue concatenation width does not match its assignment type");
    }
    append_expression_fragment(
        design,
        design.active_procedure_builder->effects(std::move(effects), source),
        source);
    return result;
  }

  if (constant_element_select_is_out_of_range(design, assignment.left())) {
    auto *rhs = cast_to_lvalue_type(
        design, lower_expr(design, assignment.right()), assignment.left());
    append_expression_fragment(design,
                               design.active_procedure_builder->effects(
                                   {{result, rhs, true, source}}, source),
                               source);
    return result;
  }

  auto *lhs = freeze_dynamic_lvalue(
      design, assignment.left(), lower_signal_expr(design, assignment.left()));
  if (assignment.isCompound()) {
    design.lvalue_references.push_back(
        snapshot_compound_lvalue(design, assignment, lhs));
  }
  ScopeExit release_lvalue_reference([&] {
    if (assignment.isCompound()) {
      design.lvalue_references.pop_back();
    }
  });
  auto *rhs = cast_to_lvalue_type(
      design, lower_expr(design, assignment.right()), assignment.left());
  append_expression_fragment(design,
                             design.active_procedure_builder->effects(
                                 {
                                     {result, rhs, true, source},
                                     {lhs, result, true, source},
                                 },
                                 source),
                             source);
  return result;
}

OptoSlangExpr *lower_update_expression(ModuleLoweringContext &design,
                                       const UnaryExpression &unary) {
  const bool increment = unary.op == UnaryOperator::Preincrement ||
                         unary.op == UnaryOperator::Postincrement;
  const bool decrement = unary.op == UnaryOperator::Predecrement ||
                         unary.op == UnaryOperator::Postdecrement;
  const bool prefix = unary.op == UnaryOperator::Preincrement ||
                      unary.op == UnaryOperator::Predecrement;
  if (!increment && !decrement) {
    throw std::runtime_error(
        "update-expression lowering received a non-update operator");
  }
  if (!unary.type->isIntegral() || !unary.operand().type->isIntegral()) {
    throw std::runtime_error(
        "increment and decrement expressions require an integral lvalue");
  }
  if (!design.active_expression_prelude || !design.active_procedure_builder) {
    throw std::runtime_error("increment and decrement expressions are only "
                             "supported in procedural code");
  }

  const auto source = source_span(design, unary);
  const auto width = checked_width(lowered_type_width(*unary.type),
                                   "increment or decrement expression result");
  auto result_name = add_internal_net(
      design, "__opto_update_" + std::to_string(design.next_lvalue_instance++),
      width, unary.type->isSigned(), true);
  OptoSlangExpr result_value;
  result_value.kind = OPTO_SLANG_EXPR_SIGNAL;
  result_value.signal_name = intern_string(design, std::move(result_name));
  auto *result = make_expr(design, std::move(result_value), unary);

  if (constant_element_select_is_out_of_range(design, unary.operand())) {
    append_expression_fragment(
        design,
        design.active_procedure_builder->effects(
            {{result, lower_expr(design, unary.operand()), true, source}},
            source),
        source);
    return result;
  }

  auto *lvalue = freeze_dynamic_lvalue(
      design, unary.operand(), lower_signal_expr(design, unary.operand()));
  auto *one = unary.type->isSigned()
                  ? make_signed_constant_expr(design, 1, width, unary)
                  : make_unsigned_constant_expr(design, 1, width, unary);
  std::vector<OptoSlangEffectData> effects;
  effects.reserve(2);
  if (prefix) {
    auto *updated = make_binary_expr(
        design, increment ? OPTO_SLANG_BINARY_ADD : OPTO_SLANG_BINARY_SUB,
        lvalue, one, unary);
    effects.push_back({result, updated, true, source});
    effects.push_back({lvalue, result, true, source});
  } else {
    effects.push_back({result, lvalue, true, source});
    auto *updated = make_binary_expr(
        design, increment ? OPTO_SLANG_BINARY_ADD : OPTO_SLANG_BINARY_SUB,
        result, one, unary);
    effects.push_back({lvalue, updated, true, source});
  }
  append_expression_fragment(
      design,
      design.active_procedure_builder->effects(std::move(effects), source),
      source);
  return result;
}

OptoSlangExpr *lower_short_circuit_operand(ModuleLoweringContext &design,
                                           const Expression &expression) {
  if (!expression.type->isIntegral()) {
    // Preserve the narrower diagnostic owned by an invalid child (for
    // example, an out-of-range select) before reporting its propagated
    // error type as an invalid Boolean context.
    static_cast<void>(lower_expr(design, expression));
  }
  return lower_boolean_context(design, expression);
}

OptoSlangExpr *lower_short_circuit_expression(ModuleLoweringContext &design,
                                              const BinaryExpression &binary) {
  const bool is_and = binary.op == BinaryOperator::LogicalAnd;
  if (!is_and && binary.op != BinaryOperator::LogicalOr) {
    throw std::runtime_error(
        "short-circuit lowering received a non-logical operator");
  }
  const auto left_constant = constant_boolean_value(design, binary.left());
  if ((is_and && left_constant == false) ||
      (!is_and && left_constant == true)) {
    return make_unsigned_constant_expr(design, is_and ? 0 : 1, 1, binary);
  }
  if (left_constant == is_and) {
    return lower_short_circuit_operand(design, binary.right());
  }

  auto *left = lower_short_circuit_operand(design, binary.left());
  if (!design.active_expression_prelude || !design.active_procedure_builder) {
    return make_binary_expr(
        design,
        is_and ? OPTO_SLANG_BINARY_LOGICAL_AND : OPTO_SLANG_BINARY_LOGICAL_OR,
        left, lower_short_circuit_operand(design, binary.right()), binary);
  }

  CfgFragment right_prelude;
  OptoSlangExpr *right = nullptr;
  {
    ScopedValue expression_prelude(design.active_expression_prelude,
                                   &right_prelude);
    right = lower_short_circuit_operand(design, binary.right());
  }
  if (right_prelude.empty()) {
    const auto right_constant = constant_boolean_value(design, binary.right());
    if ((is_and && right_constant == false) ||
        (!is_and && right_constant == true)) {
      return make_unsigned_constant_expr(design, is_and ? 0 : 1, 1, binary);
    }
    if (right_constant == is_and) {
      return left;
    }
    return make_binary_expr(design,
                            is_and ? OPTO_SLANG_BINARY_LOGICAL_AND
                                   : OPTO_SLANG_BINARY_LOGICAL_OR,
                            left, right, binary);
  }

  const auto source = source_span(design, binary);
  auto result_name = add_internal_net(
      design,
      "__opto_short_circuit_" + std::to_string(design.next_lvalue_instance++),
      1, false, true);
  OptoSlangExpr result_value;
  result_value.kind = OPTO_SLANG_EXPR_SIGNAL;
  result_value.signal_name = intern_string(design, std::move(result_name));
  auto *result = make_expr(design, std::move(result_value), binary);
  auto assign_result = [&](const OptoSlangExpr *value) {
    return design.active_procedure_builder->effects(
        {{result, value, true, source}}, source);
  };
  right_prelude = design.active_procedure_builder->sequence(
      std::move(right_prelude), assign_result(right), source);
  auto decisive = assign_result(
      make_unsigned_constant_expr(design, is_and ? 0 : 1, 1, binary));
  append_expression_fragment(
      design,
      is_and ? design.active_procedure_builder->conditional(
                   left, std::move(right_prelude), std::move(decisive), source)
             : design.active_procedure_builder->conditional(
                   left, std::move(decisive), std::move(right_prelude), source),
      source);
  return result;
}

using PatternBindingScope = ScopedSymbolMapBindings<OptoSlangExpr *>;

OptoSlangExpr *capture_pattern_value(ModuleLoweringContext &design,
                                     const Type &type, OptoSlangExpr *value,
                                     const Expression &source) {
  if (!design.active_expression_prelude || !design.active_procedure_builder) {
    return value;
  }
  const auto storage_width = lowered_type_width(type);
  if (storage_width == 0) {
    throw std::runtime_error(
        "pattern variables require a fixed synthesis representation at " +
        expression_location(design, source));
  }
  const auto width = checked_width(storage_width, "pattern variable");
  auto name = add_internal_net(
      design, "__opto_pattern_" + std::to_string(design.next_lvalue_instance++),
      width, type.isSigned(), true);
  OptoSlangExpr captured;
  captured.kind = OPTO_SLANG_EXPR_SIGNAL;
  captured.signal_name = intern_string(design, std::move(name));
  auto *result = make_expr(design, std::move(captured), source);
  const auto span = source_span(design, source);
  append_expression_fragment(design,
                             design.active_procedure_builder->effects(
                                 {{result, value, true, span}}, span),
                             span);
  return result;
}

OptoSlangExpr *lower_pattern_predicate(
    ModuleLoweringContext &design, const Pattern &pattern, OptoSlangExpr *value,
    const Type &value_type, const Expression &source,
    PatternBindingScope &bindings,
    CaseStatementCondition condition_kind = CaseStatementCondition::Normal) {
  switch (pattern.kind) {
  case PatternKind::Invalid:
    throw std::runtime_error("invalid pattern reached synthesis lowering at " +
                             expression_location(design, source));
  case PatternKind::Wildcard:
    return make_unsigned_constant_expr(design, 1, 1, source);
  case PatternKind::Constant: {
    auto *constant = lower_expr(design, pattern.as<ConstantPattern>().expr);
    if (constant->kind != OPTO_SLANG_EXPR_CONSTANT ||
        !constant->constant_has_width) {
      throw std::runtime_error(
          "constant pattern did not elaborate to a fixed constant at " +
          expression_location(design, source));
    }
    const auto has_x =
        constant->constant_bits.find_first_of("xX") != std::string::npos;
    const auto has_z =
        constant->constant_bits.find_first_of("zZ") != std::string::npos;
    if (condition_kind == CaseStatementCondition::WildcardXOrZ) {
      throw std::runtime_error(
          "casex pattern matching is not supported for synthesis");
    }
    if (has_x ||
        (has_z && condition_kind != CaseStatementCondition::WildcardJustZ)) {
      throw std::runtime_error("pattern matching against runtime X/Z state is "
                               "not synthesizable in the Opto ASIC profile");
    }
    if (condition_kind != CaseStatementCondition::WildcardJustZ || !has_z) {
      return make_binary_expr(design, OPTO_SLANG_BINARY_EQ, value, constant,
                              pattern.as<ConstantPattern>().expr);
    }

    std::string mask;
    std::string cared;
    mask.reserve(constant->constant_bits.size());
    cared.reserve(constant->constant_bits.size());
    for (char bit : constant->constant_bits) {
      const bool wildcard = bit == 'z' || bit == 'Z';
      mask.push_back(wildcard ? '0' : '1');
      cared.push_back(wildcard ? '0' : bit);
    }
    OptoSlangExpr mask_expr;
    mask_expr.kind = OPTO_SLANG_EXPR_CONSTANT;
    mask_expr.constant_has_width = true;
    mask_expr.constant_width = constant->constant_width;
    mask_expr.constant_bits = std::move(mask);
    auto *mask_value = make_expr(design, std::move(mask_expr),
                                 pattern.as<ConstantPattern>().expr);
    OptoSlangExpr cared_expr;
    cared_expr.kind = OPTO_SLANG_EXPR_CONSTANT;
    cared_expr.constant_has_width = true;
    cared_expr.constant_width = constant->constant_width;
    cared_expr.constant_bits = std::move(cared);
    auto *cared_value = make_expr(design, std::move(cared_expr),
                                  pattern.as<ConstantPattern>().expr);
    return make_binary_expr(
        design, OPTO_SLANG_BINARY_EQ,
        make_binary_expr(design, OPTO_SLANG_BINARY_BIT_AND, value, mask_value,
                         pattern.as<ConstantPattern>().expr),
        cared_value, pattern.as<ConstantPattern>().expr);
  }
  case PatternKind::Variable: {
    const auto &variable = pattern.as<VariablePattern>().variable;
    auto *captured =
        capture_pattern_value(design, variable.getType(), value, source);
    bindings.track(&variable);
    design.function_values.insert_or_assign(&variable, captured);
    return make_unsigned_constant_expr(design, 1, 1, source);
  }
  case PatternKind::Tagged: {
    const auto &tagged = pattern.as<TaggedPattern>();
    const auto layout = tagged_union_layout(value_type);
    OptoSlangExpr *predicate = nullptr;
    if (layout.tag_width == 0) {
      predicate = make_unsigned_constant_expr(design, 1, 1, source);
    } else {
      auto *tag = apply_rvalue_slice(design, value, layout.payload_width,
                                     layout.tag_width, source);
      auto *expected = make_unsigned_constant_expr(
          design, tagged.member.fieldIndex, layout.tag_width, source);
      predicate =
          make_binary_expr(design, OPTO_SLANG_BINARY_EQ, tag, expected, source);
    }
    if (!tagged.valuePattern) {
      return predicate;
    }
    const auto field_width = checked_width(
        lowered_type_width(tagged.member.getType()), tagged.member.name);
    auto *field_value =
        apply_rvalue_slice(design, value, 0, field_width, source);
    auto *field_predicate = lower_pattern_predicate(
        design, *tagged.valuePattern, field_value, tagged.member.getType(),
        source, bindings, condition_kind);
    return make_binary_expr(design, OPTO_SLANG_BINARY_LOGICAL_AND, predicate,
                            field_predicate, source);
  }
  case PatternKind::Structure: {
    const auto &canonical = value_type.getCanonicalType();
    if (!canonical.isStruct() || !canonical.isFixedSize()) {
      throw std::runtime_error(
          "structure patterns require a fixed synthesis struct at " +
          expression_location(design, source));
    }
    OptoSlangExpr *predicate = nullptr;
    for (const auto &field_pattern : pattern.as<StructurePattern>().patterns) {
      const auto &field = *field_pattern.field;
      auto *field_value = apply_rvalue_slice(
          design, value, aggregate_field_storage_offset(canonical, field),
          checked_width(lowered_type_width(field.getType()), field.name),
          source);
      auto *field_predicate = lower_pattern_predicate(
          design, *field_pattern.pattern, field_value, field.getType(), source,
          bindings, condition_kind);
      predicate = predicate
                      ? make_binary_expr(design, OPTO_SLANG_BINARY_LOGICAL_AND,
                                         predicate, field_predicate, source)
                      : field_predicate;
    }
    return predicate ? predicate
                     : make_unsigned_constant_expr(design, 1, 1, source);
  }
  }
  throw std::runtime_error("unknown pattern kind during synthesis lowering");
}

struct LoweredConditionList {
  OptoSlangExpr *predicate = nullptr;
  std::optional<bool> constant;
};

template <typename Conditions>
LoweredConditionList lower_condition_list(ModuleLoweringContext &design,
                                          const Conditions &conditions,
                                          PatternBindingScope &bindings) {
  OptoSlangExpr *combined = nullptr;
  bool saw_runtime = false;
  for (const auto &condition : conditions) {
    if (!condition.pattern) {
      if (const auto constant =
              constant_boolean_value(design, *condition.expr)) {
        if (!*constant) {
          return {
              make_unsigned_constant_expr(design, 0, 1, *condition.expr),
              false,
          };
        }
        continue;
      }
    }

    CfgFragment term_prelude;
    OptoSlangExpr *term = nullptr;
    if (design.active_expression_prelude && design.active_procedure_builder) {
      ScopedValue expression_prelude(design.active_expression_prelude,
                                     &term_prelude);
      auto *value =
          condition.pattern ? lower_expr(design, *condition.expr) : nullptr;
      term = condition.pattern
                 ? lower_pattern_predicate(design, *condition.pattern, value,
                                           *condition.expr->type,
                                           *condition.expr, bindings)
                 : lower_boolean_context(design, *condition.expr);
    } else {
      auto *value =
          condition.pattern ? lower_expr(design, *condition.expr) : nullptr;
      term = condition.pattern
                 ? lower_pattern_predicate(design, *condition.pattern, value,
                                           *condition.expr->type,
                                           *condition.expr, bindings)
                 : lower_boolean_context(design, *condition.expr);
    }

    saw_runtime = true;
    if (!combined) {
      if (!term_prelude.empty()) {
        append_expression_fragment(design, std::move(term_prelude),
                                   source_span(design, *condition.expr));
      }
      combined = term;
      continue;
    }
    if (term_prelude.empty()) {
      combined = make_binary_expr(design, OPTO_SLANG_BINARY_LOGICAL_AND,
                                  combined, term, *condition.expr);
      continue;
    }

    const auto source = source_span(design, *condition.expr);
    auto name = add_internal_net(
        design,
        "__opto_condition_" + std::to_string(design.next_lvalue_instance++), 1,
        false, true);
    OptoSlangExpr result_expr;
    result_expr.kind = OPTO_SLANG_EXPR_SIGNAL;
    result_expr.signal_name = intern_string(design, std::move(name));
    auto *result = make_expr(design, std::move(result_expr), *condition.expr);
    term_prelude = design.active_procedure_builder->sequence(
        std::move(term_prelude),
        design.active_procedure_builder->effects({{result, term, true, source}},
                                                 source),
        source);
    auto false_branch = design.active_procedure_builder->effects(
        {{
            result,
            make_unsigned_constant_expr(design, 0, 1, *condition.expr),
            true,
            source,
        }},
        source);
    append_expression_fragment(
        design,
        design.active_procedure_builder->conditional(
            combined, std::move(term_prelude), std::move(false_branch), source),
        source);
    combined = result;
  }

  if (!saw_runtime) {
    return {
        make_unsigned_constant_expr(design, 1, 1, *conditions.front().expr),
        true,
    };
  }
  return {combined, std::nullopt};
}

OptoSlangExpr *
lower_conditional_expression(ModuleLoweringContext &design,
                             const ConditionalExpression &conditional) {
  if (conditional.conditions.empty()) {
    throw std::runtime_error(
        "conditional expression requires at least one condition");
  }

  LoweredConditionList lowered_conditions;
  CfgFragment then_prelude;
  OptoSlangExpr *then_value = nullptr;
  {
    PatternBindingScope bindings(design.function_values);
    lowered_conditions =
        lower_condition_list(design, conditional.conditions, bindings);
    if (lowered_conditions.constant == true) {
      return cast_to_expression_type(
          design, lower_expr(design, conditional.left()), conditional);
    }
    if (lowered_conditions.constant != false) {
      if (design.active_expression_prelude && design.active_procedure_builder) {
        ScopedValue expression_prelude(design.active_expression_prelude,
                                       &then_prelude);
        then_value = cast_to_expression_type(
            design, lower_expr(design, conditional.left()), conditional);
      } else {
        then_value = cast_to_expression_type(
            design, lower_expr(design, conditional.left()), conditional);
      }
    }
  }
  if (lowered_conditions.constant == false) {
    return cast_to_expression_type(
        design, lower_expr(design, conditional.right()), conditional);
  }

  CfgFragment else_prelude;
  OptoSlangExpr *else_value = nullptr;
  if (design.active_expression_prelude && design.active_procedure_builder) {
    ScopedValue expression_prelude(design.active_expression_prelude,
                                   &else_prelude);
    else_value = cast_to_expression_type(
        design, lower_expr(design, conditional.right()), conditional);
  } else {
    else_value = cast_to_expression_type(
        design, lower_expr(design, conditional.right()), conditional);
  }
  if (then_prelude.empty() && else_prelude.empty()) {
    return make_mux_expr(design, lowered_conditions.predicate, then_value,
                         else_value, conditional);
  }

  const auto source = source_span(design, conditional);
  const auto width = checked_width(lowered_type_width(*conditional.type),
                                   "conditional expression result");
  auto result_name = add_internal_net(
      design,
      "__opto_conditional_" + std::to_string(design.next_lvalue_instance++),
      width, conditional.type->isSigned(), true);
  OptoSlangExpr result_value;
  result_value.kind = OPTO_SLANG_EXPR_SIGNAL;
  result_value.signal_name = intern_string(design, std::move(result_name));
  auto *result = make_expr(design, std::move(result_value), conditional);
  then_prelude = design.active_procedure_builder->sequence(
      std::move(then_prelude),
      design.active_procedure_builder->effects(
          {{result, then_value, true, source}}, source),
      source);
  else_prelude = design.active_procedure_builder->sequence(
      std::move(else_prelude),
      design.active_procedure_builder->effects(
          {{result, else_value, true, source}}, source),
      source);
  append_expression_fragment(design,
                             design.active_procedure_builder->conditional(
                                 lowered_conditions.predicate,
                                 std::move(then_prelude),
                                 std::move(else_prelude), source),
                             source);
  return result;
}

CfgFragment lower_assignment_statement(ProcedureBuilder &builder,
                                       ModuleLoweringContext &design,
                                       const AssignmentExpression &assignment,
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
    std::vector<LvalueLeaf> leaves;
    collect_lvalue_leaves(assignment.left(), leaves);
    if (leaves.empty()) {
      throw std::runtime_error("procedural assignment has an empty lvalue");
    }
    const auto total_width =
        checked_width(lowered_type_width(*assignment.left().type),
                      "procedural assignment lvalue");
    std::vector<OptoSlangExpr *> targets;
    targets.reserve(leaves.size());
    for (const auto &leaf : leaves) {
      if (constant_element_select_is_out_of_range(design, *leaf.expression)) {
        targets.push_back(nullptr);
        continue;
      }
      targets.push_back(
          freeze_dynamic_lvalue(design, *leaf.expression,
                                lower_signal_expr(design, *leaf.expression)));
    }
    if (assignment.isCompound()) {
      auto *old_value = snapshot_compound_lvalue(
          design, assignment,
          concatenated_lvalue_value(design, assignment.left(), leaves,
                                    targets));
      design.lvalue_references.push_back(old_value);
    }
    ScopeExit release_lvalue_reference([&] {
      if (assignment.isCompound()) {
        design.lvalue_references.pop_back();
      }
    });
    const auto *rhs = cast_to_lvalue_type(
        design, lower_expr(design, assignment.right()), assignment.left());
    std::vector<OptoSlangEffectData> effects;
    effects.reserve(leaves.size() + 1);
    if (blocking) {
      auto temp_name = add_internal_net(
          design,
          "__opto_lvalue_" + std::to_string(design.next_lvalue_instance++),
          total_width, assignment.left().type->isSigned());
      OptoSlangExpr temp;
      temp.kind = OPTO_SLANG_EXPR_SIGNAL;
      temp.signal_name = intern_string(design, std::move(temp_name));
      auto *temp_value = make_expr(design, std::move(temp), assignment.right());
      effects.push_back({temp_value, rhs, true, source});
      rhs = temp_value;
    }

    uint64_t consumed = 0;
    for (size_t index = 0; index < leaves.size(); ++index) {
      const auto &leaf = leaves[index];
      consumed += leaf.width;
      if (consumed > total_width) {
        throw std::runtime_error(
            "lvalue concatenation width exceeds its assignment type");
      }
      if (!targets[index]) {
        continue;
      }
      effects.push_back({
          targets[index],
          apply_rvalue_slice(design, rhs, total_width - consumed, leaf.width,
                             assignment.right()),
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

  auto *lhs = lower_signal_expr(design, assignment.left());
  if (assignment.isCompound() ||
      expression_may_produce_procedural_effects(assignment.right())) {
    lhs = freeze_dynamic_lvalue(design, assignment.left(), lhs);
  }
  if (assignment.isCompound()) {
    design.lvalue_references.push_back(
        snapshot_compound_lvalue(design, assignment, lhs));
  }
  ScopeExit release_lvalue_reference([&] {
    if (assignment.isCompound()) {
      design.lvalue_references.pop_back();
    }
  });
  const auto *rhs = cast_to_lvalue_type(
      design, lower_expr(design, assignment.right()), assignment.left());
  return builder.effects({{lhs, rhs, blocking, source}}, source);
}

std::optional<bool> constant_boolean_value(ModuleLoweringContext &design,
                                           const Expression &expression) {
  if (expression.kind == ExpressionKind::Invalid) {
    auto *child = expression.as<InvalidExpression>().child;
    return child ? constant_boolean_value(design, *child) : std::nullopt;
  }
  if (expression.kind == ExpressionKind::Conversion &&
      !expression.type->isIntegral()) {
    return constant_boolean_value(
        design, expression.as<ConversionExpression>().operand());
  }
  if (expression.kind == ExpressionKind::IntegerLiteral ||
      expression.kind == ExpressionKind::UnbasedUnsizedIntegerLiteral) {
    auto value = expression.kind == ExpressionKind::IntegerLiteral
                     ? expression.as<IntegerLiteral>().getValue()
                     : expression.as<UnbasedUnsizedIntegerLiteral>().getValue();
    auto truth = static_cast<logic_t>(value);
    return truth.isUnknown() ? std::nullopt
                             : std::optional<bool>(static_cast<bool>(truth));
  }
  if (expression.kind == ExpressionKind::MemberAccess) {
    const auto &access = expression.as<MemberAccessExpression>();
    EvalContext context(access.member);
    auto value = expression.eval(context);
    if (value && value.isInteger() && !value.integer().hasUnknown()) {
      return value.isTrue();
    }
  }
  if (expression.kind == ExpressionKind::BinaryOp) {
    const auto &binary = expression.as<BinaryExpression>();
    if (binary.op == BinaryOperator::LogicalAnd ||
        binary.op == BinaryOperator::LogicalOr) {
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
        return binary.op == BinaryOperator::LogicalAnd ? *left && *right
                                                       : *left || *right;
      }
      return std::nullopt;
    }
  }
  if (auto *value = expression.getConstant();
      value && value->isInteger() && !value->integer().hasUnknown()) {
    return value->isTrue();
  }
  auto value = evaluate_lowering_constant(design, expression);
  if (value && value.isInteger() && !value.integer().hasUnknown()) {
    return value.isTrue();
  }
  return std::nullopt;
}

CfgFragment lower_conditional_statement(ProcedureBuilder &builder,
                                        ModuleLoweringContext &design,
                                        const ConditionalStatement &statement,
                                        OptoSlangProcedureKind procedure_kind) {
  if (statement.conditions.empty()) {
    throw std::runtime_error(
        "if statement without a condition is not supported");
  }
  const auto source = source_span(design, statement);
  LoweredConditionList lowered_conditions;
  CfgFragment then_branch;
  {
    PatternBindingScope bindings(design.function_values);
    lowered_conditions =
        lower_condition_list(design, statement.conditions, bindings);
    if (lowered_conditions.constant != false) {
      then_branch =
          lower_statement(builder, design, statement.ifTrue, procedure_kind);
    }
  }
  if (lowered_conditions.constant == true) {
    return then_branch;
  }

  CfgFragment else_branch;
  if (statement.ifFalse) {
    else_branch =
        lower_statement(builder, design, *statement.ifFalse, procedure_kind);
  }
  if (lowered_conditions.constant == false) {
    return else_branch;
  }

  const auto dispatch = builder.add_block(source);
  const bool then_falls_through =
      then_branch.empty() || !then_branch.exits.empty();
  const bool else_falls_through =
      else_branch.empty() || !else_branch.exits.empty();
  const auto join = then_falls_through || else_falls_through
                        ? std::optional<uint32_t>(builder.add_block(source))
                        : std::nullopt;
  builder.branch(dispatch, lowered_conditions.predicate,
                 then_branch.empty() ? *join : *then_branch.entry,
                 else_branch.empty() ? *join : *else_branch.entry, source);
  if (join && !then_branch.empty()) {
    for (auto exit : then_branch.exits) {
      builder.jump(exit, *join, source);
    }
  }
  if (join && !else_branch.empty()) {
    for (auto exit : else_branch.exits) {
      builder.jump(exit, *join, source);
    }
  }
  return {
      dispatch,
      join ? std::vector<uint32_t>{*join} : std::vector<uint32_t>{},
  };
}

struct PriorityArm {
  const OptoSlangExpr *condition;
  uint32_t dispatch;
  CfgFragment body;
  CfgFragment predicate_prelude{};
};

CfgFragment finish_priority_case(ProcedureBuilder &builder,
                                 std::vector<PriorityArm> arms,
                                 CfgFragment default_body,
                                 OptoSlangSourceSpanView source) {
  if (arms.empty()) {
    return default_body;
  }
  std::vector<uint32_t> entries;
  entries.reserve(arms.size());
  for (auto &arm : arms) {
    if (arm.predicate_prelude.empty()) {
      entries.push_back(arm.dispatch);
      continue;
    }
    auto predicate = builder.sequence(std::move(arm.predicate_prelude),
                                      {arm.dispatch, {arm.dispatch}}, source);
    entries.push_back(*predicate.entry);
  }
  const bool falls_through =
      default_body.empty() || !default_body.exits.empty() ||
      std::ranges::any_of(arms, [](const PriorityArm &arm) {
        return arm.body.empty() || !arm.body.exits.empty();
      });
  const auto join = falls_through
                        ? std::optional<uint32_t>(builder.add_block(source))
                        : std::nullopt;
  const auto default_target =
      default_body.empty() ? *join : *default_body.entry;
  for (size_t index = 0; index < arms.size(); ++index) {
    const auto false_target =
        index + 1 < arms.size() ? entries[index + 1] : default_target;
    const auto true_target =
        arms[index].body.empty() ? *join : *arms[index].body.entry;
    builder.branch(arms[index].dispatch, arms[index].condition, true_target,
                   false_target, source);
    if (join) {
      for (auto exit : arms[index].body.exits) {
        builder.jump(exit, *join, source);
      }
    }
  }
  if (join) {
    for (auto exit : default_body.exits) {
      builder.jump(exit, *join, source);
    }
  }
  return {
      entries.front(),
      join ? std::vector<uint32_t>{*join} : std::vector<uint32_t>{},
  };
}

CfgFragment lower_case_statement(ProcedureBuilder &builder,
                                 ModuleLoweringContext &design,
                                 const CaseStatement &statement,
                                 OptoSlangProcedureKind procedure_kind) {
  if (statement.condition == CaseStatementCondition::WildcardXOrZ) {
    throw std::runtime_error("casex is not supported for synthesis lowering");
  }
  const auto source = source_span(design, statement);
  if (statement.condition == CaseStatementCondition::Inside) {
    auto *selector = lower_expr(design, statement.expr);
    std::vector<PriorityArm> arms;
    arms.reserve(statement.items.size());
    for (const auto &item : statement.items) {
      OptoSlangExpr *condition = nullptr;
      for (auto *expression : item.expressions) {
        if (!expression) {
          throw std::runtime_error("case inside item contains a null pattern");
        }
        auto *matched = lower_inside_item_match(design, selector, *expression);
        condition = condition
                        ? make_binary_expr(design, OPTO_SLANG_BINARY_LOGICAL_OR,
                                           condition, matched, *expression)
                        : matched;
      }
      if (!condition) {
        throw std::runtime_error("case inside item has no match patterns");
      }
      const auto dispatch = builder.add_block(source_span(design, *item.stmt));
      auto body = lower_statement(builder, design, *item.stmt, procedure_kind);
      arms.push_back({
          condition,
          dispatch,
          std::move(body),
      });
    }
    if (arms.empty()) {
      throw std::runtime_error("case inside statement has no selectable items");
    }
    CfgFragment default_body;
    if (statement.defaultCase) {
      default_body = lower_statement(builder, design, *statement.defaultCase,
                                     procedure_kind);
    }
    return finish_priority_case(builder, std::move(arms),
                                std::move(default_body), source);
  }
  if (statement.condition == CaseStatementCondition::WildcardJustZ) {
    auto *selector = lower_expr(design, statement.expr);
    const auto selector_width = checked_width(
        lowered_type_width(*statement.expr.type), "casez selector");
    std::vector<PriorityArm> arms;
    arms.reserve(statement.items.size());
    for (const auto &item : statement.items) {
      const OptoSlangExpr *condition = nullptr;
      for (auto *expression : item.expressions) {
        if (!expression) {
          throw std::runtime_error("casez item contains a null pattern");
        }
        auto *pattern = lower_expr(design, *expression);
        if (pattern->kind != OPTO_SLANG_EXPR_CONSTANT ||
            !pattern->constant_has_width ||
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
            throw std::runtime_error("casez pattern contains an X bit that "
                                     "cannot be synthesized exactly");
          }
        }
        OptoSlangExpr mask_expr;
        mask_expr.kind = OPTO_SLANG_EXPR_CONSTANT;
        mask_expr.constant_has_width = true;
        mask_expr.constant_width = selector_width;
        mask_expr.constant_bits = std::move(care_mask);
        auto *mask = make_expr(design, std::move(mask_expr), *expression);
        OptoSlangExpr value_expr;
        value_expr.kind = OPTO_SLANG_EXPR_CONSTANT;
        value_expr.constant_has_width = true;
        value_expr.constant_width = selector_width;
        value_expr.constant_bits = std::move(cared_value);
        auto *value = make_expr(design, std::move(value_expr), *expression);
        auto *masked_selector = make_binary_expr(
            design, OPTO_SLANG_BINARY_BIT_AND, selector, mask, *expression);
        auto *matched = make_binary_expr(design, OPTO_SLANG_BINARY_EQ,
                                         masked_selector, value, *expression);
        condition = condition
                        ? make_binary_expr(design, OPTO_SLANG_BINARY_LOGICAL_OR,
                                           condition, matched, *expression)
                        : matched;
      }
      if (!condition) {
        throw std::runtime_error("casez item has no match patterns");
      }
      const auto dispatch = builder.add_block(source_span(design, *item.stmt));
      auto body = lower_statement(builder, design, *item.stmt, procedure_kind);
      arms.push_back({
          condition,
          dispatch,
          std::move(body),
      });
    }
    if (arms.empty()) {
      throw std::runtime_error("casez statement has no selectable items");
    }
    CfgFragment default_body;
    if (statement.defaultCase) {
      default_body = lower_statement(builder, design, *statement.defaultCase,
                                     procedure_kind);
    }
    return finish_priority_case(builder, std::move(arms),
                                std::move(default_body), source);
  }

  const auto *selector = lower_expr(design, statement.expr);
  const auto dispatch = builder.add_block(source);
  struct SwitchBody {
    std::vector<const OptoSlangExpr *> patterns;
    CfgFragment body;
  };
  std::vector<SwitchBody> bodies;
  bodies.reserve(statement.items.size());
  for (const auto &item : statement.items) {
    auto &patterns = bodies.emplace_back().patterns;
    patterns.reserve(item.expressions.size());
    for (const auto *expression : item.expressions) {
      if (!expression) {
        throw std::runtime_error("case item contains a null match expression");
      }
      patterns.push_back(lower_expr(design, *expression));
    }
    bodies.back().body =
        lower_statement(builder, design, *item.stmt, procedure_kind);
  }
  CfgFragment default_body;
  if (statement.defaultCase) {
    default_body = lower_statement(builder, design, *statement.defaultCase,
                                   procedure_kind);
  }
  const bool falls_through =
      default_body.empty() || !default_body.exits.empty() ||
      std::ranges::any_of(bodies, [](const SwitchBody &body) {
        return body.body.empty() || !body.body.exits.empty();
      });
  const auto join = falls_through
                        ? std::optional<uint32_t>(builder.add_block(source))
                        : std::nullopt;
  std::vector<OptoSlangSwitchArmData> arms;
  for (auto &body : bodies) {
    const auto target = body.body.empty() ? *join : *body.body.entry;
    for (auto *pattern : body.patterns) {
      arms.push_back({pattern, {target, source}});
    }
    if (join) {
      for (auto exit : body.body.exits) {
        builder.jump(exit, *join, source);
      }
    }
  }
  if (arms.empty()) {
    builder.jump(dispatch, default_body.empty() ? *join : *default_body.entry,
                 source);
  } else {
    builder.switch_(dispatch, selector, std::move(arms),
                    default_body.empty() ? *join : *default_body.entry, source);
  }
  if (join) {
    for (auto exit : default_body.exits) {
      builder.jump(exit, *join, source);
    }
  }
  return {
      dispatch,
      join ? std::vector<uint32_t>{*join} : std::vector<uint32_t>{},
  };
}

CfgFragment
lower_pattern_case_statement(ProcedureBuilder &builder,
                             ModuleLoweringContext &design,
                             const PatternCaseStatement &statement,
                             OptoSlangProcedureKind procedure_kind) {
  if (statement.condition == CaseStatementCondition::WildcardXOrZ) {
    throw std::runtime_error(
        "casex pattern matching is not supported for synthesis");
  }
  if (statement.condition == CaseStatementCondition::Inside) {
    throw std::runtime_error("case inside does not use pattern-case lowering");
  }

  const auto source = source_span(design, statement);
  auto *selector = lower_expr(design, statement.expr);
  selector = capture_pattern_value(design, *statement.expr.type, selector,
                                   statement.expr);
  std::vector<PriorityArm> arms;
  arms.reserve(statement.items.size());
  for (const auto &item : statement.items) {
    PatternBindingScope bindings(design.function_values);
    CfgFragment predicate_prelude;
    OptoSlangExpr *condition = nullptr;
    {
      ScopedValue expression_prelude(design.active_expression_prelude,
                                     &predicate_prelude);
      condition = lower_pattern_predicate(design, *item.pattern, selector,
                                          *statement.expr.type, statement.expr,
                                          bindings, statement.condition);
      if (item.filter) {
        CfgFragment filter_prelude;
        OptoSlangExpr *filter = nullptr;
        {
          ScopedValue filter_scope(design.active_expression_prelude,
                                   &filter_prelude);
          filter = lower_boolean_context(design, *item.filter);
        }
        if (filter_prelude.empty()) {
          condition = make_binary_expr(design, OPTO_SLANG_BINARY_LOGICAL_AND,
                                       condition, filter, *item.filter);
        } else {
          const auto filter_source = source_span(design, *item.filter);
          auto name = add_internal_net(
              design,
              "__opto_pattern_filter_" +
                  std::to_string(design.next_lvalue_instance++),
              1, false, true);
          OptoSlangExpr result_expr;
          result_expr.kind = OPTO_SLANG_EXPR_SIGNAL;
          result_expr.signal_name = intern_string(design, std::move(name));
          auto *result =
              make_expr(design, std::move(result_expr), *item.filter);
          filter_prelude = builder.sequence(
              std::move(filter_prelude),
              builder.effects({{result, filter, true, filter_source}},
                              filter_source),
              filter_source);
          auto false_branch = builder.effects(
              {{
                  result,
                  make_unsigned_constant_expr(design, 0, 1, *item.filter),
                  true,
                  filter_source,
              }},
              filter_source);
          append_expression_fragment(
              design,
              builder.conditional(condition, std::move(filter_prelude),
                                  std::move(false_branch), filter_source),
              filter_source);
          condition = result;
        }
      }
    }

    const auto dispatch = builder.add_block(source_span(design, *item.stmt));
    auto body = lower_statement(builder, design, *item.stmt, procedure_kind);
    arms.push_back({
        condition,
        dispatch,
        std::move(body),
        std::move(predicate_prelude),
    });
  }
  CfgFragment default_body;
  if (statement.defaultCase) {
    default_body = lower_statement(builder, design, *statement.defaultCase,
                                   procedure_kind);
  }
  return finish_priority_case(builder, std::move(arms), std::move(default_body),
                              source);
}

const Symbol &disable_target_symbol(const DisableStatement &statement) {
  if (statement.target.kind != ExpressionKind::ArbitrarySymbol) {
    throw std::runtime_error("disable target is not a bound symbol reference");
  }
  return *statement.target.as<ArbitrarySymbolExpression>().symbol;
}

void collect_disable_targets(const Statement &statement,
                             std::unordered_set<const Symbol *> &targets) {
  switch (statement.kind) {
  case StatementKind::Invalid:
    if (const auto *child = statement.as<InvalidStatement>().child) {
      collect_disable_targets(*child, targets);
    }
    return;
  case StatementKind::Disable:
    targets.insert(&disable_target_symbol(statement.as<DisableStatement>()));
    return;
  case StatementKind::List:
    for (const auto *child : statement.as<StatementList>().list) {
      if (child) {
        collect_disable_targets(*child, targets);
      }
    }
    return;
  case StatementKind::Block:
    collect_disable_targets(statement.as<BlockStatement>().body, targets);
    return;
  case StatementKind::Conditional: {
    const auto &conditional = statement.as<ConditionalStatement>();
    collect_disable_targets(conditional.ifTrue, targets);
    if (conditional.ifFalse) {
      collect_disable_targets(*conditional.ifFalse, targets);
    }
    return;
  }
  case StatementKind::Case: {
    const auto &case_statement = statement.as<CaseStatement>();
    for (const auto &item : case_statement.items) {
      collect_disable_targets(*item.stmt, targets);
    }
    if (case_statement.defaultCase) {
      collect_disable_targets(*case_statement.defaultCase, targets);
    }
    return;
  }
  case StatementKind::PatternCase: {
    const auto &case_statement = statement.as<PatternCaseStatement>();
    for (const auto &item : case_statement.items) {
      collect_disable_targets(*item.stmt, targets);
    }
    if (case_statement.defaultCase) {
      collect_disable_targets(*case_statement.defaultCase, targets);
    }
    return;
  }
  case StatementKind::ForLoop:
    collect_disable_targets(statement.as<ForLoopStatement>().body, targets);
    return;
  case StatementKind::RepeatLoop:
    collect_disable_targets(statement.as<RepeatLoopStatement>().body, targets);
    return;
  case StatementKind::ForeachLoop:
    collect_disable_targets(statement.as<ForeachLoopStatement>().body, targets);
    return;
  case StatementKind::WhileLoop:
    collect_disable_targets(statement.as<WhileLoopStatement>().body, targets);
    return;
  case StatementKind::DoWhileLoop:
    collect_disable_targets(statement.as<DoWhileLoopStatement>().body, targets);
    return;
  case StatementKind::ForeverLoop:
    collect_disable_targets(statement.as<ForeverLoopStatement>().body, targets);
    return;
  default:
    return;
  }
}

bool statement_disables_target(const Statement &statement,
                               const Symbol &target) {
  std::unordered_set<const Symbol *> targets;
  collect_disable_targets(statement, targets);
  return targets.contains(&target);
}

const Expression &disable_anchor(const Statement &statement,
                                 const Symbol &target) {
  if (statement.kind == StatementKind::Disable) {
    const auto &disable = statement.as<DisableStatement>();
    if (&disable_target_symbol(disable) == &target) {
      return disable.target;
    }
  }
  if (statement.kind == StatementKind::Invalid) {
    if (const auto *child = statement.as<InvalidStatement>().child) {
      return disable_anchor(*child, target);
    }
  } else if (statement.kind == StatementKind::List) {
    for (const auto *child : statement.as<StatementList>().list) {
      if (child && statement_disables_target(*child, target)) {
        return disable_anchor(*child, target);
      }
    }
  } else if (statement.kind == StatementKind::Block) {
    return disable_anchor(statement.as<BlockStatement>().body, target);
  } else if (statement.kind == StatementKind::Conditional) {
    const auto &conditional = statement.as<ConditionalStatement>();
    if (statement_disables_target(conditional.ifTrue, target)) {
      return disable_anchor(conditional.ifTrue, target);
    }
    if (conditional.ifFalse &&
        statement_disables_target(*conditional.ifFalse, target)) {
      return disable_anchor(*conditional.ifFalse, target);
    }
  } else if (statement.kind == StatementKind::Case) {
    const auto &case_statement = statement.as<CaseStatement>();
    for (const auto &item : case_statement.items) {
      if (statement_disables_target(*item.stmt, target)) {
        return disable_anchor(*item.stmt, target);
      }
    }
    if (case_statement.defaultCase &&
        statement_disables_target(*case_statement.defaultCase, target)) {
      return disable_anchor(*case_statement.defaultCase, target);
    }
  } else if (statement.kind == StatementKind::PatternCase) {
    const auto &case_statement = statement.as<PatternCaseStatement>();
    for (const auto &item : case_statement.items) {
      if (statement_disables_target(*item.stmt, target)) {
        return disable_anchor(*item.stmt, target);
      }
    }
    if (case_statement.defaultCase &&
        statement_disables_target(*case_statement.defaultCase, target)) {
      return disable_anchor(*case_statement.defaultCase, target);
    }
  } else if (statement.kind == StatementKind::ForLoop) {
    return disable_anchor(statement.as<ForLoopStatement>().body, target);
  } else if (statement.kind == StatementKind::RepeatLoop) {
    return disable_anchor(statement.as<RepeatLoopStatement>().body, target);
  } else if (statement.kind == StatementKind::ForeachLoop) {
    return disable_anchor(statement.as<ForeachLoopStatement>().body, target);
  } else if (statement.kind == StatementKind::WhileLoop) {
    return disable_anchor(statement.as<WhileLoopStatement>().body, target);
  } else if (statement.kind == StatementKind::DoWhileLoop) {
    return disable_anchor(statement.as<DoWhileLoopStatement>().body, target);
  } else if (statement.kind == StatementKind::ForeverLoop) {
    return disable_anchor(statement.as<ForeverLoopStatement>().body, target);
  }
  throw std::runtime_error(
      "disable target analysis lost its bound anchor expression");
}

bool statement_contains_return(const Statement &statement) {
  switch (statement.kind) {
  case StatementKind::Return:
    return true;
  case StatementKind::Block:
    return statement_contains_return(statement.as<BlockStatement>().body);
  case StatementKind::List:
    return std::ranges::any_of(
        statement.as<StatementList>().list, [](const Statement *child) {
          return child && statement_contains_return(*child);
        });
  case StatementKind::Conditional: {
    const auto &conditional = statement.as<ConditionalStatement>();
    return statement_contains_return(conditional.ifTrue) ||
           (conditional.ifFalse &&
            statement_contains_return(*conditional.ifFalse));
  }
  case StatementKind::Case: {
    const auto &case_statement = statement.as<CaseStatement>();
    return std::ranges::any_of(case_statement.items,
                               [](const auto &item) {
                                 return item.stmt &&
                                        statement_contains_return(*item.stmt);
                               }) ||
           (case_statement.defaultCase &&
            statement_contains_return(*case_statement.defaultCase));
  }
  case StatementKind::PatternCase: {
    const auto &case_statement = statement.as<PatternCaseStatement>();
    return std::ranges::any_of(case_statement.items,
                               [](const auto &item) {
                                 return statement_contains_return(*item.stmt);
                               }) ||
           (case_statement.defaultCase &&
            statement_contains_return(*case_statement.defaultCase));
  }
  case StatementKind::ForLoop:
    return statement_contains_return(statement.as<ForLoopStatement>().body);
  case StatementKind::RepeatLoop:
    return statement_contains_return(statement.as<RepeatLoopStatement>().body);
  case StatementKind::ForeachLoop:
    return statement_contains_return(statement.as<ForeachLoopStatement>().body);
  case StatementKind::WhileLoop:
    return statement_contains_return(statement.as<WhileLoopStatement>().body);
  case StatementKind::DoWhileLoop:
    return statement_contains_return(statement.as<DoWhileLoopStatement>().body);
  case StatementKind::ForeverLoop:
    return statement_contains_return(statement.as<ForeverLoopStatement>().body);
  default:
    return false;
  }
}

bool statement_contains_break(const Statement &statement) {
  switch (statement.kind) {
  case StatementKind::Break:
    return true;
  case StatementKind::Block:
    return statement_contains_break(statement.as<BlockStatement>().body);
  case StatementKind::List:
    return std::ranges::any_of(
        statement.as<StatementList>().list, [](const Statement *child) {
          return child && statement_contains_break(*child);
        });
  case StatementKind::Conditional: {
    const auto &conditional = statement.as<ConditionalStatement>();
    return statement_contains_break(conditional.ifTrue) ||
           (conditional.ifFalse &&
            statement_contains_break(*conditional.ifFalse));
  }
  case StatementKind::Case: {
    const auto &case_statement = statement.as<CaseStatement>();
    return std::ranges::any_of(case_statement.items,
                               [](const auto &item) {
                                 return item.stmt &&
                                        statement_contains_break(*item.stmt);
                               }) ||
           (case_statement.defaultCase &&
            statement_contains_break(*case_statement.defaultCase));
  }
  case StatementKind::PatternCase: {
    const auto &case_statement = statement.as<PatternCaseStatement>();
    return std::ranges::any_of(case_statement.items,
                               [](const auto &item) {
                                 return statement_contains_break(*item.stmt);
                               }) ||
           (case_statement.defaultCase &&
            statement_contains_break(*case_statement.defaultCase));
  }
  case StatementKind::ForLoop:
  case StatementKind::RepeatLoop:
  case StatementKind::ForeachLoop:
  case StatementKind::WhileLoop:
  case StatementKind::DoWhileLoop:
  case StatementKind::ForeverLoop:
    return false;
  default:
    return false;
  }
}

bool statement_has_static_loop_exit(const ModuleLoweringContext &design,
                                    const Statement &statement) {
  if (statement_contains_break(statement) ||
      (!design.subroutine_return_targets.empty() &&
       statement_contains_return(statement))) {
    return true;
  }
  std::unordered_set<const Symbol *> targets;
  collect_disable_targets(statement, targets);
  return std::ranges::any_of(
      design.disable_controls, [&](const DisableControl &control) {
        return control.target && targets.contains(control.target);
      });
}

bool statement_contains_continue(const Statement &statement) {
  switch (statement.kind) {
  case StatementKind::Continue:
    return true;
  case StatementKind::Block:
    return statement_contains_continue(statement.as<BlockStatement>().body);
  case StatementKind::List:
    return std::ranges::any_of(
        statement.as<StatementList>().list, [](const Statement *child) {
          return child && statement_contains_continue(*child);
        });
  case StatementKind::Conditional: {
    const auto &conditional = statement.as<ConditionalStatement>();
    return statement_contains_continue(conditional.ifTrue) ||
           (conditional.ifFalse &&
            statement_contains_continue(*conditional.ifFalse));
  }
  case StatementKind::Case: {
    const auto &case_statement = statement.as<CaseStatement>();
    return std::ranges::any_of(case_statement.items,
                               [](const auto &item) {
                                 return item.stmt &&
                                        statement_contains_continue(*item.stmt);
                               }) ||
           (case_statement.defaultCase &&
            statement_contains_continue(*case_statement.defaultCase));
  }
  case StatementKind::PatternCase: {
    const auto &case_statement = statement.as<PatternCaseStatement>();
    return std::ranges::any_of(case_statement.items,
                               [](const auto &item) {
                                 return statement_contains_continue(*item.stmt);
                               }) ||
           (case_statement.defaultCase &&
            statement_contains_continue(*case_statement.defaultCase));
  }
  case StatementKind::ForLoop:
  case StatementKind::RepeatLoop:
  case StatementKind::ForeachLoop:
  case StatementKind::WhileLoop:
  case StatementKind::DoWhileLoop:
  case StatementKind::ForeverLoop:
    return false;
  default:
    return false;
  }
}

std::vector<const VariableSymbol *>
procedural_for_variables(const ForLoopStatement &loop) {
  if (!loop.loopVars.empty()) {
    return {loop.loopVars.begin(), loop.loopVars.end()};
  }
  std::vector<const VariableSymbol *> variables;
  variables.reserve(loop.initializers.size());
  for (auto *initializer : loop.initializers) {
    if (!initializer || initializer->kind != ExpressionKind::Assignment) {
      throw std::runtime_error(
          "procedural for initializer must assign a loop variable");
    }
    const auto &lhs = initializer->as<AssignmentExpression>().left();
    if (lhs.kind != ExpressionKind::NamedValue ||
        !VariableSymbol::isKind(lhs.as<NamedValueExpression>().symbol.kind)) {
      throw std::runtime_error(
          "procedural for initializer target must be a variable");
    }
    const auto *variable =
        &lhs.as<NamedValueExpression>().symbol.as<VariableSymbol>();
    if (std::ranges::find(variables, variable) == variables.end()) {
      variables.push_back(variable);
    }
  }
  return variables;
}

const Expression *statement_anchor_expression(const Statement &statement) {
  switch (statement.kind) {
  case StatementKind::Invalid:
    if (const auto *child = statement.as<InvalidStatement>().child) {
      return statement_anchor_expression(*child);
    }
    return nullptr;
  case StatementKind::Block:
    return statement_anchor_expression(statement.as<BlockStatement>().body);
  case StatementKind::List:
    for (const auto *child : statement.as<StatementList>().list) {
      if (child) {
        if (const auto *anchor = statement_anchor_expression(*child)) {
          return anchor;
        }
      }
    }
    return nullptr;
  case StatementKind::ExpressionStatement:
    return &statement.as<ExpressionStatement>().expr;
  case StatementKind::VariableDeclaration:
    return statement.as<VariableDeclStatement>().symbol.getInitializer();
  case StatementKind::Return:
    return statement.as<ReturnStatement>().expr;
  case StatementKind::Disable:
    return &statement.as<DisableStatement>().target;
  case StatementKind::Conditional: {
    const auto &conditional = statement.as<ConditionalStatement>();
    if (conditional.conditions.empty()) {
      return nullptr;
    }
    return conditional.conditions.front().expr.get();
  }
  case StatementKind::Case:
    return &statement.as<CaseStatement>().expr;
  case StatementKind::PatternCase:
    return &statement.as<PatternCaseStatement>().expr;
  case StatementKind::ForLoop: {
    const auto &loop = statement.as<ForLoopStatement>();
    if (loop.stopExpr) {
      return loop.stopExpr;
    }
    if (!loop.steps.empty()) {
      return loop.steps.front();
    }
    if (!loop.initializers.empty()) {
      return loop.initializers.front();
    }
    for (auto *variable : loop.loopVars) {
      if (const auto *initializer = variable->getInitializer()) {
        return initializer;
      }
    }
    return statement_anchor_expression(loop.body);
  }
  case StatementKind::RepeatLoop:
    return &statement.as<RepeatLoopStatement>().count;
  case StatementKind::ForeachLoop:
    return &statement.as<ForeachLoopStatement>().arrayRef;
  case StatementKind::WhileLoop:
    return &statement.as<WhileLoopStatement>().cond;
  case StatementKind::DoWhileLoop:
    return &statement.as<DoWhileLoopStatement>().cond;
  case StatementKind::ForeverLoop:
    return statement_anchor_expression(
        statement.as<ForeverLoopStatement>().body);
  default:
    return nullptr;
  }
}

bool statement_guarantees_expression_free_break(const Statement &statement) {
  switch (statement.kind) {
  case StatementKind::Invalid:
    if (const auto *child = statement.as<InvalidStatement>().child) {
      return statement_guarantees_expression_free_break(*child);
    }
    return false;
  case StatementKind::Block:
    return statement_guarantees_expression_free_break(
        statement.as<BlockStatement>().body);
  case StatementKind::List:
    for (const auto *child : statement.as<StatementList>().list) {
      if (!child || child->kind == StatementKind::Empty) {
        continue;
      }
      if (statement_guarantees_expression_free_break(*child)) {
        return true;
      }
      if (statement_contains_continue(*child) ||
          statement_contains_return(*child)) {
        return false;
      }
    }
    return false;
  case StatementKind::Break:
    return true;
  default:
    return false;
  }
}

std::pair<DisableControl, CfgFragment>
lower_disable_control(ProcedureBuilder &builder, ModuleLoweringContext &design,
                      const Symbol &target, const Expression &anchor,
                      OptoSlangSourceSpanView source) {
  const auto ordinal = design.next_disable_instance++;
  auto name =
      add_internal_net(design, "__opto_disable_" + std::to_string(ordinal), 1,
                       false, design.active_procedure_builder != nullptr);
  OptoSlangExpr value;
  value.kind = OPTO_SLANG_EXPR_SIGNAL;
  value.signal_name = intern_string(design, std::move(name));
  auto *disabled = make_expr(design, std::move(value), anchor);
  auto *inactive =
      make_unary_expr(design, OPTO_SLANG_UNARY_LOGICAL_NOT, disabled, anchor);
  auto *false_value = make_unsigned_constant_expr(design, 0, 1, anchor);
  auto *true_value = make_unsigned_constant_expr(design, 1, 1, anchor);
  DisableControl control{
      &target,
      {disabled, inactive},
      true_value,
      false_value,
  };
  return {
      control,
      builder.effects({{disabled, false_value, true, source}}, source),
  };
}

CfgFragment
guard_disable_predicates(ProcedureBuilder &builder,
                         std::span<const OptoSlangExpr *const> predicates,
                         CfgFragment body, OptoSlangSourceSpanView source) {
  if (body.empty() || predicates.empty()) {
    return body;
  }
  for (const auto *predicate : predicates) {
    body = builder.guard(predicate, std::move(body), source);
  }
  return body;
}

CfgFragment
guard_undisabled_statements(ProcedureBuilder &builder,
                            ModuleLoweringContext &design,
                            const std::unordered_set<const Symbol *> &targets,
                            CfgFragment body, OptoSlangSourceSpanView source) {
  if (body.empty() || targets.empty()) {
    return body;
  }
  std::vector<const OptoSlangExpr *> predicates;
  for (const auto &control : design.disable_controls) {
    if (control.target && targets.contains(control.target)) {
      predicates.push_back(control.disabled.inactive);
    }
  }
  return guard_disable_predicates(builder, predicates, std::move(body), source);
}

CfgFragment lower_block_statement(ProcedureBuilder &builder,
                                  ModuleLoweringContext &design,
                                  const BlockStatement &block,
                                  OptoSlangProcedureKind procedure_kind) {
  if (block.blockKind != StatementBlockKind::Sequential) {
    throw std::runtime_error("parallel statement blocks are not supported in "
                             "synthesizable procedures at " +
                             statement_location(design, block));
  }
  if (!block.blockSymbol ||
      !statement_disables_target(block.body, *block.blockSymbol)) {
    return lower_statement(builder, design, block.body, procedure_kind);
  }

  const auto &anchor = disable_anchor(block.body, *block.blockSymbol);
  auto [control, initialization] = lower_disable_control(
      builder, design, *block.blockSymbol, anchor, source_span(design, block));
  design.disable_controls.push_back(control);
  ScopeExit leave_block([&] { design.disable_controls.pop_back(); });
  auto body = lower_statement(builder, design, block.body, procedure_kind);
  return builder.sequence(std::move(initialization), std::move(body),
                          source_span(design, block));
}

CfgFragment lower_statement_list(ProcedureBuilder &builder,
                                 ModuleLoweringContext &design,
                                 std::span<const Statement *const> statements,
                                 OptoSlangProcedureKind procedure_kind) {
  CfgFragment lowered;
  std::unordered_set<const Symbol *> prior_disable_targets;
  for (const auto *child : statements) {
    if (child) {
      auto child_lowered =
          lower_statement(builder, design, *child, procedure_kind);
      const auto source = source_span(design, *child);
      child_lowered =
          guard_undisabled_statements(builder, design, prior_disable_targets,
                                      std::move(child_lowered), source);
      lowered = builder.sequence(std::move(lowered), std::move(child_lowered),
                                 source);
      collect_disable_targets(*child, prior_disable_targets);
      if (!lowered.empty() && lowered.exits.empty()) {
        break;
      }
    }
  }
  return lowered;
}

CfgFragment lower_procedural_expression_statement(
    ProcedureBuilder &builder, ModuleLoweringContext &design,
    const Expression &expression, OptoSlangProcedureKind procedure_kind) {
  CfgFragment prelude;
  ScopedValue expression_prelude(design.active_expression_prelude, &prelude);
  ScopedValue active_builder(design.active_procedure_builder, &builder);

  CfgFragment body;
  if (expression.kind == ExpressionKind::Call &&
      !expression.as<CallExpression>().isSystemCall()) {
    body = lower_subroutine_call_statement(
        builder, design, expression.as<CallExpression>(), procedure_kind);
  } else if (expression.kind == ExpressionKind::UnaryOp) {
    const auto &unary = expression.as<UnaryExpression>();
    const bool increment = unary.op == UnaryOperator::Preincrement ||
                           unary.op == UnaryOperator::Postincrement;
    const bool decrement = unary.op == UnaryOperator::Predecrement ||
                           unary.op == UnaryOperator::Postdecrement;
    if (!increment && !decrement) {
      throw std::runtime_error("unary expression statement '" +
                               copy_string(toString(unary.op)) +
                               "' is not synthesizable at " +
                               expression_location(design, expression));
    }
    const auto width = checked_width(lowered_type_width(*unary.operand().type),
                                     "increment or decrement operand");
    OptoSlangExpr one;
    one.kind = OPTO_SLANG_EXPR_CONSTANT;
    one.constant_has_width = true;
    one.constant_width = width;
    one.constant_bits.assign(width, '0');
    one.constant_bits.back() = '1';
    auto *one_value = make_expr(design, std::move(one), expression);
    const auto source = source_span(design, expression);
    body = builder.effects(
        {{
            lower_signal_expr(design, unary.operand()),
            make_binary_expr(
                design,
                increment ? OPTO_SLANG_BINARY_ADD : OPTO_SLANG_BINARY_SUB,
                lower_expr(design, unary.operand()), one_value, expression),
            true,
            source,
        }},
        source);
  } else {
    if (expression.kind != ExpressionKind::Assignment) {
      throw std::runtime_error("expression statement '" +
                               copy_string(toString(expression.kind)) +
                               "' is not supported in procedural blocks at " +
                               expression_location(design, expression));
    }
    body = lower_assignment_statement(
        builder, design, expression.as<AssignmentExpression>(), procedure_kind);
  }
  return builder.sequence(std::move(prelude), std::move(body),
                          source_span(design, expression));
}

OptoSlangExpr *make_loop_local(ModuleLoweringContext &design, std::string role,
                               uint32_t width, bool is_signed,
                               const Expression &anchor) {
  auto name = add_internal_net(design,
                               "__opto_loop_" +
                                   std::to_string(design.next_loop_instance++) +
                                   "_" + std::move(role),
                               width, is_signed, true);
  OptoSlangExpr value;
  value.kind = OPTO_SLANG_EXPR_SIGNAL;
  value.signal_name = intern_string(design, std::move(name));
  return make_expr(design, std::move(value), anchor);
}

// Loop-declared variables have lexical identities that do not correspond to
// module storage. The source adapter maps only those declarations to locals;
// Rust owns recurrence promotion for persistent signal-backed variables.
class CyclicLoopLocals {
public:
  CyclicLoopLocals(ModuleLoweringContext &design, const Expression &anchor)
      : design_(design), anchor_(anchor),
        value_bindings_(design.function_values),
        lvalue_bindings_(design.function_lvalues) {}

  const OptoSlangExpr *bind(const VariableSymbol &variable) {
    if (auto found = locals_.find(&variable); found != locals_.end()) {
      return found->second;
    }
    value_bindings_.track(&variable);
    lvalue_bindings_.track(&variable);

    auto *local = make_loop_local(
        design_, copy_string(variable.name) + "_state",
        checked_width(lowered_type_width(variable.getType()), variable.name),
        variable.getType().isSigned(), anchor_);
    design_.function_values.insert_or_assign(&variable, local);
    design_.function_lvalues.insert_or_assign(&variable, local);
    design_.procedural_constants.erase(&variable);
    locals_.insert_or_assign(&variable, local);
    return local;
  }

private:
  ModuleLoweringContext &design_;
  const Expression &anchor_;
  ScopedSymbolMapBindings<OptoSlangExpr *> value_bindings_;
  ScopedSymbolMapBindings<OptoSlangExpr *> lvalue_bindings_;
  std::unordered_map<const VariableSymbol *, const OptoSlangExpr *> locals_;
};

struct LoweredLoopCondition {
  CfgFragment prelude;
  const OptoSlangExpr *value = nullptr;
};

LoweredLoopCondition lower_loop_condition(ProcedureBuilder &builder,
                                          ModuleLoweringContext &design,
                                          const Expression &condition) {
  CfgFragment prelude;
  ScopedValue expression_prelude(design.active_expression_prelude, &prelude);
  ScopedValue active_builder(design.active_procedure_builder, &builder);
  return {std::move(prelude), lower_boolean_context(design, condition)};
}

// Owns one canonical natural-loop skeleton. Source syntax only chooses where
// its condition is evaluated; ordinary CFG edges carry all loop semantics.
class CyclicLoopGraph {
public:
  CyclicLoopGraph(ProcedureBuilder &builder, ModuleLoweringContext &design,
                  const Statement &loop, const Statement &body,
                  const Expression &anchor, OptoSlangLoopForm form,
                  std::optional<uint32_t> external_exit = std::nullopt)
      : builder_(builder), design_(design), body_(body), anchor_(anchor),
        form_(form), source_(source_span(design, loop)),
        header_(builder.add_block(source_)),
        body_entry_(builder.add_block(source_)),
        continue_entry_(builder.add_block(source_)),
        latch_(builder.add_block(source_)),
        exit_(external_exit ? *external_exit : builder.add_block(source_)) {
    std::optional<uint32_t> parent;
    for (auto active = design_.loop_controls.rbegin();
         active != design_.loop_controls.rend(); ++active) {
      if (active->cyclic_region) {
        parent = active->cyclic_region;
        break;
      }
    }
    const auto region = builder_.add_loop_region(
        {header_, body_entry_, latch_, exit_, form_, parent, source_});
    LoopControl control;
    control.break_target = exit_;
    control.continue_target = continue_entry_;
    control.cyclic_region = region;
    design_.loop_controls.push_back(control);
    ++design_.cyclic_loop_depth;
  }

  CyclicLoopGraph(const CyclicLoopGraph &) = delete;
  CyclicLoopGraph &operator=(const CyclicLoopGraph &) = delete;

  ~CyclicLoopGraph() {
    --design_.cyclic_loop_depth;
    design_.loop_controls.pop_back();
  }

  CfgFragment finish(CfgFragment body, CfgFragment latch_prefix,
                     LoweredLoopCondition condition = {}) {
    builder_.jump(body_entry_, body.empty() ? continue_entry_ : *body.entry,
                  source_);
    for (auto block : body.exits) {
      builder_.jump(block, continue_entry_, source_);
    }

    const auto condition_value = with_activation(condition.value);
    if (form_ == OPTO_SLANG_LOOP_PRE_TEST) {
      const auto decision =
          condition.prelude.empty() ? header_ : builder_.add_block(source_);
      if (!condition.prelude.empty()) {
        builder_.jump(header_, *condition.prelude.entry, source_);
        for (auto block : condition.prelude.exits) {
          builder_.jump(block, decision, source_);
        }
      }
      builder_.branch(decision,
                      condition_value ? condition_value : true_value(),
                      body_entry_, exit_, source_);
    } else {
      builder_.jump(header_, body_entry_, source_);
    }

    if (form_ == OPTO_SLANG_LOOP_POST_TEST && !condition.prelude.empty()) {
      latch_prefix = builder_.sequence(std::move(latch_prefix),
                                       std::move(condition.prelude), source_);
    }
    builder_.jump(continue_entry_,
                  latch_prefix.empty() ? latch_ : *latch_prefix.entry, source_);
    for (auto block : latch_prefix.exits) {
      builder_.jump(block, latch_, source_);
    }
    if (form_ == OPTO_SLANG_LOOP_POST_TEST) {
      builder_.branch(latch_, condition_value ? condition_value : true_value(),
                      header_, exit_, source_);
    } else if (form_ == OPTO_SLANG_LOOP_UNCONDITIONAL) {
      if (condition_value) {
        builder_.branch(latch_, condition_value, header_, exit_, source_);
      } else {
        builder_.jump(latch_, header_, source_);
      }
    } else {
      builder_.jump(latch_, header_, source_);
    }
    return {header_, {exit_}};
  }

private:
  const OptoSlangExpr *true_value() {
    return make_unsigned_constant_expr(design_, 1, 1, anchor_);
  }

  const OptoSlangExpr *with_activation(const OptoSlangExpr *condition) {
    auto append = [&](const OptoSlangExpr *predicate) {
      if (!predicate) {
        return;
      }
      condition = condition
                      ? make_binary_expr(design_, OPTO_SLANG_BINARY_LOGICAL_AND,
                                         condition, predicate, anchor_)
                      : predicate;
    };
    std::unordered_set<const Symbol *> disable_targets;
    collect_disable_targets(body_, disable_targets);
    for (const auto &disable : design_.disable_controls) {
      if (disable.target && disable_targets.contains(disable.target)) {
        append(disable.disabled.inactive);
      }
    }
    return condition;
  }

  ProcedureBuilder &builder_;
  ModuleLoweringContext &design_;
  const Statement &body_;
  const Expression &anchor_;
  OptoSlangLoopForm form_;
  OptoSlangSourceSpanView source_;
  uint32_t header_;
  uint32_t body_entry_;
  uint32_t continue_entry_;
  uint32_t latch_;
  uint32_t exit_;
};

CfgFragment lower_for_loop_cyclic(ProcedureBuilder &builder,
                                  ModuleLoweringContext &design,
                                  const ForLoopStatement &loop,
                                  OptoSlangProcedureKind procedure_kind) {
  if (!loop.stopExpr && !statement_has_static_loop_exit(design, loop.body)) {
    throw std::runtime_error(
        "procedural for loop without a stop condition requires a lexically "
        "contained "
        "break, current-activation return, or enclosing disable at " +
        statement_location(design, loop));
  }
  const Expression *anchor = loop.stopExpr;
  if (!anchor && !loop.steps.empty()) {
    anchor = loop.steps.front();
  }
  if (!anchor) {
    anchor = statement_anchor_expression(loop.body);
  }
  if (!anchor) {
    if (statement_guarantees_expression_free_break(loop.body)) {
      return {};
    }
    throw std::runtime_error("procedural for loop has no expression that can "
                             "anchor synthesis lowering at " +
                             statement_location(design, loop));
  }

  CyclicLoopLocals locals(design, *anchor);
  for (auto *variable : loop.loopVars) {
    if (!has_registered_value(design, *variable) &&
        !design.function_values.contains(variable)) {
      locals.bind(*variable);
    }
  }
  CyclicLoopGraph graph(builder, design, loop, loop.body, *anchor,
                        OPTO_SLANG_LOOP_PRE_TEST);
  const auto source = source_span(design, loop);
  CfgFragment initialization;
  for (auto *variable : loop.loopVars) {
    const auto *initializer = variable->getInitializer();
    if (!initializer) {
      throw std::runtime_error(
          "procedural for loop variable requires an initializer at " +
          statement_location(design, loop));
    }
    const auto assignment_source = source_span(design, *initializer);
    const auto *lvalue = find_function_lvalue(design, *variable);
    if (!lvalue) {
      OptoSlangExpr registered;
      registered.kind = OPTO_SLANG_EXPR_SIGNAL;
      registered.signal_name =
          intern_string(design, registered_value_name(design, *variable));
      lvalue = make_expr(design, std::move(registered), *initializer);
    }
    initialization = builder.sequence(
        std::move(initialization),
        builder.effects({{lvalue,
                          cast_to_type(design, lower_expr(design, *initializer),
                                       variable->getType(), *initializer),
                          true, assignment_source}},
                        assignment_source),
        assignment_source);
  }
  for (auto *initializer : loop.initializers) {
    if (!initializer) {
      throw std::runtime_error(
          "procedural for loop contains a null initializer");
    }
    initialization =
        builder.sequence(std::move(initialization),
                         lower_procedural_expression_statement(
                             builder, design, *initializer, procedure_kind),
                         source_span(design, *initializer));
  }

  LoweredLoopCondition condition;
  if (loop.stopExpr) {
    condition = lower_loop_condition(builder, design, *loop.stopExpr);
  }
  auto body = lower_statement(builder, design, loop.body, procedure_kind);
  CfgFragment steps;
  for (auto *step : loop.steps) {
    if (!step) {
      throw std::runtime_error("procedural for loop contains a null step");
    }
    steps = builder.sequence(std::move(steps),
                             lower_procedural_expression_statement(
                                 builder, design, *step, procedure_kind),
                             source_span(design, *step));
  }
  auto cyclic =
      graph.finish(std::move(body), std::move(steps), std::move(condition));
  return builder.sequence(std::move(initialization), std::move(cyclic), source);
}

CfgFragment lower_repeat_loop_cyclic(ProcedureBuilder &builder,
                                     ModuleLoweringContext &design,
                                     const RepeatLoopStatement &loop,
                                     OptoSlangProcedureKind procedure_kind) {
  if (!loop.count.type->isIntegral() || !loop.count.type->isFixedSize()) {
    throw std::runtime_error("procedural repeat count requires a fixed-size "
                             "integral expression at " +
                             expression_location(design, loop.count));
  }
  const auto source = source_span(design, loop);
  const auto exact_count = evaluate_lowering_constant(design, loop.count);
  const bool has_exact_count = exact_count && exact_count.isInteger() &&
                               !exact_count.integer().hasUnknown();
  uint32_t count_width = checked_width(lowered_type_width(*loop.count.type),
                                       "procedural repeat count");
  bool count_signed = loop.count.type->isSigned();
  uint32_t maximum_count = 0;
  const OptoSlangExpr *count_value = nullptr;
  CfgFragment initialization;

  if (has_exact_count) {
    const auto count = exact_count.integer().as<uint64_t>();
    if (!count || *count > PROCEDURAL_LOOP_COUNT_CAPACITY) {
      throw std::runtime_error("procedural repeat count must be nonnegative "
                               "and fit the 32-bit transient "
                               "loop-count representation at " +
                               expression_location(design, loop.count));
    }
    if (*count == 0) {
      return {};
    }
    maximum_count = static_cast<uint32_t>(*count);
    count_width =
        std::max(1U, 64U - static_cast<uint32_t>(std::countl_zero(*count)));
    count_signed = false;
    count_value = make_unsigned_constant_expr(design, maximum_count,
                                              count_width, loop.count);
  } else {
    const auto maximum_bounded_width = count_signed ? 33u : 32u;
    if (count_width > maximum_bounded_width) {
      throw std::runtime_error(
          "runtime procedural repeat count type exceeds the 32-bit transient "
          "loop-count representation at " +
          expression_location(design, loop.count));
    }
    maximum_count =
        count_signed ? count_width == 1
                           ? 0
                           : static_cast<uint32_t>(
                                 (uint64_t{1} << (count_width - 1)) - 1)
                     : static_cast<uint32_t>((uint64_t{1} << count_width) - 1);
    CfgFragment count_prelude;
    OptoSlangExpr *lowered_count = nullptr;
    {
      ScopedValue expression_prelude(design.active_expression_prelude,
                                     &count_prelude);
      ScopedValue active_builder(design.active_procedure_builder, &builder);
      lowered_count = cast_to_expression_type(
          design, lower_expr(design, loop.count), loop.count);
    }
    auto *snapshot = make_loop_local(design, "repeat_count", count_width,
                                     count_signed, loop.count);
    initialization = builder.sequence(
        std::move(count_prelude),
        builder.effects({{snapshot, lowered_count, true, source}}, source),
        source);
    count_value = snapshot;
  }
  if (maximum_count == 0) {
    return initialization;
  }

  auto *counter = make_loop_local(design, "repeat_counter", count_width,
                                  count_signed, loop.count);
  auto *zero =
      count_signed
          ? make_signed_constant_expr(design, 0, count_width, loop.count)
          : make_unsigned_constant_expr(design, 0, count_width, loop.count);
  auto *one =
      count_signed
          ? make_signed_constant_expr(design, 1, count_width, loop.count)
          : make_unsigned_constant_expr(design, 1, count_width, loop.count);
  auto *limit = count_signed
                    ? make_signed_constant_expr(design, maximum_count,
                                                count_width, loop.count)
                    : make_unsigned_constant_expr(design, maximum_count,
                                                  count_width, loop.count);
  initialization = builder.sequence(
      std::move(initialization),
      builder.effects({{counter, zero, true, source}}, source), source);
  auto *within_domain = make_binary_expr(design, OPTO_SLANG_BINARY_LT, counter,
                                         limit, loop.count);
  auto *below_count = make_binary_expr(design, OPTO_SLANG_BINARY_GT,
                                       count_value, counter, loop.count);
  LoweredLoopCondition condition{
      {},
      make_binary_expr(design, OPTO_SLANG_BINARY_LOGICAL_AND, within_domain,
                       below_count, loop.count),
  };
  auto *increment =
      make_binary_expr(design, OPTO_SLANG_BINARY_ADD, counter, one, loop.count);
  CyclicLoopGraph graph(builder, design, loop, loop.body, loop.count,
                        OPTO_SLANG_LOOP_PRE_TEST);
  auto body = lower_statement(builder, design, loop.body, procedure_kind);
  auto latch = builder.effects({{counter, increment, true, source}}, source);
  auto cyclic =
      graph.finish(std::move(body), std::move(latch), std::move(condition));
  return builder.sequence(std::move(initialization), std::move(cyclic), source);
}

CfgFragment lower_foreach_loop_cyclic(ProcedureBuilder &builder,
                                      ModuleLoweringContext &design,
                                      const ForeachLoopStatement &loop,
                                      OptoSlangProcedureKind procedure_kind) {
  struct Dimension {
    const VariableSymbol *variable;
    ConstantRange range;
    uint32_t length;
    uint32_t stride;
  };
  std::vector<Dimension> dimensions;
  uint64_t total = 1;
  for (const auto &dimension : loop.loopDims) {
    if (!dimension.range) {
      throw std::runtime_error(
          "procedural foreach requires statically sized dimensions at " +
          statement_location(design, loop));
    }
    if (!dimension.loopVar) {
      continue;
    }
    const auto length = dimension.range->width();
    if (length == 0 || length > UINT32_MAX ||
        total > PROCEDURAL_LOOP_COUNT_CAPACITY / length) {
      throw std::runtime_error(
          "procedural foreach exceeds the 32-bit transient loop-count "
          "representation at " +
          statement_location(design, loop));
    }
    total *= length;
    dimensions.push_back({dimension.loopVar, *dimension.range,
                          static_cast<uint32_t>(length), 1});
  }
  if (dimensions.empty()) {
    return {};
  }
  uint32_t stride = 1;
  for (auto iterator = dimensions.rbegin(); iterator != dimensions.rend();
       ++iterator) {
    iterator->stride = stride;
    stride *= iterator->length;
  }
  const auto count = static_cast<uint32_t>(total);
  const auto width =
      std::max(1U, 64U - static_cast<uint32_t>(std::countl_zero(total)));
  const auto source = source_span(design, loop);
  auto *counter =
      make_loop_local(design, "foreach_counter", width, false, loop.arrayRef);
  auto *zero = make_unsigned_constant_expr(design, 0, width, loop.arrayRef);
  auto *one = make_unsigned_constant_expr(design, 1, width, loop.arrayRef);
  auto *limit =
      make_unsigned_constant_expr(design, count, width, loop.arrayRef);
  CyclicLoopLocals locals(design, loop.arrayRef);
  for (const auto &dimension : dimensions) {
    locals.bind(*dimension.variable);
  }
  CyclicLoopGraph graph(builder, design, loop, loop.body, loop.arrayRef,
                        OPTO_SLANG_LOOP_PRE_TEST);
  auto initialization =
      builder.effects({{counter, zero, true, source}}, source);
  CfgFragment indices;
  for (const auto &dimension : dimensions) {
    const OptoSlangExpr *offset =
        make_unsigned_cast_expr(design, counter, 32, loop.arrayRef);
    if (dimension.stride != 1) {
      offset =
          make_binary_expr(design, OPTO_SLANG_BINARY_DIV, offset,
                           make_unsigned_constant_expr(design, dimension.stride,
                                                       32, loop.arrayRef),
                           loop.arrayRef);
    }
    if (dimension.length != 1) {
      offset =
          make_binary_expr(design, OPTO_SLANG_BINARY_MOD, offset,
                           make_unsigned_constant_expr(design, dimension.length,
                                                       32, loop.arrayRef),
                           loop.arrayRef);
    } else {
      offset = make_unsigned_constant_expr(design, 0, 32, loop.arrayRef);
    }
    auto *left = make_signed_constant_expr(design, dimension.range.left, 32,
                                           loop.arrayRef);
    auto *index =
        make_binary_expr(design,
                         dimension.range.isDescending() ? OPTO_SLANG_BINARY_SUB
                                                        : OPTO_SLANG_BINARY_ADD,
                         left, offset, loop.arrayRef);
    index = make_signed_cast_expr(design, index, 32, loop.arrayRef);
    indices = builder.sequence(
        std::move(indices),
        builder.effects({{design.function_lvalues.at(dimension.variable), index,
                          true, source}},
                        source),
        source);
  }
  auto body = builder.sequence(
      std::move(indices),
      lower_statement(builder, design, loop.body, procedure_kind), source);
  LoweredLoopCondition condition{
      {},
      make_binary_expr(design, OPTO_SLANG_BINARY_LT, counter, limit,
                       loop.arrayRef),
  };
  auto latch = builder.effects({{counter,
                                 make_binary_expr(design, OPTO_SLANG_BINARY_ADD,
                                                  counter, one, loop.arrayRef),
                                 true, source}},
                               source);
  auto cyclic =
      graph.finish(std::move(body), std::move(latch), std::move(condition));
  return builder.sequence(std::move(initialization), std::move(cyclic), source);
}

CfgFragment lower_condition_loop_cyclic(ProcedureBuilder &builder,
                                        ModuleLoweringContext &design,
                                        const Statement &loop,
                                        const Expression &condition_expression,
                                        const Statement &body_statement,
                                        bool condition_precedes_body,
                                        OptoSlangProcedureKind procedure_kind) {
  CyclicLoopGraph graph(builder, design, loop, body_statement,
                        condition_expression,
                        condition_precedes_body ? OPTO_SLANG_LOOP_PRE_TEST
                                                : OPTO_SLANG_LOOP_POST_TEST);
  auto condition = lower_loop_condition(builder, design, condition_expression);
  auto body = lower_statement(builder, design, body_statement, procedure_kind);
  auto cyclic = graph.finish(std::move(body), {}, std::move(condition));
  return cyclic;
}

CfgFragment lower_forever_loop_cyclic(ProcedureBuilder &builder,
                                      ModuleLoweringContext &design,
                                      const ForeverLoopStatement &loop,
                                      OptoSlangProcedureKind procedure_kind) {
  if (!statement_has_static_loop_exit(design, loop.body)) {
    throw std::runtime_error(
        "procedural forever loop requires a lexically contained break, "
        "current-activation return, or enclosing disable at " +
        statement_location(design, loop));
  }
  const auto *anchor = statement_anchor_expression(loop.body);
  if (!anchor) {
    if (statement_guarantees_expression_free_break(loop.body)) {
      return {};
    }
    throw std::runtime_error(
        "procedural forever loop cannot anchor activation state at " +
        statement_location(design, loop));
  }
  std::optional<uint32_t> activation_exit;
  if (!design.subroutine_return_targets.empty() &&
      statement_contains_return(loop.body) &&
      !statement_contains_break(loop.body)) {
    std::unordered_set<const Symbol *> disable_targets;
    collect_disable_targets(loop.body, disable_targets);
    const bool has_active_disable = std::ranges::any_of(
        design.disable_controls, [&](const DisableControl &control) {
          return control.target && disable_targets.contains(control.target);
        });
    if (!has_active_disable) {
      activation_exit = design.subroutine_return_targets.back();
    }
  }
  CyclicLoopGraph graph(builder, design, loop, loop.body, *anchor,
                        OPTO_SLANG_LOOP_UNCONDITIONAL, activation_exit);
  auto body = lower_statement(builder, design, loop.body, procedure_kind);
  auto cyclic = graph.finish(std::move(body), {});
  return cyclic;
}

CfgFragment lower_statement_impl(ProcedureBuilder &builder,
                                 ModuleLoweringContext &design,
                                 const Statement &stmt,
                                 OptoSlangProcedureKind procedure_kind) {
  switch (stmt.kind) {
  case StatementKind::Invalid: {
    auto *child = stmt.as<InvalidStatement>().child;
    if (!child) {
      throw std::runtime_error("invalid statement at " +
                               statement_location(design, stmt));
    }
    return lower_statement(builder, design, *child, procedure_kind);
  }
  case StatementKind::Empty:
    return {};
  case StatementKind::List:
    return lower_statement_list(builder, design, stmt.as<StatementList>().list,
                                procedure_kind);
  case StatementKind::Block:
    return lower_block_statement(builder, design, stmt.as<BlockStatement>(),
                                 procedure_kind);
  case StatementKind::VariableDeclaration: {
    const auto &symbol = stmt.as<VariableDeclStatement>().symbol;
    if (symbol.getInitializer() &&
        symbol.lifetime != VariableLifetime::Automatic) {
      throw LoweringFailure(
          OPTO_SLANG_LOWERING_UNSUPPORTED_PROFILE, 1, stmt.sourceRange.start(),
          "static procedural declaration initialization for '" +
              copy_string(symbol.name) +
              "' is time-zero state and is outside the explicit-reset "
              "synthesis profile at " +
              statement_location(design, stmt));
    }
    if (auto *initializer = symbol.getInitializer()) {
      OptoSlangExpr lhs;
      lhs.kind = OPTO_SLANG_EXPR_SIGNAL;
      lhs.signal_name =
          intern_string(design, registered_value_name(design, symbol));
      const auto source = source_span(design, stmt);
      return builder.effects(
          {{
              make_expr(design, std::move(lhs), *initializer),
              cast_to_type(design, lower_expr(design, *initializer),
                           symbol.getType(), *initializer),
              true,
              source,
          }},
          source);
    }
    return {};
  }
  case StatementKind::ForLoop:
    return lower_for_loop_cyclic(builder, design, stmt.as<ForLoopStatement>(),
                                 procedure_kind);
  case StatementKind::RepeatLoop:
    return lower_repeat_loop_cyclic(
        builder, design, stmt.as<RepeatLoopStatement>(), procedure_kind);
  case StatementKind::ForeachLoop:
    return lower_foreach_loop_cyclic(
        builder, design, stmt.as<ForeachLoopStatement>(), procedure_kind);
  case StatementKind::WhileLoop: {
    const auto &loop = stmt.as<WhileLoopStatement>();
    return lower_condition_loop_cyclic(builder, design, loop, loop.cond,
                                       loop.body, true, procedure_kind);
  }
  case StatementKind::DoWhileLoop: {
    const auto &loop = stmt.as<DoWhileLoopStatement>();
    return lower_condition_loop_cyclic(builder, design, loop, loop.cond,
                                       loop.body, false, procedure_kind);
  }
  case StatementKind::ForeverLoop:
    return lower_forever_loop_cyclic(
        builder, design, stmt.as<ForeverLoopStatement>(), procedure_kind);
  case StatementKind::Continue: {
    if (design.loop_controls.empty()) {
      throw std::runtime_error(
          "continue statement has no active synthesizable loop");
    }
    const auto &control = design.loop_controls.back();
    const auto source = source_span(design, stmt);
    if (!control.continue_target) {
      throw std::logic_error("active loop has no continue target");
    }
    const auto transfer = builder.add_block(source);
    builder.jump(transfer, *control.continue_target, source);
    return {transfer, {}};
  }
  case StatementKind::Break: {
    if (design.loop_controls.empty()) {
      throw std::runtime_error(
          "break statement has no active synthesizable loop");
    }
    const auto &control = design.loop_controls.back();
    const auto source = source_span(design, stmt);
    if (!control.break_target) {
      throw std::logic_error("active loop has no break target");
    }
    const auto transfer = builder.add_block(source);
    builder.jump(transfer, *control.break_target, source);
    return {transfer, {}};
  }
  case StatementKind::Disable: {
    const auto &disable = stmt.as<DisableStatement>();
    if (disable.target.kind != ExpressionKind::ArbitrarySymbol) {
      throw std::runtime_error(
          "disable target is not a bound symbol reference at " +
          statement_location(design, stmt));
    }
    const auto &target = disable.target.as<ArbitrarySymbolExpression>();
    if (target.hierRef.target) {
      throw std::runtime_error("hierarchical disable is not supported in "
                               "synthesizable procedures at " +
                               statement_location(design, stmt));
    }
    if (target.symbol->kind == SymbolKind::Subroutine &&
        (design.function_stack.empty() ||
         design.function_stack.back() != target.symbol)) {
      throw std::runtime_error(
          "a synthesizable task can disable only its current activation at " +
          statement_location(design, stmt));
    }
    auto found = std::ranges::find_if(design.disable_controls.rbegin(),
                                      design.disable_controls.rend(),
                                      [&](const DisableControl &control) {
                                        return control.target == target.symbol;
                                      });
    if (found == design.disable_controls.rend()) {
      throw std::runtime_error(
          "disable target '" + copy_string(target.symbol->name) +
          "' is not an active lexical synthesis scope at " +
          statement_location(design, stmt));
    }
    const auto source = source_span(design, stmt);
    return builder.effects(
        {{found->disabled.value, found->true_value, true, source}}, source);
  }
  case StatementKind::Return: {
    if (design.subroutine_return_targets.empty() ||
        design.function_stack.empty()) {
      throw std::runtime_error(
          "return statement appears outside an inlined subroutine at " +
          statement_location(design, stmt));
    }
    const auto &returned = stmt.as<ReturnStatement>();
    const auto &subroutine = *design.function_stack.back();
    const bool value_function =
        subroutine.subroutineKind == SubroutineKind::Function &&
        !subroutine.getReturnType().isVoid();
    if (returned.expr && !value_function) {
      throw std::runtime_error(
          "valued return is not permitted in task or void function '" +
          copy_string(subroutine.name) + "' at " +
          statement_location(design, stmt));
    }
    if (!returned.expr && value_function) {
      throw std::runtime_error(
          "valueless return is not permitted in value-returning function '" +
          copy_string(subroutine.name) + "' at " +
          statement_location(design, stmt));
    }
    const auto source = source_span(design, stmt);
    std::vector<OptoSlangEffectData> effects;
    if (returned.expr) {
      if (design.function_returns.empty()) {
        throw std::logic_error(
            "value-returning activation has no return variable");
      }
      const auto *return_variable = design.function_returns.back();
      OptoSlangExpr lhs;
      lhs.kind = OPTO_SLANG_EXPR_SIGNAL;
      lhs.signal_name = intern_string(
          design, registered_value_name(design, *return_variable));
      effects.push_back({
          make_expr(design, std::move(lhs), *returned.expr),
          lower_expr(design, *returned.expr),
          true,
          source,
      });
    }
    auto transfer = effects.empty()
                        ? CfgFragment{builder.add_block(source), {}}
                        : builder.effects(std::move(effects), source);
    builder.jump(*transfer.entry, design.subroutine_return_targets.back(),
                 source);
    transfer.exits.clear();
    return transfer;
  }
  case StatementKind::Conditional:
    return lower_conditional_statement(
        builder, design, stmt.as<ConditionalStatement>(), procedure_kind);
  case StatementKind::Case:
    return lower_case_statement(builder, design, stmt.as<CaseStatement>(),
                                procedure_kind);
  case StatementKind::PatternCase:
    return lower_pattern_case_statement(
        builder, design, stmt.as<PatternCaseStatement>(), procedure_kind);
  case StatementKind::ExpressionStatement:
    return lower_procedural_expression_statement(
        builder, design, stmt.as<ExpressionStatement>().expr, procedure_kind);
  default: {
    std::string context;
    if (!design.function_stack.empty()) {
      context = " while inlining function '" +
                copy_string(design.function_stack.back()->name) + "'";
    } else {
      context =
          " in module '" + copy_string(design.body.getDefinition().name) + "'";
    }
    throw std::runtime_error("unsupported statement '" +
                             copy_string(toString(stmt.kind)) +
                             "' in procedural block" + context + " at " +
                             statement_location(design, stmt));
  }
  }
}

CfgFragment lower_statement(ProcedureBuilder &builder,
                            ModuleLoweringContext &design,
                            const Statement &stmt,
                            OptoSlangProcedureKind procedure_kind) {
  CfgFragment prelude;
  ScopedValue expression_prelude(design.active_expression_prelude, &prelude);
  ScopedValue active_builder(design.active_procedure_builder, &builder);
  auto body = lower_statement_impl(builder, design, stmt, procedure_kind);
  return builder.sequence(std::move(prelude), std::move(body),
                          source_span(design, stmt));
}

OptoSlangEventData lower_flop_event(ModuleLoweringContext &design,
                                    const TimingControl &timing) {
  if (timing.kind != TimingControlKind::SignalEvent) {
    throw std::runtime_error("edge-triggered procedural block event must be a "
                             "posedge or negedge signal");
  }
  const auto &event = timing.as<SignalEventControl>();

  OptoSlangEdge edge;
  switch (event.edge) {
  case EdgeKind::PosEdge:
    edge = OPTO_SLANG_EDGE_POS;
    break;
  case EdgeKind::NegEdge:
    edge = OPTO_SLANG_EDGE_NEG;
    break;
  default:
    throw std::runtime_error("edge-triggered procedural block event must be a "
                             "posedge or negedge signal");
  }
  return OptoSlangEventData{
      edge,
      lower_signal_expr(design, event.expr),
      nullptr,
      source_span(design, event.expr),
  };
}

void lower_flop_events(ModuleLoweringContext &design,
                       const TimingControl &timing,
                       std::vector<OptoSlangEventData> &events,
                       std::vector<const Expression *> &qualifiers,
                       std::vector<const Expression *> &event_expressions) {
  if (timing.kind == TimingControlKind::EventList) {
    const auto &list = timing.as<EventListControl>();
    if (list.events.empty()) {
      throw std::runtime_error(
          "edge-triggered procedural block has an empty event list");
    }
    for (auto *event : list.events) {
      if (!event) {
        throw std::runtime_error(
            "edge-triggered procedural block has a null event");
      }
      lower_flop_events(design, *event, events, qualifiers, event_expressions);
    }
    return;
  }
  const auto &event = timing.as<SignalEventControl>();
  events.push_back(lower_flop_event(design, timing));
  qualifiers.push_back(event.iffCondition);
  event_expressions.push_back(&event.expr);
}

std::optional<bool>
event_level_qualifier_value(const OptoSlangEventData &event,
                            const Expression &event_expression,
                            const Expression &qualifier) {
  const Expression *event_operand = &event_expression;
  while (event_operand->kind == ExpressionKind::Conversion) {
    event_operand = &event_operand->as<ConversionExpression>().operand();
  }
  const Expression *expression = &qualifier;
  bool inverted = false;
  while (expression->kind == ExpressionKind::Conversion) {
    expression = &expression->as<ConversionExpression>().operand();
  }
  if (expression->kind == ExpressionKind::UnaryOp) {
    const auto &unary = expression->as<UnaryExpression>();
    if (unary.op != UnaryOperator::LogicalNot &&
        unary.op != UnaryOperator::BitwiseNot) {
      return std::nullopt;
    }
    inverted = true;
    expression = &unary.operand();
    while (expression->kind == ExpressionKind::Conversion) {
      expression = &expression->as<ConversionExpression>().operand();
    }
  }
  if (!event_operand->isEquivalentTo(*expression)) {
    return std::nullopt;
  }
  const bool post_edge_level = event.edge == OPTO_SLANG_EDGE_POS;
  return inverted ? !post_edge_level : post_edge_level;
}

void canonicalize_constant_event_qualifiers(
    ModuleLoweringContext &design, std::vector<OptoSlangEventData> &events,
    std::vector<const Expression *> &qualifiers,
    std::vector<const Expression *> &event_expressions) {
  if (events.size() != qualifiers.size() ||
      events.size() != event_expressions.size()) {
    throw std::runtime_error("event qualifier storage is inconsistent");
  }
  size_t destination = 0;
  for (size_t source = 0; source < events.size(); ++source) {
    auto *qualifier = qualifiers[source];
    if (qualifier) {
      auto constant = constant_boolean_value(design, *qualifier);
      if (!constant) {
        constant = event_level_qualifier_value(
            events[source], *event_expressions[source], *qualifier);
      }
      if (constant == false) {
        continue;
      }
      if (constant == true) {
        qualifier = nullptr;
      }
    }
    if (destination != source) {
      events[destination] = std::move(events[source]);
      event_expressions[destination] = event_expressions[source];
    }
    qualifiers[destination] = qualifier;
    ++destination;
  }
  events.resize(destination);
  qualifiers.resize(destination);
  event_expressions.resize(destination);
}

bool is_combinational_sensitivity(const TimingControl &timing) {
  switch (timing.kind) {
  case TimingControlKind::ImplicitEvent:
    return true;
  case TimingControlKind::SignalEvent: {
    const auto &event = timing.as<SignalEventControl>();
    return event.edge == EdgeKind::None && !event.iffCondition;
  }
  case TimingControlKind::EventList: {
    const auto &list = timing.as<EventListControl>();
    return !list.events.empty() &&
           std::ranges::all_of(list.events, [](const TimingControl *event) {
             return event && is_combinational_sensitivity(*event);
           });
  }
  default:
    return false;
  }
}

bool is_edge_sensitivity(const TimingControl &timing) {
  if (timing.kind == TimingControlKind::EventList) {
    const auto &list = timing.as<EventListControl>();
    return !list.events.empty() &&
           std::ranges::all_of(list.events, [](const TimingControl *event) {
             return event && is_edge_sensitivity(*event);
           });
  }
  if (timing.kind != TimingControlKind::SignalEvent) {
    return false;
  }
  const auto &event = timing.as<SignalEventControl>();
  return event.edge == EdgeKind::PosEdge || event.edge == EdgeKind::NegEdge;
}

OptoSlangProcedureData lower_procedure(ModuleLoweringContext &design,
                                       const InstanceBodySymbol &body,
                                       const ProceduralBlockSymbol &process) {
  ProcedureBuilder builder;
  OptoSlangProcedureKind kind;
  std::vector<OptoSlangEventData> events;
  std::vector<const Expression *> event_qualifiers;
  std::vector<const Expression *> event_expressions;
  const Statement *statement = nullptr;
  if (process.procedureKind == ProceduralBlockKind::AlwaysComb) {
    kind = OPTO_SLANG_PROCEDURE_COMB;
    statement = &process.getBody();
  } else if (process.procedureKind == ProceduralBlockKind::AlwaysLatch) {
    kind = OPTO_SLANG_PROCEDURE_LATCH;
    statement = &process.getBody();
  } else if (process.procedureKind == ProceduralBlockKind::AlwaysFF) {
    kind = OPTO_SLANG_PROCEDURE_FLOP;
    if (process.getBody().kind != StatementKind::Timed) {
      throw std::runtime_error("edge-triggered procedural block requires a "
                               "posedge or negedge event list");
    }
    const auto &timed = process.getBody().as<TimedStatement>();
    lower_flop_events(design, timed.timing, events, event_qualifiers,
                      event_expressions);
    statement = &timed.stmt;
  } else if (process.procedureKind == ProceduralBlockKind::Always) {
    if (process.getBody().kind != StatementKind::Timed) {
      throw std::runtime_error(
          "always procedural block requires an event control for synthesis");
    }
    const auto &timed = process.getBody().as<TimedStatement>();
    if (is_edge_sensitivity(timed.timing)) {
      kind = OPTO_SLANG_PROCEDURE_FLOP;
      lower_flop_events(design, timed.timing, events, event_qualifiers,
                        event_expressions);
    } else if (is_combinational_sensitivity(timed.timing)) {
      kind = OPTO_SLANG_PROCEDURE_COMB_OR_LATCH;
    } else {
      throw std::runtime_error("always event control is not a supported "
                               "combinational or edge sensitivity list");
    }
    statement = &timed.stmt;
  } else {
    throw std::runtime_error(unsupported_member_message(body, process));
  }
  if (kind == OPTO_SLANG_PROCEDURE_FLOP) {
    canonicalize_constant_event_qualifiers(design, events, event_qualifiers,
                                           event_expressions);
    if (events.empty()) {
      throw std::runtime_error(
          "edge-triggered procedural block has no reachable event after "
          "constant iff qualification");
    }
  }
  for (size_t index = 0; index < events.size(); ++index) {
    const auto *qualifier = event_qualifiers[index];
    if (!qualifier) {
      continue;
    }
    CfgFragment prelude;
    {
      ScopedValue expression_prelude(design.active_expression_prelude,
                                     &prelude);
      ScopedValue active_builder(design.active_procedure_builder, &builder);
      events[index].qualifier = lower_boolean_context(design, *qualifier);
    }
    if (!prelude.empty()) {
      throw std::runtime_error(
          "side-effecting event iff qualifier is not supported for synthesis");
    }
  }
  const auto source = source_span(design, *statement);
  auto lowered = lower_statement(builder, design, *statement, kind);
  return builder.finish(std::move(lowered), kind, std::move(events), source);
}
void collect_function_locals(const Scope &scope,
                             std::vector<const VariableSymbol *> &locals) {
  for (const auto &member : scope.members()) {
    if (member.kind == SymbolKind::Variable) {
      locals.push_back(&member.as<VariableSymbol>());
    } else if (member.kind == SymbolKind::StatementBlock) {
      collect_function_locals(member.as<StatementBlockSymbol>(), locals);
    }
  }
}

const ValueSymbol *expression_root_value(const Expression &expression) {
  switch (expression.kind) {
  case ExpressionKind::NamedValue:
    return &expression.as<NamedValueExpression>().symbol;
  case ExpressionKind::ElementSelect:
    return expression_root_value(
        expression.as<ElementSelectExpression>().value());
  case ExpressionKind::RangeSelect:
    return expression_root_value(
        expression.as<RangeSelectExpression>().value());
  case ExpressionKind::MemberAccess:
    return expression_root_value(
        expression.as<MemberAccessExpression>().value());
  default:
    return nullptr;
  }
}

bool statement_assigns_value(const Statement &statement,
                             const ValueSymbol &value) {
  struct WriteVisitor : ASTVisitor<WriteVisitor, VisitFlags::AllGood> {
    explicit WriteVisitor(const ValueSymbol &value) : value(value) {}

    void handle(const AssignmentExpression &expression) {
      found = found || expression_root_value(expression.left()) == &value;
      if (!found) {
        visitDefault(expression);
      }
    }

    void handle(const UnaryExpression &expression) {
      found = found || ((expression.op == UnaryOperator::Preincrement ||
                         expression.op == UnaryOperator::Postincrement ||
                         expression.op == UnaryOperator::Predecrement ||
                         expression.op == UnaryOperator::Postdecrement) &&
                        expression_root_value(expression.operand()) == &value);
      if (!found) {
        visitDefault(expression);
      }
    }

    void handle(const CallExpression &expression) {
      if (const auto *selected =
              std::get_if<const SubroutineSymbol *>(&expression.subroutine);
          selected && *selected) {
        const auto arguments = (*selected)->getArguments();
        const auto actuals = expression.arguments();
        for (size_t index = 0;
             index < std::min(arguments.size(), actuals.size()); ++index) {
          if (arguments[index] && actuals[index] &&
              arguments[index]->direction != ArgumentDirection::In &&
              expression_root_value(call_output_lvalue(*actuals[index])) ==
                  &value) {
            found = true;
            return;
          }
        }
      }
      visitDefault(expression);
    }

    const ValueSymbol &value;
    bool found = false;
  } visitor(value);
  statement.visit(visitor);
  return visitor.found;
}

bool module_has_value_name(const OptoSlangModulePayload &module,
                           std::string_view name) {
  return std::ranges::any_of(
             module.ports,
             [name](const auto &port) { return port.name == name; }) ||
         std::ranges::any_of(
             module.nets, [name](const auto &net) { return net.name == name; });
}

std::string allocate_function_value_name(ModuleLoweringContext &design,
                                         const SubroutineSymbol &function,
                                         std::string_view local) {
  while (true) {
    const auto ordinal = design.next_function_instance++;
    auto name = "__opto_fn_" + std::to_string(ordinal) + "_" +
                copy_string(function.name) + "_" + copy_string(local);
    if (!module_has_value_name(design.module, name)) {
      return name;
    }
  }
}

OptoSlangExpr *bind_ref_argument(ModuleLoweringContext &design,
                                 const SubroutineSymbol &function,
                                 const FormalArgumentSymbol &argument,
                                 const Expression &actual, bool process_local,
                                 std::vector<OptoSlangEffectData> &initializers,
                                 OptoSlangSourceSpanView source) {
  const auto &lvalue = call_output_lvalue(actual);
  auto *alias = lower_signal_expr(design, lvalue);
  if (alias->kind != OPTO_SLANG_EXPR_DYNAMIC_EXTRACT) {
    return alias;
  }

  auto selector_local = copy_string(argument.name) + "_ref_selector";
  auto selector_name =
      allocate_function_value_name(design, function, selector_local);
  selector_name = add_internal_net(design, std::move(selector_name),
                                   alias->dynamic_extract_offset_width, false,
                                   process_local);
  OptoSlangExpr selector_lhs;
  selector_lhs.kind = OPTO_SLANG_EXPR_SIGNAL;
  selector_lhs.signal_name = intern_string(design, selector_name);
  initializers.push_back({
      make_expr(design, std::move(selector_lhs), lvalue),
      alias->dynamic_extract_offset,
      true,
      source,
  });
  OptoSlangExpr selector_value;
  selector_value.kind = OPTO_SLANG_EXPR_SIGNAL;
  selector_value.signal_name = intern_string(design, std::move(selector_name));
  auto frozen = *alias;
  frozen.dynamic_extract_offset =
      make_expr(design, std::move(selector_value), lvalue);
  return make_expr(design, std::move(frozen), lvalue);
}

OptoSlangExpr *lower_function_call(ModuleLoweringContext &design,
                                   const CallExpression &call) {
  const auto *selected =
      std::get_if<const SubroutineSymbol *>(&call.subroutine);
  if (!selected || !*selected) {
    throw std::runtime_error("unsupported non-system call '" +
                             copy_string(call.getSubroutineName()) + "'");
  }
  const auto &function =
      resolve_synthesizable_subroutine(**selected, call.sourceRange.start());
  if (function.subroutineKind != SubroutineKind::Function ||
      function.isVirtual() ||
      function.flags.has(MethodFlags::DPIImport | MethodFlags::BuiltIn)) {
    throw std::runtime_error("subroutine '" + copy_string(function.name) +
                             "' is not a synthesizable function");
  }
  auto arguments = function.getArguments();
  auto actuals = call.arguments();
  if (arguments.size() != actuals.size()) {
    throw std::runtime_error("function call argument count does not match its "
                             "elaborated declaration");
  }
  std::vector<OptoSlangExpr *> lowered_actuals;
  std::vector<ConstantValue> constant_actuals;
  lowered_actuals.reserve(actuals.size());
  constant_actuals.reserve(actuals.size());
  for (auto *actual : actuals) {
    if (!actual) {
      throw std::runtime_error("function call contains an unbound argument");
    }
    lowered_actuals.push_back(lower_expr(design, *actual));
    constant_actuals.push_back(evaluate_lowering_constant(design, *actual));
  }
  constexpr size_t max_recursive_depth = 256;
  if (static_cast<size_t>(std::ranges::count(
          design.function_stack, &function)) >= max_recursive_depth) {
    throw std::runtime_error("recursive synthesizable function '" +
                             copy_string(function.name) +
                             "' does not reach a constant base case within " +
                             std::to_string(max_recursive_depth) + " calls");
  }
  const bool process_local = design.active_procedure_builder != nullptr;
  const auto source = source_span(design, call);
  const auto *return_variable = function.returnValVar;
  if (!return_variable) {
    throw std::runtime_error("function '" + copy_string(function.name) +
                             "' has no return variable");
  }
  std::vector<const VariableSymbol *> locals;
  collect_function_locals(function, locals);
  ScopedSymbolMapBindings function_value_bindings(design.function_values);
  ScopedSymbolMapBindings function_lvalue_bindings(design.function_lvalues);
  ScopedSymbolMapBindings constant_bindings(design.procedural_constants);
  ScopedSymbolMapBindings name_bindings(design.value_names);
  for (auto *argument : arguments) {
    function_value_bindings.track(argument);
    function_lvalue_bindings.track(argument);
    constant_bindings.track(argument);
    name_bindings.track(argument);
  }
  function_value_bindings.track(return_variable);
  constant_bindings.track(return_variable);
  name_bindings.track(return_variable);
  for (auto *local : locals) {
    function_value_bindings.track(local);
    constant_bindings.track(local);
    name_bindings.track(local);
  }
  design.function_stack.push_back(&function);

  std::vector<const ValueSymbol *> installed_values;
  std::vector<const ValueSymbol *> installed_lvalues;
  std::vector<const ValueSymbol *> installed_constants;
  std::vector<const ValueSymbol *> installed_names;
  std::vector<OptoSlangEffectData> argument_initializers;
  std::string return_name;
  bool return_scope_pushed = false;
  ScopeExit leave_function([&] {
    if (return_scope_pushed) {
      design.function_returns.pop_back();
      design.subroutine_return_targets.pop_back();
    }
    for (auto *symbol : installed_values) {
      design.function_values.erase(symbol);
    }
    for (auto *symbol : installed_lvalues) {
      design.function_lvalues.erase(symbol);
    }
    for (auto *symbol : installed_constants) {
      design.procedural_constants.erase(symbol);
    }
    for (auto *symbol : installed_names) {
      design.value_names.erase(symbol);
    }
    design.function_stack.pop_back();
  });

  for (size_t index = 0; index < arguments.size(); ++index) {
    auto *argument = arguments[index];
    if (!argument) {
      throw std::runtime_error(
          "synthesizable function contains an unbound argument");
    }
    if (argument->direction == ArgumentDirection::Ref) {
      auto *alias =
          bind_ref_argument(design, function, *argument, *actuals[index],
                            process_local, argument_initializers, source);
      design.function_lvalues.insert_or_assign(argument, alias);
      installed_lvalues.push_back(argument);
      continue;
    }
    if (argument->direction != ArgumentDirection::In) {
      throw std::runtime_error("synthesizable value-returning function "
                               "arguments must be input or ref");
    }
    const bool is_written =
        statement_assigns_value(function.getBody(), *argument);
    if (is_written) {
      auto name =
          allocate_function_value_name(design, function, argument->name);
      name = add_internal_net(
          design, std::move(name),
          checked_width(lowered_type_width(argument->getType()),
                        argument->name),
          argument->getType().isSigned(), process_local);
      design.value_names.insert_or_assign(argument, name);
      installed_names.push_back(argument);
      OptoSlangExpr lhs;
      lhs.kind = OPTO_SLANG_EXPR_SIGNAL;
      lhs.signal_name = intern_string(design, std::move(name));
      argument_initializers.push_back({
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
      design.procedural_constants.insert_or_assign(argument,
                                                   constant_actuals[index]);
      installed_constants.push_back(argument);
    }
  }

  return_name = allocate_function_value_name(design, function, "return");
  return_name = add_internal_net(
      design, std::move(return_name),
      checked_width(lowered_type_width(function.getReturnType()),
                    function.name),
      function.getReturnType().isSigned(), process_local);
  design.value_names.insert_or_assign(return_variable, return_name);
  installed_names.push_back(return_variable);

  for (auto *local : locals) {
    if (local == return_variable ||
        std::ranges::find(arguments, local) != arguments.end()) {
      continue;
    }
    auto name = allocate_function_value_name(design, function, local->name);
    name = add_internal_net(
        design, std::move(name),
        checked_width(lowered_type_width(local->getType()), local->name),
        local->getType().isSigned(), process_local);
    design.value_names.insert_or_assign(local, std::move(name));
    installed_names.push_back(local);
  }

  ProcedureBuilder standalone_builder;
  auto &builder =
      process_local ? *design.active_procedure_builder : standalone_builder;
  OptoSlangExpr return_signal;
  return_signal.kind = OPTO_SLANG_EXPR_SIGNAL;
  return_signal.signal_name = intern_string(design, return_name);
  auto *return_lhs = make_expr(design, std::move(return_signal), call);
  OptoSlangExpr unknown_return;
  unknown_return.kind = OPTO_SLANG_EXPR_CONSTANT;
  unknown_return.constant_has_width = true;
  unknown_return.constant_width = checked_width(
      lowered_type_width(function.getReturnType()), function.name);
  unknown_return.constant_bits.assign(unknown_return.constant_width, 'x');
  std::vector<OptoSlangEffectData> initializers;
  initializers.reserve(argument_initializers.size() + 1);
  initializers.push_back({
      return_lhs,
      make_expr(design, std::move(unknown_return), call),
      true,
      source,
  });
  initializers.insert(initializers.end(),
                      std::make_move_iterator(argument_initializers.begin()),
                      std::make_move_iterator(argument_initializers.end()));

  const auto return_exit = builder.add_block(source);
  design.function_returns.push_back(return_variable);
  design.subroutine_return_targets.push_back(return_exit);
  return_scope_pushed = true;
  auto initialization = builder.effects(std::move(initializers), source);
  auto lowered_body = lower_statement(builder, design, function.getBody(),
                                      OPTO_SLANG_PROCEDURE_COMB);
  auto body = builder.join_at(builder.sequence(std::move(initialization),
                                               std::move(lowered_body), source),
                              return_exit, source);
  design.subroutine_return_targets.pop_back();
  design.function_returns.pop_back();
  return_scope_pushed = false;
  if (design.active_expression_prelude) {
    auto &prelude = *design.active_expression_prelude;
    prelude = builder.sequence(std::move(prelude), std::move(body), source);
  } else {
    design.module.procedures.push_back(
        builder.finish(std::move(body), OPTO_SLANG_PROCEDURE_COMB, {}, source));
  }
  OptoSlangExpr result;
  result.kind = OPTO_SLANG_EXPR_SIGNAL;
  result.signal_name = intern_string(design, std::move(return_name));
  return make_expr(design, std::move(result), call);
}

CfgFragment lower_subroutine_call_statement(
    ProcedureBuilder &builder, ModuleLoweringContext &design,
    const CallExpression &call, OptoSlangProcedureKind procedure_kind) {
  const auto *selected =
      std::get_if<const SubroutineSymbol *>(&call.subroutine);
  if (!selected || !*selected) {
    throw std::runtime_error("unsupported non-system call statement '" +
                             copy_string(call.getSubroutineName()) + "'");
  }
  const auto &function =
      resolve_synthesizable_subroutine(**selected, call.sourceRange.start());
  const bool synthesizable_kind =
      function.subroutineKind == SubroutineKind::Task ||
      (function.subroutineKind == SubroutineKind::Function &&
       function.getReturnType().isVoid());
  if (!synthesizable_kind || function.isVirtual() ||
      function.flags.has(MethodFlags::DPIImport | MethodFlags::BuiltIn)) {
    throw std::runtime_error("call statement '" + copy_string(function.name) +
                             "' is not a synthesizable task or void function");
  }
  const auto arguments = function.getArguments();
  const auto actuals = call.arguments();
  if (arguments.size() != actuals.size()) {
    throw std::runtime_error("task or void function call argument count does "
                             "not match its declaration");
  }
  constexpr size_t max_recursive_depth = 256;
  if (static_cast<size_t>(std::ranges::count(
          design.function_stack, &function)) >= max_recursive_depth) {
    throw std::runtime_error("recursive synthesizable subroutine '" +
                             copy_string(function.name) +
                             "' does not reach a constant base case within " +
                             std::to_string(max_recursive_depth) + " calls");
  }
  const bool process_local = design.active_procedure_builder != nullptr;
  const auto source = source_span(design, call);
  std::vector<const VariableSymbol *> locals;
  collect_function_locals(function, locals);
  ScopedSymbolMapBindings function_value_bindings(design.function_values);
  ScopedSymbolMapBindings function_lvalue_bindings(design.function_lvalues);
  ScopedSymbolMapBindings constant_bindings(design.procedural_constants);
  ScopedSymbolMapBindings name_bindings(design.value_names);
  for (auto *argument : arguments) {
    function_value_bindings.track(argument);
    function_lvalue_bindings.track(argument);
    constant_bindings.track(argument);
    name_bindings.track(argument);
  }
  for (auto *local : locals) {
    function_value_bindings.track(local);
    constant_bindings.track(local);
    name_bindings.track(local);
  }
  design.function_stack.push_back(&function);

  struct CopyOut {
    const FormalArgumentSymbol *argument;
    const Expression *actual;
  };
  std::vector<const ValueSymbol *> installed_values;
  std::vector<const ValueSymbol *> installed_lvalues;
  std::vector<const ValueSymbol *> installed_constants;
  std::vector<const ValueSymbol *> installed_names;
  std::vector<CopyOut> copy_outs;
  std::vector<OptoSlangEffectData> initializers;
  bool return_target_pushed = false;
  bool disable_control_pushed = false;
  ScopeExit leave_function([&] {
    if (disable_control_pushed) {
      design.disable_controls.pop_back();
    }
    if (return_target_pushed) {
      design.subroutine_return_targets.pop_back();
    }
    for (auto *symbol : installed_values) {
      design.function_values.erase(symbol);
    }
    for (auto *symbol : installed_lvalues) {
      design.function_lvalues.erase(symbol);
    }
    for (auto *symbol : installed_constants) {
      design.procedural_constants.erase(symbol);
    }
    for (auto *symbol : installed_names) {
      design.value_names.erase(symbol);
    }
    design.function_stack.pop_back();
  });

  for (size_t index = 0; index < arguments.size(); ++index) {
    auto *argument = arguments[index];
    auto *actual = actuals[index];
    if (!argument || !actual) {
      throw std::runtime_error("subroutine call contains an unbound argument");
    }
    if (argument->direction == ArgumentDirection::Ref) {
      auto *alias = bind_ref_argument(design, function, *argument, *actual,
                                      process_local, initializers, source);
      design.function_lvalues.insert_or_assign(argument, alias);
      installed_lvalues.push_back(argument);
      continue;
    }

    auto constant = evaluate_lowering_constant(design, *actual);

    const bool input_only = argument->direction == ArgumentDirection::In;
    const bool is_written =
        !input_only || statement_assigns_value(function.getBody(), *argument);
    if (!is_written) {
      design.function_values.insert_or_assign(argument,
                                              lower_expr(design, *actual));
      installed_values.push_back(argument);
      if (constant) {
        design.procedural_constants.insert_or_assign(argument, constant);
        installed_constants.push_back(argument);
      }
      continue;
    }

    auto name = allocate_function_value_name(design, function, argument->name);
    name = add_internal_net(
        design, std::move(name),
        checked_width(lowered_type_width(argument->getType()), argument->name),
        argument->getType().isSigned(), process_local);
    design.value_names.insert_or_assign(argument, name);
    installed_names.push_back(argument);
    OptoSlangExpr local;
    local.kind = OPTO_SLANG_EXPR_SIGNAL;
    local.signal_name = intern_string(design, name);
    const auto *lhs = make_expr(design, std::move(local), *actual);
    const OptoSlangExpr *rhs;
    if (argument->direction == ArgumentDirection::Out) {
      OptoSlangExpr unknown;
      unknown.kind = OPTO_SLANG_EXPR_CONSTANT;
      unknown.constant_has_width = true;
      unknown.constant_width = checked_width(
          lowered_type_width(argument->getType()), argument->name);
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

  for (auto *local : locals) {
    if (std::ranges::find(arguments, local) != arguments.end()) {
      continue;
    }
    auto name = allocate_function_value_name(design, function, local->name);
    name = add_internal_net(
        design, std::move(name),
        checked_width(lowered_type_width(local->getType()), local->name),
        local->getType().isSigned(), process_local);
    design.value_names.insert_or_assign(local, std::move(name));
    installed_names.push_back(local);
  }

  const auto return_exit = builder.add_block(source);
  design.subroutine_return_targets.push_back(return_exit);
  return_target_pushed = true;
  auto initialization = builder.effects(std::move(initializers), source);
  CfgFragment disable_initialization;
  if (statement_disables_target(function.getBody(), function)) {
    auto [control, control_initialization] =
        lower_disable_control(builder, design, function, call, source);
    design.disable_controls.push_back(control);
    disable_control_pushed = true;
    disable_initialization = std::move(control_initialization);
  }
  auto lowered_body =
      lower_statement(builder, design, function.getBody(), procedure_kind);
  if (disable_control_pushed) {
    design.disable_controls.pop_back();
    disable_control_pushed = false;
  }
  auto body = builder.join_at(
      builder.sequence(std::move(initialization),
                       builder.sequence(std::move(disable_initialization),
                                        std::move(lowered_body), source),
                       source),
      return_exit, source);
  design.subroutine_return_targets.pop_back();
  return_target_pushed = false;
  std::vector<OptoSlangEffectData> copy_effects;
  copy_effects.reserve(copy_outs.size());
  for (const auto &copy : copy_outs) {
    OptoSlangExpr value;
    value.kind = OPTO_SLANG_EXPR_SIGNAL;
    value.signal_name =
        intern_string(design, registered_value_name(design, *copy.argument));
    copy_effects.push_back({
        lower_signal_expr(design, *copy.actual),
        make_expr(design, std::move(value), *copy.actual),
        true,
        source,
    });
  }
  return builder.sequence(std::move(body),
                          builder.effects(std::move(copy_effects), source),
                          source);
}

void validate_initial_process(ModuleLoweringContext &design,
                              const InstanceBodySymbol &body,
                              const ProceduralBlockSymbol &process) {
  ProcedureBuilder builder;
  auto initial = lower_statement(builder, design, process.getBody(),
                                 OPTO_SLANG_PROCEDURE_COMB);
  if (!initial.empty()) {
    throw std::runtime_error(unsupported_member_message(body, process));
  }
}
} // namespace opto::slang_lower
