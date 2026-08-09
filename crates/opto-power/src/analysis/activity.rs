// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::SwitchingActivity;
use crate::PowerError;
use opto_library::{BooleanFunction, BooleanFunctionRef};
use std::collections::BTreeSet;

const EXACT_FUNCTION_INPUT_LIMIT: usize = 20;

pub(super) trait BooleanExpression {
    fn evaluate(&self, lookup: &mut impl FnMut(&str) -> Option<bool>) -> Option<bool>;
    fn for_each_pin(&self, visitor: &mut impl FnMut(&str));
}

impl BooleanExpression for BooleanFunction {
    fn evaluate(&self, lookup: &mut impl FnMut(&str) -> Option<bool>) -> Option<bool> {
        self.eval(lookup)
    }

    fn for_each_pin(&self, visitor: &mut impl FnMut(&str)) {
        visit_owned_pins(self, visitor);
    }
}

impl BooleanExpression for BooleanFunctionRef<'_> {
    fn evaluate(&self, lookup: &mut impl FnMut(&str) -> Option<bool>) -> Option<bool> {
        (*self).eval(lookup)
    }

    fn for_each_pin(&self, visitor: &mut impl FnMut(&str)) {
        (*self).for_each_pin(visitor);
    }
}

pub(super) fn propagated_activity(
    function: &impl BooleanExpression,
    input: impl Copy + Fn(&str) -> Option<SwitchingActivity>,
) -> Result<SwitchingActivity, PowerError> {
    let variables = function_variables(function);
    validate_variables(&variables, input)?;
    let static_probability = function_probability(function, &variables, input)?;
    let toggle_rate = variables
        .iter()
        .enumerate()
        .map(|(index, variable)| {
            let activity = input(variable).expect("validated Boolean-function input");
            boolean_difference_probability(function, &variables, input, index)
                .map(|probability| activity.toggle_rate * probability)
        })
        .sum::<Result<_, _>>()?;
    SwitchingActivity::new(static_probability, toggle_rate, 0.5)
}

pub(super) fn function_probability(
    function: &impl BooleanExpression,
    variables: &[String],
    input: impl Copy + Fn(&str) -> Option<SwitchingActivity>,
) -> Result<f64, PowerError> {
    validate_input_limit(variables.len())?;
    let mut total = 0.0;
    for assignment in 0..(1usize << variables.len()) {
        let mut weight = 1.0;
        for (index, variable) in variables.iter().enumerate() {
            let probability = input(variable)
                .expect("validated Boolean-function input")
                .static_probability;
            weight *= if assignment & (1usize << index) == 0 {
                1.0 - probability
            } else {
                probability
            };
        }
        if evaluate(function, variables, assignment)? {
            total += weight;
        }
    }
    Ok(total)
}

fn boolean_difference_probability(
    function: &impl BooleanExpression,
    variables: &[String],
    input: impl Copy + Fn(&str) -> Option<SwitchingActivity>,
    target: usize,
) -> Result<f64, PowerError> {
    validate_input_limit(variables.len())?;
    let target_mask = 1usize << target;
    let mut total = 0.0;
    for assignment in 0..(1usize << variables.len()) {
        if assignment & target_mask != 0 {
            continue;
        }
        let mut weight = 1.0;
        for (index, variable) in variables.iter().enumerate() {
            if index == target {
                continue;
            }
            let probability = input(variable)
                .expect("validated Boolean-function input")
                .static_probability;
            weight *= if assignment & (1usize << index) == 0 {
                1.0 - probability
            } else {
                probability
            };
        }
        if evaluate(function, variables, assignment)?
            != evaluate(function, variables, assignment | target_mask)?
        {
            total += weight;
        }
    }
    Ok(total)
}

fn evaluate(
    function: &impl BooleanExpression,
    variables: &[String],
    assignment: usize,
) -> Result<bool, PowerError> {
    function
        .evaluate(&mut |name| {
            variables
                .iter()
                .position(|variable| variable == name)
                .map(|index| assignment & (1usize << index) != 0)
        })
        .ok_or_else(|| PowerError::UnknownFunctionPin {
            pin: "<evaluation>".to_string(),
        })
}

pub(super) fn validate_variables(
    variables: &[String],
    input: impl Copy + Fn(&str) -> Option<SwitchingActivity>,
) -> Result<(), PowerError> {
    validate_input_limit(variables.len())?;
    if let Some(pin) = variables.iter().find(|variable| input(variable).is_none()) {
        return Err(PowerError::UnknownFunctionPin { pin: pin.clone() });
    }
    Ok(())
}

fn validate_input_limit(inputs: usize) -> Result<(), PowerError> {
    if inputs > EXACT_FUNCTION_INPUT_LIMIT {
        Err(PowerError::FunctionInputLimit {
            inputs,
            limit: EXACT_FUNCTION_INPUT_LIMIT,
        })
    } else {
        Ok(())
    }
}

pub(super) fn function_variables(function: &impl BooleanExpression) -> Vec<String> {
    let mut variables = BTreeSet::new();
    function.for_each_pin(&mut |name| {
        variables.insert(name.to_string());
    });
    variables.into_iter().collect()
}

fn visit_owned_pins(function: &BooleanFunction, visitor: &mut impl FnMut(&str)) {
    match function {
        BooleanFunction::Const(_) => {}
        BooleanFunction::Pin(name) => visitor(name),
        BooleanFunction::Not(argument) => visit_owned_pins(argument, visitor),
        BooleanFunction::And(left, right)
        | BooleanFunction::Or(left, right)
        | BooleanFunction::Xor(left, right)
        | BooleanFunction::Imp(left, right)
        | BooleanFunction::Iff(left, right) => {
            visit_owned_pins(left, visitor);
            visit_owned_pins(right, visitor);
        }
        BooleanFunction::Cond(condition, when_true, when_false) => {
            visit_owned_pins(condition, visitor);
            visit_owned_pins(when_true, visitor);
            visit_owned_pins(when_false, visitor);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn propagates_exact_and_and_xor_probabilities() {
        let inputs = [
            (
                "A",
                SwitchingActivity::new(0.25, 0.2, 0.5).expect("valid A activity"),
            ),
            (
                "B",
                SwitchingActivity::new(0.5, 0.1, 0.5).expect("valid B activity"),
            ),
        ];
        let input = |name: &str| {
            inputs
                .iter()
                .find_map(|(pin, activity)| (*pin == name).then_some(*activity))
        };
        let and = BooleanFunction::And(
            Box::new(BooleanFunction::Pin("A".to_string())),
            Box::new(BooleanFunction::Pin("B".to_string())),
        );
        let xor = BooleanFunction::Xor(
            Box::new(BooleanFunction::Pin("A".to_string())),
            Box::new(BooleanFunction::Pin("B".to_string())),
        );

        let and_activity = propagated_activity(&and, input).expect("AND activity");
        assert!((and_activity.static_probability - 0.125).abs() < 1e-12);
        assert!((and_activity.toggle_rate - 0.125).abs() < 1e-12);
        let xor_activity = propagated_activity(&xor, input).expect("XOR activity");
        assert!((xor_activity.static_probability - 0.5).abs() < 1e-12);
        assert!((xor_activity.toggle_rate - 0.3).abs() < 1e-12);
    }

    #[test]
    fn rejects_functions_above_the_exact_input_limit() {
        let mut function = BooleanFunction::Const(true);
        let names = (0..=EXACT_FUNCTION_INPUT_LIMIT)
            .map(|index| format!("A{index}"))
            .collect::<Vec<_>>();
        for name in &names {
            function = BooleanFunction::And(
                Box::new(function),
                Box::new(BooleanFunction::Pin(name.clone())),
            );
        }
        let activity = SwitchingActivity::new(0.5, 0.1, 0.5).expect("valid activity");

        assert!(matches!(
            propagated_activity(&function, |_| Some(activity)),
            Err(PowerError::FunctionInputLimit {
                inputs: 21,
                limit: EXACT_FUNCTION_INPUT_LIMIT
            })
        ));
    }
}
