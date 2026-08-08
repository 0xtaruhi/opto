// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    BTreeMap, BitVal, BlockOutput, Coverage, DecisionChoice, EventControl, ExecutionState, FrameId,
    HashSet, MaterializedPredicate, PendingWrite, Predicate, PredicateArena, ProcedureCfg,
    ProcedureInput, ProcedureNormalizer, ResetList, SignalResolutionContext, Slot, StateArena,
    TargetKey, block_effects, cfg, constant_value, derived_source, extract_assignment,
    inferred_reset_kind, memory_write_data, predicate, proc, resolve_signal, rewrite_value,
    target_layout, word,
};

mod merge;

impl<'a> ProcedureNormalizer<'a> {
    pub(super) fn base_value(&self, key: TargetKey) -> Result<word::ValueId, crate::SynthError> {
        self.bases.get(&key).copied().ok_or_else(|| {
            crate::SynthError::invariant("procedural target has no entry-state value")
        })
    }

    pub(super) fn new(
        procedure_id: proc::ProcedureId,
        cfg: ProcedureCfg,
        input: ProcedureInput<'a>,
    ) -> Result<Self, crate::SynthError> {
        let ProcedureInput {
            module,
            procedures,
            reads,
            outputs,
            edge_guards,
            rewrite_scratch,
            incomplete_comb,
        } = input;
        let procedure = procedures
            .procedure(procedure_id)
            .ok_or_else(|| crate::SynthError::invariant("procedural definition disappeared"))?;
        let layout = target_layout(module, procedures, cfg.blocks())?;
        let keys = layout.values().flatten().copied().collect::<Vec<_>>();
        let bases = keys
            .iter()
            .map(|&key| {
                let signal = module.signal(key.signal).ok_or_else(|| {
                    crate::SynthError::invariant("procedural target signal disappeared")
                })?;
                let signal_identity = signal.source.identity().ok_or_else(|| {
                    crate::SynthError::invariant(
                        "procedural target declaration has no stable source identity",
                    )
                })?;
                let mut role = Vec::with_capacity(40);
                role.extend_from_slice(&signal_identity.bytes());
                role.extend_from_slice(&key.lsb.to_le_bytes());
                role.extend_from_slice(&key.width.to_le_bytes());
                let source = derived_source(&procedure.source, "procedural state entry", &role)?;
                module
                    .read_signal_slice(key.signal, key.lsb, key.width, source)
                    .map(|value| (key, value))
                    .map_err(crate::SynthError::from)
            })
            .collect::<Result<_, _>>()?;
        let mut predicates = PredicateArena::new();
        let mut event_controls = Vec::new();
        if procedure.kind == proc::ProcedureKind::FlipFlop {
            for (_, event) in procedures.sensitivity_events(procedure_id).ok_or_else(|| {
                crate::SynthError::invariant("edge-sensitive procedure lost its sensitivity events")
            })? {
                let mut role = Vec::with_capacity(33);
                let signal = module.signal(event.signal).ok_or_else(|| {
                    crate::SynthError::invariant("sensitivity signal disappeared")
                })?;
                let identity = signal.source.identity().ok_or_else(|| {
                    crate::SynthError::invariant(
                        "sensitivity signal declaration has no stable source identity",
                    )
                })?;
                role.extend_from_slice(&identity.bytes());
                role.push(u8::from(event.edge == word::Edge::Neg));
                let source = derived_source(&procedure.source, "sensitivity read", &role)?;
                let value = module
                    .read_signal(event.signal, source)
                    .map_err(crate::SynthError::from)?;
                let predicate = predicates.value(module, value)?;
                event_controls.push(EventControl {
                    event: *event,
                    value,
                    asserted: if event.edge == word::Edge::Pos {
                        predicate
                    } else {
                        PredicateArena::not(predicate)
                    },
                });
            }
        }
        Ok(Self {
            module,
            procedures,
            procedure_id,
            procedure,
            cfg,
            layout,
            keys,
            bases,
            reads,
            rewrite_scratch,
            predicates,
            event_controls,
            decision_choices: BTreeMap::new(),
            states: StateArena::default(),
            outputs,
            edge_guards,
            writes: Vec::new(),
            incomplete_comb,
        })
    }

    pub(super) fn run(mut self) -> Result<(), crate::SynthError> {
        self.validate_assignment_styles()?;
        for block in self.cfg.order().to_vec() {
            self.lower_block(block)?;
        }
        let returns = self.cfg.returns().to_vec();
        let final_state = self.merge_outputs(&returns)?;
        self.emit(final_state)
    }

    fn validate_assignment_styles(&self) -> Result<(), crate::SynthError> {
        let mut style = None;
        for &block in self.cfg.blocks() {
            for (_, effect) in block_effects(self.procedures, block)? {
                let proc::ProcTarget::Signal { signal, .. } = effect.target else {
                    if self.procedure.kind != proc::ProcedureKind::FlipFlop {
                        return Err(crate::SynthError::unsupported(
                            "procedural memory writes require an edge-sensitive procedure",
                        ));
                    }
                    continue;
                };
                let local = self
                    .module
                    .signal(signal)
                    .is_some_and(|signal| signal.kind == word::SignalKind::ProcessLocal);
                if local && effect.mode == proc::AssignmentMode::Nonblocking {
                    return Err(crate::SynthError::unsupported(
                        "process-local assignments must be blocking",
                    ));
                }
                if self.procedure.kind == proc::ProcedureKind::Combinational
                    && effect.mode == proc::AssignmentMode::Nonblocking
                {
                    return Err(crate::SynthError::unsupported(
                        "nonblocking assignment in always_comb",
                    ));
                }
                if local
                    || !matches!(
                        self.procedure.kind,
                        proc::ProcedureKind::CombinationalOrLatch | proc::ProcedureKind::Latch
                    )
                {
                    continue;
                }
                let blocking = effect.mode == proc::AssignmentMode::Blocking;
                if style.replace(blocking).is_some_and(|old| old != blocking) {
                    return Err(crate::SynthError::unsupported(
                        "a latch procedure cannot mix blocking and nonblocking persistent assignments",
                    ));
                }
            }
        }
        Ok(())
    }

    fn lower_block(&mut self, block: proc::BlockId) -> Result<(), crate::SynthError> {
        let (mut state, guard) = self.block_input(block)?;
        if guard != Predicate::Never {
            for (_, effect) in block_effects(self.procedures, block)? {
                self.lower_effect(&mut state, guard, effect)?;
            }
        }
        self.outputs[block.index()] = Some(BlockOutput { state, guard });
        self.lower_terminator(block, state, guard)
    }

    fn block_input(
        &mut self,
        block: proc::BlockId,
    ) -> Result<(ExecutionState, Predicate), crate::SynthError> {
        let incoming = self.cfg.predecessors(block).to_vec();
        if block == self.cfg.entry() {
            return Ok((
                ExecutionState {
                    visible: self.states.root(),
                    scheduled: self.states.root(),
                },
                Predicate::Always,
            ));
        }
        let guard = incoming.iter().try_fold(Predicate::Never, |guard, edge| {
            let edge_guard = self.edge_guard(*edge)?;
            self.or(guard, edge_guard)
        })?;
        let state = if let [edge] = incoming.as_slice() {
            let source = self.edge_source(*edge)?;
            let parent = self.output(source)?.state;
            if matches!(self.cfg.terminator(source)?, cfg::Terminator::Jump { .. }) {
                parent
            } else {
                ExecutionState {
                    visible: self.states.child(parent.visible),
                    scheduled: self.states.child(parent.scheduled),
                }
            }
        } else {
            self.merge_edges(block, &incoming)?
        };
        Ok((state, guard))
    }

    fn lower_effect(
        &mut self,
        state: &mut ExecutionState,
        guard: Predicate,
        effect: &proc::Effect,
    ) -> Result<(), crate::SynthError> {
        let value = self.rewrite(state.visible, effect.value)?;
        match effect.target {
            proc::ProcTarget::Signal { signal, select } => {
                let select = self.rewrite_select(state.visible, select)?;
                if effect.mode == proc::AssignmentMode::Blocking {
                    self.assign(state.visible, signal, select, value, &effect.source)?;
                }
                self.assign(state.scheduled, signal, select, value, &effect.source)
            }
            proc::ProcTarget::Memory {
                memory,
                address,
                select,
            } => {
                if self.procedure.kind != proc::ProcedureKind::FlipFlop {
                    return Err(crate::SynthError::unsupported(
                        "procedural memory writes require always_ff",
                    ));
                }
                let address = self.rewrite(state.visible, address)?;
                let select = self.rewrite_select(state.visible, select)?;
                let (data, mask) =
                    memory_write_data(self.module, memory, select, value, &effect.source)?;
                let write = PendingWrite {
                    memory,
                    address,
                    data,
                    mask,
                    guard: self
                        .predicates
                        .materialize(self.module, guard, &effect.source)?,
                    blocking: effect.mode == proc::AssignmentMode::Blocking,
                    source: effect.source.clone(),
                };
                self.writes.push(write);
                Ok(())
            }
        }
    }

    fn rewrite_select(
        &mut self,
        frame: FrameId,
        select: proc::TargetSelect,
    ) -> Result<proc::TargetSelect, crate::SynthError> {
        Ok(match select {
            proc::TargetSelect::Dynamic { offset, width } => proc::TargetSelect::Dynamic {
                offset: self.rewrite(frame, offset)?,
                width,
            },
            other => other,
        })
    }

    fn assign(
        &mut self,
        frame: FrameId,
        signal: word::SignalId,
        select: proc::TargetSelect,
        value: word::ValueId,
        source: &word::SourceSpan,
    ) -> Result<(), crate::SynthError> {
        let updated = if let proc::TargetSelect::Dynamic { offset, .. } = select {
            let original = self
                .module
                .read_signal(signal, source.clone())
                .map_err(crate::SynthError::from)?;
            let word::ValueKind::Signal(reference) = self.value(original)?.kind else {
                unreachable!("read_signal must return a signal reference");
            };
            let base = self.resolve_signal(frame, reference, source)?;
            Some(
                self.module
                    .dynamic_insert(
                        base,
                        offset,
                        value,
                        derived_source(source, "procedural dynamic assignment", b"insert")?,
                    )
                    .map_err(crate::SynthError::from)?,
            )
        } else {
            None
        };
        let keys = self
            .layout
            .get(&signal)
            .ok_or_else(|| crate::SynthError::invariant("procedural target has no state layout"))?;
        match select {
            proc::TargetSelect::Whole => {
                for &key in keys {
                    let value = extract_assignment(self.module, value, key.lsb, key.width, source)?;
                    self.states
                        .set(frame, key, Slot::assigned(value, source.clone()));
                }
            }
            proc::TargetSelect::Static(range) => {
                if range.msb < range.lsb {
                    return Err(crate::SynthError::unsupported(
                        "ascending procedural part-select targets",
                    ));
                }
                let end = range
                    .lsb
                    .checked_add(range.width())
                    .ok_or_else(|| crate::SynthError::capacity("part-select range overflow"))?;
                for &key in keys
                    .iter()
                    .filter(|key| key.lsb >= range.lsb && key.lsb + key.width <= end)
                {
                    let value = extract_assignment(
                        self.module,
                        value,
                        key.lsb - range.lsb,
                        key.width,
                        source,
                    )?;
                    self.states
                        .set(frame, key, Slot::assigned(value, source.clone()));
                }
            }
            proc::TargetSelect::Dynamic { .. } => {
                let updated = updated.ok_or_else(|| {
                    crate::SynthError::invariant(
                        "dynamic procedural target produced no inserted value",
                    )
                })?;
                for &key in keys {
                    let value =
                        extract_assignment(self.module, updated, key.lsb, key.width, source)?;
                    self.states
                        .set(frame, key, Slot::assigned(value, source.clone()));
                }
            }
        }
        Ok(())
    }

    fn rewrite(
        &mut self,
        frame: FrameId,
        value: word::ValueId,
    ) -> Result<word::ValueId, crate::SynthError> {
        let states = &self.states;
        let layout = &self.layout;
        let bases = &self.bases;
        let reads = self.reads;
        let writes = &self.writes;
        let context = SignalResolutionContext {
            states,
            layout,
            bases,
            reads,
            writes,
        };
        rewrite_value(
            self.module,
            value,
            self.rewrite_scratch,
            |module, original, reference| {
                resolve_signal(module, &context, frame, original, reference)
            },
        )
    }

    pub(super) fn resolve_signal(
        &mut self,
        frame: FrameId,
        reference: word::SignalRef,
        parent: &word::SourceSpan,
    ) -> Result<word::ValueId, crate::SynthError> {
        let signal = self
            .module
            .signal(reference.signal)
            .ok_or_else(|| crate::SynthError::invariant("procedural read signal disappeared"))?;
        let identity = signal.source.identity().ok_or_else(|| {
            crate::SynthError::invariant(
                "procedural read signal declaration has no stable source identity",
            )
        })?;
        let mut role = Vec::with_capacity(40);
        role.extend_from_slice(&identity.bytes());
        role.extend_from_slice(&reference.lsb.to_le_bytes());
        role.extend_from_slice(&reference.width().to_le_bytes());
        let source = derived_source(parent, "procedural read", role)?;
        let original = self
            .module
            .read_signal_slice(reference.signal, reference.lsb, reference.width(), source)
            .map_err(crate::SynthError::from)?;
        let context = SignalResolutionContext {
            states: &self.states,
            layout: &self.layout,
            bases: &self.bases,
            reads: self.reads,
            writes: &self.writes,
        };
        resolve_signal(self.module, &context, frame, original, reference)?
            .ok_or_else(|| crate::SynthError::invariant("procedural target state was not resolved"))
    }

    fn lower_terminator(
        &mut self,
        block: proc::BlockId,
        state: ExecutionState,
        guard: Predicate,
    ) -> Result<(), crate::SynthError> {
        let terminator = self.cfg.terminator(block)?.clone();
        match terminator {
            cfg::Terminator::Return => Ok(()),
            cfg::Terminator::Jump { edge } => self.set_edge_guard(edge, guard),
            cfg::Terminator::Branch {
                condition,
                then_edge,
                else_edge,
            } => {
                if guard == Predicate::Never {
                    self.set_edge_guard(then_edge, Predicate::Never)?;
                    return self.set_edge_guard(else_edge, Predicate::Never);
                }
                let condition = self.rewrite(state.visible, condition)?;
                let condition = self.predicate(condition)?;
                let inverse = Self::not(condition);
                self.decision_choices.insert(
                    block,
                    vec![
                        DecisionChoice {
                            edges: smallvec::smallvec![then_edge],
                            predicate: condition,
                        },
                        DecisionChoice {
                            edges: smallvec::smallvec![else_edge],
                            predicate: inverse,
                        },
                    ],
                );
                let then_guard = self.and(guard, condition)?;
                let else_guard = self.and(guard, inverse)?;
                self.set_edge_guard(then_edge, then_guard)?;
                self.set_edge_guard(else_edge, else_guard)
            }
            cfg::Terminator::Switch {
                selector,
                arms,
                default,
            } => {
                if guard == Predicate::Never {
                    for arm in &arms {
                        self.set_edge_guard(arm.edge, Predicate::Never)?;
                    }
                    return self.set_edge_guard(default, Predicate::Never);
                }
                let selector = self.rewrite(state.visible, selector)?;
                let exhaustive = self.switch_covers_binary_domain(selector, &arms)?;
                let mut remaining = guard;
                let mut local_remaining = Predicate::Always;
                let mut choices = Vec::with_capacity(arms.len() + usize::from(!exhaustive));
                for arm in &arms {
                    let pattern = self.rewrite(state.visible, arm.pattern)?;
                    let arm_source = &self
                        .procedures
                        .edge(arm.edge)
                        .ok_or_else(|| crate::SynthError::invariant("case arm edge disappeared"))?
                        .source_span;
                    let matched = self
                        .module
                        .binary(
                            word::BinaryOp::Eq,
                            selector,
                            pattern,
                            derived_source(arm_source, "case comparison", b"match")?,
                        )
                        .map_err(crate::SynthError::from)?;
                    let matched = self.predicate(matched)?;
                    let arm_guard = self.and(remaining, matched)?;
                    self.set_edge_guard(arm.edge, arm_guard)?;
                    choices.push(DecisionChoice {
                        edges: smallvec::smallvec![arm.edge],
                        predicate: self.and(local_remaining, matched)?,
                    });
                    let unmatched = Self::not(matched);
                    remaining = self.and(remaining, unmatched)?;
                    local_remaining = self.and(local_remaining, unmatched)?;
                }
                if !exhaustive {
                    choices.push(DecisionChoice {
                        edges: smallvec::smallvec![default],
                        predicate: local_remaining,
                    });
                }
                let choices = self.coalesce_choices(choices)?;
                self.decision_choices.insert(block, choices);
                self.set_edge_guard(
                    default,
                    if exhaustive {
                        Predicate::Never
                    } else {
                        remaining
                    },
                )
            }
        }
    }

    fn switch_covers_binary_domain(
        &self,
        selector: word::ValueId,
        arms: &[cfg::SwitchArm],
    ) -> Result<bool, crate::SynthError> {
        let selector = self.module.value(selector).ok_or_else(|| {
            crate::SynthError::invariant("canonical switch selector is absent from Word IR")
        })?;
        let width = selector.ty.width();
        let Some(domain_size) = 1_usize.checked_shl(width) else {
            return Ok(false);
        };
        if arms.len() != domain_size {
            return Ok(false);
        }
        let mut patterns = HashSet::with_capacity(arms.len());
        for arm in arms {
            let pattern = self.module.value(arm.pattern).ok_or_else(|| {
                crate::SynthError::invariant("canonical switch pattern is absent from Word IR")
            })?;
            if pattern.ty.width() != width {
                return Ok(false);
            }
            let word::ValueKind::Constant(bits) = &pattern.kind else {
                return Ok(false);
            };
            if bits
                .as_slice()
                .iter()
                .any(|bit| matches!(bit, BitVal::X | BitVal::Z))
                || !patterns.insert(bits.clone())
            {
                return Ok(false);
            }
        }
        Ok(patterns.len() == domain_size)
    }

    pub(super) fn coalesce_choices(
        &mut self,
        choices: Vec<DecisionChoice>,
    ) -> Result<Vec<DecisionChoice>, crate::SynthError> {
        let mut by_target = BTreeMap::<proc::BlockId, usize>::new();
        let mut canonical = Vec::<DecisionChoice>::new();
        for mut choice in choices {
            let target = self.cfg.edge_target(choice.edges[0])?;
            if let Some(&index) = by_target.get(&target) {
                let predicate = self.or(canonical[index].predicate, choice.predicate)?;
                canonical[index].predicate = predicate;
                canonical[index].edges.append(&mut choice.edges);
            } else {
                by_target.insert(target, canonical.len());
                canonical.push(choice);
            }
        }
        Ok(canonical)
    }

    pub(super) fn set_edge_guard(
        &mut self,
        edge: proc::EdgeId,
        guard: Predicate,
    ) -> Result<(), crate::SynthError> {
        let slot = self.edge_guards.get_mut(edge.index()).ok_or_else(|| {
            crate::SynthError::invariant(format!("unknown procedural edge {edge:?}"))
        })?;
        if slot.replace(guard).is_some() {
            return Err(crate::SynthError::invariant(
                "procedural edge guard was materialized twice",
            ));
        }
        Ok(())
    }

    pub(super) fn edge_guard(&self, edge: proc::EdgeId) -> Result<Predicate, crate::SynthError> {
        self.edge_guards
            .get(edge.index())
            .copied()
            .flatten()
            .ok_or_else(|| {
                crate::SynthError::invariant(
                    "topological predecessor guard has not been materialized",
                )
            })
    }

    pub(super) fn output(&self, block: proc::BlockId) -> Result<BlockOutput, crate::SynthError> {
        self.outputs
            .get(block.index())
            .copied()
            .flatten()
            .ok_or_else(|| {
                crate::SynthError::invariant("topological predecessor output is unavailable")
            })
    }

    pub(super) fn edge_source(
        &self,
        edge: proc::EdgeId,
    ) -> Result<proc::BlockId, crate::SynthError> {
        self.cfg.edge_source(edge)
    }

    pub(super) fn value(&self, value: word::ValueId) -> Result<&word::Value, crate::SynthError> {
        self.module
            .value(value)
            .ok_or_else(|| crate::SynthError::invariant("value is not in the module arena"))
    }
}
