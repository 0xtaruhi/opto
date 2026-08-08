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
    views: &[BufferTimingView<'_, '_, '_>],
    sinks: &[PinId],
    leaf_groups: &[Vec<usize>],
    factor: usize,
    cell_levels: usize,
) -> Option<f64> {
    if views.is_empty() || sinks.is_empty() || leaf_groups.is_empty() || cell_levels == 0 {
        return None;
    }
    let mut worst = None::<f64>;
    for view in views {
        let wire = view.wire_load?;
        let leaf_delay = leaf_groups
            .iter()
            .map(|group| receiver_group_load(view, group))
            .map(|load| buffered_stage_delay(view, load))
            .collect::<Option<Vec<_>>>()?
            .into_iter()
            .max_by(f64::total_cmp)?;
        let internal_delay = if cell_levels > 1 {
            buffered_stage_delay(view, buffer_receiver_load(view, factor))?
                * (cell_levels - 1) as f64
        } else {
            0.0
        };
        let root_count = root_buffer_count(leaf_groups.len(), factor);
        let root_load = buffer_receiver_load(view, root_count);
        let source_wire_delay = wire.resistance_at(root_load.fanout) * root_load.capacitance;
        let path_delay = source_wire_delay + internal_delay + leaf_delay;
        worst = Some(worst.map_or(path_delay, |current| current.max(path_delay)));
    }
    worst
}

pub(super) fn receiver_group_load(
    view: &BufferTimingView<'_, '_, '_>,
    group: &[usize],
) -> ElectricalLoad {
    let mut load = group
        .iter()
        .fold(ElectricalLoad::default(), |mut total, &index| {
            total.capacitance += view.sink_loads[index].capacitance;
            total.fanout += view.sink_loads[index].fanout;
            total
        });
    if let Some(wire) = view.wire_load {
        load.capacitance += wire.capacitance_at(load.fanout);
    }
    load
}

#[allow(
    clippy::cast_precision_loss,
    reason = "bounded receiver counts scale approximate library load values"
)]
pub(super) fn buffer_receiver_load(
    view: &BufferTimingView<'_, '_, '_>,
    count: usize,
) -> ElectricalLoad {
    let fanout = view.input.design_fanout_load() * count as f64;
    ElectricalLoad {
        capacitance: view.input.design_input_capacitance() * count as f64
            + view
                .wire_load
                .map_or(0.0, |wire| wire.capacitance_at(fanout)),
        fanout,
    }
}

pub(super) fn buffered_stage_delay(
    view: &BufferTimingView<'_, '_, '_>,
    load: ElectricalLoad,
) -> Option<f64> {
    if maximum_characterized_load(view).is_some_and(|maximum| load.capacitance > maximum) {
        return None;
    }
    let cell_delay = view
        .output
        .timing_arcs()
        .filter(|arc| arc.timing_type() == TargetTimingType::Combinational)
        .flat_map(|arc| {
            TimingEdge::ALL
                .into_iter()
                .filter_map(move |edge| arc.delay_at(edge, None, Some(load.capacitance)))
        })
        .max_by(f64::total_cmp)?;
    let wire = view.wire_load?;
    Some(cell_delay + wire.resistance_at(load.fanout) * load.capacitance)
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
