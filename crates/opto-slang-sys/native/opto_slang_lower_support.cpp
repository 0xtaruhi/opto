// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

#include "opto_slang_lower_internal.h"

namespace opto::slang_lower {
ConstantValue evaluate_lowering_constant(ModuleLoweringContext &design,
                                         const Expression &expression) {
  if (design.eval_context) {
    auto value = expression.eval(*design.eval_context);
    if (value) {
      return value;
    }
  }
  EvalContext context(design.body);
  for (const auto &[symbol, value] : design.procedural_constants) {
    context.createLocal(symbol, value);
  }
  return expression.eval(context);
}

std::string copy_string(std::string_view text) {
  if (text.empty()) {
    return {};
  }
  return std::string(text.data(), text.size());
}

uint32_t checked_width(uint64_t width, std::string_view object_name) {
  if (width == 0 || width > UINT32_MAX) {
    throw std::runtime_error("unsupported width for '" +
                             copy_string(object_name) + "'");
  }
  return static_cast<uint32_t>(width);
}

uint64_t lowered_type_width(const Type &source_type) {
  const auto &type = source_type.getCanonicalType();
  if (type.kind == SymbolKind::VoidType) {
    return 0;
  }
  if (type.kind == SymbolKind::FixedSizeUnpackedArrayType) {
    const auto &array = type.as<FixedSizeUnpackedArrayType>();
    const auto elements = array.range.width();
    const auto element_width = lowered_type_width(array.elementType);
    if (element_width != 0 && elements > UINT64_MAX / element_width) {
      throw std::runtime_error(
          "unpacked array synthesis width exceeds 64-bit capacity");
    }
    return elements * element_width;
  }
  if (type.kind == SymbolKind::UnpackedStructType) {
    uint64_t width = 0;
    for (const auto *field : type.as<UnpackedStructType>().fields) {
      const auto field_width = lowered_type_width(field->getType());
      if (field_width > UINT64_MAX - width) {
        throw std::runtime_error(
            "unpacked struct synthesis width exceeds 64-bit capacity");
      }
      width += field_width;
    }
    return width;
  }
  if (type.kind == SymbolKind::UnpackedUnionType) {
    const auto &union_type = type.as<UnpackedUnionType>();
    uint64_t payload_width = 0;
    for (const auto *field : union_type.fields) {
      payload_width =
          std::max(payload_width, lowered_type_width(field->getType()));
    }
    if (!union_type.isTagged) {
      return payload_width;
    }
    const auto tag_width = union_type.fields.empty()
                               ? 0u
                               : static_cast<uint32_t>(std::bit_width(
                                     union_type.fields.size() - 1));
    if (payload_width > UINT64_MAX - tag_width) {
      throw std::runtime_error(
          "tagged union synthesis width exceeds 64-bit capacity");
    }
    return payload_width + tag_width;
  }
  return type.getBitstreamWidth();
}

TaggedUnionLayout tagged_union_layout(const Type &source_type) {
  const auto &type = source_type.getCanonicalType();
  TaggedUnionLayout layout;
  if (type.kind == SymbolKind::PackedUnionType) {
    const auto &union_type = type.as<PackedUnionType>();
    if (!union_type.isTagged) {
      throw std::runtime_error(
          "tagged union layout requested for an untagged packed union");
    }
    layout.tag_width = union_type.tagBits;
    layout.total_width =
        checked_width(lowered_type_width(type), "packed tagged union");
    layout.payload_width = layout.total_width - layout.tag_width;
    return layout;
  }
  if (type.kind == SymbolKind::UnpackedUnionType) {
    const auto &union_type = type.as<UnpackedUnionType>();
    if (!union_type.isTagged) {
      throw std::runtime_error(
          "tagged union layout requested for an untagged unpacked union");
    }
    layout.tag_width = union_type.fields.empty()
                           ? 0u
                           : static_cast<uint32_t>(
                                 std::bit_width(union_type.fields.size() - 1));
    layout.total_width =
        checked_width(lowered_type_width(type), "unpacked tagged union");
    layout.payload_width = layout.total_width - layout.tag_width;
    return layout;
  }
  throw std::runtime_error(
      "tagged union layout requested for a non-union type");
}

const OptoSlangTypeLayout *store_type_layout(ModuleLoweringContext &design,
                                             OptoSlangTypeLayout layout) {
  auto owned = std::make_unique<OptoSlangTypeLayout>(std::move(layout));
  const auto *result = owned.get();
  design.module.type_layouts.emplace_back(std::move(owned));
  return result;
}

const OptoSlangTypeLayout *scalar_type_layout(ModuleLoweringContext &design) {
  if (!design.scalar_type_layout) {
    design.scalar_type_layout = store_type_layout(
        design, OptoSlangTypeLayout{
                    OPTO_SLANG_TYPE_SCALAR, 1, 0, 0, false, nullptr, {}});
  }
  return design.scalar_type_layout;
}

uint32_t aggregate_field_storage_offset(const Type &aggregate_type,
                                        const FieldSymbol &field) {
  const auto &type = aggregate_type.getCanonicalType();
  if (type.kind == SymbolKind::PackedStructType ||
      type.kind == SymbolKind::PackedUnionType) {
    if (field.bitOffset > UINT32_MAX) {
      throw std::runtime_error(
          "packed aggregate field offset exceeds 32-bit capacity");
    }
    return static_cast<uint32_t>(field.bitOffset);
  }
  if (type.kind == SymbolKind::UnpackedUnionType) {
    for (const auto *candidate : type.as<UnpackedUnionType>().fields) {
      if (candidate == &field) {
        return 0;
      }
    }
    throw std::runtime_error(
        "member field does not belong to its unpacked union type");
  }
  if (type.kind != SymbolKind::UnpackedStructType) {
    throw std::runtime_error(
        "aggregate member access requires a fixed-size struct or union");
  }

  uint64_t offset = lowered_type_width(type);
  for (const auto *candidate : type.as<UnpackedStructType>().fields) {
    const auto width = lowered_type_width(candidate->getType());
    if (width > offset) {
      throw std::runtime_error(
          "unpacked struct field layout exceeds aggregate width");
    }
    offset -= width;
    if (candidate == &field) {
      if (offset > UINT32_MAX) {
        throw std::runtime_error(
            "unpacked struct field offset exceeds 32-bit capacity");
      }
      return static_cast<uint32_t>(offset);
    }
  }
  throw std::runtime_error(
      "member field does not belong to its aggregate type");
}

const OptoSlangTypeLayout *intern_type_layout(ModuleLoweringContext &design,
                                              const Type &source_type) {
  const auto &type = source_type.getCanonicalType();
  if (auto found = design.type_layout_by_type.find(&type);
      found != design.type_layout_by_type.end()) {
    return found->second;
  }

  const auto width = checked_width(lowered_type_width(type), "type layout");
  if (width == 1 && !type.isArray() && !type.isStruct()) {
    const auto *layout = scalar_type_layout(design);
    design.type_layout_by_type.emplace(&type, layout);
    return layout;
  }

  OptoSlangTypeLayout layout;
  layout.width = width;
  if (type.kind == SymbolKind::PackedArrayType) {
    const auto &array = type.as<PackedArrayType>();
    layout.kind = OPTO_SLANG_TYPE_ARRAY;
    layout.array_left = array.range.left;
    layout.array_right = array.range.right;
    layout.array_is_packed = true;
    layout.array_element = intern_type_layout(design, array.elementType);
  } else if (type.kind == SymbolKind::FixedSizeUnpackedArrayType) {
    const auto &array = type.as<FixedSizeUnpackedArrayType>();
    layout.kind = OPTO_SLANG_TYPE_ARRAY;
    layout.array_left = array.range.left;
    layout.array_right = array.range.right;
    layout.array_is_packed = false;
    layout.array_element = intern_type_layout(design, array.elementType);
  } else if (type.kind == SymbolKind::PackedStructType) {
    layout.kind = OPTO_SLANG_TYPE_STRUCT;
    for (const auto &field :
         type.as<PackedStructType>().membersOfType<FieldSymbol>()) {
      layout.fields.push_back(OptoSlangTypeLayoutField{
          copy_string(field.name),
          aggregate_field_storage_offset(type, field),
          intern_type_layout(design, field.getType()),
      });
    }
  } else if (type.kind == SymbolKind::UnpackedStructType) {
    layout.kind = OPTO_SLANG_TYPE_STRUCT;
    for (const auto *field : type.as<UnpackedStructType>().fields) {
      layout.fields.push_back(OptoSlangTypeLayoutField{
          copy_string(field->name),
          aggregate_field_storage_offset(type, *field),
          intern_type_layout(design, field->getType()),
      });
    }
  } else if (type.kind == SymbolKind::UnpackedUnionType) {
    if (width > static_cast<uint32_t>(INT32_MAX) + 1u) {
      throw std::runtime_error(
          "unpacked union type layout exceeds signed index capacity");
    }
    layout.kind = OPTO_SLANG_TYPE_ARRAY;
    layout.array_left = static_cast<int32_t>(width - 1);
    layout.array_right = 0;
    layout.array_is_packed = true;
    layout.array_element = scalar_type_layout(design);
  } else {
    const auto range = type.getFixedRange();
    layout.kind = OPTO_SLANG_TYPE_ARRAY;
    layout.array_left = range.left;
    layout.array_right = range.right;
    layout.array_is_packed = true;
    layout.array_element = scalar_type_layout(design);
  }

  const auto *result = store_type_layout(design, std::move(layout));
  design.type_layout_by_type.emplace(&type, result);
  return result;
}

OptoSlangPortDirection lower_direction(ArgumentDirection direction) {
  switch (direction) {
  case ArgumentDirection::In:
    return OPTO_SLANG_PORT_INPUT;
  case ArgumentDirection::Out:
    return OPTO_SLANG_PORT_OUTPUT;
  case ArgumentDirection::InOut:
    return OPTO_SLANG_PORT_INOUT;
  case ArgumentDirection::Ref:
    return OPTO_SLANG_PORT_REF;
  }
  throw std::runtime_error("unknown port direction");
}

OptoSlangUnaryOp lower_unary_op(UnaryOperator op) {
  switch (op) {
  case UnaryOperator::LogicalNot:
    return OPTO_SLANG_UNARY_LOGICAL_NOT;
  case UnaryOperator::BitwiseNot:
    return OPTO_SLANG_UNARY_BIT_NOT;
  case UnaryOperator::BitwiseAnd:
    return OPTO_SLANG_UNARY_REDUCTION_AND;
  case UnaryOperator::BitwiseOr:
    return OPTO_SLANG_UNARY_REDUCTION_OR;
  case UnaryOperator::BitwiseXor:
    return OPTO_SLANG_UNARY_REDUCTION_XOR;
  default:
    throw std::runtime_error("unsupported unary operator '" +
                             copy_string(toString(op)) + "'");
  }
}

OptoSlangBinaryOp lower_binary_op(BinaryOperator op) {
  switch (op) {
  case BinaryOperator::Add:
    return OPTO_SLANG_BINARY_ADD;
  case BinaryOperator::Subtract:
    return OPTO_SLANG_BINARY_SUB;
  case BinaryOperator::Multiply:
    return OPTO_SLANG_BINARY_MUL;
  case BinaryOperator::BinaryAnd:
    return OPTO_SLANG_BINARY_BIT_AND;
  case BinaryOperator::BinaryOr:
    return OPTO_SLANG_BINARY_BIT_OR;
  case BinaryOperator::BinaryXor:
    return OPTO_SLANG_BINARY_BIT_XOR;
  case BinaryOperator::LogicalAnd:
    return OPTO_SLANG_BINARY_LOGICAL_AND;
  case BinaryOperator::LogicalOr:
    return OPTO_SLANG_BINARY_LOGICAL_OR;
  case BinaryOperator::Equality:
    return OPTO_SLANG_BINARY_EQ;
  case BinaryOperator::Inequality:
    return OPTO_SLANG_BINARY_NE;
  case BinaryOperator::LessThan:
    return OPTO_SLANG_BINARY_LT;
  case BinaryOperator::LessThanEqual:
    return OPTO_SLANG_BINARY_LE;
  case BinaryOperator::GreaterThan:
    return OPTO_SLANG_BINARY_GT;
  case BinaryOperator::GreaterThanEqual:
    return OPTO_SLANG_BINARY_GE;
  case BinaryOperator::LogicalShiftLeft:
  case BinaryOperator::ArithmeticShiftLeft:
    return OPTO_SLANG_BINARY_SHL;
  case BinaryOperator::LogicalShiftRight:
    return OPTO_SLANG_BINARY_SHR;
  case BinaryOperator::ArithmeticShiftRight:
    return OPTO_SLANG_BINARY_ASHR;
  case BinaryOperator::Divide:
    return OPTO_SLANG_BINARY_DIV;
  case BinaryOperator::Mod:
    return OPTO_SLANG_BINARY_MOD;
  default:
    throw std::runtime_error("unsupported binary operator '" +
                             copy_string(toString(op)) + "'");
  }
}

uint32_t checked_source_coordinate(size_t value, std::string_view coordinate) {
  if (value > UINT32_MAX) {
    throw std::runtime_error("source " + copy_string(coordinate) +
                             " exceeds 32-bit capacity");
  }
  return static_cast<uint32_t>(value);
}

const std::string *interned_source_path(ModuleLoweringContext &design,
                                        slang::SourceLocation location) {
  const auto buffer = location.buffer().getId();
  auto found = design.module.source_paths_by_buffer.find(buffer);
  if (found == design.module.source_paths_by_buffer.end()) {
    auto file = std::filesystem::path(
        copy_string(design.source_manager->getFileName(location)));
    std::string resolved;
    if (!file.empty()) {
      if (file.is_relative()) {
        file = std::filesystem::absolute(file);
      }
      resolved = file.lexically_normal().string();
    }
    design.module.source_path_storage.push_back(std::move(resolved));
    found = design.module.source_paths_by_buffer
                .emplace(buffer, &design.module.source_path_storage.back())
                .first;
  }
  return found->second->empty() ? nullptr : found->second;
}

void set_expr_source(ModuleLoweringContext &design, OptoSlangExpr &lowered,
                     const Expression &expression) {
  if (!design.source_manager) {
    throw std::runtime_error(
        "slang source manager is unavailable during lowering");
  }
  auto location = expression.sourceRange.start();
  if (!location.valid()) {
    return;
  }
  location = design.source_manager->getFullyOriginalLoc(location);
  lowered.source_file = interned_source_path(design, location);
  lowered.source_line = checked_source_coordinate(
      design.source_manager->getLineNumber(location), "line");
  lowered.source_column = checked_source_coordinate(
      design.source_manager->getColumnNumber(location), "column");
}

OptoSlangSourceSpanView source_span(ModuleLoweringContext &design,
                                    slang::SourceLocation location) {
  if (!design.source_manager) {
    throw std::runtime_error(
        "slang source manager is unavailable during lowering");
  }
  if (!location.valid()) {
    return {};
  }
  location = design.source_manager->getFullyOriginalLoc(location);
  const auto *file = interned_source_path(design, location);
  return {
      file ? file->c_str() : nullptr,
      checked_source_coordinate(design.source_manager->getLineNumber(location),
                                "line"),
      checked_source_coordinate(
          design.source_manager->getColumnNumber(location), "column"),
  };
}

std::string expression_location(const ModuleLoweringContext &design,
                                const Expression &expression) {
  if (!design.source_manager) {
    return "<unknown>";
  }
  auto location = expression.sourceRange.start();
  if (!location.valid()) {
    return "<unknown>";
  }
  location = design.source_manager->getFullyOriginalLoc(location);
  auto file = copy_string(design.source_manager->getFileName(location));
  return file + ":" +
         std::to_string(design.source_manager->getLineNumber(location)) + ":" +
         std::to_string(design.source_manager->getColumnNumber(location));
}

std::string statement_location(const ModuleLoweringContext &design,
                               const Statement &statement) {
  if (!design.source_manager) {
    return "<unknown>";
  }
  auto location = statement.sourceRange.start();
  if (!location.valid()) {
    return "<unknown>";
  }
  location = design.source_manager->getFullyOriginalLoc(location);
  auto file = copy_string(design.source_manager->getFileName(location));
  return file + ":" +
         std::to_string(design.source_manager->getLineNumber(location)) + ":" +
         std::to_string(design.source_manager->getColumnNumber(location));
}

const std::string *intern_string(ModuleLoweringContext &design,
                                 std::string value) {
  auto found = design.module.interned_index.find(std::string_view(value));
  if (found != design.module.interned_index.end()) {
    return found->second;
  }
  design.module.interned_strings.push_back(std::move(value));
  const std::string *stored = &design.module.interned_strings.back();
  design.module.interned_index.emplace(std::string_view(*stored), stored);
  return stored;
}

OptoSlangExpr *make_expr(ModuleLoweringContext &design, OptoSlangExpr expr,
                         const Expression &source) {
  set_expr_source(design, expr, source);
  if (expr.kind == OPTO_SLANG_EXPR_CONSTANT && source.type) {
    expr.constant_signed = source.type->isSigned();
  }
  design.module.exprs.push_back(std::move(expr));
  return &design.module.exprs.back();
}

OptoSlangExpr *make_signal_expr(ModuleLoweringContext &design,
                                std::string name) {
  OptoSlangExpr expr;
  expr.kind = OPTO_SLANG_EXPR_SIGNAL;
  expr.signal_name = intern_string(design, std::move(name));
  design.module.exprs.push_back(std::move(expr));
  return &design.module.exprs.back();
}

bool is_port_backref(const ValueSymbol &symbol) {
  return symbol.getFirstPortBackref() != nullptr;
}

std::string unsupported_member_message(const InstanceBodySymbol &body,
                                       const Symbol &symbol) {
  auto module_name = copy_string(body.getDefinition().name);
  if (symbol.kind == SymbolKind::ProceduralBlock) {
    const auto &proc = symbol.as<ProceduralBlockSymbol>();
    return "unsupported procedural block '" +
           copy_string(SemanticFacts::getProcedureKindStr(proc.procedureKind)) +
           "' in module '" + module_name + "'";
  }
  if (symbol.kind == SymbolKind::GenerateBlock ||
      symbol.kind == SymbolKind::GenerateBlockArray) {
    return "unsupported generate block in module '" + module_name + "'";
  }
  if (!symbol.name.empty()) {
    return "unsupported " + copy_string(toString(symbol.kind)) + " member '" +
           copy_string(symbol.name) + "' in module '" + module_name + "'";
  }
  return "unsupported " + copy_string(toString(symbol.kind)) +
         " member in module '" + module_name + "'";
}

void collect_interface_behavior(const InstanceBodySymbol &body,
                                const Scope &scope, ModuleMembers &members);

void collect_instance_leaf(const InstanceBodySymbol &body, const Symbol &symbol,
                           ModuleMembers &members) {
  switch (symbol.kind) {
  case SymbolKind::Instance: {
    const auto &instance = symbol.as<InstanceSymbol>();
    if (instance.isInterface()) {
      members.interface_instances.push_back(&instance);
      collect_interface_behavior(body, instance.body, members);
    } else {
      members.instances.push_back(&instance);
    }
    break;
  }
  case SymbolKind::PrimitiveInstance:
    members.primitives.push_back(&symbol.as<PrimitiveInstanceSymbol>());
    break;
  case SymbolKind::InstanceArray:
    for (auto *element : symbol.as<InstanceArraySymbol>().elements) {
      if (element) {
        collect_instance_leaf(body, *element, members);
      }
    }
    break;
  default:
    throw std::runtime_error(unsupported_member_message(body, symbol));
  }
}

void collect_interface_behavior(const InstanceBodySymbol &body,
                                const Scope &scope, ModuleMembers &members) {
  for (const auto &symbol : scope.members()) {
    switch (symbol.kind) {
    case SymbolKind::StatementBlock:
      collect_interface_behavior(body, symbol.as<StatementBlockSymbol>(),
                                 members);
      break;
    case SymbolKind::GenerateBlock: {
      const auto &block = symbol.as<GenerateBlockSymbol>();
      if (!block.isUninstantiated) {
        collect_interface_behavior(body, block, members);
      }
      break;
    }
    case SymbolKind::GenerateBlockArray:
      for (auto *block : symbol.as<GenerateBlockArraySymbol>().entries) {
        if (block && !block->isUninstantiated) {
          collect_interface_behavior(body, *block, members);
        }
      }
      break;
    case SymbolKind::Instance:
    case SymbolKind::InstanceArray:
    case SymbolKind::PrimitiveInstance:
      collect_instance_leaf(body, symbol, members);
      break;
    case SymbolKind::ContinuousAssign:
      members.assigns.push_back(&symbol.as<ContinuousAssignSymbol>());
      break;
    case SymbolKind::ProceduralBlock:
      members.processes.push_back(&symbol.as<ProceduralBlockSymbol>());
      break;
    default:
      break;
    }
  }
}

void collect_elaborated_members(const InstanceBodySymbol &body,
                                const Scope &scope, ModuleMembers &members) {
  for (const auto &symbol : scope.members()) {
    switch (symbol.kind) {
    case SymbolKind::StatementBlock:
      collect_elaborated_members(body, symbol.as<StatementBlockSymbol>(),
                                 members);
      break;
    case SymbolKind::GenerateBlock: {
      const auto &block = symbol.as<GenerateBlockSymbol>();
      if (!block.isUninstantiated) {
        collect_elaborated_members(body, block, members);
      }
      break;
    }
    case SymbolKind::GenerateBlockArray: {
      const auto &array = symbol.as<GenerateBlockArraySymbol>();
      for (auto *block : array.entries) {
        if (block && !block->isUninstantiated) {
          collect_elaborated_members(body, *block, members);
        }
      }
      break;
    }
    case SymbolKind::Net:
      members.nets.push_back(&symbol.as<NetSymbol>());
      break;
    case SymbolKind::Variable:
      members.variables.push_back(&symbol.as<VariableSymbol>());
      break;
    case SymbolKind::Instance:
    case SymbolKind::InstanceArray:
      collect_instance_leaf(body, symbol, members);
      break;
    case SymbolKind::UninstantiatedDef:
      members.unresolved_instances.push_back(
          &symbol.as<UninstantiatedDefSymbol>());
      break;
    case SymbolKind::PrimitiveInstance:
      collect_instance_leaf(body, symbol, members);
      break;
    case SymbolKind::ContinuousAssign:
      members.assigns.push_back(&symbol.as<ContinuousAssignSymbol>());
      break;
    case SymbolKind::ProceduralBlock:
      members.processes.push_back(&symbol.as<ProceduralBlockSymbol>());
      break;
    case SymbolKind::Port:
    case SymbolKind::MultiPort:
    case SymbolKind::InterfacePort:
    case SymbolKind::TransparentMember:
    case SymbolKind::EmptyMember:
    case SymbolKind::Parameter:
    case SymbolKind::TypeParameter:
    case SymbolKind::TypeAlias:
    case SymbolKind::NetType:
    case SymbolKind::Genvar:
    case SymbolKind::Iterator:
    case SymbolKind::PatternVar:
    case SymbolKind::ExplicitImport:
    case SymbolKind::WildcardImport:
    case SymbolKind::Subroutine:
    case SymbolKind::LetDecl:
    case SymbolKind::DefParam:
    case SymbolKind::Specparam:
    case SymbolKind::SpecifyBlock:
      break;
    case SymbolKind::Primitive:
    case SymbolKind::Sequence:
    case SymbolKind::Property:
    case SymbolKind::AssertionPort:
    case SymbolKind::ClockingBlock:
    case SymbolKind::Checker:
    case SymbolKind::CheckerInstance:
    case SymbolKind::CovergroupType:
    case SymbolKind::CovergroupBody:
    case SymbolKind::AnonymousProgram:
    case SymbolKind::NetAlias:
    case SymbolKind::ConfigBlock:
      throw std::runtime_error(unsupported_member_message(body, symbol));
    default:
      throw std::runtime_error(unsupported_member_message(body, symbol));
    }
  }
}

void collect_interface_leaves(const Symbol *symbol,
                              std::vector<const InstanceSymbol *> &leaves) {
  if (!symbol) {
    return;
  }
  if (symbol->kind == SymbolKind::Instance) {
    const auto &instance = symbol->as<InstanceSymbol>();
    if (!instance.isInterface()) {
      throw std::runtime_error(
          "interface connection references non-interface instance '" +
          copy_string(instance.name) + "'");
    }
    leaves.push_back(&instance);
    return;
  }
  if (symbol->kind == SymbolKind::InstanceArray) {
    for (auto *element : symbol->as<InstanceArraySymbol>().elements) {
      collect_interface_leaves(element, leaves);
    }
    return;
  }
  throw std::runtime_error("interface connection references unsupported " +
                           copy_string(toString(symbol->kind)) + " symbol");
}

std::vector<InterfaceSignal> interface_signals(const InstanceSymbol &instance,
                                               std::string_view modport_name) {
  std::vector<InterfaceSignal> signals;
  if (modport_name.empty()) {
    for (const auto &symbol : instance.body.members()) {
      if (symbol.kind == SymbolKind::Net ||
          symbol.kind == SymbolKind::Variable) {
        signals.push_back(InterfaceSignal{
            copy_string(symbol.name),
            &symbol.as<ValueSymbol>(),
            &symbol.as<ValueSymbol>(),
            nullptr,
            ArgumentDirection::InOut,
        });
      }
    }
    return signals;
  }

  auto *symbol = instance.body.find(modport_name);
  if (!symbol || symbol->kind != SymbolKind::Modport) {
    throw std::runtime_error("interface instance '" +
                             copy_string(instance.name) + "' has no modport '" +
                             copy_string(modport_name) + "'");
  }
  const auto storage_belongs_to_instance = [&](const ValueSymbol &value) {
    auto *scope = value.getParentScope();
    while (scope) {
      const auto &parent = scope->asSymbol();
      if (&parent == &instance.body) {
        return true;
      }
      if (parent.kind == SymbolKind::Subroutine) {
        return false;
      }
      scope = parent.getParentScope();
    }
    return false;
  };
  struct MethodCapture {
    const ValueSymbol *value;
    const Expression *expression;
    bool written;
  };
  const auto append_method_captures =
      [&](const MethodPrototypeSymbol &method,
          const SubroutineSymbol &implementation) {
        struct CaptureVisitor
            : ASTVisitor<CaptureVisitor, VisitFlags::AllGood> {
          explicit CaptureVisitor(
              const decltype(storage_belongs_to_instance) &belongs)
              : belongs(belongs) {}

          void capture(const ValueSymbol &value, const Expression &expression,
                       bool written = false) {
            if (!belongs(value)) {
              if ((value.kind == SymbolKind::Net ||
                   value.kind == SymbolKind::Variable) &&
                  !is_subroutine_local(value)) {
                external_storage.push_back(&value);
              }
              return;
            }
            auto existing =
                std::ranges::find(captures, &value, &MethodCapture::value);
            if (existing == captures.end()) {
              captures.push_back(MethodCapture{&value, &expression, written});
            } else {
              existing->written = existing->written || written;
            }
          }

          void handle(const NamedValueExpression &expression) {
            capture(expression.symbol, expression);
          }

          void handle(const HierarchicalValueExpression &expression) {
            capture(expression.symbol, expression);
          }

          void handle(const CallExpression &expression) {
            if (const auto *selected = std::get_if<const SubroutineSymbol *>(
                    &expression.subroutine);
                selected && *selected) {
              const auto &called = resolve_synthesizable_subroutine(
                  **selected, expression.sourceRange.start());
              calls.push_back(&called);
              const auto arguments = called.getArguments();
              const auto actuals = expression.arguments();
              for (size_t index = 0;
                   index < std::min(arguments.size(), actuals.size());
                   ++index) {
                const auto *argument = arguments[index];
                const auto *actual = actuals[index];
                if (!argument || !actual ||
                    argument->direction == ArgumentDirection::In) {
                  continue;
                }
                const auto &lvalue = call_output_lvalue(*actual);
                if (const auto *value = expression_root_value(lvalue)) {
                  capture(*value, lvalue, true);
                }
              }
            }
            visitDefault(expression);
          }

          static bool is_subroutine_local(const ValueSymbol &value) {
            auto *scope = value.getParentScope();
            while (scope) {
              const auto &parent = scope->asSymbol();
              if (parent.kind == SymbolKind::Subroutine) {
                return true;
              }
              scope = parent.getParentScope();
            }
            return false;
          }

          const decltype(storage_belongs_to_instance) &belongs;
          std::vector<MethodCapture> captures;
          std::vector<const SubroutineSymbol *> calls;
          std::vector<const ValueSymbol *> external_storage;
        };
        std::vector<MethodCapture> captures;
        std::vector<const SubroutineSymbol *> active;
        const auto collect = [&](const auto &self,
                                 const SubroutineSymbol &subroutine) -> void {
          if (std::ranges::find(active, &subroutine) != active.end()) {
            return;
          }
          active.push_back(&subroutine);
          CaptureVisitor visitor(storage_belongs_to_instance);
          subroutine.getBody().visit(visitor);
          if (!visitor.external_storage.empty()) {
            const auto *value = visitor.external_storage.front();
            throw LoweringFailure(
                OPTO_SLANG_LOWERING_UNSUPPORTED_PROFILE, 6, method.location,
                "modport method '" + copy_string(method.name) +
                    "' captures storage outside its interface: '" +
                    copy_string(value->name) + "'");
          }
          for (auto capture : visitor.captures) {
            capture.written =
                capture.written ||
                statement_assigns_value(subroutine.getBody(), *capture.value);
            auto existing = std::ranges::find(captures, capture.value,
                                              &MethodCapture::value);
            if (existing == captures.end()) {
              captures.push_back(capture);
            } else {
              existing->written = existing->written || capture.written;
            }
          }
          for (const auto *called : visitor.calls) {
            self(self, *called);
          }
          active.pop_back();
        };
        collect(collect, implementation);
        for (const auto &capture : captures) {
          const auto *value = capture.value;
          auto direction =
              capture.written ? ArgumentDirection::Ref : ArgumentDirection::In;
          if (direction == ArgumentDirection::Ref &&
              value->kind == SymbolKind::Net) {
            throw LoweringFailure(
                OPTO_SLANG_LOWERING_UNSUPPORTED_PROFILE, 3, method.location,
                "modport method '" + copy_string(method.name) +
                    "' writes captured net '" + copy_string(value->name) + "'");
          }
          auto existing =
              std::ranges::find_if(signals, [&](const InterfaceSignal &signal) {
                return signal.value == value;
              });
          if (existing != signals.end()) {
            if (direction == ArgumentDirection::Ref) {
              existing->direction = ArgumentDirection::Ref;
            }
            continue;
          }
          auto name = "__opto_method_" + copy_string(method.name) + "." +
                      module_relative_name(instance.body, *value);
          signals.push_back(InterfaceSignal{
              std::move(name),
              value,
              value,
              capture.expression,
              direction,
          });
        }
      };
  // Materialize named ports before method dependencies so a method capture
  // reuses an explicitly exposed member instead of adding a hidden duplicate.
  for (const auto &member : symbol->as<ModportSymbol>().members()) {
    if (member.kind == SymbolKind::MethodPrototype) {
      continue;
    }
    if (member.kind != SymbolKind::ModportPort) {
      throw std::runtime_error("unsupported modport member '" +
                               copy_string(member.name) + "'");
    }
    const auto &port = member.as<ModportPortSymbol>();
    const auto *connection = port.getConnectionExpr();
    if (!connection) {
      throw std::runtime_error("modport port '" + copy_string(port.name) +
                               "' has no elaborated connection");
    }
    const ValueSymbol *value = nullptr;
    if (port.internalSymbol && ValueSymbol::isKind(port.internalSymbol->kind)) {
      value = &port.internalSymbol->as<ValueSymbol>();
    }
    signals.push_back(InterfaceSignal{
        copy_string(port.name),
        value,
        &port,
        connection,
        port.direction,
    });
  }
  for (const auto &member : symbol->as<ModportSymbol>().members()) {
    if (member.kind != SymbolKind::MethodPrototype) {
      continue;
    }
    const auto &method = member.as<MethodPrototypeSymbol>();
    const auto *selected = method.getSubroutine();
    if (!selected) {
      throw LoweringFailure(OPTO_SLANG_LOWERING_UNSUPPORTED_PROFILE, 2,
                            member.location,
                            "modport method '" + copy_string(member.name) +
                                "' has no statically resolved implementation");
    }
    const auto &implementation =
        resolve_synthesizable_subroutine(*selected, member.location);
    append_method_captures(method, implementation);
  }
  return signals;
}

const SubroutineSymbol &
resolve_synthesizable_subroutine(const SubroutineSymbol &selected,
                                 slang::SourceLocation location) {
  const SubroutineSymbol *implementation = &selected;
  if (selected.flags.has(MethodFlags::InterfaceExtern)) {
    const auto *prototype = selected.getPrototype();
    const auto *candidate =
        prototype ? prototype->getFirstExternImpl() : nullptr;
    if (!candidate) {
      throw LoweringFailure(
          OPTO_SLANG_LOWERING_UNSUPPORTED_PROFILE, 2, location,
          "extern interface method '" + copy_string(selected.name) +
              "' has no statically resolved implementation");
    }
    if (candidate->getNextImpl()) {
      throw LoweringFailure(
          OPTO_SLANG_LOWERING_UNSUPPORTED_PROFILE, 4, location,
          "extern interface method '" + copy_string(selected.name) +
              "' has ambiguous exported implementations");
    }
    implementation = candidate->impl;
  }
  if (implementation->isVirtual() ||
      implementation->flags.has(MethodFlags::DPIImport |
                                MethodFlags::BuiltIn)) {
    throw LoweringFailure(
        OPTO_SLANG_LOWERING_UNSUPPORTED_PROFILE, 2, location,
        "modport method '" + copy_string(selected.name) +
            "' does not resolve to a synthesizable implementation");
  }
  return *implementation;
}

std::optional<ArgumentDirection>
merge_interface_direction(std::optional<ArgumentDirection> current,
                          ArgumentDirection next) {
  if (!current || *current == next) {
    return next;
  }
  if (*current == ArgumentDirection::Ref || next == ArgumentDirection::Ref) {
    return ArgumentDirection::Ref;
  }
  return ArgumentDirection::InOut;
}

std::optional<ArgumentDirection> infer_interface_direction_impl(
    const InstanceBodySymbol &body, const ValueSymbol &value,
    std::unordered_set<const InstanceBodySymbol *> &visiting) {
  if (!visiting.insert(&body).second) {
    throw std::runtime_error(
        "recursive generic interface direction inference in module '" +
        copy_string(body.getDefinition().name) + "'");
  }

  std::optional<ArgumentDirection> direction;
  ModuleMembers members;
  collect_elaborated_members(body, body, members);
  for (auto *child : members.instances) {
    for (auto *connection : child->getPortConnections()) {
      if (!connection || connection->port.kind != SymbolKind::InterfacePort) {
        continue;
      }
      const auto &port = connection->port.as<InterfacePortSymbol>();
      auto [connected, selected_modport] = connection->getIfaceConn();
      std::vector<const InstanceSymbol *> leaves;
      collect_interface_leaves(connected, leaves);
      auto modport_name = port.modport;
      if (modport_name.empty() && selected_modport) {
        modport_name = selected_modport->name;
      }
      for (auto *leaf : leaves) {
        for (const auto &signal : interface_signals(*leaf, modport_name)) {
          if (signal.value != &value &&
              (!signal.connection || !expression_references_interface_value(
                                         *signal.connection, value))) {
            continue;
          }
          auto child_direction =
              modport_name.empty()
                  ? infer_interface_direction_impl(child->body, value, visiting)
                  : std::optional(signal.direction);
          if (child_direction) {
            direction = merge_interface_direction(direction, *child_direction);
          }
        }
      }
    }
  }
  visiting.erase(&body);
  return direction;
}

ArgumentDirection infer_interface_direction(const InstanceBodySymbol &body,
                                            const ValueSymbol &value) {
  std::unordered_set<const InstanceBodySymbol *> visiting;
  auto direction = infer_interface_direction_impl(body, value, visiting);
  if (!direction) {
    throw std::runtime_error(
        "cannot infer direction of generic interface signal '" +
        value.getHierarchicalPath() + "' in module '" +
        copy_string(body.getDefinition().name) + "'");
  }
  return *direction;
}

bool same_interface_value(const Symbol *reference, const ValueSymbol &target) {
  if (!reference) {
    return false;
  }
  if (reference == &target ||
      reference->getHierarchicalPath() == target.getHierarchicalPath()) {
    return true;
  }
  if (reference->kind != SymbolKind::ModportPort) {
    return false;
  }
  const auto &port = reference->as<ModportPortSymbol>();
  if (port.internalSymbol == &target ||
      (port.internalSymbol && ValueSymbol::isKind(port.internalSymbol->kind) &&
       port.internalSymbol->getHierarchicalPath() ==
           target.getHierarchicalPath())) {
    return true;
  }
  return port.explicitConnection && expression_references_interface_value(
                                        *port.explicitConnection, target);
}

bool expression_references_interface_value(const Expression &expression,
                                           const ValueSymbol &target) {
  if (same_interface_value(expression.getSymbolReference(), target)) {
    return true;
  }
  switch (expression.kind) {
  case ExpressionKind::MemberAccess:
    return expression_references_interface_value(
        expression.as<MemberAccessExpression>().value(), target);
  case ExpressionKind::ElementSelect:
    return expression_references_interface_value(
        expression.as<ElementSelectExpression>().value(), target);
  case ExpressionKind::RangeSelect:
    return expression_references_interface_value(
        expression.as<RangeSelectExpression>().value(), target);
  case ExpressionKind::Conversion:
    return expression_references_interface_value(
        expression.as<ConversionExpression>().operand(), target);
  case ExpressionKind::Replication:
    return expression_references_interface_value(
        expression.as<ReplicationExpression>().concat(), target);
  case ExpressionKind::Assignment: {
    const auto &assignment = expression.as<AssignmentExpression>();
    return expression_references_interface_value(assignment.left(), target) ||
           expression_references_interface_value(assignment.right(), target);
  }
  case ExpressionKind::Concatenation:
    for (auto *operand : expression.as<ConcatenationExpression>().operands()) {
      if (operand && expression_references_interface_value(*operand, target)) {
        return true;
      }
    }
    return false;
  default:
    return false;
  }
}

struct InterfaceValueDriverVisitor
    : ASTVisitor<InterfaceValueDriverVisitor, VisitFlags::AllGood> {
  explicit InterfaceValueDriverVisitor(const ValueSymbol &target)
      : target(target) {}

  void handle(const AssignmentExpression &assignment) {
    driven |= expression_references_interface_value(assignment.left(), target);
    visitDefault(assignment);
  }

  void handle(const InstanceSymbol &) {}
  void handle(const InstanceArraySymbol &) {}

  const ValueSymbol &target;
  bool driven = false;
};

bool interface_value_is_driven_impl(
    const InstanceBodySymbol &body, const ValueSymbol &value,
    std::unordered_set<const InstanceBodySymbol *> &visiting) {
  if (!visiting.insert(&body).second) {
    throw std::runtime_error("recursive interface driver analysis in module '" +
                             copy_string(body.getDefinition().name) + "'");
  }

  ModuleMembers members;
  collect_elaborated_members(body, body, members);
  InterfaceValueDriverVisitor visitor(value);
  for (auto *assign : members.assigns) {
    const auto &expression = assign->getAssignment();
    if (expression.kind == ExpressionKind::Assignment) {
      visitor.handle(expression.as<AssignmentExpression>());
    } else {
      expression.visit(visitor);
    }
  }
  for (auto *process : members.processes) {
    process->getBody().visit(visitor);
  }
  if (visitor.driven) {
    visiting.erase(&body);
    return true;
  }

  for (auto *child : members.instances) {
    for (auto *connection : child->getPortConnections()) {
      if (!connection) {
        continue;
      }
      if (connection->port.kind == SymbolKind::Port) {
        const auto &port = connection->port.as<PortSymbol>();
        if (port.direction != ArgumentDirection::Out &&
            port.direction != ArgumentDirection::InOut) {
          continue;
        }
        auto *expression = connection->getExpression();
        if (expression &&
            expression_references_interface_value(*expression, value)) {
          visiting.erase(&body);
          return true;
        }
        continue;
      }
      if (connection->port.kind != SymbolKind::InterfacePort) {
        continue;
      }
      auto [connected, selected_modport] = connection->getIfaceConn();
      static_cast<void>(selected_modport);
      std::vector<const InstanceSymbol *> leaves;
      collect_interface_leaves(connected, leaves);
      bool carries_value = false;
      for (auto *leaf : leaves) {
        for (const auto &signal : interface_signals(*leaf, {})) {
          carries_value |= same_interface_value(signal.value, value);
        }
      }
      if (carries_value &&
          interface_value_is_driven_impl(child->body, value, visiting)) {
        visiting.erase(&body);
        return true;
      }
    }
  }
  visiting.erase(&body);
  return false;
}

bool interface_value_is_driven(const InstanceBodySymbol &body,
                               const ValueSymbol &value) {
  std::unordered_set<const InstanceBodySymbol *> visiting;
  return interface_value_is_driven_impl(body, value, visiting);
}

std::string flattened_interface_port_name(std::string_view port, size_t element,
                                          size_t element_count,
                                          std::string_view signal) {
  std::string name(port);
  if (element_count > 1) {
    name += "[" + std::to_string(element) + "]";
  }
  name.push_back('.');
  name += signal;
  return name;
}

std::string module_relative_name(const InstanceBodySymbol &body,
                                 const Symbol &symbol) {
  auto body_path = body.getHierarchicalPath();
  auto symbol_path = symbol.getHierarchicalPath();
  body_path.push_back('.');
  if (!symbol_path.starts_with(body_path)) {
    throw std::runtime_error("symbol '" + symbol_path +
                             "' is outside module '" +
                             copy_string(body.getDefinition().name) + "'");
  }
  auto relative = symbol_path.substr(body_path.size());
  if (relative.empty()) {
    throw std::runtime_error(
        "elaborated module member has an empty relative name");
  }
  return relative;
}

bool is_procedural_local(const InstanceBodySymbol &body, const Symbol &symbol) {
  auto *scope = symbol.getParentScope();
  bool found_statement_block = false;
  while (scope) {
    const auto &parent = scope->asSymbol();
    if (&parent == &body) {
      return found_statement_block;
    }
    found_statement_block |= parent.kind == SymbolKind::StatementBlock;
    scope = parent.getParentScope();
  }
  throw std::runtime_error("symbol '" + symbol.getHierarchicalPath() +
                           "' is outside its active module body");
}

std::string procedural_local_base_name(const InstanceBodySymbol &body,
                                       const ValueSymbol &symbol) {
  std::vector<const Symbol *> blocks;
  auto *scope = symbol.getParentScope();
  while (scope) {
    const auto &parent = scope->asSymbol();
    if (&parent == &body) {
      break;
    }
    if (parent.kind == SymbolKind::StatementBlock) {
      blocks.push_back(&parent);
    }
    scope = parent.getParentScope();
  }
  if (!scope || blocks.empty()) {
    throw std::runtime_error("procedural local '" + copy_string(symbol.name) +
                             "' has no lexical statement block");
  }

  std::string name = "__opto_local";
  for (auto iter = blocks.rbegin(); iter != blocks.rend(); ++iter) {
    const auto &block = **iter;
    name.push_back('_');
    if (block.location.valid()) {
      name += std::to_string(block.location.buffer().getId());
      name.push_back('_');
      name += std::to_string(block.location.offset());
    } else {
      name += std::to_string(static_cast<uint32_t>(block.getIndex()));
    }
  }
  name.push_back('_');
  name += copy_string(symbol.name);
  return name;
}

std::string unique_internal_name(std::unordered_set<std::string> &existing,
                                 std::string base) {
  if (existing.insert(base).second) {
    return base;
  }
  for (uint64_t suffix = 1;; ++suffix) {
    auto candidate = base + "_" + std::to_string(suffix);
    if (existing.insert(candidate).second) {
      return candidate;
    }
  }
}

std::string add_internal_net(ModuleLoweringContext &design, std::string base,
                             uint32_t width, bool is_signed,
                             bool is_process_local) {
  auto name = unique_internal_name(design.net_names, std::move(base));
  design.module.nets.push_back(OptoSlangNetData{
      name,
      width,
      is_signed,
      is_signed,
      is_process_local,
      OPTO_SLANG_NET_SINGLE_DRIVER,
      nullptr,
      {},
  });
  design.value_shapes.insert_or_assign(name, ValueShape{width, is_signed});
  return name;
}

std::string registered_value_name(const ModuleLoweringContext &design,
                                  const ValueSymbol &symbol) {
  const ValueSymbol *canonical = &symbol;
  if (symbol.kind == SymbolKind::ModportPort) {
    const auto *internal = symbol.as<ModportPortSymbol>().internalSymbol;
    if (internal && ValueSymbol::isKind(internal->kind)) {
      canonical = &internal->as<ValueSymbol>();
    }
  }
  auto body = design.interface_port_names.find(&design.body);
  if (body != design.interface_port_names.end()) {
    auto found = body->second.find(&symbol);
    if (found != body->second.end()) {
      return found->second;
    }
    found = body->second.find(canonical);
    if (found != body->second.end()) {
      return found->second;
    }
  }
  auto found = design.value_names.find(canonical);
  if (found == design.value_names.end()) {
    throw std::runtime_error("signal '" + symbol.getHierarchicalPath() +
                             "' was not registered in its elaborated module");
  }
  return found->second;
}

const OptoSlangExpr *find_function_lvalue(const ModuleLoweringContext &design,
                                          const ValueSymbol &symbol) {
  if (auto found = design.function_lvalues.find(&symbol);
      found != design.function_lvalues.end()) {
    return found->second;
  }
  // Slang clones automatic subroutine formals when materializing a body. The
  // clone retains originating syntax identity, which is the stable alias key
  // for nested calls; hierarchical names are not used as semantic identity.
  const auto *syntax = symbol.getSyntax();
  if (!syntax) {
    return nullptr;
  }
  const OptoSlangExpr *matched = nullptr;
  for (const auto &[bound_symbol, value] : design.function_lvalues) {
    if (bound_symbol->getSyntax() == syntax) {
      if (matched && matched != value) {
        throw std::runtime_error("ambiguous subroutine ref binding for '" +
                                 symbol.getHierarchicalPath() + "'");
      }
      matched = value;
    }
  }
  return matched;
}

bool has_registered_value(const ModuleLoweringContext &design,
                          const ValueSymbol &symbol) {
  const ValueSymbol *canonical = &symbol;
  if (symbol.kind == SymbolKind::ModportPort) {
    const auto *internal = symbol.as<ModportPortSymbol>().internalSymbol;
    if (internal && ValueSymbol::isKind(internal->kind)) {
      canonical = &internal->as<ValueSymbol>();
    }
  }
  auto body = design.interface_port_names.find(&design.body);
  if (body != design.interface_port_names.end()) {
    if (body->second.contains(&symbol) || body->second.contains(canonical)) {
      return true;
    }
  }
  return design.value_names.contains(canonical);
}

std::optional<uint32_t> integer_literal_u32(ModuleLoweringContext &design,
                                            const Expression &expr) {
  auto convert = [](const SVInt &value) -> std::optional<uint32_t> {
    if (value.hasUnknown()) {
      return std::nullopt;
    }
    auto int_value = value.as<uint64_t>();
    if (!int_value || *int_value > UINT32_MAX) {
      return std::nullopt;
    }
    return static_cast<uint32_t>(*int_value);
  };
  if (expr.kind == ExpressionKind::IntegerLiteral) {
    return convert(expr.as<IntegerLiteral>().getValue());
  }
  if (ValueExpressionBase::isKind(expr.kind)) {
    const auto &symbol = expr.as<ValueExpressionBase>().symbol;
    if (symbol.kind == SymbolKind::Parameter) {
      const auto &value =
          symbol.as<ParameterSymbol>().getValue(expr.sourceRange);
      if (value.isInteger()) {
        return convert(value.integer());
      }
    }
    if (symbol.kind == SymbolKind::EnumValue) {
      const auto &value =
          symbol.as<EnumValueSymbol>().getValue(expr.sourceRange);
      if (value.isInteger()) {
        return convert(value.integer());
      }
    }
  }
  if (expr.kind == ExpressionKind::Conversion) {
    return integer_literal_u32(design,
                               expr.as<ConversionExpression>().operand());
  }
  if (auto *constant = expr.getConstant(); constant && constant->isInteger()) {
    return convert(constant->integer());
  }
  auto constant = evaluate_lowering_constant(design, expr);
  if (constant && constant.isInteger()) {
    return convert(constant.integer());
  }
  return std::nullopt;
}

std::string exact_binary_string(const SVInt &value) {
  const auto width = value.getBitWidth();
  if (width > static_cast<bitwidth_t>(std::numeric_limits<int32_t>::max())) {
    throw std::runtime_error(
        "integer literal width exceeds supported bit indexing");
  }
  std::string bits;
  bits.reserve(static_cast<size_t>(width));
  for (auto index = static_cast<int32_t>(width); index > 0; --index) {
    bits.push_back(value[index - 1].toChar());
  }
  return bits;
}

OptoSlangExpr *lower_expr(ModuleLoweringContext &design,
                          const Expression &expr);
std::optional<bool> constant_boolean_value(ModuleLoweringContext &design,
                                           const Expression &expression);
bool is_empty_connection_expression(const Expression &expr);

const Expression &
require_primitive_port(const PrimitiveInstanceSymbol &instance,
                       std::span<const Expression *const> ports, size_t index) {
  auto *expression = ports[index];
  if (!expression || is_empty_connection_expression(*expression)) {
    throw std::runtime_error("primitive '" +
                             copy_string(instance.primitiveType.name) +
                             "' instance '" + copy_string(instance.name) +
                             "' has an empty terminal");
  }
  return *expression;
}

OptoSlangExpr *make_unary_expr(ModuleLoweringContext &design,
                               OptoSlangUnaryOp op, const OptoSlangExpr *arg,
                               const Expression &source) {
  OptoSlangExpr lowered;
  lowered.kind = OPTO_SLANG_EXPR_UNARY;
  lowered.unary_op = op;
  lowered.unary_arg = arg;
  return make_expr(design, std::move(lowered), source);
}

OptoSlangExpr *make_binary_expr(ModuleLoweringContext &design,
                                OptoSlangBinaryOp op, const OptoSlangExpr *left,
                                const OptoSlangExpr *right,
                                const Expression &source) {
  OptoSlangExpr lowered;
  lowered.kind = OPTO_SLANG_EXPR_BINARY;
  lowered.binary_op = op;
  lowered.binary_left = left;
  lowered.binary_right = right;
  return make_expr(design, std::move(lowered), source);
}

OptoSlangExpr *make_unsigned_constant_expr(ModuleLoweringContext &design,
                                           uint32_t value, uint32_t width,
                                           const Expression &source) {
  OptoSlangExpr lowered;
  lowered.kind = OPTO_SLANG_EXPR_CONSTANT;
  lowered.constant_has_width = true;
  lowered.constant_width = width;
  lowered.constant_bits.resize(width);
  for (uint32_t bit = 0; bit < width; ++bit) {
    lowered.constant_bits[width - bit - 1] =
        bit < 32 && ((value >> bit) & 1u) ? '1' : '0';
  }
  auto *result = make_expr(design, std::move(lowered), source);
  // Synthetic constants have an explicit semantic type independent of the
  // source node used only for attribution. `make_expr` normally inherits a
  // source constant's signedness, so restore the helper's contract here.
  result->constant_signed = false;
  return result;
}

OptoSlangExpr *make_signed_constant_expr(ModuleLoweringContext &design,
                                         int64_t value, uint32_t width,
                                         const Expression &source) {
  OptoSlangExpr lowered;
  lowered.kind = OPTO_SLANG_EXPR_CONSTANT;
  lowered.constant_has_width = true;
  lowered.constant_width = width;
  lowered.constant_signed = true;
  lowered.constant_bits.resize(width);
  const auto bits = static_cast<uint64_t>(value);
  for (uint32_t bit = 0; bit < width; ++bit) {
    const bool one = bit < 64 ? ((bits >> bit) & 1u) != 0 : value < 0;
    lowered.constant_bits[width - bit - 1] = one ? '1' : '0';
  }
  auto *result = make_expr(design, std::move(lowered), source);
  result->constant_signed = true;
  return result;
}

OptoSlangExpr *make_unsigned_cast_expr(ModuleLoweringContext &design,
                                       const OptoSlangExpr *value,
                                       uint32_t width,
                                       const Expression &source) {
  OptoSlangExpr lowered;
  lowered.kind = OPTO_SLANG_EXPR_CAST;
  lowered.cast_kind = OPTO_SLANG_CAST_ZERO_EXTEND;
  lowered.cast_value = value;
  lowered.cast_width = width;
  lowered.cast_signed = false;
  return make_expr(design, std::move(lowered), source);
}

OptoSlangExpr *make_signed_cast_expr(ModuleLoweringContext &design,
                                     const OptoSlangExpr *value, uint32_t width,
                                     const Expression &source) {
  OptoSlangExpr lowered;
  lowered.kind = OPTO_SLANG_EXPR_CAST;
  lowered.cast_kind = OPTO_SLANG_CAST_SIGN_EXTEND;
  lowered.cast_value = value;
  lowered.cast_width = width;
  lowered.cast_signed = true;
  return make_expr(design, std::move(lowered), source);
}

ValueShape lowered_value_shape(const ModuleLoweringContext &design,
                               const OptoSlangExpr &value) {
  switch (value.kind) {
  case OPTO_SLANG_EXPR_SIGNAL: {
    if (value.signal_has_range) {
      return ValueShape{
          value.signal_msb - value.signal_lsb + 1,
          false,
      };
    }
    if (!value.signal_name) {
      throw std::runtime_error("signal value has no name");
    }
    auto found = design.value_shapes.find(*value.signal_name);
    if (found == design.value_shapes.end()) {
      throw std::runtime_error("signal value '" + *value.signal_name +
                               "' has no storage type");
    }
    return found->second;
  }
  case OPTO_SLANG_EXPR_CONSTANT:
    if (!value.constant_has_width) {
      throw std::runtime_error("constant value has no explicit width");
    }
    return ValueShape{
        value.constant_width,
        value.constant_signed,
    };
  case OPTO_SLANG_EXPR_UNARY: {
    if (!value.unary_arg) {
      throw std::runtime_error("unary value has no operand");
    }
    if (value.unary_op == OPTO_SLANG_UNARY_BIT_NOT) {
      return lowered_value_shape(design, *value.unary_arg);
    }
    return ValueShape{1, false};
  }
  case OPTO_SLANG_EXPR_BINARY: {
    if (!value.binary_left || !value.binary_right) {
      throw std::runtime_error("binary value has an empty operand");
    }
    const auto left = lowered_value_shape(design, *value.binary_left);
    const auto right = lowered_value_shape(design, *value.binary_right);
    switch (value.binary_op) {
    case OPTO_SLANG_BINARY_LOGICAL_AND:
    case OPTO_SLANG_BINARY_LOGICAL_OR:
    case OPTO_SLANG_BINARY_EQ:
    case OPTO_SLANG_BINARY_NE:
    case OPTO_SLANG_BINARY_LT:
    case OPTO_SLANG_BINARY_LE:
    case OPTO_SLANG_BINARY_GT:
    case OPTO_SLANG_BINARY_GE:
      return ValueShape{1, false};
    case OPTO_SLANG_BINARY_SHL:
    case OPTO_SLANG_BINARY_SHR:
    case OPTO_SLANG_BINARY_ASHR:
      return left;
    default:
      return ValueShape{
          std::max(left.width, right.width),
          left.is_signed && right.is_signed,
      };
    }
  }
  case OPTO_SLANG_EXPR_MUX:
    if (!value.mux_then || !value.mux_else) {
      throw std::runtime_error("mux value has an empty branch");
    }
    if (const auto then_shape = lowered_value_shape(design, *value.mux_then);
        then_shape == lowered_value_shape(design, *value.mux_else)) {
      return then_shape;
    }
    throw std::runtime_error(
        "mux value branches have inconsistent bridge types");
  case OPTO_SLANG_EXPR_CONCAT: {
    uint64_t width = 0;
    for (auto *part : value.concat_parts) {
      if (!part) {
        throw std::runtime_error("concatenation value has an empty operand");
      }
      width += lowered_value_shape(design, *part).width;
      if (width > UINT32_MAX) {
        throw std::runtime_error(
            "concatenation value width exceeds 32-bit capacity");
      }
    }
    return ValueShape{static_cast<uint32_t>(width), false};
  }
  case OPTO_SLANG_EXPR_CAST:
    return ValueShape{value.cast_width, value.cast_signed};
  case OPTO_SLANG_EXPR_EXTRACT:
    if (!value.extract_value) {
      throw std::runtime_error("extract value has no operand");
    }
    return ValueShape{
        value.extract_width,
        lowered_value_shape(design, *value.extract_value).is_signed,
    };
  case OPTO_SLANG_EXPR_DYNAMIC_EXTRACT:
    if (!value.dynamic_extract_value) {
      throw std::runtime_error("dynamic extract value has no operand");
    }
    return ValueShape{
        value.dynamic_extract_width,
        lowered_value_shape(design, *value.dynamic_extract_value).is_signed,
    };
  }
  throw std::runtime_error(
      "unknown bridge expression kind while deriving its type");
}

OptoSlangExpr *cast_to_shape(ModuleLoweringContext &design,
                             OptoSlangExpr *value, uint32_t target_width,
                             bool target_signed, const Expression &source,
                             bool force) {
  const auto source_shape = lowered_value_shape(design, *value);
  const auto source_width = source_shape.width;
  if (!force && source_width == target_width &&
      source_shape.is_signed == target_signed) {
    return value;
  }
  OptoSlangExpr lowered;
  lowered.kind = OPTO_SLANG_EXPR_CAST;
  lowered.cast_value = value;
  lowered.cast_width = target_width;
  lowered.cast_signed = target_signed;
  if (target_width < source_width) {
    lowered.cast_kind = OPTO_SLANG_CAST_TRUNCATE;
  } else if (target_width > source_width && source_shape.is_signed) {
    lowered.cast_kind = OPTO_SLANG_CAST_SIGN_EXTEND;
  } else {
    lowered.cast_kind = OPTO_SLANG_CAST_ZERO_EXTEND;
  }
  return make_expr(design, std::move(lowered), source);
}

OptoSlangExpr *cast_to_type(ModuleLoweringContext &design, OptoSlangExpr *value,
                            const Type &result_type, const Expression &source,
                            bool force) {
  return cast_to_shape(
      design, value,
      checked_width(lowered_type_width(result_type), "conversion result"),
      result_type.isSigned(), source, force);
}

ValueShape lvalue_shape(const ModuleLoweringContext &design,
                        const Expression &expression) {
  switch (expression.kind) {
  case ExpressionKind::NamedValue: {
    const auto &symbol = expression.as<NamedValueExpression>().symbol;
    if (auto *alias = find_function_lvalue(design, symbol)) {
      return lowered_value_shape(design, *alias);
    }
    const auto name = registered_value_name(design, symbol);
    auto found = design.value_shapes.find(name);
    if (found == design.value_shapes.end()) {
      throw std::runtime_error("registered assignment target '" + name +
                               "' has no storage type");
    }
    return found->second;
  }
  case ExpressionKind::HierarchicalValue: {
    const auto &symbol = expression.as<HierarchicalValueExpression>().symbol;
    const auto name = registered_value_name(design, symbol);
    auto found = design.value_shapes.find(name);
    if (found == design.value_shapes.end()) {
      throw std::runtime_error("registered assignment target '" + name +
                               "' has no storage type");
    }
    return found->second;
  }
  case ExpressionKind::MemberAccess: {
    const auto &field =
        expression.as<MemberAccessExpression>().member.as<FieldSymbol>();
    return ValueShape{
        checked_width(lowered_type_width(field.getType()), field.name),
        field.getType().isSigned(),
    };
  }
  case ExpressionKind::ElementSelect:
  case ExpressionKind::RangeSelect:
  case ExpressionKind::Concatenation:
    return ValueShape{
        checked_width(lowered_type_width(*expression.type),
                      "assignment selection"),
        false,
    };
  default:
    throw std::runtime_error("unsupported assignment target kind '" +
                             copy_string(toString(expression.kind)) + "'");
  }
}

OptoSlangExpr *cast_to_lvalue_type(ModuleLoweringContext &design,
                                   OptoSlangExpr *value,
                                   const Expression &lvalue) {
  const auto target = lvalue_shape(design, lvalue);
  return cast_to_shape(design, value, target.width, target.is_signed, lvalue);
}

OptoSlangExpr *cast_to_expression_type(ModuleLoweringContext &design,
                                       OptoSlangExpr *value,
                                       const Expression &result, bool force) {
  return cast_to_type(design, value, *result.type, result, force);
}

OptoSlangExpr *make_high_impedance_expr(ModuleLoweringContext &design,
                                        const Expression &source) {
  OptoSlangExpr lowered;
  lowered.kind = OPTO_SLANG_EXPR_CONSTANT;
  lowered.constant_has_width = true;
  lowered.constant_width = 1;
  lowered.constant_bits = "z";
  return make_expr(design, std::move(lowered), source);
}

OptoSlangExpr *make_mux_expr(ModuleLoweringContext &design,
                             const OptoSlangExpr *condition,
                             const OptoSlangExpr *then_value,
                             const OptoSlangExpr *else_value,
                             const Expression &source) {
  OptoSlangExpr lowered;
  lowered.kind = OPTO_SLANG_EXPR_MUX;
  lowered.mux_condition = condition;
  lowered.mux_then = then_value;
  lowered.mux_else = else_value;
  return make_expr(design, std::move(lowered), source);
}

OptoSlangExpr *lower_boolean_context(ModuleLoweringContext &design,
                                     const Expression &expression) {
  if (expression.kind == ExpressionKind::Invalid) {
    const auto *child = expression.as<InvalidExpression>().child;
    if (child && child->type->isIntegral()) {
      auto *value = lower_expr(design, *child);
      if (lowered_type_width(*child->type) == 1) {
        return value;
      }
      return make_unary_expr(design, OPTO_SLANG_UNARY_REDUCTION_OR, value,
                             *child);
    }
  }
  if (!expression.type->isIntegral()) {
    const Expression *diagnostic = &expression;
    while (diagnostic->kind == ExpressionKind::Invalid) {
      const auto *child = diagnostic->as<InvalidExpression>().child;
      if (!child) {
        break;
      }
      diagnostic = child;
    }
    throw std::runtime_error(
        "non-integral boolean context is not supported for synthesis (" +
        copy_string(toString(diagnostic->kind)) + ", type " +
        copy_string(toString(diagnostic->type->getCanonicalType().kind)) +
        ") at " + expression_location(design, *diagnostic));
  }
  auto *value = lower_expr(design, expression);
  if (lowered_type_width(*expression.type) == 1) {
    return value;
  }
  return make_unary_expr(design, OPTO_SLANG_UNARY_REDUCTION_OR, value,
                         expression);
}

bool is_empty_connection_expression(const Expression &expr) {
  return expr.kind == ExpressionKind::EmptyArgument;
}

const Expression &call_output_lvalue(const Expression &actual) {
  if (actual.kind != ExpressionKind::Assignment) {
    return actual;
  }
  const auto &assignment = actual.as<AssignmentExpression>();
  if (assignment.right().kind != ExpressionKind::EmptyArgument) {
    throw std::runtime_error(
        "output argument conversion does not contain an lvalue");
  }
  return assignment.left();
}
} // namespace opto::slang_lower
