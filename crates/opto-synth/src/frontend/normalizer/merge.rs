// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Join orchestration and state materialization for procedural normalization.

use super::{
    Coverage, ExecutionState, FrameId, MaterializedPredicate, Predicate, ProcedureNormalizer,
    ResetList, Slot, TargetKey, constant_value, inferred_reset_kind,
    materialize_synthesis_constant, predicate, proc, word,
};
use crate::frontend::cfg::{MergeOrigin, MergeSite};
use control::{ChoiceMembership, ControlTree};

mod control;

#[derive(Debug, Clone, Copy)]
struct StateInput {
    guard: Predicate,
    state: ExecutionState,
    origin: MergeOrigin,
}

#[derive(Debug, Clone)]
struct SlotInput {
    /// Control for this alternative at its immediate decision-tree node.
    selection: Predicate,
    slot: Slot,
    origins: smallvec::SmallVec<[MergeOrigin; 2]>,
}

impl ProcedureNormalizer<'_> {
    pub(super) fn merge_edges(
        &mut self,
        block: proc::BlockId,
        edges: &[proc::EdgeId],
    ) -> Result<ExecutionState, crate::SynthError> {
        let mut inputs = edges
            .iter()
            .map(|&edge| {
                let from = self.edge_source(edge)?;
                Ok(StateInput {
                    guard: self.edge_guard(edge)?,
                    state: self.output(from)?.state,
                    origin: MergeOrigin::Edge(edge),
                })
            })
            .collect::<Result<Vec<_>, crate::SynthError>>()?;
        if inputs.iter().any(|input| input.guard != Predicate::Never) {
            inputs.retain(|input| input.guard != Predicate::Never);
        }
        Ok(ExecutionState {
            visible: self.merge_frames(
                &inputs,
                |state| state.visible,
                None,
                MergeSite::Block(block),
            )?,
            scheduled: self.merge_frames(
                &inputs,
                |state| state.scheduled,
                inferred_reset_kind(self.module, self.procedure, &self.event_controls),
                MergeSite::Block(block),
            )?,
        })
    }

    pub(super) fn merge_outputs(
        &mut self,
        blocks: &[proc::BlockId],
    ) -> Result<ExecutionState, crate::SynthError> {
        if blocks.is_empty() {
            return Err(crate::SynthError::invariant(
                "acyclic procedure has no return block",
            ));
        }
        if let [block] = blocks {
            return Ok(self.output(*block)?.state);
        }
        let mut inputs = blocks
            .iter()
            .map(|&block| {
                let output = self.output(block)?;
                Ok(StateInput {
                    guard: output.guard,
                    state: output.state,
                    origin: MergeOrigin::Return(block),
                })
            })
            .collect::<Result<Vec<_>, crate::SynthError>>()?;
        if inputs.iter().any(|input| input.guard != Predicate::Never) {
            inputs.retain(|input| input.guard != Predicate::Never);
        }
        Ok(ExecutionState {
            visible: self.merge_frames(&inputs, |state| state.visible, None, MergeSite::Exit)?,
            scheduled: self.merge_frames(
                &inputs,
                |state| state.scheduled,
                inferred_reset_kind(self.module, self.procedure, &self.event_controls),
                MergeSite::Exit,
            )?,
        })
    }

    fn merge_controlled_slots(
        &mut self,
        control: &ControlTree,
        inputs: &[SlotInput],
        reset_kind: Option<word::ResetKind>,
        key: TargetKey,
        site: MergeSite,
    ) -> Result<Slot, crate::SynthError> {
        if control.requires_predicate_fallback() {
            return self.merge_slots_plain(inputs, reset_kind, key);
        }
        let mut results = vec![None::<SlotInput>; control.len()];
        for (node_index, node) in control.postorder() {
            if !node.leaves.is_empty() && !node.choices.is_empty() {
                // A certified loop expansion can join early-exit choices with
                // its final proof-exit path. The latter bypasses every local
                // decision and therefore lives at the trie root. Preserve the
                // exact accumulated edge guards for this mixed node instead
                // of inventing a source-level choice for the proof exit.
                return self.merge_slots_plain(inputs, reset_kind, key);
            }
            if !node.leaves.is_empty() {
                let &first = node.leaves.first().ok_or_else(|| {
                    crate::SynthError::invariant("procedural control-tree leaf disappeared")
                })?;
                let result = inputs.get(first).cloned().ok_or_else(|| {
                    crate::SynthError::invariant(
                        "procedural control-tree leaf is outside its merge inputs",
                    )
                })?;
                for &leaf in &node.leaves[1..] {
                    let candidate = inputs.get(leaf).ok_or_else(|| {
                        crate::SynthError::invariant(
                            "procedural control-tree leaf is outside its merge inputs",
                        )
                    })?;
                    if !result.slot.semantically_eq(&candidate.slot) {
                        return Err(crate::SynthError::invariant(
                            "one procedural control choice produces conflicting state inputs",
                        ));
                    }
                }
                results[node_index] = Some(result);
                continue;
            }
            if node.decision.is_none() || node.choices.is_empty() {
                return Err(crate::SynthError::invariant(
                    "procedural control-tree node has neither a leaf nor a decision",
                ));
            }
            let mut alternatives = Vec::with_capacity(node.choices.len());
            for &(_, predicate, child) in &node.choices {
                let mut alternative =
                    results
                        .get_mut(child)
                        .and_then(Option::take)
                        .ok_or_else(|| {
                            crate::SynthError::invariant(
                                "procedural control-tree child was not evaluated",
                            )
                        })?;
                alternative.selection = predicate;
                alternatives.push(alternative);
            }
            let mut result = alternatives[0].clone();
            if !alternatives
                .windows(2)
                .all(|pair| pair[0].slot.semantically_eq(&pair[1].slot))
            {
                result.slot = self.merge_slots(&alternatives, reset_kind, key, site)?;
            }
            result.origins.clear();
            for alternative in &alternatives {
                result.origins.extend_from_slice(&alternative.origins);
            }
            result.selection = Predicate::Always;
            results[node_index] = Some(result);
        }
        results
            .into_iter()
            .next()
            .flatten()
            .map(|input| input.slot)
            .ok_or_else(|| {
                crate::SynthError::invariant("procedural control tree has no root result")
            })
    }

    /// Merges the incoming states of one join into a frame the join owns.
    ///
    /// The result is always a fresh child, never an input frame. A join whose
    /// inputs all carry the same frame still writes its own effects, and a
    /// block reached twice from one predecessor — which is exactly what a case
    /// item with several labels lowers to — would otherwise write them into
    /// that predecessor's output state, where every other successor reads them.
    fn merge_frames(
        &mut self,
        inputs: &[StateInput],
        frame_of: impl Fn(ExecutionState) -> FrameId,
        reset_kind: Option<word::ResetKind>,
        site: MergeSite,
    ) -> Result<FrameId, crate::SynthError> {
        let input_frames = inputs
            .iter()
            .map(|input| frame_of(input.state))
            .collect::<Vec<_>>();
        let Some(ancestor) = self.states.common_ancestor(input_frames.iter().copied())? else {
            return Err(crate::SynthError::invariant(
                "a procedural state join has no inputs",
            ));
        };
        if inputs.iter().all(|input| input.guard == Predicate::Never) {
            return Ok(self.states.child(ancestor));
        }
        if input_frames.iter().all(|&frame| frame == ancestor) {
            return Ok(self.states.child(ancestor));
        }
        let mut changed = Vec::new();
        for &input in &input_frames {
            self.states
                .collect_changed_keys(input, ancestor, &mut changed)?;
        }
        changed.sort_unstable();
        changed.dedup();

        let control = ControlTree::build(
            &self.cfg,
            &self.decision_choices,
            inputs.iter().map(|input| input.origin),
            site,
        )?;

        let frame = self.states.child(ancestor);
        for key in changed {
            let base = self.base_value(key)?;
            let base_source = self
                .module
                .value(base)
                .ok_or_else(|| crate::SynthError::invariant("procedural base value disappeared"))?
                .source
                .clone();
            let slots = inputs
                .iter()
                .zip(&input_frames)
                .map(|(state_input, input)| SlotInput {
                    selection: state_input.guard,
                    slot: self
                        .states
                        .get(*input, key)
                        .cloned()
                        .unwrap_or_else(|| Slot::unassigned(base, base_source.clone())),
                    origins: smallvec::smallvec![state_input.origin],
                })
                .collect::<Vec<_>>();
            let slot = if slots
                .windows(2)
                .all(|pair| pair[0].slot.semantically_eq(&pair[1].slot))
            {
                slots[0].slot.clone()
            } else {
                self.merge_controlled_slots(&control, &slots, reset_kind, key, site)?
            };
            let inherited = self
                .states
                .get(ancestor, key)
                .cloned()
                .unwrap_or_else(|| Slot::unassigned(base, base_source));
            if !slot.semantically_eq(&inherited) {
                self.states.set(frame, key, slot);
            }
        }
        Ok(frame)
    }

    fn merge_slots(
        &mut self,
        inputs: &[SlotInput],
        reset_kind: Option<word::ResetKind>,
        key: TargetKey,
        site: MergeSite,
    ) -> Result<Slot, crate::SynthError> {
        if let Some(kind) = reset_kind {
            let reset = if kind == word::ResetKind::Async
                && self.procedure.kind == proc::ProcedureKind::FlipFlop
                && self.event_controls.len() > 1
            {
                self.infer_event_reset(inputs, key, site)?
            } else {
                self.infer_path_reset(inputs, kind, key, site)?
            };
            if let Some(reset) = reset {
                return Ok(reset);
            }
        }
        self.merge_slots_plain(inputs, reset_kind, key)
    }

    fn merge_slots_plain(
        &mut self,
        inputs: &[SlotInput],
        reset_kind: Option<word::ResetKind>,
        key: TargetKey,
    ) -> Result<Slot, crate::SynthError> {
        if !inputs
            .windows(2)
            .all(|pair| pair[0].slot.resets == pair[1].slot.resets)
        {
            if reset_kind != Some(word::ResetKind::Sync) {
                if let Some(factored) = self.factor_common_async_resets(inputs)? {
                    return self.merge_slots_plain(&factored, reset_kind, key);
                }
                let name = self
                    .module
                    .signal(key.signal)
                    .and_then(|signal| signal.name)
                    .map_or("<unnamed>", |name| self.module.name_str(name));
                return Err(crate::SynthError::unsupported(format!(
                    "nested asynchronous reset controls for '{name}'[{} +: {}] in procedure at {:?} cannot be represented losslessly; conflicting paths: {}",
                    key.lsb,
                    key.width,
                    self.procedure.source,
                    inputs
                        .iter()
                        .map(|input| format!(
                            "guard={guard:?}, resets={:?}, assignment={:?}",
                            input.slot.resets,
                            input.slot.source,
                            guard = input.selection
                        ))
                        .collect::<Vec<_>>()
                        .join("; ")
                )));
            }
            let mut materialized = inputs.to_vec();
            for input in &mut materialized {
                self.materialize_sync_resets(&mut input.slot)?;
            }
            return self.merge_slots_plain(&materialized, reset_kind, key);
        }

        if inputs.is_empty() {
            return Err(crate::SynthError::invariant(
                "a procedural state join has no predecessors",
            ));
        }
        // The held value follows the same rule as the assigned one: paths that
        // agree on it belong under one predicate, and a select per path would
        // make the key depend on conditions it never saw.
        let mut held = Vec::<(Predicate, word::ValueId)>::with_capacity(inputs.len());
        for input in inputs {
            if let Some(index) = held
                .iter()
                .position(|&(_, value)| value == input.slot.current)
            {
                held[index].0 = self.or(held[index].0, input.selection)?;
                continue;
            }
            held.push((input.selection, input.slot.current));
        }
        let (&(_, last_current), preceding) = held
            .split_last()
            .ok_or_else(|| crate::SynthError::invariant("a procedural state join has no value"))?;
        let mut current = last_current;
        for &(guard, value) in preceding.iter().rev() {
            current = self.select(guard, value, current)?;
        }

        // Paths reaching the join with the same value differ only in how they
        // got here, so they belong under one predicate. Giving each its own
        // select would make this key depend on every condition those paths
        // branched on — conditions that never touched it — and such a
        // dependency can close a combinational loop that the source does not
        // have. Edge guards are mutually exclusive, so folding them is exact;
        // a slot carrying its own coverage is left alone because two coverages
        // may overlap.
        let mut assigned = Vec::<(Predicate, &Slot)>::with_capacity(inputs.len());
        let mut exclusive = Vec::<bool>::with_capacity(inputs.len());
        for input in inputs {
            let slot = &input.slot;
            let (predicate, guarded_by_edge) = match slot.coverage {
                Predicate::Never => continue,
                Predicate::Always => (input.selection, true),
                coverage @ Predicate::Value { .. } => (self.and(input.selection, coverage)?, false),
            };
            let existing = guarded_by_edge.then(|| {
                assigned.iter().position(|(_, candidate)| {
                    candidate.update == slot.update && candidate.coverage == Predicate::Always
                })
            });
            if let Some(Some(index)) = existing
                && exclusive[index]
            {
                assigned[index].0 = self.or(assigned[index].0, predicate)?;
                continue;
            }
            assigned.push((predicate, slot));
            exclusive.push(guarded_by_edge);
        }
        let mut update = assigned.last().map_or(current, |(_, slot)| slot.update);
        for (guard, slot) in assigned[..assigned.len().saturating_sub(1)].iter().rev() {
            update = self.select(*guard, slot.update, update)?;
        }

        let coverage = if inputs
            .iter()
            .all(|input| input.slot.coverage == Predicate::Always)
        {
            Predicate::Always
        } else {
            assigned
                .iter()
                .try_fold(Predicate::Never, |result, (guard, _)| {
                    self.or(result, *guard)
                })?
        };
        Ok(Slot {
            current,
            update,
            coverage,
            resets: inputs[0].slot.resets.clone(),
            source: self.procedure.source.clone(),
        })
    }

    fn factor_common_async_resets(
        &mut self,
        inputs: &[SlotInput],
    ) -> Result<Option<Vec<SlotInput>>, crate::SynthError> {
        let Some(common) = inputs
            .iter()
            .map(|input| &input.slot.resets)
            .find(|resets| !resets.is_empty())
            .cloned()
        else {
            return Ok(None);
        };
        if inputs
            .iter()
            .any(|input| !input.slot.resets.is_empty() && input.slot.resets != common)
        {
            return Ok(None);
        }

        let mut asserted = Predicate::Never;
        for reset in &common {
            let predicate = self.predicate(reset.value)?;
            let predicate = if reset.active_high {
                predicate
            } else {
                Self::not(predicate)
            };
            asserted = self.or(asserted, predicate)?;
        }

        let mut factored = inputs.to_vec();
        for input in &mut factored {
            if !input.slot.resets.is_empty() {
                continue;
            }
            let mut reset_asserted = self.predicates.restriction(asserted, true)?;
            if self.restrict_predicate(input.selection, &mut reset_asserted)? != Predicate::Never {
                return Ok(None);
            }
            // The reset-free path cannot be selected while any common reset
            // is asserted, so attaching the reset list changes no reachable
            // transition and permits the outer conditional hold to merge.
            input.slot.resets.clone_from(&common);
        }
        Ok(Some(factored))
    }

    fn infer_path_reset(
        &mut self,
        inputs: &[SlotInput],
        kind: word::ResetKind,
        key: TargetKey,
        site: MergeSite,
    ) -> Result<Option<Slot>, crate::SynthError> {
        for (reset_index, reset_input) in inputs.iter().enumerate() {
            if !control::has_complete_choice(
                &self.cfg,
                &self.decision_choices,
                site,
                &reset_input.origins,
            )? || reset_input.slot.coverage != Predicate::Always
                || !reset_input.slot.resets.is_empty()
                || !constant_value(self.module, reset_input.slot.update)
            {
                continue;
            }
            let source = self.procedure.source.clone();
            let condition =
                match self
                    .predicates
                    .materialize(self.module, reset_input.selection, &source)?
                {
                    MaterializedPredicate::Value(value) => value,
                    MaterializedPredicate::Never | MaterializedPredicate::Always => continue,
                };
            let mut inactive = self.predicates.restriction(reset_input.selection, false)?;
            let mut data_inputs = Vec::with_capacity(inputs.len() - 1);
            for (index, input) in inputs.iter().enumerate() {
                if index == reset_index {
                    continue;
                }
                if let Some(input) = self.restrict_input(input.clone(), &mut inactive)? {
                    data_inputs.push(input);
                }
            }
            if data_inputs.is_empty() {
                continue;
            }
            let mut data = self.merge_slots(&data_inputs, Some(kind), key, site)?;
            if data.coverage == Predicate::Never {
                continue;
            }
            let mut resets = ResetList::with_capacity(data.resets.len() + 1);
            let reset_value =
                materialize_synthesis_constant(self.module, reset_input.slot.update, &source)?;
            resets.push(word::Reset {
                kind,
                value: condition,
                active_high: true,
                reset_value,
            });
            resets.append(&mut data.resets);
            data.resets = resets;
            return Ok(Some(data));
        }
        Ok(None)
    }

    fn infer_event_reset(
        &mut self,
        inputs: &[SlotInput],
        key: TargetKey,
        site: MergeSite,
    ) -> Result<Option<Slot>, crate::SynthError> {
        let mut candidates = Vec::new();
        for &decision in self.cfg.decisions(site) {
            let Some(choices) = self.decision_choices.get(&decision) else {
                continue;
            };
            for choice in choices {
                for event in &self.event_controls {
                    if event.asserted == choice.predicate {
                        candidates.push((choice.clone(), *event));
                    }
                }
            }
        }

        for (choice, event) in candidates {
            let mut asserted = self.predicates.restriction(choice.predicate, true)?;
            let mut inactive = self.predicates.restriction(choice.predicate, false)?;
            let mut reset_value = None;
            let mut data_inputs = Vec::with_capacity(inputs.len());
            let mut valid = true;
            for input in inputs {
                match control::choice_membership(&self.cfg, &choice.edges, site, &input.origins)? {
                    ChoiceMembership::All => {
                        let Some(input) = self.restrict_input(input.clone(), &mut asserted)? else {
                            continue;
                        };
                        if input.slot.coverage != Predicate::Always
                            || !input.slot.resets.is_empty()
                            || !constant_value(self.module, input.slot.update)
                            || reset_value.is_some_and(|value| value != input.slot.update)
                        {
                            valid = false;
                            break;
                        }
                        reset_value = Some(input.slot.update);
                    }
                    ChoiceMembership::None => {
                        if let Some(input) = self.restrict_input(input.clone(), &mut inactive)? {
                            data_inputs.push(input);
                        }
                    }
                    ChoiceMembership::Mixed => {
                        valid = false;
                        break;
                    }
                }
            }
            let Some(reset_value) = reset_value.filter(|_| valid) else {
                continue;
            };
            if data_inputs.is_empty() {
                continue;
            }
            let mut data =
                self.merge_slots(&data_inputs, Some(word::ResetKind::Async), key, site)?;
            if data.coverage == Predicate::Never {
                let base = self.base_value(key)?;
                data.current = base;
                data.update = base;
                data.coverage = Predicate::Always;
            }
            let mut resets = ResetList::with_capacity(data.resets.len() + 1);
            let reset_value =
                materialize_synthesis_constant(self.module, reset_value, &self.procedure.source)?;
            resets.push(word::Reset {
                kind: word::ResetKind::Async,
                value: event.event.value,
                active_high: event.event.edge == word::Edge::Pos,
                reset_value,
            });
            resets.append(&mut data.resets);
            data.resets = resets;
            return Ok(Some(data));
        }
        Ok(None)
    }

    fn restrict_input(
        &mut self,
        mut input: SlotInput,
        restriction: &mut predicate::PredicateRestriction,
    ) -> Result<Option<SlotInput>, crate::SynthError> {
        input.selection = self.restrict_predicate(input.selection, restriction)?;
        if input.selection == Predicate::Never {
            return Ok(None);
        }
        input.slot.coverage = self.restrict_coverage(input.slot.coverage, restriction)?;
        self.restrict_resets(&mut input.slot.resets, restriction)?;
        Ok(Some(input))
    }

    pub(in crate::frontend) fn materialize_coverage(
        &mut self,
        predicate: Predicate,
    ) -> Result<Coverage, crate::SynthError> {
        let source = self.procedure.source.clone();
        Ok(
            match self
                .predicates
                .materialize(self.module, predicate, &source)?
            {
                MaterializedPredicate::Never => Coverage::Never,
                MaterializedPredicate::Always => Coverage::Always,
                MaterializedPredicate::Value(value) => Coverage::When(value),
            },
        )
    }

    pub(in crate::frontend) fn held_events(
        &mut self,
        coverage: Predicate,
    ) -> Result<smallvec::SmallVec<[proc::EventId; 2]>, crate::SynthError> {
        let controls = self.event_controls.clone();
        let mut held = smallvec::SmallVec::new();
        for control in controls {
            if control.qualified == Predicate::Never {
                continue;
            }
            let mut restricted = coverage;
            for assumption in [control.asserted, control.qualified] {
                match assumption {
                    Predicate::Never => {
                        restricted = Predicate::Never;
                        break;
                    }
                    Predicate::Always => {}
                    Predicate::Value { .. } => {
                        let mut restriction = self.predicates.restriction(assumption, true)?;
                        restricted = self.predicates.restrict(restricted, &mut restriction)?;
                    }
                }
            }
            if restricted == Predicate::Never {
                held.push(control.id);
            }
        }
        Ok(held)
    }

    fn materialize_sync_resets(&mut self, slot: &mut Slot) -> Result<(), crate::SynthError> {
        let mut reset_coverage = Predicate::Never;
        for reset in slot.resets.iter().rev() {
            if reset.kind != word::ResetKind::Sync {
                return Err(crate::SynthError::invariant(
                    "synchronous reset materialization received an asynchronous control",
                ));
            }
            let reset_predicate = self.predicate(reset.value)?;
            let asserted = if reset.active_high {
                reset_predicate
            } else {
                Self::not(reset_predicate)
            };
            slot.update = self.select(asserted, reset.reset_value, slot.update)?;
            reset_coverage = self.or(reset_coverage, asserted)?;
        }
        slot.coverage = self.or(reset_coverage, slot.coverage)?;
        slot.resets.clear();
        Ok(())
    }

    fn restrict_coverage(
        &mut self,
        coverage: Predicate,
        restriction: &mut predicate::PredicateRestriction,
    ) -> Result<Predicate, crate::SynthError> {
        self.restrict_predicate(coverage, restriction)
    }

    fn restrict_predicate(
        &mut self,
        predicate: Predicate,
        restriction: &mut predicate::PredicateRestriction,
    ) -> Result<Predicate, crate::SynthError> {
        self.predicates.restrict(predicate, restriction)
    }

    fn restrict_resets(
        &mut self,
        resets: &mut ResetList,
        restriction: &mut predicate::PredicateRestriction,
    ) -> Result<(), crate::SynthError> {
        let mut normalized = ResetList::with_capacity(resets.len());
        for mut reset in std::mem::take(resets) {
            let predicate = self.predicate(reset.value)?;
            let asserted = if reset.active_high {
                predicate
            } else {
                Self::not(predicate)
            };
            match self.restrict_predicate(asserted, restriction)? {
                Predicate::Never => {}
                predicate @ Predicate::Value { .. } => {
                    let source = self.procedure.source.clone();
                    let MaterializedPredicate::Value(value) =
                        self.predicates
                            .materialize(self.module, predicate, &source)?
                    else {
                        return Err(crate::SynthError::invariant(
                            "conditional reset predicate did not materialize to a value",
                        ));
                    };
                    reset.value = value;
                    reset.active_high = true;
                    normalized.push(reset);
                }
                Predicate::Always => {
                    return Err(crate::SynthError::unsupported(
                        "nested procedural reset is unconditional in its parent data phase",
                    ));
                }
            }
        }
        *resets = normalized;
        Ok(())
    }
}
