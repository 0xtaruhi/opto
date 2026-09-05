// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Fanout-tree geometry: branching factors, sink balancing, and stage delay.

use super::{BufferTimingView, ElectricalLoad, PinId, TargetTimingType, TimingEdge};

pub(super) fn maximum_legal_factor(
    view: &BufferTimingView<'_, '_, '_>,
    maximum_load: f64,
    maximum_factor: usize,
) -> usize {
    let mut legal = 1usize;
    let mut upper = maximum_factor;
    while legal < upper {
        let factor = legal + (upper - legal).div_ceil(2);
        if estimated_output_load(view, factor) <= maximum_load {
            legal = factor;
        } else {
            upper = factor - 1;
        }
    }
    legal
}

/// Returns the two Pareto-relevant endpoints of every distinct tree-depth
/// interval. Within one interval, the low factor minimizes electrical load and
/// the high factor minimizes buffer count. The number of evaluated factors is
/// logarithmic in fanout rather than linear in sink count.
pub(super) fn branching_factor_candidates(
    sink_count: usize,
    maximum_factor: usize,
) -> Result<Vec<usize>, crate::SynthError> {
    if maximum_factor < 2 {
        return Ok(Vec::new());
    }
    let maximum_levels = tree_shape(sink_count, 2)?.0;
    let minimum_levels = tree_shape(sink_count, maximum_factor)?.0;
    let mut candidates = Vec::new();
    for levels in minimum_levels..=maximum_levels {
        let first = first_factor_with_at_most_levels(sink_count, maximum_factor, levels)?;
        if tree_shape(sink_count, first)?.0 != levels {
            continue;
        }
        let last = if levels == minimum_levels {
            maximum_factor
        } else {
            first_factor_with_at_most_levels(sink_count, maximum_factor, levels - 1)?
                .saturating_sub(1)
        };
        candidates.push(first);
        candidates.push(last);
    }
    candidates.sort_unstable();
    candidates.dedup();
    Ok(candidates)
}

pub(super) fn first_factor_with_at_most_levels(
    sink_count: usize,
    maximum_factor: usize,
    levels: usize,
) -> Result<usize, crate::SynthError> {
    let mut lower = 2usize;
    let mut upper = maximum_factor;
    while lower < upper {
        let factor = lower + (upper - lower) / 2;
        if tree_shape(sink_count, factor)?.0 <= levels {
            upper = factor;
        } else {
            lower = factor + 1;
        }
    }
    Ok(lower)
}

pub(super) fn estimated_output_load(view: &BufferTimingView<'_, '_, '_>, factor: usize) -> f64 {
    buffer_receiver_load(view, factor).capacitance
}

pub(super) struct BalancedSinkGroup {
    indices: Vec<usize>,
    capacitance: f64,
    fanout: f64,
}

pub(super) fn balance_sink_groups(
    sinks: &[PinId],
    views: &[BufferTimingView<'_, '_, '_>],
    factor: usize,
) -> Result<Vec<Vec<usize>>, crate::SynthError> {
    if factor < 2 || sinks.is_empty() || views.is_empty() {
        return Err(crate::SynthError::invariant(
            "fanout sink balancing requires timing views, sinks, and a legal branching factor",
        ));
    }
    if views
        .iter()
        .any(|view| view.sink_loads.len() != sinks.len())
    {
        return Err(crate::SynthError::invariant(
            "fanout timing views do not align with the sink set",
        ));
    }
    let (first_view, remaining_views) = views.split_first().ok_or_else(|| {
        crate::SynthError::invariant("fanout sink balancing lost its timing views")
    })?;
    let mut weighted = (0..sinks.len())
        .map(|index| {
            let first = first_view.sink_loads[index];
            let (capacitance, fanout) = remaining_views.iter().fold(
                (first.capacitance, first.fanout),
                |(capacitance, fanout), view| {
                    (
                        capacitance.max(view.sink_loads[index].capacitance),
                        fanout.max(view.sink_loads[index].fanout),
                    )
                },
            );
            (index, capacitance, fanout)
        })
        .collect::<Vec<_>>();
    weighted.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| right.2.total_cmp(&left.2))
            .then_with(|| sinks[left.0].cmp(&sinks[right.0]))
    });
    let group_count = sinks.len().div_ceil(factor);
    let mut groups = (0..group_count)
        .map(|_| BalancedSinkGroup {
            indices: Vec::new(),
            capacitance: 0.0,
            fanout: 0.0,
        })
        .collect::<Vec<_>>();
    for (index, capacitance, fanout) in weighted {
        let group = groups
            .iter_mut()
            .enumerate()
            .filter(|(_, group)| group.indices.len() < factor)
            .min_by(|left, right| {
                left.1
                    .capacitance
                    .total_cmp(&right.1.capacitance)
                    .then_with(|| left.1.fanout.total_cmp(&right.1.fanout))
                    .then_with(|| left.1.indices.len().cmp(&right.1.indices.len()))
                    .then_with(|| left.0.cmp(&right.0))
            })
            .map(|(_, group)| group)
            .ok_or_else(|| {
                crate::SynthError::invariant("fanout sink balancing exhausted group capacity")
            })?;
        group.indices.push(index);
        group.capacitance += capacitance;
        group.fanout += fanout;
    }
    for group in &mut groups {
        group.indices.sort_by_key(|&index| sinks[index]);
    }
    if groups.iter().any(|group| group.indices.is_empty()) {
        return Err(crate::SynthError::invariant(
            "fanout sink balancing produced an empty leaf group",
        ));
    }
    groups.sort_by_key(|group| group.indices.first().map(|&index| sinks[index]));
    Ok(groups.into_iter().map(|group| group.indices).collect())
}

#[allow(
    clippy::cast_precision_loss,
    reason = "bounded tree levels scale an approximate physical delay estimate"
)]
pub(super) fn estimated_buffer_path_delay(
    view: &BufferTimingView<'_, '_, '_>,
    sinks: &[PinId],
    leaf_groups: &[Vec<usize>],
    factor: usize,
    cell_levels: usize,
) -> Option<f64> {
    if sinks.is_empty() || leaf_groups.is_empty() || cell_levels == 0 {
        return None;
    }
    let leaf_delays = leaf_groups
        .iter()
        .map(|group| receiver_group_load(view, group))
        .map(|load| buffered_stage_delay(view, load))
        .collect::<Option<Vec<_>>>()?;
    let leaf_delay = view_delay(view, leaf_delays.into_iter())?;
    let internal_delay = if cell_levels > 1 {
        buffered_stage_delay(view, buffer_receiver_load(view, factor))? * (cell_levels - 1) as f64
    } else {
        0.0
    };
    let root_count = root_buffer_count(leaf_groups.len(), factor);
    let root_load = buffer_and_direct_receiver_load(view, root_count, view.direct_load);
    let source_wire_delay =
        receiver_wire_delay(view, root_load, view.input.design_input_capacitance())?;
    let buffered_delay = source_wire_delay + internal_delay + leaf_delay;
    if view.direct_load.receivers > 0.0 {
        let direct_delay = receiver_wire_delay(
            view,
            root_load,
            view.direct_load.receiver_capacitance(view.delay_type),
        )?;
        view_delay(view, [buffered_delay, direct_delay].into_iter())
    } else {
        Some(buffered_delay)
    }
}

pub(super) fn receiver_group_load(
    view: &BufferTimingView<'_, '_, '_>,
    group: &[usize],
) -> ElectricalLoad {
    let mut load = group
        .iter()
        .fold(ElectricalLoad::default(), |mut total, &index| {
            total.add_receivers(view.sink_loads[index]);
            total
        });
    if let Some(wire) = view.wire_load {
        load.capacitance += wire.capacitance_at(load.receivers);
    }
    load
}

pub(super) fn buffer_receiver_load(
    view: &BufferTimingView<'_, '_, '_>,
    count: usize,
) -> ElectricalLoad {
    buffer_and_direct_receiver_load(view, count, ElectricalLoad::default())
}

#[allow(
    clippy::cast_precision_loss,
    reason = "bounded receiver counts scale approximate library load values"
)]
fn buffer_and_direct_receiver_load(
    view: &BufferTimingView<'_, '_, '_>,
    count: usize,
    direct: ElectricalLoad,
) -> ElectricalLoad {
    let mut load = ElectricalLoad {
        capacitance: view.input.design_input_capacitance() * count as f64,
        fanout: view.input.design_fanout_load() * count as f64,
        receivers: count as f64,
        max_sink_capacitance: view.input.design_input_capacitance(),
        min_sink_capacitance: view.input.design_input_capacitance(),
    };
    load.add_receivers(direct);
    if let Some(wire) = view.wire_load {
        load.capacitance += wire.capacitance_at(load.receivers);
    }
    load
}

pub(super) fn buffered_stage_delay(
    view: &BufferTimingView<'_, '_, '_>,
    load: ElectricalLoad,
) -> Option<f64> {
    if maximum_characterized_load(view).is_some_and(|maximum| load.capacitance > maximum) {
        return None;
    }
    let cell_delays = view
        .output
        .timing_arcs()
        .filter(|arc| arc.timing_type() == TargetTimingType::Combinational)
        .flat_map(|arc| {
            TimingEdge::ALL
                .into_iter()
                .filter_map(move |edge| arc.delay_at(edge, None, Some(load.capacitance)))
        });
    let cell_delay = view_delay(view, cell_delays)?;
    Some(cell_delay + receiver_wire_delay(view, load, load.receiver_capacitance(view.delay_type))?)
}

/// Balanced trees have a distinct source-to-receiver delay. Root buffers and
/// protected direct sinks share total load but retain their own branch load.
fn receiver_wire_delay(
    view: &BufferTimingView<'_, '_, '_>,
    load: ElectricalLoad,
    sink_capacitance: f64,
) -> Option<f64> {
    let wire = view.wire_load?;
    Some(
        view.wire_tree.sink_delay(
            view.units
                .normalize_resistance(wire.resistance_at(load.receivers)),
            wire.capacitance_at(load.receivers),
            load.receivers,
            load.capacitance,
            sink_capacitance,
        ),
    )
}

/// Use the analysis view's propagation polarity when bounding a stage or
/// path. A late maximum would overstate the slack left in an early view.
pub(super) fn view_delay(
    view: &BufferTimingView<'_, '_, '_>,
    delays: impl Iterator<Item = f64>,
) -> Option<f64> {
    match view.delay_type {
        opto_timing::DelayType::Max => delays.max_by(f64::total_cmp),
        opto_timing::DelayType::Min => delays.min_by(f64::total_cmp),
    }
}

pub(super) fn maximum_characterized_load(view: &BufferTimingView<'_, '_, '_>) -> Option<f64> {
    view.output
        .timing_arcs()
        .filter(|arc| arc.timing_type() == TargetTimingType::Combinational)
        .filter_map(|arc| {
            arc.delay_model()
                .and_then(opto_library::ArcDelayModel::maximum_characterized_output_load)
        })
        .min_by(f64::total_cmp)
}

pub(super) fn root_buffer_count(mut nodes: usize, factor: usize) -> usize {
    while nodes > factor {
        nodes = nodes.div_ceil(factor);
    }
    nodes
}

pub(super) fn tree_shape(
    sink_count: usize,
    branching_factor: usize,
) -> Result<(usize, usize), crate::SynthError> {
    if branching_factor < 2 {
        return Err(crate::SynthError::invariant(
            "fanout tree requires a branching factor of at least two",
        ));
    }
    let mut level_nodes = sink_count.div_ceil(branching_factor);
    let mut levels = 1usize;
    let mut buffers = level_nodes;
    while level_nodes > branching_factor {
        level_nodes = level_nodes.div_ceil(branching_factor);
        buffers = buffers.checked_add(level_nodes).ok_or_else(|| {
            crate::SynthError::capacity("fanout-tree buffer count exceeds capacity")
        })?;
        levels = levels
            .checked_add(1)
            .ok_or_else(|| crate::SynthError::capacity("fanout-tree depth exceeds capacity"))?;
    }
    Ok((levels, buffers))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_receivers_preserve_physical_fanout_and_branch_polarity() {
        let mut buffer = crate::test_support::target_cell(
            "BUF",
            1.0,
            &[
                ("A", opto_library::TargetPinDirection::Input, None),
                ("Y", opto_library::TargetPinDirection::Output, Some("A")),
            ],
        );
        buffer.pins[0].capacitance = Some(1.0);
        buffer.pins[0].fanout_load = Some(10.0);
        let cells: opto_library::TargetCellSet = vec![buffer].into();
        let cell = cells.get(0).unwrap();
        let wire =
            opto_library::WireLoadModel::new("test".into(), 2.0, 3.0, 1.0, Vec::new()).unwrap();
        let view = BufferTimingView {
            input: cell.pins().next().unwrap(),
            output: cell.pins().nth(1).unwrap(),
            wire_load: Some(&wire),
            wire_tree: opto_library::WireLoadTree::Balanced,
            units: opto_library::TimingLibraryUnits::default(),
            net_state: None,
            delay_type: opto_timing::DelayType::Max,
            sink_loads: &[],
            direct_load: ElectricalLoad {
                capacitance: 4.0,
                fanout: 30.0,
                receivers: 1.0,
                max_sink_capacitance: 4.0,
                min_sink_capacitance: 4.0,
            },
        };
        let load = buffer_and_direct_receiver_load(&view, 2, view.direct_load);
        // Three physical branches carry 6 units of wire capacitance, despite
        // an abstract electrical fanout of 50. The direct branch is slower.
        assert_eq!(load.receivers, 3.0);
        assert_eq!(load.fanout, 50.0);
        assert_eq!(load.capacitance, 12.0);
        for (polarity, expected) in [
            (opto_timing::DelayType::Min, 9.0),
            (opto_timing::DelayType::Max, 18.0),
        ] {
            assert_eq!(
                receiver_wire_delay(&view, load, load.receiver_capacitance(polarity)),
                Some(expected)
            );
        }
        let mut direct = ElectricalLoad::default();
        direct.add_receivers(view.direct_load);
        assert_eq!(direct.min_sink_capacitance, 4.0);
    }
}
