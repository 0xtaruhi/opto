// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    Candidate, CellBinding, CellCost, CombinationalCellCatalog, CompiledMapping, CoverDemand,
    CoverPlanner, CoverTiming, ExecutionContext, FlowChoice, InverterCell, LogicGraph, LogicNode,
    LogicNodeId, MappingCost, SlotChoice, TruthTable, inverter_truth, opposite, slot,
};
use super::{ReferenceScratch, TrialScratch};
use crate::planning::mapping_policy::{
    compare_mapping_cost, compare_mapping_cost_with_required_time,
};

#[derive(Clone, Copy)]
pub(in crate::mapping::cover::search) struct CoverEndpoints<'a> {
    pub(in crate::mapping::cover::search) outputs: &'a [LogicNodeId],
    pub(in crate::mapping::cover::search) timing: CoverTiming<'a>,
}

impl<'a> CoverPlanner<'a> {
    pub(in crate::mapping::cover::search) fn rebuild_nominal_flow(
        &mut self,
        runtime: &ExecutionContext,
    ) -> Result<(), crate::SynthError> {
        self.choices.fill(None);
        self.choice_areas.fill(0.0);
        self.loads_ready = false;
        self.flow_pass(runtime)
    }

    pub(in crate::mapping::cover::search) fn joint_count(&self) -> usize {
        self.joints.len()
    }

    pub(in crate::mapping::cover::search) fn new(
        network: &'a LogicGraph,
        mapping: &'a CompiledMapping,
        catalog: &'a CombinationalCellCatalog,
        endpoints: CoverEndpoints<'_>,
    ) -> Result<Self, crate::SynthError> {
        let CoverEndpoints { outputs, timing } = endpoints;
        let CoverTiming {
            output_loads,
            input_transitions,
            input_arrivals,
            ..
        } = timing;
        let CompiledMapping {
            cuts,
            truths,
            live_nodes,
            candidates,
            candidate_dependencies,
            joints,
            slot_joints,
            joints_by_node,
        } = mapping;
        let node_count = network.node_count();
        if truths.node_count() != node_count {
            return Err(crate::SynthError::invariant(
                "compiled truth rows do not align with the choice graph",
            ));
        }
        let slots = node_count * 2;
        let inverter = catalog
            .best_binding_for_truth(inverter_truth())
            .map(|binding| InverterCell {
                binding,
                cost: catalog.cost_for_binding(binding),
            });
        let mut reference_estimates = vec![0.0f64; slots];
        for (index, &is_live) in live_nodes.iter().enumerate() {
            if !is_live {
                continue;
            }
            let node = LogicNodeId::from_index(index);
            let stored = network.node(node);
            for fanin in stored.fanins() {
                reference_estimates[slot(fanin)] += 1.0;
                reference_estimates[slot(fanin) ^ 1] += 1.0;
            }
        }
        let total = slots + joints.len();
        reference_estimates.resize(total, 1.0);
        if input_arrivals.len() != input_transitions.len() {
            return Err(crate::SynthError::invariant(
                "regional input arrivals and transitions do not align",
            ));
        }
        let mut endpoint_loads = vec![0.0; total];
        for (&output, load) in outputs.iter().zip(output_loads) {
            endpoint_loads[slot(output)] += load.unwrap_or(0.0);
        }
        Ok(Self {
            network,
            cuts,
            catalog,
            inverter,
            candidates,
            candidate_dependencies,
            joints,
            slot_joints,
            joints_by_node,
            base_slots: slots,
            choices: vec![None; total],
            choice_areas: vec![0.0; total],
            flows: vec![MappingCost::zero(); total],
            required_arrivals: vec![f64::INFINITY; total],
            load_estimates: endpoint_loads.clone(),
            endpoint_loads,
            input_transitions: input_transitions
                .iter()
                .map(|transition| transition.unwrap_or(0.0))
                .collect(),
            input_arrivals: input_arrivals
                .iter()
                .map(|arrival| arrival.unwrap_or(0.0))
                .collect(),
            loads_ready: false,
            reference_estimates,
            demand: CoverDemand::empty(total),
            live_nodes: live_nodes.clone(),
            reference_scratch: ReferenceScratch::default(),
            trial_scratch: TrialScratch::default(),
        })
    }

    pub(in crate::mapping::cover::search) fn flow_pass(
        &mut self,
        runtime: &ExecutionContext,
    ) -> Result<(), crate::SynthError> {
        let node_count = self.network.node_count();
        let mut levels = vec![Vec::new(); self.network.max_level() + 1];
        for index in 0..node_count {
            if self.live_nodes[index] {
                levels[self.network.level(LogicNodeId::from_index(index)) as usize].push(index);
            }
        }
        for nodes in levels {
            let ready_joints = nodes
                .iter()
                .filter_map(|&index| self.joints_by_node.get(index))
                .flatten()
                .copied()
                .collect::<Vec<_>>();
            let joint_costs = runtime.analyze_indexed(ready_joints.len(), |position| {
                let joint_id = ready_joints[position];
                Ok::<_, crate::SynthError>((joint_id, self.flow_joint_cost(joint_id)))
            })?;
            for (joint_id, cost) in joint_costs {
                let virtual_slot = self.base_slots + joint_id as usize;
                match cost {
                    Some(cost) => self.set(virtual_slot, SlotChoice::JointCell(joint_id), cost),
                    None => self.set_choice(virtual_slot, None),
                }
            }

            let node_choices = runtime.analyze_indexed(nodes.len(), |position| {
                let index = nodes[position];
                Ok::<_, crate::SynthError>((index, self.flow_node(index)))
            })?;
            for (index, choices) in node_choices {
                for (phase, choice) in choices.into_iter().enumerate() {
                    let slot_id = index * 2 + phase;
                    match choice {
                        Some(choice) => self.set(slot_id, choice.choice, choice.cost),
                        None => self.set_choice(slot_id, None),
                    }
                }
            }
        }
        Ok(())
    }

    fn flow_joint_cost(&self, joint_id: u32) -> Option<MappingCost> {
        let joint = self.joints[joint_id as usize].clone();
        let mut cost = MappingCost::zero();
        for leaf_slot in joint.leaf_slots() {
            self.choices[leaf_slot]?;
            cost = cost.combine(self.literal_access_cost(leaf_slot));
        }
        let signature_transitions = joint
            .leaf_slots()
            .map(|leaf| self.flows[leaf].electrical_transition)
            .collect::<Vec<_>>();
        let cell_cost = if self.loads_ready {
            self.catalog.estimate_joint_binding(
                joint.binding,
                &signature_transitions,
                [
                    self.load_estimates[joint.slots[0]],
                    self.load_estimates[joint.slots[1]],
                ],
            )
        } else {
            joint.cost
        };
        Some(cost.cell_with_electrical(joint.cost, cell_cost))
    }

    fn flow_node(&self, index: usize) -> [Option<FlowChoice>; 2] {
        let node = LogicNodeId::from_index(index);
        let positive = index * 2;
        match self.network.node(node) {
            LogicNode::Const(value) => {
                debug_assert!(!value);
                [
                    Some(FlowChoice {
                        choice: SlotChoice::Constant(false),
                        cost: MappingCost::zero(),
                        truth: TruthTable {
                            input_count: 0,
                            bits: 0,
                        },
                        order: (0, 0, 0),
                    }),
                    Some(FlowChoice {
                        choice: SlotChoice::Constant(true),
                        cost: MappingCost::zero(),
                        truth: TruthTable {
                            input_count: 0,
                            bits: 0,
                        },
                        order: (0, 0, 0),
                    }),
                ]
            }
            LogicNode::Var(origin) => {
                let arrival = self
                    .input_arrivals
                    .get(origin as usize)
                    .copied()
                    .unwrap_or(0.0);
                let transition = self
                    .input_transitions
                    .get(origin as usize)
                    .copied()
                    .unwrap_or(0.0);
                let boundary = Some(FlowChoice {
                    choice: SlotChoice::Boundary(origin as usize),
                    cost: MappingCost {
                        delay: arrival,
                        electrical_delay: arrival,
                        transition,
                        electrical_transition: transition,
                        ..MappingCost::zero()
                    },
                    truth: TruthTable {
                        input_count: 0,
                        bits: 0,
                    },
                    order: (0, 0, 0),
                });
                let inverted = self.inverter.map(|inverter| {
                    let cost = self.estimated_binding_cost(
                        inverter.binding,
                        &[transition],
                        positive + 1,
                        inverter.cost,
                    );
                    FlowChoice {
                        choice: SlotChoice::Inverter,
                        cost: MappingCost {
                            delay: arrival,
                            electrical_delay: arrival,
                            transition,
                            electrical_transition: transition,
                            ..MappingCost::zero()
                        }
                        .cell_with_electrical(inverter.cost, cost),
                        truth: inverter_truth(),
                        order: (0, 0, 0),
                    }
                });
                [boundary, inverted]
            }
            LogicNode::And(..) | LogicNode::Xor(..) | LogicNode::Mux { .. } => {
                let direct = [self.best_direct(index, 0), self.best_direct(index, 1)];
                let anchors = std::array::from_fn(|phase| {
                    let slot_id = positive + phase;
                    let mut best = direct[phase];
                    for &joint_id in &self.slot_joints[slot_id] {
                        let virtual_slot = self.base_slots + joint_id as usize;
                        if self.choices[virtual_slot].is_none() {
                            continue;
                        }
                        let joint = &self.joints[joint_id as usize];
                        let side = u8::from(joint.slots[1] == slot_id);
                        let candidate = FlowChoice {
                            choice: SlotChoice::JointOutput(joint_id),
                            cost: self.literal_access_cost(virtual_slot),
                            truth: joint.truths[usize::from(side)],
                            order: (u8::MAX - 1, side, joint_id),
                        };
                        if best.is_none_or(|best| self.prefers(slot_id, &candidate, &best)) {
                            best = Some(candidate);
                        }
                    }
                    best
                });
                let mut selected = std::array::from_fn(|phase| {
                    let slot_id = positive + phase;
                    let mut best = anchors[phase];
                    if let (Some(inverter), Some(other)) = (self.inverter, anchors[1 - phase]) {
                        let inverter_cost = self.estimated_binding_cost(
                            inverter.binding,
                            &[other.cost.electrical_transition],
                            slot_id,
                            inverter.cost,
                        );
                        let candidate = FlowChoice {
                            choice: SlotChoice::Inverter,
                            cost: other
                                .cost
                                .cell_with_electrical(inverter.cost, inverter_cost),
                            truth: inverter_truth(),
                            order: (u8::MAX, u8::MAX, 0),
                        };
                        if best.is_none_or(|best| self.prefers(slot_id, &candidate, &best)) {
                            best = Some(candidate);
                        }
                    }
                    best
                });
                if selected.iter().all(|choice| {
                    choice.is_some_and(|choice| choice.choice == SlotChoice::Inverter)
                }) {
                    let phase = usize::from(self.phase_anchor_prefers(index, 1, 0, anchors));
                    selected[phase] = anchors[phase];
                }
                selected
            }
        }
    }

    fn phase_anchor_prefers(
        &self,
        index: usize,
        candidate: usize,
        current: usize,
        anchors: [Option<FlowChoice>; 2],
    ) -> bool {
        let candidate_profile = self.phase_anchor_profile(index, candidate, anchors[candidate]);
        let current_profile = self.phase_anchor_profile(index, current, anchors[current]);
        candidate_profile
            .0
            .total_cmp(&current_profile.0)
            .then_with(|| compare_mapping_cost(candidate_profile.1, current_profile.1))
            .then_with(|| candidate.cmp(&current))
            .is_lt()
    }

    fn phase_anchor_profile(
        &self,
        index: usize,
        phase: usize,
        anchor: Option<FlowChoice>,
    ) -> (f64, MappingCost) {
        let anchor = anchor.expect("mutual phase inversion requires two acyclic covers");
        let inverter = self
            .inverter
            .expect("mutual phase inversion requires an inverter");
        let inverted_slot = index * 2 + 1 - phase;
        let electrical = self.estimated_binding_cost(
            inverter.binding,
            &[anchor.cost.electrical_transition],
            inverted_slot,
            inverter.cost,
        );
        let cost = anchor.cost.cell_with_electrical(inverter.cost, electrical);
        let mut arrivals = [cost.electrical_delay; 2];
        arrivals[phase] = anchor.cost.electrical_delay;
        let lateness = arrivals
            .into_iter()
            .enumerate()
            .filter_map(|(phase, arrival)| {
                let required = self.required_arrivals[index * 2 + phase];
                required
                    .is_finite()
                    .then_some((arrival - required).max(0.0))
            })
            .fold(0.0f64, f64::max);
        (lateness, cost)
    }

    fn best_direct(&self, index: usize, phase: usize) -> Option<FlowChoice> {
        let slot_id = index * 2 + phase;
        let node = LogicNodeId::from_index(index);
        let cut_list = self.cuts.cuts(node);
        let mut best: Option<FlowChoice> = None;
        'candidates: for (candidate_index, candidate) in self.candidates[slot_id].iter().enumerate()
        {
            let cut = cut_list[candidate.cut as usize];
            let mut cost = MappingCost::zero();
            let mut signature_transitions = Vec::with_capacity(cut.len() + 1);
            let extra = candidate.extra_slot(cut);
            for leaf_slot in cut
                .leaves()
                .iter()
                .copied()
                .enumerate()
                .map(|(input, leaf)| candidate.leaf_slot(input, leaf))
                .chain(extra)
            {
                if self.choices[leaf_slot].is_none() {
                    continue 'candidates;
                }
                let leaf_cost = self.literal_access_cost(leaf_slot);
                signature_transitions.push(leaf_cost.electrical_transition);
                cost = cost.combine(leaf_cost);
            }
            let binding = candidate.cell_binding(self.catalog);
            let nominal = candidate.nominal_cost(self.catalog);
            let cell_cost =
                self.estimated_binding_cost(binding, &signature_transitions, slot_id, nominal);
            let flow = FlowChoice {
                choice: SlotChoice::Cell(
                    u32::try_from(candidate_index)
                        .expect("candidate construction enforces compact indices"),
                ),
                cost: cost.cell_with_electrical(nominal, cell_cost),
                truth: candidate.truth(),
                order: (candidate.cut, candidate.inversions, 0),
            };
            if best.is_none_or(|best| self.prefers(slot_id, &flow, &best)) {
                best = Some(flow);
            }
        }
        best
    }

    fn prefers(&self, slot_id: usize, candidate: &FlowChoice, current: &FlowChoice) -> bool {
        let required = self.required_arrivals[slot_id];
        let order = compare_mapping_cost_with_required_time(required, candidate.cost, current.cost);
        order
            .then_with(|| candidate.truth.cmp(&current.truth))
            .then_with(|| candidate.order.cmp(&current.order))
            .is_lt()
    }

    fn set(&mut self, slot: usize, choice: SlotChoice, cost: MappingCost) {
        self.set_choice(slot, Some(choice));
        self.flows[slot] = cost;
    }

    fn estimated_binding_cost(
        &self,
        binding: CellBinding,
        signature_transitions: &[f64],
        output_slot: usize,
        nominal: CellCost,
    ) -> CellCost {
        if self.loads_ready {
            self.catalog.estimate_binding(
                binding,
                signature_transitions,
                self.load_estimates[output_slot],
            )
        } else {
            nominal
        }
    }

    pub(crate) fn candidate_electrical_cost(
        &self,
        slot_id: usize,
        candidate: Candidate,
    ) -> CellCost {
        let cut = self.candidate_cut(slot_id, &candidate);
        let mut transitions = cut
            .leaves()
            .iter()
            .copied()
            .enumerate()
            .map(|(input, leaf)| self.flows[candidate.leaf_slot(input, leaf)].electrical_transition)
            .collect::<Vec<_>>();
        if let Some(extra) = candidate.extra_slot(cut) {
            transitions.push(self.flows[extra].electrical_transition);
        }
        self.estimated_binding_cost(
            candidate.cell_binding(self.catalog),
            &transitions,
            slot_id,
            candidate.nominal_cost(self.catalog),
        )
    }

    pub(crate) fn inverter_electrical_cost(
        &self,
        slot_id: usize,
        inverter: InverterCell,
    ) -> CellCost {
        self.estimated_binding_cost(
            inverter.binding,
            &[self.flows[opposite(slot_id)].electrical_transition],
            slot_id,
            inverter.cost,
        )
    }

    pub(crate) fn joint_electrical_cost(&self, joint_id: u32) -> CellCost {
        let joint = &self.joints[joint_id as usize];
        if !self.loads_ready {
            return joint.cost;
        }
        let transitions = joint
            .leaf_slots()
            .map(|leaf| self.flows[leaf].electrical_transition)
            .collect::<Vec<_>>();
        self.catalog.estimate_joint_binding(
            joint.binding,
            &transitions,
            [
                self.load_estimates[joint.slots[0]],
                self.load_estimates[joint.slots[1]],
            ],
        )
    }
}
