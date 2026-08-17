// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Language-independent bounded-loop proof and structural loop elimination.

use super::exact::{ExactEvaluator, ExactState, local_slot, unknown_state};
use super::{
    LoopRegion, ProcExprKind, TransientProcModule, TransientTarget, TransientTargetSelect,
    TransientTerminatorKind,
};
use crate::proc::{BlockId, LoopRegionId, ProcError, ProcExprId};
use crate::word::{BitRange, SourceSpan, WordModule};
use std::collections::{BTreeMap, BTreeSet};

/// Deterministic work limits for boundedness proof and structural expansion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopAnalysisLimits {
    /// Maximum blocks in the transient module after one loop-elimination
    /// operation. This is the source-profile structural acceptance boundary.
    pub max_expanded_blocks: usize,
    /// Implementation guard for distinct exact local states retained by one
    /// proof. Exhausting it reports an analysis capability gap, not an
    /// unsupported source construct.
    pub max_analysis_states: usize,
    /// Implementation guard for block-state transfer steps performed by one
    /// proof. Exhausting it reports an analysis capability gap, not an
    /// unsupported source construct.
    pub max_analysis_steps: usize,
}

impl Default for LoopAnalysisLimits {
    fn default() -> Self {
        Self {
            max_expanded_blocks: 1_048_576,
            max_analysis_states: 262_144,
            max_analysis_steps: 1_048_576,
        }
    }
}

/// Algorithm that established a finite loop bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopProofMethod {
    /// Exhaustive traversal of exact two-state procedural-local values,
    /// augmented by conservative known-bit extrema for runtime comparisons.
    ExactStateEnumeration,
}

/// Opaque certificate authorizing removal of one specific loop backedge.
///
/// The certificate borrows the exact graph and Word module used by the
/// analysis. It can therefore eliminate only that graph, and neither owner can
/// be mutated between proof and consumption. No serialized or debug-format
/// fingerprint participates in this authority contract.
pub struct LoopProof<'a> {
    _graph: &'a TransientProcModule,
    _word: &'a WordModule,
    region: LoopRegionId,
    max_header_visits: u32,
    explored_states: usize,
    method: LoopProofMethod,
}

impl std::fmt::Debug for LoopProof<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoopProof")
            .field("region", &self.region)
            .field("max_header_visits", &self.max_header_visits)
            .field("explored_states", &self.explored_states)
            .field("method", &self.method)
            .finish_non_exhaustive()
    }
}

impl LoopProof<'_> {
    /// Returns the certified loop region.
    #[must_use]
    pub const fn region(&self) -> LoopRegionId {
        self.region
    }

    /// Returns the maximum number of loop-header visits on any execution.
    #[must_use]
    pub const fn max_header_visits(&self) -> u32 {
        self.max_header_visits
    }

    /// Returns the number of distinct local states explored by the proof.
    #[must_use]
    pub const fn explored_states(&self) -> usize {
        self.explored_states
    }

    /// Returns the proof algorithm.
    #[must_use]
    pub const fn method(&self) -> LoopProofMethod {
        self.method
    }
}

/// Read-only boundedness analysis over an owned cyclic procedural graph.
///
/// The analyzer never mutates or lowers source syntax. Unknown module-level
/// values fork control conservatively, so exact-state enumeration can reject a
/// provable loop but cannot accept one merely because a runtime path was
/// ignored.
#[derive(Debug)]
pub struct LoopBoundednessAnalysis<'a> {
    graph: &'a TransientProcModule,
    word: &'a WordModule,
    limits: LoopAnalysisLimits,
}

impl<'a> LoopBoundednessAnalysis<'a> {
    /// Creates an analyzer with explicit deterministic work limits.
    #[must_use]
    pub const fn new(
        graph: &'a TransientProcModule,
        word: &'a WordModule,
        limits: LoopAnalysisLimits,
    ) -> Self {
        Self {
            graph,
            word,
            limits,
        }
    }

    /// Proves one innermost loop by exact-local-state traversal plus conservative
    /// runtime comparison facts.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] for an unknown or non-innermost region, unsupported
    /// local scheduling, a repeated reachable state, an expression whose exact
    /// local transition cannot be evaluated, or deterministic work exhaustion.
    pub fn prove_exact(&self, region: LoopRegionId) -> Result<LoopProof<'a>, ProcError> {
        let region_data =
            self.graph.loop_regions.get(region.index()).ok_or_else(|| {
                ProcError::new(format!("unknown transient loop region {region:?}"))
            })?;
        if self
            .graph
            .loop_regions
            .iter()
            .any(|candidate| candidate.parent == Some(region))
        {
            return Err(ProcError::new(
                "boundedness analysis requires innermost loops first",
            ));
        }

        let natural = self.graph.natural_loop_blocks(region_data)?;
        self.validate_iteration_dag(region_data, &natural)?;
        let max_header_visits = self.structural_header_visit_limit(&natural)?;
        let relevant_locals = self.relevant_locals(region_data, &natural)?;
        let evaluator = ExactEvaluator::new(self.graph, self.word);
        let mut transfer_steps = 0usize;
        let initial = self.initial_states(
            region_data,
            &relevant_locals,
            &evaluator,
            &mut transfer_steps,
        )?;
        if initial.is_empty() {
            return Err(ProcError::new(
                "transient loop header has no analyzable entry state",
            ));
        }

        let mut frontier = initial;
        let mut seen = BTreeSet::new();
        seen.extend(frontier.iter().cloned());
        let mut visits = 0u32;
        loop {
            visits = visits.checked_add(1).ok_or_else(|| {
                ProcError::new("transient loop header-visit count exceeds 32-bit capacity")
            })?;
            if visits > max_header_visits {
                let location = loop_source_location(region_data);
                return Err(ProcError::new(format!(
                    "loop expansion would exceed the {}-block source-profile structural limit{location}",
                    self.limits.max_expanded_blocks,
                )));
            }
            let mut next = BTreeSet::new();
            for state in &frontier {
                next.extend(self.traverse_iteration(
                    region_data,
                    &natural,
                    &relevant_locals,
                    state.clone(),
                    &evaluator,
                    &mut transfer_steps,
                )?);
            }
            if next.is_empty() {
                return Ok(LoopProof {
                    _graph: self.graph,
                    _word: self.word,
                    region,
                    max_header_visits: visits,
                    explored_states: seen.len(),
                    method: LoopProofMethod::ExactStateEnumeration,
                });
            }
            for state in &next {
                if seen.contains(state) {
                    let location = loop_source_location(region_data);
                    return Err(ProcError::new(format!(
                        "cannot prove loop finite: an exact local state reaches the header twice after {visits} header visits{location}",
                    )));
                }
            }
            seen.extend(next.iter().cloned());
            if seen.len() > self.limits.max_analysis_states {
                return Err(ProcError::new(format!(
                    "loop proof exhausts the {}-state implementation analysis budget; this is a boundedness-analysis capability gap, not an unsupported loop syntax",
                    self.limits.max_analysis_states
                )));
            }
            frontier = next;
        }
    }

    fn structural_header_visit_limit(&self, natural: &BTreeSet<usize>) -> Result<u32, ProcError> {
        let outside = self
            .graph
            .blocks
            .len()
            .checked_sub(natural.len())
            .ok_or_else(|| ProcError::new("natural loop exceeds transient block storage"))?;
        let available = self
            .limits
            .max_expanded_blocks
            .checked_sub(outside)
            .ok_or_else(|| {
                ProcError::new(format!(
                    "transient module already exceeds the {}-block source-profile structural limit",
                    self.limits.max_expanded_blocks
                ))
            })?;
        let visits = available
            .checked_div(natural.len())
            .ok_or_else(|| ProcError::new("natural loop contains no blocks"))?;
        u32::try_from(visits).map_err(|_| {
            ProcError::new("loop structural header-visit capacity exceeds 32-bit storage")
        })
    }

    fn initial_states(
        &self,
        region: &LoopRegion,
        relevant_locals: &[bool],
        evaluator: &ExactEvaluator<'_>,
        transfer_steps: &mut usize,
    ) -> Result<BTreeSet<ExactState>, ProcError> {
        let procedure = &self.graph.procedures[region.procedure.index()];
        let initial = unknown_state(self.graph.locals.len());
        let mut pending = vec![(procedure.entry, initial)];
        let mut visited = BTreeSet::new();
        let mut states = BTreeSet::new();
        while let Some((block, mut state)) = pending.pop() {
            if block == region.header {
                states.insert(state);
                continue;
            }
            if !visited.insert((block.index(), state.clone())) {
                // Earlier certified sibling loops may expand to a DAG whose
                // runtime arms reconverge with the same local state. Region
                // validation and innermost/source-order elimination ensure no
                // undeclared backedge is accepted here.
                continue;
            }
            self.consume_step(transfer_steps)?;
            self.apply_effects(block, &mut state, relevant_locals, evaluator)?;
            for successor in self.successors(block, &state, evaluator)? {
                pending.push((successor, state.clone()));
            }
        }
        Ok(states)
    }

    fn traverse_iteration(
        &self,
        region: &LoopRegion,
        natural: &BTreeSet<usize>,
        relevant_locals: &[bool],
        state: ExactState,
        evaluator: &ExactEvaluator<'_>,
        transfer_steps: &mut usize,
    ) -> Result<BTreeSet<ExactState>, ProcError> {
        let mut pending = vec![(region.header, state)];
        let mut visited = BTreeSet::new();
        let mut next = BTreeSet::new();
        while let Some((block, mut state)) = pending.pop() {
            if !visited.insert((block.index(), state.clone())) {
                // Multiple control paths may converge with the same exact
                // local state. The structural DAG check performed before the
                // traversal distinguishes such joins from undeclared cycles.
                continue;
            }
            self.consume_step(transfer_steps)?;
            self.apply_effects(block, &mut state, relevant_locals, evaluator)?;
            for successor in self.successors(block, &state, evaluator)? {
                if successor == region.header {
                    next.insert(state.clone());
                } else if natural.contains(&successor.index()) {
                    pending.push((successor, state.clone()));
                }
            }
        }
        Ok(next)
    }

    fn validate_iteration_dag(
        &self,
        region: &LoopRegion,
        natural: &BTreeSet<usize>,
    ) -> Result<(), ProcError> {
        let mut indegree = natural
            .iter()
            .copied()
            .map(|block| (block, 0usize))
            .collect::<BTreeMap<_, _>>();
        for &block_index in natural {
            let block = &self.graph.blocks[block_index];
            let mut invalid_header_edge = false;
            block.terminator.kind.for_each_target(|target| {
                if block_index == region.latch.index() && target == region.header {
                    return;
                }
                if target == region.header {
                    invalid_header_edge = true;
                } else if natural.contains(&target.index()) {
                    *indegree
                        .get_mut(&target.index())
                        .expect("natural-loop indegree owns every natural block") += 1;
                }
            });
            if invalid_header_edge {
                return Err(ProcError::new(
                    "loop contains a non-latch edge to its header",
                ));
            }
        }

        let mut ready = natural
            .iter()
            .copied()
            .filter(|&block| indegree[&block] == 0)
            .collect::<Vec<_>>();
        let mut visited = 0usize;
        while let Some(block_index) = ready.pop() {
            visited += 1;
            self.graph.blocks[block_index]
                .terminator
                .kind
                .for_each_target(|target| {
                    if block_index == region.latch.index() && target == region.header {
                        return;
                    }
                    if natural.contains(&target.index()) {
                        let degree = indegree
                            .get_mut(&target.index())
                            .expect("natural-loop indegree owns every natural block");
                        *degree -= 1;
                        if *degree == 0 {
                            ready.push(target.index());
                        }
                    }
                });
        }
        if visited != natural.len() {
            return Err(ProcError::new(
                "loop contains an internal cycle besides its declared latch-to-header backedge",
            ));
        }
        Ok(())
    }

    fn consume_step(&self, transfer_steps: &mut usize) -> Result<(), ProcError> {
        *transfer_steps = transfer_steps
            .checked_add(1)
            .ok_or_else(|| ProcError::new("loop transfer-step count exceeds address capacity"))?;
        if *transfer_steps > self.limits.max_analysis_steps {
            return Err(ProcError::new(format!(
                "loop proof exhausts the {}-step implementation analysis budget; this is a boundedness-analysis capability gap, not an unsupported loop syntax",
                self.limits.max_analysis_steps
            )));
        }
        Ok(())
    }

    fn apply_effects(
        &self,
        block: BlockId,
        state: &mut ExactState,
        relevant_locals: &[bool],
        evaluator: &ExactEvaluator<'_>,
    ) -> Result<(), ProcError> {
        for effect in self.graph.block_effects(block).ok_or_else(|| {
            ProcError::new(format!(
                "unknown transient block {block:?} during loop proof"
            ))
        })? {
            Self::apply_effect(effect, state, relevant_locals, evaluator)?;
        }
        Ok(())
    }

    fn apply_effect(
        effect: &super::TransientEffect,
        state: &mut ExactState,
        relevant_locals: &[bool],
        evaluator: &ExactEvaluator<'_>,
    ) -> Result<(), ProcError> {
        let TransientTarget::Local { local, select } = effect.target else {
            return Ok(());
        };
        if !relevant_locals.get(local.index()).copied().unwrap_or(false) {
            return Ok(());
        }
        if effect.mode != crate::proc::AssignmentMode::Blocking {
            return Err(ProcError::new(
                "exact loop proof does not model nonblocking automatic-local assignments",
            ));
        }
        let value = evaluator.evaluate(effect.value, state);
        let dynamic_offset = match select {
            TransientTargetSelect::Dynamic { offset, .. } => evaluator
                .evaluate(offset, state)
                .and_then(|value| value.unsigned_usize()),
            TransientTargetSelect::Whole | TransientTargetSelect::Static(_) => None,
        };
        let slot = local_slot(state, local)
            .ok_or_else(|| ProcError::new("loop effect targets an unknown local"))?;
        match (select, value) {
            (TransientTargetSelect::Whole, value) => *slot = value,
            (TransientTargetSelect::Static(range), Some(value)) => {
                let mut base = slot.clone().ok_or_else(|| {
                    ProcError::new("partial local update requires an exact incoming local value")
                })?;
                base.assign_slice(
                    range.msb.min(range.lsb) as usize,
                    &value,
                    range.msb < range.lsb,
                )
                .ok_or_else(|| ProcError::new("static local update is out of bounds"))?;
                *slot = Some(base);
            }
            (TransientTargetSelect::Dynamic { offset: _, width }, Some(value)) => {
                if value.width() != width.get() as usize {
                    return Err(ProcError::new(
                        "dynamic local update width does not match its value",
                    ));
                }
                if let (Some(mut base), Some(offset)) = (slot.clone(), dynamic_offset) {
                    if base.assign_slice(offset, &value, false).is_some() {
                        *slot = Some(base);
                    } else {
                        *slot = None;
                    }
                } else {
                    *slot = None;
                }
            }
            (TransientTargetSelect::Static(_) | TransientTargetSelect::Dynamic { .. }, None) => {
                *slot = None;
            }
        }
        Ok(())
    }

    fn relevant_locals(
        &self,
        region: &LoopRegion,
        natural: &BTreeSet<usize>,
    ) -> Result<Vec<bool>, ProcError> {
        let mut relevant = vec![false; self.graph.locals.len()];
        for &block in natural {
            self.graph.blocks[block]
                .terminator
                .kind
                .for_each_expression(|expression| {
                    self.collect_expression_locals(expression, &mut relevant);
                });
        }

        loop {
            let previous = relevant.iter().filter(|value| **value).count();
            for block in &self.graph.procedures[region.procedure.index()].blocks {
                let block_index = block.index();
                let block_id = BlockId::from_index(block_index)?;
                for effect in self.graph.block_effects(block_id).ok_or_else(|| {
                    ProcError::new("loop-local dependency analysis found an unknown block")
                })? {
                    let TransientTarget::Local { local, select } = effect.target else {
                        continue;
                    };
                    if !relevant.get(local.index()).copied().unwrap_or(false) {
                        continue;
                    }
                    self.collect_expression_locals(effect.value, &mut relevant);
                    if let TransientTargetSelect::Dynamic { offset, .. } = select {
                        self.collect_expression_locals(offset, &mut relevant);
                    }
                }
            }
            if relevant.iter().filter(|value| **value).count() == previous {
                return Ok(relevant);
            }
        }
    }

    fn collect_expression_locals(&self, root: crate::proc::ProcExprId, relevant: &mut [bool]) {
        let mut pending = vec![root];
        let mut visited = vec![false; self.graph.expressions.len()];
        while let Some(expression) = pending.pop() {
            let Some(stored) = self.graph.expressions.get(expression.index()) else {
                continue;
            };
            if std::mem::replace(&mut visited[expression.index()], true) {
                continue;
            }
            if let ProcExprKind::LocalRead(local) = stored.kind
                && let Some(slot) = relevant.get_mut(local.index())
            {
                *slot = true;
            }
            stored
                .kind
                .for_each_operand(|operand| pending.push(operand));
        }
    }

    fn successors(
        &self,
        block: BlockId,
        state: &ExactState,
        evaluator: &ExactEvaluator<'_>,
    ) -> Result<Vec<BlockId>, ProcError> {
        let terminator = &self.graph.blocks[block.index()].terminator.kind;
        Ok(match terminator {
            TransientTerminatorKind::Return => Vec::new(),
            TransientTerminatorKind::Jump(target) => vec![*target],
            TransientTerminatorKind::Branch {
                condition,
                then_target,
                else_target,
            } => match evaluator.evaluate_truth(*condition, state) {
                Some(true) => vec![*then_target],
                Some(false) => vec![*else_target],
                None if then_target == else_target => vec![*then_target],
                None => vec![*then_target, *else_target],
            },
            TransientTerminatorKind::Switch {
                selector,
                arms,
                default,
            } => {
                let selector = evaluator.evaluate(*selector, state);
                if let Some(selector) = selector {
                    let mut uncertain = false;
                    for arm in arms {
                        match evaluator.evaluate(arm.pattern, state) {
                            Some(pattern) if pattern == selector => return Ok(vec![arm.target]),
                            Some(_) => {}
                            None => uncertain = true,
                        }
                    }
                    if uncertain {
                        arms.iter()
                            .map(|arm| arm.target)
                            .chain(std::iter::once(*default))
                            .collect()
                    } else {
                        vec![*default]
                    }
                } else {
                    arms.iter()
                        .map(|arm| arm.target)
                        .chain(std::iter::once(*default))
                        .collect()
                }
            }
        })
    }
}

fn loop_source_location(region: &LoopRegion) -> String {
    match (
        region.source.file(),
        region.source.line(),
        region.source.column(),
    ) {
        (Some(file), Some(line), Some(column)) => format!(" at {file}:{line}:{column}"),
        (Some(file), Some(line), None) => format!(" at {file}:{line}"),
        (Some(file), None, _) => format!(" at {file}"),
        (None, Some(line), Some(column)) => format!(" at line {line}, column {column}"),
        (None, Some(line), None) => format!(" at line {line}"),
        (None, None, _) => String::new(),
    }
}

impl TransientProcModule {
    /// Proves and eliminates every loop from innermost to outermost.
    ///
    /// A fresh proof is computed after each transformation, so parent-loop
    /// certificates always describe the graph that the eliminator consumes.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] when any remaining loop cannot be proved finite or
    /// structurally expanded within `limits`.
    pub fn prove_and_eliminate_loops(
        mut self,
        word: &WordModule,
        limits: LoopAnalysisLimits,
    ) -> Result<Self, ProcError> {
        while !self.loop_regions.is_empty() {
            // Source adapters publish parents before children. Pick the first
            // leaf region so earlier sibling loops are eliminated before a
            // later loop's entry-state traversal reaches them, while nested
            // loops are still always processed before their parent.
            let region_index = (0..self.loop_regions.len())
                .find(|&index| {
                    LoopRegionId::from_index(index).is_ok_and(|region| {
                        !self
                            .loop_regions
                            .iter()
                            .any(|candidate| candidate.parent == Some(region))
                    })
                })
                .ok_or_else(|| ProcError::new("transient loop-region parent graph is cyclic"))?;
            let region = LoopRegionId::from_index(region_index)?;
            let max_header_visits = {
                let proof =
                    LoopBoundednessAnalysis::new(&self, word, limits).prove_exact(region)?;
                proof.max_header_visits()
            };
            self = self.eliminate_proved_loop(region, max_header_visits, limits)?;
        }
        self.specialize_exact_locals(word, limits)?;
        self.validate()?;
        Ok(self)
    }

    fn specialize_exact_locals(
        &mut self,
        word: &WordModule,
        limits: LoopAnalysisLimits,
    ) -> Result<(), ProcError> {
        let mut expressions = self.expressions.to_vec();
        let mut effects = self.effects.to_vec();
        let mut blocks = self.blocks.to_vec();
        let mut feasible_blocks = BTreeSet::new();
        {
            let evaluator = ExactEvaluator::new(self, word);
            let analysis = LoopBoundednessAnalysis::new(self, word, limits);
            let relevant = vec![true; self.locals.len()];
            for procedure in &self.procedures {
                let mut pending = vec![(procedure.entry, unknown_state(self.locals.len()))];
                let mut visited = BTreeSet::new();
                let mut entries = BTreeMap::<usize, BTreeSet<ExactState>>::new();
                let mut exhausted = false;
                while let Some((block, mut state)) = pending.pop() {
                    if !visited.insert((block.index(), state.clone())) {
                        continue;
                    }
                    if visited.len() > limits.max_analysis_states
                        || visited.len() > limits.max_analysis_steps
                    {
                        exhausted = true;
                        break;
                    }
                    entries
                        .entry(block.index())
                        .or_default()
                        .insert(state.clone());
                    if analysis
                        .apply_effects(block, &mut state, &relevant, &evaluator)
                        .is_err()
                    {
                        exhausted = true;
                        break;
                    }
                    for successor in analysis.successors(block, &state, &evaluator)? {
                        pending.push((successor, state.clone()));
                    }
                }
                if exhausted {
                    feasible_blocks.extend(procedure.blocks.iter().map(|block| block.index()));
                    continue;
                }
                feasible_blocks.extend(entries.keys().copied());
                for (block_index, incoming) in entries {
                    let mut states = incoming.into_iter().collect::<Vec<_>>();
                    for effect_index in self.blocks[block_index].effects.indices() {
                        let original = &self.effects[effect_index];
                        effects[effect_index].value = self.specialize_expression(
                            original.value,
                            &states,
                            &evaluator,
                            &mut expressions,
                        )?;
                        Self::specialize_target(
                            self,
                            &mut effects[effect_index].target,
                            &states,
                            &evaluator,
                            &mut expressions,
                        )?;
                        for state in &mut states {
                            LoopBoundednessAnalysis::apply_effect(
                                original, state, &relevant, &evaluator,
                            )?;
                        }
                    }
                    Self::specialize_terminator(
                        self,
                        &mut blocks[block_index].terminator.kind,
                        &states,
                        &evaluator,
                        &mut expressions,
                    )?;
                }
            }
        }
        self.expressions = expressions.into_boxed_slice();
        self.effects = effects.into_boxed_slice();
        self.blocks = blocks.into_boxed_slice();
        self.eliminate_dead_local_effects(&feasible_blocks)?;
        Ok(())
    }

    fn eliminate_dead_local_effects(
        &mut self,
        feasible_blocks: &BTreeSet<usize>,
    ) -> Result<(), ProcError> {
        let mut pending = Vec::new();
        for (block_index, block) in self.blocks.iter().enumerate() {
            if !feasible_blocks.contains(&block_index) {
                continue;
            }
            for effect_index in block.effects.indices() {
                let effect = &self.effects[effect_index];
                match effect.target {
                    TransientTarget::Memory {
                        address, select, ..
                    } => {
                        pending.push(effect.value);
                        pending.push(address);
                        if let TransientTargetSelect::Dynamic { offset, .. } = select {
                            pending.push(offset);
                        }
                    }
                    TransientTarget::Signal { select, .. } => {
                        pending.push(effect.value);
                        if let TransientTargetSelect::Dynamic { offset, .. } = select {
                            pending.push(offset);
                        }
                    }
                    TransientTarget::Local { .. } => {}
                }
            }
            block
                .terminator
                .kind
                .for_each_expression(|expression| pending.push(expression));
        }
        let mut visited = BTreeSet::new();
        let mut read = BTreeSet::new();
        loop {
            while let Some(expression) = pending.pop() {
                if !visited.insert(expression.index()) {
                    continue;
                }
                let stored = &self.expressions[expression.index()];
                if let ProcExprKind::LocalRead(local) = stored.kind {
                    read.insert(local);
                }
                stored
                    .kind
                    .for_each_operand(|operand| pending.push(operand));
            }
            for effect in &self.effects {
                let TransientTarget::Local { local, select } = effect.target else {
                    continue;
                };
                if !read.contains(&local) {
                    continue;
                }
                if !visited.contains(&effect.value.index()) {
                    pending.push(effect.value);
                }
                if let TransientTargetSelect::Dynamic { offset, .. } = select
                    && !visited.contains(&offset.index())
                {
                    pending.push(offset);
                }
            }
            if pending.is_empty() {
                break;
            }
        }

        let old = self.effects.to_vec();
        let mut effects = Vec::new();
        for block in &mut self.blocks {
            let start = effects.len();
            effects.extend(block.effects.indices().filter_map(|index| {
                let effect = &old[index];
                let dead = matches!(
                    effect.target,
                    TransientTarget::Local { local, .. } if !read.contains(&local)
                );
                (!dead).then(|| effect.clone())
            }));
            block.effects = super::ArenaRange::new(
                start,
                effects.len() - start,
                "specialized transient effect",
            )?;
        }
        self.effects = effects.into_boxed_slice();
        Ok(())
    }

    fn specialize_expression(
        &self,
        root: ProcExprId,
        states: &[ExactState],
        evaluator: &ExactEvaluator<'_>,
        expressions: &mut Vec<super::ProcExpr>,
    ) -> Result<ProcExprId, ProcError> {
        let original = &self.expressions[root.index()];
        let exact = states
            .first()
            .and_then(|state| evaluator.evaluate(root, state))
            .filter(|first| {
                states
                    .iter()
                    .skip(1)
                    .all(|state| evaluator.evaluate(root, state).as_ref() == Some(first))
            });
        if let Some(exact) = exact {
            let constant = ProcExprId::from_index(expressions.len())?;
            expressions.push(super::ProcExpr {
                ty: original.ty,
                kind: ProcExprKind::Constant(exact.to_constant().ok_or_else(|| {
                    ProcError::new("exact local specialization produced an invalid constant")
                })?),
                source: original.source.clone(),
            });
            return Ok(constant);
        }

        let mut kind = original.kind.clone();
        let mut changed = false;
        let mut error = None;
        kind.for_each_operand_mut(|operand| {
            if error.is_some() {
                return;
            }
            match self.specialize_expression(*operand, states, evaluator, expressions) {
                Ok(replacement) => {
                    changed |= replacement != *operand;
                    *operand = replacement;
                }
                Err(found) => error = Some(found),
            }
        });
        if let Some(error) = error {
            return Err(error);
        }
        if !changed {
            return Ok(root);
        }
        let replacement = ProcExprId::from_index(expressions.len())?;
        expressions.push(super::ProcExpr {
            ty: original.ty,
            kind,
            source: original.source.clone(),
        });
        Ok(replacement)
    }

    fn specialize_target(
        graph: &Self,
        target: &mut TransientTarget,
        states: &[ExactState],
        evaluator: &ExactEvaluator<'_>,
        expressions: &mut Vec<super::ProcExpr>,
    ) -> Result<(), ProcError> {
        let select = match target {
            TransientTarget::Local { select, .. } | TransientTarget::Signal { select, .. } => {
                select
            }
            TransientTarget::Memory {
                address, select, ..
            } => {
                *address = graph.specialize_expression(*address, states, evaluator, expressions)?;
                select
            }
        };
        if let TransientTargetSelect::Dynamic { offset, width } = *select {
            let exact = states
                .first()
                .and_then(|state| evaluator.evaluate(offset, state))
                .filter(|first| {
                    states
                        .iter()
                        .skip(1)
                        .all(|state| evaluator.evaluate(offset, state).as_ref() == Some(first))
                })
                .and_then(|value| value.unsigned_usize())
                .and_then(|offset| u32::try_from(offset).ok());
            if let Some(lsb) = exact
                && let Some(msb) = lsb.checked_add(width.get() - 1)
            {
                *select = TransientTargetSelect::Static(BitRange { msb, lsb });
            } else if let TransientTargetSelect::Dynamic { offset, .. } = select {
                *offset = graph.specialize_expression(*offset, states, evaluator, expressions)?;
            }
        }
        Ok(())
    }

    fn specialize_terminator(
        graph: &Self,
        terminator: &mut TransientTerminatorKind,
        states: &[ExactState],
        evaluator: &ExactEvaluator<'_>,
        expressions: &mut Vec<super::ProcExpr>,
    ) -> Result<(), ProcError> {
        match terminator {
            TransientTerminatorKind::Return | TransientTerminatorKind::Jump(_) => {}
            TransientTerminatorKind::Branch { condition, .. } => {
                *condition =
                    graph.specialize_expression(*condition, states, evaluator, expressions)?;
            }
            TransientTerminatorKind::Switch { selector, arms, .. } => {
                *selector =
                    graph.specialize_expression(*selector, states, evaluator, expressions)?;
                for arm in arms {
                    arm.pattern =
                        graph.specialize_expression(arm.pattern, states, evaluator, expressions)?;
                }
            }
        }
        Ok(())
    }

    fn eliminate_proved_loop(
        mut self,
        proved_region: LoopRegionId,
        max_header_visits: u32,
        limits: LoopAnalysisLimits,
    ) -> Result<Self, ProcError> {
        let region = self
            .loop_regions
            .get(proved_region.index())
            .cloned()
            .ok_or_else(|| ProcError::new("loop proof references an unknown region"))?;
        if self
            .loop_regions
            .iter()
            .any(|candidate| candidate.parent == Some(proved_region))
        {
            return Err(ProcError::new(
                "loop elimination requires innermost loops first",
            ));
        }
        let natural = self.natural_loop_blocks(&region)?;
        self.validate_natural_loop_entry(&region, &natural)?;
        let natural_blocks = natural.iter().copied().collect::<Vec<_>>();
        let copies = usize::try_from(max_header_visits)
            .map_err(|_| ProcError::new("loop proof bound exceeds address capacity"))?;
        if copies == 0 {
            return Err(ProcError::new(
                "loop proof must certify at least one header visit",
            ));
        }
        let added = natural
            .len()
            .checked_mul(copies)
            .and_then(|count| count.checked_add(self.blocks.len() - natural.len()))
            .ok_or_else(|| ProcError::new("loop expansion block count overflows"))?;
        if added > limits.max_expanded_blocks {
            return Err(ProcError::new(format!(
                "loop expansion would produce {added} blocks, exceeding the {}-block limit",
                limits.max_expanded_blocks
            )));
        }

        let copy_slots = natural_blocks
            .len()
            .checked_mul(copies)
            .ok_or_else(|| ProcError::new("loop copy-map size overflows"))?;
        let mut copy_map = vec![None; copy_slots];
        for (slot, &original) in natural_blocks.iter().enumerate() {
            copy_map[slot] = Some(BlockId::from_index(original)?);
        }

        let templates = natural_blocks
            .iter()
            .map(|&index| {
                let block = self.blocks[index].clone();
                let effects = block
                    .effects
                    .indices()
                    .map(|effect| self.effects[effect].clone())
                    .collect::<Vec<_>>();
                (block, effects)
            })
            .collect::<Vec<_>>();
        let mut blocks = std::mem::take(&mut self.blocks).into_vec();
        let mut effects = std::mem::take(&mut self.effects).into_vec();

        for iteration in 1..copies {
            for (slot, (template, template_effects)) in templates.iter().enumerate() {
                let effect_start = effects.len();
                effects.extend(template_effects.iter().cloned().map(|mut effect| {
                    effect.source = cloned_source(&effect.source, iteration);
                    effect
                }));
                let output = BlockId::from_index(blocks.len())?;
                copy_map[iteration * natural_blocks.len() + slot] = Some(output);
                blocks.push(super::TransientBlock {
                    procedure: template.procedure,
                    terminator: template.terminator.clone(),
                    source: cloned_source(&template.source, iteration),
                    effects: super::ArenaRange::new(
                        effect_start,
                        template_effects.len(),
                        "unrolled transient effect",
                    )?,
                });
            }
        }

        let copied_block = |iteration: usize, block: BlockId| -> Result<BlockId, ProcError> {
            let slot = natural_blocks
                .binary_search(&block.index())
                .map_err(|_| ProcError::new("cannot find dense loop-block slot"))?;
            copy_map
                .get(iteration * natural_blocks.len() + slot)
                .copied()
                .flatten()
                .ok_or_else(|| ProcError::new("cannot map cloned loop block"))
        };

        for (slot, (template, _)) in templates.iter().enumerate() {
            let original = BlockId::from_index(natural_blocks[slot])?;
            for iteration in 0..copies {
                let output = copied_block(iteration, original)?;
                let map = |target: BlockId| -> Result<BlockId, ProcError> {
                    if original == region.latch && target == region.header {
                        if iteration + 1 < copies {
                            copied_block(iteration + 1, target)
                        } else {
                            Ok(region.exit)
                        }
                    } else if target == region.header {
                        Err(ProcError::new(
                            "loop contains a non-latch edge to its header",
                        ))
                    } else if natural.contains(&target.index()) {
                        copied_block(iteration, target)
                    } else {
                        Ok(target)
                    }
                };
                blocks[output.index()].terminator =
                    remapped_terminator(&template.terminator, iteration, map)?;
            }
        }

        let procedure = &mut self.procedures[region.procedure.index()];
        let old_order = std::mem::take(&mut procedure.blocks).into_vec();
        let mut new_order = Vec::with_capacity(old_order.len() - natural.len() + copy_slots);
        let mut inserted = false;
        for block in old_order {
            if natural.contains(&block.index()) {
                if !inserted {
                    for iteration in 0..copies {
                        for &original in &natural_blocks {
                            new_order
                                .push(copied_block(iteration, BlockId::from_index(original)?)?);
                        }
                    }
                    inserted = true;
                }
            } else {
                new_order.push(block);
            }
        }
        procedure.blocks = new_order.into_boxed_slice();
        self.blocks = blocks.into_boxed_slice();
        self.effects = effects.into_boxed_slice();

        let old_regions = std::mem::take(&mut self.loop_regions).into_vec();
        let mut new_regions = Vec::with_capacity(old_regions.len() - 1);
        let mut region_map = vec![None; old_regions.len()];
        for (index, mut candidate) in old_regions.into_iter().enumerate() {
            if index == proved_region.index() {
                continue;
            }
            candidate.parent = match candidate.parent {
                Some(parent) if parent == proved_region => {
                    return Err(ProcError::new(
                        "loop elimination encountered a non-innermost region",
                    ));
                }
                Some(parent) => Some(region_map[parent.index()].ok_or_else(|| {
                    ProcError::new("cannot remap enclosing loop region metadata")
                })?),
                None => None,
            };
            let new_region = LoopRegionId::from_index(new_regions.len())?;
            new_regions.push(candidate);
            region_map[index] = Some(new_region);
        }
        self.loop_regions = new_regions.into_boxed_slice();
        Ok(self)
    }

    pub(super) fn natural_loop_blocks(
        &self,
        region: &LoopRegion,
    ) -> Result<BTreeSet<usize>, ProcError> {
        let procedure = &self.procedures[region.procedure.index()];
        let mut predecessors = procedure
            .blocks
            .iter()
            .map(|block| (block.index(), Vec::new()))
            .collect::<BTreeMap<_, _>>();
        for block_index in procedure.blocks.iter().map(|block| block.index()) {
            self.blocks[block_index]
                .terminator
                .kind
                .for_each_target(|target| {
                    predecessors
                        .get_mut(&target.index())
                        .expect("validated CFG target belongs to its procedure")
                        .push(block_index);
                });
        }
        let mut natural = BTreeSet::from([region.header.index(), region.latch.index()]);
        let mut pending = vec![region.latch.index()];
        while let Some(block) = pending.pop() {
            for predecessor in predecessors[&block].iter().copied() {
                if predecessor != region.header.index() && natural.insert(predecessor) {
                    pending.push(predecessor);
                }
            }
        }
        if !natural.contains(&region.body.index()) || natural.contains(&region.exit.index()) {
            return Err(ProcError::new(
                "loop metadata does not describe a canonical natural region",
            ));
        }
        Ok(natural)
    }

    fn validate_natural_loop_entry(
        &self,
        region: &LoopRegion,
        natural: &BTreeSet<usize>,
    ) -> Result<(), ProcError> {
        for block in &self.procedures[region.procedure.index()].blocks {
            let index = block.index();
            let block = &self.blocks[index];
            let mut invalid = false;
            block.terminator.kind.for_each_target(|target| {
                if !natural.contains(&index)
                    && natural.contains(&target.index())
                    && target != region.header
                {
                    invalid = true;
                }
            });
            if invalid {
                return Err(ProcError::new(
                    "natural loop has an external edge into a non-header block",
                ));
            }
        }
        Ok(())
    }
}

fn remapped_terminator(
    terminator: &super::TransientTerminator,
    iteration: usize,
    mut map: impl FnMut(BlockId) -> Result<BlockId, ProcError>,
) -> Result<super::TransientTerminator, ProcError> {
    let kind = match &terminator.kind {
        TransientTerminatorKind::Return => TransientTerminatorKind::Return,
        TransientTerminatorKind::Jump(target) => TransientTerminatorKind::Jump(map(*target)?),
        TransientTerminatorKind::Branch {
            condition,
            then_target,
            else_target,
        } => TransientTerminatorKind::Branch {
            condition: *condition,
            then_target: map(*then_target)?,
            else_target: map(*else_target)?,
        },
        TransientTerminatorKind::Switch {
            selector,
            arms,
            default,
        } => TransientTerminatorKind::Switch {
            selector: *selector,
            arms: arms
                .iter()
                .map(|arm| {
                    Ok(super::TransientSwitchArm {
                        pattern: arm.pattern,
                        target: map(arm.target)?,
                        source: cloned_source(&arm.source, iteration),
                    })
                })
                .collect::<Result<Vec<_>, ProcError>>()?
                .into_boxed_slice(),
            default: map(*default)?,
        },
    };
    Ok(super::TransientTerminator {
        kind,
        source: cloned_source(&terminator.source, iteration),
    })
}

fn cloned_source(source: &SourceSpan, iteration: usize) -> SourceSpan {
    source
        .derived("unrolled loop iteration", iteration.to_le_bytes())
        .unwrap_or_else(|| source.clone())
}
