// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::ReferenceScratch;
use super::{
    Candidate, CellBinding, CoverDemand, CoverPlanner, ExactChoice, ExactViability,
    ExecutionContext, KCut, LibraryCover, LibraryCoverBinding, LibraryCoverCell,
    LibraryCoverSource, LogicNodeId, SlotChoice, inverter_truth, opposite, slot_node,
    tighten_required_arrival,
};

impl CoverPlanner<'_> {
    pub(in crate::mapping::cover::search) fn select(
        &mut self,
        output_slots: &[usize],
    ) -> Result<bool, crate::SynthError> {
        let Some(demand) = CoverDemand::build(self.choices.len(), output_slots, |slot| {
            let choice = self.choices.get(slot).copied().flatten()?;
            Some(self.choice_dependencies(slot, choice))
        })?
        else {
            return Ok(false);
        };
        self.demand = demand;
        Ok(true)
    }

    pub(crate) fn candidate_cut(&self, slot_id: usize, candidate: &Candidate) -> KCut {
        let node = LogicNodeId::from_index(slot_id / 2);
        self.cuts.cuts(node)[candidate.cut as usize]
    }

    pub(in crate::mapping::cover::search) fn update_reference_estimates(&mut self) {
        for (slot_id, &references) in self.demand.references().iter().enumerate() {
            if references > 0 {
                self.reference_estimates[slot_id] = f64::from(references);
            }
        }
    }

    pub(in crate::mapping::cover::search) fn update_required_arrivals(
        &mut self,
        output_slots: &[usize],
        required_times: &[Option<f64>],
    ) -> Result<(), crate::SynthError> {
        self.required_arrivals.fill(f64::INFINITY);
        let mut pending = Vec::new();
        for (&output, required_time) in output_slots.iter().zip(required_times) {
            let Some(required_time) = *required_time else {
                continue;
            };
            if required_time < self.required_arrivals[output] {
                self.required_arrivals[output] = required_time;
                pending.push(output);
            }
        }
        while let Some(current) = pending.pop() {
            let choice = self.choices[current].ok_or_else(|| {
                crate::SynthError::invariant(
                    "required-time propagation reached an unselected cover slot",
                )
            })?;
            let required = self.required_arrivals[current];
            let delay = match choice {
                SlotChoice::Constant(_) | SlotChoice::Boundary(_) | SlotChoice::JointOutput(_) => {
                    0.0
                }
                SlotChoice::Inverter => {
                    let inverter = self.inverter.ok_or_else(|| {
                        crate::SynthError::invariant("selected inverter has no library binding")
                    })?;
                    self.inverter_electrical_cost(current, inverter).delay
                }
                SlotChoice::Cell(candidate) => {
                    self.candidate_electrical_cost(
                        current,
                        self.candidates[current][candidate as usize],
                    )
                    .delay
                }
                SlotChoice::JointCell(joint) => self.joint_electrical_cost(joint).delay,
            };
            for dependency in self.choice_dependencies(current, choice) {
                tighten_required_arrival(
                    &mut self.required_arrivals,
                    &mut pending,
                    dependency,
                    required - delay,
                );
            }
        }
        Ok(())
    }

    pub(in crate::mapping::cover::search) fn update_load_estimates(
        &mut self,
    ) -> Result<(), crate::SynthError> {
        self.load_estimates.clone_from(&self.endpoint_loads);
        for position in 0..self.demand.order().len() {
            let slot_id = self.demand.order()[position];
            let choice = self.choices[slot_id].ok_or_else(|| {
                crate::SynthError::invariant("load propagation reached an unselected cover slot")
            })?;
            let dependencies = self.choice_dependencies(slot_id, choice);
            match choice {
                SlotChoice::Constant(_) | SlotChoice::Boundary(_) | SlotChoice::JointOutput(_) => {}
                SlotChoice::Inverter => {
                    let inverter = self.inverter.ok_or_else(|| {
                        crate::SynthError::invariant("selected inverter has no library binding")
                    })?;
                    self.add_binding_loads(inverter.binding, &dependencies);
                }
                SlotChoice::Cell(candidate_index) => {
                    let candidate = self.candidates[slot_id][candidate_index as usize];
                    let binding = candidate.cell_binding(self.catalog);
                    self.add_binding_loads(binding, &dependencies);
                }
                SlotChoice::JointCell(joint_id) => {
                    let joint = &self.joints[joint_id as usize];
                    for (signature, load) in self.catalog.joint_input_loads(joint.binding) {
                        if let Some(&source) = dependencies.get(signature)
                            && load.is_finite()
                            && load >= 0.0
                        {
                            self.load_estimates[source] += load;
                        }
                    }
                }
            }
        }
        self.loads_ready = true;
        Ok(())
    }

    fn add_binding_loads(&mut self, binding: CellBinding, sources: &[usize]) {
        for (signature, load) in self.catalog.binding_input_loads(binding) {
            if let Some(&source) = sources.get(signature)
                && load.is_finite()
                && load >= 0.0
            {
                self.load_estimates[source] += load;
            }
        }
    }

    fn recovery_meets_required(&self, slot_id: usize, arrival: f64) -> bool {
        let required = self.required_arrivals[slot_id];
        !required.is_finite() || arrival <= required.next_up()
    }

    pub(in crate::mapping::cover::search) fn exact_pass(
        &mut self,
        runtime: &ExecutionContext,
    ) -> Result<usize, crate::SynthError> {
        // Joint cells occupy virtual slots and have no direct candidate arena.
        let viability = runtime.analyze_indexed(self.base_slots, |slot_id| {
            Ok::<_, crate::SynthError>(self.exact_slot_viability(slot_id))
        })?;
        let mut stack = Vec::new();
        let mut removed = Vec::new();
        let mut activated = Vec::new();
        let mut was_removed = vec![false; self.choices.len()];
        let mut pending = (0..self.base_slots).collect::<Vec<_>>();
        let mut queued = vec![false; self.base_slots];
        for &slot in &pending {
            queued[slot] = true;
        }
        let mut changes = 0usize;
        // Reverse topology propagates changed sharing costs through one pass.
        while let Some(slot_id) = pending.pop() {
            queued[slot_id] = false;
            if self.demand.reference_count(slot_id) == 0 {
                continue;
            }
            let Some(current) = self.choices[slot_id] else {
                continue;
            };
            if !matches!(
                current,
                SlotChoice::Cell(_) | SlotChoice::Inverter | SlotChoice::JointOutput(_)
            ) {
                continue;
            }
            let recomputed;
            let slot_viability = if viability[slot_id].active {
                &viability[slot_id]
            } else {
                recomputed = self.exact_slot_viability(slot_id);
                &recomputed
            };
            removed.clear();
            self.change_choice_references_tracked(
                slot_id,
                current,
                -1,
                &mut stack,
                Some(&mut removed),
            )?;
            for &removed_slot in &removed {
                was_removed[removed_slot] = true;
            }
            let timing_driven = self.required_arrivals[slot_id].is_finite();
            let mut best: Option<ExactChoice> = None;
            for candidate_index in 0..self.candidates[slot_id].len() {
                if !slot_viability.candidates[candidate_index] {
                    continue;
                }
                let candidate = self.candidates[slot_id][candidate_index];
                let choice = SlotChoice::Cell(u32::try_from(candidate_index).map_err(|_| {
                    crate::SynthError::capacity("cover candidate count exceeds compact capacity")
                })?);
                let added = self.trial_choice_area(slot_id, choice)?;
                let exact = ExactChoice {
                    choice,
                    area: added + candidate.nominal_cost(self.catalog).area,
                    arrival: self.candidate_arrival_estimate(slot_id, candidate),
                    truth: candidate.truth(),
                    order: (candidate.cut, candidate.inversions, 0),
                };
                if best
                    .as_ref()
                    .is_none_or(|best| exact.prefers_over(best, timing_driven))
                {
                    best = Some(exact);
                }
            }
            if let Some(inverter) = self.inverter {
                let other = opposite(slot_id);
                if self.choices[other].is_some_and(|choice| choice != SlotChoice::Inverter)
                    && slot_viability.inverter
                {
                    let added = self.trial_choice_area(slot_id, SlotChoice::Inverter)?;
                    let exact = ExactChoice {
                        choice: SlotChoice::Inverter,
                        area: added + inverter.cost.area,
                        arrival: self.flows[other].electrical_delay
                            + self.inverter_electrical_cost(slot_id, inverter).delay,
                        truth: inverter_truth(),
                        order: (u8::MAX, u8::MAX, 0),
                    };
                    if best
                        .as_ref()
                        .is_none_or(|best| exact.prefers_over(best, timing_driven))
                    {
                        best = Some(exact);
                    }
                }
            }
            for joint_index in 0..self.slot_joints[slot_id].len() {
                let joint_id = self.slot_joints[slot_id][joint_index];
                let virtual_slot = self.base_slots + joint_id as usize;
                if self.choices[virtual_slot].is_none() {
                    continue;
                }
                if !slot_viability.joints[joint_index] {
                    continue;
                }
                let choice = SlotChoice::JointOutput(joint_id);
                let added = self.trial_choice_area(slot_id, choice)?;
                let joint = &self.joints[joint_id as usize];
                let side = u8::from(joint.slots[1] == slot_id);
                let exact = ExactChoice {
                    choice,
                    area: added,
                    arrival: self.joint_arrival_estimate(joint_id),
                    truth: joint.truths[usize::from(side)],
                    order: (u8::MAX - 1, side, joint_id),
                };
                if best
                    .as_ref()
                    .is_none_or(|best| exact.prefers_over(best, timing_driven))
                {
                    best = Some(exact);
                }
            }
            let restored = best.map_or(current, |best| best.choice);
            activated.clear();
            // Dependencies activated below must observe the committed choice.
            self.choices[slot_id] = Some(restored);
            self.change_choice_references_tracked(
                slot_id,
                restored,
                1,
                &mut stack,
                Some(&mut activated),
            )?;
            if restored != current {
                changes += 1;
                // Revisit suffix slots reactivated by the committed cover.
                activated.sort_unstable();
                for &activated_slot in &activated {
                    if activated_slot < self.base_slots
                        && !was_removed[activated_slot]
                        && !queued[activated_slot]
                    {
                        pending.push(activated_slot);
                        queued[activated_slot] = true;
                    }
                }
            }
            for &removed_slot in &removed {
                was_removed[removed_slot] = false;
            }
        }
        Ok(changes)
    }

    fn exact_slot_viability(&self, slot_id: usize) -> ExactViability {
        let active = self.demand.reference_count(slot_id) != 0
            && self.choices[slot_id].is_some_and(|choice| {
                matches!(
                    choice,
                    SlotChoice::Cell(_) | SlotChoice::Inverter | SlotChoice::JointOutput(_)
                )
            });
        if !active {
            return ExactViability::default();
        }
        let candidates = self.candidates[slot_id]
            .iter()
            .map(|&candidate| {
                for leaf_slot in self.candidate_dependencies(slot_id, candidate) {
                    if self.choices[leaf_slot].is_none() {
                        return false;
                    }
                }
                self.recovery_meets_required(
                    slot_id,
                    self.candidate_arrival_estimate(slot_id, candidate),
                )
            })
            .collect();
        let inverter = self.inverter.is_some_and(|inverter| {
            let other = opposite(slot_id);
            self.recovery_meets_required(
                slot_id,
                self.flows[other].electrical_delay
                    + self.inverter_electrical_cost(slot_id, inverter).delay,
            )
        });
        let joints = self.slot_joints[slot_id]
            .iter()
            .map(|&joint_id| {
                self.recovery_meets_required(slot_id, self.joint_arrival_estimate(joint_id))
            })
            .collect();
        ExactViability {
            active,
            candidates,
            inverter,
            joints,
        }
    }

    fn candidate_arrival_estimate(&self, slot_id: usize, candidate: Candidate) -> f64 {
        let leaf_arrival = self
            .candidate_dependencies(slot_id, candidate)
            .into_iter()
            .map(|leaf_slot| self.flows[leaf_slot].electrical_delay)
            .fold(0.0f64, f64::max);
        leaf_arrival + self.candidate_electrical_cost(slot_id, candidate).delay
    }

    pub(in crate::mapping::cover::search) fn selected_area(&self) -> f64 {
        let mut area = 0.0;
        for (slot_id, &references) in self.demand.references().iter().enumerate() {
            if references == 0 {
                continue;
            }
            let choice =
                self.choices[slot_id].expect("active cover slot has a selected implementation");
            area += self.choice_cell_area(slot_id, choice);
        }
        area
    }

    pub(in crate::mapping::cover::search) fn joint_pass(
        &mut self,
    ) -> Result<usize, crate::SynthError> {
        let mut stack = Vec::new();
        let mut changes = 0usize;
        loop {
            let mut best = None::<(u32, f64)>;
            let joint_count = u32::try_from(self.joints.len())
                .map_err(|_| crate::SynthError::capacity("joint cover candidate count"))?;
            for joint_id in 0..joint_count {
                let Some(gain) = self.joint_gain(joint_id, &mut stack)? else {
                    continue;
                };
                if best.is_none_or(|(best_id, best_gain)| {
                    gain.total_cmp(&best_gain)
                        .then_with(|| best_id.cmp(&joint_id))
                        .is_gt()
                }) {
                    best = Some((joint_id, gain));
                }
            }
            let Some((joint_id, _)) = best else {
                break;
            };
            let virtual_slot = self.base_slots + joint_id as usize;
            if self.choices[virtual_slot].is_none() {
                continue;
            }
            let [first, second] = self.joints[joint_id as usize].slots;
            if self.demand.reference_count(first) == 0 || self.demand.reference_count(second) == 0 {
                continue;
            }
            let joint_choice = SlotChoice::JointOutput(joint_id);
            let (Some(first_current), Some(second_current)) =
                (self.choices[first], self.choices[second])
            else {
                continue;
            };
            if first_current == joint_choice && second_current == joint_choice {
                continue;
            }
            let rewritable = |choice: SlotChoice| {
                matches!(
                    choice,
                    SlotChoice::Cell(_) | SlotChoice::Inverter | SlotChoice::JointOutput(_)
                )
            };
            if !rewritable(first_current) || !rewritable(second_current) {
                continue;
            }
            let joint_arrival = self.joint_arrival_estimate(joint_id);
            if !self.recovery_meets_required(first, joint_arrival)
                || !self.recovery_meets_required(second, joint_arrival)
            {
                continue;
            }
            let (timing_driven, current_meets_timing, current_arrival) =
                self.joint_current_arrival(first, first_current, second, second_current)?;
            let freed = self.choice_cell_area(first, first_current)
                + self.choice_cell_area(second, second_current)
                + self
                    .change_choices_references(
                        &[(first, first_current), (second, second_current)],
                        -1,
                        &mut stack,
                        None,
                    )
                    .map_err(|error| {
                        crate::SynthError::invariant(format!(
                            "joint {joint_id} failed to remove current choices: {error}"
                        ))
                    })?;
            // Activated dependencies must observe both trial choices.
            self.choices[first] = Some(joint_choice);
            self.choices[second] = Some(joint_choice);
            let added = self.change_choices_references(
                &[(first, joint_choice), (second, joint_choice)],
                1,
                &mut stack,
                None,
            )?;
            let take = super::super::joint_replacement_is_preferred(
                timing_driven,
                !current_meets_timing,
                added,
                joint_arrival,
                freed,
                current_arrival,
            );
            crate::api::diagnostics::trace!(
                crate::api::diagnostics::SynthTrace::new(self.catalog.diagnostics().joint_cells),
                "cover.joint_pass",
                "joint={joint_id} cut_len={} freed={freed:.3} added={added:.3} take={}",
                self.joints[joint_id as usize].cut.len(),
                take
            );
            if take {
                changes += 1;
            } else {
                self.change_choices_references(
                    &[(first, joint_choice), (second, joint_choice)],
                    -1,
                    &mut stack,
                    None,
                )?;
                self.choices[first] = Some(first_current);
                self.choices[second] = Some(second_current);
                self.change_choices_references(
                    &[(first, first_current), (second, second_current)],
                    1,
                    &mut stack,
                    None,
                )?;
            }
        }
        Ok(changes)
    }

    fn joint_gain(
        &mut self,
        joint_id: u32,
        stack: &mut Vec<usize>,
    ) -> Result<Option<f64>, crate::SynthError> {
        let virtual_slot = self.base_slots + joint_id as usize;
        if self.choices[virtual_slot].is_none() {
            return Ok(None);
        }
        let [first, second] = self.joints[joint_id as usize].slots;
        if self.demand.reference_count(first) == 0 || self.demand.reference_count(second) == 0 {
            return Ok(None);
        }
        let (Some(first_current), Some(second_current)) =
            (self.choices[first], self.choices[second])
        else {
            return Ok(None);
        };
        let joint_choice = SlotChoice::JointOutput(joint_id);
        if first_current == joint_choice && second_current == joint_choice {
            return Ok(None);
        }
        let rewritable = |choice: SlotChoice| {
            matches!(
                choice,
                SlotChoice::Cell(_) | SlotChoice::Inverter | SlotChoice::JointOutput(_)
            )
        };
        if !rewritable(first_current) || !rewritable(second_current) {
            return Ok(None);
        }
        let joint_arrival = self.joint_arrival_estimate(joint_id);
        if !self.recovery_meets_required(first, joint_arrival)
            || !self.recovery_meets_required(second, joint_arrival)
        {
            return Ok(None);
        }
        let (timing_driven, current_meets_timing, current_arrival) =
            self.joint_current_arrival(first, first_current, second, second_current)?;
        let freed = self.choice_cell_area(first, first_current)
            + self.choice_cell_area(second, second_current)
            + self.change_choices_references(
                &[(first, first_current), (second, second_current)],
                -1,
                stack,
                None,
            )?;
        self.choices[first] = Some(joint_choice);
        self.choices[second] = Some(joint_choice);
        let added = self.change_choices_references(
            &[(first, joint_choice), (second, joint_choice)],
            1,
            stack,
            None,
        )?;
        self.change_choices_references(
            &[(first, joint_choice), (second, joint_choice)],
            -1,
            stack,
            None,
        )?;
        self.choices[first] = Some(first_current);
        self.choices[second] = Some(second_current);
        self.change_choices_references(
            &[(first, first_current), (second, second_current)],
            1,
            stack,
            None,
        )?;
        Ok(super::super::joint_replacement_is_preferred(
            timing_driven,
            !current_meets_timing,
            added,
            joint_arrival,
            freed,
            current_arrival,
        )
        .then_some(freed - added))
    }

    fn joint_current_arrival(
        &self,
        first: usize,
        first_choice: SlotChoice,
        second: usize,
        second_choice: SlotChoice,
    ) -> Result<(bool, bool, f64), crate::SynthError> {
        let mut timing_driven = false;
        let mut meets_timing = true;
        let mut arrival = 0.0f64;
        for (slot, choice) in [(first, first_choice), (second, second_choice)] {
            let selected_arrival = self.selected_choice_arrival(slot, choice)?;
            timing_driven |= self.required_arrivals[slot].is_finite();
            meets_timing &= self.recovery_meets_required(slot, selected_arrival);
            arrival = arrival.max(selected_arrival);
        }
        Ok((timing_driven, meets_timing, arrival))
    }

    fn selected_choice_arrival(
        &self,
        slot_id: usize,
        choice: SlotChoice,
    ) -> Result<f64, crate::SynthError> {
        match choice {
            SlotChoice::Cell(candidate) => {
                let candidate = self.candidates[slot_id]
                    .get(candidate as usize)
                    .copied()
                    .ok_or_else(|| {
                        crate::SynthError::invariant(
                            "selected recovery candidate is outside its slot",
                        )
                    })?;
                Ok(self.candidate_arrival_estimate(slot_id, candidate))
            }
            SlotChoice::Inverter => {
                let inverter = self.inverter.ok_or_else(|| {
                    crate::SynthError::invariant("selected inverter has no library binding")
                })?;
                Ok(self.flows[opposite(slot_id)].electrical_delay
                    + self.inverter_electrical_cost(slot_id, inverter).delay)
            }
            SlotChoice::JointOutput(joint_id) => Ok(self.joint_arrival_estimate(joint_id)),
            SlotChoice::Constant(_) | SlotChoice::Boundary(_) | SlotChoice::JointCell(_) => {
                Err(crate::SynthError::invariant(
                    "joint recovery compared a non-rewritable selected choice",
                ))
            }
        }
    }

    fn joint_arrival_estimate(&self, joint_id: u32) -> f64 {
        let joint = &self.joints[joint_id as usize];
        let mut arrival = 0.0f64;
        for leaf_slot in joint.leaf_slots() {
            arrival = arrival.max(self.flows[leaf_slot].electrical_delay);
        }
        arrival + self.joint_electrical_cost(joint_id).delay
    }

    fn choice_cell_area(&self, slot_id: usize, choice: SlotChoice) -> f64 {
        match choice {
            SlotChoice::Constant(_) | SlotChoice::Boundary(_) | SlotChoice::JointOutput(_) => 0.0,
            SlotChoice::Inverter => {
                self.inverter
                    .expect("selected inverter has a library binding")
                    .cost
                    .area
            }
            SlotChoice::Cell(candidate) => {
                self.candidates[slot_id][candidate as usize]
                    .nominal_cost(self.catalog)
                    .area
            }
            SlotChoice::JointCell(joint) => self.joints[joint as usize].cost.area,
        }
    }

    /// Scores a choice using committed reference counts without mutating them.
    fn trial_choice_area(
        &mut self,
        slot_id: usize,
        choice: SlotChoice,
    ) -> Result<f64, crate::SynthError> {
        if self.demand.reference_count(slot_id) == 0 {
            return Ok(0.0);
        }
        #[cfg(test)]
        let references_before = self.demand.references().to_vec();
        let mut scratch = std::mem::take(&mut self.trial_scratch);
        scratch.begin(self.choices.len());
        let mut frontier = std::mem::take(&mut scratch.frontier);
        let mut next = std::mem::take(&mut scratch.next);
        frontier.extend(self.choice_dependencies(slot_id, choice));
        let mut area = 0.0;
        let mut error = None;
        while !frontier.is_empty() && error.is_none() {
            frontier.sort_unstable();
            next.clear();
            for &current in &frontier {
                if !scratch.mark(current) || self.demand.reference_count(current) != 0 {
                    continue;
                }
                let Some(selected) = self.choices[current] else {
                    error = Some(crate::SynthError::invariant(
                        "cover recovery scored a slot without an implementation",
                    ));
                    break;
                };
                area += self.choice_cell_area(current, selected);
                next.extend(self.choice_dependencies(current, selected));
            }
            std::mem::swap(&mut frontier, &mut next);
        }
        scratch.frontier = frontier;
        scratch.next = next;
        self.trial_scratch = scratch;
        let result = error.map_or(Ok(area), Err);
        #[cfg(test)]
        assert_eq!(self.demand.references(), references_before);
        result
    }

    fn change_choice_references_tracked(
        &mut self,
        slot_id: usize,
        choice: SlotChoice,
        delta: i32,
        stack: &mut Vec<usize>,
        crossed_slots: Option<&mut Vec<usize>>,
    ) -> Result<f64, crate::SynthError> {
        self.change_choices_references(&[(slot_id, choice)], delta, stack, crossed_slots)
    }

    fn change_choices_references(
        &mut self,
        roots: &[(usize, SlotChoice)],
        delta: i32,
        pending: &mut Vec<usize>,
        mut crossed_slots: Option<&mut Vec<usize>>,
    ) -> Result<f64, crate::SynthError> {
        if !matches!(delta, -1 | 1) {
            return Err(crate::SynthError::invariant(
                "cover reference delta must be +1 or -1",
            ));
        }
        debug_assert!(pending.is_empty());
        let mut scratch = std::mem::take(&mut self.reference_scratch);
        let ReferenceScratch { seeded_roots, next } = &mut scratch;
        seeded_roots.clear();
        next.clear();
        for &(slot_id, choice) in roots {
            if self.demand.reference_count(slot_id) != 0 {
                pending.extend(self.choice_dependencies(slot_id, choice));
                seeded_roots.push(slot_id);
            }
        }
        seeded_roots.sort_unstable();
        seeded_roots.dedup();
        let mut area = 0.0;
        while !pending.is_empty() {
            pending.sort_unstable();
            let mut start = 0;
            while start < pending.len() {
                let current = pending[start];
                let end = pending[start..]
                    .iter()
                    .position(|&slot| slot != current)
                    .map_or(pending.len(), |offset| start + offset);
                let count = u32::try_from(end - start).map_err(|_| {
                    crate::SynthError::capacity(
                        "batched cover reference change exceeds 32-bit capacity",
                    )
                })?;
                let crossed = self.demand.change_by(current, delta, count)?;
                if crossed {
                    if let Some(crossed_slots) = crossed_slots.as_mut() {
                        crossed_slots.push(current);
                    }
                    // Do not charge a replaced root reached through another root.
                    if seeded_roots.binary_search(&current).is_err() {
                        let selected = self.choices[current].ok_or_else(|| {
                            crate::SynthError::invariant(
                                "cover recovery activated a slot without an implementation",
                            )
                        })?;
                        area += self.choice_cell_area(current, selected);
                        next.extend(self.choice_dependencies(current, selected));
                    }
                }
                start = end;
            }
            pending.clear();
            pending.append(next);
        }
        self.reference_scratch = scratch;
        Ok(area)
    }

    pub(in crate::mapping::cover::search) fn flatten(
        &self,
        output_slots: &[usize],
    ) -> Result<LibraryCover, crate::SynthError> {
        let mut cells = Vec::new();
        let mut materialized = vec![None; self.choices.len()];
        for &slot_id in self.demand.order() {
            let choice = self.choices[slot_id].ok_or_else(|| {
                crate::SynthError::invariant("active cover slot has no selected implementation")
            })?;
            let dependencies = self.choice_dependencies(slot_id, choice);
            let sources = dependencies
                .iter()
                .map(|&dependency| {
                    materialized
                        .get(dependency)
                        .copied()
                        .flatten()
                        .ok_or_else(|| {
                            crate::SynthError::invariant(
                                "cover dependency was not materialized before its user",
                            )
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let source = match choice {
                SlotChoice::Constant(value) => LibraryCoverSource::Constant(value),
                SlotChoice::Boundary(input) => LibraryCoverSource::Input(input),
                SlotChoice::Inverter => {
                    let inverter = self.inverter.ok_or_else(|| {
                        crate::SynthError::invariant("selected inverter has no library binding")
                    })?;
                    let cell = cells.len();
                    cells.push(LibraryCoverCell {
                        second_node: None,
                        binding: LibraryCoverBinding::Single(inverter.binding),
                        binding_identity: self
                            .catalog
                            .binding_identity(inverter.binding)
                            .into_boxed_slice(),
                        truth: inverter_truth(),
                        second_truth: None,
                        sources: sources.into_boxed_slice(),
                    });
                    LibraryCoverSource::Cell(cell)
                }
                SlotChoice::Cell(candidate) => {
                    let candidate = self.candidates[slot_id][candidate as usize];
                    let binding = candidate.cell_binding(self.catalog);
                    let cell = cells.len();
                    cells.push(LibraryCoverCell {
                        second_node: None,
                        binding: LibraryCoverBinding::Single(binding),
                        binding_identity: self.catalog.binding_identity(binding).into_boxed_slice(),
                        truth: candidate.truth(),
                        second_truth: None,
                        sources: sources.into_boxed_slice(),
                    });
                    LibraryCoverSource::Cell(cell)
                }
                SlotChoice::JointOutput(joint_id) => {
                    let joint = &self.joints[joint_id as usize];
                    let Some(LibraryCoverSource::Cell(cell)) = sources.first().copied() else {
                        return Err(crate::SynthError::invariant(
                            "joint output has no materialized joint cell",
                        ));
                    };
                    if joint.slots[0] == slot_id {
                        LibraryCoverSource::Cell(cell)
                    } else {
                        LibraryCoverSource::CellSecond(cell)
                    }
                }
                SlotChoice::JointCell(joint_id) => {
                    let joint = &self.joints[joint_id as usize];
                    let cell = cells.len();
                    cells.push(LibraryCoverCell {
                        second_node: Some(slot_node(joint.slots[1])),
                        binding: LibraryCoverBinding::Joint(joint.binding),
                        binding_identity: self
                            .catalog
                            .joint_binding_identity(joint.binding)
                            .into_boxed_slice(),
                        truth: joint.truths[0],
                        second_truth: Some(joint.truths[1]),
                        sources: sources.into_boxed_slice(),
                    });
                    LibraryCoverSource::Cell(cell)
                }
            };
            materialized[slot_id] = Some(source);
        }
        let outputs = output_slots
            .iter()
            .map(|&output| {
                materialized.get(output).copied().flatten().ok_or_else(|| {
                    crate::SynthError::invariant("cover output was not materialized")
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let total_area = self.selected_area();
        let output_costs = output_slots
            .iter()
            .map(|&slot| self.flows[slot])
            .collect::<Vec<_>>()
            .into_boxed_slice();
        LibraryCover {
            cells: cells.into_boxed_slice(),
            outputs: outputs.into_boxed_slice(),
            total_area,
            output_costs,
        }
        .normalize(true)
    }
}
