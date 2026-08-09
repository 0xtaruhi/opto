// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{GraphArcKind, SequentialElement};
use crate::{
    TargetCellRef, TargetPinRef, TargetSequentialKind, TargetTimingArcRef, TargetTimingType,
    TimingEdge,
};

pub(super) fn sink_response(
    net: Option<crate::parasitics::ParasiticNetRef<'_>>,
    instance: &str,
    pin: &str,
) -> ([f64; 2], [Option<f64>; 2]) {
    let delay = std::array::from_fn(|index| {
        let edge = TimingEdge::ALL[index];
        net.and_then(|net| net.sink_delay_parts(instance, pin, edge))
            .unwrap_or(0.0)
    });
    let transition = std::array::from_fn(|index| {
        let edge = TimingEdge::ALL[index];
        net.and_then(|net| net.sink_transition_parts(instance, pin, edge))
    });
    (delay, transition)
}

pub(in crate::analysis) fn sequential_element_for_control(
    cell: TargetCellRef<'_>,
    control_pin: &str,
) -> SequentialElement {
    for sequential in cell.sequential() {
        if sequential.kind() != TargetSequentialKind::Latch {
            continue;
        }
        let Some((enable_pin, active_high)) = sequential
            .enable()
            .and_then(opto_library::BooleanFunctionRef::as_literal)
        else {
            continue;
        };
        if enable_pin == control_pin {
            return SequentialElement::Latch {
                open_edge: if active_high {
                    TimingEdge::Rise
                } else {
                    TimingEdge::Fall
                },
                close_edge: if active_high {
                    TimingEdge::Fall
                } else {
                    TimingEdge::Rise
                },
            };
        }
    }
    SequentialElement::FlipFlop
}

pub(super) fn graph_arc_kind(
    cell: TargetCellRef<'_>,
    output_pin: TargetPinRef<'_>,
    arc: TargetTimingArcRef<'_>,
    pins: &super::InstancePinRow<'_>,
    constant_values: &[Option<bool>],
    instance: &str,
) -> Result<Option<GraphArcKind>, crate::TimingError> {
    let Some((enable_pin, open_edge, close_edge)) =
        latch_data_control(cell, output_pin, arc.related_pin())
    else {
        return Ok(Some(GraphArcKind::Combinational));
    };
    let has_opening_arc = output_pin.timing_arcs().any(|candidate| {
        candidate.related_pin() == enable_pin
            && candidate.timing_type() == TargetTimingType::ClockToQ(open_edge)
    });
    if !has_opening_arc {
        return Err(crate::TimingModelError::MissingLatchOpeningArc {
            cell: cell.name().to_string(),
            output: output_pin.name().to_string(),
            edge: edge_name(open_edge),
        }
        .into());
    }
    let enable_net = pins.net_by_name(enable_pin).ok_or_else(|| {
        crate::TimingModelError::MissingLatchEnableConnection {
            instance: instance.to_string(),
            pin: enable_pin.to_string(),
        }
    })?;
    if let Some(value) = constant_values[enable_net.index()] {
        let active = match open_edge {
            TimingEdge::Rise => value,
            TimingEdge::Fall => !value,
        };
        return Ok(active.then_some(GraphArcKind::Combinational));
    }
    Ok(Some(GraphArcKind::LatchData {
        enable_net,
        open_edge,
        close_edge,
    }))
}

fn latch_data_control<'a>(
    cell: TargetCellRef<'a>,
    output_pin: TargetPinRef<'a>,
    data_pin: &str,
) -> Option<(&'a str, TimingEdge, TimingEdge)> {
    let (state, _) = output_pin.function()?.as_literal()?;
    for sequential in cell.sequential() {
        if sequential.kind() != TargetSequentialKind::Latch
            || !sequential
                .state_variables()
                .any(|variable| variable == state)
        {
            continue;
        }
        let Some((sequential_data, data_positive)) = sequential
            .next_state()
            .and_then(opto_library::BooleanFunctionRef::as_literal)
        else {
            continue;
        };
        let Some((enable_pin, active_high)) = sequential
            .enable()
            .and_then(opto_library::BooleanFunctionRef::as_literal)
        else {
            continue;
        };
        if data_positive && sequential_data == data_pin {
            let (open_edge, close_edge) = if active_high {
                (TimingEdge::Rise, TimingEdge::Fall)
            } else {
                (TimingEdge::Fall, TimingEdge::Rise)
            };
            return Some((enable_pin, open_edge, close_edge));
        }
    }
    None
}

fn edge_name(edge: TimingEdge) -> &'static str {
    match edge {
        TimingEdge::Rise => "rising",
        TimingEdge::Fall => "falling",
    }
}
