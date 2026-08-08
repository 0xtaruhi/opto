// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Transactional electrical-rule index for incremental timing edits.

use super::ConstraintIndex;
use crate::analysis::{PropagationState, net_timing_state};
use crate::{
    DesignRuleKind, DesignRuleSummary, DesignRuleViolation, ReportTimingOptions, TimingContext,
    TimingModel,
};
use std::cmp::Ordering;
use std::collections::BTreeMap;

#[derive(Debug)]
pub(super) struct DesignRuleIndex {
    violations: BTreeMap<(DesignRuleKind, usize), DesignRuleViolation>,
    ratios: BTreeMap<OrderedRatio, usize>,
    total_excess: f64,
}

#[derive(Debug, Clone, Copy)]
struct OrderedRatio(f64);

impl PartialEq for OrderedRatio {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for OrderedRatio {}

impl PartialOrd for OrderedRatio {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderedRatio {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

#[derive(Debug)]
pub(super) enum DesignRuleEdit {
    Nets {
        previous: Vec<((DesignRuleKind, usize), Option<DesignRuleViolation>)>,
        total_excess: f64,
    },
    Rebuilt(Box<DesignRuleIndex>),
}

#[derive(Clone, Copy)]
pub(super) struct DesignRuleInputs<'a> {
    pub(super) timing: &'a TimingContext,
    pub(super) model: &'a TimingModel,
    pub(super) options: &'a ReportTimingOptions,
    pub(super) propagation: &'a PropagationState,
    pub(super) constraints: &'a ConstraintIndex,
}

impl DesignRuleIndex {
    pub(super) fn owned_memory_bytes(&self) -> usize {
        opto_core::resident::slice_bytes::<(
            (DesignRuleKind, usize),
            DesignRuleViolation,
            [usize; 4],
        )>(self.violations.len())
        .saturating_add(opto_core::resident::slice_bytes::<(
            OrderedRatio,
            usize,
            [usize; 4],
        )>(self.ratios.len()))
        .saturating_add(
            self.violations
                .values()
                .map(|violation| opto_core::resident::allocation_bytes(violation.object.len()))
                .sum::<usize>(),
        )
    }

    pub(super) fn build(inputs: DesignRuleInputs<'_>) -> Self {
        let mut index = Self {
            violations: BTreeMap::new(),
            ratios: BTreeMap::new(),
            total_excess: 0.0,
        };
        if !inputs.constraints.has_design_rule_limits {
            return index;
        }
        for net in 0..inputs.model.graph.net_count() {
            index.recompute_net(inputs, net);
        }
        index
    }

    pub(super) fn update(
        &mut self,
        inputs: DesignRuleInputs<'_>,
        changed_nets: &[usize],
        structure_changed: bool,
    ) -> DesignRuleEdit {
        // Topology changes can alter clock scopes and dense net meanings.
        if structure_changed {
            let replacement = Self::build(inputs);
            return DesignRuleEdit::Rebuilt(Box::new(std::mem::replace(self, replacement)));
        }
        let total_excess = self.total_excess;
        let mut previous = Vec::new();
        for &net in changed_nets {
            for kind in design_rule_kinds() {
                let key = (kind, net);
                previous.push((key, self.remove(key)));
            }
            self.recompute_net(inputs, net);
        }
        DesignRuleEdit::Nets {
            previous,
            total_excess,
        }
    }

    pub(super) fn rollback(&mut self, edit: DesignRuleEdit) {
        match edit {
            DesignRuleEdit::Nets {
                previous,
                total_excess,
            } => {
                for (key, violation) in previous {
                    self.remove(key);
                    if let Some(violation) = violation {
                        self.insert(key, violation);
                    }
                }
                self.total_excess = total_excess;
            }
            DesignRuleEdit::Rebuilt(previous) => *self = *previous,
        }
    }

    fn recompute_net(&mut self, inputs: DesignRuleInputs<'_>, net: usize) {
        let Some(id) = crate::TimingNetId::from_index(net).ok() else {
            return;
        };
        let Some(name) = inputs.model.net_name(id) else {
            return;
        };
        let Some(state) = net_timing_state(
            inputs.timing,
            inputs.model,
            inputs.propagation,
            inputs.options.delay_type,
            &name,
        ) else {
            return;
        };
        for kind in design_rule_kinds() {
            let Some(limit) = inputs.constraints.design_rule_limit(kind, net) else {
                continue;
            };
            let actual = match kind {
                DesignRuleKind::MaxTransition => state.transition,
                DesignRuleKind::MaxCapacitance => Some(state.capacitance),
                DesignRuleKind::MaxFanout => Some(state.fanout),
            };
            if let Some(actual) = actual
                && actual > limit
            {
                self.insert(
                    (kind, net),
                    DesignRuleViolation {
                        kind,
                        net: state.id,
                        mapped_net: inputs.model.mapped_net(state.id),
                        object: state.name.clone(),
                        actual,
                        limit,
                    },
                );
            }
        }
    }

    fn insert(&mut self, key: (DesignRuleKind, usize), violation: DesignRuleViolation) {
        debug_assert!(!self.violations.contains_key(&key));
        let ratio = OrderedRatio(violation.actual / violation.limit);
        *self.ratios.entry(ratio).or_default() += 1;
        self.total_excess += violation.actual - violation.limit;
        self.violations.insert(key, violation);
    }

    fn remove(&mut self, key: (DesignRuleKind, usize)) -> Option<DesignRuleViolation> {
        let violation = self.violations.remove(&key)?;
        let ratio = OrderedRatio(violation.actual / violation.limit);
        let count = self
            .ratios
            .get_mut(&ratio)
            .expect("recorded design-rule violation ratio must exist");
        *count -= 1;
        if *count == 0 {
            self.ratios.remove(&ratio);
        }
        self.total_excess -= violation.actual - violation.limit;
        Some(violation)
    }

    pub(super) fn summary(&self) -> DesignRuleSummary {
        DesignRuleSummary::new(
            self.ratios
                .last_key_value()
                .map_or(0.0, |(ratio, _)| ratio.0),
            self.total_excess,
            self.violations.len(),
        )
    }

    pub(super) fn violations(&self, model: &TimingModel) -> Vec<DesignRuleViolation> {
        let mut violations = self.violations.values().cloned().collect::<Vec<_>>();
        for violation in &mut violations {
            violation.mapped_net = model.mapped_net(violation.net);
        }
        violations.sort_by(|left, right| {
            left.kind
                .cmp(&right.kind)
                .then_with(|| (right.actual / right.limit).total_cmp(&(left.actual / left.limit)))
                .then_with(|| left.object.cmp(&right.object))
        });
        violations
    }
}

pub(super) fn design_rule_kinds() -> [DesignRuleKind; 3] {
    [
        DesignRuleKind::MaxTransition,
        DesignRuleKind::MaxCapacitance,
        DesignRuleKind::MaxFanout,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removing_a_zero_limit_violation_restores_finite_summary_state() {
        let mut index = DesignRuleIndex {
            violations: BTreeMap::new(),
            ratios: BTreeMap::new(),
            total_excess: 0.0,
        };
        let key = (DesignRuleKind::MaxFanout, 0);
        index.insert(
            key,
            DesignRuleViolation {
                kind: DesignRuleKind::MaxFanout,
                net: crate::TimingNetId::from_index(0).unwrap(),
                mapped_net: None,
                object: "n".to_string(),
                actual: 2.0,
                limit: 0.0,
            },
        );
        assert!(index.summary().worst_ratio().is_infinite());
        assert_eq!(index.summary().total_excess(), 2.0);

        index.remove(key).unwrap();
        assert_eq!(index.summary().worst_ratio(), 0.0);
        assert_eq!(index.summary().total_excess(), 0.0);
    }
}
