// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Read-only structural control analysis for procedural joins.
//!
//! This module is deliberately independent of state and Word IR mutation. It
//! preserves the decision/choice hierarchy that flat path predicates cannot
//! recover reliably once a procedure is materialized as muxes.

use crate::frontend::{DecisionChoice, Predicate, cfg};
use opto_ir::proc;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChoiceMembership {
    None,
    All,
    Mixed,
}

#[derive(Debug, Clone, Copy)]
struct ControlStep {
    decision: proc::BlockId,
    choice: usize,
    predicate: Predicate,
}

#[derive(Debug, Default)]
pub(super) struct ControlNode {
    pub(super) decision: Option<proc::BlockId>,
    pub(super) choices: smallvec::SmallVec<[(usize, Predicate, usize); 2]>,
    pub(super) leaves: smallvec::SmallVec<[usize; 2]>,
}

#[derive(Debug)]
pub(super) struct ControlTree {
    nodes: Vec<ControlNode>,
    predicate_fallback: bool,
}

impl ControlTree {
    /// Builds a compact trie of post-dominator-local control choices.
    ///
    /// Each leaf is an incoming state. Decisions are ordered by dominance, so
    /// a bottom-up consumer can eliminate a decision structurally whenever all
    /// of its children carry the same value.
    pub(super) fn build(
        cfg: &cfg::ProcedureCfg,
        decision_choices: &BTreeMap<proc::BlockId, Vec<DecisionChoice>>,
        origins: impl IntoIterator<Item = cfg::MergeOrigin>,
        site: cfg::MergeSite,
    ) -> Result<Self, crate::SynthError> {
        let origins = origins.into_iter().collect::<Vec<_>>();
        let mut tree = Self {
            nodes: vec![ControlNode::default()],
            predicate_fallback: false,
        };
        for (input_index, origin) in origins.iter().copied().enumerate() {
            let path = control_path(cfg, decision_choices, origin, site)?;
            let mut node = 0usize;
            for step in path {
                match tree.nodes[node].decision {
                    Some(decision) if decision != step.decision => {
                        return Ok(Self {
                            nodes: vec![ControlNode {
                                leaves: (0..origins.len()).collect(),
                                ..ControlNode::default()
                            }],
                            predicate_fallback: true,
                        });
                    }
                    None => tree.nodes[node].decision = Some(step.decision),
                    Some(_) => {}
                }
                let child = tree.nodes[node]
                    .choices
                    .iter()
                    .find_map(|&(choice, _, child)| (choice == step.choice).then_some(child));
                node = if let Some(child) = child {
                    child
                } else {
                    let child = tree.nodes.len();
                    tree.nodes.push(ControlNode::default());
                    tree.nodes[node]
                        .choices
                        .push((step.choice, step.predicate, child));
                    child
                };
            }
            tree.nodes[node].leaves.push(input_index);
        }
        Ok(tree)
    }

    pub(super) fn len(&self) -> usize {
        self.nodes.len()
    }

    pub(super) fn requires_predicate_fallback(&self) -> bool {
        self.predicate_fallback
    }

    /// Visits children before parents without exposing the arena's insertion
    /// order as a requirement on state materialization.
    pub(super) fn postorder(&self) -> impl Iterator<Item = (usize, &ControlNode)> {
        self.nodes.iter().enumerate().rev()
    }
}

fn control_path(
    cfg: &cfg::ProcedureCfg,
    decision_choices: &BTreeMap<proc::BlockId, Vec<DecisionChoice>>,
    origin: cfg::MergeOrigin,
    site: cfg::MergeSite,
) -> Result<smallvec::SmallVec<[ControlStep; 4]>, crate::SynthError> {
    let mut path = smallvec::SmallVec::new();
    for &decision in cfg.decisions(site) {
        let Some(choices) = decision_choices.get(&decision) else {
            continue;
        };
        let mut decision_choice = None;
        for (choice_index, choice) in choices.iter().enumerate() {
            if !cfg.choice_contains(&choice.edges, site, origin)? {
                continue;
            }
            let step = ControlStep {
                decision,
                choice: choice_index,
                predicate: choice.predicate,
            };
            if decision_choice.replace(step).is_some() {
                return Err(crate::SynthError::invariant(
                    "procedural merge origin belongs to multiple choices of one decision",
                ));
            }
        }
        if let Some(step) = decision_choice {
            path.push(step);
        }
    }
    Ok(path)
}

pub(super) fn choice_membership(
    cfg: &cfg::ProcedureCfg,
    choice: &[proc::EdgeId],
    site: cfg::MergeSite,
    origins: &[cfg::MergeOrigin],
) -> Result<ChoiceMembership, crate::SynthError> {
    if origins.is_empty() {
        return Err(crate::SynthError::invariant(
            "procedural merge input has no control-flow origins",
        ));
    }
    let mut inside = 0usize;
    for &origin in origins {
        inside += usize::from(cfg.choice_contains(choice, site, origin)?);
    }
    Ok(if inside == 0 {
        ChoiceMembership::None
    } else if inside == origins.len() {
        ChoiceMembership::All
    } else {
        ChoiceMembership::Mixed
    })
}

pub(super) fn has_complete_choice(
    cfg: &cfg::ProcedureCfg,
    decision_choices: &BTreeMap<proc::BlockId, Vec<DecisionChoice>>,
    site: cfg::MergeSite,
    origins: &[cfg::MergeOrigin],
) -> Result<bool, crate::SynthError> {
    for &decision in cfg.decisions(site) {
        let Some(choices) = decision_choices.get(&decision) else {
            continue;
        };
        for choice in choices {
            if choice_membership(cfg, &choice.edges, site, origins)? == ChoiceMembership::All {
                return Ok(true);
            }
        }
    }
    Ok(false)
}
