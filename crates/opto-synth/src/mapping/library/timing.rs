// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    CellCost, Deserialize, LogicSignature, MappedCell, MappedInputConnection,
    MappedOutputConnection, PinPermutation, Serialize, TargetCellRef, TargetPinRef, TruthTable,
    function_truth_table, normalized_cell_area,
};

#[derive(Debug)]
pub(crate) struct CellTemplate {
    pub(crate) cell_order: usize,
    pub(crate) cell_name: String,
    pub(crate) area: f64,
    pub(crate) input_capacitance: f64,
    pub(crate) input_capacitances: Box<[f64]>,
    pub(crate) input_pins: Box<[String]>,
    pub(crate) outputs: Box<[CellOutputTemplate]>,
}

impl CellTemplate {
    pub(crate) fn new(
        cell_order: usize,
        cell: TargetCellRef<'_>,
        inputs: &[TargetPinRef<'_>],
        outputs: Vec<CellOutputTemplate>,
    ) -> Self {
        Self {
            cell_order,
            cell_name: cell.name().to_string(),
            area: normalized_cell_area(cell.area()),
            input_capacitance: input_capacitance(inputs),
            input_capacitances: inputs
                .iter()
                .map(|pin| normalized_pin_capacitance(pin.max_capacitance()))
                .collect(),
            input_pins: inputs
                .iter()
                .map(|pin| pin.name().to_string())
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            outputs: outputs.into_boxed_slice(),
        }
    }

    pub(crate) fn cost_for_output(&self, output: usize) -> CellCost {
        let output = &self.outputs[output];
        CellCost {
            area: self.area,
            delay: output.delay,
            transition: output.transition,
            input_capacitance: self.input_capacitance,
        }
    }

    pub(crate) fn estimate_output(
        &self,
        output: usize,
        pin_to_signature: PinPermutation,
        signature_transitions: &[f64],
        output_load: f64,
    ) -> CellCost {
        let output = &self.outputs[output];
        let load = if output_load.is_finite() && output_load >= 0.0 {
            output_load.max(output.reference_load)
        } else {
            output.reference_load
        };
        let mut delay = None::<f64>;
        let mut transition = None::<f64>;
        for (pin, models) in output.models_by_input.iter().enumerate() {
            let input_transition = signature_transitions
                .get(pin_to_signature.signature_index(pin))
                .copied()
                .filter(|value| value.is_finite() && *value >= 0.0)
                .unwrap_or(0.0);
            for model in models {
                let (model_delay, model_transition) = model.estimate(input_transition, load);
                delay = Some(delay.map_or(model_delay, |current| current.max(model_delay)));
                transition = Some(
                    transition.map_or(model_transition, |current| current.max(model_transition)),
                );
            }
        }
        CellCost {
            area: self.area,
            delay: delay.map_or(output.delay, |delay| delay.max(output.delay)),
            transition: transition.map_or(output.transition, |transition| {
                transition.max(output.transition)
            }),
            input_capacitance: self.input_capacitance,
        }
    }
}

#[derive(Debug)]
pub(crate) struct CellOutputTemplate {
    pub(crate) pin: String,
    pub(crate) truth: TruthTable,
    delay: f64,
    transition: f64,
    reference_load: f64,
    models_by_input: Box<[Box<[PinTimingLinearization]>]>,
}

impl CellOutputTemplate {
    pub(crate) fn new(output: TargetPinRef<'_>, inputs: &[TargetPinRef<'_>]) -> Option<Self> {
        let function = output.function()?;
        Some(Self {
            pin: output.name().to_string(),
            truth: function_truth_table(function, inputs)?,
            delay: output_delay(output, inputs),
            transition: output_transition(output, inputs),
            reference_load: reference_fanout_load(inputs),
            models_by_input: inputs
                .iter()
                .map(|input| {
                    output
                        .timing_arcs()
                        .filter(|arc| arc.related_pin().trim() == input.name())
                        .flat_map(|arc| {
                            opto_timing::TimingEdge::ALL
                                .into_iter()
                                .filter_map(move |edge| {
                                    PinTimingLinearization::new(
                                        arc,
                                        edge,
                                        reference_fanout_load(inputs),
                                    )
                                })
                        })
                        .collect::<Vec<_>>()
                        .into_boxed_slice()
                })
                .collect(),
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct PinTimingLinearization {
    reference_transition: f64,
    reference_load: f64,
    delay: f64,
    transition: f64,
    delay_per_transition: f64,
    delay_per_load: f64,
    transition_per_transition: f64,
    transition_per_load: f64,
}

impl PinTimingLinearization {
    pub(crate) fn new(
        arc: crate::TargetTimingArcRef<'_>,
        edge: opto_timing::TimingEdge,
        reference_load: f64,
    ) -> Option<Self> {
        let reference_transition = reference_transition(arc, edge, reference_load).unwrap_or(0.0);
        let delay = normalized_nonnegative(arc.delay_at(
            edge,
            Some(reference_transition),
            Some(reference_load),
        ))?;
        let transition = normalized_nonnegative(arc.transition_at(
            edge,
            Some(reference_transition),
            Some(reference_load),
        ))?;
        let transition_step = reference_transition.max(0.001);
        let load_step = reference_load.max(0.000_001);
        let delay_at_transition = arc
            .delay_at(
                edge,
                Some(reference_transition + transition_step),
                Some(reference_load),
            )
            .unwrap_or(delay);
        let delay_at_load = arc
            .delay_at(
                edge,
                Some(reference_transition),
                Some(reference_load + load_step),
            )
            .unwrap_or(delay);
        let transition_at_transition = arc
            .transition_at(
                edge,
                Some(reference_transition + transition_step),
                Some(reference_load),
            )
            .unwrap_or(transition);
        let transition_at_load = arc
            .transition_at(
                edge,
                Some(reference_transition),
                Some(reference_load + load_step),
            )
            .unwrap_or(transition);
        Some(Self {
            reference_transition,
            reference_load,
            delay,
            transition,
            delay_per_transition: ((delay_at_transition - delay) / transition_step).max(0.0),
            delay_per_load: ((delay_at_load - delay) / load_step).max(0.0),
            transition_per_transition: ((transition_at_transition - transition) / transition_step)
                .max(0.0),
            transition_per_load: ((transition_at_load - transition) / load_step).max(0.0),
        })
    }

    fn estimate(self, input_transition: f64, output_load: f64) -> (f64, f64) {
        let transition_delta = input_transition - self.reference_transition;
        let load_delta = output_load - self.reference_load;
        (
            (self.delay
                + transition_delta * self.delay_per_transition
                + load_delta * self.delay_per_load)
                .max(0.0),
            (self.transition
                + transition_delta * self.transition_per_transition
                + load_delta * self.transition_per_load)
                .max(0.0),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct JointCellBinding {
    pub(crate) template: usize,
    pub(crate) outputs: [usize; 2],
    pub(crate) pin_to_signature: PinPermutation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct CellBinding {
    pub(crate) template: usize,
    pub(crate) output: usize,
    pub(crate) pin_to_signature: PinPermutation,
    pub(crate) inverted_input: Option<u8>,
}

impl CellBinding {
    pub(crate) fn inverted_input(&self) -> Option<usize> {
        self.inverted_input.map(usize::from)
    }

    pub(crate) fn materialize_mapped_cell(
        &self,
        template: &CellTemplate,
        signature: &LogicSignature,
        target: opto_ir::word::ValueId,
    ) -> MappedCell {
        MappedCell {
            cell_name: template.cell_name.clone(),
            input_connections: template
                .input_pins
                .iter()
                .enumerate()
                .map(|(pin_index, pin)| MappedInputConnection {
                    pin: pin.clone(),
                    value: signature.inputs[self.pin_to_signature.signature_index(pin_index)],
                })
                .collect(),
            output_connections: smallvec::smallvec![MappedOutputConnection {
                pin: template.outputs[self.output].pin.clone(),
                value: target,
            }],
        }
    }
}

fn input_capacitance(inputs: &[TargetPinRef<'_>]) -> f64 {
    inputs
        .iter()
        .map(|pin| normalized_pin_capacitance(pin.max_capacitance()))
        .sum()
}

fn output_delay(output: TargetPinRef<'_>, inputs: &[TargetPinRef<'_>]) -> f64 {
    let load = reference_fanout_load(inputs);
    output
        .timing_arcs()
        .filter(|arc| related_to_input(*arc, inputs))
        .flat_map(|arc| {
            opto_timing::TimingEdge::ALL
                .into_iter()
                .filter_map(move |edge| {
                    let transition = reference_transition(arc, edge, load);
                    normalized_nonnegative(arc.delay_at(edge, transition, Some(load)))
                })
        })
        .max_by(f64::total_cmp)
        .unwrap_or(f64::INFINITY)
}

fn output_transition(output: TargetPinRef<'_>, inputs: &[TargetPinRef<'_>]) -> f64 {
    let load = reference_fanout_load(inputs);
    output
        .timing_arcs()
        .filter(|arc| related_to_input(*arc, inputs))
        .flat_map(|arc| {
            opto_timing::TimingEdge::ALL
                .into_iter()
                .filter_map(move |edge| reference_transition(arc, edge, load))
        })
        .max_by(f64::total_cmp)
        .unwrap_or(f64::INFINITY)
}

fn reference_transition(
    arc: crate::TargetTimingArcRef<'_>,
    edge: opto_timing::TimingEdge,
    load: f64,
) -> Option<f64> {
    let mut transition = None;
    for _ in 0..4 {
        transition = arc.transition_at(edge, transition, Some(load));
    }
    normalized_nonnegative(transition)
}

fn related_to_input(arc: crate::TargetTimingArcRef<'_>, inputs: &[TargetPinRef<'_>]) -> bool {
    let related = arc.related_pin().trim();
    !related.is_empty() && inputs.iter().any(|input| input.name() == related)
}

#[allow(
    clippy::cast_precision_loss,
    reason = "the bounded input count normalizes an approximate FO4 load"
)]
pub(crate) fn reference_fanout_load(inputs: &[TargetPinRef<'_>]) -> f64 {
    let total = input_capacitance(inputs);
    if total.is_finite() && !inputs.is_empty() {
        // FO4 is the standard technology-independent stage-effort point. It
        // compares drive variants at equivalent electrical effort instead of
        // rewarding the largest cell for being characterized at a tiny load.
        4.0 * total / inputs.len() as f64
    } else {
        0.0
    }
}

fn normalized_nonnegative(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite() && *value >= 0.0)
}

fn normalized_pin_capacitance(capacitance: Option<f64>) -> f64 {
    capacitance
        .filter(|capacitance| capacitance.is_finite() && *capacitance >= 0.0)
        .unwrap_or(f64::INFINITY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linearization_responds_monotonically_to_slew_and_load() {
        let model = PinTimingLinearization {
            reference_transition: 1.0,
            reference_load: 2.0,
            delay: 3.0,
            transition: 4.0,
            delay_per_transition: 0.5,
            delay_per_load: 1.0,
            transition_per_transition: 1.5,
            transition_per_load: 2.0,
        };
        assert_eq!(model.estimate(1.0, 2.0), (3.0, 4.0));
        assert_eq!(model.estimate(2.0, 3.0), (4.5, 7.5));
    }
}
