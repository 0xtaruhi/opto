// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

mod cell;
mod current;
mod table;
mod units;
mod wire_load;

use super::syntax::{
    Cursor, SourceSlice, Statement, StatementKind, SyntaxError, SyntaxErrorKind, Values,
};
use crate::parser::LibraryImport;
use crate::{
    LibraryError, LibrarySyntaxErrorKind, TargetCell, TargetPinDirection, TimingModelCounts,
    TimingModelKind, TimingThresholds,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

pub(super) struct ParseContext<'a> {
    source: &'a str,
    source_name: &'a str,
}

pub(super) struct Reader<'a, 'context> {
    cursor: Cursor<'a>,
    context: &'context ParseContext<'context>,
}

impl<'a, 'context> Reader<'a, 'context> {
    pub(super) fn new(source: SourceSlice<'a>, context: &'context ParseContext<'context>) -> Self {
        Self {
            cursor: Cursor::new(source),
            context,
        }
    }

    pub(super) fn next(&mut self) -> Result<Option<Statement<'a>>, LibraryError> {
        self.cursor
            .next_statement()
            .map_err(|error| self.context.syntax_error(&error))
    }
}

pub(super) struct GroupRef<'a> {
    pub(super) arguments: Values<'a>,
    pub(super) body: SourceSlice<'a>,
}

impl<'a> GroupRef<'a> {
    fn from_statement(statement: Statement<'a>) -> Option<Self> {
        let StatementKind::Group { arguments, body } = statement.kind else {
            return None;
        };
        Some(Self { arguments, body })
    }
}

struct LibraryIndex<'a> {
    name: String,
    default_fanout_load: f64,
    default_operating_conditions: Option<String>,
    default_wire_load: Option<String>,
    default_wire_load_mode: Option<String>,
    wire_loads: BTreeMap<String, crate::WireLoadModel>,
    templates: HashMap<String, table::TableTemplate>,
    power_templates: HashMap<String, table::TableTemplate>,
    ecsm_templates: HashMap<String, table::TableTemplate>,
    output_current_templates: HashSet<String>,
    timing_config: LibraryTimingConfig,
    bus_naming_style: String,
    bus_types: HashMap<String, BusType>,
    cells: Vec<GroupRef<'a>>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct BusType {
    bit_from: i32,
    bit_to: i32,
}

impl BusType {
    pub(super) fn lsb_first(self) -> impl Iterator<Item = i32> {
        self.bit_from.min(self.bit_to)..=self.bit_from.max(self.bit_to)
    }
}

#[derive(Debug, Default)]
pub(super) struct LibraryTimingConfig {
    pub(super) thresholds: TimingThresholds,
    units: units::UnitScales,
    nominal_voltage: Option<f64>,
}

impl LibraryTimingConfig {
    pub(super) fn ccs_charge_scale(&self) -> Result<f64, LibraryError> {
        let missing = |name: &'static str| LibraryError::InvalidTimingModel {
            model: "CCS",
            detail: format!("library attribute '{name}' is required"),
        };
        let time = self.units.time.ok_or_else(|| missing("time_unit"))?;
        let current = self.units.current.ok_or_else(|| missing("current_unit"))?;
        let capacitance = self
            .units
            .capacitance
            .ok_or_else(|| missing("capacitive_load_unit"))?;
        let voltage_unit = self.units.voltage.ok_or_else(|| missing("voltage_unit"))?;
        let nominal_voltage = self.nominal_voltage.ok_or_else(|| missing("nom_voltage"))?;
        if nominal_voltage <= 0.0 || !nominal_voltage.is_finite() {
            return Err(LibraryError::InvalidTimingModel {
                model: "CCS",
                detail: "nom_voltage must be positive and finite".to_string(),
            });
        }
        Ok(current * time / (capacitance * voltage_unit * nominal_voltage))
    }
}

pub(crate) fn parse_liberty(text: &str, source_name: &str) -> Result<LibraryImport, LibraryError> {
    let context = ParseContext {
        source: text,
        source_name,
    };
    let mut root = Reader::new(SourceSlice::new(text), &context);
    let statement = root
        .next()?
        .ok_or_else(|| LibraryError::MissingLibraryName {
            source_name: source_name.to_owned(),
        })?;
    if statement.name != "library" {
        return Err(context.unexpected_group(statement.offset, "library"));
    }
    let library = GroupRef::from_statement(statement)
        .ok_or_else(|| context.unexpected_group(0, "library group"))?;
    if root.next()?.is_some() {
        return Err(context.unexpected_group(text.len(), "end of file"));
    }
    let index = index_library(&library, &context)?;

    let mut table_builder = crate::lookup_table::LookupTableBuilder::default();
    let mut target_cells = Vec::with_capacity(index.cells.len());
    let mut power_cells = Vec::with_capacity(index.cells.len());
    let mut pin_count = Some(0usize);
    let cell_config = cell::CellParseConfig {
        templates: &index.templates,
        power_templates: &index.power_templates,
        ecsm_templates: &index.ecsm_templates,
        output_current_templates: &index.output_current_templates,
        timing: &index.timing_config,
        bus_naming_style: &index.bus_naming_style,
        bus_types: &index.bus_types,
    };
    for source in index.cells {
        let parsed = cell::parse_cell(&source, &cell_config, &mut table_builder, &context)?;
        pin_count = pin_count.and_then(|count| count.checked_add(parsed.target.pins.len()));
        target_cells.push(parsed.target);
        power_cells.push(parsed.power);
    }
    apply_default_fanout(&mut target_cells, index.default_fanout_load);
    let pin_count = pin_count.ok_or_else(|| LibraryError::PinCountCapacity {
        source_name: source_name.to_owned(),
    })?;
    let cell_count = target_cells.len();
    let units = crate::TimingLibraryUnits {
        time_seconds: index.timing_config.units.time,
        capacitance_farads: index.timing_config.units.capacitance,
        resistance_ohms: index.timing_config.units.resistance,
    };
    let power_units = crate::PowerLibraryUnits {
        time_seconds: index.timing_config.units.time,
        capacitance_farads: index.timing_config.units.capacitance,
        voltage_volts: index.timing_config.units.voltage,
        leakage_power_watts: index.timing_config.units.leakage_power,
        nominal_voltage: index.timing_config.nominal_voltage,
    };
    let mut timing_models = TimingModelCounts::default();
    for model in target_cells
        .iter()
        .flat_map(|cell| &cell.pins)
        .flat_map(|pin| &pin.timing_arcs)
        .filter_map(|arc| arc.delay_model.as_ref())
    {
        match model.kind() {
            TimingModelKind::Nldm => timing_models.nldm += 1,
            TimingModelKind::Ccs => timing_models.ccs += 1,
            TimingModelKind::Ecsm => timing_models.ecsm += 1,
        }
    }

    Ok(LibraryImport {
        name: index.name,
        source: source_name.to_owned(),
        default_operating_conditions: index.default_operating_conditions,
        default_wire_load: index.default_wire_load,
        default_wire_load_mode: index.default_wire_load_mode,
        wire_loads: index.wire_loads,
        units,
        power_units,
        target_cells: crate::TargetCellSet::try_from_cells(target_cells)?,
        power_cells: Arc::from(power_cells),
        timing_models,
        cell_count,
        pin_count,
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "library indexing is one exhaustive top-level Liberty statement dispatch"
)]
fn index_library<'a>(
    library: &GroupRef<'a>,
    context: &ParseContext<'_>,
) -> Result<LibraryIndex<'a>, LibraryError> {
    let name = required_value(&library.arguments, "library")?.into_owned();
    let mut index = LibraryIndex {
        name,
        default_fanout_load: 1.0,
        default_operating_conditions: None,
        default_wire_load: None,
        default_wire_load_mode: None,
        wire_loads: BTreeMap::new(),
        templates: HashMap::new(),
        power_templates: HashMap::new(),
        ecsm_templates: HashMap::new(),
        output_current_templates: HashSet::new(),
        timing_config: LibraryTimingConfig::default(),
        bus_naming_style: "%s[%d]".to_string(),
        bus_types: HashMap::new(),
        cells: Vec::new(),
    };
    let mut reader = Reader::new(library.body, context);
    while let Some(statement) = reader.next()? {
        match (statement.name, statement.kind) {
            ("default_fanout_load", StatementKind::Simple(values)) => {
                index.default_fanout_load = parse_number(&values, "default_fanout_load")?;
            }
            ("default_operating_conditions", StatementKind::Simple(values)) => {
                index.default_operating_conditions = optional_value(&values);
            }
            ("default_wire_load", StatementKind::Simple(values)) => {
                index.default_wire_load = optional_value(&values);
            }
            ("default_wire_load_mode", StatementKind::Simple(values)) => {
                index.default_wire_load_mode = optional_value(&values);
            }
            ("bus_naming_style", StatementKind::Simple(values)) => {
                index.bus_naming_style =
                    optional_value(&values).unwrap_or_else(|| "%s[%d]".to_string());
            }
            ("type", StatementKind::Group { arguments, body }) => {
                let (name, ty) = parse_bus_type(&GroupRef { arguments, body }, context)?;
                if index.bus_types.insert(name.clone(), ty).is_some() {
                    return Err(LibraryError::UnsupportedConstruct {
                        construct: format!("duplicate Liberty bus type '{name}'"),
                    });
                }
            }
            ("wire_load", StatementKind::Group { arguments, body }) => {
                let model = wire_load::parse_wire_load(&GroupRef { arguments, body }, context)?;
                let name = model.name.clone();
                if index.wire_loads.insert(name.clone(), model).is_some() {
                    return Err(LibraryError::InvalidWireLoad {
                        name,
                        detail: "duplicate wire_load group".to_string(),
                    });
                }
            }
            ("time_unit", StatementKind::Simple(values)) => {
                index.timing_config.units.time = Some(units::parse_scalar_unit(
                    &values,
                    "time_unit",
                    units::UnitDimension::Time,
                )?);
            }
            ("current_unit", StatementKind::Simple(values)) => {
                index.timing_config.units.current = Some(units::parse_scalar_unit(
                    &values,
                    "current_unit",
                    units::UnitDimension::Current,
                )?);
            }
            ("voltage_unit", StatementKind::Simple(values)) => {
                index.timing_config.units.voltage = Some(units::parse_scalar_unit(
                    &values,
                    "voltage_unit",
                    units::UnitDimension::Voltage,
                )?);
            }
            ("leakage_power_unit", StatementKind::Simple(values)) => {
                index.timing_config.units.leakage_power = Some(units::parse_scalar_unit(
                    &values,
                    "leakage_power_unit",
                    units::UnitDimension::Power,
                )?);
            }
            ("capacitive_load_unit", StatementKind::Complex(values)) => {
                index.timing_config.units.capacitance =
                    Some(units::parse_capacitance_unit(&values)?);
            }
            ("pulling_resistance_unit", StatementKind::Simple(values)) => {
                index.timing_config.units.resistance = Some(units::parse_scalar_unit(
                    &values,
                    "pulling_resistance_unit",
                    units::UnitDimension::Resistance,
                )?);
            }
            ("nom_voltage", StatementKind::Simple(values)) => {
                index.timing_config.nominal_voltage = Some(parse_number(&values, "nom_voltage")?);
            }
            (name, StatementKind::Simple(values)) if threshold_slot(name).is_some() => {
                let value = parse_number(&values, threshold_slot(name).unwrap().0)? / 100.0;
                let (_, target, edge) = threshold_slot(name).unwrap();
                let thresholds = &mut index.timing_config.thresholds;
                match target {
                    ThresholdTarget::Input => thresholds.input[edge.index()] = value,
                    ThresholdTarget::Output => thresholds.output[edge.index()] = value,
                    ThresholdTarget::SlewLower => thresholds.slew_lower[edge.index()] = value,
                    ThresholdTarget::SlewUpper => thresholds.slew_upper[edge.index()] = value,
                }
            }
            ("slew_derate_from_library", StatementKind::Simple(values)) => {
                index.timing_config.thresholds.slew_derate =
                    parse_number(&values, "slew_derate_from_library")?;
            }
            ("lu_table_template", StatementKind::Group { arguments, body }) => {
                let group = GroupRef { arguments, body };
                let (name, template) = table::parse_template(&group, "lu_table_template", context)?;
                _ = index.templates.insert(name, template);
            }
            ("power_lut_template", StatementKind::Group { arguments, body }) => {
                let group = GroupRef { arguments, body };
                let (name, template) =
                    table::parse_template(&group, "power_lut_template", context)?;
                _ = index.power_templates.insert(name, template);
            }
            ("ecsm_lut_template", StatementKind::Group { arguments, body }) => {
                let group = GroupRef { arguments, body };
                let (name, template) = table::parse_template(&group, "ecsm_lut_template", context)?;
                _ = index.ecsm_templates.insert(name, template);
            }
            ("output_current_template", StatementKind::Group { arguments, body }) => {
                let name =
                    current::parse_output_current_template(&GroupRef { arguments, body }, context)?;
                _ = index.output_current_templates.insert(name);
            }
            ("cell", StatementKind::Group { arguments, body }) => {
                index.cells.push(GroupRef { arguments, body });
            }
            ("include_file", _) => {
                return Err(LibraryError::UnsupportedConstruct {
                    construct: "include_file".to_string(),
                });
            }
            _ => {}
        }
    }
    Ok(index)
}

fn parse_bus_type(
    group: &GroupRef<'_>,
    context: &ParseContext<'_>,
) -> Result<(String, BusType), LibraryError> {
    let name = required_value(&group.arguments, "type")?.into_owned();
    let mut bit_from = None;
    let mut bit_to = None;
    let mut bit_width = None;
    let mut reader = Reader::new(group.body, context);
    while let Some(statement) = reader.next()? {
        let StatementKind::Simple(values) = statement.kind else {
            continue;
        };
        match statement.name {
            "bit_from" => bit_from = Some(parse_integer(&values, "bit_from")?),
            "bit_to" => bit_to = Some(parse_integer(&values, "bit_to")?),
            "bit_width" => bit_width = Some(parse_integer(&values, "bit_width")?),
            _ => {}
        }
    }
    let (bit_from, bit_to) = match (bit_from, bit_to, bit_width) {
        (Some(from), Some(to), _) => (from, to),
        (None, None, Some(width)) if width > 0 => (width - 1, 0),
        _ => {
            return Err(LibraryError::UnsupportedConstruct {
                construct: format!(
                    "bus type '{name}' requires bit_from/bit_to or positive bit_width"
                ),
            });
        }
    };
    Ok((name, BusType { bit_from, bit_to }))
}

#[derive(Clone, Copy)]
enum ThresholdTarget {
    Input,
    Output,
    SlewLower,
    SlewUpper,
}

fn threshold_slot(name: &str) -> Option<(&'static str, ThresholdTarget, crate::TimingEdge)> {
    use crate::TimingEdge::{Fall, Rise};
    match name {
        "input_threshold_pct_rise" => {
            Some(("input_threshold_pct_rise", ThresholdTarget::Input, Rise))
        }
        "input_threshold_pct_fall" => {
            Some(("input_threshold_pct_fall", ThresholdTarget::Input, Fall))
        }
        "output_threshold_pct_rise" => {
            Some(("output_threshold_pct_rise", ThresholdTarget::Output, Rise))
        }
        "output_threshold_pct_fall" => {
            Some(("output_threshold_pct_fall", ThresholdTarget::Output, Fall))
        }
        "slew_lower_threshold_pct_rise" => Some((
            "slew_lower_threshold_pct_rise",
            ThresholdTarget::SlewLower,
            Rise,
        )),
        "slew_lower_threshold_pct_fall" => Some((
            "slew_lower_threshold_pct_fall",
            ThresholdTarget::SlewLower,
            Fall,
        )),
        "slew_upper_threshold_pct_rise" => Some((
            "slew_upper_threshold_pct_rise",
            ThresholdTarget::SlewUpper,
            Rise,
        )),
        "slew_upper_threshold_pct_fall" => Some((
            "slew_upper_threshold_pct_fall",
            ThresholdTarget::SlewUpper,
            Fall,
        )),
        _ => None,
    }
}

fn apply_default_fanout(cells: &mut [TargetCell], default_fanout_load: f64) {
    for cell in cells {
        for pin in &mut cell.pins {
            if matches!(
                pin.direction,
                TargetPinDirection::Input | TargetPinDirection::Inout
            ) && pin.fanout_load.is_none()
            {
                pin.fanout_load = Some(default_fanout_load);
            }
        }
    }
}

pub(super) fn required_value<'a>(
    values: &Values<'a>,
    attribute: &'static str,
) -> Result<std::borrow::Cow<'a, str>, LibraryError> {
    values
        .first()
        .map(super::syntax::Value::decoded)
        .ok_or(LibraryError::MissingValue { attribute })
}

pub(super) fn optional_value(values: &Values<'_>) -> Option<String> {
    values.first().map(|value| value.decoded().into_owned())
}

pub(super) fn parse_number(
    values: &Values<'_>,
    attribute: &'static str,
) -> Result<f64, LibraryError> {
    let value = required_value(values, attribute)?;
    value
        .parse::<f64>()
        .map_err(|_| LibraryError::InvalidNumber {
            attribute,
            value: value.into_owned(),
        })
}

pub(super) fn parse_integer(
    values: &Values<'_>,
    attribute: &'static str,
) -> Result<i32, LibraryError> {
    let value = required_value(values, attribute)?;
    value
        .parse::<i32>()
        .map_err(|_| LibraryError::InvalidNumber {
            attribute,
            value: value.into_owned(),
        })
}

impl ParseContext<'_> {
    fn syntax_error(&self, error: &SyntaxError) -> LibraryError {
        let prefix = &self.source[..error.offset.min(self.source.len())];
        let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
        let column = prefix
            .rsplit_once('\n')
            .map_or(prefix.len(), |(_, tail)| tail.len())
            + 1;
        let kind = match error.kind {
            SyntaxErrorKind::InvalidToken => LibrarySyntaxErrorKind::InvalidToken,
            SyntaxErrorKind::UnexpectedEnd { expected } => {
                LibrarySyntaxErrorKind::UnexpectedEnd { expected }
            }
            SyntaxErrorKind::UnexpectedToken { expected, found } => {
                LibrarySyntaxErrorKind::UnexpectedToken { expected, found }
            }
        };
        LibraryError::Syntax {
            source_name: self.source_name.to_owned(),
            line,
            column,
            kind,
        }
    }

    fn unexpected_group(&self, offset: usize, expected: &'static str) -> LibraryError {
        self.syntax_error(&SyntaxError {
            offset,
            kind: SyntaxErrorKind::UnexpectedToken {
                expected,
                found: "different top-level construct",
            },
        })
    }
}
