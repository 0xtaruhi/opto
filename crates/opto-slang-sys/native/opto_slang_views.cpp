// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

#include "opto_slang_internal.h"

namespace {

template <typename Source, typename View> bool valid_view(const Source* source, const View* view) {
    return source && view;
}

const char* optional_string(const std::string* value) {
    return value && !value->empty() ? value->c_str() : nullptr;
}

const OptoSlangModulePayload* module_payload(
    const OptoSlangSnapshot* design,
    size_t module_index) {
    if (!design || module_index >= design->modules.size()) {
        return nullptr;
    }
    return design->modules[module_index]->payload.get();
}

void assign_attribute_view(
    const OptoSlangAttributeData& attribute,
    OptoSlangAttributeView& view) {
    view = OptoSlangAttributeView{
        attribute.name.c_str(),
        attribute.kind,
        attribute.value.c_str(),
        attribute.integer_width,
        attribute.integer_signed ? 1 : 0,
        attribute.is_true ? 1 : 0,
        attribute.source,
    };
}

} // namespace

extern "C" {

void opto_slang_snapshot_free(OptoSlangSnapshot* design) {
    delete design;
}

OptoSlangStatus
opto_slang_snapshot_view(const OptoSlangSnapshot* design, OptoSlangSnapshotView* view) {
    if (!valid_view(design, view)) {
        return OPTO_SLANG_ERROR;
    }
    *view = OptoSlangSnapshotView{
        design->top.empty() ? nullptr : design->top.c_str(),
        design->modules.size(),
    };
    return OPTO_SLANG_OK;
}

OptoSlangStatus opto_slang_module_info(
    const OptoSlangSnapshot* design, size_t module_index, OptoSlangModuleInfoView* view) {
    if (!design || !view || module_index >= design->modules.size()) {
        return OPTO_SLANG_ERROR;
    }
    const auto& module = *design->modules[module_index];
    *view = OptoSlangModuleInfoView{module.name.c_str(), module.source_order};
    return OPTO_SLANG_OK;
}

OptoSlangStatus opto_slang_module_view(
    const OptoSlangSnapshot* design, size_t module_index, OptoSlangModuleView* view) {
    if (!design || !view || module_index >= design->modules.size()) {
        return OPTO_SLANG_ERROR;
    }
    try {
        auto& module = *design->modules[module_index];
        std::lock_guard lock(module.materialize_mutex);
        if (!module.payload || module.materialize_users == 0) {
            return OPTO_SLANG_ERROR;
        }
        const auto& payload = *module.payload;
        *view = OptoSlangModuleView{
            payload.attributes.size(),
            payload.ports.size(),
            payload.nets.size(),
            payload.instances.size(),
            payload.assigns.size(),
            payload.procedures.size(),
        };
        return OPTO_SLANG_OK;
    } catch (...) {
        return OPTO_SLANG_ERROR;
    }
}

OptoSlangStatus opto_slang_module_attribute_view(
    const OptoSlangSnapshot* design,
    size_t module_index,
    size_t attribute_index,
    OptoSlangAttributeView* view) {
    const auto* payload = module_payload(design, module_index);
    if (!payload || !view || attribute_index >= payload->attributes.size()) {
        return OPTO_SLANG_ERROR;
    }
    assign_attribute_view(payload->attributes[attribute_index], *view);
    return OPTO_SLANG_OK;
}

OptoSlangStatus opto_slang_module_materialize(OptoSlangSnapshot* design, size_t module_index) {
    if (!design) {
        return OPTO_SLANG_ERROR;
    }
    return opto_slang_materialize_module(*design, module_index);
}

OptoSlangStatus opto_slang_module_materialize_failure(
    const OptoSlangSnapshot* design,
    size_t module_index,
    OptoSlangLoweringFailureView* view) {
    if (!design || !view || module_index >= design->modules.size()) {
        return OPTO_SLANG_ERROR;
    }
    try {
        auto& module = *design->modules[module_index];
        std::lock_guard lock(module.materialize_mutex);
        if (!module.materialize_failure) {
            return OPTO_SLANG_ERROR;
        }
        const auto& failure = *module.materialize_failure;
        view->category = failure.category;
        view->code = failure.code;
        view->message = failure.message.c_str();
        view->source = {
            failure.file.empty() ? nullptr : failure.file.c_str(),
            failure.line,
            failure.column,
        };
        return OPTO_SLANG_OK;
    } catch (...) {
        return OPTO_SLANG_ERROR;
    }
}

void opto_slang_module_release(OptoSlangSnapshot* design, size_t module_index) {
    if (design) {
        opto_slang_release_module(*design, module_index);
    }
}

OptoSlangStatus opto_slang_port_view(
    const OptoSlangSnapshot* design,
    size_t module_index,
    size_t port_index,
    OptoSlangPortView* view) {
    const auto* payload = module_payload(design, module_index);
    if (!payload || !view) {
        return OPTO_SLANG_ERROR;
    }
    const auto& ports = payload->ports;
    if (port_index >= ports.size()) {
        return OPTO_SLANG_ERROR;
    }
    const auto* port = &ports[port_index];
    *view = OptoSlangPortView{
        port,
        port->name.c_str(),
        port->direction,
        port->width,
        port->is_signed ? 1 : 0,
        port->resolution,
        port->type_layout,
        port->attributes.size(),
    };
    return OPTO_SLANG_OK;
}

OptoSlangStatus opto_slang_port_attribute_view(
    const OptoSlangPortData* port,
    size_t attribute_index,
    OptoSlangAttributeView* view) {
    if (!port || !view || attribute_index >= port->attributes.size()) {
        return OPTO_SLANG_ERROR;
    }
    assign_attribute_view(port->attributes[attribute_index], *view);
    return OPTO_SLANG_OK;
}

OptoSlangStatus opto_slang_net_view(
    const OptoSlangSnapshot* design,
    size_t module_index,
    size_t net_index,
    OptoSlangNetView* view) {
    const auto* payload = module_payload(design, module_index);
    if (!payload || !view) {
        return OPTO_SLANG_ERROR;
    }
    const auto& nets = payload->nets;
    if (net_index >= nets.size()) {
        return OPTO_SLANG_ERROR;
    }
    const auto* net = &nets[net_index];
    *view = OptoSlangNetView{
        net,
        net->name.c_str(),
        net->width,
        net->is_signed ? 1 : 0,
        net->element_is_signed ? 1 : 0,
        net->is_process_local ? 1 : 0,
        net->resolution,
        net->type_layout,
        net->attributes.size(),
    };
    return OPTO_SLANG_OK;
}

OptoSlangStatus opto_slang_net_attribute_view(
    const OptoSlangNetData* net,
    size_t attribute_index,
    OptoSlangAttributeView* view) {
    if (!net || !view || attribute_index >= net->attributes.size()) {
        return OPTO_SLANG_ERROR;
    }
    assign_attribute_view(net->attributes[attribute_index], *view);
    return OPTO_SLANG_OK;
}

OptoSlangStatus opto_slang_instance_view(
    const OptoSlangSnapshot* design,
    size_t module_index,
    size_t instance_index,
    OptoSlangInstanceView* view) {
    const auto* payload = module_payload(design, module_index);
    if (!payload || !view) {
        return OPTO_SLANG_ERROR;
    }
    const auto& instances = payload->instances;
    if (instance_index >= instances.size()) {
        return OPTO_SLANG_ERROR;
    }
    const auto* instance = &instances[instance_index];
    *view = OptoSlangInstanceView{
        instance,
        instance->name.c_str(),
        instance->module_name.c_str(),
        instance->connections.size(),
        instance->attributes.size(),
    };
    return OPTO_SLANG_OK;
}

OptoSlangStatus opto_slang_instance_attribute_view(
    const OptoSlangInstanceData* instance,
    size_t attribute_index,
    OptoSlangAttributeView* view) {
    if (!instance || !view || attribute_index >= instance->attributes.size()) {
        return OPTO_SLANG_ERROR;
    }
    assign_attribute_view(instance->attributes[attribute_index], *view);
    return OPTO_SLANG_OK;
}

OptoSlangStatus opto_slang_connection_view(
    const OptoSlangInstanceData* instance, size_t connection_index, OptoSlangConnectionView* view) {
    if (!instance || !view || connection_index >= instance->connections.size()) {
        return OPTO_SLANG_ERROR;
    }
    const auto* connection = &instance->connections[connection_index];
    *view = OptoSlangConnectionView{
        connection->port.c_str(),
        connection->expr,
    };
    return OPTO_SLANG_OK;
}

OptoSlangStatus opto_slang_assign_view(
    const OptoSlangSnapshot* design,
    size_t module_index,
    size_t assign_index,
    OptoSlangAssignView* view) {
    const auto* payload = module_payload(design, module_index);
    if (!payload || !view) {
        return OPTO_SLANG_ERROR;
    }
    const auto& assigns = payload->assigns;
    if (assign_index >= assigns.size()) {
        return OPTO_SLANG_ERROR;
    }
    const auto* assign = &assigns[assign_index];
    *view = OptoSlangAssignView{assign->lhs, assign->rhs};
    return OPTO_SLANG_OK;
}

OptoSlangStatus opto_slang_procedure_view(
    const OptoSlangSnapshot* design,
    size_t module_index,
    size_t procedure_index,
    OptoSlangProcedureView* view) {
    const auto* payload = module_payload(design, module_index);
    if (!payload || !view) {
        return OPTO_SLANG_ERROR;
    }
    const auto& procedures = payload->procedures;
    if (procedure_index >= procedures.size()) {
        return OPTO_SLANG_ERROR;
    }
    const auto* procedure = &procedures[procedure_index];
    *view = OptoSlangProcedureView{
        procedure,
        procedure->kind,
        procedure->events.size(),
        procedure->blocks.size(),
        procedure->loop_regions.size(),
        procedure->entry_block,
        procedure->source,
    };
    return OPTO_SLANG_OK;
}

OptoSlangStatus opto_slang_loop_region_view(
    const OptoSlangProcedureData* procedure,
    size_t region_index,
    OptoSlangLoopRegionView* view) {
    if (!procedure || !view || region_index >= procedure->loop_regions.size()) {
        return OPTO_SLANG_ERROR;
    }
    const auto& region = procedure->loop_regions[region_index];
    *view = OptoSlangLoopRegionView{
        region.header,
        region.body,
        region.latch,
        region.exit,
        region.form,
        region.parent ? 1 : 0,
        region.parent.value_or(0),
        region.source,
    };
    return OPTO_SLANG_OK;
}

OptoSlangStatus opto_slang_event_view(
    const OptoSlangProcedureData* procedure, size_t event_index, OptoSlangEventView* view) {
    if (!procedure || !view || event_index >= procedure->events.size()) {
        return OPTO_SLANG_ERROR;
    }
    const auto* event = &procedure->events[event_index];
    *view = OptoSlangEventView{event->edge, event->signal, event->source};
    return OPTO_SLANG_OK;
}

OptoSlangStatus opto_slang_block_view(
    const OptoSlangProcedureData* procedure, size_t block_index, OptoSlangBlockView* view) {
    if (!procedure || !view || block_index >= procedure->blocks.size()) {
        return OPTO_SLANG_ERROR;
    }
    const auto& block = procedure->blocks[block_index];
    if (!block.terminated) {
        return OPTO_SLANG_ERROR;
    }
    *view = OptoSlangBlockView{block.effects.size(), block.terminator.kind, block.source};
    return OPTO_SLANG_OK;
}

OptoSlangStatus opto_slang_effect_view(
    const OptoSlangProcedureData* procedure,
    size_t block_index,
    size_t effect_index,
    OptoSlangEffectView* view) {
    if (!procedure || !view || block_index >= procedure->blocks.size() ||
        effect_index >= procedure->blocks[block_index].effects.size()) {
        return OPTO_SLANG_ERROR;
    }
    const auto& effect = procedure->blocks[block_index].effects[effect_index];
    *view = OptoSlangEffectView{
        effect.lhs,
        effect.rhs,
        effect.blocking ? 1 : 0,
        effect.source,
    };
    return OPTO_SLANG_OK;
}

OptoSlangStatus opto_slang_terminator_view(
    const OptoSlangProcedureData* procedure,
    size_t block_index,
    OptoSlangTerminatorView* view) {
    if (!procedure || !view || block_index >= procedure->blocks.size() ||
        !procedure->blocks[block_index].terminated) {
        return OPTO_SLANG_ERROR;
    }
    const auto& terminator = procedure->blocks[block_index].terminator;
    *view = OptoSlangTerminatorView{
        terminator.kind,
        terminator.condition,
        terminator.selector,
        {terminator.jump_edge.block, terminator.jump_edge.source},
        {terminator.then_edge.block, terminator.then_edge.source},
        {terminator.else_edge.block, terminator.else_edge.source},
        {terminator.default_edge.block, terminator.default_edge.source},
        terminator.arms.size(),
        terminator.source,
    };
    return OPTO_SLANG_OK;
}

OptoSlangStatus opto_slang_switch_arm_view(
    const OptoSlangProcedureData* procedure,
    size_t block_index,
    size_t arm_index,
    OptoSlangSwitchArmView* view) {
    if (!procedure || !view || block_index >= procedure->blocks.size()) {
        return OPTO_SLANG_ERROR;
    }
    const auto& terminator = procedure->blocks[block_index].terminator;
    if (terminator.kind != OPTO_SLANG_TERMINATOR_SWITCH || arm_index >= terminator.arms.size()) {
        return OPTO_SLANG_ERROR;
    }
    const auto& arm = terminator.arms[arm_index];
    *view = OptoSlangSwitchArmView{
        arm.pattern,
        {arm.edge.block, arm.edge.source},
    };
    return OPTO_SLANG_OK;
}

OptoSlangStatus
opto_slang_type_layout_view(const OptoSlangTypeLayout* layout, OptoSlangTypeLayoutView* view) {
    if (!valid_view(layout, view)) {
        return OPTO_SLANG_ERROR;
    }
    *view = OptoSlangTypeLayoutView{
        layout->kind,
        layout->width,
        layout->array_left,
        layout->array_right,
        layout->array_is_packed ? 1 : 0,
        layout->array_element,
        layout->fields.size(),
    };
    return OPTO_SLANG_OK;
}

OptoSlangStatus opto_slang_type_field_view(
    const OptoSlangTypeLayout* layout, size_t field_index, OptoSlangTypeFieldView* view) {
    if (!layout || !view || field_index >= layout->fields.size()) {
        return OPTO_SLANG_ERROR;
    }
    const auto* field = &layout->fields[field_index];
    *view = OptoSlangTypeFieldView{
        field->name.c_str(),
        field->bit_offset,
        field->layout,
    };
    return OPTO_SLANG_OK;
}

OptoSlangStatus opto_slang_expr_view(const OptoSlangExpr* expression, OptoSlangExprView* view) {
    if (!valid_view(expression, view)) {
        return OPTO_SLANG_ERROR;
    }
    *view = OptoSlangExprView{
        expression->kind,
        optional_string(expression->source_file),
        expression->source_line,
        expression->source_column,
        optional_string(expression->signal_name),
        expression->signal_has_range ? 1 : 0,
        expression->signal_msb,
        expression->signal_lsb,
        expression->constant_has_width ? 1 : 0,
        expression->constant_width,
        expression->constant_signed ? 1 : 0,
        expression->constant_bits.c_str(),
        expression->unary_op,
        expression->unary_arg,
        expression->binary_op,
        expression->binary_left,
        expression->binary_right,
        expression->concat_parts.data(),
        expression->concat_parts.size(),
        expression->mux_condition,
        expression->mux_then,
        expression->mux_else,
        expression->cast_kind,
        expression->cast_value,
        expression->cast_width,
        expression->cast_signed ? 1 : 0,
        expression->extract_value,
        expression->extract_lsb,
        expression->extract_width,
        expression->dynamic_extract_value,
        expression->dynamic_extract_offset,
        expression->dynamic_extract_width,
    };
    return OPTO_SLANG_OK;
}

} // extern "C"
