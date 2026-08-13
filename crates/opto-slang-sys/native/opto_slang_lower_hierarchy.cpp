// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

#include "opto_slang_lower_internal.h"

namespace opto::slang_lower {

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
        checked_width(symbol.getType().getBitstreamWidth(), symbol.name),
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

    if (primitive == "bufif0" || primitive == "bufif1") {
        require_count(3, 3);
        const auto& data_expr = require_primitive_port(instance, ports, 1);
        const auto& control_expr = require_primitive_port(instance, ports, 2);
        auto* data = lower_expr(design, data_expr);
        auto* control = lower_expr(design, control_expr);
        auto* high_impedance = make_high_impedance_expr(design, control_expr);
        auto* result = primitive == "bufif1"
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
                        !interface_value_is_driven(body, *signal.value)) {
                        continue;
                    }
                    auto name = flattened_interface_port_name(
                        port.name, element, leaves.size(), signal.name);
                    if (!known_value_names.insert(name).second) {
                        throw std::runtime_error(
                            "duplicate flattened interface port '" + name + "'");
                    }
                    auto [found, inserted] = scoped_names.emplace(signal.value, name);
                    if (!inserted && found->second != name) {
                        throw std::runtime_error(
                            "interface signal maps to conflicting flattened ports");
                    }
                    auto [reference, reference_inserted] =
                        scoped_names.emplace(signal.reference, name);
                    if (!reference_inserted && reference->second != name) {
                        throw std::runtime_error(
                            "interface reference maps to conflicting flattened ports");
                    }
                    const auto& type = signal.value->getType();
                    OptoSlangPortData lowered{
                        std::move(name),
                        lower_direction(signal.direction),
                        checked_width(type.getBitstreamWidth(), signal.name),
                        type.isSigned(),
                        signal.value->kind == SymbolKind::Net
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
        if (port_symbol->kind != SymbolKind::Port) {
            throw std::runtime_error(
                "unsupported " + copy_string(toString(port_symbol->kind)) + " port in module '" +
                copy_string(definition.name) + "'");
        }
        const auto& port = port_symbol->as<PortSymbol>();
        auto name = copy_string(port.name);
        known_value_names.insert(name);
        if (port.internalSymbol) {
            if (port.internalSymbol->kind == SymbolKind::Net) {
                design.value_names.emplace(&port.internalSymbol->as<NetSymbol>(), name);
            } else if (port.internalSymbol->kind == SymbolKind::Variable) {
                design.value_names.emplace(&port.internalSymbol->as<VariableSymbol>(), name);
            }
        }
        OptoSlangPortData lowered{
            std::move(name),
            lower_direction(port.direction),
            checked_width(port.getType().getBitstreamWidth(), port.name),
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
        design.value_names.emplace(net, name);
        add_value_as_net(design, module, known_value_names, *net, std::move(name));
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
        design.value_names.emplace(var, name);
        add_value_as_net(design, module, known_value_names, *var, std::move(name));
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
            checked_width(net->getType().getBitstreamWidth(), net->name),
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
            checked_width(var->getType().getBitstreamWidth(), var->name),
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
                            !interface_value_is_driven(child->body, *signal.value)) {
                            continue;
                        }
                        instance.connections.push_back(
                            OptoSlangConnectionData{
                                flattened_interface_port_name(
                                    port.name, element, leaves.size(), signal.name),
                                make_signal_expr(
                                    design, registered_value_name(design, *signal.value)),
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
            if (!target.materialize_error.empty()) {
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
        } catch (const std::exception& error) {
            try {
                target.materialize_error = error.what();
            } catch (...) {
                target.materialize_error.clear();
            }
            return OPTO_SLANG_ERROR;
        } catch (...) {
            try {
                target.materialize_error = "unknown slang module materialization failure";
            } catch (...) {
                target.materialize_error.clear();
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
