// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[derive(TclCommand)]
#[command(
    name = "get_db",
    handler = get_db,
    summary = "Read a typed root property, query an object class, or project object properties.",
    requires = "Requested properties and object handles must be available in the current lifecycle.",
    example = "get_db insts * -if {.ref_name =~ DFF*}"
)]
pub(crate) struct GetDbArgs<'a> {
    #[arg(long = "-if")]
    filter: Option<TclArg<'a>>,
    #[arg(long = "-of")]
    of_objects: Option<TclArg<'a>>,
    #[arg(positional, min = 1, max = 2)]
    terms: Vec<TclArg<'a>>,
}

#[derive(TclCommand)]
#[command(
    name = "set_db",
    handler = set_db,
    summary = "Atomically update a writable typed database property.",
    requires = "The property must be writable and the complete value must satisfy its schema.",
    example = "set_db synth_effort high"
)]
pub(crate) struct SetDbArgs<'a> {
    #[arg(positional, min = 2, max = 3)]
    terms: Vec<TclArg<'a>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootPropertyKind {
    ClockGating,
    ClockGatingLatchBased,
    ClockGatingMinimumBitwidth,
    CurrentDesign,
    HdlSearchPath,
    LibSearchPath,
    SynthEffort,
}

#[derive(Debug, Clone, Copy)]
struct RootPropertySpec {
    name: &'static str,
    kind: RootPropertyKind,
    readable: bool,
    writable: bool,
}

const ROOT_PROPERTIES: &[RootPropertySpec] = &[
    RootPropertySpec {
        name: "clock_gating",
        kind: RootPropertyKind::ClockGating,
        readable: true,
        writable: true,
    },
    RootPropertySpec {
        name: "clock_gating_latch_based",
        kind: RootPropertyKind::ClockGatingLatchBased,
        readable: true,
        writable: true,
    },
    RootPropertySpec {
        name: "clock_gating_minimum_bitwidth",
        kind: RootPropertyKind::ClockGatingMinimumBitwidth,
        readable: true,
        writable: true,
    },
    RootPropertySpec {
        name: "current_design",
        kind: RootPropertyKind::CurrentDesign,
        readable: true,
        writable: true,
    },
    RootPropertySpec {
        name: "hdl_search_path",
        kind: RootPropertyKind::HdlSearchPath,
        readable: true,
        writable: true,
    },
    RootPropertySpec {
        name: "lib_search_path",
        kind: RootPropertyKind::LibSearchPath,
        readable: true,
        writable: true,
    },
    RootPropertySpec {
        name: "synth_effort",
        kind: RootPropertyKind::SynthEffort,
        readable: true,
        writable: true,
    },
];

fn root_property(name: &str) -> Option<&'static RootPropertySpec> {
    ROOT_PROPERTIES
        .binary_search_by_key(&name, |property| property.name)
        .ok()
        .map(|index| &ROOT_PROPERTIES[index])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ObjectQueryKind {
    Clock,
    Design,
    Cell,
    Library,
    LibraryCell,
    Net,
    Pin,
    Port,
}

#[derive(Debug, Clone, Copy)]
struct ObjectQuerySpec {
    name: &'static str,
    kind: ObjectQueryKind,
    related: bool,
    filter: bool,
}

const OBJECT_QUERIES: &[ObjectQuerySpec] = &[
    ObjectQuerySpec {
        name: "clocks",
        kind: ObjectQueryKind::Clock,
        related: false,
        filter: true,
    },
    ObjectQuerySpec {
        name: "designs",
        kind: ObjectQueryKind::Design,
        related: false,
        filter: true,
    },
    ObjectQuerySpec {
        name: "insts",
        kind: ObjectQueryKind::Cell,
        related: true,
        filter: true,
    },
    ObjectQuerySpec {
        name: "lib_cells",
        kind: ObjectQueryKind::LibraryCell,
        related: false,
        filter: false,
    },
    ObjectQuerySpec {
        name: "libraries",
        kind: ObjectQueryKind::Library,
        related: false,
        filter: false,
    },
    ObjectQuerySpec {
        name: "nets",
        kind: ObjectQueryKind::Net,
        related: true,
        filter: true,
    },
    ObjectQuerySpec {
        name: "pins",
        kind: ObjectQueryKind::Pin,
        related: true,
        filter: true,
    },
    ObjectQuerySpec {
        name: "ports",
        kind: ObjectQueryKind::Port,
        related: true,
        filter: true,
    },
];

fn object_query(name: &str) -> Option<&'static ObjectQuerySpec> {
    OBJECT_QUERIES
        .binary_search_by_key(&name, |query| query.name)
        .ok()
        .map(|index| &OBJECT_QUERIES[index])
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
    if let Some(property) = root_property(term.as_str()).filter(|property| property.readable) {
        if args.filter.is_some() || args.of_objects.is_some() {
            return Err(crate::ShellError::command(format!(
                "get_db: -if and -of are valid only for object-class queries; '{}' is a root property",
                term.as_str()
            )));
        }
        if pattern != "*" {
            return Err(crate::ShellError::command(format!(
                "get_db: root property '{}' does not accept name pattern '{pattern}'",
                term.as_str()
            )));
        }
        return get_root_property(state, property.kind);
    }

    let filter = args
        .filter
        .as_ref()
        .map(|raw| super::collection::parse_filter(interp, command, raw))
        .transpose()?;
    let related = args.of_objects.as_ref().map(TclArg::as_str);
    let Some(query) = object_query(term.as_str()) else {
        return Err(crate::ShellError::command(format!(
            "get_db: unknown root property or object class '{}'",
            term.as_str()
        )));
    };
    if related.is_some() && !query.related {
        return Err(crate::ShellError::command(format!(
            "get_db: {} does not support -of",
            query.name
        )));
    }
    if filter.is_some() && !query.filter {
        return Err(crate::ShellError::command(format!(
            "get_db: {} currently supports name patterns only",
            query.name
        )));
    }
    let handles = query_objects(
        &mut state.session.borrow_mut(),
        query.kind,
        pattern,
        related,
        filter.as_ref(),
    )?;
    Ok(CommandResult::List(handles))
}

fn get_root_property(
    state: &ShellState,
    property: RootPropertyKind,
) -> Result<CommandResult, crate::ShellError> {
    match property {
        RootPropertyKind::CurrentDesign => {
            if state.session.borrow().current_design().is_none() {
                return Ok(CommandResult::List(Vec::new()));
            }
            let handle = state
                .session
                .borrow_mut()
                .store_current_design_collection()?;
            Ok(CommandResult::List(vec![handle]))
        }
        RootPropertyKind::HdlSearchPath => Ok(CommandResult::List(
            state
                .session
                .borrow()
                .hdl_search_path()
                .iter()
                .map(|path| path.display().to_string())
                .collect(),
        )),
        RootPropertyKind::LibSearchPath => Ok(CommandResult::List(
            state
                .session
                .borrow()
                .lib_search_path()
                .iter()
                .map(|path| path.display().to_string())
                .collect(),
        )),
        RootPropertyKind::SynthEffort => {
            let value = match state.session.borrow().synth_effort() {
                SynthesisEffort::Low => "low",
                SynthesisEffort::Medium => "medium",
                SynthesisEffort::High => "high",
            };
            Ok(CommandResult::Complete(value.to_string()))
        }
        RootPropertyKind::ClockGating => Ok(CommandResult::Complete(
            state.session.borrow().clock_gating_enabled().to_string(),
        )),
        RootPropertyKind::ClockGatingMinimumBitwidth => Ok(CommandResult::Complete(
            state
                .session
                .borrow()
                .clock_gating_minimum_bitwidth()
                .to_string(),
        )),
        RootPropertyKind::ClockGatingLatchBased => Ok(CommandResult::Complete(
            state
                .session
                .borrow()
                .clock_gating_latch_based()
                .to_string(),
        )),
    }
}

pub(crate) fn query_objects(
    session: &mut opto_session::Session,
    kind: ObjectQueryKind,
    pattern: &str,
    related: Option<&str>,
    filter: Option<&CollectionFilter>,
) -> Result<Vec<String>, crate::ShellError> {
    let handles = match kind {
        ObjectQueryKind::Design => {
            let objects = session.get_designs(pattern)?;
            session.collection_handles_filtered(objects, filter)
        }
        ObjectQueryKind::Port => {
            if let Some(related) = related {
                let objects = session.get_ports_of_objects(related, pattern)?;
                session.collection_handles_filtered(objects, filter)
            } else {
                let objects = session.get_ports(pattern)?;
                session.collection_handles_filtered(objects, filter)
            }
        }
        ObjectQueryKind::Cell => {
            if let Some(related) = related {
                let objects = session.get_cells_of_objects(related, pattern)?;
                session.collection_handles_filtered(objects, filter)
            } else {
                let objects = session.get_cells(pattern)?;
                session.collection_handles_filtered(objects, filter)
            }
        }
        ObjectQueryKind::Pin => {
            if let Some(related) = related {
                let objects = session.get_pins_of_objects(related, pattern)?;
                session.collection_handles_filtered(objects, filter)
            } else {
                let objects = session.get_pins(pattern)?;
                session.collection_handles_filtered(objects, filter)
            }
        }
        ObjectQueryKind::Net => {
            if let Some(related) = related {
                let objects = session.get_nets_of_objects(related, pattern)?;
                session.collection_handles_filtered(objects, filter)
            } else {
                let objects = session.get_nets(pattern)?;
                session.collection_handles_filtered(objects, filter)
            }
        }
        ObjectQueryKind::Clock => {
            let objects = session.get_clocks(pattern)?;
            session.collection_handles_filtered(objects, filter)
        }
        ObjectQueryKind::Library => return Ok(session.library_names_matching(pattern)),
        ObjectQueryKind::LibraryCell => {
            return session
                .library_cell_names_matching(pattern)
                .map_err(Into::into);
        }
    };
    Ok(handles)
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
    let Some(spec) = root_property(property) else {
        return Err(crate::ShellError::command(format!(
            "set_db: unknown root property '{property}'"
        )));
    };
    if !spec.writable {
        return Err(crate::ShellError::command(format!(
            "set_db: root property '{property}' is read-only"
        )));
    }
    match spec.kind {
        RootPropertyKind::HdlSearchPath => Ok(state.session.borrow_mut().set_hdl_search_path(
            split_tcl_list(interp, value)?
                .into_iter()
                .map(PathBuf::from)
                .collect(),
        )),
        RootPropertyKind::LibSearchPath => Ok(state.session.borrow_mut().set_lib_search_path(
            split_tcl_list(interp, value)?
                .into_iter()
                .map(PathBuf::from)
                .collect(),
        )),
        RootPropertyKind::SynthEffort => {
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
        RootPropertyKind::ClockGating => Ok(state
            .session
            .borrow_mut()
            .set_clock_gating_enabled(parse_boolean(value.as_str())?)),
        RootPropertyKind::ClockGatingMinimumBitwidth => {
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
        RootPropertyKind::ClockGatingLatchBased => Ok(state
            .session
            .borrow_mut()
            .set_clock_gating_latch_based(parse_boolean(value.as_str())?)),
        RootPropertyKind::CurrentDesign => {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn root_property_schema_is_sorted_unique_and_capability_complete() {
        let mut names = BTreeSet::new();
        for pair in ROOT_PROPERTIES.windows(2) {
            assert!(
                pair[0].name < pair[1].name,
                "root properties must stay sorted"
            );
        }
        for property in ROOT_PROPERTIES {
            assert!(names.insert(property.name));
            assert!(property.readable || property.writable);
            assert!(root_property(property.name).is_some());
        }
    }

    #[test]
    fn object_query_schema_is_sorted_unique_and_capability_complete() {
        let mut names = BTreeSet::new();
        for pair in OBJECT_QUERIES.windows(2) {
            assert!(
                pair[0].name < pair[1].name,
                "object queries must stay sorted"
            );
        }
        for query in OBJECT_QUERIES {
            assert!(names.insert(query.name));
            assert_eq!(
                object_query(query.name).map(|found| found.kind),
                Some(query.kind)
            );
        }
        assert!(object_query("ports").unwrap().related);
        assert!(!object_query("libraries").unwrap().related);
        assert!(!object_query("lib_cells").unwrap().filter);
    }

    #[test]
    fn unknown_catalog_names_do_not_resolve() {
        assert!(root_property("not_a_property").is_none());
        assert!(object_query("not_a_class").is_none());
    }
}
