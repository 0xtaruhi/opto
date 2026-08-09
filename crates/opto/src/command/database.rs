// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[derive(TclCommand)]
#[command(name = "get_db", handler = get_db)]
pub(crate) struct GetDbArgs<'a> {
    #[arg(long = "-if")]
    filter: Option<TclArg<'a>>,
    #[arg(long = "-of")]
    of_objects: Option<TclArg<'a>>,
    #[arg(positional, min = 1, max = 2)]
    terms: Vec<TclArg<'a>>,
}

#[derive(TclCommand)]
#[command(name = "set_db", handler = set_db)]
pub(crate) struct SetDbArgs<'a> {
    #[arg(positional, min = 2, max = 3)]
    terms: Vec<TclArg<'a>>,
}

pub(crate) fn get_db(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: GetDbArgs<'_>,
) -> Result<CommandResult, crate::ShellError> {
    match args.terms.as_slice() {
        [term] => get_root_or_objects(state, interp, command, term, "*", &args),
        [objects, property] if property.starts_with('.') => {
            if args.filter.is_some() || args.of_objects.is_some() {
                return Err(crate::ShellError::command(
                    "get_db: -if and -of are valid only for object-class queries",
                ));
            }
            let values = state
                .session
                .borrow()
                .collection_attribute_values(objects.as_str(), property.trim_start_matches('.'))?;
            Ok(CommandResult::List(values))
        }
        [class, pattern] => {
            get_root_or_objects(state, interp, command, class, pattern.as_str(), &args)
        }
        _ => unreachable!("derive schema validates get_db arity"),
    }
}

fn get_root_or_objects(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    term: &TclArg<'_>,
    pattern: &str,
    args: &GetDbArgs<'_>,
) -> Result<CommandResult, crate::ShellError> {
    if pattern == "*" && args.filter.is_none() && args.of_objects.is_none() {
        match term.as_str() {
            "current_design" => {
                if state.session.borrow().current_design().is_none() {
                    return Ok(CommandResult::List(Vec::new()));
                }
                let handle = state
                    .session
                    .borrow_mut()
                    .store_current_design_collection()?;
                return Ok(CommandResult::List(vec![handle]));
            }
            "hdl_search_path" => {
                return Ok(CommandResult::List(
                    state
                        .session
                        .borrow()
                        .hdl_search_path()
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect(),
                ));
            }
            "lib_search_path" => {
                return Ok(CommandResult::List(
                    state
                        .session
                        .borrow()
                        .lib_search_path()
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect(),
                ));
            }
            "synth_effort" => {
                let value = match state.session.borrow().synth_effort() {
                    SynthesisEffort::Low => "low",
                    SynthesisEffort::Medium => "medium",
                    SynthesisEffort::High => "high",
                };
                return Ok(CommandResult::Complete(value.to_string()));
            }
            "clock_gating" => {
                return Ok(CommandResult::Complete(
                    state.session.borrow().clock_gating_enabled().to_string(),
                ));
            }
            "clock_gating_minimum_bitwidth" => {
                return Ok(CommandResult::Complete(
                    state
                        .session
                        .borrow()
                        .clock_gating_minimum_bitwidth()
                        .to_string(),
                ));
            }
            "clock_gating_latch_based" => {
                return Ok(CommandResult::Complete(
                    state
                        .session
                        .borrow()
                        .clock_gating_latch_based()
                        .to_string(),
                ));
            }
            _ => {}
        }
    }

    let filter = args
        .filter
        .as_ref()
        .map(|raw| super::collection::parse_filter(interp, command, raw))
        .transpose()?;
    let related = args.of_objects.as_ref().map(TclArg::as_str);
    let mut session = state.session.borrow_mut();
    let handles = match term.as_str() {
        "designs" => {
            if related.is_some() {
                return Err(crate::ShellError::command(
                    "get_db: designs does not support -of",
                ));
            }
            let objects = session.get_designs(pattern)?;
            session.collection_handles_filtered(objects, filter.as_ref())
        }
        "ports" => {
            let objects = match related {
                Some(objects) => session.get_ports_of_objects(objects, pattern)?,
                None => session.get_ports(pattern)?,
            };
            session.collection_handles_filtered(objects, filter.as_ref())
        }
        "insts" => {
            let objects = match related {
                Some(objects) => session.get_cells_of_objects(objects, pattern)?,
                None => session.get_cells(pattern)?,
            };
            session.collection_handles_filtered(objects, filter.as_ref())
        }
        "pins" => {
            let objects = match related {
                Some(objects) => session.get_pins_of_objects(objects, pattern)?,
                None => session.get_pins(pattern)?,
            };
            session.collection_handles_filtered(objects, filter.as_ref())
        }
        "nets" => {
            let objects = match related {
                Some(objects) => session.get_nets_of_objects(objects, pattern)?,
                None => session.get_nets(pattern)?,
            };
            session.collection_handles_filtered(objects, filter.as_ref())
        }
        "clocks" => {
            if related.is_some() {
                return Err(crate::ShellError::command(
                    "get_db: clocks does not support -of",
                ));
            }
            let objects = session.get_clocks(pattern)?;
            session.collection_handles_filtered(objects, filter.as_ref())
        }
        "libraries" => {
            if related.is_some() || filter.is_some() {
                return Err(crate::ShellError::command(
                    "get_db: libraries currently supports name patterns only",
                ));
            }
            session.library_names_matching(pattern)
        }
        "lib_cells" => {
            if related.is_some() || filter.is_some() {
                return Err(crate::ShellError::command(
                    "get_db: lib_cells currently supports name patterns only",
                ));
            }
            session.library_cell_names_matching(pattern)?
        }
        other => {
            return Err(crate::ShellError::command(format!(
                "get_db: unknown root property or object class '{other}'"
            )));
        }
    };
    Ok(CommandResult::List(handles))
}

pub(crate) fn set_db(
    state: &ShellState,
    interp: *mut TclInterp,
    _command: &'static str,
    args: SetDbArgs<'_>,
) -> Result<CommandResult, crate::ShellError> {
    let changed = match args.terms.as_slice() {
        [property, value] => set_root(state, interp, property.as_str(), value)?,
        [objects, property, value] if property.starts_with('.') => {
            let enabled = parse_boolean(value.as_str())?;
            if property.as_str() == ".dont_use" {
                if !enabled {
                    return Err(crate::ShellError::command(
                        "set_db: clearing .dont_use is not supported",
                    ));
                }
                let patterns = split_tcl_list(interp, objects)?;
                state
                    .session
                    .borrow_mut()
                    .set_library_cells_dont_use(&patterns)?
            } else {
                state.session.borrow_mut().set_db_object_property(
                    objects.as_str(),
                    property.trim_start_matches('.'),
                    enabled,
                )?
            }
        }
        [_, property, _] => {
            return Err(crate::ShellError::command(format!(
                "set_db: object property '{property}' must start with '.'"
            )));
        }
        _ => unreachable!("derive schema validates set_db arity"),
    };
    Ok(CommandResult::Complete(changed.to_string()))
}

fn set_root(
    state: &ShellState,
    interp: *mut TclInterp,
    property: &str,
    value: &TclArg<'_>,
) -> Result<usize, crate::ShellError> {
    match property {
        "hdl_search_path" => Ok(state.session.borrow_mut().set_hdl_search_path(
            split_tcl_list(interp, value)?
                .into_iter()
                .map(PathBuf::from)
                .collect(),
        )),
        "lib_search_path" => Ok(state.session.borrow_mut().set_lib_search_path(
            split_tcl_list(interp, value)?
                .into_iter()
                .map(PathBuf::from)
                .collect(),
        )),
        "synth_effort" => {
            let effort = match value.as_str() {
                "low" => SynthesisEffort::Low,
                "medium" => SynthesisEffort::Medium,
                "high" => SynthesisEffort::High,
                other => {
                    return Err(crate::ShellError::command(format!(
                        "set_db: synth_effort must be low, medium, or high; got '{other}'"
                    )));
                }
            };
            Ok(state.session.borrow_mut().set_synth_effort(effort))
        }
        "clock_gating" => Ok(state
            .session
            .borrow_mut()
            .set_clock_gating_enabled(parse_boolean(value.as_str())?)),
        "clock_gating_minimum_bitwidth" => {
            let width = value.parse::<usize>().map_err(|_| {
                crate::ShellError::command(format!(
                    "set_db: clock_gating_minimum_bitwidth expects an integer; got '{value}'"
                ))
            })?;
            state
                .session
                .borrow_mut()
                .set_clock_gating_minimum_bitwidth(width)
                .map_err(Into::into)
        }
        "clock_gating_latch_based" => Ok(state
            .session
            .borrow_mut()
            .set_clock_gating_latch_based(parse_boolean(value.as_str())?)),
        "current_design" => {
            let values = split_tcl_list(interp, value)?;
            let [handle] = values.as_slice() else {
                return Err(crate::ShellError::command(
                    "set_db: current_design expects exactly one design handle",
                ));
            };
            let name = state
                .session
                .borrow()
                .collection_first_object_name(handle)?
                .ok_or_else(|| {
                    crate::ShellError::command(
                        "set_db: current_design expects a design object handle",
                    )
                })?;
            if state.session.borrow().current_design() == Some(name.as_str()) {
                Ok(0)
            } else {
                state.session.borrow_mut().set_current_design(&name)?;
                Ok(1)
            }
        }
        other => Err(crate::ShellError::command(format!(
            "set_db: unknown or read-only root property '{other}'"
        ))),
    }
}

fn parse_boolean(value: &str) -> Result<bool, crate::ShellError> {
    match value {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        other => Err(crate::ShellError::command(format!(
            "set_db: expected boolean true or false; got '{other}'"
        ))),
    }
}
