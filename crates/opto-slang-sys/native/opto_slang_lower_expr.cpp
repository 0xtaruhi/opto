// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

#include "opto_slang_lower_internal.h"

namespace opto::slang_lower {
constexpr uint64_t RUNTIME_POWER_MULTIPLICATION_LIMIT = 1024;
constexpr uint64_t ASSIGNMENT_PATTERN_ELEMENT_LIMIT = 65536;

uint32_t dynamic_offset_sum_width(uint32_t left_width, uint32_t right_width) {
  const auto operand_width = std::max(left_width, right_width);
  if (operand_width == UINT32_MAX) {
    throw std::runtime_error("dynamic selection offset width overflow");
  }
  return operand_width + 1;
}

uint32_t scaled_dynamic_offset_width(uint32_t offset_width,
                                     uint32_t element_width) {
  if (element_width <= 1) {
    return offset_width;
  }
  const auto scale_bits = 32u - std::countl_zero(element_width - 1);
  if (offset_width > UINT32_MAX - scale_bits) {
    throw std::runtime_error("dynamic selection offset width overflow");
  }
  return offset_width + scale_bits;
}

void apply_signal_slice(ModuleLoweringContext &design, OptoSlangExpr &signal,
                        uint64_t relative_lsb, uint32_t width,
                        const Expression &source) {
  if (signal.kind == OPTO_SLANG_EXPR_DYNAMIC_EXTRACT) {
    if (relative_lsb + width > signal.dynamic_extract_width ||
        relative_lsb > UINT32_MAX) {
      throw std::runtime_error(
          "nested dynamic assignment slice exceeds its selected element");
    }
    if (relative_lsb != 0) {
      const auto relative_lsb_u32 = static_cast<uint32_t>(relative_lsb);
      const auto relative_width = 32u - std::countl_zero(relative_lsb_u32);
      const auto offset_width = dynamic_offset_sum_width(
          signal.dynamic_extract_offset_width, relative_width);
      auto *offset = signal.dynamic_extract_offset;
      if (offset_width != signal.dynamic_extract_offset_width) {
        offset = make_unsigned_cast_expr(design, offset, offset_width, source);
      }
      signal.dynamic_extract_offset =
          make_binary_expr(design, OPTO_SLANG_BINARY_ADD, offset,
                           make_unsigned_constant_expr(design, relative_lsb_u32,
                                                       offset_width, source),
                           source);
      signal.dynamic_extract_offset_width = offset_width;
    }
    signal.dynamic_extract_width = width;
    set_expr_source(design, signal, source);
    return;
  }
  if (signal.kind != OPTO_SLANG_EXPR_SIGNAL) {
    throw std::runtime_error("assignment slice base is neither a signal nor a "
                             "dynamic signal selection");
  }
  const auto base_lsb = signal.signal_has_range
                            ? std::min(signal.signal_msb, signal.signal_lsb)
                            : 0;
  const auto lsb = static_cast<uint64_t>(base_lsb) + relative_lsb;
  const auto msb = lsb + width - 1;
  if (msb > UINT32_MAX) {
    throw std::runtime_error(
        "selected signal slice exceeds 32-bit flattened range capacity");
  }
  signal.signal_has_range = true;
  signal.signal_msb = static_cast<uint32_t>(msb);
  signal.signal_lsb = static_cast<uint32_t>(lsb);
  set_expr_source(design, signal, source);
}

uint32_t selected_element_width(const Type &type) {
  auto *element = type.getArrayElementType();
  return element ? checked_width(lowered_type_width(*element),
                                 "selected array element")
                 : 1;
}

ConstantRange selection_storage_range(const Type &type) {
  const auto range = type.getFixedRange();
  return type.isUnpackedArray() ? range.reverse() : range;
}

struct DynamicOffset {
  const OptoSlangExpr *expression;
  uint32_t width;
};

OptoSlangExpr *make_dynamic_extract(ModuleLoweringContext &design,
                                    const OptoSlangExpr *value,
                                    DynamicOffset offset, uint32_t width,
                                    const Expression &source) {
  if (value->kind == OPTO_SLANG_EXPR_DYNAMIC_EXTRACT) {
    if (width > value->dynamic_extract_width) {
      throw std::runtime_error(
          "nested dynamic selection exceeds its containing element");
    }
    const auto combined_width = dynamic_offset_sum_width(
        value->dynamic_extract_offset_width, offset.width);
    offset.expression = make_binary_expr(
        design, OPTO_SLANG_BINARY_ADD,
        make_unsigned_cast_expr(design, value->dynamic_extract_offset,
                                combined_width, source),
        make_unsigned_cast_expr(design, offset.expression, combined_width,
                                source),
        source);
    offset.width = combined_width;
    value = value->dynamic_extract_value;
  }
  if (value->kind == OPTO_SLANG_EXPR_SIGNAL && value->signal_has_range) {
    const auto base_lsb = std::min(value->signal_msb, value->signal_lsb);
    if (base_lsb != 0) {
      const auto base_width = 32u - std::countl_zero(base_lsb);
      const auto combined_width =
          dynamic_offset_sum_width(offset.width, base_width);
      offset.expression = make_binary_expr(
          design, OPTO_SLANG_BINARY_ADD,
          make_unsigned_cast_expr(design, offset.expression, combined_width,
                                  source),
          make_unsigned_constant_expr(design, base_lsb, combined_width, source),
          source);
      offset.width = combined_width;
    }
    auto base = *value;
    base.signal_has_range = false;
    base.signal_msb = 0;
    base.signal_lsb = 0;
    value = make_expr(design, std::move(base), source);
  }

  OptoSlangExpr lowered;
  lowered.kind = OPTO_SLANG_EXPR_DYNAMIC_EXTRACT;
  lowered.dynamic_extract_value = value;
  lowered.dynamic_extract_offset = offset.expression;
  lowered.dynamic_extract_offset_width = offset.width;
  lowered.dynamic_extract_width = width;
  return make_expr(design, std::move(lowered), source);
}

DynamicOffset make_dynamic_offset(ModuleLoweringContext &design,
                                  const Type &selected_type,
                                  const Expression &selector_expression,
                                  const Expression &source) {
  if (!selected_type.hasFixedRange()) {
    throw std::runtime_error("dynamic select requires a fixed-size value at " +
                             expression_location(design, source));
  }
  const auto range = selection_storage_range(selected_type);
  const auto selector_width = checked_width(
      lowered_type_width(*selector_expression.type), "dynamic selector");
  if (range.lower() < 0 || range.upper() < 0) {
    const auto offset_width = std::max(selector_width + 1, 33u);
    auto *selector =
        selector_expression.type->isSigned()
            ? make_signed_cast_expr(design,
                                    lower_expr(design, selector_expression),
                                    offset_width, selector_expression)
            : make_unsigned_cast_expr(design,
                                      lower_expr(design, selector_expression),
                                      offset_width, selector_expression);
    const auto origin = range.isDescending() ? range.lower() : range.upper();
    auto *origin_expr =
        make_signed_constant_expr(design, static_cast<int64_t>(origin),
                                  offset_width, selector_expression);
    const auto *offset = make_binary_expr(
        design, OPTO_SLANG_BINARY_SUB,
        range.isDescending() ? selector : origin_expr,
        range.isDescending() ? origin_expr : selector, selector_expression);
    const auto element_width = selected_element_width(selected_type);
    if (element_width > 1) {
      const auto scaled_width =
          scaled_dynamic_offset_width(offset_width, element_width);
      offset = make_binary_expr(
          design, OPTO_SLANG_BINARY_MUL,
          make_unsigned_cast_expr(design, offset, scaled_width,
                                  selector_expression),
          make_unsigned_constant_expr(design, element_width, scaled_width,
                                      selector_expression),
          selector_expression);
      return DynamicOffset{offset, scaled_width};
    }
    return DynamicOffset{
        make_unsigned_cast_expr(design, offset, offset_width,
                                selector_expression),
        offset_width,
    };
  }
  const auto bound = static_cast<uint32_t>(range.upper());
  const auto bound_width = bound == 0 ? 1u : 32u - std::countl_zero(bound);
  const auto offset_width = std::max(selector_width, bound_width);
  auto *selector =
      make_unsigned_cast_expr(design, lower_expr(design, selector_expression),
                              offset_width, selector_expression);
  auto *bound_expr = make_unsigned_constant_expr(
      design,
      static_cast<uint32_t>(range.isDescending() ? range.lower()
                                                 : range.upper()),
      offset_width, selector_expression);
  const OptoSlangExpr *offset = nullptr;
  if (range.isDescending()) {
    offset = range.lower() == 0
                 ? selector
                 : make_binary_expr(design, OPTO_SLANG_BINARY_SUB, selector,
                                    bound_expr, selector_expression);
  } else {
    offset = make_binary_expr(design, OPTO_SLANG_BINARY_SUB, bound_expr,
                              selector, selector_expression);
  }
  const auto element_width = selected_element_width(selected_type);
  if (element_width > 1) {
    const auto scaled_width =
        scaled_dynamic_offset_width(offset_width, element_width);
    offset = make_binary_expr(
        design, OPTO_SLANG_BINARY_MUL,
        make_unsigned_cast_expr(design, offset, scaled_width,
                                selector_expression),
        make_unsigned_constant_expr(design, element_width, scaled_width,
                                    selector_expression),
        selector_expression);
    return DynamicOffset{offset, scaled_width};
  }
  return DynamicOffset{offset, offset_width};
}

OptoSlangExpr *
make_dynamic_element_select(ModuleLoweringContext &design,
                            const ElementSelectExpression &select,
                            const OptoSlangExpr *value) {
  const auto &selected_type = *select.value().type;
  const auto offset =
      make_dynamic_offset(design, selected_type, select.selector(), select);
  return make_dynamic_extract(design, value, offset,
                              selected_element_width(selected_type), select);
}

OptoSlangExpr *make_dynamic_indexed_part_select(
    ModuleLoweringContext &design, const RangeSelectExpression &select,
    const OptoSlangExpr *value, uint32_t selected_elements) {
  const auto &selected_type = *select.value().type;
  auto offset =
      make_dynamic_offset(design, selected_type, select.left(), select);
  const auto element_width = selected_element_width(selected_type);
  const auto selected_width =
      static_cast<uint64_t>(selected_elements) * element_width;
  if (selected_width == 0 || selected_width > UINT32_MAX) {
    throw std::runtime_error(
        "dynamic indexed part-select width is out of range");
  }
  const auto range = selection_storage_range(selected_type);
  const bool base_is_high_end =
      (range.isDescending() &&
       select.getSelectionKind() == RangeSelectionKind::IndexedDown) ||
      (!range.isDescending() &&
       select.getSelectionKind() == RangeSelectionKind::IndexedUp);
  if (base_is_high_end && selected_width > element_width) {
    offset.expression = make_binary_expr(
        design, OPTO_SLANG_BINARY_SUB, offset.expression,
        make_unsigned_constant_expr(
            design, static_cast<uint32_t>(selected_width - element_width),
            offset.width, select.left()),
        select.left());
  }
  return make_dynamic_extract(design, value, offset,
                              static_cast<uint32_t>(selected_width), select);
}

OptoSlangExpr *
lower_direct_child_port_reference(ModuleLoweringContext &design,
                                  const HierarchicalValueExpression &value) {
  ModuleMembers members;
  collect_elaborated_members(design.body, design.body, members);
  for (auto *child : members.instances) {
    for (auto *connection : child->getPortConnections()) {
      if (!connection || connection->port.kind != SymbolKind::Port) {
        continue;
      }
      const auto &port = connection->port.as<PortSymbol>();
      if (port.internalSymbol != &value.symbol) {
        continue;
      }
      auto *expression = connection->getExpression();
      if (!expression || is_empty_connection_expression(*expression)) {
        throw std::runtime_error(
            "hierarchical reference targets an unconnected child port '" +
            copy_string(port.name) + "'");
      }
      return lower_expr(design, *expression);
    }
  }
  throw std::runtime_error(
      "hierarchical signal '" + value.symbol.getHierarchicalPath() +
      "' is not a directly connected child port of the active module");
}

OptoSlangExpr *lower_signal_expr(ModuleLoweringContext &design,
                                 const Expression &expr) {
  switch (expr.kind) {
  case ExpressionKind::NamedValue: {
    const auto &value = expr.as<NamedValueExpression>();
    if (auto *alias = find_function_lvalue(design, value.symbol)) {
      return make_expr(design, *alias, expr);
    }
    OptoSlangExpr lowered;
    lowered.kind = OPTO_SLANG_EXPR_SIGNAL;
    lowered.signal_name =
        intern_string(design, registered_value_name(design, value.symbol));
    return make_expr(design, std::move(lowered), expr);
  }
  case ExpressionKind::HierarchicalValue: {
    const auto &value = expr.as<HierarchicalValueExpression>();
    if (!has_registered_value(design, value.symbol)) {
      return lower_direct_child_port_reference(design, value);
    }
    OptoSlangExpr lowered;
    lowered.kind = OPTO_SLANG_EXPR_SIGNAL;
    lowered.signal_name =
        intern_string(design, registered_value_name(design, value.symbol));
    return make_expr(design, std::move(lowered), expr);
  }
  case ExpressionKind::ElementSelect: {
    const auto &select = expr.as<ElementSelectExpression>();
    auto *base = lower_signal_expr(design, select.value());
    auto index = integer_literal_u32(design, select.selector());
    if (!index) {
      return make_dynamic_element_select(design, select, base);
    }
    const auto &selected_type = *select.value().type;
    if (!selected_type.hasFixedRange()) {
      throw std::runtime_error(
          "element select requires a fixed-size packed or unpacked value");
    }
    const auto range = selection_storage_range(selected_type);
    if (*index > static_cast<uint32_t>(INT32_MAX) ||
        !range.containsPoint(static_cast<int32_t>(*index))) {
      throw std::runtime_error(
          "element select index is outside its declared range at " +
          expression_location(design, expr));
    }
    const auto *element_type = selected_type.getArrayElementType();
    const auto element_width =
        element_type
            ? checked_width(lowered_type_width(*element_type), "array element")
            : 1;
    const auto translated = range.translateIndex(static_cast<int32_t>(*index));
    if (translated < 0) {
      throw std::runtime_error("element select produced a negative bit offset");
    }
    const auto relative_lsb = static_cast<uint64_t>(translated) * element_width;
    apply_signal_slice(design, *base, relative_lsb, element_width, expr);
    return base;
  }
  case ExpressionKind::RangeSelect: {
    const auto &select = expr.as<RangeSelectExpression>();
    auto *base = lower_signal_expr(design, select.value());
    auto left = integer_literal_u32(design, select.left());
    auto right = integer_literal_u32(design, select.right());
    if (!left) {
      if (!right || select.getSelectionKind() == RangeSelectionKind::Simple) {
        throw std::runtime_error(
            "dynamic part-select requires a constant indexed width");
      }
      return make_dynamic_indexed_part_select(design, select, base, *right);
    }
    if (!right) {
      throw std::runtime_error("indexed part-select width must be constant");
    }
    uint64_t right_index = *right;
    if (select.getSelectionKind() != RangeSelectionKind::Simple) {
      if (*right == 0) {
        throw std::runtime_error("indexed part-select width must be positive");
      }
      if (select.getSelectionKind() == RangeSelectionKind::IndexedUp) {
        right_index = static_cast<uint64_t>(*left) + *right - 1;
      } else if (*left + 1 < *right) {
        throw std::runtime_error("indexed down part-select exceeds index zero");
      } else {
        right_index = *left - *right + 1;
      }
    }
    if (right_index > static_cast<uint64_t>(INT32_MAX) ||
        *left > static_cast<uint32_t>(INT32_MAX)) {
      throw std::runtime_error(
          "range select index exceeds 32-bit signed capacity");
    }
    const auto &selected_type = *select.value().type;
    if (!selected_type.hasFixedRange()) {
      throw std::runtime_error("range select requires a fixed-size value");
    }
    const auto declared = selection_storage_range(selected_type);
    const auto left_index = static_cast<int32_t>(*left);
    const auto right_index_i32 = static_cast<int32_t>(right_index);
    if (!declared.containsPoint(left_index) ||
        !declared.containsPoint(right_index_i32)) {
      throw std::runtime_error("range select is outside its declared range");
    }
    const auto element_width = selected_element_width(selected_type);
    const auto left_offset = declared.translateIndex(left_index);
    const auto right_offset = declared.translateIndex(right_index_i32);
    if (left_offset < 0 || right_offset < 0) {
      throw std::runtime_error("range select produced a negative bit offset");
    }
    const auto relative_lsb =
        static_cast<uint64_t>(std::min(left_offset, right_offset)) *
        element_width;
    const auto distance = std::abs(static_cast<int64_t>(left_offset) -
                                   static_cast<int64_t>(right_offset));
    const auto selected_elements = static_cast<uint64_t>(distance) + 1;
    const auto width = selected_elements * element_width;
    if (width > UINT32_MAX) {
      throw std::runtime_error("range select width exceeds 32-bit capacity");
    }
    apply_signal_slice(design, *base, relative_lsb,
                       static_cast<uint32_t>(width), expr);
    return base;
  }
  case ExpressionKind::MemberAccess: {
    const auto &access = expr.as<MemberAccessExpression>();
    if (access.member.kind != SymbolKind::Field) {
      throw std::runtime_error(
          "member access does not reference a struct or union field at " +
          expression_location(design, expr));
    }
    const auto &aggregate_type = access.value().type->getCanonicalType();
    const auto &field = access.member.as<FieldSymbol>();
    const auto width =
        checked_width(lowered_type_width(field.getType()), field.name);
    auto *base = lower_signal_expr(design, access.value());
    apply_signal_slice(design, *base,
                       aggregate_field_storage_offset(aggregate_type, field),
                       width, expr);
    return base;
  }
  default:
    throw std::runtime_error(
        "expression kind '" + copy_string(toString(expr.kind)) +
        "' is not a signal reference at " + expression_location(design, expr));
  }
}

bool constant_element_select_is_out_of_range(ModuleLoweringContext &design,
                                             const Expression &expression) {
  if (expression.kind != ExpressionKind::ElementSelect) {
    return false;
  }
  const auto &select = expression.as<ElementSelectExpression>();
  const auto &selected_type = *select.value().type;
  if (!selected_type.hasFixedRange()) {
    return false;
  }
  auto constant = evaluate_lowering_constant(design, select.selector());
  if (!constant || !constant.isInteger() || constant.integer().hasUnknown()) {
    return false;
  }
  auto index = constant.integer().as<int64_t>();
  if (!index || *index < INT32_MIN || *index > INT32_MAX) {
    return true;
  }
  return !selection_storage_range(selected_type)
              .containsPoint(static_cast<int32_t>(*index));
}

OptoSlangExpr *make_unknown_expression(ModuleLoweringContext &design,
                                       const Expression &expression) {
  OptoSlangExpr lowered;
  lowered.kind = OPTO_SLANG_EXPR_CONSTANT;
  lowered.constant_has_width = true;
  lowered.constant_width =
      checked_width(lowered_type_width(*expression.type), "unknown expression");
  lowered.constant_signed = expression.type->isSigned();
  lowered.constant_bits.assign(lowered.constant_width, 'x');
  return make_expr(design, std::move(lowered), expression);
}

OptoSlangExpr *lower_function_call(ModuleLoweringContext &design,
                                   const CallExpression &call);

OptoSlangExpr *apply_rvalue_slice(ModuleLoweringContext &design,
                                  const OptoSlangExpr *value,
                                  uint64_t relative_lsb, uint32_t width,
                                  const Expression &source) {
  if (value->kind == OPTO_SLANG_EXPR_SIGNAL) {
    auto selected = *value;
    apply_signal_slice(design, selected, relative_lsb, width, source);
    return make_expr(design, std::move(selected), source);
  }
  if (value->kind == OPTO_SLANG_EXPR_EXTRACT) {
    const auto lsb = static_cast<uint64_t>(value->extract_lsb) + relative_lsb;
    if (relative_lsb + width > value->extract_width || lsb > UINT32_MAX) {
      throw std::runtime_error("nested extract exceeds its source slice");
    }
    OptoSlangExpr selected;
    selected.kind = OPTO_SLANG_EXPR_EXTRACT;
    selected.extract_value = value->extract_value;
    selected.extract_lsb = static_cast<uint32_t>(lsb);
    selected.extract_width = width;
    return make_expr(design, std::move(selected), source);
  }
  OptoSlangExpr selected;
  selected.kind = OPTO_SLANG_EXPR_EXTRACT;
  selected.extract_value = value;
  if (relative_lsb > UINT32_MAX) {
    throw std::runtime_error("extract offset exceeds 32-bit capacity");
  }
  selected.extract_lsb = static_cast<uint32_t>(relative_lsb);
  selected.extract_width = width;
  return make_expr(design, std::move(selected), source);
}

OptoSlangExpr *lower_constant_exponent_power(ModuleLoweringContext &design,
                                             const BinaryExpression &power,
                                             const Expression &source) {
  auto exponent_value = evaluate_lowering_constant(design, power.right());
  if (!exponent_value || !exponent_value.isInteger() ||
      exponent_value.integer().hasUnknown()) {
    return nullptr;
  }
  const auto &exponent_bits = exponent_value.integer();
  if (exponent_bits.isSigned() && exponent_bits.isNegative()) {
    throw std::runtime_error(
        "negative runtime power exponent is not supported at " +
        expression_location(design, source));
  }
  auto exponent = exponent_bits.as<uint64_t>();
  if (!exponent || *exponent > RUNTIME_POWER_MULTIPLICATION_LIMIT + 1) {
    throw std::runtime_error(
        "runtime power requires more than the deterministic limit of " +
        std::to_string(RUNTIME_POWER_MULTIPLICATION_LIMIT) + " at " +
        expression_location(design, source));
  }
  if (!source.type->isIntegral() || !power.left().type->isIntegral()) {
    throw std::runtime_error("runtime power requires integral operands at " +
                             expression_location(design, source));
  }
  if (*exponent == 0) {
    return cast_to_type(
        design,
        make_unsigned_constant_expr(
            design, 1,
            checked_width(lowered_type_width(*source.type), "power result"),
            source),
        *source.type, source);
  }

  auto *base = cast_to_type(design, lower_expr(design, power.left()),
                            *source.type, source);
  auto *result = base;
  for (uint64_t multiplication = 1; multiplication < *exponent;
       ++multiplication) {
    result = cast_to_type(
        design,
        make_binary_expr(design, OPTO_SLANG_BINARY_MUL, result, base, source),
        *source.type, source);
  }
  return result;
}

OptoSlangExpr *lower_count_ones(ModuleLoweringContext &design,
                                const Expression &argument,
                                const Expression &source) {
  if (!argument.type->isBitstreamType() || !argument.type->isFixedSize()) {
    throw std::runtime_error(
        "$countones requires a fixed-size bitstream argument at " +
        expression_location(design, source));
  }
  const auto width =
      checked_width(lowered_type_width(*argument.type), "$countones argument");
  auto *value = lower_expr(design, argument);
  std::vector<OptoSlangExpr *> terms;
  terms.reserve(width);
  for (uint32_t bit = 0; bit < width; ++bit) {
    terms.push_back(make_unsigned_cast_expr(
        design, apply_rvalue_slice(design, value, bit, 1, argument), 32,
        argument));
  }
  while (terms.size() > 1) {
    std::vector<OptoSlangExpr *> reduced;
    reduced.reserve((terms.size() + 1) / 2);
    for (size_t index = 0; index < terms.size(); index += 2) {
      if (index + 1 == terms.size()) {
        reduced.push_back(terms[index]);
      } else {
        reduced.push_back(make_binary_expr(design, OPTO_SLANG_BINARY_ADD,
                                           terms[index], terms[index + 1],
                                           source));
      }
    }
    terms = std::move(reduced);
  }
  return cast_to_type(design, terms.front(), *source.type, source);
}

OptoSlangExpr *lower_count_bits(ModuleLoweringContext &design,
                                std::span<const Expression *const> arguments,
                                const Expression &source) {
  if (arguments.size() < 2 || !arguments[0]) {
    throw std::runtime_error(
        "$countbits requires a value and at least one control bit at " +
        expression_location(design, source));
  }
  bool count_zero = false;
  bool count_one = false;
  for (auto *selector : arguments.subspan(1)) {
    if (!selector) {
      throw std::runtime_error("$countbits has an empty control-bit argument");
    }
    auto value = evaluate_lowering_constant(design, *selector);
    if (!value || !value.isInteger() || value.integer().hasUnknown()) {
      throw std::runtime_error(
          "$countbits runtime X/Z matching is not synthesizable at " +
          expression_location(design, *selector));
    }
    if (value.integer()[0].value == 0) {
      count_zero = true;
    } else {
      count_one = true;
    }
  }
  const auto width = checked_width(lowered_type_width(*arguments[0]->type),
                                   "$countbits argument");
  if (count_zero && count_one) {
    return cast_to_type(design,
                        make_unsigned_constant_expr(design, width, 32, source),
                        *source.type, source);
  }
  auto *ones = lower_count_ones(design, *arguments[0], source);
  if (count_one) {
    return ones;
  }
  return cast_to_type(
      design,
      make_binary_expr(design, OPTO_SLANG_BINARY_SUB,
                       make_unsigned_constant_expr(design, width, 32, source),
                       make_unsigned_cast_expr(design, ones, 32, source),
                       source),
      *source.type, source);
}

OptoSlangExpr *lower_onehot_call(ModuleLoweringContext &design,
                                 const Expression &argument,
                                 const Expression &source, bool allow_zero) {
  if (!argument.type->isBitstreamType() || !argument.type->isFixedSize()) {
    throw std::runtime_error(
        "onehot system call requires a fixed-size bitstream argument at " +
        expression_location(design, source));
  }
  const auto width =
      checked_width(lowered_type_width(*argument.type), "onehot argument");
  auto *value = make_unsigned_cast_expr(design, lower_expr(design, argument),
                                        width, argument);
  auto *zero = make_unsigned_constant_expr(design, 0, width, source);
  auto *one = make_unsigned_constant_expr(design, 1, width, source);
  auto *predecessor =
      make_binary_expr(design, OPTO_SLANG_BINARY_SUB, value, one, source);
  auto *at_most_one =
      make_binary_expr(design, OPTO_SLANG_BINARY_EQ,
                       make_binary_expr(design, OPTO_SLANG_BINARY_BIT_AND,
                                        value, predecessor, source),
                       zero, source);
  if (allow_zero) {
    return at_most_one;
  }
  return make_binary_expr(
      design, OPTO_SLANG_BINARY_LOGICAL_AND,
      make_binary_expr(design, OPTO_SLANG_BINARY_NE, value, zero, source),
      at_most_one, source);
}

struct PriorityCode {
  OptoSlangExpr *any = nullptr;
  OptoSlangExpr *value = nullptr;
};

// Builds a balanced priority encoder for the highest set bit. The returned
// value is the one-based bit position, so it directly represents
// floor(log2(input)) + 1. Keeping the subtree's reduction alongside its value
// avoids rebuilding reduction trees at every level.
PriorityCode lower_highest_set_bit_position(ModuleLoweringContext &design,
                                            const OptoSlangExpr *input,
                                            uint32_t first_bit, uint32_t width,
                                            const Expression &source) {
  if (width == 0) {
    throw std::logic_error("priority encoder requires a non-empty input");
  }
  if (width == 1) {
    auto *bit = apply_rvalue_slice(design, input, first_bit, 1, source);
    return {
        bit,
        make_mux_expr(
            design, bit,
            make_unsigned_constant_expr(design, first_bit + 1, 32, source),
            make_unsigned_constant_expr(design, 0, 32, source), source),
    };
  }

  const auto lower_width = width / 2;
  auto lower = lower_highest_set_bit_position(design, input, first_bit,
                                              lower_width, source);
  auto upper = lower_highest_set_bit_position(
      design, input, first_bit + lower_width, width - lower_width, source);
  return {
      make_binary_expr(design, OPTO_SLANG_BINARY_BIT_OR, lower.any, upper.any,
                       source),
      make_mux_expr(design, upper.any, upper.value, lower.value, source),
  };
}

OptoSlangExpr *lower_clog2_call(ModuleLoweringContext &design,
                                const Expression &argument,
                                const Expression &source) {
  if (!argument.type->isIntegral() || !argument.type->isFixedSize()) {
    throw std::runtime_error(
        "$clog2 requires a fixed-size integral argument at " +
        expression_location(design, source));
  }
  const auto width =
      checked_width(lowered_type_width(*argument.type), "$clog2 argument");
  if (width == UINT32_MAX) {
    throw std::runtime_error(
        "$clog2 argument width exceeds the priority-encoder capacity at " +
        expression_location(design, source));
  }

  auto *value = make_unsigned_cast_expr(design, lower_expr(design, argument),
                                        width, argument);
  auto *zero = make_unsigned_constant_expr(design, 0, width, source);
  auto *predecessor = make_binary_expr(
      design, OPTO_SLANG_BINARY_SUB, value,
      make_unsigned_constant_expr(design, 1, width, source), source);
  auto encoded =
      lower_highest_set_bit_position(design, predecessor, 0, width, source);
  auto *result = make_mux_expr(
      design,
      make_binary_expr(design, OPTO_SLANG_BINARY_NE, value, zero, source),
      encoded.value, make_unsigned_constant_expr(design, 0, 32, source),
      source);
  return cast_to_type(design, result, *source.type, source);
}

bool expression_has_intrinsic_two_state_type(const Expression &expression) {
  switch (expression.kind) {
  case ExpressionKind::NamedValue:
    return !expression.as<NamedValueExpression>()
                .symbol.getType()
                .isFourState();
  case ExpressionKind::HierarchicalValue:
    return !expression.as<HierarchicalValueExpression>()
                .symbol.getType()
                .isFourState();
  case ExpressionKind::ElementSelect:
    return expression_has_intrinsic_two_state_type(
        expression.as<ElementSelectExpression>().value());
  case ExpressionKind::RangeSelect:
    return expression_has_intrinsic_two_state_type(
        expression.as<RangeSelectExpression>().value());
  case ExpressionKind::MemberAccess:
    return !expression.type->isFourState();
  case ExpressionKind::Conversion:
    return expression_has_intrinsic_two_state_type(
        expression.as<ConversionExpression>().operand());
  default:
    return !expression.type->isFourState();
  }
}

OptoSlangExpr *lower_extended_equality(ModuleLoweringContext &design,
                                       const BinaryExpression &equality,
                                       const Expression &source) {
  const bool inequality = equality.op == BinaryOperator::CaseInequality ||
                          equality.op == BinaryOperator::WildcardInequality;
  if (equality.op == BinaryOperator::CaseEquality ||
      equality.op == BinaryOperator::CaseInequality) {
    if (!expression_has_intrinsic_two_state_type(equality.left()) ||
        !expression_has_intrinsic_two_state_type(equality.right())) {
      throw std::runtime_error(
          "four-state case equality requires runtime X/Z observability at " +
          expression_location(design, source));
    }
    return make_binary_expr(
        design, inequality ? OPTO_SLANG_BINARY_NE : OPTO_SLANG_BINARY_EQ,
        lower_expr(design, equality.left()),
        lower_expr(design, equality.right()), source);
  }

  if (!expression_has_intrinsic_two_state_type(equality.left())) {
    throw std::runtime_error("four-state wildcard equality requires runtime "
                             "X/Z observability on its left operand at " +
                             expression_location(design, source));
  }
  auto *value = lower_expr(design, equality.left());
  if (expression_has_intrinsic_two_state_type(equality.right())) {
    return make_binary_expr(
        design, inequality ? OPTO_SLANG_BINARY_NE : OPTO_SLANG_BINARY_EQ, value,
        lower_expr(design, equality.right()), source);
  }
  auto *pattern = lower_expr(design, equality.right());
  if (pattern->kind != OPTO_SLANG_EXPR_CONSTANT ||
      !pattern->constant_has_width) {
    throw std::runtime_error("wildcard equality requires a two-state right "
                             "operand or a constant wildcard pattern at " +
                             expression_location(design, source));
  }

  std::string mask;
  std::string cared;
  mask.reserve(pattern->constant_bits.size());
  cared.reserve(pattern->constant_bits.size());
  for (char bit : pattern->constant_bits) {
    const bool wildcard = bit == 'x' || bit == 'X' || bit == 'z' || bit == 'Z';
    mask.push_back(wildcard ? '0' : '1');
    cared.push_back(wildcard ? '0' : bit);
  }
  OptoSlangExpr mask_expression;
  mask_expression.kind = OPTO_SLANG_EXPR_CONSTANT;
  mask_expression.constant_has_width = true;
  mask_expression.constant_width = pattern->constant_width;
  mask_expression.constant_bits = std::move(mask);
  auto *mask_value = make_expr(design, std::move(mask_expression), source);
  OptoSlangExpr cared_expression;
  cared_expression.kind = OPTO_SLANG_EXPR_CONSTANT;
  cared_expression.constant_has_width = true;
  cared_expression.constant_width = pattern->constant_width;
  cared_expression.constant_bits = std::move(cared);
  auto *cared_value = make_expr(design, std::move(cared_expression), source);
  return make_binary_expr(
      design, inequality ? OPTO_SLANG_BINARY_NE : OPTO_SLANG_BINARY_EQ,
      make_binary_expr(design, OPTO_SLANG_BINARY_BIT_AND, value, mask_value,
                       source),
      cared_value, source);
}

void collect_lvalue_leaves(const Expression &expression,
                           std::vector<LvalueLeaf> &leaves) {
  if (expression.kind == ExpressionKind::Concatenation) {
    for (auto *operand : expression.as<ConcatenationExpression>().operands()) {
      if (!operand) {
        throw std::runtime_error(
            "lvalue concatenation contains an empty operand");
      }
      collect_lvalue_leaves(*operand, leaves);
    }
    return;
  }
  leaves.push_back(LvalueLeaf{
      &expression,
      checked_width(lowered_type_width(*expression.type), "lvalue operand"),
  });
}

std::vector<OptoSlangAssignData>
lower_continuous_assignment(ModuleLoweringContext &design,
                            const AssignmentExpression &assignment) {
  if (assignment.left().kind != ExpressionKind::Concatenation &&
      constant_element_select_is_out_of_range(design, assignment.left())) {
    return {};
  }
  auto *rhs = cast_to_lvalue_type(
      design, lower_expr(design, assignment.right()), assignment.left());
  if (assignment.left().kind != ExpressionKind::Concatenation) {
    return {{lower_signal_expr(design, assignment.left()), rhs}};
  }

  std::vector<LvalueLeaf> leaves;
  collect_lvalue_leaves(assignment.left(), leaves);
  if (leaves.empty()) {
    throw std::runtime_error("continuous assignment has an empty lvalue");
  }

  const auto total_width =
      checked_width(lowered_type_width(*assignment.left().type),
                    "continuous assignment lvalue");
  uint64_t consumed = 0;
  std::vector<OptoSlangAssignData> lowered;
  lowered.reserve(leaves.size());
  for (const auto &leaf : leaves) {
    consumed += leaf.width;
    if (consumed > total_width) {
      throw std::runtime_error(
          "lvalue concatenation width exceeds its assignment type");
    }
    if (constant_element_select_is_out_of_range(design, *leaf.expression)) {
      continue;
    }
    auto *slice = apply_rvalue_slice(design, rhs, total_width - consumed,
                                     leaf.width, assignment.right());
    lowered.push_back(OptoSlangAssignData{
        lower_signal_expr(design, *leaf.expression),
        cast_to_lvalue_type(design, slice, *leaf.expression),
    });
  }
  if (consumed != total_width) {
    throw std::runtime_error(
        "lvalue concatenation width does not match its assignment type");
  }
  return lowered;
}

OptoSlangExpr *lower_select_expr(ModuleLoweringContext &design,
                                 const Expression &expr) {
  if (expr.kind == ExpressionKind::ElementSelect) {
    if (constant_element_select_is_out_of_range(design, expr)) {
      return make_unknown_expression(design, expr);
    }
    const auto &select = expr.as<ElementSelectExpression>();
    auto index = integer_literal_u32(design, select.selector());
    if (!index) {
      return make_dynamic_element_select(design, select,
                                         lower_expr(design, select.value()));
    }
    const auto &selected_type = *select.value().type;
    if (!selected_type.hasFixedRange() ||
        *index > static_cast<uint32_t>(INT32_MAX)) {
      throw std::runtime_error(
          "element select requires a fixed 32-bit index range");
    }
    const auto range = selection_storage_range(selected_type);
    const auto index_i32 = static_cast<int32_t>(*index);
    if (!range.containsPoint(index_i32)) {
      throw std::runtime_error(
          "element select index is outside its declared range at " +
          expression_location(design, expr));
    }
    const auto translated = range.translateIndex(index_i32);
    if (translated < 0) {
      throw std::runtime_error("element select produced a negative bit offset");
    }
    const auto width = selected_element_width(selected_type);
    return apply_rvalue_slice(design, lower_expr(design, select.value()),
                              static_cast<uint64_t>(translated) * width, width,
                              expr);
  }
  if (expr.kind == ExpressionKind::RangeSelect) {
    const auto &select = expr.as<RangeSelectExpression>();
    auto left = integer_literal_u32(design, select.left());
    auto right = integer_literal_u32(design, select.right());
    if (!left) {
      if (!right || select.getSelectionKind() == RangeSelectionKind::Simple) {
        throw std::runtime_error(
            "dynamic part-select requires a constant indexed width");
      }
      return make_dynamic_indexed_part_select(
          design, select, lower_expr(design, select.value()), *right);
    }
    if (!right) {
      throw std::runtime_error("indexed part-select width must be constant");
    }
    uint64_t right_index = *right;
    if (select.getSelectionKind() != RangeSelectionKind::Simple) {
      if (*right == 0) {
        throw std::runtime_error("indexed part-select width must be positive");
      }
      if (select.getSelectionKind() == RangeSelectionKind::IndexedUp) {
        right_index = static_cast<uint64_t>(*left) + *right - 1;
      } else if (*left + 1 < *right) {
        throw std::runtime_error("indexed down part-select exceeds index zero");
      } else {
        right_index = *left - *right + 1;
      }
    }
    if (right_index > static_cast<uint64_t>(INT32_MAX) ||
        *left > static_cast<uint32_t>(INT32_MAX)) {
      throw std::runtime_error(
          "range select index exceeds 32-bit signed capacity");
    }
    const auto &selected_type = *select.value().type;
    if (!selected_type.hasFixedRange()) {
      throw std::runtime_error("range select requires a fixed-size value");
    }
    const auto declared = selection_storage_range(selected_type);
    const auto left_index = static_cast<int32_t>(*left);
    const auto right_index_i32 = static_cast<int32_t>(right_index);
    if (!declared.containsPoint(left_index) ||
        !declared.containsPoint(right_index_i32)) {
      throw std::runtime_error("range select is outside its declared range");
    }
    const auto left_offset = declared.translateIndex(left_index);
    const auto right_offset = declared.translateIndex(right_index_i32);
    if (left_offset < 0 || right_offset < 0) {
      throw std::runtime_error("range select produced a negative bit offset");
    }
    const auto element_width = selected_element_width(selected_type);
    const auto relative_lsb =
        static_cast<uint64_t>(std::min(left_offset, right_offset)) *
        element_width;
    const auto distance = std::abs(static_cast<int64_t>(left_offset) -
                                   static_cast<int64_t>(right_offset));
    const auto width = (static_cast<uint64_t>(distance) + 1) * element_width;
    if (width > UINT32_MAX) {
      throw std::runtime_error("range select width exceeds 32-bit capacity");
    }
    return apply_rvalue_slice(design, lower_expr(design, select.value()),
                              relative_lsb, static_cast<uint32_t>(width), expr);
  }
  const auto &access = expr.as<MemberAccessExpression>();
  if (access.member.kind != SymbolKind::Field) {
    throw std::runtime_error(
        "member access does not select an aggregate field");
  }
  const auto &aggregate_type = access.value().type->getCanonicalType();
  const auto &field = access.member.as<FieldSymbol>();
  return apply_rvalue_slice(
      design, lower_expr(design, access.value()),
      aggregate_field_storage_offset(aggregate_type, field),
      checked_width(lowered_type_width(field.getType()), field.name), expr);
}

OptoSlangExpr *lower_constant_value(ModuleLoweringContext &design,
                                    const Type &type, ConstantValue value,
                                    const Expression &source,
                                    const Symbol &context_symbol,
                                    std::string_view description) {
  if (!value.isInteger()) {
    if (!type.isBitstreamType() || !type.isFixedSize()) {
      throw std::runtime_error(
          "constant '" + copy_string(description) +
          "' does not have a fixed bitstream representation");
    }
    EvalContext context(context_symbol);
    value = Bitstream::convertToBitVector(std::move(value), source.sourceRange,
                                          context);
  }
  if (!value || !value.isInteger()) {
    throw std::runtime_error("constant '" + copy_string(description) +
                             "' could not be converted to a bit vector");
  }
  const auto &bits = value.integer();
  OptoSlangExpr lowered;
  lowered.kind = OPTO_SLANG_EXPR_CONSTANT;
  lowered.constant_has_width = true;
  lowered.constant_width = checked_width(bits.getBitWidth(), description);
  lowered.constant_bits = exact_binary_string(bits);
  return make_expr(design, std::move(lowered), source);
}

OptoSlangExpr *lower_streaming_concatenation(
    ModuleLoweringContext &design,
    const StreamingConcatenationExpression &streaming) {
  if (!streaming.isFixedSize() || streaming.getBitstreamWidth() == 0 ||
      streaming.getBitstreamWidth() > UINT32_MAX) {
    throw std::runtime_error(
        "streaming concatenation requires a fixed nonzero 32-bit width");
  }
  OptoSlangExpr normal;
  normal.kind = OPTO_SLANG_EXPR_CONCAT;
  OptoSlangExpr *single_operand = nullptr;
  for (const auto &stream : streaming.streams()) {
    if (stream.withExpr) {
      throw std::runtime_error("streaming concatenation with-clauses are not "
                               "supported for synthesis");
    }
    auto *operand = lower_expr(design, *stream.operand);
    single_operand = normal.concat_parts.empty() ? operand : nullptr;
    normal.concat_parts.push_back(operand);
  }
  if (normal.concat_parts.empty()) {
    throw std::runtime_error("streaming concatenation has no operands");
  }
  OptoSlangExpr *value = normal.concat_parts.size() == 1
                             ? single_operand
                             : make_expr(design, std::move(normal), streaming);
  const auto slice_size = streaming.getSliceSize();
  const auto total_width = static_cast<uint32_t>(streaming.getBitstreamWidth());
  if (slice_size == 0 || slice_size >= total_width) {
    return value;
  }
  if (slice_size > UINT32_MAX) {
    throw std::runtime_error("streaming slice size exceeds 32-bit capacity");
  }
  OptoSlangExpr reordered;
  reordered.kind = OPTO_SLANG_EXPR_CONCAT;
  for (uint32_t offset = 0; offset < total_width;) {
    const auto width = static_cast<uint32_t>(
        std::min<uint64_t>(slice_size, total_width - offset));
    reordered.concat_parts.push_back(
        apply_rvalue_slice(design, value, offset, width, streaming));
    offset += width;
  }
  return make_expr(design, std::move(reordered), streaming);
}

OptoSlangExpr *lower_inside_item_match(ModuleLoweringContext &design,
                                       const OptoSlangExpr *value,
                                       const Expression &item) {
  if (item.kind == ExpressionKind::ValueRange) {
    const auto &range = item.as<ValueRangeExpression>();
    if (range.rangeKind != ValueRangeKind::Simple ||
        range.left().type->isUnbounded() || range.right().type->isUnbounded()) {
      throw std::runtime_error(
          "only bounded simple inside ranges are supported for synthesis");
    }
    auto *at_least = make_binary_expr(design, OPTO_SLANG_BINARY_GE, value,
                                      lower_expr(design, range.left()), item);
    auto *at_most = make_binary_expr(design, OPTO_SLANG_BINARY_LE, value,
                                     lower_expr(design, range.right()), item);
    return make_binary_expr(design, OPTO_SLANG_BINARY_LOGICAL_AND, at_least,
                            at_most, item);
  }

  auto *matched_value = lower_expr(design, item);
  if (matched_value->kind != OPTO_SLANG_EXPR_CONSTANT ||
      !matched_value->constant_has_width ||
      matched_value->constant_bits.find_first_of("xXzZ") == std::string::npos) {
    return make_binary_expr(design, OPTO_SLANG_BINARY_EQ, value, matched_value,
                            item);
  }

  std::string mask;
  std::string cared;
  mask.reserve(matched_value->constant_bits.size());
  cared.reserve(matched_value->constant_bits.size());
  for (char bit : matched_value->constant_bits) {
    const bool wildcard = bit == 'x' || bit == 'X' || bit == 'z' || bit == 'Z';
    mask.push_back(wildcard ? '0' : '1');
    cared.push_back(wildcard ? '0' : bit);
  }
  OptoSlangExpr mask_expr;
  mask_expr.kind = OPTO_SLANG_EXPR_CONSTANT;
  mask_expr.constant_has_width = true;
  mask_expr.constant_width = matched_value->constant_width;
  mask_expr.constant_bits = std::move(mask);
  auto *mask_value = make_expr(design, std::move(mask_expr), item);
  OptoSlangExpr cared_expr;
  cared_expr.kind = OPTO_SLANG_EXPR_CONSTANT;
  cared_expr.constant_has_width = true;
  cared_expr.constant_width = matched_value->constant_width;
  cared_expr.constant_bits = std::move(cared);
  auto *cared_value = make_expr(design, std::move(cared_expr), item);
  auto *masked = make_binary_expr(design, OPTO_SLANG_BINARY_BIT_AND, value,
                                  mask_value, item);
  return make_binary_expr(design, OPTO_SLANG_BINARY_EQ, masked, cared_value,
                          item);
}

OptoSlangExpr *
lower_tagged_union_expression(ModuleLoweringContext &design,
                              const TaggedUnionExpression &tagged) {
  const auto layout = tagged_union_layout(*tagged.type);
  const auto &field = tagged.member.as<FieldSymbol>();
  const auto field_width = checked_width(
      std::max<uint64_t>(lowered_type_width(field.getType()), 1), field.name);
  const auto stored_field_width = field.getType().isVoid() ? 0u : field_width;
  if (stored_field_width > layout.payload_width) {
    throw std::runtime_error(
        "tagged union member exceeds its canonical payload width");
  }

  OptoSlangExpr lowered;
  lowered.kind = OPTO_SLANG_EXPR_CONCAT;
  if (layout.tag_width != 0) {
    lowered.concat_parts.push_back(make_unsigned_constant_expr(
        design, field.fieldIndex, layout.tag_width, tagged));
  }
  const auto padding_width = layout.payload_width - stored_field_width;
  if (padding_width != 0) {
    OptoSlangExpr padding;
    padding.kind = OPTO_SLANG_EXPR_CONSTANT;
    padding.constant_has_width = true;
    padding.constant_width = padding_width;
    padding.constant_bits.assign(padding_width,
                                 tagged.type->isFourState() ? 'x' : '0');
    lowered.concat_parts.push_back(
        make_expr(design, std::move(padding), tagged));
  }
  if (tagged.valueExpr) {
    lowered.concat_parts.push_back(lower_expr(design, *tagged.valueExpr));
  } else if (stored_field_width != 0) {
    throw std::runtime_error(
        "non-void tagged union member has no value expression");
  }
  if (lowered.concat_parts.empty()) {
    throw std::runtime_error(
        "zero-width tagged union has no synthesis representation");
  }
  if (lowered.concat_parts.size() == 1) {
    return const_cast<OptoSlangExpr *>(lowered.concat_parts.front());
  }
  return make_expr(design, std::move(lowered), tagged);
}

OptoSlangExpr *lower_expr(ModuleLoweringContext &design,
                          const Expression &expr) {
  if (expr.kind == ExpressionKind::MemberAccess) {
    const auto &access = expr.as<MemberAccessExpression>();
    EvalContext context(access.member);
    auto constant = expr.eval(context);
    if (constant) {
      return lower_constant_value(design, *expr.type, std::move(constant), expr,
                                  access.member, access.member.name);
    }
  }
  if (expr.kind == ExpressionKind::NamedValue) {
    const auto &symbol = expr.as<NamedValueExpression>().symbol;
    if (auto found = design.function_values.find(&symbol);
        found != design.function_values.end()) {
      return found->second;
    }
    if (auto found = design.procedural_constants.find(&symbol);
        found != design.procedural_constants.end()) {
      if (!found->second.isInteger()) {
        throw std::runtime_error(
            "procedural loop variable has a non-integral value");
      }
      const auto &value = found->second.integer();
      OptoSlangExpr lowered;
      lowered.kind = OPTO_SLANG_EXPR_CONSTANT;
      lowered.constant_has_width = true;
      lowered.constant_width = checked_width(value.getBitWidth(), symbol.name);
      lowered.constant_bits = exact_binary_string(value);
      return make_expr(design, std::move(lowered), expr);
    }
  }
  if (ValueExpressionBase::isKind(expr.kind)) {
    const auto &symbol = expr.as<ValueExpressionBase>().symbol;
    const ConstantValue *constant = nullptr;
    if (symbol.kind == SymbolKind::Parameter) {
      constant = &symbol.as<ParameterSymbol>().getValue(expr.sourceRange);
    } else if (symbol.kind == SymbolKind::EnumValue) {
      constant = &symbol.as<EnumValueSymbol>().getValue(expr.sourceRange);
    }
    if (constant) {
      return lower_constant_value(design, symbol.getType(), *constant, expr,
                                  symbol, symbol.name);
    }
  }
  if (expr.kind != ExpressionKind::IntegerLiteral) {
    if (auto *constant = expr.getConstant();
        constant && constant->isInteger()) {
      const auto &value = constant->integer();
      OptoSlangExpr lowered;
      lowered.kind = OPTO_SLANG_EXPR_CONSTANT;
      lowered.constant_has_width = true;
      lowered.constant_width =
          checked_width(value.getBitWidth(), "constant expression");
      lowered.constant_bits = exact_binary_string(value);
      return make_expr(design, std::move(lowered), expr);
    }
  }
  if (expr.kind == ExpressionKind::Call &&
      !expr.as<CallExpression>().isSystemCall()) {
    ConstantValue constant;
    if (design.eval_context) {
      constant = expr.eval(*design.eval_context);
    } else {
      EvalContext context(design.body);
      constant = expr.eval(context);
    }
    if (constant && constant.isInteger()) {
      const auto &value = constant.integer();
      OptoSlangExpr lowered;
      lowered.kind = OPTO_SLANG_EXPR_CONSTANT;
      lowered.constant_has_width = true;
      lowered.constant_width =
          checked_width(value.getBitWidth(), "constant function call");
      lowered.constant_bits = exact_binary_string(value);
      return make_expr(design, std::move(lowered), expr);
    }
  }
  switch (expr.kind) {
  case ExpressionKind::Invalid: {
    auto *child = expr.as<InvalidExpression>().child;
    if (!child) {
      throw std::runtime_error("invalid expression at " +
                               expression_location(design, expr));
    }
    return lower_expr(design, *child);
  }
  case ExpressionKind::NamedValue:
  case ExpressionKind::HierarchicalValue:
    return lower_signal_expr(design, expr);
  case ExpressionKind::ElementSelect:
  case ExpressionKind::RangeSelect:
  case ExpressionKind::MemberAccess:
    return lower_select_expr(design, expr);
  case ExpressionKind::IntegerLiteral: {
    const auto &literal = expr.as<IntegerLiteral>();
    auto value = literal.getValue();
    OptoSlangExpr lowered;
    lowered.kind = OPTO_SLANG_EXPR_CONSTANT;
    lowered.constant_has_width = true;
    lowered.constant_width =
        checked_width(value.getBitWidth(), "integer literal");
    lowered.constant_bits = exact_binary_string(value);
    return make_expr(design, std::move(lowered), expr);
  }
  case ExpressionKind::UnbasedUnsizedIntegerLiteral: {
    auto value = expr.as<UnbasedUnsizedIntegerLiteral>().getValue();
    OptoSlangExpr lowered;
    lowered.kind = OPTO_SLANG_EXPR_CONSTANT;
    lowered.constant_has_width = true;
    lowered.constant_width =
        checked_width(value.getBitWidth(), "unbased unsized integer literal");
    lowered.constant_bits = exact_binary_string(value);
    return make_expr(design, std::move(lowered), expr);
  }
  case ExpressionKind::EmptyArgument: {
    OptoSlangExpr lowered;
    lowered.kind = OPTO_SLANG_EXPR_CONSTANT;
    lowered.constant_has_width = true;
    lowered.constant_width =
        checked_width(lowered_type_width(*expr.type), "unconnected expression");
    lowered.constant_bits.assign(lowered.constant_width, 'x');
    return make_expr(design, std::move(lowered), expr);
  }
  case ExpressionKind::LValueReference:
    if (design.lvalue_references.empty()) {
      throw std::runtime_error(
          "compound assignment lvalue reference has no active assignment");
    }
    return design.lvalue_references.back();
  case ExpressionKind::UnaryOp: {
    const auto &unary = expr.as<UnaryExpression>();
    if (unary.op == UnaryOperator::Preincrement ||
        unary.op == UnaryOperator::Predecrement ||
        unary.op == UnaryOperator::Postincrement ||
        unary.op == UnaryOperator::Postdecrement) {
      return lower_update_expression(design, unary);
    }
    if (unary.op == UnaryOperator::Plus) {
      return lower_expr(design, unary.operand());
    }
    if (unary.op == UnaryOperator::Minus) {
      const auto width =
          checked_width(lowered_type_width(*expr.type), "unary minus result");
      OptoSlangExpr zero;
      zero.kind = OPTO_SLANG_EXPR_CONSTANT;
      zero.constant_has_width = true;
      zero.constant_width = width;
      zero.constant_bits.assign(width, '0');
      OptoSlangExpr lowered;
      lowered.kind = OPTO_SLANG_EXPR_BINARY;
      lowered.binary_op = OPTO_SLANG_BINARY_SUB;
      lowered.binary_left = make_expr(design, std::move(zero), expr);
      lowered.binary_right = lower_expr(design, unary.operand());
      return make_expr(design, std::move(lowered), expr);
    }
    if (unary.op == UnaryOperator::BitwiseNand ||
        unary.op == UnaryOperator::BitwiseNor ||
        unary.op == UnaryOperator::BitwiseXnor) {
      const auto reduction = unary.op == UnaryOperator::BitwiseNand
                                 ? OPTO_SLANG_UNARY_REDUCTION_AND
                             : unary.op == UnaryOperator::BitwiseNor
                                 ? OPTO_SLANG_UNARY_REDUCTION_OR
                                 : OPTO_SLANG_UNARY_REDUCTION_XOR;
      auto *reduced = make_unary_expr(
          design, reduction, lower_expr(design, unary.operand()), expr);
      return make_unary_expr(design, OPTO_SLANG_UNARY_BIT_NOT, reduced, expr);
    }
    OptoSlangExpr lowered;
    lowered.kind = OPTO_SLANG_EXPR_UNARY;
    lowered.unary_op = lower_unary_op(unary.op);
    lowered.unary_arg = lower_expr(design, unary.operand());
    return make_expr(design, std::move(lowered), expr);
  }
  case ExpressionKind::BinaryOp: {
    const auto &binary = expr.as<BinaryExpression>();
    if (binary.op == BinaryOperator::LogicalAnd ||
        binary.op == BinaryOperator::LogicalOr) {
      return lower_short_circuit_expression(design, binary);
    }
    if ((binary.op == BinaryOperator::Divide ||
         binary.op == BinaryOperator::Mod ||
         binary.op == BinaryOperator::Power)) {
      ConstantValue constant;
      if (design.eval_context) {
        constant = expr.eval(*design.eval_context);
      } else {
        EvalContext context(design.body);
        constant = expr.eval(context);
      }
      if (constant && constant.isInteger()) {
        const auto &value = constant.integer();
        OptoSlangExpr lowered;
        lowered.kind = OPTO_SLANG_EXPR_CONSTANT;
        lowered.constant_has_width = true;
        lowered.constant_width =
            checked_width(value.getBitWidth(), "constant binary expression");
        lowered.constant_bits = exact_binary_string(value);
        return make_expr(design, std::move(lowered), expr);
      }
    }
    if (binary.op == BinaryOperator::BinaryXnor) {
      auto *xored = make_binary_expr(design, OPTO_SLANG_BINARY_BIT_XOR,
                                     lower_expr(design, binary.left()),
                                     lower_expr(design, binary.right()), expr);
      return make_unary_expr(design, OPTO_SLANG_UNARY_BIT_NOT, xored, expr);
    }
    if (binary.op == BinaryOperator::CaseEquality ||
        binary.op == BinaryOperator::CaseInequality ||
        binary.op == BinaryOperator::WildcardEquality ||
        binary.op == BinaryOperator::WildcardInequality) {
      return lower_extended_equality(design, binary, expr);
    }
    if (binary.op == BinaryOperator::Power) {
      if (auto *lowered = lower_constant_exponent_power(design, binary, expr)) {
        return lowered;
      }
    }
    if (binary.op == BinaryOperator::Power &&
        integer_literal_u32(design, binary.left()) == 2) {
      const auto width =
          checked_width(lowered_type_width(*expr.type), "power-of-two result");
      return make_binary_expr(
          design, OPTO_SLANG_BINARY_SHL,
          make_unsigned_constant_expr(design, 1, width, binary.left()),
          lower_expr(design, binary.right()), expr);
    }
    if (binary.op == BinaryOperator::Power) {
      throw std::runtime_error(
          "runtime binary operator '" + copy_string(toString(binary.op)) +
          "' is not supported at " + expression_location(design, expr));
    }
    OptoSlangExpr lowered;
    lowered.kind = OPTO_SLANG_EXPR_BINARY;
    lowered.binary_op = lower_binary_op(binary.op);
    lowered.binary_left = lower_expr(design, binary.left());
    lowered.binary_right = lower_expr(design, binary.right());
    return make_expr(design, std::move(lowered), expr);
  }
  case ExpressionKind::Concatenation: {
    const auto &concat = expr.as<ConcatenationExpression>();
    OptoSlangExpr lowered;
    lowered.kind = OPTO_SLANG_EXPR_CONCAT;
    for (auto *part : concat.operands()) {
      if (part->kind == ExpressionKind::EmptyArgument) {
        continue;
      }
      if (part->kind == ExpressionKind::Replication) {
        const auto &replication = part->as<ReplicationExpression>();
        const auto count = integer_literal_u32(design, replication.count());
        if (!count) {
          throw std::runtime_error(
              "replication expression requires a constant count");
        }
        if (*count == 0) {
          continue;
        }
      }
      lowered.concat_parts.push_back(lower_expr(design, *part));
    }
    if (lowered.concat_parts.empty()) {
      throw std::runtime_error(
          "zero-width concatenation cannot be represented as an RTL value");
    }
    return make_expr(design, std::move(lowered), expr);
  }
  case ExpressionKind::Replication: {
    const auto &replication = expr.as<ReplicationExpression>();
    const auto count = integer_literal_u32(design, replication.count());
    if (!count || *count == 0) {
      throw std::runtime_error(
          "replication expression requires a positive constant count");
    }
    const auto result_width =
        checked_width(lowered_type_width(*expr.type), "replication expression");
    if (*count > result_width) {
      throw std::runtime_error(
          "replication count exceeds its elaborated result width");
    }
    auto *part = lower_expr(design, replication.concat());
    OptoSlangExpr lowered;
    lowered.kind = OPTO_SLANG_EXPR_CONCAT;
    if (part->kind == OPTO_SLANG_EXPR_CONCAT) {
      lowered.concat_parts.reserve(part->concat_parts.size() * *count);
      for (uint32_t index = 0; index < *count; ++index) {
        lowered.concat_parts.insert(lowered.concat_parts.end(),
                                    part->concat_parts.begin(),
                                    part->concat_parts.end());
      }
    } else {
      lowered.concat_parts.assign(*count, part);
    }
    return make_expr(design, std::move(lowered), expr);
  }
  case ExpressionKind::SimpleAssignmentPattern:
  case ExpressionKind::StructuredAssignmentPattern:
  case ExpressionKind::ReplicatedAssignmentPattern: {
    std::span<const Expression *const> elements;
    uint32_t replication_count = 1;
    if (expr.kind == ExpressionKind::SimpleAssignmentPattern) {
      elements = expr.as<SimpleAssignmentPatternExpression>().elements();
    } else if (expr.kind == ExpressionKind::StructuredAssignmentPattern) {
      elements = expr.as<StructuredAssignmentPatternExpression>().elements();
    } else {
      const auto &replicated = expr.as<ReplicatedAssignmentPatternExpression>();
      const auto count = integer_literal_u32(design, replicated.count());
      if (!count || *count == 0) {
        throw std::runtime_error(
            "replicated assignment pattern requires a positive constant count");
      }
      if (!expr.type->isFixedSize()) {
        throw std::runtime_error("replicated assignment pattern requires a "
                                 "fixed-size synthesis target");
      }
      replication_count = *count;
      elements = replicated.elements();
    }
    const auto expanded_elements =
        static_cast<uint64_t>(elements.size()) * replication_count;
    if (expanded_elements == 0 ||
        expanded_elements > ASSIGNMENT_PATTERN_ELEMENT_LIMIT) {
      throw std::runtime_error(
          "assignment pattern exceeds the deterministic expansion limit of " +
          std::to_string(ASSIGNMENT_PATTERN_ELEMENT_LIMIT) + " elements");
    }

    // Lower in language evaluation order before adapting to the canonical
    // storage order. This is observable when an element calls a function
    // or contains an assignment expression.
    std::vector<const OptoSlangExpr *> parts;
    parts.reserve(static_cast<size_t>(expanded_elements));
    for (uint32_t repetition = 0; repetition < replication_count;
         ++repetition) {
      for (const auto *element : elements) {
        if (!element) {
          throw std::runtime_error(
              "assignment pattern contains an empty elaborated element");
        }
        parts.push_back(lower_expr(design, *element));
      }
    }
    if (expr.type->isUnpackedArray()) {
      std::ranges::reverse(parts);
    }

    OptoSlangExpr lowered;
    lowered.kind = OPTO_SLANG_EXPR_CONCAT;
    lowered.concat_parts = std::move(parts);
    return make_expr(design, std::move(lowered), expr);
  }
  case ExpressionKind::ConditionalOp: {
    return lower_conditional_expression(design,
                                        expr.as<ConditionalExpression>());
  }
  case ExpressionKind::Streaming:
    return lower_streaming_concatenation(
        design, expr.as<StreamingConcatenationExpression>());
  case ExpressionKind::Inside: {
    const auto &inside = expr.as<InsideExpression>();
    auto *value = lower_expr(design, inside.left());
    OptoSlangExpr *condition = nullptr;
    for (auto *item : inside.rangeList()) {
      if (!item) {
        throw std::runtime_error("inside expression contains a null set item");
      }
      auto *matched = lower_inside_item_match(design, value, *item);
      condition = condition
                      ? make_binary_expr(design, OPTO_SLANG_BINARY_LOGICAL_OR,
                                         condition, matched, *item)
                      : matched;
    }
    if (!condition) {
      throw std::runtime_error("inside expression has an empty set");
    }
    return condition;
  }
  case ExpressionKind::TaggedUnion:
    return lower_tagged_union_expression(design,
                                         expr.as<TaggedUnionExpression>());
  case ExpressionKind::Conversion: {
    const auto &conversion = expr.as<ConversionExpression>();
    const auto &operand = conversion.operand();
    const auto streaming_width =
        operand.kind == ExpressionKind::Streaming
            ? std::optional<uint64_t>(
                  operand.as<StreamingConcatenationExpression>()
                      .getBitstreamWidth())
            : std::nullopt;
    if (!expr.type->isIntegral() ||
        (!streaming_width &&
         (!operand.type->isBitstreamType() || !operand.type->isFixedSize()))) {
      throw std::runtime_error("only fixed bitstream conversion expressions "
                               "are supported for synthesis at " +
                               expression_location(design, expr));
    }
    const auto source_bit_width =
        streaming_width ? *streaming_width : lowered_type_width(*operand.type);
    const auto source_width =
        checked_width(source_bit_width, "conversion source");
    const auto target_width =
        checked_width(lowered_type_width(*expr.type), "conversion target");
    OptoSlangExpr lowered;
    lowered.kind = OPTO_SLANG_EXPR_CAST;
    lowered.cast_value = lower_expr(design, operand);
    lowered.cast_width = target_width;
    lowered.cast_signed = expr.type->isSigned();
    const bool sign_extend =
        conversion.conversionKind == ConversionKind::Propagated
            ? expr.type->isSigned()
            : operand.type->isSigned();
    if (target_width < source_width) {
      lowered.cast_kind = OPTO_SLANG_CAST_TRUNCATE;
    } else if (target_width > source_width && sign_extend) {
      lowered.cast_kind = OPTO_SLANG_CAST_SIGN_EXTEND;
    } else {
      lowered.cast_kind = OPTO_SLANG_CAST_ZERO_EXTEND;
    }
    return make_expr(design, std::move(lowered), expr);
  }
  case ExpressionKind::Call: {
    const auto &call = expr.as<CallExpression>();
    if (!call.isSystemCall()) {
      return lower_function_call(design, call);
    }
    ConstantValue constant;
    if (design.eval_context) {
      constant = expr.eval(*design.eval_context);
    } else {
      EvalContext context(design.body);
      constant = expr.eval(context);
    }
    if (constant && constant.isInteger()) {
      const auto &value = constant.integer();
      OptoSlangExpr lowered;
      lowered.kind = OPTO_SLANG_EXPR_CONSTANT;
      lowered.constant_has_width = true;
      lowered.constant_width =
          checked_width(value.getBitWidth(), "constant system call");
      lowered.constant_bits = exact_binary_string(value);
      return make_expr(design, std::move(lowered), expr);
    }
    const auto name = call.getSubroutineName();
    const auto arguments = call.arguments();
    if (name == "$countones" && arguments.size() == 1 && arguments[0]) {
      return lower_count_ones(design, *arguments[0], expr);
    }
    if (name == "$countbits") {
      return lower_count_bits(design, arguments, expr);
    }
    if (name == "$onehot" && arguments.size() == 1 && arguments[0]) {
      return lower_onehot_call(design, *arguments[0], expr, false);
    }
    if (name == "$onehot0" && arguments.size() == 1 && arguments[0]) {
      return lower_onehot_call(design, *arguments[0], expr, true);
    }
    if (name == "$clog2" && arguments.size() == 1 && arguments[0]) {
      return lower_clog2_call(design, *arguments[0], expr);
    }
    if (name == "$isunknown") {
      throw std::runtime_error(
          "$isunknown requires runtime X/Z observability and is not "
          "synthesizable in the Opto ASIC profile");
    }
    if ((name != "$signed" && name != "$unsigned") || arguments.size() != 1 ||
        !arguments[0]) {
      throw std::runtime_error("unsupported synthesis call '" +
                               copy_string(name) + "'");
    }
    if (!expr.type->isIntegral() || !arguments[0]->type->isIntegral()) {
      throw std::runtime_error(
          "integral cast system call has a non-integral argument");
    }
    const auto source_width = checked_width(
        lowered_type_width(*arguments[0]->type), "system cast source");
    const auto target_width =
        checked_width(lowered_type_width(*expr.type), "system cast target");
    OptoSlangExpr lowered;
    lowered.kind = OPTO_SLANG_EXPR_CAST;
    lowered.cast_value = lower_expr(design, *arguments[0]);
    lowered.cast_width = target_width;
    lowered.cast_signed = name == "$signed";
    if (target_width < source_width) {
      lowered.cast_kind = OPTO_SLANG_CAST_TRUNCATE;
    } else if (target_width > source_width && name == "$signed") {
      lowered.cast_kind = OPTO_SLANG_CAST_SIGN_EXTEND;
    } else {
      lowered.cast_kind = OPTO_SLANG_CAST_ZERO_EXTEND;
    }
    return make_expr(design, std::move(lowered), expr);
  }
  case ExpressionKind::Assignment: {
    const auto &assignment = expr.as<AssignmentExpression>();
    if (assignment.right().kind == ExpressionKind::EmptyArgument) {
      return lower_expr(design, assignment.left());
    }
    return lower_assignment_expression(design, assignment);
  }
  default:
    throw std::runtime_error(
        "unsupported expression kind '" + copy_string(toString(expr.kind)) +
        "' for synthesis lowering at " + expression_location(design, expr));
  }
}
} // namespace opto::slang_lower
