// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Deterministic structural Verilog serialization.
//!
//! Word IR is emitted as synthesizable behavioral/structural Verilog where
//! possible. Mapped netlists are emitted as cell instances and explicit wires;
//! the writer never reconstructs hierarchy or changes connectivity.

use crate::FormatError;
use opto_ir::word;
use std::collections::BTreeSet;
use std::io::Write;

/// Write a Word IR module as synthesizable Verilog.
///
/// # Errors
///
/// Returns an IR error for invalid references, [`FormatError::Unsupported`] for
/// constructs outside the writer's representable subset, or an I/O error from
/// `out`.
#[allow(
    clippy::too_many_lines,
    reason = "the module writer preserves Verilog declaration and statement emission order"
)]
pub fn write_verilog<W: Write>(out: &mut W, module: &word::WordModule) -> Result<(), FormatError> {
    module.validate().map_err(FormatError::Word)?;
    let registered_signals = registered_signals(module)?;
    write_annotations(out, module, word::AnnotationTarget::Module, "")?;
    write!(out, "module {}(", escape_ident(module.name()))?;
    for (index, port) in module.ports().iter().enumerate() {
        if index != 0 {
            write!(out, ", ")?;
        }
        write!(out, "{}", escape_ident(module.name_str(port.name)))?;
    }
    writeln!(out, ");")?;
    for port in module.ports() {
        write_annotations(
            out,
            module,
            word::AnnotationTarget::Port(
                match module.signal(port.signal).map(|signal| signal.kind) {
                    Some(word::SignalKind::Port(port)) => port,
                    _ => {
                        return Err(FormatError::invalid(
                            "write_verilog: port signal does not retain its port identity",
                        ));
                    }
                },
            ),
            "  ",
        )?;
        let direction = match port.direction {
            word::PortDirection::Input => "input",
            word::PortDirection::Output => "output",
            word::PortDirection::Inout => "inout",
            word::PortDirection::Ref => "ref",
        };
        let storage = if registered_signals.contains(&port.signal) {
            " reg"
        } else {
            ""
        };
        if port.ty.width() > 1 {
            let range = declaration_range(module, port.signal, port.ty.width())?;
            writeln!(
                out,
                "  {}{} [{range}] {};",
                direction,
                storage,
                escape_ident(module.name_str(port.name))
            )?;
        } else {
            writeln!(
                out,
                "  {}{} {};",
                direction,
                storage,
                escape_ident(module.name_str(port.name))
            )?;
        }
    }
    for (index, signal) in module.signals().iter().enumerate() {
        let Some(name) = signal.name else {
            continue;
        };
        let signal_id = word::SignalId::from_index(index).map_err(|_| {
            FormatError::capacity("write_verilog: signal index exceeds RTL ID capacity")
        })?;
        let keyword = match signal.kind {
            _ if registered_signals.contains(&signal_id) => "reg",
            word::SignalKind::Wire => "wire",
            word::SignalKind::Register => "reg",
            word::SignalKind::ProcessLocal | word::SignalKind::Port(_) => continue,
        };
        write_annotations(out, module, word::AnnotationTarget::Signal(signal_id), "  ")?;
        if signal.ty.width() > 1 {
            let range = declaration_range(module, signal_id, signal.ty.width())?;
            writeln!(
                out,
                "  {} [{range}] {};",
                keyword,
                escape_ident(module.name_str(name))
            )?;
        } else {
            writeln!(
                out,
                "  {} {};",
                keyword,
                escape_ident(module.name_str(name))
            )?;
        }
    }
    for connect in module.connects() {
        match sequential_for_connect(module, connect)? {
            Some(SequentialForConnect::Register(register)) => {
                write_register(out, module, &connect.target, register)?;
                continue;
            }
            Some(SequentialForConnect::Latch(latch)) => {
                write_latch(out, module, &connect.target, latch)?;
                continue;
            }
            None => {}
        }
        writeln!(
            out,
            "  assign {} = {};",
            write_lvalue(module, &connect.target)?,
            write_value(module, connect.value)?
        )?;
    }
    for (index, instance) in module.instances().iter().enumerate() {
        let instance_id = word::InstId::from_index(index).map_err(|_| {
            FormatError::capacity("write_verilog: instance index exceeds RTL ID capacity")
        })?;
        write_annotations(
            out,
            module,
            word::AnnotationTarget::Instance(instance_id),
            "  ",
        )?;
        let connections = instance
            .connections
            .iter()
            .map(|connection| -> Result<String, FormatError> {
                Ok(format!(
                    ".{}({})",
                    escape_ident(module.name_str(connection.port)),
                    write_value(module, connection.value)?
                ))
            })
            .collect::<Result<Vec<_>, _>>()?
            .join(", ");
        writeln!(
            out,
            "  {} {}({});",
            escape_ident(module.name_str(instance.module)),
            escape_ident(module.name_str(instance.name)),
            connections
        )?;
    }
    writeln!(out, "endmodule")?;
    Ok(())
}

fn write_annotations<W: Write>(
    out: &mut W,
    module: &word::WordModule,
    target: word::AnnotationTarget,
    indent: &str,
) -> Result<(), FormatError> {
    for annotation in module
        .annotations()
        .iter()
        .filter(|annotation| annotation.target == target)
    {
        let name = escape_ident(module.name_str(annotation.name));
        let value = match &annotation.value {
            word::AnnotationValue::Integer {
                bits,
                width,
                signed,
            } => format!(
                "{}'{}b{}",
                width,
                if *signed { "s" } else { "" },
                module.name_str(*bits)
            ),
            word::AnnotationValue::String(value) => quote_verilog_string(module.name_str(*value))?,
            word::AnnotationValue::Other(value) => module.name_str(*value).to_string(),
        };
        writeln!(out, "{indent}(* {name} = {value} *)")?;
    }
    Ok(())
}

fn quote_verilog_string(value: &str) -> Result<String, FormatError> {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for character in value.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            character if character.is_ascii_graphic() || character == ' ' => {
                quoted.push(character);
            }
            _ => {
                return Err(FormatError::unsupported(
                    "write_verilog: annotation strings must contain printable ASCII",
                ));
            }
        }
    }
    quoted.push('"');
    Ok(quoted)
}

fn registered_signals(module: &word::WordModule) -> Result<BTreeSet<word::SignalId>, FormatError> {
    let mut signals = BTreeSet::new();
    for connect in module.connects() {
        if sequential_for_connect(module, connect)?.is_some() {
            signals.insert(connect.target.signal);
        }
    }
    Ok(signals)
}

enum SequentialForConnect<'a> {
    Register(&'a word::RegisterOp),
    Latch(&'a word::LatchOp),
}

fn sequential_for_connect<'a>(
    module: &'a word::WordModule,
    connect: &word::Connect,
) -> Result<Option<SequentialForConnect<'a>>, FormatError> {
    let value = module.value(connect.value).ok_or_else(|| {
        FormatError::invalid(format!(
            "write_verilog: unknown RTL value {:?}",
            connect.value
        ))
    })?;
    let word::ValueKind::Operation(operation_id) = value.kind else {
        return Ok(None);
    };
    let operation = module.operation(operation_id).ok_or_else(|| {
        FormatError::invalid(format!(
            "write_verilog: unknown RTL operation {operation_id:?}"
        ))
    })?;
    Ok(match &operation.kind {
        word::OpKind::Register(register) => Some(SequentialForConnect::Register(register)),
        word::OpKind::Latch(latch) => Some(SequentialForConnect::Latch(latch)),
        _ => None,
    })
}

fn write_register<W: Write>(
    out: &mut W,
    module: &word::WordModule,
    target: &word::LValue,
    register: &word::RegisterOp,
) -> Result<(), FormatError> {
    let edge = match register.edge {
        word::Edge::Pos => "posedge",
        word::Edge::Neg => "negedge",
    };
    let clock = write_value(module, register.clock)?;
    let mut sensitivity = format!("{edge} {clock}");
    for reset in register
        .resets
        .iter()
        .filter(|reset| reset.kind == word::ResetKind::Async)
    {
        let reset_edge = if reset.active_high {
            "posedge"
        } else {
            "negedge"
        };
        sensitivity.push_str(" or ");
        sensitivity.push_str(reset_edge);
        sensitivity.push(' ');
        sensitivity.push_str(&write_value(module, reset.value)?);
    }
    writeln!(out, "  always @({sensitivity}) begin")?;
    if register.resets.is_empty() {
        write_register_update(out, module, target, register, 4)?;
    } else {
        for (index, reset) in register.resets.iter().enumerate() {
            let condition = write_control_condition(module, reset.value, reset.active_high)?;
            let keyword = if index == 0 { "if" } else { "else if" };
            writeln!(out, "    {keyword} ({condition}) begin")?;
            writeln!(
                out,
                "      {} <= {};",
                write_lvalue(module, target)?,
                write_value(module, reset.reset_value)?
            )?;
            writeln!(out, "    end")?;
        }
        writeln!(out, "    else begin")?;
        write_register_update(out, module, target, register, 6)?;
        writeln!(out, "    end")?;
    }
    writeln!(out, "  end")?;
    Ok(())
}

fn write_register_update<W: Write>(
    out: &mut W,
    module: &word::WordModule,
    target: &word::LValue,
    register: &word::RegisterOp,
    indent: usize,
) -> Result<(), FormatError> {
    let padding = " ".repeat(indent);
    if let Some(enable) = register.enable {
        let condition = write_control_condition(module, enable.value, enable.active_high)?;
        writeln!(out, "{padding}if ({condition}) begin")?;
        writeln!(
            out,
            "{padding}  {} <= {};",
            write_lvalue(module, target)?,
            write_value(module, register.d)?
        )?;
        writeln!(out, "{padding}end")?;
    } else {
        writeln!(
            out,
            "{padding}{} <= {};",
            write_lvalue(module, target)?,
            write_value(module, register.d)?
        )?;
    }
    Ok(())
}

fn write_latch<W: Write>(
    out: &mut W,
    module: &word::WordModule,
    target: &word::LValue,
    latch: &word::LatchOp,
) -> Result<(), FormatError> {
    writeln!(out, "  always @* begin")?;
    if latch.resets.is_empty() {
        write_latch_update(out, module, target, latch, 4)?;
    } else {
        for (index, reset) in latch.resets.iter().enumerate() {
            let condition = write_control_condition(module, reset.value, reset.active_high)?;
            let keyword = if index == 0 { "if" } else { "else if" };
            writeln!(out, "    {keyword} ({condition}) begin")?;
            writeln!(
                out,
                "      {} <= {};",
                write_lvalue(module, target)?,
                write_value(module, reset.reset_value)?
            )?;
            writeln!(out, "    end")?;
        }
        writeln!(out, "    else begin")?;
        write_latch_update(out, module, target, latch, 6)?;
        writeln!(out, "    end")?;
    }
    writeln!(out, "  end")?;
    Ok(())
}

fn write_latch_update<W: Write>(
    out: &mut W,
    module: &word::WordModule,
    target: &word::LValue,
    latch: &word::LatchOp,
    indent: usize,
) -> Result<(), FormatError> {
    let padding = " ".repeat(indent);
    let condition = write_control_condition(module, latch.enable.value, latch.enable.active_high)?;
    writeln!(out, "{padding}if ({condition}) begin")?;
    writeln!(
        out,
        "{padding}  {} <= {};",
        write_lvalue(module, target)?,
        write_value(module, latch.d)?
    )?;
    writeln!(out, "{padding}end")?;
    Ok(())
}

fn write_control_condition(
    module: &word::WordModule,
    value: word::ValueId,
    active_high: bool,
) -> Result<String, FormatError> {
    let condition = write_value(module, value)?;
    Ok(if active_high {
        condition
    } else {
        format!("!({condition})")
    })
}

fn write_lvalue(module: &word::WordModule, lvalue: &word::LValue) -> Result<String, FormatError> {
    if let Some(dynamic) = lvalue.dynamic {
        let name = write_signal_ref(module, lvalue.signal, None)?;
        return Ok(format!(
            "{name}[{} +: {}]",
            write_value(module, dynamic.offset)?,
            dynamic.width.get()
        ));
    }
    write_signal_ref(module, lvalue.signal, lvalue.range)
}

fn write_signal_ref(
    module: &word::WordModule,
    signal_id: word::SignalId,
    range: Option<word::BitRange>,
) -> Result<String, FormatError> {
    let signal = module.signal(signal_id).ok_or_else(|| {
        FormatError::invalid(format!("write_verilog: unknown RTL signal {signal_id:?}"))
    })?;
    let name = signal.name.ok_or_else(|| {
        FormatError::invalid(format!(
            "write_verilog: RTL signal {signal_id:?} has no name"
        ))
    })?;
    let name = escape_ident(module.name_str(name));
    if let Some(range) = range {
        let (msb, lsb) = if let Some(source_range) = module
            .signal_simple_packed_range(signal_id)
            .map_err(FormatError::Word)?
            .filter(|range| range.left >= 0 && range.right >= 0)
        {
            (
                source_range
                    .index_from_lsb(range.msb)
                    .map_err(FormatError::Word)?,
                source_range
                    .index_from_lsb(range.lsb)
                    .map_err(FormatError::Word)?,
            )
        } else {
            (
                i32::try_from(range.msb).map_err(|_| {
                    FormatError::capacity("write_verilog: signal index exceeds i32 capacity")
                })?,
                i32::try_from(range.lsb).map_err(|_| {
                    FormatError::capacity("write_verilog: signal index exceeds i32 capacity")
                })?,
            )
        };
        if msb == lsb {
            Ok(format!("{name}[{msb}]"))
        } else {
            Ok(format!("{name}[{msb}:{lsb}]"))
        }
    } else {
        Ok(name)
    }
}

fn declaration_range(
    module: &word::WordModule,
    signal: word::SignalId,
    width: u32,
) -> Result<String, FormatError> {
    if let Some(range) = module
        .signal_simple_packed_range(signal)
        .map_err(FormatError::Word)?
        .filter(|range| range.left >= 0 && range.right >= 0)
    {
        Ok(format!("{}:{}", range.left, range.right))
    } else {
        Ok(format!("{}:0", width - 1))
    }
}

fn write_value(module: &word::WordModule, value_id: word::ValueId) -> Result<String, FormatError> {
    let value = module.value(value_id).ok_or_else(|| {
        FormatError::invalid(format!("write_verilog: unknown RTL value {value_id:?}"))
    })?;
    match &value.kind {
        word::ValueKind::Signal(reference) => write_signal_ref(
            module,
            reference.signal,
            signal_reference_range(module, *reference)?,
        ),
        word::ValueKind::Constant(bits) => Ok(format!("{}'b{}", value.ty.width(), bits)),
        word::ValueKind::Operation(op_id) => write_operation(module, *op_id),
    }
}

fn signal_reference_range(
    module: &word::WordModule,
    reference: word::SignalRef,
) -> Result<Option<word::BitRange>, FormatError> {
    let signal = module.signal(reference.signal).ok_or_else(|| {
        FormatError::invalid(format!(
            "write_verilog: unknown RTL signal {:?}",
            reference.signal
        ))
    })?;
    if reference.lsb == 0 && reference.width() == signal.ty.width() {
        return Ok(None);
    }
    let msb = reference
        .lsb
        .checked_add(reference.width() - 1)
        .ok_or_else(|| FormatError::capacity("write_verilog: signal reference range overflow"))?;
    Ok(Some(word::BitRange {
        msb,
        lsb: reference.lsb,
    }))
}

fn write_operation(module: &word::WordModule, op_id: word::OpId) -> Result<String, FormatError> {
    let operation = module.operation(op_id).ok_or_else(|| {
        FormatError::invalid(format!("write_verilog: unknown RTL operation {op_id:?}"))
    })?;
    match &operation.kind {
        word::OpKind::Unary { op, arg } => Ok(format!(
            "{}{}",
            unary_op_text(*op),
            write_wrapped_value(module, *arg)?
        )),
        word::OpKind::Binary { op, left, right } => Ok(format!(
            "{} {} {}",
            write_wrapped_value(module, *left)?,
            binary_op_text(*op),
            write_wrapped_value(module, *right)?
        )),
        word::OpKind::Mux {
            cond,
            then_value,
            else_value,
        } => Ok(format!(
            "{} ? {} : {}",
            write_wrapped_value(module, *cond)?,
            write_wrapped_value(module, *then_value)?,
            write_wrapped_value(module, *else_value)?
        )),
        word::OpKind::TriState { data, enable } => {
            let condition = write_wrapped_value(module, enable.value)?;
            let data = write_wrapped_value(module, *data)?;
            let width = module
                .value(operation.result)
                .ok_or_else(|| FormatError::invalid("write_verilog: tri-state result is absent"))?
                .ty
                .width();
            let high_impedance = format!("{width}'bz");
            if enable.active_high {
                Ok(format!("{condition} ? {data} : {high_impedance}"))
            } else {
                Ok(format!("{condition} ? {high_impedance} : {data}"))
            }
        }
        word::OpKind::Concat { parts } => Ok(format!(
            "{{{}}}",
            parts
                .iter()
                .map(|part| write_value(module, *part))
                .collect::<Result<Vec<_>, _>>()?
                .join(", ")
        )),
        word::OpKind::Extract { value, lsb, width } => {
            let msb = lsb
                .checked_add(width.get())
                .and_then(|end| end.checked_sub(1))
                .ok_or_else(|| FormatError::capacity("write_verilog: extract range overflow"))?;
            Ok(format!(
                "{}[{msb}:{lsb}]",
                write_wrapped_value(module, *value)?
            ))
        }
        word::OpKind::DynamicExtract {
            value,
            offset,
            width,
        } => Ok(format!(
            "{}[{} +: {}]",
            write_wrapped_value(module, *value)?,
            write_value(module, *offset)?,
            width.get()
        )),
        word::OpKind::DynamicInsert { .. } => Err(FormatError::unsupported(
            "write_verilog: dynamic insert must be lowered before structural emission",
        )),
        word::OpKind::Cast { .. } => Err(FormatError::unsupported(
            "write_verilog: cast op is not supported by the RTL writer",
        )),
        word::OpKind::Register(_) | word::OpKind::Latch(_) => Err(FormatError::unsupported(
            "write_verilog: sequential op is not supported by the structural RTL writer",
        )),
    }
}

fn write_wrapped_value(
    module: &word::WordModule,
    value_id: word::ValueId,
) -> Result<String, FormatError> {
    let value = module.value(value_id).ok_or_else(|| {
        FormatError::invalid(format!("write_verilog: unknown RTL value {value_id:?}"))
    })?;
    match value.kind {
        word::ValueKind::Signal(_) | word::ValueKind::Constant(_) => write_value(module, value_id),
        word::ValueKind::Operation(_) => Ok(format!("({})", write_value(module, value_id)?)),
    }
}

fn unary_op_text(op: word::UnaryOp) -> &'static str {
    match op {
        word::UnaryOp::LogicalNot => "!",
        word::UnaryOp::BitNot => "~",
        word::UnaryOp::ReductionAnd => "&",
        word::UnaryOp::ReductionOr => "|",
        word::UnaryOp::ReductionXor => "^",
    }
}

fn binary_op_text(op: word::BinaryOp) -> &'static str {
    match op {
        word::BinaryOp::Add => "+",
        word::BinaryOp::Sub => "-",
        word::BinaryOp::Mul => "*",
        word::BinaryOp::Div => "/",
        word::BinaryOp::Mod => "%",
        word::BinaryOp::BitAnd => "&",
        word::BinaryOp::BitOr => "|",
        word::BinaryOp::BitXor => "^",
        word::BinaryOp::LogicalAnd => "&&",
        word::BinaryOp::LogicalOr => "||",
        word::BinaryOp::Eq => "==",
        word::BinaryOp::Ne => "!=",
        word::BinaryOp::Lt => "<",
        word::BinaryOp::Le => "<=",
        word::BinaryOp::Gt => ">",
        word::BinaryOp::Ge => ">=",
        word::BinaryOp::Shl => "<<",
        word::BinaryOp::Shr => ">>",
        word::BinaryOp::Ashr => ">>>",
    }
}

/// Write a mapped netlist as structural Verilog.
///
/// Port aliases are selected before internal net names so each mapped net has
/// one deterministic expression. Cells are emitted in typed-ID order.
///
/// # Errors
///
/// Returns an error for invalid mapped references, unrepresentable connection
/// shapes, capacity overflow, or a failure from `out`.
#[allow(
    clippy::too_many_lines,
    reason = "the structural writer keeps name selection and connectivity emission in one deterministic pass"
)]
pub fn write_mapped_verilog<W: Write>(
    out: &mut W,
    netlist: &opto_ir::mapped::MappedNetlist,
) -> Result<(), FormatError> {
    use opto_ir::mapped::{ConnectionSignal, PortDirection, PortId};

    validate_mapped_port_aliases(netlist)?;
    let port_names = netlist
        .ports()
        .iter()
        .enumerate()
        .map(|(index, _)| {
            let id = PortId::from_index(index).map_err(FormatError::Mapped)?;
            netlist.port_name(id).map(escape_ident).ok_or_else(|| {
                FormatError::invalid(format!("write_verilog: mapped port {index} has no name"))
            })
        })
        .collect::<Result<Vec<_>, FormatError>>()?;
    writeln!(
        out,
        "module {}({});",
        escape_ident(netlist.name()),
        port_names.join(", ")
    )?;

    let mut net_expressions = (0..netlist.net_slot_count())
        .map(|index| format!("_n{index}"))
        .collect::<Vec<_>>();
    let mut port_primaries = vec![None::<(PortDirection, String)>; netlist.net_slot_count()];
    let mut port_bindings = Vec::<(PortDirection, String, usize)>::new();
    let mut port_nets = BTreeSet::new();
    let mut used_net_names = BTreeSet::new();
    for (index, port) in netlist.ports().iter().enumerate() {
        let id = PortId::from_index(index).map_err(FormatError::Mapped)?;
        let name = netlist.port_name(id).ok_or_else(|| {
            FormatError::invalid(format!("write_verilog: mapped port {index} has no name"))
        })?;
        used_net_names.insert(name.to_string());
        let nets = netlist.port_nets(id).ok_or_else(|| {
            FormatError::invalid(format!(
                "write_verilog: mapped port {index} has invalid net range"
            ))
        })?;
        let direction = match port.direction {
            PortDirection::Input => "input",
            PortDirection::Output => "output",
            PortDirection::Inout => "inout",
        };
        if nets.len() == 1 {
            writeln!(out, "  {direction} {};", escape_ident(name))?;
        } else {
            writeln!(
                out,
                "  {direction} [{}:0] {};",
                nets.len() - 1,
                escape_ident(name)
            )?;
        }
        for (bit, net) in nets.iter().enumerate() {
            port_nets.insert(net.index());
            let expression = if nets.len() == 1 {
                escape_ident(name)
            } else {
                format!("{}[{bit}]", escape_ident(name))
            };
            port_bindings.push((port.direction, expression.clone(), net.index()));
            let primary = &mut port_primaries[net.index()];
            if primary.as_ref().is_none_or(|(direction, _)| {
                *direction == PortDirection::Output && port.direction != PortDirection::Output
            }) {
                *primary = Some((port.direction, expression));
            }
        }
    }
    for (index, primary) in port_primaries.iter().enumerate() {
        if let Some((_, expression)) = primary {
            net_expressions[index].clone_from(expression);
        }
    }

    for net in netlist.net_ids() {
        let index = net.index();
        if port_nets.contains(&index) {
            continue;
        }
        let base_name = netlist
            .net_name(net)
            .map_or_else(|| format!("_n{index}"), ToString::to_string);
        let mut unique_name = base_name.clone();
        let mut disambiguator = 1u32;
        while !used_net_names.insert(unique_name.clone()) {
            unique_name = format!("{base_name}_{disambiguator}");
            disambiguator = disambiguator.checked_add(1).ok_or_else(|| {
                FormatError::capacity("write_verilog: exhausted net name disambiguators")
            })?;
        }
        let name = escape_ident(&unique_name);
        net_expressions[index].clone_from(&name);
        writeln!(out, "  wire {name};")?;
    }

    for (direction, expression, net) in port_bindings {
        if direction == PortDirection::Output && expression != net_expressions[net] {
            writeln!(out, "  assign {expression} = {};", net_expressions[net])?;
        }
    }

    for &(net, value) in netlist.constant_drivers() {
        writeln!(
            out,
            "  assign {} = 1'b{};",
            net_expressions[net.index()],
            u8::from(value)
        )?;
    }
    for cell in netlist.cell_ids() {
        let index = cell.index();
        let cell_type = netlist.cell_type(cell).ok_or_else(|| {
            FormatError::invalid(format!("write_verilog: mapped cell {index} has no type"))
        })?;
        let name = netlist.cell_name(cell).ok_or_else(|| {
            FormatError::invalid(format!("write_verilog: mapped cell {index} has no name"))
        })?;
        let connections = netlist
            .connections(cell)
            .ok_or_else(|| {
                FormatError::invalid(format!(
                    "write_verilog: mapped cell {index} has invalid pin range"
                ))
            })?
            .iter()
            .map(|connection| {
                let pin = netlist.pin_name(connection).ok_or_else(|| {
                    FormatError::invalid(format!(
                        "write_verilog: mapped cell {index} has unnamed pin"
                    ))
                })?;
                let signal = match connection.signal {
                    ConnectionSignal::Net(net) => net_expressions[net.index()].clone(),
                    ConnectionSignal::Constant(value) => format!("1'b{}", u8::from(value)),
                };
                Ok(format!(".{}({signal})", escape_ident(pin)))
            })
            .collect::<Result<Vec<_>, FormatError>>()?;
        writeln!(
            out,
            "  {} {}({});",
            escape_ident(cell_type),
            escape_ident(name),
            connections.join(", ")
        )?;
    }
    for instance in netlist.design_instance_ids() {
        let index = instance.index();
        let module = netlist.design_instance_module(instance).ok_or_else(|| {
            FormatError::invalid(format!(
                "write_verilog: mapped design instance {index} has no module"
            ))
        })?;
        let name = netlist.design_instance_name(instance).ok_or_else(|| {
            FormatError::invalid(format!(
                "write_verilog: mapped design instance {index} has no name"
            ))
        })?;
        let connections = netlist
            .design_instance_connections(instance)
            .ok_or_else(|| {
                FormatError::invalid(format!(
                    "write_verilog: mapped design instance {index} has an invalid connection range"
                ))
            })?
            .iter()
            .map(|connection| {
                let port = netlist.design_connection_port(connection).ok_or_else(|| {
                    FormatError::invalid(format!(
                        "write_verilog: mapped design instance {index} has an unnamed port"
                    ))
                })?;
                let signals = netlist.design_connection_signals(connection).ok_or_else(|| {
                    FormatError::invalid(format!(
                        "write_verilog: mapped design instance {index} has an invalid signal range"
                    ))
                })?;
                let mut expressions = signals
                    .iter()
                    .map(|signal| match signal {
                        ConnectionSignal::Net(net) => net_expressions[net.index()].clone(),
                        ConnectionSignal::Constant(value) => format!("1'b{}", u8::from(*value)),
                    })
                    .collect::<Vec<_>>();
                let signal = if let [expression] = expressions.as_slice() {
                    expression.clone()
                } else {
                    expressions.reverse();
                    format!("{{{}}}", expressions.join(", "))
                };
                Ok(format!(".{}({signal})", escape_ident(port)))
            })
            .collect::<Result<Vec<_>, FormatError>>()?;
        writeln!(
            out,
            "  {} {}({});",
            escape_ident(module),
            escape_ident(name),
            connections.join(", ")
        )?;
    }
    writeln!(out, "endmodule")?;
    Ok(())
}

fn validate_mapped_port_aliases(
    netlist: &opto_ir::mapped::MappedNetlist,
) -> Result<(), FormatError> {
    use opto_ir::mapped::{PortDirection, PortId};

    let mut external_drivers = vec![0u8; netlist.net_slot_count()];
    for (index, port) in netlist.ports().iter().enumerate() {
        if port.direction == PortDirection::Output {
            continue;
        }
        let id = PortId::from_index(index).map_err(FormatError::Mapped)?;
        let nets = netlist.port_nets(id).ok_or_else(|| {
            FormatError::invalid(format!(
                "write_verilog: mapped port {index} has an invalid net range"
            ))
        })?;
        for net in nets {
            let drivers = external_drivers.get_mut(net.index()).ok_or_else(|| {
                FormatError::invalid(format!(
                    "write_verilog: mapped port {index} references unknown net {net:?}"
                ))
            })?;
            *drivers = drivers.saturating_add(1);
            if *drivers > 1 {
                return Err(FormatError::unsupported(format!(
                    "write_verilog: mapped net {net:?} is shared by multiple input/inout ports"
                )));
            }
        }
    }
    Ok(())
}

fn escape_ident(name: &str) -> String {
    if name
        .chars()
        .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
        && !name.chars().next().is_some_and(|ch| ch.is_ascii_digit())
        && !is_verilog_keyword(name)
    {
        name.to_string()
    } else {
        format!("\\{name} ")
    }
}

fn is_verilog_keyword(name: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "accept_on",
        "alias",
        "always",
        "always_comb",
        "always_ff",
        "always_latch",
        "and",
        "assert",
        "assign",
        "assume",
        "automatic",
        "before",
        "begin",
        "bind",
        "bins",
        "binsof",
        "bit",
        "break",
        "buf",
        "bufif0",
        "bufif1",
        "byte",
        "case",
        "casex",
        "casez",
        "cell",
        "chandle",
        "checker",
        "class",
        "clocking",
        "cmos",
        "config",
        "const",
        "constraint",
        "context",
        "continue",
        "cover",
        "covergroup",
        "coverpoint",
        "cross",
        "deassign",
        "default",
        "defparam",
        "design",
        "disable",
        "dist",
        "do",
        "edge",
        "else",
        "end",
        "endchecker",
        "endclass",
        "endclocking",
        "endconfig",
        "endfunction",
        "endgenerate",
        "endgroup",
        "endinterface",
        "endmodule",
        "endpackage",
        "endprimitive",
        "endprogram",
        "endproperty",
        "endsequence",
        "endspecify",
        "endtable",
        "endtask",
        "enum",
        "event",
        "eventually",
        "expect",
        "export",
        "extends",
        "extern",
        "final",
        "first_match",
        "for",
        "force",
        "foreach",
        "forever",
        "fork",
        "forkjoin",
        "function",
        "generate",
        "genvar",
        "global",
        "highz0",
        "highz1",
        "if",
        "iff",
        "ifnone",
        "ignore_bins",
        "illegal_bins",
        "implements",
        "implies",
        "import",
        "incdir",
        "include",
        "initial",
        "inout",
        "input",
        "inside",
        "int",
        "integer",
        "interconnect",
        "intersect",
        "join",
        "join_any",
        "join_none",
        "large",
        "let",
        "liblist",
        "library",
        "local",
        "localparam",
        "logic",
        "longint",
        "macromodule",
        "matches",
        "medium",
        "modport",
        "module",
        "nand",
        "negedge",
        "nettype",
        "new",
        "nexttime",
        "nmos",
        "nor",
        "noshowcancelled",
        "not",
        "notif0",
        "notif1",
        "null",
        "or",
        "output",
        "package",
        "packed",
        "parameter",
        "pmos",
        "posedge",
        "primitive",
        "priority",
        "program",
        "property",
        "protected",
        "pull0",
        "pull1",
        "pulldown",
        "pullup",
        "pure",
        "rand",
        "randc",
        "randcase",
        "randsequence",
        "rcmos",
        "real",
        "realtime",
        "ref",
        "reg",
        "reject_on",
        "release",
        "repeat",
        "restrict",
        "return",
        "rnmos",
        "rpmos",
        "rtran",
        "rtranif0",
        "rtranif1",
        "s_always",
        "s_eventually",
        "s_nexttime",
        "s_until",
        "s_until_with",
        "scalared",
        "sequence",
        "shortint",
        "shortreal",
        "showcancelled",
        "signed",
        "small",
        "solve",
        "specify",
        "specparam",
        "static",
        "string",
        "strong",
        "strong0",
        "strong1",
        "struct",
        "super",
        "supply0",
        "supply1",
        "sync_accept_on",
        "sync_reject_on",
        "table",
        "tagged",
        "task",
        "this",
        "throughout",
        "time",
        "timeprecision",
        "timeunit",
        "tran",
        "tranif0",
        "tranif1",
        "tri",
        "tri0",
        "tri1",
        "triand",
        "trior",
        "trireg",
        "type",
        "typedef",
        "union",
        "unique",
        "unique0",
        "unsigned",
        "use",
        "uwire",
        "var",
        "vectored",
        "virtual",
        "void",
        "wait",
        "wait_order",
        "wand",
        "weak",
        "weak0",
        "weak1",
        "while",
        "wildcard",
        "wire",
        "with",
        "within",
        "wor",
        "xnor",
        "xor",
    ];
    KEYWORDS.contains(&name)
}

#[cfg(test)]
mod word_tests {
    use super::*;
    use opto_ir::ConstBits;
    use opto_ir::word::{
        AnnotationTarget, AnnotationValueSpec, DefinitionKind, Enable, LValue, LogicStateKind,
        PortDirection, SourceSpan, UnaryOp, WordModule, WordType,
    };

    fn verilog_text(module: &WordModule) -> String {
        let mut output = Vec::new();
        write_verilog(&mut output, module).unwrap();
        String::from_utf8(output).unwrap()
    }

    #[test]
    fn writes_structural_assigns_and_instance_connections() {
        let bit = WordType::bits(1).unwrap();
        let span = SourceSpan::default();
        let mut module = WordModule::new("top");
        let a = module
            .add_port("a", PortDirection::Input, bit, span.clone())
            .unwrap();
        let y = module
            .add_port("y", PortDirection::Output, bit, span.clone())
            .unwrap();
        let n = module.add_wire("n", bit, span.clone()).unwrap();
        let a_value = module
            .read_signal(module.port(a).unwrap().signal, span.clone())
            .unwrap();
        let not_a = module
            .unary(UnaryOp::BitNot, a_value, span.clone())
            .unwrap();
        module
            .connect(LValue::signal(n), not_a, span.clone())
            .unwrap();
        let n_value = module.read_signal(n, span.clone()).unwrap();
        let y_value = module
            .read_signal(module.port(y).unwrap().signal, span.clone())
            .unwrap();
        module
            .add_instance(
                "u_child",
                "child",
                vec![
                    ("i".to_string(), n_value, span.clone()),
                    ("o".to_string(), y_value, span.clone()),
                ],
                span,
            )
            .unwrap();

        let text = verilog_text(&module);
        assert!(text.contains("assign n = ~a;"));
        assert!(text.contains("child u_child(.i(n), .o(y));"));
    }

    #[test]
    fn writes_sized_constants() {
        let mut module = WordModule::new("top");
        let ty = WordType::new(4, false, LogicStateKind::FourState).unwrap();
        let span = SourceSpan::default();
        let y = module
            .add_port("y", PortDirection::Output, ty, span.clone())
            .unwrap();
        let value = module
            .constant(ConstBits::from_bin_str("1010").unwrap(), ty, span.clone())
            .unwrap();
        module
            .connect(LValue::signal(module.port(y).unwrap().signal), value, span)
            .unwrap();

        assert!(verilog_text(&module).contains("assign y = 4'b1010;"));
    }

    #[test]
    fn writes_explicit_tri_state_operations() {
        let mut module = WordModule::new("top");
        let vector = WordType::new(4, false, LogicStateKind::FourState).unwrap();
        let bit = WordType::new(1, false, LogicStateKind::FourState).unwrap();
        let span = SourceSpan::default();
        let data = module
            .add_port("data", PortDirection::Input, vector, span.clone())
            .unwrap();
        let enable = module
            .add_port("enable", PortDirection::Input, bit, span.clone())
            .unwrap();
        let output = module
            .add_port("y", PortDirection::Output, vector, span.clone())
            .unwrap();
        let data = module
            .read_signal(module.port(data).unwrap().signal, span.clone())
            .unwrap();
        let enable = module
            .read_signal(module.port(enable).unwrap().signal, span.clone())
            .unwrap();
        let driver = module
            .tri_state(
                data,
                Enable {
                    value: enable,
                    active_high: true,
                },
                span.clone(),
            )
            .unwrap();
        module
            .connect(
                LValue::signal(module.port(output).unwrap().signal),
                driver,
                span,
            )
            .unwrap();

        assert!(verilog_text(&module).contains("assign y = enable ? data : 4'bz;"));
    }

    #[test]
    fn word_area_report_counts_port_and_net_bits() {
        let mut module = WordModule::new("top");
        let vector = WordType::new(2, false, LogicStateKind::FourState).unwrap();
        let span = SourceSpan::default();
        let input = module
            .add_port("input_bus", PortDirection::Input, vector, span.clone())
            .unwrap();
        let output = module
            .add_port("output_bus", PortDirection::Output, vector, span.clone())
            .unwrap();
        let input_value = module
            .read_signal(module.port(input).unwrap().signal, span.clone())
            .unwrap();
        module
            .connect(
                LValue::signal(module.port(output).unwrap().signal),
                input_value,
                span,
            )
            .unwrap();

        let report =
            crate::report_area(&module, &crate::AreaReportContext::default()).render_plain();
        assert!(report.contains("Number of ports: 4"));
        assert!(report.contains("Number of nets: 2"));
    }

    #[test]
    fn writes_evaluated_module_annotations_and_black_box_interfaces() {
        let mut module = WordModule::new("macro");
        module
            .add_annotation(
                AnnotationTarget::Module,
                "black_box",
                AnnotationValueSpec::Integer {
                    bits: ConstBits::from_bin_str("1").unwrap(),
                    signed: false,
                },
                SourceSpan::default(),
            )
            .unwrap();
        module
            .add_annotation(
                AnnotationTarget::Module,
                "implementation",
                AnnotationValueSpec::String(r#"memory "macro""#.to_string()),
                SourceSpan::default(),
            )
            .unwrap();
        module
            .add_port(
                "a",
                PortDirection::Input,
                WordType::bits(1).unwrap(),
                SourceSpan::default(),
            )
            .unwrap();
        module.set_definition_kind(DefinitionKind::BlackBox);

        let mut output = Vec::new();
        write_verilog(&mut output, &module).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.starts_with(concat!(
            "(* black_box = 1'b1 *)\n",
            r#"(* implementation = "memory \"macro\"" *)"#,
            "\nmodule macro(a);",
        )));
        assert!(output.ends_with("endmodule\n"));
    }

    #[test]
    fn writes_structural_object_annotations_before_their_declarations() {
        let mut module = WordModule::new("top");
        let port = module
            .add_port(
                "a",
                PortDirection::Input,
                WordType::bits(1).unwrap(),
                SourceSpan::default(),
            )
            .unwrap();
        let signal = module
            .add_wire("n", WordType::bits(1).unwrap(), SourceSpan::default())
            .unwrap();
        let instance = module
            .add_instance("u", "macro", Vec::new(), SourceSpan::default())
            .unwrap();
        for (target, name) in [
            (AnnotationTarget::Port(port), "port_tag"),
            (AnnotationTarget::Signal(signal), "net_tag"),
            (AnnotationTarget::Instance(instance), "instance_tag"),
        ] {
            module
                .add_annotation(
                    target,
                    name,
                    AnnotationValueSpec::Integer {
                        bits: ConstBits::from_bin_str("1").unwrap(),
                        signed: false,
                    },
                    SourceSpan::default(),
                )
                .unwrap();
        }

        let mut output = Vec::new();
        write_verilog(&mut output, &module).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("  (* port_tag = 1'b1 *)\n  input a;"));
        assert!(output.contains("  (* net_tag = 1'b1 *)\n  wire n;"));
        assert!(output.contains("  (* instance_tag = 1'b1 *)\n  macro u();"));
    }
}

#[cfg(test)]
mod mapped_tests {
    use super::*;
    use opto_ir::RevisionId;
    use opto_ir::mapped::{ConnectionSignal, MappedBuilder, PortDirection, RegionDelta};

    fn mapped_verilog_text(
        netlist: &opto_ir::mapped::MappedNetlist,
    ) -> Result<String, FormatError> {
        let mut output = Vec::new();
        write_mapped_verilog(&mut output, netlist)?;
        Ok(String::from_utf8(output).expect("Verilog writer only emits UTF-8 text"))
    }

    #[test]
    fn mapped_writer_preserves_aliased_input_to_output_ports() {
        let mut builder = MappedBuilder::new("top", RevisionId::INITIAL).unwrap();
        let net = builder.add_net(Some("a")).unwrap();
        builder.add_port("a", PortDirection::Input, &[net]).unwrap();
        builder
            .add_port("y", PortDirection::Output, &[net])
            .unwrap();
        let text = mapped_verilog_text(&builder.freeze().unwrap()).unwrap();

        assert!(text.contains("assign y = a;"), "{text}");
    }

    #[test]
    fn mapped_writer_rejects_multiple_external_drivers_on_one_net() {
        let mut builder = MappedBuilder::new("top", RevisionId::INITIAL).unwrap();
        let net = builder.add_net(Some("shared")).unwrap();
        builder.add_port("a", PortDirection::Input, &[net]).unwrap();
        builder.add_port("b", PortDirection::Input, &[net]).unwrap();
        let error = mapped_verilog_text(&builder.freeze().unwrap()).unwrap_err();

        assert!(error.to_string().contains("multiple input/inout ports"));
    }

    #[test]
    fn writer_escapes_verilog_and_system_verilog_keywords() {
        assert_eq!(escape_ident("module"), "\\module ");
        assert_eq!(escape_ident("always_ff"), "\\always_ff ");
        assert_eq!(escape_ident("ordinary_name"), "ordinary_name");
    }

    #[test]
    fn mapped_writer_indexes_sparse_stable_net_ids_by_slot() {
        let mut builder = MappedBuilder::new("top", RevisionId::INITIAL).unwrap();
        let input = builder.add_net(Some("a")).unwrap();
        let dead = builder.add_net(Some("dead")).unwrap();
        let output = builder.add_net(Some("y")).unwrap();
        builder
            .add_port("a", PortDirection::Input, &[input])
            .unwrap();
        builder
            .add_port("y", PortDirection::Output, &[output])
            .unwrap();
        builder
            .add_cell(
                "U0",
                "BUF",
                None,
                &[
                    ("A".to_string(), None, ConnectionSignal::Net(input)),
                    ("Y".to_string(), None, ConnectionSignal::Net(output)),
                ],
            )
            .unwrap();
        let mut netlist = builder.freeze().unwrap();
        let snapshot = netlist.snapshot_region([], [dead]).unwrap();
        let mut delta = RegionDelta::new(snapshot);
        delta.remove_net(dead).unwrap();
        netlist.apply_region_delta(delta).unwrap();

        assert_eq!(netlist.net_count(), 2);
        assert_eq!(netlist.net_slot_count(), 3);
        let text = mapped_verilog_text(&netlist).unwrap();
        assert!(text.contains("BUF U0(.A(a), .Y(y));"), "{text}");
        assert!(!text.contains("wire dead"), "{text}");
    }

    #[test]
    fn mapped_writer_emits_named_cell_pins_on_canonical_nets() {
        let mut builder = MappedBuilder::new("top", RevisionId::INITIAL).unwrap();
        let a = builder.add_net(Some("a")).unwrap();
        let y = builder.add_net(Some("y")).unwrap();
        builder.add_port("a", PortDirection::Input, &[a]).unwrap();
        builder.add_port("y", PortDirection::Output, &[y]).unwrap();
        builder
            .add_cell(
                "U1",
                "INVX1",
                Some(0),
                &[
                    ("A".to_string(), Some(0), ConnectionSignal::Net(a)),
                    ("Y".to_string(), Some(1), ConnectionSignal::Net(y)),
                ],
            )
            .unwrap();
        let text = mapped_verilog_text(&builder.freeze().unwrap()).unwrap();

        assert!(text.contains("INVX1 U1(.A(a), .Y(y));"), "{text}");
    }
}
