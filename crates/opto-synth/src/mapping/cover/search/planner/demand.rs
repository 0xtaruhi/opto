// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{CoverPlanner, LiteralDependencies, MappingCost, SlotChoice, opposite};

#[derive(Debug)]
pub(crate) struct CoverDemand {
    references: Box<[u32]>,
    order: Box<[usize]>,
}

impl CoverDemand {
    pub(crate) fn empty(slot_count: usize) -> Self {
        Self {
            references: vec![0; slot_count].into_boxed_slice(),
            order: Box::new([]),
        }
    }

    pub(crate) fn build(
        slot_count: usize,
        outputs: &[usize],
        mut dependencies: impl FnMut(usize) -> Option<LiteralDependencies>,
    ) -> Result<Option<Self>, crate::SynthError> {
        let mut references = vec![0u32; slot_count];
        let mut state = vec![0u8; slot_count];
        let mut order = Vec::new();
        let mut stack = Vec::<(usize, bool)>::new();
        for &output in outputs {
            increment(&mut references, output)?;
            let output_state = state.get(output).copied().ok_or_else(|| {
                crate::SynthError::invariant("cover output slot is outside the planner arena")
            })?;
            if output_state == 2 {
                continue;
            }
            stack.push((output, false));
            while let Some((slot, expanded)) = stack.pop() {
                let slot_state = state.get(slot).copied().ok_or_else(|| {
                    crate::SynthError::invariant("cover dependency is outside the planner arena")
                })?;
                match (slot_state, expanded) {
                    (2, _) => {}
                    (1, false) => {
                        return Err(crate::SynthError::invariant(
                            "selected cover contains a dependency cycle",
                        ));
                    }
                    (_, true) => {
                        state[slot] = 2;
                        order.push(slot);
                    }
                    (0, false) => {
                        state[slot] = 1;
                        stack.push((slot, true));
                        let Some(dependencies) = dependencies(slot) else {
                            return Ok(None);
                        };
                        for dependency in dependencies.into_iter().rev() {
                            increment(&mut references, dependency)?;
                            match state.get(dependency).copied().ok_or_else(|| {
                                crate::SynthError::invariant(
                                    "cover dependency is outside the planner arena",
                                )
                            })? {
                                0 => stack.push((dependency, false)),
                                1 => {
                                    return Err(crate::SynthError::invariant(
                                        "selected cover contains a dependency cycle",
                                    ));
                                }
                                2 => {}
                                _ => unreachable!("cover demand state is bounded"),
                            }
                        }
                    }
                    _ => unreachable!("cover demand state is bounded"),
                }
            }
        }
        Ok(Some(Self {
            references: references.into_boxed_slice(),
            order: order.into_boxed_slice(),
        }))
    }

    pub(crate) fn reference_count(&self, slot: usize) -> u32 {
        self.references[slot]
    }

    pub(crate) fn references(&self) -> &[u32] {
        &self.references
    }

    pub(crate) fn order(&self) -> &[usize] {
        &self.order
    }

    pub(crate) fn change_by(
        &mut self,
        slot: usize,
        delta: i32,
        count: u32,
    ) -> Result<bool, crate::SynthError> {
        let reference = self.references.get_mut(slot).ok_or_else(|| {
            crate::SynthError::invariant("cover reference is outside the planner arena")
        })?;
        match delta {
            1 => {
                let was_zero = *reference == 0;
                *reference = reference.checked_add(count).ok_or_else(|| {
                    crate::SynthError::capacity("cover reference count exceeds 32-bit capacity")
                })?;
                Ok(was_zero && count != 0)
            }
            -1 => {
                *reference = reference.checked_sub(count).ok_or_else(|| {
                    crate::SynthError::invariant(
                        "cover reference count is smaller than the batched decrement",
                    )
                })?;
                Ok(*reference == 0 && count != 0)
            }
            _ => Err(crate::SynthError::invariant(
                "cover reference delta must be +1 or -1",
            )),
        }
    }
}

fn contains_dependency_cycle(
    slot_count: usize,
    roots: &[usize],
    mut dependencies: impl FnMut(usize) -> Option<LiteralDependencies>,
) -> bool {
    let mut state = vec![0u8; slot_count];
    let mut stack = Vec::new();
    for &root in roots {
        if state.get(root).copied().unwrap_or(2) == 2 {
            continue;
        }
        stack.push((root, false));
        while let Some((slot, expanded)) = stack.pop() {
            let Some(&slot_state) = state.get(slot) else {
                continue;
            };
            match (slot_state, expanded) {
                (2, _) => {}
                (1, false) => return true,
                (_, true) => state[slot] = 2,
                (0, false) => {
                    state[slot] = 1;
                    stack.push((slot, true));
                    let Some(dependencies) = dependencies(slot) else {
                        continue;
                    };
                    for dependency in dependencies.into_iter().rev() {
                        match state.get(dependency).copied() {
                            Some(0) => stack.push((dependency, false)),
                            Some(1) => return true,
                            _ => {}
                        }
                    }
                }
                _ => unreachable!("cover cycle state is bounded"),
            }
        }
    }
    false
}

fn increment(references: &mut [u32], slot: usize) -> Result<bool, crate::SynthError> {
    let reference = references.get_mut(slot).ok_or_else(|| {
        crate::SynthError::invariant("cover reference is outside the planner arena")
    })?;
    *reference = reference.checked_add(1).ok_or_else(|| {
        crate::SynthError::capacity("cover reference count exceeds 32-bit capacity")
    })?;
    Ok(*reference == 1)
}

impl CoverPlanner<'_> {
    pub(in crate::mapping::cover::search) fn selected_cover_has_cycle(
        &self,
        output_slots: &[usize],
    ) -> bool {
        contains_dependency_cycle(self.choices.len(), output_slots, |slot| {
            self.choices
                .get(slot)
                .copied()
                .flatten()
                .map(|choice| self.choice_dependencies(slot, choice))
        })
    }

    pub(crate) fn candidate_dependencies(
        &self,
        slot: usize,
        candidate: usize,
    ) -> impl Iterator<Item = usize> + '_ {
        self.candidate_dependencies
            .row(self.candidates.dependency_row(slot, candidate))
            .iter()
            .map(|&dependency| dependency as usize)
    }

    pub(crate) fn choice_dependencies(
        &self,
        slot: usize,
        choice: SlotChoice,
    ) -> LiteralDependencies {
        match choice {
            SlotChoice::Constant(_) | SlotChoice::Boundary(_) => LiteralDependencies::new(),
            SlotChoice::Inverter => LiteralDependencies::from_slice(&[opposite(slot)]),
            SlotChoice::Cell(candidate) => self
                .candidate_dependencies(slot, candidate as usize)
                .collect(),
            SlotChoice::JointOutput(joint) => {
                LiteralDependencies::from_slice(&[self.base_slots + joint as usize])
            }
            SlotChoice::JointCell(joint) => self.joints[joint as usize].leaf_slots().collect(),
        }
    }

    pub(crate) fn visit_choice_dependencies(
        &self,
        slot: usize,
        choice: SlotChoice,
        mut visit: impl FnMut(usize) -> Result<(), crate::SynthError>,
    ) -> Result<(), crate::SynthError> {
        match choice {
            SlotChoice::Constant(_) | SlotChoice::Boundary(_) => {}
            SlotChoice::Inverter => visit(opposite(slot))?,
            SlotChoice::Cell(candidate) => {
                for dependency in self.candidate_dependencies(slot, candidate as usize) {
                    visit(dependency)?;
                }
            }
            SlotChoice::JointOutput(joint) => visit(self.base_slots + joint as usize)?,
            SlotChoice::JointCell(joint) => {
                for dependency in self.joints[joint as usize].leaf_slots() {
                    visit(dependency)?;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn literal_access_cost(&self, slot: usize) -> MappingCost {
        let flow = self.flows[slot];
        MappingCost {
            area: flow.area / self.reference_estimates[slot].max(1.0),
            ..flow
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_shared_reference_counts_and_topological_order() {
        let dependencies = [
            LiteralDependencies::new(),
            LiteralDependencies::from_slice(&[0]),
            LiteralDependencies::from_slice(&[0]),
            LiteralDependencies::from_slice(&[1, 2]),
        ];
        let demand = CoverDemand::build(dependencies.len(), &[3], |slot| {
            Some(dependencies[slot].clone())
        })
        .unwrap()
        .unwrap();

        assert_eq!(demand.references(), [2, 1, 1, 1]);
        for (position, &slot) in demand.order().iter().enumerate() {
            for dependency in &dependencies[slot] {
                assert!(
                    demand.order()[..position].contains(dependency),
                    "dependency {dependency} must precede slot {slot}"
                );
            }
        }
    }

    #[test]
    fn rejects_cycles() {
        let dependencies = [
            LiteralDependencies::from_slice(&[1]),
            LiteralDependencies::from_slice(&[0]),
        ];
        assert!(
            CoverDemand::build(dependencies.len(), &[0], |slot| {
                Some(dependencies[slot].clone())
            })
            .is_err()
        );
    }

    #[test]
    fn applies_shared_reference_changes_atomically() {
        let mut demand = CoverDemand::empty(1);
        assert!(demand.change_by(0, 1, 2).unwrap());
        assert_eq!(demand.reference_count(0), 2);
        assert!(demand.change_by(0, -1, 2).unwrap());
        assert_eq!(demand.reference_count(0), 0);
        assert!(demand.change_by(0, -1, 1).is_err());
    }

    #[test]
    fn detects_a_cycle_inside_the_selected_root_cone() {
        let selected = [
            Some(LiteralDependencies::new()),
            Some(LiteralDependencies::from_slice(&[2])),
            Some(LiteralDependencies::from_slice(&[1])),
        ];
        assert!(contains_dependency_cycle(3, &[1], |slot| selected[slot].clone()));
        assert!(!contains_dependency_cycle(3, &[0], |slot| selected[slot].clone()));
    }
}
