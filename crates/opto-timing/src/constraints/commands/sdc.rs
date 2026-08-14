// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! `write_sdc` rendering for the constraint context.

use super::super::PortValueSlots;
use crate::{
    CaseAnalysisValue, Clock, ClockId, DelayType, DesignRuleKind, DesignRuleScope, EdgeSelection,
    ExceptionCorner, IoDelay, PathException, PathExceptionKind, PortId, TimingContext,
    TimingDerateKind, TimingEdge, TimingEndpoint,
};
use std::collections::{BTreeMap, BTreeSet};

impl TimingContext {
    /// Renders the live typed constraint state as executable SDC.
    ///
    /// The renderer emits stable object collections and preserves exact option
    /// selections, including non-default values that are numerically close to
    /// their defaults.
    ///
    /// # Errors
    ///
    /// Returns [`ConstraintError::UnresolvedSdcObject`] when `resolve` cannot
    /// name a referenced live object, or [`ConstraintError::ClockNotFound`]
    /// when constraint storage refers to a clock that is no longer live.
    pub fn write_sdc(
        &self,
        mut resolve: impl FnMut(opto_db::AnyObjectId) -> Option<String>,
    ) -> Result<String, crate::TimingError> {
        let mut output = String::new();
        let mut object_expression =
            |objects: &[opto_db::AnyObjectId]| -> Result<String, crate::TimingError> {
                sdc_collection_expression(objects, &mut resolve)
            };
        let clock_name = |id: ClockId| {
            self.clock(id)
                .map(|clock| clock.name.clone())
                .ok_or(crate::ConstraintError::ClockNotFound { id })
        };

        write_clocks(self, &mut output, &clock_name, &mut object_expression)?;

        write_environment_constraints(self, &mut output, &mut object_expression)?;
        write_clock_properties(self, &mut output);
        write_clock_uncertainties(self, &mut output, &clock_name)?;

        write_io_delays(self, &mut output, &mut object_expression)?;
        write_logic_constraints(self, &mut output, &mut object_expression)?;

        write_timing_derates(self, &mut output);

        write_clock_groups(self, &mut output, &clock_name)?;
        write_path_exceptions(self, &mut output, &mut object_expression)?;
        write_design_rules(self, &mut output, &mut object_expression)?;
        Ok(output)
    }
}

fn write_clocks(
    context: &TimingContext,
    output: &mut String,
    clock_name: &impl Fn(ClockId) -> Result<String, crate::ConstraintError>,
    object_expression: &mut impl FnMut(&[opto_db::AnyObjectId]) -> Result<String, crate::TimingError>,
) -> Result<(), crate::TimingError> {
    let mut occupied_sources = BTreeSet::new();
    for clock in &context.clocks {
        let needs_add = clock
            .sources
            .iter()
            .any(|source| occupied_sources.contains(source));
        if let Some(generated) = &clock.generated {
            let master = clock_name(generated.master)?;
            let source = object_expression(&[generated.source.erase()])?;
            let targets = object_expression(
                &clock
                    .sources
                    .iter()
                    .map(|port| port.erase())
                    .collect::<Vec<_>>(),
            )?;
            let mut line = format!(
                "create_generated_clock -name {} -source {} -master_clock [get_clocks {}]",
                tcl_quote(&clock.name),
                source,
                tcl_quote(&master)
            );
            if let Some(value) = generated.divide_by {
                append_format(&mut line, format_args!(" -divide_by {value}"));
            }
            if let Some(value) = generated.multiply_by {
                append_format(&mut line, format_args!(" -multiply_by {value}"));
            }
            if let Some(value) = generated.duty_cycle {
                append_format(
                    &mut line,
                    format_args!(" -duty_cycle {}", sdc_number(value)),
                );
            }
            if generated.invert {
                line.push_str(" -invert");
            }
            if let Some([first, second, third]) = generated.edges {
                append_format(
                    &mut line,
                    format_args!(" -edges [list {first} {second} {third}]"),
                );
            }
            if let Some(shifts) = generated.edge_shift {
                append_format(
                    &mut line,
                    format_args!(
                        " -edge_shift [list {} {} {}]",
                        sdc_number(shifts[0]),
                        sdc_number(shifts[1]),
                        sdc_number(shifts[2])
                    ),
                );
            }
            if generated.combinational {
                line.push_str(" -combinational");
            }
            if needs_add {
                line.push_str(" -add");
            }
            if !generated.comment.is_empty() {
                append_format(
                    &mut line,
                    format_args!(" -comment {}", tcl_quote(&generated.comment)),
                );
            }
            line.push(' ');
            line.push_str(&targets);
            push_sdc_line(output, &line);
        } else {
            write_primary_clock(output, clock, needs_add, object_expression)?;
        }
        occupied_sources.extend(clock.sources.iter().copied());
    }
    Ok(())
}

fn write_environment_constraints(
    context: &TimingContext,
    output: &mut String,
    object_expression: &mut impl FnMut(&[opto_db::AnyObjectId]) -> Result<String, crate::TimingError>,
) -> Result<(), crate::TimingError> {
    for (&port, slots) in &context.input_transitions {
        let object = object_expression(&[port.erase()])?;
        write_edge_delay_slots(output, "set_input_transition", slots, &object);
    }
    for (&port, slots) in &context.loads {
        let object = object_expression(&[port.erase()])?;
        write_edge_delay_slots(output, "set_load", slots, &object);
    }
    for (&endpoint, slots) in &context.resistances {
        let object = object_expression(&[endpoint.object_id()])?;
        match endpoint {
            TimingEndpoint::Port(_) => {
                write_edge_delay_slots(output, "set_drive", slots, &object);
            }
            TimingEndpoint::Net(_) => {
                for delay_type in [DelayType::Max, DelayType::Min] {
                    if let Some(value) = slots.value(TimingEdge::Rise, delay_type) {
                        push_sdc_line(
                            output,
                            &format!(
                                "set_resistance -{} {} {}",
                                delay_type_name(delay_type),
                                sdc_number(value),
                                object
                            ),
                        );
                    }
                }
            }
            TimingEndpoint::Cell(_) | TimingEndpoint::Pin(_) | TimingEndpoint::Clock(_) => {
                unreachable!("only ports and nets store resistance constraints")
            }
        }
    }
    Ok(())
}

fn write_clock_properties(context: &TimingContext, output: &mut String) {
    for clock in &context.clocks {
        let object = format!("[get_clocks {}]", tcl_quote(&clock.name));
        for delay_type in [DelayType::Max, DelayType::Min] {
            for edge in TimingEdge::ALL {
                if let Some(value) = clock.transitions[delay_type.index()][edge.index()] {
                    push_sdc_line(
                        output,
                        &format!(
                            "set_clock_transition -{} -{} {} {}",
                            edge_name_sdc(edge),
                            delay_type_name(delay_type),
                            sdc_number(value),
                            object
                        ),
                    );
                }
                for (early_late, early_late_name) in [(0, "early"), (1, "late")] {
                    if let Some(value) =
                        clock.source_latencies[delay_type.index()][early_late][edge.index()]
                    {
                        push_sdc_line(
                            output,
                            &format!(
                                "set_clock_latency -source -{} -{} -{} {} {}",
                                edge_name_sdc(edge),
                                delay_type_name(delay_type),
                                early_late_name,
                                sdc_number(value),
                                object
                            ),
                        );
                    }
                }
                if let Some(value) = clock.network_latencies[delay_type.index()][edge.index()] {
                    push_sdc_line(
                        output,
                        &format!(
                            "set_clock_latency -{} -{} {} {}",
                            edge_name_sdc(edge),
                            delay_type_name(delay_type),
                            sdc_number(value),
                            object
                        ),
                    );
                }
            }
        }
        if clock.propagated {
            push_sdc_line(output, &format!("set_propagated_clock {object}"));
        }
    }
}

fn write_clock_uncertainties(
    context: &TimingContext,
    output: &mut String,
    clock_name: &impl Fn(ClockId) -> Result<String, crate::ConstraintError>,
) -> Result<(), crate::TimingError> {
    for (&key, &value) in &context.clock_uncertainties {
        let from = clock_name(key.from)?;
        let to = clock_name(key.to)?;
        push_sdc_line(
            output,
            &format!(
                "set_clock_uncertainty {} [get_clocks {}] {} [get_clocks {}] -{} {}",
                edge_selector_option(key.from_edge, "from"),
                tcl_quote(&from),
                edge_selector_option(key.to_edge, "to"),
                tcl_quote(&to),
                delay_type_corner_name(key.delay_type),
                sdc_number(value)
            ),
        );
    }
    Ok(())
}

fn write_io_delays(
    context: &TimingContext,
    output: &mut String,
    object_expression: &mut impl FnMut(&[opto_db::AnyObjectId]) -> Result<String, crate::TimingError>,
) -> Result<(), crate::TimingError> {
    for (&port, rows) in &context.input_delays {
        write_io_delay_rows(
            output,
            "set_input_delay",
            port,
            rows,
            context,
            object_expression,
        )?;
    }
    for (&port, rows) in &context.output_delays {
        write_io_delay_rows(
            output,
            "set_output_delay",
            port,
            rows,
            context,
            object_expression,
        )?;
    }
    Ok(())
}

fn write_logic_constraints(
    context: &TimingContext,
    output: &mut String,
    object_expression: &mut impl FnMut(&[opto_db::AnyObjectId]) -> Result<String, crate::TimingError>,
) -> Result<(), crate::TimingError> {
    for (&endpoint, &value) in &context.case_analysis {
        push_sdc_line(
            output,
            &format!(
                "set_case_analysis {} {}",
                case_analysis_name(value),
                object_expression(&[endpoint.object_id()])?
            ),
        );
    }
    for disabled in &context.disabled_timing {
        let mut line = "set_disable_timing".to_string();
        if matches!(disabled.target, TimingEndpoint::Cell(_)) {
            if let Some(from) = &disabled.from {
                append_format(&mut line, format_args!(" -from {}", tcl_quote(from)));
            }
            if let Some(to) = &disabled.to {
                append_format(&mut line, format_args!(" -to {}", tcl_quote(to)));
            }
        }
        line.push(' ');
        line.push_str(&object_expression(&[disabled.target.object_id()])?);
        push_sdc_line(output, &line);
    }
    Ok(())
}

fn write_timing_derates(context: &TimingContext, output: &mut String) {
    for kind in [
        TimingDerateKind::NetDelay,
        TimingDerateKind::CellDelay,
        TimingDerateKind::CellCheck,
    ] {
        for (path_index, path_name) in [(0, "clock"), (1, "data")] {
            for (early_late, early_late_name) in [(0, "early"), (1, "late")] {
                for edge in TimingEdge::ALL {
                    let value = context.timing_derates.0[kind.index()][path_index][early_late]
                        [edge.index()];
                    if value.to_bits() != 1.0f64.to_bits() {
                        push_sdc_line(
                            output,
                            &format!(
                                "set_timing_derate -{} -{} -{} -{} {}",
                                early_late_name,
                                edge_name_sdc(edge),
                                path_name,
                                timing_derate_kind_name(kind),
                                sdc_number(value)
                            ),
                        );
                    }
                }
            }
        }
    }
}

fn write_clock_groups(
    context: &TimingContext,
    output: &mut String,
    clock_name: &impl Fn(ClockId) -> Result<String, crate::ConstraintError>,
) -> Result<(), crate::TimingError> {
    let mut written = BTreeSet::new();
    for exception in &context.path_exceptions {
        let Some((kind, name, comment)) = parse_clock_group_marker(&exception.comment) else {
            continue;
        };
        let mut from = clock_group_ids(exception.from.objects());
        let mut to = clock_group_ids(exception.to.objects());
        if to < from {
            std::mem::swap(&mut from, &mut to);
        }
        if !written.insert((kind.to_string(), name.to_string(), from.clone(), to.clone())) {
            continue;
        }
        let from_names = from
            .iter()
            .map(|clock| clock_name(*clock))
            .collect::<Result<Vec<_>, _>>()?;
        let to_names = to
            .iter()
            .map(|clock| clock_name(*clock))
            .collect::<Result<Vec<_>, _>>()?;
        let mut line = format!(
            "set_clock_groups -{} -name {} -group [get_clocks [list {}]] -group [get_clocks [list {}]]",
            kind,
            tcl_quote(name),
            quoted_clock_names(&from_names),
            quoted_clock_names(&to_names),
        );
        if !comment.is_empty() {
            append_format(&mut line, format_args!(" -comment {}", tcl_quote(comment)));
        }
        push_sdc_line(output, &line);
    }
    Ok(())
}

fn clock_group_ids(endpoints: &[TimingEndpoint]) -> Vec<ClockId> {
    let mut clocks = endpoints
        .iter()
        .filter_map(|endpoint| match endpoint {
            TimingEndpoint::Clock(clock) => Some(*clock),
            _ => None,
        })
        .collect::<Vec<_>>();
    clocks.sort_unstable();
    clocks
}

fn quoted_clock_names(names: &[String]) -> String {
    names
        .iter()
        .map(|name| tcl_quote(name))
        .collect::<Vec<_>>()
        .join(" ")
}

fn write_path_exceptions(
    context: &TimingContext,
    output: &mut String,
    object_expression: &mut impl FnMut(&[opto_db::AnyObjectId]) -> Result<String, crate::TimingError>,
) -> Result<(), crate::TimingError> {
    for exception in context
        .path_exceptions
        .iter()
        .filter(|exception| parse_clock_group_marker(&exception.comment).is_none())
    {
        write_path_exception(output, exception, object_expression)?;
    }
    Ok(())
}

fn write_design_rules(
    context: &TimingContext,
    output: &mut String,
    object_expression: &mut impl FnMut(&[opto_db::AnyObjectId]) -> Result<String, crate::TimingError>,
) -> Result<(), crate::TimingError> {
    for (kind, command) in [
        (DesignRuleKind::MaxTransition, "set_max_transition"),
        (DesignRuleKind::MaxCapacitance, "set_max_capacitance"),
        (DesignRuleKind::MaxFanout, "set_max_fanout"),
    ] {
        for rule in context.design_rule_constraints(kind).iter() {
            let mut line = format!("{command} {}", sdc_number(rule.limit));
            match rule.scope {
                DesignRuleScope::DataPath => line.push_str(" -data_path"),
                DesignRuleScope::ClockPath => line.push_str(" -clock_path"),
                DesignRuleScope::ClockAndData => line.push_str(" -clock_path -data_path"),
                DesignRuleScope::All => {}
            }
            for object in &rule.objects {
                line.push(' ');
                line.push_str(&object_expression(&[object.object_id()])?);
            }
            push_sdc_line(output, &line);
        }
    }
    Ok(())
}

fn write_primary_clock(
    output: &mut String,
    clock: &Clock,
    needs_add: bool,
    object_expression: &mut impl FnMut(&[opto_db::AnyObjectId]) -> Result<String, crate::TimingError>,
) -> Result<(), crate::TimingError> {
    let mut line = format!(
        "create_clock -name {} -period {}",
        tcl_quote(&clock.name),
        sdc_number(clock.period)
    );
    if let Some((rise, fall)) = clock.waveform {
        append_format(
            &mut line,
            format_args!(
                " -waveform [list {} {}]",
                sdc_number(rise),
                sdc_number(fall)
            ),
        );
    }
    if !clock.comment.is_empty() {
        append_format(
            &mut line,
            format_args!(" -comment {}", tcl_quote(&clock.comment)),
        );
    }
    if needs_add {
        line.push_str(" -add");
    }
    if !clock.sources.is_empty() {
        line.push(' ');
        line.push_str(&object_expression(
            &clock
                .sources
                .iter()
                .map(|port| port.erase())
                .collect::<Vec<_>>(),
        )?);
    }
    push_sdc_line(output, &line);
    Ok(())
}

/// Writes one SDC line per populated (edge, delay type) slot of `slots`.
///
/// `set_input_transition`, `set_load`, and `set_drive` all take the same
/// `-rise/-fall -max/-min <value> <object>` shape.
fn write_edge_delay_slots(
    output: &mut String,
    command: &str,
    slots: &PortValueSlots,
    object: &str,
) {
    for delay_type in [DelayType::Max, DelayType::Min] {
        for edge in TimingEdge::ALL {
            if let Some(value) = slots.value(edge, delay_type) {
                push_sdc_line(
                    output,
                    &format!(
                        "{command} -{} -{} {} {object}",
                        edge_name_sdc(edge),
                        delay_type_name(delay_type),
                        sdc_number(value),
                    ),
                );
            }
        }
    }
}

fn push_sdc_line(output: &mut String, line: &str) {
    output.push_str(line);
    output.push('\n');
}

fn sdc_number(value: f64) -> String {
    value.to_string()
}

fn tcl_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for character in value.chars() {
        match character {
            '\\' | '"' | '$' | '[' | ']' => {
                quoted.push('\\');
                quoted.push(character);
            }
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

fn sdc_collection_expression(
    objects: &[opto_db::AnyObjectId],
    resolve: &mut impl FnMut(opto_db::AnyObjectId) -> Option<String>,
) -> Result<String, crate::TimingError> {
    let mut classes = BTreeMap::<opto_db::ObjectClass, Vec<String>>::new();
    for &object in objects {
        let name = resolve(object).ok_or_else(|| crate::ConstraintError::UnresolvedSdcObject {
            object: format!("{object:?}"),
        })?;
        classes.entry(object.class()).or_default().push(name);
    }
    let mut expressions = classes
        .into_iter()
        .map(|(class, names)| {
            let command = match class {
                opto_db::ObjectClass::Design => "get_designs",
                opto_db::ObjectClass::Port => "get_ports",
                opto_db::ObjectClass::Cell => "get_cells",
                opto_db::ObjectClass::Pin => "get_pins",
                opto_db::ObjectClass::Net => "get_nets",
                opto_db::ObjectClass::Clock => "get_clocks",
            };
            let words = names
                .iter()
                .map(|name| tcl_quote(name))
                .collect::<Vec<_>>()
                .join(" ");
            format!("[{command} [list {words}]]")
        })
        .collect::<Vec<_>>();
    if expressions.is_empty() {
        return Ok(String::new());
    }
    let mut expression = expressions.remove(0);
    for addition in expressions {
        expression = format!("[concat {expression} {addition}]");
    }
    Ok(expression)
}

fn edge_name_sdc(edge: TimingEdge) -> &'static str {
    match edge {
        TimingEdge::Rise => "rise",
        TimingEdge::Fall => "fall",
    }
}

fn delay_type_name(delay_type: DelayType) -> &'static str {
    match delay_type {
        DelayType::Max => "max",
        DelayType::Min => "min",
    }
}

fn delay_type_corner_name(delay_type: DelayType) -> &'static str {
    match delay_type {
        DelayType::Max => "setup",
        DelayType::Min => "hold",
    }
}

fn edge_selector_option(selection: EdgeSelection, role: &str) -> String {
    match selection {
        EdgeSelection::Both => format!("-{role}"),
        EdgeSelection::Rise => format!("-rise_{role}"),
        EdgeSelection::Fall => format!("-fall_{role}"),
    }
}

fn case_analysis_name(value: CaseAnalysisValue) -> &'static str {
    match value {
        CaseAnalysisValue::Zero => "0",
        CaseAnalysisValue::One => "1",
        CaseAnalysisValue::Rise => "rise",
        CaseAnalysisValue::Fall => "fall",
    }
}

fn timing_derate_kind_name(kind: TimingDerateKind) -> &'static str {
    match kind {
        TimingDerateKind::NetDelay => "net_delay",
        TimingDerateKind::CellDelay => "cell_delay",
        TimingDerateKind::CellCheck => "cell_check",
    }
}

fn parse_clock_group_marker(comment: &str) -> Option<(&'static str, &str, &str)> {
    let rest = comment.strip_prefix("\0opto-clock-group:")?;
    let (kind, rest) = rest.split_once(':')?;
    let (name, comment) = rest.split_once('\0')?;
    let kind = match kind {
        "logical" => "logically_exclusive",
        "physical" => "physically_exclusive",
        "asynchronous" => "asynchronous",
        _ => return None,
    };
    Some((kind, name, comment))
}

fn write_io_delay_rows(
    output: &mut String,
    command: &str,
    port: PortId,
    rows: &[IoDelay],
    timing: &TimingContext,
    object_expression: &mut impl FnMut(&[opto_db::AnyObjectId]) -> Result<String, crate::TimingError>,
) -> Result<(), crate::TimingError> {
    let object = object_expression(&[port.erase()])?;
    let mut emitted = false;
    for row in rows {
        for delay_type in [DelayType::Max, DelayType::Min] {
            for edge in TimingEdge::ALL {
                let Some(value) = row.delay(edge, delay_type) else {
                    continue;
                };
                let mut line = format!(
                    "{command} -{} -{}",
                    edge_name_sdc(edge),
                    delay_type_name(delay_type)
                );
                if let Some(clock) = row.clock {
                    let name = &timing
                        .clock(clock)
                        .ok_or(crate::ConstraintError::ClockNotFound { id: clock })?
                        .name;
                    append_format(
                        &mut line,
                        format_args!(" -clock [get_clocks {}]", tcl_quote(name)),
                    );
                    if row.clock_edge == TimingEdge::Fall {
                        line.push_str(" -clock_fall");
                    }
                }
                if row.source_latency_included {
                    line.push_str(" -source_latency_included");
                }
                if row.network_latency_included {
                    line.push_str(" -network_latency_included");
                }
                if emitted {
                    line.push_str(" -add_delay");
                }
                append_format(&mut line, format_args!(" {} {}", sdc_number(value), object));
                push_sdc_line(output, &line);
                emitted = true;
            }
        }
    }
    Ok(())
}

fn write_path_exception(
    output: &mut String,
    exception: &PathException,
    object_expression: &mut impl FnMut(&[opto_db::AnyObjectId]) -> Result<String, crate::TimingError>,
) -> Result<(), crate::TimingError> {
    let mut line = match exception.kind {
        PathExceptionKind::FalsePath => "set_false_path".to_string(),
        PathExceptionKind::MaxDelay { delay } => {
            format!("set_max_delay {}", sdc_number(delay))
        }
        PathExceptionKind::MinDelay { delay } => {
            format!("set_min_delay {}", sdc_number(delay))
        }
        PathExceptionKind::MultiCycle { cycles, .. } => {
            format!("set_multicycle_path {cycles}")
        }
    };
    if matches!(
        exception.kind,
        PathExceptionKind::FalsePath | PathExceptionKind::MultiCycle { .. }
    ) {
        match exception.corner {
            ExceptionCorner::Setup => line.push_str(" -setup"),
            ExceptionCorner::Hold => line.push_str(" -hold"),
            ExceptionCorner::Both => {}
        }
    }
    match exception.edges.end {
        EdgeSelection::Rise => line.push_str(" -rise"),
        EdgeSelection::Fall => line.push_str(" -fall"),
        EdgeSelection::Both => {}
    }
    if let PathExceptionKind::MultiCycle { use_end_clock, .. } = exception.kind {
        line.push_str(if use_end_clock { " -end" } else { " -start" });
    }
    if exception.ignore_clock_latency {
        line.push_str(" -ignore_clock_latency");
    }
    if !exception.from.is_unrestricted() {
        let objects = exception
            .from
            .objects()
            .iter()
            .map(|endpoint| endpoint.object_id())
            .collect::<Vec<_>>();
        append_format(
            &mut line,
            format_args!(
                " {} {}",
                edge_selector_option(exception.edges.from, "from"),
                object_expression(&objects)?
            ),
        );
    }
    for (filter, edge) in exception.through.iter().zip(exception.edges.through.iter()) {
        let objects = filter
            .objects()
            .iter()
            .map(|endpoint| endpoint.object_id())
            .collect::<Vec<_>>();
        append_format(
            &mut line,
            format_args!(
                " {} {}",
                edge_selector_option(*edge, "through"),
                object_expression(&objects)?
            ),
        );
    }
    if !exception.to.is_unrestricted() {
        let objects = exception
            .to
            .objects()
            .iter()
            .map(|endpoint| endpoint.object_id())
            .collect::<Vec<_>>();
        append_format(
            &mut line,
            format_args!(
                " {} {}",
                edge_selector_option(exception.edges.to, "to"),
                object_expression(&objects)?
            ),
        );
    }
    if !exception.comment.is_empty() && !exception.comment.starts_with('\0') {
        append_format(
            &mut line,
            format_args!(" -comment {}", tcl_quote(&exception.comment)),
        );
    }
    push_sdc_line(output, &line);
    Ok(())
}

fn append_format(output: &mut String, arguments: std::fmt::Arguments<'_>) {
    use std::fmt::Write as _;

    output
        .write_fmt(arguments)
        .expect("formatting into a String cannot fail");
}
