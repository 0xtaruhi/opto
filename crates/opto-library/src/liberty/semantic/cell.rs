// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::current::parse_output_current;
use super::table::{
    ParsedTransition, TableTemplate, parse_float_list, parse_table, parse_transition,
};
use super::{
    BusType, GroupRef, LibraryTimingConfig, ParseContext, Reader, optional_value, parse_number,
    required_value,
};
use crate::lookup_table::LookupTableBuilder;
use crate::target_cells::target_timing_type;
use crate::{
    ArcDelayModel, BooleanFunction, CcsTimingModel, EcsmPinReceiverCapacitanceModel,
    EcsmTimingModel, InternalPower, LeakagePower, LibraryError, LookupTable, NldmTimingModel,
    PinPower, PinReceiverCapacitanceModel, PowerCell, ReceiverCapacitanceModel,
    SampledWaveformGrid, TargetCell, TargetCellUsage, TargetClockGateKind, TargetClockGateRole,
    TargetNextStateType, TargetPin, TargetPinDirection, TargetSequential, TargetSequentialKind,
    TargetTimingArc, TargetTimingType, TimingEdge, TimingSense,
};
use smallvec::SmallVec;
use std::collections::{HashMap, HashSet};

mod memory;

pub(super) struct CellParseConfig<'a> {
    pub(super) templates: &'a HashMap<String, TableTemplate>,
    pub(super) power_templates: &'a HashMap<String, TableTemplate>,
    pub(super) ecsm_templates: &'a HashMap<String, TableTemplate>,
    pub(super) output_current_templates: &'a HashSet<String>,
    pub(super) timing: &'a LibraryTimingConfig,
    pub(super) bus_naming_style: &'a str,
    pub(super) bus_types: &'a HashMap<String, BusType>,
}

pub(super) struct ParsedCell {
    pub(super) target: TargetCell,
    pub(super) power: PowerCell,
}

#[allow(
    clippy::too_many_lines,
    reason = "cell-group lowering is one exhaustive Liberty statement dispatch"
)]
pub(super) fn parse_cell(
    group: &GroupRef<'_>,
    config: &CellParseConfig<'_>,
    table_builder: &mut LookupTableBuilder,
    context: &ParseContext<'_>,
) -> Result<ParsedCell, LibraryError> {
    let name = required_value(&group.arguments, "cell")?.into_owned();
    let mut cell = TargetCell {
        dont_use: false,
        usage: TargetCellUsage::default(),
        name: name.clone(),
        area: None,
        pins: Vec::new(),
        sequential: Vec::new(),
        clock_gate: None,
        memory: None,
    };
    let mut power = PowerCell {
        name,
        cell_leakage_power: None,
        leakage_power: Vec::new(),
        pins: Vec::new(),
    };
    let mut memory_shape = None;
    let mut memory_buses = Vec::new();
    let mut reader = Reader::new(group.body, context);
    while let Some(statement) = reader.next()? {
        match (statement.name, statement.kind) {
            ("area", super::super::syntax::StatementKind::Simple(values)) => {
                cell.area = Some(parse_number(&values, "area")?);
            }
            ("dont_use", super::super::syntax::StatementKind::Simple(values)) => {
                cell.dont_use = optional_value(&values).as_deref() == Some("true");
            }
            ("always_on", super::super::syntax::StatementKind::Simple(values)) => {
                if optional_value(&values).as_deref() == Some("true") {
                    cell.usage.insert(TargetCellUsage::ALWAYS_ON);
                }
            }
            ("is_isolation_cell", super::super::syntax::StatementKind::Simple(values)) => {
                if optional_value(&values).as_deref() == Some("true") {
                    cell.usage.insert(TargetCellUsage::ISOLATION);
                }
            }
            ("is_level_shifter", super::super::syntax::StatementKind::Simple(values)) => {
                if optional_value(&values).as_deref() == Some("true") {
                    cell.usage.insert(TargetCellUsage::LEVEL_SHIFTER);
                }
            }
            (
                "clock_gating_integrated_cell",
                super::super::syntax::StatementKind::Simple(values),
            ) => {
                cell.usage.insert(TargetCellUsage::INTEGRATED_CLOCK_GATING);
                cell.clock_gate = optional_value(&values)
                    .as_deref()
                    .and_then(TargetClockGateKind::parse);
            }
            ("cell_leakage_power", super::super::syntax::StatementKind::Simple(values)) => {
                power.cell_leakage_power = Some(parse_number(&values, "cell_leakage_power")?);
            }
            (
                "leakage_power",
                super::super::syntax::StatementKind::Group { arguments: _, body },
            ) => {
                power
                    .leakage_power
                    .push(parse_leakage_power(body, context)?);
            }
            ("pin", super::super::syntax::StatementKind::Group { arguments, body }) => {
                let (pin, pin_power) = parse_pin(
                    &GroupRef { arguments, body },
                    config,
                    table_builder,
                    context,
                )?;
                cell.pins.push(pin);
                if let Some(pin_power) = pin_power {
                    power.pins.push(pin_power);
                }
            }
            ("ff", super::super::syntax::StatementKind::Group { arguments, body }) => {
                cell.sequential.push(parse_sequential(
                    &GroupRef { arguments, body },
                    TargetSequentialKind::FlipFlop,
                    context,
                )?);
            }
            ("latch", super::super::syntax::StatementKind::Group { arguments, body }) => {
                cell.sequential.push(parse_sequential(
                    &GroupRef { arguments, body },
                    TargetSequentialKind::Latch,
                    context,
                )?);
            }
            ("memory", super::super::syntax::StatementKind::Group { arguments: _, body }) => {
                if memory_shape
                    .replace(memory::parse_memory_shape(body, context)?)
                    .is_some()
                {
                    return Err(LibraryError::UnsupportedConstruct {
                        construct: format!("duplicate memory group in cell '{}'", cell.name),
                    });
                }
            }
            ("bus", super::super::syntax::StatementKind::Group { arguments, body }) => {
                let bus = memory::parse_memory_bus(
                    &GroupRef { arguments, body },
                    config,
                    table_builder,
                    context,
                )?;
                cell.pins.extend(bus.pins.iter().map(|name| TargetPin {
                    name: name.clone(),
                    direction: bus.direction,
                    function: None,
                    three_state: None,
                    capacitance: None,
                    rise_capacitance: None,
                    fall_capacitance: None,
                    receiver_capacitance: None,
                    fanout_load: None,
                    next_state_type: None,
                    clock_gate_role: None,
                    timing_arcs: bus.timing_arcs.clone(),
                }));
                memory_buses.push(bus);
            }
            _ => {}
        }
    }
    cell.memory = memory_shape
        .map(|shape| memory::assemble_memory(&shape, &memory_buses, &cell.name))
        .transpose()?;
    Ok(ParsedCell {
        target: cell,
        power,
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "pin-group lowering keeps all mutually interacting electrical attributes in one dispatch"
)]
fn parse_pin(
    group: &GroupRef<'_>,
    config: &CellParseConfig<'_>,
    table_builder: &mut LookupTableBuilder,
    context: &ParseContext<'_>,
) -> Result<(TargetPin, Option<PinPower>), LibraryError> {
    let name = required_value(&group.arguments, "pin")?.into_owned();
    let mut pin = TargetPin {
        name: name.clone(),
        direction: TargetPinDirection::Internal,
        function: None,
        three_state: None,
        capacitance: None,
        rise_capacitance: None,
        fall_capacitance: None,
        receiver_capacitance: None,
        fanout_load: None,
        next_state_type: None,
        clock_gate_role: None,
        timing_arcs: Vec::new(),
    };
    let mut internal_power = Vec::new();
    let mut ccs_receiver = None;
    let mut ecsm_receiver = EcsmPinReceiverCapacitanceModel::default();
    let mut reader = Reader::new(group.body, context);
    while let Some(statement) = reader.next()? {
        match (statement.name, statement.kind) {
            ("direction", super::super::syntax::StatementKind::Simple(values)) => {
                pin.direction = match optional_value(&values).as_deref() {
                    Some("input") => TargetPinDirection::Input,
                    Some("output") => TargetPinDirection::Output,
                    Some("inout") => TargetPinDirection::Inout,
                    _ => TargetPinDirection::Internal,
                };
            }
            ("function", super::super::syntax::StatementKind::Simple(values)) => {
                pin.function = parse_boolean(&values, "function")?;
            }
            ("three_state", super::super::syntax::StatementKind::Simple(values)) => {
                pin.three_state = parse_boolean(&values, "three_state")?;
            }
            ("capacitance", super::super::syntax::StatementKind::Simple(values)) => {
                pin.capacitance = Some(parse_number(&values, "capacitance")?);
            }
            ("rise_capacitance", super::super::syntax::StatementKind::Simple(values)) => {
                pin.rise_capacitance = Some(parse_number(&values, "rise_capacitance")?);
            }
            ("fall_capacitance", super::super::syntax::StatementKind::Simple(values)) => {
                pin.fall_capacitance = Some(parse_number(&values, "fall_capacitance")?);
            }
            (
                "receiver_capacitance",
                super::super::syntax::StatementKind::Group { arguments: _, body },
            ) => {
                if ccs_receiver.is_some() {
                    return Err(invalid_receiver("duplicate pin-level receiver_capacitance"));
                }
                let receiver =
                    parse_receiver_capacitance(body, config.templates, table_builder, context)?;
                receiver.validate("CCS")?;
                if receiver.depends_on_output_load() {
                    return Err(invalid_receiver(
                        "pin-level CCS receiver capacitance cannot depend on an output load",
                    ));
                }
                ccs_receiver = Some(receiver);
            }
            (
                "ecsm_capacitance",
                super::super::syntax::StatementKind::Group { arguments, body },
            ) => {
                let (edge, table) = parse_pin_ecsm_capacitance(
                    &GroupRef { arguments, body },
                    table_builder,
                    context,
                )?;
                merge_pin_ecsm_capacitance(&mut ecsm_receiver, edge, table)?;
            }
            ("fanout_load", super::super::syntax::StatementKind::Simple(values)) => {
                pin.fanout_load = Some(parse_number(&values, "fanout_load")?);
            }
            ("clock_gate_clock_pin", super::super::syntax::StatementKind::Simple(values)) => {
                if optional_value(&values).as_deref() == Some("true") {
                    pin.clock_gate_role = Some(TargetClockGateRole::Clock);
                }
            }
            ("clock_gate_enable_pin", super::super::syntax::StatementKind::Simple(values)) => {
                if optional_value(&values).as_deref() == Some("true") {
                    pin.clock_gate_role = Some(TargetClockGateRole::Enable);
                }
            }
            ("clock_gate_out_pin", super::super::syntax::StatementKind::Simple(values)) => {
                if optional_value(&values).as_deref() == Some("true") {
                    pin.clock_gate_role = Some(TargetClockGateRole::Output);
                }
            }
            ("clock_gate_test_pin", super::super::syntax::StatementKind::Simple(values)) => {
                if optional_value(&values).as_deref() == Some("true") {
                    pin.clock_gate_role = Some(TargetClockGateRole::TestEnable);
                }
            }
            ("nextstate_type", super::super::syntax::StatementKind::Simple(values)) => {
                pin.next_state_type = match optional_value(&values).as_deref() {
                    Some("data") => Some(TargetNextStateType::Data),
                    Some("preset") => Some(TargetNextStateType::Preset),
                    Some("clear") => Some(TargetNextStateType::Clear),
                    Some("load") => Some(TargetNextStateType::Load),
                    Some("scan_in") => Some(TargetNextStateType::ScanIn),
                    Some("scan_enable") => Some(TargetNextStateType::ScanEnable),
                    _ => None,
                };
            }
            ("timing", super::super::syntax::StatementKind::Group { arguments, body }) => {
                pin.timing_arcs.extend(parse_timing(
                    &GroupRef { arguments, body },
                    config,
                    table_builder,
                    context,
                )?);
            }
            (
                "internal_power",
                super::super::syntax::StatementKind::Group { arguments: _, body },
            ) => {
                internal_power.push(parse_internal_power(
                    body,
                    config.power_templates,
                    table_builder,
                    context,
                )?);
            }
            _ => {}
        }
    }
    if ccs_receiver.is_some() && !ecsm_receiver.is_empty() {
        return Err(invalid_receiver(
            "a pin cannot select both CCS and ECSM receiver capacitance models",
        ));
    }
    ecsm_receiver.validate()?;
    pin.receiver_capacitance = ccs_receiver
        .map(PinReceiverCapacitanceModel::Ccs)
        .or_else(|| {
            (!ecsm_receiver.is_empty()).then_some(PinReceiverCapacitanceModel::Ecsm(ecsm_receiver))
        });
    let power = (!internal_power.is_empty()).then_some(PinPower {
        name,
        internal_power,
    });
    Ok((pin, power))
}

fn parse_leakage_power(
    body: super::super::syntax::SourceSlice<'_>,
    context: &ParseContext<'_>,
) -> Result<LeakagePower, LibraryError> {
    let mut when = None;
    let mut value = None;
    let mut reader = Reader::new(body, context);
    while let Some(statement) = reader.next()? {
        let super::super::syntax::StatementKind::Simple(values) = statement.kind else {
            continue;
        };
        match statement.name {
            "when" => when = parse_boolean(&values, "when")?,
            "value" => value = Some(parse_number(&values, "value")?),
            _ => {}
        }
    }
    Ok(LeakagePower {
        when,
        value: value.ok_or(LibraryError::MissingValue { attribute: "value" })?,
    })
}

fn parse_internal_power(
    body: super::super::syntax::SourceSlice<'_>,
    templates: &HashMap<String, TableTemplate>,
    table_builder: &mut LookupTableBuilder,
    context: &ParseContext<'_>,
) -> Result<InternalPower, LibraryError> {
    let mut power = InternalPower {
        related_pin: None,
        when: None,
        rise_power: None,
        fall_power: None,
    };
    let mut reader = Reader::new(body, context);
    while let Some(statement) = reader.next()? {
        match (statement.name, statement.kind) {
            ("related_pin", super::super::syntax::StatementKind::Simple(values)) => {
                power.related_pin = optional_value(&values);
            }
            ("when", super::super::syntax::StatementKind::Simple(values)) => {
                power.when = parse_boolean(&values, "when")?;
            }
            (
                name @ ("rise_power" | "fall_power"),
                super::super::syntax::StatementKind::Group { arguments, body },
            ) => {
                let table = parse_table(
                    &GroupRef { arguments, body },
                    templates,
                    table_builder,
                    context,
                )?;
                match name {
                    "rise_power" => power.rise_power = table,
                    "fall_power" => power.fall_power = table,
                    _ => unreachable!("power table name pattern is exhaustive"),
                }
            }
            _ => {}
        }
    }
    Ok(power)
}

struct TimingDraft {
    related_pins: SmallVec<[String; 1]>,
    timing_type: Option<String>,
    timing_sense: TimingSense,
    cell_rise: Option<LookupTable>,
    cell_fall: Option<LookupTable>,
    rise_transition: Option<LookupTable>,
    fall_transition: Option<LookupTable>,
    output_current_rise: Option<SampledWaveformGrid>,
    output_current_fall: Option<SampledWaveformGrid>,
    ecsm_waveform_rise: Option<SampledWaveformGrid>,
    ecsm_waveform_fall: Option<SampledWaveformGrid>,
    ecsm_capacitance_rise: Option<LookupTable>,
    ecsm_capacitance_fall: Option<LookupTable>,
    receiver_capacitance: ReceiverCapacitanceModel,
    rise_constraint: Option<LookupTable>,
    fall_constraint: Option<LookupTable>,
}

impl Default for TimingDraft {
    fn default() -> Self {
        Self {
            related_pins: SmallVec::new(),
            timing_type: None,
            timing_sense: TimingSense::NonUnate,
            cell_rise: None,
            cell_fall: None,
            rise_transition: None,
            fall_transition: None,
            output_current_rise: None,
            output_current_fall: None,
            ecsm_waveform_rise: None,
            ecsm_waveform_fall: None,
            ecsm_capacitance_rise: None,
            ecsm_capacitance_fall: None,
            receiver_capacitance: ReceiverCapacitanceModel::default(),
            rise_constraint: None,
            fall_constraint: None,
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "timing-group lowering is an exhaustive model-attribute dispatch with shared validation"
)]
fn parse_timing(
    group: &GroupRef<'_>,
    config: &CellParseConfig<'_>,
    table_builder: &mut LookupTableBuilder,
    context: &ParseContext<'_>,
) -> Result<Vec<TargetTimingArc>, LibraryError> {
    let mut draft = TimingDraft::default();
    let mut reader = Reader::new(group.body, context);
    while let Some(statement) = reader.next()? {
        match (statement.name, statement.kind) {
            ("related_pin", super::super::syntax::StatementKind::Simple(values)) => {
                if let Some(value) = optional_value(&values) {
                    draft
                        .related_pins
                        .extend(value.split_whitespace().map(str::to_owned));
                }
            }
            ("timing_type", super::super::syntax::StatementKind::Simple(values)) => {
                draft.timing_type = optional_value(&values);
            }
            ("timing_sense", super::super::syntax::StatementKind::Simple(values)) => {
                draft.timing_sense = match optional_value(&values).as_deref() {
                    Some("positive_unate") => TimingSense::PositiveUnate,
                    Some("negative_unate") => TimingSense::NegativeUnate,
                    _ => TimingSense::NonUnate,
                };
            }
            (
                name @ ("cell_rise" | "cell_fall" | "rise_constraint" | "fall_constraint"),
                super::super::syntax::StatementKind::Group { arguments, body },
            ) => {
                let table = parse_table(
                    &GroupRef { arguments, body },
                    config.templates,
                    table_builder,
                    context,
                )?;
                match name {
                    "cell_rise" => draft.cell_rise = table,
                    "cell_fall" => draft.cell_fall = table,
                    "rise_constraint" => draft.rise_constraint = table,
                    "fall_constraint" => draft.fall_constraint = table,
                    _ => unreachable!("table name pattern is exhaustive"),
                }
            }
            (
                name @ ("rise_transition" | "fall_transition"),
                super::super::syntax::StatementKind::Group { arguments, body },
            ) => {
                let parsed = parse_transition(
                    &GroupRef { arguments, body },
                    config.templates,
                    config.ecsm_templates,
                    table_builder,
                    context,
                )?;
                apply_transition(&mut draft, name, parsed);
            }
            (
                name @ ("output_current_rise" | "output_current_fall"),
                super::super::syntax::StatementKind::Group { arguments, body },
            ) => {
                let grid = parse_output_current(
                    &GroupRef { arguments, body },
                    config.output_current_templates,
                    table_builder,
                    context,
                )?;
                match name {
                    "output_current_rise" => draft.output_current_rise = Some(grid),
                    "output_current_fall" => draft.output_current_fall = Some(grid),
                    _ => unreachable!("output current name pattern is exhaustive"),
                }
            }
            (
                name @ ("receiver_capacitance1_rise"
                | "receiver_capacitance1_fall"
                | "receiver_capacitance2_rise"
                | "receiver_capacitance2_fall"),
                super::super::syntax::StatementKind::Group { arguments, body },
            ) => {
                let table = parse_table(
                    &GroupRef { arguments, body },
                    config.templates,
                    table_builder,
                    context,
                )?;
                set_receiver_table(&mut draft.receiver_capacitance, name, table);
            }
            (
                "compact_ccs_rise" | "compact_ccs_fall",
                super::super::syntax::StatementKind::Group { .. },
            ) => {
                return Err(LibraryError::UnsupportedConstruct {
                    construct: "compact CCS timing".to_string(),
                });
            }
            _ => {}
        }
    }

    let Some(timing_type) = target_timing_type(draft.timing_type.as_deref()) else {
        let timing_type = draft.timing_type.unwrap_or_else(|| "<missing>".to_owned());
        return Err(LibraryError::UnsupportedConstruct {
            construct: format!("timing type '{timing_type}'"),
        });
    };
    if draft.related_pins.is_empty() {
        draft.related_pins.push(String::new());
    }
    let has_delay_model = matches!(
        timing_type,
        TargetTimingType::Combinational
            | TargetTimingType::ClockToQ(_)
            | TargetTimingType::Clear
            | TargetTimingType::Preset
            | TargetTimingType::ThreeStateEnable
            | TargetTimingType::ThreeStateDisable
    );
    let delay_model = has_delay_model
        .then(|| build_delay_model(&mut draft, config.timing))
        .transpose()?
        .flatten();
    let (rise_constraint, fall_constraint) = match timing_type {
        TargetTimingType::Check { .. }
        | TargetTimingType::Recovery(_)
        | TargetTimingType::Removal(_)
        | TargetTimingType::MinPulseWidth
        | TargetTimingType::NonSequentialSetup(_)
        | TargetTimingType::NonSequentialHold(_) => (draft.rise_constraint, draft.fall_constraint),
        TargetTimingType::Combinational
        | TargetTimingType::ClockToQ(_)
        | TargetTimingType::Clear
        | TargetTimingType::Preset
        | TargetTimingType::ThreeStateEnable
        | TargetTimingType::ThreeStateDisable => (None, None),
    };
    Ok(draft
        .related_pins
        .into_iter()
        .map(|related_pin| TargetTimingArc {
            related_pin,
            timing_type,
            timing_sense: draft.timing_sense,
            delay_model: delay_model.clone(),
            rise_constraint: rise_constraint.clone(),
            fall_constraint: fall_constraint.clone(),
        })
        .collect())
}

fn apply_transition(draft: &mut TimingDraft, name: &str, parsed: ParsedTransition) {
    match name {
        "rise_transition" => {
            draft.rise_transition = parsed.table;
            draft.ecsm_waveform_rise = parsed.waveforms;
            draft.ecsm_capacitance_rise = parsed.capacitance;
        }
        "fall_transition" => {
            draft.fall_transition = parsed.table;
            draft.ecsm_waveform_fall = parsed.waveforms;
            draft.ecsm_capacitance_fall = parsed.capacitance;
        }
        _ => unreachable!("transition name pattern is exhaustive"),
    }
}

fn build_delay_model(
    draft: &mut TimingDraft,
    config: &LibraryTimingConfig,
) -> Result<Option<ArcDelayModel>, LibraryError> {
    let has_ccs = draft.output_current_rise.is_some()
        || draft.output_current_fall.is_some()
        || !draft.receiver_capacitance.is_empty();
    let has_ecsm = draft.ecsm_waveform_rise.is_some()
        || draft.ecsm_waveform_fall.is_some()
        || draft.ecsm_capacitance_rise.is_some()
        || draft.ecsm_capacitance_fall.is_some();
    if has_ccs && has_ecsm {
        return Err(LibraryError::InvalidTimingModel {
            model: "CCS/ECSM",
            detail: "a timing arc cannot select both CCS and ECSM waveform models".to_string(),
        });
    }
    if has_ccs {
        if draft.output_current_rise.is_none() && draft.output_current_fall.is_none() {
            return Err(LibraryError::InvalidTimingModel {
                model: "CCS",
                detail: "receiver capacitance data requires an output current waveform".to_string(),
            });
        }
        validate_advanced_scalar_edges(
            "CCS",
            draft.output_current_rise.as_ref(),
            draft.output_current_fall.as_ref(),
            draft,
        )?;
        let scalar = take_scalar_model(draft);
        return Ok(Some(ArcDelayModel::Ccs(CcsTimingModel::new(
            config.thresholds,
            config.ccs_charge_scale()?,
            scalar,
            std::mem::take(&mut draft.receiver_capacitance),
            draft.output_current_rise.take(),
            draft.output_current_fall.take(),
        )?)));
    }
    if has_ecsm {
        if draft.ecsm_waveform_rise.is_none() && draft.ecsm_waveform_fall.is_none() {
            return Err(LibraryError::InvalidTimingModel {
                model: "ECSM",
                detail: "effective capacitance data requires a voltage-time waveform".to_string(),
            });
        }
        validate_advanced_scalar_edges(
            "ECSM",
            draft.ecsm_waveform_rise.as_ref(),
            draft.ecsm_waveform_fall.as_ref(),
            draft,
        )?;
        let scalar = take_scalar_model(draft);
        return Ok(Some(ArcDelayModel::Ecsm(EcsmTimingModel::new(
            config.thresholds,
            scalar,
            draft.ecsm_waveform_rise.take(),
            draft.ecsm_waveform_fall.take(),
            draft.ecsm_capacitance_rise.take(),
            draft.ecsm_capacitance_fall.take(),
        )?)));
    }
    let has_nldm = draft.cell_rise.is_some()
        || draft.cell_fall.is_some()
        || draft.rise_transition.is_some()
        || draft.fall_transition.is_some();
    Ok(has_nldm.then(|| ArcDelayModel::Nldm(take_scalar_model(draft))))
}

fn take_scalar_model(draft: &mut TimingDraft) -> NldmTimingModel {
    NldmTimingModel::new(
        draft.cell_rise.take(),
        draft.cell_fall.take(),
        draft.rise_transition.take(),
        draft.fall_transition.take(),
    )
}

fn validate_advanced_scalar_edges(
    model: &'static str,
    rise: Option<&SampledWaveformGrid>,
    fall: Option<&SampledWaveformGrid>,
    draft: &TimingDraft,
) -> Result<(), LibraryError> {
    for (edge, advanced, delay, transition) in [
        ("rise", rise, &draft.cell_rise, &draft.rise_transition),
        ("fall", fall, &draft.cell_fall, &draft.fall_transition),
    ] {
        match advanced {
            None if delay.is_some() || transition.is_some() => {
                return Err(LibraryError::InvalidTimingModel {
                    model,
                    detail: format!(
                        "a timing arc cannot mix a {model} waveform on one edge with scalar {edge} tables"
                    ),
                });
            }
            Some(_) if delay.is_none() || transition.is_none() => {
                return Err(LibraryError::InvalidTimingModel {
                    model,
                    detail: format!(
                        "a {model} {edge} waveform requires both scalar cell delay and transition tables"
                    ),
                });
            }
            None | Some(_) => {}
        }
    }
    Ok(())
}

fn parse_receiver_capacitance(
    body: super::super::syntax::SourceSlice<'_>,
    templates: &HashMap<String, TableTemplate>,
    table_builder: &mut LookupTableBuilder,
    context: &ParseContext<'_>,
) -> Result<ReceiverCapacitanceModel, LibraryError> {
    let mut receiver = ReceiverCapacitanceModel::default();
    let mut reader = Reader::new(body, context);
    while let Some(statement) = reader.next()? {
        let (
            name @ ("receiver_capacitance1_rise"
            | "receiver_capacitance1_fall"
            | "receiver_capacitance2_rise"
            | "receiver_capacitance2_fall"),
            super::super::syntax::StatementKind::Group { arguments, body },
        ) = (statement.name, statement.kind)
        else {
            continue;
        };
        let table = parse_table(
            &GroupRef { arguments, body },
            templates,
            table_builder,
            context,
        )?;
        set_receiver_table(&mut receiver, name, table);
    }
    Ok(receiver)
}

fn parse_pin_ecsm_capacitance(
    group: &GroupRef<'_>,
    table_builder: &mut LookupTableBuilder,
    context: &ParseContext<'_>,
) -> Result<(TimingEdge, LookupTable), LibraryError> {
    let edge = match required_value(&group.arguments, "ecsm_capacitance")?.as_ref() {
        "rise" => TimingEdge::Rise,
        "fall" => TimingEdge::Fall,
        value => {
            return Err(invalid_receiver(format!(
                "unsupported pin-level ecsm_capacitance edge '{value}'"
            )));
        }
    };
    let mut index_1 = Vec::new();
    let mut values = Vec::new();
    let mut reader = Reader::new(group.body, context);
    while let Some(statement) = reader.next()? {
        match (statement.name, statement.kind) {
            (
                "index_1",
                super::super::syntax::StatementKind::Simple(items)
                | super::super::syntax::StatementKind::Complex(items),
            ) => index_1 = parse_float_list(&items, "index_1")?,
            (
                "values",
                super::super::syntax::StatementKind::Simple(items)
                | super::super::syntax::StatementKind::Complex(items),
            ) => values = parse_float_list(&items, "values")?,
            _ => {}
        }
    }
    Ok((edge, table_builder.build(&index_1, &[], &values)))
}

fn merge_pin_ecsm_capacitance(
    receiver: &mut EcsmPinReceiverCapacitanceModel,
    edge: TimingEdge,
    table: LookupTable,
) -> Result<(), LibraryError> {
    let candidate = match edge {
        TimingEdge::Rise => EcsmPinReceiverCapacitanceModel {
            rise: Some(table.clone()),
            fall: None,
        },
        TimingEdge::Fall => EcsmPinReceiverCapacitanceModel {
            rise: None,
            fall: Some(table.clone()),
        },
    };
    candidate.validate()?;
    let slot = match edge {
        TimingEdge::Rise => &mut receiver.rise,
        TimingEdge::Fall => &mut receiver.fall,
    };
    *slot = Some(match slot.take() {
        Some(existing) => existing.pointwise_max(&table).ok_or_else(|| {
            invalid_receiver("duplicate pin-level ECSM capacitance tables use incompatible axes")
        })?,
        None => table,
    });
    Ok(())
}

fn invalid_receiver(detail: impl Into<String>) -> LibraryError {
    LibraryError::InvalidTimingModel {
        model: "receiver capacitance",
        detail: detail.into(),
    }
}

fn set_receiver_table(
    receiver: &mut ReceiverCapacitanceModel,
    name: &str,
    table: Option<LookupTable>,
) {
    match name {
        "receiver_capacitance1_rise" => receiver.segment_1_rise = table,
        "receiver_capacitance1_fall" => receiver.segment_1_fall = table,
        "receiver_capacitance2_rise" => receiver.segment_2_rise = table,
        "receiver_capacitance2_fall" => receiver.segment_2_fall = table,
        _ => unreachable!("receiver capacitance name pattern is exhaustive"),
    }
}

fn parse_sequential(
    group: &GroupRef<'_>,
    kind: TargetSequentialKind,
    context: &ParseContext<'_>,
) -> Result<TargetSequential, LibraryError> {
    let state_variables = group
        .arguments
        .iter()
        .map(|value| value.decoded().into_owned())
        .filter(|value| !value.is_empty())
        .collect();
    let mut sequential = TargetSequential {
        kind,
        state_variables,
        clocked_on: None,
        next_state: None,
        enable: None,
        clear: None,
        preset: None,
    };
    let mut reader = Reader::new(group.body, context);
    while let Some(statement) = reader.next()? {
        let super::super::syntax::StatementKind::Simple(values) = statement.kind else {
            continue;
        };
        match (kind, statement.name) {
            (TargetSequentialKind::FlipFlop, "clocked_on") => {
                sequential.clocked_on = parse_boolean(&values, "clocked_on")?;
            }
            (TargetSequentialKind::FlipFlop, "next_state")
            | (TargetSequentialKind::Latch, "data_in") => {
                sequential.next_state = parse_boolean(&values, "next_state")?;
            }
            (TargetSequentialKind::Latch, "enable") => {
                sequential.enable = parse_boolean(&values, "enable")?;
            }
            (_, "clear") => sequential.clear = parse_boolean(&values, "clear")?,
            (_, "preset") => sequential.preset = parse_boolean(&values, "preset")?,
            _ => {}
        }
    }
    Ok(sequential)
}

fn parse_boolean(
    values: &super::super::syntax::Values<'_>,
    attribute: &'static str,
) -> Result<Option<BooleanFunction>, LibraryError> {
    let value = required_value(values, attribute)?;
    BooleanFunction::parse(&value).map(Some)
}
