// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    Assignment, ExecutionState, MaterializedPredicate, Predicate, PredicateArena,
    ProcedureNormalizer, Slot, events, normalized_enable, predicate_enable, proc,
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
            self.module
                .connect(assignment.target, assignment.value, assignment.source)
                .map_err(crate::SynthError::from)?;
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
            self.module
                .add_memory_write_port(word::MemoryWritePort {
                    memory: write.memory,
                    address: write.address,
                    data: write.data,
                    clock: word::MemoryClock {
                        value: clock_value,
                        edge: clock.edge,
                    },
                    enable: predicate_enable(write.guard),
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
