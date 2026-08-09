// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

pub(super) fn latch_data_is_transparent(
    inputs: &PropagationInputs<'_, '_>,
    launch: Option<f64>,
    arrival: f64,
    enable_net: usize,
    open_edge: TimingEdge,
    close_edge: TimingEdge,
) -> Result<bool, crate::TimingError> {
    if enable_net >= inputs.graph.net_count() {
        return Err(crate::TimingAnalysisError::DirtyNetOutOfRange { index: enable_net }.into());
    }
    for (_, clock) in clocks_on_net(inputs.timing, inputs.graph, enable_net) {
        let Some((opening, closing)) = latch_window(clock, open_edge, close_edge, launch, arrival)
        else {
            continue;
        };
        if arrival > opening && arrival < closing {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn latch_window(
    clock: &Clock,
    open_edge: TimingEdge,
    close_edge: TimingEdge,
    launch: Option<f64>,
    arrival: f64,
) -> Option<(f64, f64)> {
    if let Some(launch) = launch {
        let closing = clock.next_edge_after(close_edge, launch);
        let opening = clock.edge_at_or_before(open_edge, closing)?;
        return (opening < closing).then_some((opening, closing));
    }

    if let Some(opening) = clock.edge_at_or_before(open_edge, arrival) {
        let closing = clock.next_edge_after(close_edge, opening);
        if opening < closing && arrival < closing {
            return Some((opening, closing));
        }
    }
    let opening = clock.next_edge_after(open_edge, arrival);
    let closing = clock.next_edge_after(close_edge, opening);
    (opening < closing).then_some((opening, closing))
}

pub(super) fn sequential_description(
    element: SequentialElement,
    clock_edge: TimingEdge,
    clock: &str,
) -> String {
    match element {
        SequentialElement::FlipFlop => format!(
            "{} edge-triggered flip-flop clocked by {clock}",
            edge_adjective(clock_edge)
        ),
        SequentialElement::Latch { open_edge, .. } => format!(
            "{} level-sensitive latch enabled by {clock}",
            edge_adjective(open_edge)
        ),
    }
}
