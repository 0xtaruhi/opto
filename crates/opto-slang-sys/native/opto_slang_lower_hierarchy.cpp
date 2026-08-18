// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

#include "opto_slang_lower_internal.h"

namespace opto::slang_lower {
constexpr size_t UDP_TABLE_ROW_LIMIT = 65536;
constexpr size_t UDP_INPUT_LIMIT = 1024;

struct UdpBinaryPredicate {
    const OptoSlangExpr* expression = nullptr;
    bool reachable = true;
};

struct UdpInputField {
    std::string_view symbols;
    bool is_edge = false;
};

struct UdpBinaryEdges {
    bool pos = false;
    bool neg = false;
};

struct UdpEdgePredicate {
    UdpBinaryPredicate predicate;
    size_t event_port = 0;
    UdpBinaryEdges edges;
};

struct UdpRowEdge {
    size_t event_port = 0;
    UdpBinaryEdges edges;
};

struct UdpAsyncControl {
    bool reachable = true;
    size_t event_port = 0;
    bool active_high = false;
};

struct ExternalPortPart {
    const PortSymbol* port = nullptr;
    uint32_t width = 0;
};

struct ExternalPortProjection {
    std::string name;
    ArgumentDirection direction = ArgumentDirection::In;
    uint32_t width = 0;
    bool is_signed = false;
    SourceLocation location;
    std::vector<ExternalPortPart> parts;
};

[[noreturn]] void reject_external_port_projection(
    SourceLocation location, std::string message) {
    throw LoweringFailure(
        OPTO_SLANG_LOWERING_INVALID_PROJECTION, 1, location, std::move(message));
}

const ValueSymbol& external_port_internal_value(
    const PortSymbol& port, SourceLocation location, std::string_view external_name) {
    if (!port.internalSymbol || !ValueSymbol::isKind(port.internalSymbol->kind)) {
        reject_external_port_projection(
            location,
            "external port '" + std::string(external_name) +
                "' does not map to an integral internal value");
    }
    return port.internalSymbol->as<ValueSymbol>();
}

OptoSlangExpr* make_external_port_slice(
    ModuleLoweringContext& design,
    const std::string& name,
    uint32_t total_width,
    uint32_t lsb,
    uint32_t width) {
    auto* value = make_signal_expr(design, name);
    if (lsb == 0 && width == total_width) {
        return value;
    }
    OptoSlangExpr slice;
    slice.kind = OPTO_SLANG_EXPR_EXTRACT;
    slice.extract_value = value;
    slice.extract_lsb = lsb;
    slice.extract_width = width;
    design.module.exprs.push_back(std::move(slice));
    return &design.module.exprs.back();
}

OptoSlangExpr* lower_external_port_part(
    ModuleLoweringContext& design, const ExternalPortPart& part, bool lvalue) {
    if (!part.port) {
        throw std::logic_error("external port projection contains a null component");
    }
    if (auto* expression = part.port->getInternalExpr()) {
        return lvalue ? lower_signal_expr(design, *expression) : lower_expr(design, *expression);
    }
    const auto& value = external_port_internal_value(
        *part.port, part.port->externalLoc, part.port->name);
    return make_signal_expr(design, registered_value_name(design, value));
}

void validate_external_port_parts(
    const ExternalPortProjection& projection,
    const std::vector<OptoSlangExpr*>& parts) {
    std::unordered_map<std::string_view, std::vector<std::pair<uint32_t, uint32_t>>> occupied;
    for (size_t index = 0; index < parts.size(); ++index) {
        const auto* part = parts[index];
        if (!part || part->kind != OPTO_SLANG_EXPR_SIGNAL || !part->signal_name) {
            reject_external_port_projection(
                projection.location,
                "external port '" + projection.name +
                    "' contains a non-static or non-signal projection");
        }
        const auto lsb = part->signal_has_range
                             ? std::min(part->signal_msb, part->signal_lsb)
                             : 0;
        const auto msb = part->signal_has_range
                             ? std::max(part->signal_msb, part->signal_lsb)
                             : projection.parts[index].width - 1;
        auto& ranges = occupied[*part->signal_name];
        if (std::ranges::any_of(ranges, [lsb, msb](const auto& range) {
                return lsb <= range.second && range.first <= msb;
            })) {
            reject_external_port_projection(
                projection.location,
                "external port '" + projection.name +
                    "' has overlapping internal bit mappings");
        }
        ranges.emplace_back(lsb, msb);
    }
}

void lower_external_port_projections(
    ModuleLoweringContext& design,
    const std::vector<ExternalPortProjection>& projections) {
    for (const auto& projection : projections) {
        uint64_t total_width = 0;
        for (const auto& part : projection.parts) {
            total_width += part.width;
            if (total_width > UINT32_MAX) {
                reject_external_port_projection(
                    projection.location,
                    "external port '" + projection.name + "' width exceeds 32-bit capacity");
            }
        }
        if (projection.parts.empty() || total_width != projection.width) {
            reject_external_port_projection(
                projection.location,
                "external port '" + projection.name +
                    "' has an internal projection width that does not match its declared width");
        }

        std::vector<OptoSlangExpr*> lowered_parts;
        lowered_parts.reserve(projection.parts.size());
        for (const auto& part : projection.parts) {
            lowered_parts.push_back(lower_external_port_part(
                design, part, projection.direction == ArgumentDirection::In));
        }
        validate_external_port_parts(projection, lowered_parts);

        if (projection.direction == ArgumentDirection::In) {
            uint64_t consumed = 0;
            for (size_t index = 0; index < projection.parts.size(); ++index) {
                const auto width = projection.parts[index].width;
                consumed += width;
                design.module.assigns.push_back(
                    OptoSlangAssignData{
                        lowered_parts[index],
                        make_external_port_slice(
                            design,
                            projection.name,
                            projection.width,
                            static_cast<uint32_t>(projection.width - consumed),
                            width),
                    });
            }
            continue;
        }
        if (projection.direction != ArgumentDirection::Out) {
            reject_external_port_projection(
                projection.location,
                "external port '" + projection.name +
                    "' requires an exact whole-signal inout or ref mapping");
        }

        const OptoSlangExpr* value = lowered_parts.front();
        if (lowered_parts.size() > 1) {
            OptoSlangExpr concat;
            concat.kind = OPTO_SLANG_EXPR_CONCAT;
            concat.concat_parts.assign(lowered_parts.begin(), lowered_parts.end());
            design.module.exprs.push_back(std::move(concat));
            value = &design.module.exprs.back();
        }
        design.module.assigns.push_back(
            OptoSlangAssignData{make_signal_expr(design, projection.name), value});
    }
}

bool is_udp_edge_symbol(char symbol) {
    return symbol == '*' || symbol == 'r' || symbol == 'f' || symbol == 'p' || symbol == 'n';
}

std::vector<UdpInputField> parse_udp_input_fields(
    const PrimitiveInstanceSymbol& instance, const PrimitiveSymbol::TableEntry& row) {
    std::vector<UdpInputField> fields;
    for (size_t offset = 0; offset < row.inputs.size();) {
        const auto symbol = row.inputs[offset];
        if (symbol != '(') {
            fields.push_back({row.inputs.substr(offset, 1), is_udp_edge_symbol(symbol)});
            ++offset;
            continue;
        }
        if (offset + 3 >= row.inputs.size() || row.inputs[offset + 3] != ')') {
            throw std::runtime_error(
                "UDP '" + copy_string(instance.primitiveType.name) +
                "' has malformed normalized transition field");
        }
        fields.push_back({row.inputs.substr(offset + 1, 2), true});
        offset += 4;
    }
    return fields;
}

bool udp_level_symbol_matches_binary(char symbol, bool value) {
    switch (symbol) {
    case '0':
        return !value;
    case '1':
        return value;
    case '?':
    case 'b':
        return true;
    case 'x':
        return false;
    default:
        return false;
    }
}

UdpBinaryEdges lower_udp_binary_edges(
    const PrimitiveInstanceSymbol& instance, const UdpInputField& field) {
    if (field.symbols.size() == 1) {
        switch (field.symbols[0]) {
        case 'r':
        case 'p':
            return {true, false};
        case 'f':
        case 'n':
            return {false, true};
        case '*':
            return {true, true};
        default:
            throw std::runtime_error(
                "UDP '" + copy_string(instance.primitiveType.name) +
                "' has unsupported transition symbol '" + copy_string(field.symbols) + "'");
        }
    }
    if (field.symbols.size() != 2) {
        throw std::runtime_error(
            "UDP '" + copy_string(instance.primitiveType.name) +
            "' has malformed normalized transition field");
    }
    return {
        udp_level_symbol_matches_binary(field.symbols[0], false) &&
            udp_level_symbol_matches_binary(field.symbols[1], true),
        udp_level_symbol_matches_binary(field.symbols[0], true) &&
            udp_level_symbol_matches_binary(field.symbols[1], false),
    };
}

UdpRowEdge inspect_udp_row_edge(
    const PrimitiveInstanceSymbol& instance,
    const PrimitiveSymbol::TableEntry& row,
    size_t input_count) {
    UdpRowEdge result;
    bool saw_edge = false;
    const auto fields = parse_udp_input_fields(instance, row);
    if (fields.size() != input_count) {
        throw std::runtime_error(
            "UDP '" + copy_string(instance.primitiveType.name) +
            "' normalized table row has the wrong input count");
    }
    for (size_t index = 0; index < fields.size(); ++index) {
        if (!fields[index].is_edge) {
            continue;
        }
        if (saw_edge) {
            throw std::runtime_error(
                "UDP '" + copy_string(instance.primitiveType.name) +
                "' table row has multiple transition inputs");
        }
        saw_edge = true;
        result.event_port = index + 1;
        result.edges = lower_udp_binary_edges(instance, fields[index]);
    }
    if (!saw_edge) {
        throw std::runtime_error(
            "edge-sensitive UDP '" + copy_string(instance.primitiveType.name) +
            "' has a level-sensitive update row");
    }
    return result;
}

UdpAsyncControl inspect_udp_async_control(
    const PrimitiveInstanceSymbol& instance,
    const PrimitiveSymbol::TableEntry& row,
    size_t input_count) {
    UdpAsyncControl result;
    if (row.state == 'x') {
        result.reachable = false;
        return result;
    }
    if (row.state != '?' && row.state != 'b') {
        throw std::runtime_error(
            "edge-sensitive UDP '" + copy_string(instance.primitiveType.name) +
            "' level-sensitive update depends on current state");
    }
    const auto fields = parse_udp_input_fields(instance, row);
    if (fields.size() != input_count) {
        throw std::runtime_error(
            "UDP '" + copy_string(instance.primitiveType.name) +
            "' normalized table row has the wrong input count");
    }
    size_t constrained_inputs = 0;
    for (size_t index = 0; index < fields.size(); ++index) {
        const auto& field = fields[index];
        if (field.is_edge || field.symbols.size() != 1) {
            throw std::runtime_error(
                "edge-sensitive UDP '" + copy_string(instance.primitiveType.name) +
                "' async-control row contains a transition field");
        }
        switch (field.symbols[0]) {
        case 'x':
            result.reachable = false;
            return result;
        case '?':
        case 'b':
            break;
        case '0':
        case '1':
            ++constrained_inputs;
            result.event_port = index + 1;
            result.active_high = field.symbols[0] == '1';
            break;
        default:
            throw std::runtime_error(
                "edge-sensitive UDP '" + copy_string(instance.primitiveType.name) +
                "' has an unsupported async-control level symbol");
        }
    }
    if (constrained_inputs != 1) {
        throw std::runtime_error(
            "edge-sensitive UDP '" + copy_string(instance.primitiveType.name) +
            "' level-sensitive update is not one asynchronous control");
    }
    return result;
}

void append_udp_binary_match(
    ModuleLoweringContext& design,
    UdpBinaryPredicate& predicate,
    char symbol,
    const OptoSlangExpr* value,
    const Expression& source,
    std::string_view primitive) {
    const OptoSlangExpr* term = nullptr;
    switch (symbol) {
    case '0':
        term = make_unary_expr(design, OPTO_SLANG_UNARY_BIT_NOT, value, source);
        break;
    case '1':
        term = value;
        break;
    case '?':
    case 'b':
        return;
    case 'x':
        predicate.reachable = false;
        return;
    default:
        throw std::runtime_error(
            "UDP '" + copy_string(primitive) + "' has unsupported level symbol '" +
            std::string(1, symbol) + "'");
    }
    predicate.expression =
        predicate.expression
            ? make_binary_expr(
                  design,
                  OPTO_SLANG_BINARY_BIT_AND,
                  predicate.expression,
                  term,
                  source)
            : term;
}

UdpBinaryPredicate lower_udp_level_predicate(
    ModuleLoweringContext& design,
    const PrimitiveInstanceSymbol& instance,
    std::span<const Expression* const> ports,
    const PrimitiveSymbol::TableEntry& row,
    const Expression* current_state) {
    UdpBinaryPredicate predicate;
    size_t input_index = 1;
    for (const auto& field : parse_udp_input_fields(instance, row)) {
        if (input_index >= ports.size()) {
            throw std::runtime_error(
                "UDP '" + copy_string(instance.primitiveType.name) +
                "' table row has too many inputs");
        }
        if (field.is_edge || field.symbols.size() != 1) {
            throw std::runtime_error(
                "UDP '" + copy_string(instance.primitiveType.name) +
                "' edge row reached level-sensitive lowering");
        }
        const auto& input = require_primitive_port(instance, ports, input_index++);
        append_udp_binary_match(
            design,
            predicate,
            field.symbols[0],
            lower_expr(design, input),
            input,
            instance.primitiveType.name);
    }
    if (input_index != ports.size()) {
        throw std::runtime_error(
            "UDP '" + copy_string(instance.primitiveType.name) +
            "' table row has too few inputs");
    }
    if (current_state) {
        if (!row.state) {
            throw std::runtime_error(
                "sequential UDP '" + copy_string(instance.primitiveType.name) +
                "' table row has no current state");
        }
        append_udp_binary_match(
            design,
            predicate,
            row.state,
            lower_expr(design, *current_state),
            *current_state,
            instance.primitiveType.name);
    } else if (row.state) {
        throw std::runtime_error(
            "combinational UDP '" + copy_string(instance.primitiveType.name) +
            "' table row has a current state");
    }
    return predicate;
}

UdpEdgePredicate lower_udp_edge_predicate(
    ModuleLoweringContext& design,
    const PrimitiveInstanceSymbol& instance,
    std::span<const Expression* const> ports,
    const PrimitiveSymbol::TableEntry& row,
    const Expression& current_state,
    std::optional<size_t> implicit_event_port) {
    UdpEdgePredicate lowered;
    const auto inspected = inspect_udp_row_edge(instance, row, ports.size() - 1);
    lowered.event_port = inspected.event_port;
    lowered.edges = inspected.edges;
    size_t input_index = 1;
    for (const auto& field : parse_udp_input_fields(instance, row)) {
        if (input_index >= ports.size()) {
            throw std::runtime_error(
                "UDP '" + copy_string(instance.primitiveType.name) +
                "' table row has too many inputs");
        }
        const auto& input = require_primitive_port(instance, ports, input_index);
        auto* value = lower_expr(design, input);
        if (!field.is_edge) {
            if (field.symbols.size() != 1) {
                throw std::runtime_error(
                    "UDP '" + copy_string(instance.primitiveType.name) +
                    "' has malformed normalized level field");
            }
            append_udp_binary_match(
                design,
                lowered.predicate,
                field.symbols[0],
                value,
                input,
                instance.primitiveType.name);
        } else {
            if (!lowered.edges.pos && !lowered.edges.neg) {
                lowered.predicate.reachable = false;
            } else if (lowered.edges.pos != lowered.edges.neg &&
                       (!implicit_event_port || lowered.event_port != *implicit_event_port)) {
                append_udp_binary_match(
                    design,
                    lowered.predicate,
                    lowered.edges.pos ? '1' : '0',
                    value,
                    input,
                    instance.primitiveType.name);
            }
        }
        ++input_index;
    }
    if (input_index != ports.size()) {
        throw std::runtime_error(
            "UDP '" + copy_string(instance.primitiveType.name) +
            "' table row has too few inputs");
    }
    if (!row.state) {
        throw std::runtime_error(
            "sequential UDP '" + copy_string(instance.primitiveType.name) +
            "' table row has no current state");
    }
    append_udp_binary_match(
        design,
        lowered.predicate,
        row.state,
        lower_expr(design, current_state),
        current_state,
        instance.primitiveType.name);
    return lowered;
}

OptoSlangExpr* make_udp_logic_value(
    ModuleLoweringContext& design, char value, const Expression& source) {
    if (value == '0' || value == '1') {
        return make_unsigned_constant_expr(design, value == '1' ? 1 : 0, 1, source);
    }
    if (value != 'x') {
        throw std::runtime_error(
            "UDP table has unsupported next-state symbol '" + std::string(1, value) + "'");
    }
    OptoSlangExpr unknown;
    unknown.kind = OPTO_SLANG_EXPR_CONSTANT;
    unknown.constant_has_width = true;
    unknown.constant_width = 1;
    unknown.constant_bits = "x";
    return make_expr(design, std::move(unknown), source);
}


OptoSlangAttributeData lower_attribute(
    ModuleLoweringContext& design,
    const AttributeSymbol& attribute) {
    const auto& value = attribute.getValue();
    OptoSlangAttributeData result;
    result.name = copy_string(attribute.name);
    result.is_true = value.isTrue();
    result.source = source_span(design, attribute.location);
    const auto* syntax = attribute.getSyntax();
    const auto* attribute_syntax =
        syntax && syntax->kind == SyntaxKind::AttributeSpec
            ? &syntax->as<AttributeSpecSyntax>()
            : nullptr;
    const auto* expression_syntax =
        attribute_syntax && attribute_syntax->value
            ? attribute_syntax->value->expr.get()
            : nullptr;
    if (expression_syntax && expression_syntax->kind == SyntaxKind::StringLiteralExpression) {
        result.kind = OPTO_SLANG_ATTRIBUTE_STRING;
        result.value = expression_syntax->as<LiteralExpressionSyntax>().literal.valueText();
    } else if (value.isInteger()) {
        const auto& integer = value.integer();
        result.kind = OPTO_SLANG_ATTRIBUTE_INTEGER;
        result.value = exact_binary_string(integer);
        result.integer_width = checked_width(integer.getBitWidth(), attribute.name);
        result.integer_signed = integer.isSigned();
    } else if (value.isString()) {
        result.kind = OPTO_SLANG_ATTRIBUTE_STRING;
        result.value = value.str();
    } else {
        result.kind = OPTO_SLANG_ATTRIBUTE_OTHER;
        result.value = value.toString(std::numeric_limits<bitwidth_t>::max(), true);
    }
    return result;
}

void append_symbol_attributes(
    ModuleLoweringContext& design,
    const Symbol& symbol,
    std::unordered_set<const AttributeSymbol*>& seen,
    std::vector<OptoSlangAttributeData>& lowered) {
    for (auto* attribute : design.body.getCompilation().getAttributes(symbol)) {
        if (attribute && seen.insert(attribute).second) {
            lowered.push_back(lower_attribute(design, *attribute));
        }
    }
}

std::vector<OptoSlangAttributeData> lower_symbol_attributes(
    ModuleLoweringContext& design,
    const Symbol& symbol) {
    std::vector<OptoSlangAttributeData> lowered;
    std::unordered_set<const AttributeSymbol*> seen;
    append_symbol_attributes(design, symbol, seen, lowered);
    return lowered;
}

std::vector<OptoSlangAttributeData> lower_port_attributes(
    ModuleLoweringContext& design,
    const PortSymbol& port) {
    std::vector<OptoSlangAttributeData> lowered;
    std::unordered_set<const AttributeSymbol*> seen;
    append_symbol_attributes(design, port, seen, lowered);
    if (port.internalSymbol) {
        append_symbol_attributes(design, *port.internalSymbol, seen, lowered);
    }
    return lowered;
}

OptoSlangNetResolution net_resolution(const NetSymbol& net) {
    switch (net.netType.netKind) {
        case NetType::WAnd:
            return OPTO_SLANG_NET_WIRED_AND;
        case NetType::WOr:
            return OPTO_SLANG_NET_WIRED_OR;
        default:
            return OPTO_SLANG_NET_SINGLE_DRIVER;
    }
}

OptoSlangNetResolution external_port_resolution(const PortSymbol& port) {
    return port.internalSymbol && port.internalSymbol->kind == SymbolKind::Net
               ? net_resolution(port.internalSymbol->as<NetSymbol>())
               : OPTO_SLANG_NET_SINGLE_DRIVER;
}

std::vector<OptoSlangAttributeData> lower_multi_port_attributes(
    ModuleLoweringContext& design, const MultiPortSymbol& port) {
    std::vector<OptoSlangAttributeData> lowered;
    std::unordered_set<const AttributeSymbol*> seen;
    append_symbol_attributes(design, port, seen, lowered);
    for (auto* part : port.ports) {
        if (!part) {
            continue;
        }
        append_symbol_attributes(design, *part, seen, lowered);
        if (part->internalSymbol) {
            append_symbol_attributes(design, *part->internalSymbol, seen, lowered);
        }
    }
    return lowered;
}

bool unpacked_element_is_signed(const Type& source_type) {
    const Type* type = &source_type.getCanonicalType();
    while (type->kind == SymbolKind::FixedSizeUnpackedArrayType) {
        type = &type->as<FixedSizeUnpackedArrayType>().elementType.getCanonicalType();
    }
    return type->isSigned();
}

void add_value_as_net(
    ModuleLoweringContext& design,
    OptoSlangModulePayload& module,
    std::unordered_set<std::string>& existing,
    const ValueSymbol& symbol,
    std::string name,
    bool suppress_port_backref = true) {
    if (name.empty() || existing.contains(name) ||
        (suppress_port_backref && is_port_backref(symbol))) {
        return;
    }
    existing.insert(name);
    OptoSlangNetData net{
        std::move(name),
        checked_width(lowered_type_width(symbol.getType()), symbol.name),
        symbol.getType().isSigned(),
        unpacked_element_is_signed(symbol.getType()),
        false,
        symbol.kind == SymbolKind::Net
            ? net_resolution(symbol.as<NetSymbol>())
            : OPTO_SLANG_NET_SINGLE_DRIVER,
        intern_type_layout(design, symbol.getType()),
        {},
    };
    net.attributes = lower_symbol_attributes(design, symbol);
    module.nets.push_back(std::move(net));
}

uint64_t source_order(const DefinitionSymbol& definition) {
    auto location = definition.location;
    if (!location.valid()) {
        return UINT64_MAX;
    }
    return (static_cast<uint64_t>(location.buffer().getId()) << 36) |
           static_cast<uint64_t>(location.offset());
}

const Expression& instance_connection_expression(const PortConnection& connection) {
    auto* expression = connection.getExpression();
    if (!expression) {
        throw std::runtime_error("instance connection has no expression");
    }
    if (connection.port.kind != SymbolKind::Port ||
        connection.port.as<PortSymbol>().direction == ArgumentDirection::In) {
        return *expression;
    }

    // Slang context-sizes an output actual to the formal port width by wrapping
    // the lvalue in conversion nodes. The conversion describes the value
    // flowing out of the child; it is not part of the parent-side lvalue. Keep
    // the original signal selection here and let hierarchy ungrouping apply the
    // Verilog width conversion to the child value before driving it.
    if (expression->kind == ExpressionKind::Assignment) {
        expression = &expression->as<AssignmentExpression>().left();
    }
    while (expression->kind == ExpressionKind::Conversion) {
        const auto& conversion = expression->as<ConversionExpression>();
        expression = &conversion.operand();
    }
    return *expression;
}

void lower_primitive_instance(
    ModuleLoweringContext& design,
    OptoSlangModulePayload& module,
    const PrimitiveInstanceSymbol& instance) {
    if (instance.getDelay()) {
        throw std::runtime_error(
            "delayed primitive instance '" + copy_string(instance.name) + "' is not synthesizable");
    }
    const auto [drive0, drive1] = instance.getDriveStrength();
    if (drive0 || drive1) {
        throw std::runtime_error(
            "drive strength on primitive instance '" + copy_string(instance.name) +
            "' is not supported for synthesis");
    }

    const auto primitive = copy_string(instance.primitiveType.name);
    const auto ports = instance.getPortConnections();
    auto require_count = [&](size_t minimum, std::optional<size_t> exact = std::nullopt) {
        if ((exact && ports.size() != *exact) || (!exact && ports.size() < minimum)) {
            throw std::runtime_error(
                "primitive '" + primitive + "' instance '" + copy_string(instance.name) + "' has " +
                std::to_string(ports.size()) + " terminals");
        }
    };
    auto append_assign = [&](size_t output_index, const OptoSlangExpr* rhs) {
        const auto& output = require_primitive_port(instance, ports, output_index);
        const Expression* lvalue = &output;
        if (output.kind == ExpressionKind::Assignment) {
            lvalue = &output.as<AssignmentExpression>().left();
        }
        module.assigns.push_back(
            OptoSlangAssignData{
                lower_signal_expr(design, *lvalue),
                rhs,
            });
    };

    if (instance.primitiveType.primitiveKind == PrimitiveSymbol::UserDefined) {
        const auto& udp = instance.primitiveType;
        require_count(2, udp.ports.size());
        if (udp.ports.size() - 1 > UDP_INPUT_LIMIT) {
            throw std::runtime_error(
                "UDP '" + primitive + "' exceeds the deterministic input limit of " +
                std::to_string(UDP_INPUT_LIMIT));
        }
        if (udp.table.size() > UDP_TABLE_ROW_LIMIT) {
            throw std::runtime_error(
                "UDP '" + primitive + "' exceeds the deterministic table-row limit of " +
                std::to_string(UDP_TABLE_ROW_LIMIT));
        }

        const auto& output = require_primitive_port(instance, ports, 0);
        const Expression* output_lvalue = &output;
        if (output.kind == ExpressionKind::Assignment) {
            output_lvalue = &output.as<AssignmentExpression>().left();
        }
        if (udp.isSequential) {
            if (udp.initVal) {
                throw std::runtime_error(
                    "initial state on sequential UDP '" + primitive +
                    "' is outside the explicit-reset synthesis profile");
            }
            if (udp.isEdgeSensitive) {
                std::vector<GuardedEffectData> updates;
                std::vector<OptoSlangEventData> events;
                std::map<size_t, uint8_t> event_port_outputs;
                std::vector<std::pair<size_t, OptoSlangEdge>> async_controls;
                updates.reserve(udp.table.size());
                for (const auto& row : udp.table) {
                    if (row.output == '-' || row.output == 'x') {
                        continue;
                    }
                    if (!row.isEdgeSensitive) {
                        const auto control =
                            inspect_udp_async_control(instance, row, ports.size() - 1);
                        if (control.reachable) {
                            async_controls.emplace_back(
                                control.event_port,
                                control.active_high ? OPTO_SLANG_EDGE_POS : OPTO_SLANG_EDGE_NEG);
                        }
                        continue;
                    }
                    const auto inspected =
                        inspect_udp_row_edge(instance, row, ports.size() - 1);
                    if (!inspected.edges.pos && !inspected.edges.neg) {
                        continue;
                    }
                    event_port_outputs[inspected.event_port] |=
                        row.output == '0' ? uint8_t{1} : uint8_t{2};
                }
                if (event_port_outputs.empty()) {
                    throw std::runtime_error(
                        "edge-sensitive UDP '" + primitive +
                        "' has no binary-reachable update row");
                }
                std::optional<size_t> primary_event_port;
                if (event_port_outputs.size() == 1) {
                    primary_event_port = event_port_outputs.begin()->first;
                } else {
                    for (const auto& [port, outputs] : event_port_outputs) {
                        if (outputs != 3) {
                            continue;
                        }
                        if (primary_event_port) {
                            throw std::runtime_error(
                                "edge-sensitive UDP '" + primitive + "' instance '" +
                                copy_string(instance.name) +
                                "' has multiple data-update transition inputs");
                        }
                        primary_event_port = port;
                    }
                    if (!primary_event_port) {
                        throw std::runtime_error(
                            "edge-sensitive UDP '" + primitive + "' instance '" +
                            copy_string(instance.name) +
                            "' has no unique data-update transition input");
                    }
                }
                std::vector<std::pair<size_t, OptoSlangEdge>> event_keys;
                auto append_event = [&](size_t port, OptoSlangEdge edge) {
                    if (std::ranges::find(event_keys, std::pair{port, edge}) != event_keys.end()) {
                        return;
                    }
                    event_keys.emplace_back(port, edge);
                    const auto& input = require_primitive_port(instance, ports, port);
                    events.push_back(
                        OptoSlangEventData{
                            edge,
                            lower_signal_expr(design, input),
                            nullptr,
                            source_span(design, input),
                        });
                };
                auto append_row_events = [&](size_t required_port) {
                    for (const auto& row : udp.table) {
                        if (!row.isEdgeSensitive || row.output == '-' || row.output == 'x') {
                            continue;
                        }
                        const auto inspected =
                            inspect_udp_row_edge(instance, row, ports.size() - 1);
                        if (inspected.event_port != required_port) {
                            continue;
                        }
                        if (inspected.edges.pos) {
                            append_event(required_port, OPTO_SLANG_EDGE_POS);
                        }
                        if (inspected.edges.neg) {
                            append_event(required_port, OPTO_SLANG_EDGE_NEG);
                        }
                    }
                };
                append_row_events(*primary_event_port);
                for (const auto& [port, _] : event_port_outputs) {
                    if (port != *primary_event_port) {
                        append_row_events(port);
                    }
                }
                for (const auto& [port, edge] : async_controls) {
                    append_event(port, edge);
                }
                const bool primary_has_pos = std::ranges::find(
                    event_keys,
                    std::pair{*primary_event_port, OPTO_SLANG_EDGE_POS}) != event_keys.end();
                const bool primary_has_neg = std::ranges::find(
                    event_keys,
                    std::pair{*primary_event_port, OPTO_SLANG_EDGE_NEG}) != event_keys.end();
                const auto implicit_event_port = primary_has_pos && primary_has_neg
                                                     ? std::optional<size_t>{}
                                                     : primary_event_port;
                for (const auto& row : udp.table) {
                    if (row.output == '-' || row.output == 'x') {
                        continue;
                    }
                    UdpBinaryPredicate predicate;
                    if (row.isEdgeSensitive) {
                        predicate =
                            lower_udp_edge_predicate(
                                design,
                                instance,
                                ports,
                                row,
                                *output_lvalue,
                                implicit_event_port)
                                .predicate;
                    } else {
                        const auto control =
                            inspect_udp_async_control(instance, row, ports.size() - 1);
                        if (!control.reachable) {
                            continue;
                        }
                        predicate = lower_udp_level_predicate(
                            design, instance, ports, row, output_lvalue);
                    }
                    if (!predicate.reachable) {
                        continue;
                    }
                    updates.push_back(
                        GuardedEffectData{
                            predicate.expression,
                            {
                                lower_signal_expr(design, *output_lvalue),
                                make_udp_logic_value(design, row.output, output),
                                false,
                                source_span(design, output),
                            },
                        });
                }
                if (updates.empty()) {
                    throw std::runtime_error(
                        "edge-sensitive UDP '" + primitive +
                        "' has no binary-reachable update row");
                }
                module.procedures.push_back(
                    make_guarded_procedure(
                        std::move(updates),
                        OPTO_SLANG_PROCEDURE_FLOP,
                        std::move(events),
                        source_span(design, output)));
                return;
            }
            std::vector<GuardedEffectData> updates;
            updates.reserve(udp.table.size());
            for (const auto& row : udp.table) {
                if (row.output == '-') {
                    continue;
                }
                auto predicate =
                    lower_udp_level_predicate(design, instance, ports, row, output_lvalue);
                if (!predicate.reachable) {
                    continue;
                }
                updates.push_back(
                    GuardedEffectData{
                        predicate.expression,
                        {
                            lower_signal_expr(design, *output_lvalue),
                            make_udp_logic_value(design, row.output, output),
                            true,
                            source_span(design, output),
                        },
                    });
            }
            if (updates.empty()) {
                throw std::runtime_error(
                    "sequential UDP '" + primitive + "' has no binary-reachable update row");
            }
            module.procedures.push_back(
                make_guarded_procedure(
                    std::move(updates),
                    OPTO_SLANG_PROCEDURE_COMB_OR_LATCH,
                    {},
                    source_span(design, output)));
            return;
        }

        auto* result = make_udp_logic_value(design, 'x', output);

        for (auto row = udp.table.rbegin(); row != udp.table.rend(); ++row) {
            if (row->output == 'x') {
                continue;
            }
            if (row->output != '0' && row->output != '1') {
                throw std::runtime_error(
                    "combinational UDP '" + primitive + "' has unsupported output symbol '" +
                    std::string(1, row->output) + "'");
            }

            auto predicate = lower_udp_level_predicate(design, instance, ports, *row, nullptr);
            if (!predicate.reachable) {
                continue;
            }
            auto* row_value = make_udp_logic_value(design, row->output, output);
            result = predicate.expression
                         ? make_mux_expr(design, predicate.expression, row_value, result, output)
                         : row_value;
        }
        append_assign(0, result);
        return;
    }

    if (primitive == "buf" || primitive == "not") {
        require_count(2);
        const auto& input = require_primitive_port(instance, ports, ports.size() - 1);
        auto* value = lower_expr(design, input);
        for (size_t output_index = 0; output_index + 1 < ports.size(); ++output_index) {
            auto* result = value;
            if (primitive == "not") {
                result = make_unary_expr(design, OPTO_SLANG_UNARY_BIT_NOT, value, input);
            }
            append_assign(output_index, result);
        }
        return;
    }

    if (primitive == "pullup" || primitive == "pulldown") {
        require_count(1, 1);
        const auto& output = require_primitive_port(instance, ports, 0);
        append_assign(
            0,
            make_unsigned_constant_expr(
                design,
                primitive == "pullup" ? 1 : 0,
                1,
                output));
        return;
    }

    if (primitive == "and" || primitive == "or" || primitive == "xor" || primitive == "nand" ||
        primitive == "nor" || primitive == "xnor") {
        require_count(3);
        auto op = OPTO_SLANG_BINARY_BIT_AND;
        if (primitive == "or" || primitive == "nor") {
            op = OPTO_SLANG_BINARY_BIT_OR;
        } else if (primitive == "xor" || primitive == "xnor") {
            op = OPTO_SLANG_BINARY_BIT_XOR;
        }
        auto* result = lower_expr(design, require_primitive_port(instance, ports, 1));
        for (size_t input_index = 2; input_index < ports.size(); ++input_index) {
            const auto& input = require_primitive_port(instance, ports, input_index);
            result = make_binary_expr(design, op, result, lower_expr(design, input), input);
        }
        if (primitive == "nand" || primitive == "nor" || primitive == "xnor") {
            result = make_unary_expr(
                design,
                OPTO_SLANG_UNARY_BIT_NOT,
                result,
                require_primitive_port(instance, ports, 0));
        }
        append_assign(0, result);
        return;
    }

    if (primitive == "bufif0" || primitive == "bufif1" || primitive == "notif0" ||
        primitive == "notif1") {
        require_count(3, 3);
        const auto& data_expr = require_primitive_port(instance, ports, 1);
        const auto& control_expr = require_primitive_port(instance, ports, 2);
        auto* data = lower_expr(design, data_expr);
        if (primitive == "notif0" || primitive == "notif1") {
            data = make_unary_expr(design, OPTO_SLANG_UNARY_BIT_NOT, data, data_expr);
        }
        auto* control = lower_expr(design, control_expr);
        auto* high_impedance = make_high_impedance_expr(design, control_expr);
        const bool active_high = primitive == "bufif1" || primitive == "notif1";
        auto* result = active_high
                           ? make_mux_expr(design, control, data, high_impedance, control_expr)
                           : make_mux_expr(design, control, high_impedance, data, control_expr);
        append_assign(0, result);
        return;
    }

    throw std::runtime_error(
        "primitive '" + primitive + "' instance '" + copy_string(instance.name) +
        "' is not supported for synthesis");
}

void validate_net_semantics(const NetSymbol& net) {
    const auto name = copy_string(net.name);
    if (net.netType.netKind != NetType::Wire && net.netType.netKind != NetType::Tri &&
        net.netType.netKind != NetType::UWire && net.netType.netKind != NetType::WAnd &&
        net.netType.netKind != NetType::WOr) {
        throw std::runtime_error(
            "net type '" + copy_string(net.netType.name) + "' on net '" + name +
            "' is not supported for synthesis");
    }
    if (net.getDelay()) {
        throw std::runtime_error("delay on net '" + name + "' is not supported for synthesis");
    }
    const auto [drive0, drive1] = net.getDriveStrength();
    if (drive0 || drive1) {
        throw std::runtime_error(
            "drive strength on net '" + name + "' is not supported for synthesis");
    }
    if (net.getChargeStrength()) {
        throw std::runtime_error(
            "charge strength on net '" + name + "' is not supported for synthesis");
    }
}

void validate_continuous_assign_semantics(const ContinuousAssignSymbol& assign) {
    if (assign.getDelay()) {
        throw std::runtime_error("delay on continuous assignment is not supported for synthesis");
    }
    const auto [drive0, drive1] = assign.getDriveStrength();
    if (drive0 || drive1) {
        throw std::runtime_error(
            "drive strength on continuous assignment is not "
            "supported for synthesis");
    }
}

std::string printed_type(const Type& type) {
    TypePrinter printer;
    printer.append(type);
    return printer.toString();
}

std::string specialization_signature(const InstanceBodySymbol& body) {
    std::string signature = copy_string(body.getDefinition().name);
    for (auto* parameter : body.getParameters()) {
        if (!parameter) {
            throw std::runtime_error("elaborated module contains a null parameter");
        }
        signature.push_back('|');
        signature += copy_string(parameter->symbol.name);
        signature.push_back('=');
        if (parameter->symbol.kind == SymbolKind::Parameter) {
            signature += parameter->symbol.as<ParameterSymbol>().getValue().toString(
                std::numeric_limits<bitwidth_t>::max(), true, true);
        } else if (parameter->symbol.kind == SymbolKind::TypeParameter) {
            signature +=
                printed_type(parameter->symbol.as<TypeParameterSymbol>().targetType.getType());
        } else {
            throw std::runtime_error("elaborated module contains an unsupported parameter kind");
        }
    }
    for (auto* port_symbol : body.getPortList()) {
        if (!port_symbol || port_symbol->kind != SymbolKind::Port) {
            continue;
        }
        const auto& port = port_symbol->as<PortSymbol>();
        signature += "|port:" + copy_string(port.name) + "=" + printed_type(port.getType());
    }
    return signature;
}

uint64_t stable_signature_hash(std::string_view signature) {
    uint64_t hash = 14695981039346656037ull;
    for (unsigned char byte : signature) {
        hash ^= byte;
        hash *= 1099511628211ull;
    }
    return hash;
}

std::string specialization_name(std::string_view definition, std::string_view signature) {
    std::ostringstream stream;
    stream << definition << "__P" << std::hex << std::setw(16) << std::setfill('0')
           << stable_signature_hash(signature);
    return stream.str();
}

void collect_instance_bodies(
    const InstanceBodySymbol& body,
    std::vector<const InstanceBodySymbol*>& bodies,
    std::unordered_set<const InstanceBodySymbol*>& seen) {
    if (!seen.insert(&body).second) {
        return;
    }
    bodies.push_back(&body);
    ModuleMembers members;
    collect_elaborated_members(body, body, members);
    for (auto* child : members.instances) {
        collect_instance_bodies(child->body, bodies, seen);
    }
}

template <typename Context>
std::string module_name_for_body(const Context& design, const InstanceBodySymbol& body) {
    auto found = design.body_names.find(&body);
    if (found == design.body_names.end()) {
        throw std::runtime_error("elaborated module body has no registered specialization name");
    }
    return found->second;
}

bool is_blackbox(const InstanceBodySymbol& body) {
    for (auto* attribute : body.getCompilation().getAttributes(body.getDefinition())) {
        if (attribute && attribute->name == "blackbox" && attribute->getValue().isTrue()) {
            return true;
        }
    }
    return false;
}

void lower_body(
    ModuleLoweringContext& design,
    const ModuleMembers& members,
    std::span<const Expression* const> bound_unresolved_connections) {
    auto& module = design.module;
    const auto& body = design.body;
    auto& known_value_names = design.net_names;
    const auto& definition = body.getDefinition();
    std::vector<ExternalPortProjection> external_port_projections;

    module.attributes = lower_symbol_attributes(design, definition);

    for (auto* port_symbol : body.getPortList()) {
        if (!port_symbol) {
            continue;
        }
        if (port_symbol->kind == SymbolKind::InterfacePort) {
            const auto& port = port_symbol->as<InterfacePortSymbol>();
            auto [connected, selected_modport] = port.getConnection();
            std::vector<const InstanceSymbol*> leaves;
            collect_interface_leaves(connected, leaves);
            if (leaves.empty()) {
                throw std::runtime_error(
                    "interface port '" + copy_string(port.name) + "' has no elaborated connection");
            }
            auto modport_name = port.modport;
            if (modport_name.empty() && selected_modport) {
                modport_name = selected_modport->name;
            }
            auto& scoped_names = design.interface_port_names[&body];
            for (size_t element = 0; element < leaves.size(); ++element) {
                for (auto signal : interface_signals(*leaves[element], modport_name)) {
                    if (modport_name.empty()) {
                        signal.direction = infer_interface_direction(body, *signal.value);
                    }
                    if (signal.direction == ArgumentDirection::Out &&
                        !interface_value_is_driven(body, *signal.reference)) {
                        continue;
                    }
                    auto name = flattened_interface_port_name(
                        port.name, element, leaves.size(), signal.name);
                    if (!known_value_names.insert(name).second) {
                        throw std::runtime_error(
                            "duplicate flattened interface port '" + name + "'");
                    }
                    if (signal.value) {
                        auto [found, inserted] = scoped_names.emplace(signal.value, name);
                        if (!inserted && found->second != name) {
                            throw std::runtime_error(
                                "interface signal maps to conflicting flattened ports");
                        }
                    }
                    auto [reference, reference_inserted] =
                        scoped_names.emplace(signal.reference, name);
                    if (!reference_inserted && reference->second != name) {
                        throw std::runtime_error(
                            "interface reference maps to conflicting flattened ports");
                    }
                    const auto& type = signal.reference->getType();
                    OptoSlangPortData lowered{
                        std::move(name),
                        lower_direction(signal.direction),
                        checked_width(lowered_type_width(type), signal.name),
                        type.isSigned(),
                        signal.value && signal.value->kind == SymbolKind::Net
                            ? net_resolution(signal.value->as<NetSymbol>())
                            : OPTO_SLANG_NET_SINGLE_DRIVER,
                        intern_type_layout(design, type),
                        {},
                    };
                    lowered.attributes = lower_symbol_attributes(design, port);
                    module.ports.push_back(std::move(lowered));
                }
            }
            continue;
        }
        if (port_symbol->kind == SymbolKind::MultiPort) {
            const auto& port = port_symbol->as<MultiPortSymbol>();
            auto name = copy_string(port.name);
            if (name.empty()) {
                reject_external_port_projection(
                    port.location,
                    "unnamed concatenated external ports cannot be represented in the Opto module interface");
            }
            if (!known_value_names.insert(name).second) {
                reject_external_port_projection(
                    port.location, "duplicate external port name '" + name + "'");
            }
            if (port.ports.empty()) {
                reject_external_port_projection(
                    port.location, "external port '" + name + "' has no internal components");
            }

            ExternalPortProjection projection;
            projection.name = name;
            projection.direction = port.direction;
            projection.width =
                checked_width(lowered_type_width(port.getType()), port.name);
            projection.is_signed = port.getType().isSigned();
            projection.location = port.location;
            std::optional<ArgumentDirection> component_direction;
            OptoSlangNetResolution resolution = OPTO_SLANG_NET_SINGLE_DRIVER;
            bool have_resolution = false;
            for (auto* component : port.ports) {
                if (!component) {
                    throw std::logic_error("multi-port contains a null component");
                }
                external_port_internal_value(*component, port.location, port.name);
                if (component_direction && *component_direction != component->direction) {
                    reject_external_port_projection(
                        port.location,
                        "external port '" + name +
                            "' mixes input and output component directions");
                }
                component_direction = component->direction;
                const auto component_resolution = external_port_resolution(*component);
                if (!have_resolution) {
                    resolution = component_resolution;
                    have_resolution = true;
                } else if (resolution != component_resolution) {
                    resolution = OPTO_SLANG_NET_SINGLE_DRIVER;
                }
                projection.parts.push_back(
                    ExternalPortPart{
                        component,
                        checked_width(
                            lowered_type_width(component->getType()), component->name),
                    });
            }
            if (!component_direction || *component_direction != port.direction) {
                reject_external_port_projection(
                    port.location,
                    "external port '" + name +
                        "' has a direction inconsistent with its internal components");
            }
            if (port.direction != ArgumentDirection::In &&
                port.direction != ArgumentDirection::Out) {
                reject_external_port_projection(
                    port.location,
                    "external port '" + name +
                        "' requires an exact whole-signal inout or ref mapping");
            }

            OptoSlangPortData lowered{
                name,
                lower_direction(port.direction),
                projection.width,
                projection.is_signed,
                resolution,
                intern_type_layout(design, port.getType()),
                {},
            };
            lowered.attributes = lower_multi_port_attributes(design, port);
            module.ports.push_back(std::move(lowered));
            external_port_projections.push_back(std::move(projection));
            continue;
        }
        if (port_symbol->kind != SymbolKind::Port) {
            throw std::runtime_error(
                "unsupported " + copy_string(toString(port_symbol->kind)) + " port in module '" +
                copy_string(definition.name) + "'");
        }
        const auto& port = port_symbol->as<PortSymbol>();
        auto name = copy_string(port.name);
        if (!known_value_names.insert(name).second) {
            reject_external_port_projection(
                port.externalLoc, "duplicate external port name '" + name + "'");
        }
        const auto* internal_expression = port.getInternalExpr();
        if (internal_expression) {
            if (port.direction != ArgumentDirection::In &&
                port.direction != ArgumentDirection::Out) {
                reject_external_port_projection(
                    port.externalLoc,
                    "external port '" + name +
                        "' requires an exact whole-signal inout or ref mapping");
            }
            external_port_projections.push_back(
                ExternalPortProjection{
                    name,
                    port.direction,
                    checked_width(lowered_type_width(port.getType()), port.name),
                    port.getType().isSigned(),
                    port.externalLoc,
                    {{&port, checked_width(
                                  lowered_type_width(port.getType()), port.name)}},
                });
        } else if (port.internalSymbol) {
            if (port.internalSymbol->kind == SymbolKind::Net) {
                design.value_names.emplace(&port.internalSymbol->as<NetSymbol>(), name);
            } else if (port.internalSymbol->kind == SymbolKind::Variable) {
                design.value_names.emplace(&port.internalSymbol->as<VariableSymbol>(), name);
            }
        }
        OptoSlangPortData lowered{
            std::move(name),
            lower_direction(port.direction),
            checked_width(lowered_type_width(port.getType()), port.name),
            port.getType().isSigned(),
            port.internalSymbol && port.internalSymbol->kind == SymbolKind::Net
                ? net_resolution(port.internalSymbol->as<NetSymbol>())
                : OPTO_SLANG_NET_SINGLE_DRIVER,
            intern_type_layout(design, port.getType()),
            {},
        };
        lowered.attributes = lower_port_attributes(design, port);
        module.ports.push_back(std::move(lowered));
    }

    // A synthesis black box contributes only its elaborated interface. Its
    // behavioral body may exist for simulation, but lowering it here would
    // incorrectly expand macro memories and other technology IP into gates.
    if (is_blackbox(body)) {
        return;
    }

    for (auto* net : members.nets) {
        validate_net_semantics(*net);
        if (is_procedural_local(body, *net)) {
            continue;
        }
        auto name = module_relative_name(body, *net);
        const auto [_, inserted] = design.value_names.emplace(net, name);
        if (inserted) {
            add_value_as_net(
                design, module, known_value_names, *net, std::move(name), false);
        }
    }
    for (auto* var : members.variables) {
        if (!is_procedural_local(body, *var) && var->getInitializer()) {
            throw std::runtime_error(
                "module variable initializer for '" + copy_string(var->name) +
                "' is not supported for synthesis");
        }
        if (is_procedural_local(body, *var)) {
            continue;
        }
        auto name = module_relative_name(body, *var);
        const auto [_, inserted] = design.value_names.emplace(var, name);
        if (inserted) {
            add_value_as_net(
                design, module, known_value_names, *var, std::move(name), false);
        }
    }
    for (auto* interface_instance : members.interface_instances) {
        for (const auto& signal : interface_signals(*interface_instance, {})) {
            auto name = module_relative_name(body, *signal.value);
            auto [found, inserted] = design.value_names.emplace(signal.value, name);
            if (!inserted && found->second != name) {
                throw std::runtime_error("interface signal maps to conflicting module nets");
            }
            add_value_as_net(
                design, module, known_value_names, *signal.value, std::move(name), false);
        }
    }
    for (auto* net : members.nets) {
        if (!is_procedural_local(body, *net)) {
            continue;
        }
        auto name = unique_internal_name(known_value_names, procedural_local_base_name(body, *net));
        design.value_names.emplace(net, name);
        OptoSlangNetData lowered{
            std::move(name),
            checked_width(lowered_type_width(net->getType()), net->name),
            net->getType().isSigned(),
            unpacked_element_is_signed(net->getType()),
            true,
            net_resolution(*net),
            intern_type_layout(design, net->getType()),
            {},
        };
        lowered.attributes = lower_symbol_attributes(design, *net);
        module.nets.push_back(std::move(lowered));
    }
    for (auto* var : members.variables) {
        if (!is_procedural_local(body, *var)) {
            continue;
        }
        auto name = unique_internal_name(known_value_names, procedural_local_base_name(body, *var));
        design.value_names.emplace(var, name);
        OptoSlangNetData lowered{
            std::move(name),
            checked_width(lowered_type_width(var->getType()), var->name),
            var->getType().isSigned(),
            unpacked_element_is_signed(var->getType()),
            var->lifetime == VariableLifetime::Automatic,
            OPTO_SLANG_NET_SINGLE_DRIVER,
            intern_type_layout(design, var->getType()),
            {},
        };
        lowered.attributes = lower_symbol_attributes(design, *var);
        module.nets.push_back(std::move(lowered));
    }

    design.value_shapes.reserve(module.ports.size() + module.nets.size());
    for (const auto& port : module.ports) {
        design.value_shapes.emplace(port.name, ValueShape{port.width, port.is_signed});
    }
    for (const auto& net : module.nets) {
        design.value_shapes.emplace(net.name, ValueShape{net.width, net.is_signed});
    }

    // Interface instances are flattened into their enclosing module, so their
    // scalar constructor ports must become ordinary connections at the same
    // boundary. Interface-typed constructor ports are represented by the
    // nested interface storage collected above and need no value assignment.
    for (auto* interface_instance : members.interface_instances) {
        for (auto* connection : interface_instance->getPortConnections()) {
            if (!connection || connection->port.kind != SymbolKind::Port) {
                continue;
            }
            const auto& port = connection->port.as<PortSymbol>();
            const auto* actual = connection->getExpression();
            if (!actual || is_empty_connection_expression(*actual)) {
                continue;
            }
            if (!port.internalSymbol || !ValueSymbol::isKind(port.internalSymbol->kind)) {
                throw std::runtime_error(
                    "interface port '" + copy_string(port.name) +
                    "' has no synthesizable internal storage");
            }
            const auto& internal = port.internalSymbol->as<ValueSymbol>();
            auto* internal_value =
                make_signal_expr(design, registered_value_name(design, internal));
            switch (port.direction) {
            case ArgumentDirection::In:
                module.assigns.push_back(
                    OptoSlangAssignData{internal_value, lower_expr(design, *actual)});
                break;
            case ArgumentDirection::Out:
                module.assigns.push_back(
                    OptoSlangAssignData{
                        lower_signal_expr(
                            design, instance_connection_expression(*connection)),
                        internal_value,
                    });
                break;
            case ArgumentDirection::InOut:
            case ArgumentDirection::Ref:
                throw LoweringFailure(
                    OPTO_SLANG_LOWERING_UNSUPPORTED_PROFILE,
                    5,
                    port.location,
                    "interface constructor port '" + copy_string(port.name) +
                        "' requires a bidirectional alias");
            }
        }
    }

    lower_external_port_projections(design, external_port_projections);

    for (auto* child : members.instances) {
        OptoSlangInstanceData instance;
        instance.name = module_relative_name(body, *child);
        instance.module_name = module_name_for_body(design, child->body);
        instance.attributes = lower_symbol_attributes(design, *child);
        for (auto* connection : child->getPortConnections()) {
            if (!connection) {
                continue;
            }
            if (connection->port.kind == SymbolKind::InterfacePort) {
                const auto& port = connection->port.as<InterfacePortSymbol>();
                auto [connected, selected_modport] = connection->getIfaceConn();
                std::vector<const InstanceSymbol*> leaves;
                collect_interface_leaves(connected, leaves);
                if (leaves.empty()) {
                    throw std::runtime_error(
                        "interface port '" + copy_string(port.name) + "' on instance '" +
                        instance.name + "' is unconnected");
                }
                auto modport_name = port.modport;
                if (modport_name.empty() && selected_modport) {
                    modport_name = selected_modport->name;
                }
                for (size_t element = 0; element < leaves.size(); ++element) {
                    for (auto signal : interface_signals(*leaves[element], modport_name)) {
                        if (modport_name.empty()) {
                            signal.direction =
                                infer_interface_direction(child->body, *signal.value);
                        }
                        if (signal.direction == ArgumentDirection::Out &&
                            !interface_value_is_driven(child->body, *signal.reference)) {
                            continue;
                        }
                        instance.connections.push_back(
                            OptoSlangConnectionData{
                                flattened_interface_port_name(
                                    port.name, element, leaves.size(), signal.name),
                                signal.connection
                                    ? lower_expr(design, *signal.connection)
                                    : make_signal_expr(
                                          design,
                                          registered_value_name(design, *signal.value)),
                            });
                    }
                }
                continue;
            }
            auto* expr = connection->getExpression();
            if (!expr) {
                continue;
            }
            if (is_empty_connection_expression(*expr)) {
                continue;
            }
            instance.connections.push_back(
                OptoSlangConnectionData{
                    copy_string(connection->port.name),
                    lower_expr(design, instance_connection_expression(*connection)),
                });
        }
        module.instances.emplace_back(std::move(instance));
    }

    size_t unresolved_connection_index = 0;
    for (auto* child : members.unresolved_instances) {
        OptoSlangInstanceData instance;
        instance.name = module_relative_name(body, *child);
        instance.module_name = copy_string(child->definitionName);
        instance.attributes = lower_symbol_attributes(design, *child);
        auto* syntax = child->getSyntax();
        if (!syntax || syntax->kind != SyntaxKind::HierarchicalInstance) {
            throw std::runtime_error(
                "unresolved instance '" + instance.name + "' has no instance syntax");
        }

        for (auto* port : syntax->as<HierarchicalInstanceSyntax>().connections) {
            if (!port) {
                continue;
            }
            if (port->kind == SyntaxKind::OrderedPortConnection) {
                throw std::runtime_error(
                    "ordered port connections on unresolved instance '" + instance.name +
                    "' are not supported");
            }
            if (port->kind != SyntaxKind::NamedPortConnection) {
                throw std::runtime_error(
                    "unsupported port connection syntax on unresolved instance '" + instance.name +
                    "'");
            }

            const auto& named = port->as<NamedPortConnectionSyntax>();
            auto port_name = copy_string(named.name.valueText());
            if (port_name.empty()) {
                throw std::runtime_error(
                    "empty port name on unresolved instance '" + instance.name + "'");
            }

            if (unresolved_connection_index >= bound_unresolved_connections.size()) {
                throw std::runtime_error(
                    "unresolved instance connection was not bound "
                    "before parallel lowering");
            }
            const Expression* expr = bound_unresolved_connections[unresolved_connection_index++];
            if (!expr || is_empty_connection_expression(*expr)) {
                continue;
            }
            instance.connections.push_back(
                OptoSlangConnectionData{
                    std::move(port_name),
                    lower_expr(design, *expr),
                });
        }
        module.instances.emplace_back(std::move(instance));
    }
    if (unresolved_connection_index != bound_unresolved_connections.size()) {
        throw std::runtime_error(
            "unresolved instance connection binding count "
            "does not match lowering order");
    }

    for (auto* primitive : members.primitives) {
        lower_primitive_instance(design, module, *primitive);
    }

    for (auto* net : members.nets) {
        auto* initializer = net->getInitializer();
        if (!initializer) {
            continue;
        }
        OptoSlangExpr lhs;
        lhs.kind = OPTO_SLANG_EXPR_SIGNAL;
        lhs.signal_name = intern_string(design, registered_value_name(design, *net));
        module.assigns.push_back(
            OptoSlangAssignData{
                make_expr(design, std::move(lhs), *initializer),
                lower_expr(design, *initializer),
            });
    }

    for (auto* assign : members.assigns) {
        validate_continuous_assign_semantics(*assign);
        const auto& expr = assign->getAssignment();
        if (expr.kind != ExpressionKind::Assignment) {
            throw std::runtime_error("continuous assign did not lower to assignment expression");
        }
        const auto& assignment = expr.as<AssignmentExpression>();
        auto lowered = lower_continuous_assignment(design, assignment);
        module.assigns.insert(
            module.assigns.end(),
            std::make_move_iterator(lowered.begin()),
            std::make_move_iterator(lowered.end()));
    }

    for (auto* process : members.processes) {
        if (process->procedureKind == ProceduralBlockKind::Initial) {
            validate_initial_process(design, body, *process);
        }
        auto lowered = lower_procedure(design, body, *process);
        if (!lowered.blocks.empty()) {
            module.procedures.push_back(std::move(lowered));
        }
    }
}

struct ModuleLoweringJob {
    const InstanceBodySymbol* body = nullptr;
    ModuleMembers members;
    std::vector<const Expression*> bound_unresolved_connections;
};

void collect_module_jobs(
    OptoSlangSnapshot& design,
    const InstanceBodySymbol& body,
    std::unordered_set<std::string>& seen,
    std::vector<ModuleLoweringJob>& jobs) {
    auto name = module_name_for_body(design, body);
    if (!seen.insert(name).second) {
        return;
    }

    ModuleMembers members;
    if (!is_blackbox(body)) {
        collect_elaborated_members(body, body, members);
    }
    auto children = members.instances;
    jobs.push_back(ModuleLoweringJob{&body, std::move(members), {}});
    for (auto* child : children) {
        collect_module_jobs(design, child->body, seen, jobs);
    }
}

void bind_unresolved_connections(std::vector<ModuleLoweringJob>& jobs) {
    for (auto& job : jobs) {
        for (auto* child : job.members.unresolved_instances) {
            auto connections = child->getPortConnections();
            auto names = child->getPortNames();
            if (connections.size() != names.size()) {
                throw std::runtime_error(
                    "slang unresolved instance connection and name counts differ");
            }
            for (size_t index = 0; index < connections.size(); ++index) {
                if (names[index].empty()) {
                    throw std::runtime_error(
                        "ordered port connections on unresolved instance '" +
                        copy_string(child->name) + "' are not supported");
                }
                auto* assertion = connections[index];
                if (!assertion || assertion->kind != AssertionExprKind::Simple) {
                    throw std::runtime_error(
                        "unsupported assertion expression on unresolved instance '" +
                        copy_string(child->name) + "'");
                }
                const auto& simple = assertion->as<SimpleAssertionExpr>();
                if (simple.repetition) {
                    throw std::runtime_error(
                        "repeated assertion expression on unresolved instance '" +
                        copy_string(child->name) + "' is not synthesizable");
                }
                job.bound_unresolved_connections.push_back(&simple.expr);
            }
        }
    }
}

std::unique_ptr<OptoSlangModulePayload> lower_module_job(
    const OptoSlangSnapshot& shared,
    const ModuleLoweringJob& job) {
    auto module = std::make_unique<OptoSlangModulePayload>();
    ModuleLoweringContext lowering(*module, *job.body, shared.body_names, shared.source_manager);
    lower_body(lowering, job.members, job.bound_unresolved_connections);
    return module;
}

} // namespace opto::slang_lower

using namespace opto::slang_lower;

struct OptoSlangCompilationState {
    std::unique_ptr<Driver> driver;
    std::unique_ptr<Compilation> compilation;
    std::vector<ModuleLoweringJob> jobs;

    OptoSlangCompilationState(
        std::unique_ptr<Driver> driver,
        std::unique_ptr<Compilation> compilation,
        std::vector<ModuleLoweringJob> jobs)
        : driver(std::move(driver)), compilation(std::move(compilation)), jobs(std::move(jobs)) {}
};

OptoSlangSnapshot::OptoSlangSnapshot() = default;
OptoSlangSnapshot::~OptoSlangSnapshot() = default;

void opto_slang_prepare_module_names(
    OptoSlangSnapshot& design, std::span<const InstanceSymbol* const> tops) {
    struct Specialization {
        const InstanceBodySymbol* representative;
        std::vector<const InstanceBodySymbol*> bodies;
        std::string signature;
    };

    std::vector<const InstanceBodySymbol*> bodies;
    std::unordered_set<const InstanceBodySymbol*> seen;
    for (auto* top : tops) {
        if (top) {
            collect_instance_bodies(top->body, bodies, seen);
        }
    }

    std::map<std::string, std::vector<Specialization>> definitions;
    for (auto* body : bodies) {
        auto definition = copy_string(body->getDefinition().name);
        auto& specializations = definitions[definition];
        auto found =
            std::ranges::find_if(specializations, [body](const Specialization& specialization) {
                return body->hasSameType(*specialization.representative);
            });
        if (found == specializations.end()) {
            specializations.push_back(
                Specialization{
                    body,
                    {body},
                    specialization_signature(*body),
                });
        } else {
            found->bodies.push_back(body);
        }
    }

    std::unordered_map<std::string, std::string> signatures_by_name;
    for (auto& [definition, specializations] : definitions) {
        for (auto& specialization : specializations) {
            auto name = specializations.size() == 1
                            ? definition
                            : specialization_name(definition, specialization.signature);
            auto [found, inserted] = signatures_by_name.emplace(name, specialization.signature);
            if (!inserted && found->second != specialization.signature) {
                throw std::runtime_error(
                    "parameter specialization name hash collision for module '" + definition + "'");
            }
            for (auto* body : specialization.bodies) {
                design.body_names.emplace(body, name);
            }
        }
    }
}

void opto_slang_collect_modules(
    OptoSlangSnapshot& design,
    std::span<const InstanceSymbol* const> tops,
    std::unique_ptr<Driver> driver,
    std::unique_ptr<Compilation> compilation) {
    std::vector<ModuleLoweringJob> jobs;
    std::unordered_set<std::string> seen;
    for (auto* top : tops) {
        if (top) {
            collect_module_jobs(design, top->body, seen, jobs);
        }
    }
    bind_unresolved_connections(jobs);
    std::ranges::sort(jobs, [&](const auto& left, const auto& right) {
        const auto left_order = source_order(left.body->getDefinition());
        const auto right_order = source_order(right.body->getDefinition());
        if (left_order != right_order) {
            return left_order < right_order;
        }
        return module_name_for_body(design, *left.body) < module_name_for_body(design, *right.body);
    });

    design.modules.reserve(jobs.size());
    for (const auto& job : jobs) {
        auto module = std::make_unique<OptoSlangModuleData>();
        module->name = module_name_for_body(design, *job.body);
        module->source_order = source_order(job.body->getDefinition());
        design.modules.push_back(std::move(module));
    }

    design.source_manager = &driver->sourceManager;
    compilation->freeze();
    design.compilation_state = std::make_unique<OptoSlangCompilationState>(
        std::move(driver), std::move(compilation), std::move(jobs));
}

void set_lowering_failure_source(
    OptoSlangLoweringFailure& failure,
    const OptoSlangSnapshot& design,
    SourceLocation location) {
    if (!location.valid() || !design.source_manager) {
        return;
    }
    location = design.source_manager->getFullyOriginalLoc(location);
    failure.file = design.source_manager->getFullPath(location.buffer()).string();
    failure.line = static_cast<uint32_t>(std::min<size_t>(
        design.source_manager->getLineNumber(location), UINT32_MAX));
    failure.column = static_cast<uint32_t>(std::min<size_t>(
        design.source_manager->getColumnNumber(location), UINT32_MAX));
}

OptoSlangStatus opto_slang_materialize_module(OptoSlangSnapshot& design, size_t module_index) {
    if (!design.compilation_state || module_index >= design.modules.size()) {
        return OPTO_SLANG_ERROR;
    }
    auto& target = *design.modules[module_index];
    try {
        std::lock_guard lock(target.materialize_mutex);
        try {
            if (target.payload) {
                ++target.materialize_users;
                return OPTO_SLANG_OK;
            }
            if (target.materialize_failure) {
                return OPTO_SLANG_ERROR;
            }
            // Slang supports parallel visitation of a frozen AST only when the
            // visitor avoids lazy semantic work such as constant folding. Native
            // synthesis lowering performs evaluation and binding, so serialize
            // that part while keeping Rust IR conversion outside this lock.
            std::lock_guard materialization_lock(design.materialization_mutex);
            const auto& job = design.compilation_state->jobs[module_index];
            target.payload = lower_module_job(design, job);
            target.materialize_users = 1;
            return OPTO_SLANG_OK;
        } catch (const LoweringFailure& error) {
            try {
                OptoSlangLoweringFailure failure;
                failure.category = error.category;
                failure.code = error.code;
                failure.message = error.what();
                set_lowering_failure_source(failure, design, error.location);
                target.materialize_failure = std::move(failure);
            } catch (...) {
                target.materialize_failure.reset();
            }
            return OPTO_SLANG_ERROR;
        } catch (const std::bad_alloc& error) {
            try {
                target.materialize_failure = OptoSlangLoweringFailure{
                    OPTO_SLANG_LOWERING_CAPACITY, 1, error.what()};
                set_lowering_failure_source(
                    *target.materialize_failure,
                    design,
                    design.compilation_state->jobs[module_index].body->getDefinition().location);
            } catch (...) {
                target.materialize_failure.reset();
            }
            return OPTO_SLANG_ERROR;
        } catch (const std::logic_error& error) {
            try {
                target.materialize_failure = OptoSlangLoweringFailure{
                    OPTO_SLANG_LOWERING_INVARIANT, 1, error.what()};
                set_lowering_failure_source(
                    *target.materialize_failure,
                    design,
                    design.compilation_state->jobs[module_index].body->getDefinition().location);
            } catch (...) {
                target.materialize_failure.reset();
            }
            return OPTO_SLANG_ERROR;
        } catch (const std::exception& error) {
            try {
                target.materialize_failure = OptoSlangLoweringFailure{
                    OPTO_SLANG_LOWERING_UNSUPPORTED_PROFILE, 1, error.what()};
                set_lowering_failure_source(
                    *target.materialize_failure,
                    design,
                    design.compilation_state->jobs[module_index].body->getDefinition().location);
            } catch (...) {
                target.materialize_failure.reset();
            }
            return OPTO_SLANG_ERROR;
        } catch (...) {
            try {
                target.materialize_failure = OptoSlangLoweringFailure{
                    OPTO_SLANG_LOWERING_NATIVE,
                    1,
                    "unknown slang module materialization failure"};
                set_lowering_failure_source(
                    *target.materialize_failure,
                    design,
                    design.compilation_state->jobs[module_index].body->getDefinition().location);
            } catch (...) {
                target.materialize_failure.reset();
            }
            return OPTO_SLANG_ERROR;
        }
    } catch (...) {
        // Lock acquisition failure cannot be recorded without introducing an
        // unsynchronized write to shared module state.
        return OPTO_SLANG_ERROR;
    }
}

void opto_slang_release_module(OptoSlangSnapshot& design, size_t module_index) {
    if (module_index >= design.modules.size()) {
        return;
    }
    try {
        auto& module = *design.modules[module_index];
        std::lock_guard lock(module.materialize_mutex);
        if (module.materialize_users == 0) {
            return;
        }
        if (--module.materialize_users != 0) {
            return;
        }
        module.payload.reset();
    } catch (...) {
        // A release failure must not unwind through the C ABI. The payload
        // remains owned by the snapshot and will be reclaimed with it.
    }
}
