// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! CFG-owned promotion of persistent signal recurrence into activation state.

use super::{
    ProcExpr, ProcExprKind, ProcLocal, TransientEffect, TransientProcModule, TransientTarget,
};
use crate::proc::{AssignmentMode, ProcError, ProcExprId, ProcLocalId, ProcedureId};
use crate::word::{
    CastKind, MemoryReadTiming, SignalId, SignalKind, ValueId, ValueKind, WordModule, WordType,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy)]
struct Candidate {
    value: ValueId,
    local: ProcLocalId,
}

#[derive(Debug)]
struct ReachingValue {
    value: ProcExprId,
    definitions: BTreeSet<usize>,
}

#[derive(Debug)]
struct PeerLoop {
    procedure: ProcedureId,
    header: usize,
    exit: usize,
    natural: BTreeSet<usize>,
}

impl TransientProcModule {
    /// Promotes module-signal recurrence used by cyclic control to procedural locals.
    ///
    /// Source adapters may represent static procedural variables as persistent
    /// signals. This pass, rather than the source AST adapter, decides which of
    /// those signals are loop-carried state. It inserts copy-in effects on every
    /// external edge to a top-level natural loop. Live-out writes retain their
    /// original selected targets while nested loops share the promoted local.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] for malformed loop metadata, missing external loop
    /// entries, invalid module references, or compact-arena exhaustion.
    pub fn promote_loop_signal_state(mut self, word: &WordModule) -> Result<Self, ProcError> {
        self.validate()?;
        let top_level = self
            .loop_regions
            .iter()
            .enumerate()
            .filter_map(|(index, region)| region.parent.is_none().then_some(index))
            .collect::<Vec<_>>();
        let mut peers = BTreeMap::<SignalId, Vec<PeerLoop>>::new();
        for &region_index in &top_level {
            let region = &self.loop_regions[region_index];
            let natural = self.natural_loop_blocks(region)?;
            for signal in self.region_signal_candidates(word, region.procedure, &natural)? {
                peers.entry(signal).or_default().push(PeerLoop {
                    procedure: region.procedure,
                    header: region.header.index(),
                    exit: region.exit.index(),
                    natural: natural.clone(),
                });
            }
        }
        let mut activation_locals = BTreeMap::new();
        let mut locals = self.locals.to_vec();
        for (&signal, signal_peers) in &peers {
            for peer in signal_peers {
                if activation_locals.contains_key(&(peer.procedure, signal)) {
                    continue;
                }
                let definition = word.signal(signal).ok_or_else(|| {
                    ProcError::new("loop-state promotion found an unknown signal")
                })?;
                let local = ProcLocalId::from_index(locals.len())?;
                locals.push(ProcLocal {
                    name: format!("__loop_state_{}_{}", peer.procedure.index(), signal.index())
                        .into(),
                    ty: definition.ty,
                    source: self.blocks[peer.header].source.clone(),
                });
                activation_locals.insert((peer.procedure, signal), local);
            }
        }
        self.locals = locals.into_boxed_slice();
        for region_index in top_level {
            self.promote_region_signal_state(word, region_index, &peers, &activation_locals)?;
        }
        self.validate()?;
        Ok(self)
    }

    fn promote_region_signal_state(
        &mut self,
        word: &WordModule,
        region_index: usize,
        peers: &BTreeMap<SignalId, Vec<PeerLoop>>,
        activation_locals: &BTreeMap<(ProcedureId, SignalId), ProcLocalId>,
    ) -> Result<(), ProcError> {
        let region = self
            .loop_regions
            .get(region_index)
            .cloned()
            .ok_or_else(|| ProcError::new("loop-state promotion found an unknown region"))?;
        let natural = self.natural_loop_blocks(&region)?;
        let mut assigned = BTreeSet::new();
        let mut roots = Vec::new();
        for &block_index in &natural {
            let block = &self.blocks[block_index];
            for effect_index in block.effects.indices() {
                let effect = &self.effects[effect_index];
                roots.push(effect.value);
                Self::collect_target_expression_roots(effect.target, &mut roots);
                if let TransientTarget::Signal { signal, .. } = effect.target
                    && effect.mode == AssignmentMode::Blocking
                {
                    assigned.insert(signal);
                }
            }
            block
                .terminator
                .kind
                .for_each_expression(|expression| roots.push(expression));
        }
        assigned.extend(self.procedure_blocking_whole_assignments(region.procedure));

        let used_expressions = self.expression_closure(roots);
        let mut full_reads = BTreeMap::<SignalId, ValueId>::new();
        for &expression_index in &used_expressions {
            let ProcExprKind::ModuleValue(value) = self.expressions[expression_index].kind else {
                continue;
            };
            let Some(stored) = word.value(value) else {
                return Err(ProcError::new(
                    "loop-state promotion found an unknown module value",
                ));
            };
            let ValueKind::Signal(reference) = stored.kind else {
                continue;
            };
            let signal_width = word
                .signal(reference.signal)
                .ok_or_else(|| ProcError::new("loop-state promotion found an unknown signal"))?
                .ty
                .width();
            if reference.lsb == 0 && reference.width() == signal_width {
                full_reads.entry(reference.signal).or_insert(value);
            }
        }
        let fully_read = full_reads.keys().copied().collect::<BTreeSet<_>>();
        let signals = assigned
            .intersection(&fully_read)
            .copied()
            .collect::<Vec<_>>();
        if signals.is_empty() {
            return Ok(());
        }

        let procedure = &self.procedures[region.procedure.index()];
        let procedure_blocks = procedure
            .blocks
            .iter()
            .map(|block| block.index())
            .collect::<BTreeSet<_>>();
        let mut predecessors = BTreeMap::<usize, Vec<usize>>::new();
        for &block in &procedure.blocks {
            predecessors.entry(block.index()).or_default();
            self.blocks[block.index()]
                .terminator
                .kind
                .for_each_target(|target| {
                    predecessors
                        .entry(target.index())
                        .or_default()
                        .push(block.index());
                });
        }
        let mut external_predecessors = Vec::new();
        for &block in &procedure.blocks {
            if natural.contains(&block.index()) {
                continue;
            }
            let mut enters_header = false;
            self.blocks[block.index()]
                .terminator
                .kind
                .for_each_target(|target| enters_header |= target == region.header);
            if enters_header {
                external_predecessors.push(block.index());
            }
        }
        if external_predecessors.is_empty() {
            return Err(ProcError::new(
                "loop-carried signal state requires a canonical external loop entry",
            ));
        }

        let mut candidates = BTreeMap::new();
        for signal in signals {
            let local = *activation_locals
                .get(&(region.procedure, signal))
                .ok_or_else(|| ProcError::new("loop-state promotion lost an activation local"))?;
            candidates.insert(
                signal,
                Candidate {
                    value: full_reads[&signal],
                    local,
                },
            );
        }

        let independent = candidates
            .keys()
            .copied()
            .filter(|&signal| self.signal_is_independent_peer_state(word, signal, peers))
            .collect::<BTreeSet<_>>();

        let live_out = candidates
            .keys()
            .copied()
            .filter(|&signal| {
                !independent.contains(&signal)
                    && self.signal_is_live_outside(
                        word,
                        signal,
                        region.procedure,
                        region.exit.index(),
                        &natural,
                        peers,
                    )
            })
            .collect::<BTreeSet<_>>();

        let post_expressions = self.expression_closure(
            self.reachable_blocks(region.exit.index(), region.procedure)
                .into_iter()
                .flat_map(|block| self.block_expression_roots(block))
                .collect(),
        );
        self.rewrite_candidate_reads(
            word,
            &used_expressions,
            &post_expressions,
            &independent,
            &candidates,
        )?;

        let mut expressions = self.expressions.to_vec();
        let mut copyin_values = BTreeMap::new();
        for (&signal, candidate) in &candidates {
            let definition = word
                .signal(signal)
                .ok_or_else(|| ProcError::new("loop-state promotion found an unknown signal"))?;
            let copyin = ProcExprId::from_index(expressions.len())?;
            expressions.push(ProcExpr {
                ty: definition.ty,
                kind: ProcExprKind::ModuleValue(candidate.value),
                source: region.source.clone(),
            });
            copyin_values.insert(signal, copyin);
        }
        self.expressions = expressions.into_boxed_slice();

        let prepend = BTreeMap::<usize, Vec<TransientEffect>>::new();
        let mut append = BTreeMap::<usize, Vec<TransientEffect>>::new();
        let mut external_promotions = BTreeMap::<usize, ProcLocalId>::new();
        for predecessor in external_predecessors {
            let effects = append.entry(predecessor).or_default();
            for (&signal, candidate) in &candidates {
                let reaching = self.unique_reaching_signal_value(
                    predecessor,
                    signal,
                    &natural,
                    &procedure_blocks,
                    &predecessors,
                );
                if !live_out.contains(&signal)
                    && let Some(reaching) = &reaching
                {
                    for &definition in &reaching.definitions {
                        external_promotions.insert(definition, candidate.local);
                    }
                    continue;
                }
                let entry_value =
                    reaching.map_or(copyin_values[&signal], |reaching| reaching.value);
                effects.push(TransientEffect {
                    mode: AssignmentMode::Blocking,
                    target: TransientTarget::local(candidate.local),
                    value: entry_value,
                    source: region.source.clone(),
                });
            }
        }
        self.rebuild_effects(
            &natural,
            &candidates,
            &live_out,
            &external_promotions,
            prepend,
            append,
        )?;
        Ok(())
    }

    fn rewrite_candidate_reads(
        &mut self,
        word: &WordModule,
        used: &BTreeSet<usize>,
        post_used: &BTreeSet<usize>,
        independent: &BTreeSet<SignalId>,
        candidates: &BTreeMap<SignalId, Candidate>,
    ) -> Result<(), ProcError> {
        let old = std::mem::take(&mut self.expressions).into_vec();
        let mut expressions = Vec::with_capacity(old.len());
        let mut remap = Vec::with_capacity(old.len());
        for (index, mut expression) in old.into_iter().enumerate() {
            Self::remap_expression_operands(&mut expression.kind, &remap);
            let replacement = if let ProcExprKind::ModuleValue(value) = expression.kind
                && let Some(stored) = word.value(value)
                && let ValueKind::Signal(reference) = stored.kind
                && let Some(candidate) = candidates.get(&reference.signal)
                && (used.contains(&index)
                    || (post_used.contains(&index) && independent.contains(&reference.signal)))
            {
                let local_read = ProcExprId::from_index(expressions.len())?;
                expressions.push(ProcExpr {
                    ty: self.locals[candidate.local.index()].ty,
                    kind: ProcExprKind::LocalRead(candidate.local),
                    source: expression.source.clone(),
                });
                if reference.lsb == 0
                    && reference.width() == self.locals[candidate.local.index()].ty.width()
                {
                    local_read
                } else {
                    let extract = ProcExprId::from_index(expressions.len())?;
                    let local_type = self.locals[candidate.local.index()].ty;
                    let extracted_type = WordType::new(
                        reference.width(),
                        local_type.is_signed(),
                        local_type.state(),
                    )
                    .map_err(|error| ProcError::new(error.to_string()))?;
                    expressions.push(ProcExpr {
                        ty: extracted_type,
                        kind: ProcExprKind::Extract {
                            value: local_read,
                            lsb: reference.lsb,
                            width: std::num::NonZeroU32::new(reference.width())
                                .expect("module signal references have nonzero width"),
                        },
                        source: expression.source.clone(),
                    });
                    if extracted_type == expression.ty {
                        extract
                    } else {
                        let cast = ProcExprId::from_index(expressions.len())?;
                        expressions.push(ProcExpr {
                            ty: expression.ty,
                            kind: ProcExprKind::Cast {
                                kind: if expression.ty.is_signed() {
                                    CastKind::SignExtend
                                } else {
                                    CastKind::ZeroExtend
                                },
                                value: extract,
                            },
                            source: expression.source,
                        });
                        cast
                    }
                }
            } else {
                let replacement = ProcExprId::from_index(expressions.len())?;
                expressions.push(expression);
                replacement
            };
            remap.push(replacement);
        }
        for effect in &mut self.effects {
            effect.value = remap[effect.value.index()];
            Self::remap_target_expressions(&mut effect.target, &remap);
        }
        for block in &mut self.blocks {
            Self::remap_terminator_expressions(&mut block.terminator.kind, &remap);
        }
        self.expressions = expressions.into_boxed_slice();
        Ok(())
    }

    fn remap_expression_operands(kind: &mut ProcExprKind, remap: &[ProcExprId]) {
        let replace = |value: &mut ProcExprId| *value = remap[value.index()];
        match kind {
            ProcExprKind::ModuleValue(_)
            | ProcExprKind::Constant(_)
            | ProcExprKind::LocalRead(_) => {}
            ProcExprKind::MemoryRead {
                address, select, ..
            } => {
                replace(address);
                if let super::TransientTargetSelect::Dynamic { offset, .. } = select {
                    replace(offset);
                }
            }
            ProcExprKind::Unary { arg, .. } => replace(arg),
            ProcExprKind::Binary { left, right, .. } => {
                replace(left);
                replace(right);
            }
            ProcExprKind::Mux {
                condition,
                then_value,
                else_value,
            } => {
                replace(condition);
                replace(then_value);
                replace(else_value);
            }
            ProcExprKind::TriState { data, enable, .. } => {
                replace(data);
                replace(enable);
            }
            ProcExprKind::Concat(parts) => {
                for part in parts {
                    replace(part);
                }
            }
            ProcExprKind::Extract { value, .. } | ProcExprKind::Cast { value, .. } => {
                replace(value);
            }
            ProcExprKind::DynamicExtract { value, offset, .. } => {
                replace(value);
                replace(offset);
            }
            ProcExprKind::Insert {
                value, replacement, ..
            } => {
                replace(value);
                replace(replacement);
            }
            ProcExprKind::DynamicInsert {
                value,
                offset,
                replacement,
            } => {
                replace(value);
                replace(offset);
                replace(replacement);
            }
        }
    }

    fn remap_target_expressions(target: &mut TransientTarget, remap: &[ProcExprId]) {
        let select = match target {
            TransientTarget::Local { select, .. } | TransientTarget::Signal { select, .. } => {
                select
            }
            TransientTarget::Memory {
                address, select, ..
            } => {
                *address = remap[address.index()];
                select
            }
        };
        if let super::TransientTargetSelect::Dynamic { offset, .. } = select {
            *offset = remap[offset.index()];
        }
    }

    fn remap_terminator_expressions(
        terminator: &mut super::TransientTerminatorKind,
        remap: &[ProcExprId],
    ) {
        match terminator {
            super::TransientTerminatorKind::Return | super::TransientTerminatorKind::Jump(_) => {}
            super::TransientTerminatorKind::Branch { condition, .. } => {
                *condition = remap[condition.index()];
            }
            super::TransientTerminatorKind::Switch { selector, arms, .. } => {
                *selector = remap[selector.index()];
                for arm in arms {
                    arm.pattern = remap[arm.pattern.index()];
                }
            }
        }
    }

    fn expression_closure(&self, roots: Vec<ProcExprId>) -> BTreeSet<usize> {
        let mut used = BTreeSet::new();
        let mut pending = roots;
        while let Some(expression) = pending.pop() {
            let Some(stored) = self.expressions.get(expression.index()) else {
                continue;
            };
            if !used.insert(expression.index()) {
                continue;
            }
            stored
                .kind
                .for_each_operand(|operand| pending.push(operand));
        }
        used
    }

    fn block_expression_roots(&self, block: usize) -> Vec<ProcExprId> {
        let mut roots = Vec::new();
        let block = &self.blocks[block];
        for effect_index in block.effects.indices() {
            let effect = &self.effects[effect_index];
            roots.push(effect.value);
            Self::collect_target_expression_roots(effect.target, &mut roots);
        }
        block
            .terminator
            .kind
            .for_each_expression(|expression| roots.push(expression));
        roots
    }

    fn collect_target_expression_roots(target: TransientTarget, roots: &mut Vec<ProcExprId>) {
        let select = match target {
            TransientTarget::Local { select, .. } | TransientTarget::Signal { select, .. } => {
                select
            }
            TransientTarget::Memory {
                address, select, ..
            } => {
                roots.push(address);
                select
            }
        };
        if let super::TransientTargetSelect::Dynamic { offset, .. } = select {
            roots.push(offset);
        }
    }

    fn region_signal_candidates(
        &self,
        word: &WordModule,
        procedure: ProcedureId,
        natural: &BTreeSet<usize>,
    ) -> Result<BTreeSet<SignalId>, ProcError> {
        let mut assigned = BTreeSet::new();
        let mut roots = Vec::new();
        for &block_index in natural {
            let block = &self.blocks[block_index];
            for effect_index in block.effects.indices() {
                let effect = &self.effects[effect_index];
                roots.push(effect.value);
                Self::collect_target_expression_roots(effect.target, &mut roots);
                if let TransientTarget::Signal { signal, .. } = effect.target
                    && effect.mode == AssignmentMode::Blocking
                {
                    assigned.insert(signal);
                }
            }
            block
                .terminator
                .kind
                .for_each_expression(|expression| roots.push(expression));
        }
        assigned.extend(self.procedure_blocking_whole_assignments(procedure));
        let mut fully_read = BTreeSet::new();
        for expression_index in self.expression_closure(roots) {
            let ProcExprKind::ModuleValue(value) = self.expressions[expression_index].kind else {
                continue;
            };
            let Some(stored) = word.value(value) else {
                return Err(ProcError::new(
                    "loop-state promotion found an unknown module value",
                ));
            };
            let ValueKind::Signal(reference) = stored.kind else {
                continue;
            };
            let signal_width = word
                .signal(reference.signal)
                .ok_or_else(|| ProcError::new("loop-state promotion found an unknown signal"))?
                .ty
                .width();
            if reference.lsb == 0 && reference.width() == signal_width {
                fully_read.insert(reference.signal);
            }
        }
        Ok(assigned.intersection(&fully_read).copied().collect())
    }

    fn procedure_blocking_whole_assignments(&self, procedure: ProcedureId) -> BTreeSet<SignalId> {
        self.procedures[procedure.index()]
            .blocks
            .iter()
            .flat_map(|block| self.blocks[block.index()].effects.indices())
            .filter_map(|effect| {
                let effect = &self.effects[effect];
                if effect.mode != AssignmentMode::Blocking {
                    return None;
                }
                let TransientTarget::Signal { signal, select } = effect.target else {
                    return None;
                };
                (select == super::TransientTargetSelect::Whole).then_some(signal)
            })
            .collect()
    }

    fn signal_is_live_outside(
        &self,
        word: &WordModule,
        signal: SignalId,
        procedure: ProcedureId,
        exit: usize,
        natural: &BTreeSet<usize>,
        peers: &BTreeMap<SignalId, Vec<PeerLoop>>,
    ) -> bool {
        if word
            .signal(signal)
            .is_some_and(|definition| matches!(definition.kind, SignalKind::Port(_)))
        {
            return true;
        }
        let reachable = self.reachable_blocks(exit, procedure);
        let ignored_peer_blocks = peers
            .get(&signal)
            .into_iter()
            .flatten()
            .filter(|peer| {
                peer.natural != *natural
                    && (peer.procedure != procedure || !reachable.contains(&peer.header))
            })
            .flat_map(|peer| peer.natural.iter().copied())
            .collect::<BTreeSet<_>>();
        let mut roots = Vec::new();
        for (block_index, block) in self.blocks.iter().enumerate() {
            if natural.contains(&block_index) || ignored_peer_blocks.contains(&block_index) {
                continue;
            }
            for effect_index in block.effects.indices() {
                let effect = &self.effects[effect_index];
                roots.push(effect.value);
                if let TransientTarget::Memory {
                    address, select, ..
                } = effect.target
                {
                    roots.push(address);
                    if let super::TransientTargetSelect::Dynamic { offset, .. } = select {
                        roots.push(offset);
                    }
                }
            }
            block
                .terminator
                .kind
                .for_each_expression(|expression| roots.push(expression));
        }
        if self.expression_closure(roots).into_iter().any(|index| {
            let ProcExprKind::ModuleValue(value) = self.expressions[index].kind else {
                return false;
            };
            word.value(value).is_some_and(
                |value| matches!(value.kind, ValueKind::Signal(reference) if reference.signal == signal),
            )
        }) {
            return true;
        }

        let mut word_roots = word
            .connects()
            .iter()
            .map(|connect| connect.value)
            .collect::<Vec<_>>();
        for instance in word.instances() {
            word_roots.extend(
                instance
                    .connections
                    .iter()
                    .map(|connection| connection.value),
            );
        }
        for port in word.memory_read_ports() {
            word_roots.push(port.address);
            if let MemoryReadTiming::Synchronous { clock, enable, .. } = port.timing {
                word_roots.push(clock.value);
                if let Some(enable) = enable {
                    word_roots.push(enable.value);
                }
            }
        }
        for port in word.memory_write_ports() {
            word_roots.push(port.address);
            word_roots.push(port.data);
            word_roots.push(port.clock.value);
            if let Some(enable) = port.enable {
                word_roots.push(enable.value);
            }
            if let Some(mask) = port.mask {
                word_roots.push(mask.value);
            }
        }
        Self::word_values_reference_signal(word, word_roots, signal)
    }

    fn signal_is_independent_peer_state(
        &self,
        word: &WordModule,
        signal: SignalId,
        peers: &BTreeMap<SignalId, Vec<PeerLoop>>,
    ) -> bool {
        let Some(signal_peers) = peers.get(&signal) else {
            return false;
        };
        let procedures = signal_peers
            .iter()
            .map(|peer| peer.procedure)
            .collect::<BTreeSet<_>>();
        if procedures.len() < 2 || procedures.len() != signal_peers.len() {
            return false;
        }
        if word
            .signal(signal)
            .is_some_and(|definition| matches!(definition.kind, SignalKind::Port(_)))
        {
            return false;
        }

        for peer in signal_peers {
            for block in self.reachable_blocks(peer.exit, peer.procedure) {
                if self.blocks[block].effects.indices().any(|effect| {
                    matches!(
                        self.effects[effect].target,
                        TransientTarget::Signal { signal: target, .. } if target == signal
                    )
                }) {
                    return false;
                }
            }
        }

        let outside_roots = self
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, block)| !procedures.contains(&block.procedure))
            .flat_map(|(block, _)| self.block_expression_roots(block))
            .collect::<Vec<_>>();
        if self.expression_closure(outside_roots).into_iter().any(|index| {
            let ProcExprKind::ModuleValue(value) = self.expressions[index].kind else {
                return false;
            };
            word.value(value).is_some_and(
                |value| matches!(value.kind, ValueKind::Signal(reference) if reference.signal == signal),
            )
        }) {
            return false;
        }

        let mut word_roots = word
            .connects()
            .iter()
            .map(|connect| connect.value)
            .collect::<Vec<_>>();
        for instance in word.instances() {
            word_roots.extend(
                instance
                    .connections
                    .iter()
                    .map(|connection| connection.value),
            );
        }
        for port in word.memory_read_ports() {
            word_roots.push(port.address);
            if let MemoryReadTiming::Synchronous { clock, enable, .. } = port.timing {
                word_roots.push(clock.value);
                if let Some(enable) = enable {
                    word_roots.push(enable.value);
                }
            }
        }
        for port in word.memory_write_ports() {
            word_roots.push(port.address);
            word_roots.push(port.data);
            word_roots.push(port.clock.value);
            if let Some(enable) = port.enable {
                word_roots.push(enable.value);
            }
            if let Some(mask) = port.mask {
                word_roots.push(mask.value);
            }
        }
        !Self::word_values_reference_signal(word, word_roots, signal)
    }

    fn reachable_blocks(&self, start: usize, procedure: ProcedureId) -> BTreeSet<usize> {
        let mut reachable = BTreeSet::new();
        let mut pending = vec![start];
        while let Some(block) = pending.pop() {
            if !reachable.insert(block) || self.blocks[block].procedure != procedure {
                continue;
            }
            self.blocks[block]
                .terminator
                .kind
                .for_each_target(|target| pending.push(target.index()));
        }
        reachable
    }

    fn word_values_reference_signal(
        word: &WordModule,
        roots: Vec<ValueId>,
        signal: SignalId,
    ) -> bool {
        let mut pending = roots;
        let mut visited = BTreeSet::new();
        while let Some(value) = pending.pop() {
            if !visited.insert(value.index()) {
                continue;
            }
            let Some(stored) = word.value(value) else {
                continue;
            };
            match stored.kind {
                ValueKind::Signal(reference) if reference.signal == signal => return true,
                ValueKind::Operation(operation) => {
                    if let Some(operation) = word.operation(operation) {
                        operation.kind.for_each_input(|input| pending.push(input));
                    }
                }
                ValueKind::Signal(_) | ValueKind::Constant(_) => {}
            }
        }
        false
    }

    fn unique_reaching_signal_value(
        &self,
        block: usize,
        signal: SignalId,
        natural: &BTreeSet<usize>,
        procedure_blocks: &BTreeSet<usize>,
        predecessors: &BTreeMap<usize, Vec<usize>>,
    ) -> Option<ReachingValue> {
        let mut pending = vec![block];
        let mut visited = BTreeSet::new();
        let mut values = BTreeSet::new();
        let mut definitions = BTreeSet::new();
        let mut missing_definition = false;
        while let Some(current) = pending.pop() {
            if natural.contains(&current)
                || !procedure_blocks.contains(&current)
                || !visited.insert(current)
            {
                continue;
            }
            let mut definition = None;
            for effect_index in self.blocks[current].effects.indices().rev() {
                let effect = &self.effects[effect_index];
                let TransientTarget::Signal {
                    signal: target,
                    select,
                } = effect.target
                else {
                    continue;
                };
                if target != signal {
                    continue;
                }
                if effect.mode != AssignmentMode::Blocking
                    || select != super::TransientTargetSelect::Whole
                {
                    return None;
                }
                definition = Some(effect.value);
                definitions.insert(effect_index);
                break;
            }
            if let Some(value) = definition {
                values.insert(value);
                continue;
            }
            let incoming = predecessors.get(&current)?;
            if incoming.is_empty() {
                missing_definition = true;
            } else {
                pending.extend(incoming.iter().copied());
            }
        }
        if missing_definition || values.len() != 1 {
            return None;
        }
        Some(ReachingValue {
            value: values
                .into_iter()
                .next()
                .expect("one reaching value was established"),
            definitions,
        })
    }

    fn rebuild_effects(
        &mut self,
        natural: &BTreeSet<usize>,
        candidates: &BTreeMap<SignalId, Candidate>,
        live_out: &BTreeSet<SignalId>,
        external_promotions: &BTreeMap<usize, ProcLocalId>,
        mut prepend: BTreeMap<usize, Vec<TransientEffect>>,
        mut append: BTreeMap<usize, Vec<TransientEffect>>,
    ) -> Result<(), ProcError> {
        let old_effects = self.effects.to_vec();
        let mut effects = Vec::new();
        for (block_index, block) in self.blocks.iter_mut().enumerate() {
            let start = effects.len();
            effects.append(&mut prepend.remove(&block_index).unwrap_or_default());
            for effect_index in block.effects.indices() {
                let mut effect = old_effects[effect_index].clone();
                if let Some(&local) = external_promotions.get(&effect_index) {
                    let TransientTarget::Signal { select, .. } = effect.target else {
                        return Err(ProcError::new(
                            "loop-state promotion selected a non-signal reaching definition",
                        ));
                    };
                    effect.target = TransientTarget::Local { local, select };
                } else if natural.contains(&block_index)
                    && effect.mode == AssignmentMode::Blocking
                    && let TransientTarget::Signal { signal, select } = effect.target
                    && let Some(candidate) = candidates.get(&signal)
                {
                    if live_out.contains(&signal) {
                        effects.push(effect.clone());
                    }
                    effect.target = TransientTarget::Local {
                        local: candidate.local,
                        select,
                    };
                }
                effects.push(effect);
            }
            effects.append(&mut append.remove(&block_index).unwrap_or_default());
            block.effects =
                super::ArenaRange::new(start, effects.len() - start, "promoted transient effect")?;
        }
        self.effects = effects.into_boxed_slice();
        Ok(())
    }
}
