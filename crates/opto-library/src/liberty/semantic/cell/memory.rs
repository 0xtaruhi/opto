// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::CellParseConfig;
use crate::liberty::semantic::{
    GroupRef, ParseContext, Reader, optional_value, parse_integer, required_value,
};
use crate::liberty::syntax::{SourceSlice, StatementKind};
use crate::lookup_table::LookupTableBuilder;
use crate::{
    BooleanFunction, LibraryError, TargetMemory, TargetMemoryClock, TargetMemoryDisabledRead,
    TargetMemoryEdge, TargetMemoryEnable, TargetMemoryKind, TargetMemoryReadDuringWrite,
    TargetMemoryReadPort, TargetMemoryWritePort, TargetPinDirection, TargetTimingArc,
};
use std::collections::BTreeMap;

pub(super) struct ParsedMemoryShape {
    kind: TargetMemoryKind,
    address_width: u32,
    word_width: u32,
}

struct ParsedMemoryWrite {
    address: String,
    clock: TargetMemoryClock,
    enable: Option<TargetMemoryEnable>,
}

pub(super) struct ParsedMemoryBus {
    name: String,
    pub(super) pins: Vec<String>,
    pub(super) direction: TargetPinDirection,
    pub(super) timing_arcs: Vec<TargetTimingArc>,
    read_address: Option<String>,
    write: Option<ParsedMemoryWrite>,
}

pub(super) fn parse_memory_shape(
    body: SourceSlice<'_>,
    context: &ParseContext<'_>,
) -> Result<ParsedMemoryShape, LibraryError> {
    let mut kind = None;
    let mut address_width = None;
    let mut word_width = None;
    let mut reader = Reader::new(body, context);
    while let Some(statement) = reader.next()? {
        let StatementKind::Simple(values) = statement.kind else {
            continue;
        };
        match statement.name {
            "type" => {
                kind = match optional_value(&values).as_deref() {
                    Some("ram") => Some(TargetMemoryKind::Ram),
                    Some("rom") => Some(TargetMemoryKind::Rom),
                    _ => {
                        return Err(LibraryError::UnsupportedConstruct {
                            construct: "memory type other than ram or rom".to_string(),
                        });
                    }
                };
            }
            "address_width" => {
                address_width = Some(positive_u32(
                    parse_integer(&values, "address_width")?,
                    "address_width",
                )?);
            }
            "word_width" => {
                word_width = Some(positive_u32(
                    parse_integer(&values, "word_width")?,
                    "word_width",
                )?);
            }
            _ => {}
        }
    }
    Ok(ParsedMemoryShape {
        kind: kind.ok_or(LibraryError::MissingValue { attribute: "type" })?,
        address_width: address_width.ok_or(LibraryError::MissingValue {
            attribute: "address_width",
        })?,
        word_width: word_width.ok_or(LibraryError::MissingValue {
            attribute: "word_width",
        })?,
    })
}

fn positive_u32(value: i32, attribute: &'static str) -> Result<u32, LibraryError> {
    u32::try_from(value)
        .ok()
        .filter(|value| *value != 0)
        .ok_or_else(|| LibraryError::InvalidNumber {
            attribute,
            value: value.to_string(),
        })
}

pub(super) fn parse_memory_bus(
    group: &GroupRef<'_>,
    config: &CellParseConfig<'_>,
    table_builder: &mut LookupTableBuilder,
    context: &ParseContext<'_>,
) -> Result<ParsedMemoryBus, LibraryError> {
    let name = required_value(&group.arguments, "bus")?.into_owned();
    let mut bus_type = None;
    let mut direction = TargetPinDirection::Internal;
    let mut read_address = None;
    let mut write = None;
    let mut timing_arcs = Vec::new();
    let mut reader = Reader::new(group.body, context);
    while let Some(statement) = reader.next()? {
        match (statement.name, statement.kind) {
            ("bus_type", StatementKind::Simple(values)) => {
                bus_type = optional_value(&values);
            }
            ("direction", StatementKind::Simple(values)) => {
                direction = match optional_value(&values).as_deref() {
                    Some("input") => TargetPinDirection::Input,
                    Some("output") => TargetPinDirection::Output,
                    Some("inout") => TargetPinDirection::Inout,
                    _ => TargetPinDirection::Internal,
                };
            }
            ("memory_read", StatementKind::Group { .. }) if read_address.is_some() => {
                return Err(LibraryError::UnsupportedConstruct {
                    construct: format!("duplicate memory_read group on bus '{name}'"),
                });
            }
            ("memory_read", StatementKind::Group { arguments: _, body }) => {
                read_address = Some(parse_memory_read(body, context)?);
            }
            ("memory_write", StatementKind::Group { .. }) if write.is_some() => {
                return Err(LibraryError::UnsupportedConstruct {
                    construct: format!("duplicate memory_write group on bus '{name}'"),
                });
            }
            ("memory_write", StatementKind::Group { arguments: _, body }) => {
                write = Some(parse_memory_write(body, context)?);
            }
            ("timing", StatementKind::Group { arguments, body }) => {
                timing_arcs.extend(super::parse_timing(
                    &GroupRef { arguments, body },
                    config,
                    table_builder,
                    context,
                )?);
            }
            _ => {}
        }
    }
    let bus_type = bus_type
        .as_deref()
        .and_then(|name| config.bus_types.get(name))
        .copied()
        .ok_or_else(|| LibraryError::UnsupportedConstruct {
            construct: format!("bus '{name}' references an unknown bus_type"),
        })?;
    let pins = bus_type
        .lsb_first()
        .map(|index| {
            config
                .bus_naming_style
                .replace("%s", &name)
                .replace("%d", &index.to_string())
        })
        .collect();
    Ok(ParsedMemoryBus {
        name,
        pins,
        direction,
        timing_arcs,
        read_address,
        write,
    })
}

fn parse_memory_read(
    body: SourceSlice<'_>,
    context: &ParseContext<'_>,
) -> Result<String, LibraryError> {
    let mut address = None;
    let mut reader = Reader::new(body, context);
    while let Some(statement) = reader.next()? {
        if statement.name == "address"
            && let StatementKind::Simple(values) = statement.kind
        {
            address = optional_value(&values);
        }
    }
    address.ok_or(LibraryError::MissingValue {
        attribute: "address",
    })
}

fn parse_memory_write(
    body: SourceSlice<'_>,
    context: &ParseContext<'_>,
) -> Result<ParsedMemoryWrite, LibraryError> {
    let mut address = None;
    let mut clock = None;
    let mut enable = None;
    let mut reader = Reader::new(body, context);
    while let Some(statement) = reader.next()? {
        let StatementKind::Simple(values) = statement.kind else {
            continue;
        };
        match statement.name {
            "address" => address = optional_value(&values),
            "clocked_on" => {
                clock = optional_value(&values)
                    .map(|expression| parse_control(&expression))
                    .transpose()?
                    .map(|(pin, active_high)| TargetMemoryClock {
                        pin,
                        edge: if active_high {
                            TargetMemoryEdge::Rising
                        } else {
                            TargetMemoryEdge::Falling
                        },
                    });
            }
            "enable" => {
                enable = optional_value(&values)
                    .map(|expression| parse_control(&expression))
                    .transpose()?
                    .map(|(pin, active_high)| TargetMemoryEnable { pin, active_high });
            }
            _ => {}
        }
    }
    Ok(ParsedMemoryWrite {
        address: address.ok_or(LibraryError::MissingValue {
            attribute: "address",
        })?,
        clock: clock.ok_or(LibraryError::MissingValue {
            attribute: "clocked_on",
        })?,
        enable,
    })
}

fn parse_control(expression: &str) -> Result<(String, bool), LibraryError> {
    match BooleanFunction::parse(expression)? {
        BooleanFunction::Pin(pin) => Ok((pin, true)),
        BooleanFunction::Not(argument) => match *argument {
            BooleanFunction::Pin(pin) => Ok((pin, false)),
            _ => Err(LibraryError::UnsupportedConstruct {
                construct: format!("compound memory control expression '{expression}'"),
            }),
        },
        _ => Err(LibraryError::UnsupportedConstruct {
            construct: format!("compound memory control expression '{expression}'"),
        }),
    }
}

pub(super) fn assemble_memory(
    shape: &ParsedMemoryShape,
    buses: &[ParsedMemoryBus],
    cell_name: &str,
) -> Result<TargetMemory, LibraryError> {
    let buses_by_name = buses
        .iter()
        .map(|bus| (bus.name.as_str(), bus))
        .collect::<BTreeMap<_, _>>();
    let address_pins = |name: &str| {
        buses_by_name
            .get(name)
            .map(|bus| bus.pins.clone())
            .ok_or_else(|| LibraryError::UnsupportedConstruct {
                construct: format!(
                    "memory cell '{cell_name}' references unknown address bus '{name}'"
                ),
            })
    };
    let read_ports = buses
        .iter()
        .filter_map(|bus| bus.read_address.as_deref().map(|address| (bus, address)))
        .map(|(bus, address)| {
            Ok(TargetMemoryReadPort {
                address_pins: address_pins(address)?,
                data_pins: bus.pins.clone(),
                clock: None,
                enable: None,
                disabled: TargetMemoryDisabledRead::Undefined,
                read_during_write: TargetMemoryReadDuringWrite::Undefined,
            })
        })
        .collect::<Result<Vec<_>, LibraryError>>()?;
    let write_ports = buses
        .iter()
        .filter_map(|bus| bus.write.as_ref().map(|write| (bus, write)))
        .map(|(bus, write)| {
            Ok(TargetMemoryWritePort {
                address_pins: address_pins(&write.address)?,
                data_pins: bus.pins.clone(),
                clock: write.clock.clone(),
                enable: write.enable.clone(),
                mask_pins: Vec::new(),
                mask_granularity: 0,
                mask_active_high: true,
            })
        })
        .collect::<Result<Vec<_>, LibraryError>>()?;
    let depth = 1u32.checked_shl(shape.address_width).ok_or_else(|| {
        LibraryError::UnsupportedConstruct {
            construct: format!(
                "memory cell '{cell_name}' address width exceeds representable depth"
            ),
        }
    })?;
    Ok(TargetMemory {
        kind: shape.kind,
        depth,
        word_width: shape.word_width,
        read_ports,
        write_ports,
    })
}
