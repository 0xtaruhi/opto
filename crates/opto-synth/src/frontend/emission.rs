// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    Assignment, ExecutionState, MaterializedPredicate, Predicate, PredicateArena,
    ProcedureNormalizer, Slot, derived_source, events, normalized_enable, predicate_enable, proc,
    whole_target_name, word,
};

impl ProcedureNormalizer<'_> {
    pub(super) fn emit(&mut self, final_state: ExecutionState) -> Result<(), crate::SynthError> {
        let mut assignments = Vec::new();
        for index in 0..self.keys.len() {
            let key = self.keys[index];
            let base = self.base_value(key)?;
            let slot = self
                .states
                .get(final_state.scheduled, key)
                .cloned()
                .unwrap_or_else(|| {
                    let source = self
                        .module
                        .value(base)
                        .expect("procedural base value was validated")
                        .source
                        .clone();
                    Slot::unassigned(base, source)
                });
            if slot.coverage == Predicate::Never
                || self
                    .module
                    .signal(key.signal)
                    .is_some_and(|signal| signal.kind == word::SignalKind::ProcessLocal)
            {
                continue;
            }
            let held_events = self.held_events(slot.coverage)?;
            assignments.push(Assignment {
                target: key.target(self.module)?,
                value: slot.update,
                coverage: self.materialize_coverage(slot.coverage)?,
                resets: slot.resets,
                held_events,
                source: slot.source,
            });
        }
        match self.procedure.kind {
            proc::ProcedureKind::Combinational => self.emit_comb(assignments)?,
            proc::ProcedureKind::CombinationalOrLatch => self.emit_latches(assignments, false)?,
            proc::ProcedureKind::Latch => self.emit_latches(assignments, true)?,
            proc::ProcedureKind::FlipFlop => self.emit_flops(assignments)?,
        }
        Ok(())
    }

    fn emit_comb(&mut self, assignments: Vec<Assignment>) -> Result<(), crate::SynthError> {
        for assignment in assignments {
            if !assignment.is_definite() {
                self.incomplete_comb.push(assignment);
                continue;
            }
            let target_name = assignment.target_name(self.module).to_owned();
            self.module
                .connect(assignment.target, assignment.value, assignment.source)
                .map_err(|error| {
                    crate::SynthError::invariant(format!(
                        "failed to commit combinational assignment to '{target_name}': {error}"
                    ))
                })?;
        }
        Ok(())
    }

    fn emit_latches(
        &mut self,
        assignments: Vec<Assignment>,
        require_latch: bool,
    ) -> Result<(), crate::SynthError> {
        if assignments.is_empty() {
            return Err(crate::SynthError::invalid(
                "latch procedure does not assign a persistent target",
            ));
        }
        for assignment in assignments {
            if assignment.is_definite() {
                if require_latch {
                    return Err(crate::SynthError::invalid(format!(
                        "always_latch target '{}' has no hold path",
                        assignment.target_name(self.module)
                    )));
                }
                self.module
                    .connect(assignment.target, assignment.value, assignment.source)
                    .map_err(crate::SynthError::from)?;
                continue;
            }
            let enable = assignment.enable().ok_or_else(|| {
                crate::SynthError::invariant("conditional latch assignment lost its enable")
            })?;
            let enable = normalized_enable(self.module, enable, &assignment.source)?;
            let name = whole_target_name(self.module, &assignment.target);
            let q = self
                .module
                .latch(
                    word::LatchOp {
                        name,
                        d: assignment.value,
                        enable,
                        resets: assignment.resets.into_vec(),
                    },
                    assignment.source.clone(),
                )
                .map_err(crate::SynthError::from)?;
            self.module
                .connect(assignment.target, q, assignment.source)
                .map_err(crate::SynthError::from)?;
        }
        Ok(())
    }

    fn emit_flops(&mut self, mut assignments: Vec<Assignment>) -> Result<(), crate::SynthError> {
        let events = self
            .procedures
            .sensitivity_events(self.procedure_id)
            .ok_or_else(|| {
                crate::SynthError::invariant("flip-flop procedure has no sensitivity-event row")
            })?
            .map(|(_, event)| *event)
            .collect::<Vec<_>>();
        if !self.writes.is_empty() && events.len() != 1 {
            let memory = self.module.memory(self.writes[0].memory).ok_or_else(|| {
                crate::SynthError::invariant("procedural memory write references a missing memory")
            })?;
            return Err(crate::SynthError::unsupported(format!(
                "procedural writes to memory '{}' in a multi-edge always_ff",
                self.module.name_str(memory.name)
            )));
        }
        if let Some(clock) = events::dual_edge_clock(&events) {
            return self.emit_dual_edge_flops(assignments, clock);
        }
        let clock = if assignments.is_empty() {
            *events
                .first()
                .ok_or_else(|| crate::SynthError::invariant("always_ff has no sensitivity event"))?
        } else {
            events::resolve_flop_events(
                self.module,
                &events,
                &mut assignments,
                &self.procedure.source,
            )?
        };
        let clock_value = self
            .module
            .read_signal(clock.signal, self.procedure.source.clone())
            .map_err(crate::SynthError::from)?;
        for assignment in assignments {
            let name = whole_target_name(self.module, &assignment.target);
            let q = self
                .module
                .register(
                    word::RegisterOp {
                        name,
                        d: assignment.value,
                        clock: clock_value,
                        edge: clock.edge,
                        enable: assignment.enable().map(|value| word::Enable {
                            value,
                            active_high: true,
                        }),
                        resets: assignment.resets.into_vec(),
                    },
                    assignment.source.clone(),
                )
                .map_err(crate::SynthError::from)?;
            self.module
                .connect(assignment.target, q, assignment.source)
                .map_err(crate::SynthError::from)?;
        }
        let priority = self
            .module
            .memory_write_ports()
            .iter()
            .map(|port| port.priority)
            .max()
            .map_or(Some(0), |priority| priority.checked_add(1));
        let mut priority = priority.ok_or_else(|| {
            crate::SynthError::capacity("memory write priority space is exhausted")
        })?;
        for write in std::mem::take(&mut self.writes) {
            if write.guard == MaterializedPredicate::Never {
                continue;
            }
            let enable = predicate_enable(self.module, write.guard, &write.source)?;
            self.module
                .add_memory_write_port(word::MemoryWritePort {
                    memory: write.memory,
                    address: write.address,
                    data: write.data,
                    clock: word::MemoryClock {
                        value: clock_value,
                        edge: clock.edge,
                    },
                    enable,
                    mask: write.mask,
                    priority,
                    source: write.source,
                })
                .map_err(crate::SynthError::from)?;
            priority = priority.checked_add(1).ok_or_else(|| {
                crate::SynthError::capacity("memory write priority space is exhausted")
            })?;
        }
        Ok(())
    }

    fn emit_dual_edge_flops(
        &mut self,
        assignments: Vec<Assignment>,
        clock: word::SignalId,
    ) -> Result<(), crate::SynthError> {
        let clock_value = self
            .module
            .read_signal(clock, self.procedure.source.clone())
            .map_err(crate::SynthError::from)?;
        for assignment in assignments {
            if assignment
                .resets
                .iter()
                .any(|reset| reset.kind != word::ResetKind::Sync)
            {
                return Err(crate::SynthError::invariant(
                    "dual-edge state carries an asynchronous reset",
                ));
            }
            let current = self.read_assignment_target(&assignment)?;
            let mut update = match assignment.coverage {
                super::Coverage::Never => continue,
                super::Coverage::Always => assignment.value,
                super::Coverage::When(enable) => self
                    .module
                    .mux(enable, assignment.value, current, assignment.source.clone())
                    .map_err(crate::SynthError::from)?,
            };
            for reset in assignment.resets.iter().rev() {
                let asserted = if reset.active_high {
                    reset.value
                } else {
                    self.module
                        .unary(
                            word::UnaryOp::BitNot,
                            reset.value,
                            assignment.source.clone(),
                        )
                        .map_err(crate::SynthError::from)?
                };
                update = self
                    .module
                    .mux(
                        asserted,
                        reset.reset_value,
                        update,
                        assignment.source.clone(),
                    )
                    .map_err(crate::SynthError::from)?;
            }
            let (rising_signal, rising_name, rising_source) =
                self.add_dual_edge_bank_signal(&assignment, update, word::Edge::Pos)?;
            let rising = self
                .module
                .register(
                    word::RegisterOp {
                        name: Some(rising_name),
                        d: update,
                        clock: clock_value,
                        edge: word::Edge::Pos,
                        enable: None,
                        resets: Vec::new(),
                    },
                    rising_source.clone(),
                )
                .map_err(crate::SynthError::from)?;
            self.module
                .connect(
                    word::LValue::signal(rising_signal),
                    rising,
                    rising_source.clone(),
                )
                .map_err(crate::SynthError::from)?;
            let rising = self
                .module
                .read_signal(rising_signal, rising_source)
                .map_err(crate::SynthError::from)?;
            let (falling_signal, falling_name, falling_source) =
                self.add_dual_edge_bank_signal(&assignment, update, word::Edge::Neg)?;
            let falling = self
                .module
                .register(
                    word::RegisterOp {
                        name: Some(falling_name),
                        d: update,
                        clock: clock_value,
                        edge: word::Edge::Neg,
                        enable: None,
                        resets: Vec::new(),
                    },
                    falling_source.clone(),
                )
                .map_err(crate::SynthError::from)?;
            self.module
                .connect(
                    word::LValue::signal(falling_signal),
                    falling,
                    falling_source.clone(),
                )
                .map_err(crate::SynthError::from)?;
            let falling = self
                .module
                .read_signal(falling_signal, falling_source)
                .map_err(crate::SynthError::from)?;
            let q = self
                .module
                .mux(
                    clock_value,
                    rising,
                    falling,
                    derived_source(&assignment.source, "dual-edge phase selection", b"phase")?,
                )
                .map_err(crate::SynthError::from)?;
            self.module
                .connect(assignment.target, q, assignment.source)
                .map_err(crate::SynthError::from)?;
        }
        Ok(())
    }

    fn add_dual_edge_bank_signal(
        &mut self,
        assignment: &Assignment,
        value: word::ValueId,
        edge: word::Edge,
    ) -> Result<(word::SignalId, opto_ir::NameId, word::SourceSpan), crate::SynthError> {
        let (lsb, width) = assignment.target.range.map_or_else(
            || {
                self.module
                    .signal(assignment.target.signal)
                    .map(|signal| (0, signal.ty.width()))
                    .ok_or_else(|| {
                        crate::SynthError::invariant(
                            "dual-edge assignment target signal disappeared",
                        )
                    })
            },
            |range| Ok((range.lsb.min(range.msb), range.width())),
        )?;
        let edge_name = match edge {
            word::Edge::Pos => "rise",
            word::Edge::Neg => "fall",
        };
        let target_name = self
            .module
            .signal(assignment.target.signal)
            .and_then(|signal| signal.name)
            .map_or_else(
                || "anonymous".to_string(),
                |name| self.module.name_str(name).to_string(),
            );
        let base = format!("$opto$dual_edge${target_name}${lsb}${width}${edge_name}");
        let mut name = base.clone();
        let mut suffix = 0u32;
        while self.module.signal_id(&name).is_some() {
            suffix = suffix.checked_add(1).ok_or_else(|| {
                crate::SynthError::capacity("dual-edge bank name suffix is exhausted")
            })?;
            name = format!("{base}${suffix}");
        }
        let source = derived_source(&assignment.source, "dual-edge bank state", name.as_bytes())?;
        let ty = self
            .module
            .value(value)
            .ok_or_else(|| crate::SynthError::invariant("dual-edge next-state value disappeared"))?
            .ty;
        let signal = self
            .module
            .add_wire(&name, ty, source.clone())
            .map_err(crate::SynthError::from)?;
        let name = self
            .module
            .signal(signal)
            .and_then(|signal| signal.name)
            .ok_or_else(|| crate::SynthError::invariant("dual-edge bank signal has no name"))?;
        Ok((signal, name, source))
    }

    fn read_assignment_target(
        &mut self,
        assignment: &Assignment,
    ) -> Result<word::ValueId, crate::SynthError> {
        if assignment.target.dynamic.is_some() {
            return Err(crate::SynthError::invariant(
                "normalized state assignment retains a dynamic target",
            ));
        }
        match assignment.target.range {
            Some(range) => self
                .module
                .read_signal_slice(
                    assignment.target.signal,
                    range.lsb.min(range.msb),
                    range.width(),
                    assignment.source.clone(),
                )
                .map_err(crate::SynthError::from),
            None => self
                .module
                .read_signal(assignment.target.signal, assignment.source.clone())
                .map_err(crate::SynthError::from),
        }
    }

    pub(super) fn select(
        &mut self,
        condition: Predicate,
        then_value: word::ValueId,
        else_value: word::ValueId,
    ) -> Result<word::ValueId, crate::SynthError> {
        if then_value == else_value {
            return Ok(then_value);
        }
        let source = self.procedure.source.clone();
        match self
            .predicates
            .materialize(self.module, condition, &source)?
        {
            MaterializedPredicate::Never => Ok(else_value),
            MaterializedPredicate::Always => Ok(then_value),
            MaterializedPredicate::Value(condition) => self
                .module
                .mux(condition, then_value, else_value, source)
                .map_err(crate::SynthError::from),
        }
    }

    pub(super) fn not(predicate: Predicate) -> Predicate {
        PredicateArena::not(predicate)
    }

    pub(super) fn and(
        &mut self,
        left: Predicate,
        right: Predicate,
    ) -> Result<Predicate, crate::SynthError> {
        self.predicates.and(left, right)
    }

    pub(super) fn or(
        &mut self,
        left: Predicate,
        right: Predicate,
    ) -> Result<Predicate, crate::SynthError> {
        self.predicates.or(left, right)
    }

    pub(super) fn predicate(
        &mut self,
        value: word::ValueId,
    ) -> Result<Predicate, crate::SynthError> {
        self.predicates.value(self.module, value)
    }
}
