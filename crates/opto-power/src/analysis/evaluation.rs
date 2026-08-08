// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::activity::{function_probability, function_variables, validate_variables};
use super::state::NetActivity;
use super::{CellPowerValue, SwitchingActivity};
use crate::PowerError;
use opto_library::{PowerCell, TargetCellRef, TargetPinDirection};
use opto_timing::{TimingElectricalSnapshot, TimingInstanceRef, TimingNetId};

pub(super) struct CellCalculationContext<'a> {
    pub(super) activities: &'a [NetActivity],
    pub(super) electrical: &'a TimingElectricalSnapshot,
    pub(super) net_switching_watts: &'a [f64],
    pub(super) dynamic_power_unit_watts: f64,
    pub(super) leakage_power_unit_watts: f64,
}

pub(super) fn switching_power(
    capacitance: f64,
    activity: SwitchingActivity,
    capacitance_unit_farads: f64,
    voltage: f64,
    time_unit_seconds: f64,
) -> f64 {
    0.5 * capacitance * capacitance_unit_farads * voltage * voltage * activity.toggle_rate
        / time_unit_seconds
}

pub(super) fn calculate_cell_power(
    instance: TimingInstanceRef<'_>,
    target: TargetCellRef<'_>,
    power: Option<&PowerCell>,
    context: &CellCalculationContext<'_>,
) -> Result<CellPowerValue, PowerError> {
    let pin_activity = |pin: &str| {
        pin_net(instance, pin)
            .and_then(|net| context.activities.get(index(net)))
            .map(|activity| activity.value)
    };
    let internal_watts = power.map_or(Ok(0.0), |power| {
        internal_power(
            power,
            instance,
            pin_activity,
            context.electrical,
            context.dynamic_power_unit_watts,
        )
    })?;
    let leakage_watts = power.map_or(Ok(0.0), |power| {
        leakage_power(power, pin_activity, context.leakage_power_unit_watts)
    })?;
    let switching_watts = target
        .pins()
        .filter(|pin| {
            matches!(
                pin.direction(),
                TargetPinDirection::Output | TargetPinDirection::Inout
            )
        })
        .filter_map(|pin| pin_net(instance, pin.name()))
        .filter_map(|net| context.net_switching_watts.get(index(net)))
        .sum();
    Ok(CellPowerValue {
        internal: internal_watts,
        switching: switching_watts,
        leakage: leakage_watts,
    })
}

fn leakage_power(
    cell: &PowerCell,
    input: impl Copy + Fn(&str) -> Option<SwitchingActivity>,
    unit_watts: f64,
) -> Result<f64, PowerError> {
    if cell.leakage_power.is_empty() {
        return Ok(cell.cell_leakage_power.unwrap_or(0.0) * unit_watts);
    }
    cell.leakage_power
        .iter()
        .map(|group| {
            group
                .when
                .as_ref()
                .map_or(Ok(1.0), |condition| {
                    let variables = function_variables(condition);
                    validate_variables(&variables, input)?;
                    function_probability(condition, &variables, input)
                })
                .map(|probability| probability * group.value * unit_watts)
        })
        .sum()
}

fn internal_power(
    cell: &PowerCell,
    instance: TimingInstanceRef<'_>,
    input: impl Copy + Fn(&str) -> Option<SwitchingActivity>,
    electrical: &TimingElectricalSnapshot,
    unit_watts: f64,
) -> Result<f64, PowerError> {
    let mut total = 0.0;
    for pin in &cell.pins {
        let output_net =
            pin_net(instance, &pin.name).ok_or_else(|| PowerError::MissingPinConnection {
                cell: instance.name().into_owned(),
                pin: pin.name.clone(),
            })?;
        let output_activity = input(&pin.name).unwrap_or_else(SwitchingActivity::quiescent);
        let output_load = electrical.get(output_net).map(|net| net.capacitance);
        for group in &pin.internal_power {
            let input_transition = group
                .related_pin
                .as_deref()
                .and_then(|related| pin_net(instance, related))
                .and_then(|net| electrical.get(net))
                .and_then(|net| net.transition);
            let condition_probability = group.when.as_ref().map_or(Ok(1.0), |condition| {
                let variables = function_variables(condition);
                validate_variables(&variables, input)?;
                function_probability(condition, &variables, input)
            })?;
            let rise = group
                .rise_power
                .as_ref()
                .and_then(|table| table.value_at(input_transition, output_load))
                .unwrap_or(0.0)
                * output_activity.toggle_rate
                * output_activity.rise_ratio;
            let fall = group
                .fall_power
                .as_ref()
                .and_then(|table| table.value_at(input_transition, output_load))
                .unwrap_or(0.0)
                * output_activity.toggle_rate
                * (1.0 - output_activity.rise_ratio);
            total += (rise + fall) * condition_probability * unit_watts;
        }
    }
    Ok(total)
}

pub(super) fn pin_net(instance: TimingInstanceRef<'_>, pin: &str) -> Option<TimingNetId> {
    instance.pin_net(pin)
}

pub(super) const fn index(net: TimingNetId) -> usize {
    net.raw() as usize
}
