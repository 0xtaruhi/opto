// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

macro_rules! collection_query_args {
    ($name:ident, $command:literal, $hint:expr, $handler:ident) => {
        #[derive(TclCommand)]
        #[command(name = $command, handler = $handler, sdc)]
        pub(crate) struct $name<'a> {
            #[arg(long = "-of_objects")]
            of_objects: Option<TclArg<'a>>,
            #[arg(long = "-filter")]
            filter: Option<TclArg<'a>>,
            #[arg(positional, value_hint = $hint)]
            pattern: Option<String>,
        }
    };
}

collection_query_args!(GetPortsArgs, "get_ports", ValueHint::Port, get_ports);
collection_query_args!(GetCellsArgs, "get_cells", ValueHint::Cell, get_cells);
collection_query_args!(GetPinsArgs, "get_pins", ValueHint::Pin, get_pins);
collection_query_args!(GetNetsArgs, "get_nets", ValueHint::Net, get_nets);

#[derive(TclCommand)]
#[command(name = "get_clocks", handler = get_clocks, sdc)]
pub(crate) struct GetClocksArgs<'a> {
    #[arg(long = "-filter")]
    filter: Option<TclArg<'a>>,
    #[arg(positional, value_hint = ValueHint::Clock)]
    pattern: Option<String>,
}

#[derive(TclCommand)]
#[command(name = "all_inputs", handler = all_inputs, sdc)]
pub(crate) struct AllInputsArgs {
    #[arg(long = "-no_clocks")]
    no_clocks: bool,
}

#[derive(TclCommand)]
#[command(name = "all_outputs", handler = all_outputs, sdc)]
pub(crate) struct AllOutputsArgs {}

#[derive(TclCommand)]
#[command(name = "all_registers", handler = all_registers, sdc)]
pub(crate) struct AllRegistersArgs {
    #[arg(long = "-edge_triggered")]
    edge_triggered: bool,
    #[arg(long = "-level_sensitive")]
    level_sensitive: bool,
}

#[derive(TclCommand)]
#[command(name = "all_clocks", handler = all_clocks, sdc)]
pub(crate) struct AllClocksArgs {}

pub(crate) fn parse_filter(
    interp: *mut TclInterp,
    command: &str,
    raw: &TclArg<'_>,
) -> Result<CollectionFilter, crate::ShellError> {
    let parts = split_tcl_list(interp, raw)?;
    let [attribute, operator, value] = parts.as_slice() else {
        return Err(crate::ShellError::command(format!(
            "{command}: expected filter '{{property operator value}}'"
        )));
    };
    let attribute = attribute.strip_prefix('.').unwrap_or(attribute).to_string();
    let operator = match operator.as_str() {
        "==" => FilterOperator::Eq,
        "!=" => FilterOperator::Ne,
        "=~" => FilterOperator::Glob,
        "!~" => FilterOperator::NotGlob,
        other => {
            return Err(crate::ShellError::command(format!(
                "{command}: unsupported filter operator '{other}'"
            )));
        }
    };
    Ok(CollectionFilter {
        attribute,
        operator,
        value: value.clone(),
    })
}

struct Query<'a> {
    pattern: String,
    of_objects: Option<TclArg<'a>>,
    filter: Option<CollectionFilter>,
}

fn query<'a>(
    interp: *mut TclInterp,
    command: &str,
    pattern: Option<String>,
    of_objects: Option<TclArg<'a>>,
    filter: Option<TclArg<'a>>,
) -> Result<Query<'a>, crate::ShellError> {
    Ok(Query {
        pattern: pattern.unwrap_or_else(|| "*".to_string()),
        of_objects,
        filter: filter
            .map(|raw| parse_filter(interp, command, &raw))
            .transpose()?,
    })
}

macro_rules! object_query_handler {
    ($handler:ident, $args:ident, $plain:ident, $related:ident) => {
        pub(crate) fn $handler(
            state: &ShellState,
            interp: *mut TclInterp,
            command: &'static str,
            args: $args<'_>,
        ) -> Result<CommandResult, crate::ShellError> {
            let query = query(interp, command, args.pattern, args.of_objects, args.filter)?;
            let collection = {
                let mut session = state.session.borrow_mut();
                if let Some(objects) = query.of_objects {
                    session.$related(objects.as_str(), &query.pattern)?
                } else {
                    session.$plain(&query.pattern)?
                }
            };
            let handles = state
                .session
                .borrow()
                .collection_handles_filtered(collection, query.filter.as_ref());
            Ok(CommandResult::List(handles))
        }
    };
}

object_query_handler!(get_ports, GetPortsArgs, get_ports, get_ports_of_objects);
object_query_handler!(get_cells, GetCellsArgs, get_cells, get_cells_of_objects);
object_query_handler!(get_pins, GetPinsArgs, get_pins, get_pins_of_objects);
object_query_handler!(get_nets, GetNetsArgs, get_nets, get_nets_of_objects);

pub(crate) fn get_clocks(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: GetClocksArgs<'_>,
) -> Result<CommandResult, crate::ShellError> {
    let query = query(interp, command, args.pattern, None, args.filter)?;
    let collection = state.session.borrow_mut().get_clocks(&query.pattern)?;
    let handles = state
        .session
        .borrow()
        .collection_handles_filtered(collection, query.filter.as_ref());
    Ok(CommandResult::List(handles))
}

pub(crate) fn all_inputs(
    state: &ShellState,
    _interp: *mut TclInterp,
    _command: &'static str,
    args: AllInputsArgs,
) -> Result<CommandResult, crate::ShellError> {
    let collection = state.session.borrow_mut().all_inputs(args.no_clocks)?;
    Ok(CommandResult::List(
        state.session.borrow().collection_handles(collection),
    ))
}

pub(crate) fn all_outputs(
    state: &ShellState,
    _interp: *mut TclInterp,
    _command: &'static str,
    _args: AllOutputsArgs,
) -> Result<CommandResult, crate::ShellError> {
    let collection = state.session.borrow_mut().all_outputs()?;
    Ok(CommandResult::List(
        state.session.borrow().collection_handles(collection),
    ))
}

pub(crate) fn all_registers(
    state: &ShellState,
    _interp: *mut TclInterp,
    _command: &'static str,
    args: AllRegistersArgs,
) -> Result<CommandResult, crate::ShellError> {
    let (edge_triggered, level_sensitive) = if !args.edge_triggered && !args.level_sensitive {
        (true, true)
    } else {
        (args.edge_triggered, args.level_sensitive)
    };
    let collection = state
        .session
        .borrow_mut()
        .all_registers(edge_triggered, level_sensitive)?;
    Ok(CommandResult::List(
        state.session.borrow().collection_handles(collection),
    ))
}

pub(crate) fn all_clocks(
    state: &ShellState,
    _interp: *mut TclInterp,
    _command: &'static str,
    _args: AllClocksArgs,
) -> Result<CommandResult, crate::ShellError> {
    let collection = state.session.borrow_mut().get_clocks("*")?;
    Ok(CommandResult::List(
        state.session.borrow().collection_handles(collection),
    ))
}
