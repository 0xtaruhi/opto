// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Borrowed, allocation-free views into packed target-cell arenas.
//!
//! Every view borrows the arena that owns its compact IDs, preventing IDs from
//! escaping or being mixed across libraries.

use super::{
    ArcDelayModel, BTreeSet, CellRecord, FunctionNode, LocalCellId, LookupTable,
    PinReceiverCapacitanceModel, PinRecord, SequentialRecord, TargetCellArena, TargetCellUsage,
    TargetClockGateKind, TargetClockGateRole, TargetFunctionId, TargetMemory, TargetNextStateType,
    TargetPinDirection, TargetPinId, TargetSequentialId, TargetSequentialKind, TargetTimingArcId,
    TargetTimingType, TimingArcRecord, TimingEdge, fmt,
};

#[derive(Clone, Copy)]
/// Borrowed view of one target cell.
pub struct TargetCellRef<'a> {
    pub(super) arena: &'a TargetCellArena,
    pub(super) local: LocalCellId,
    pub(super) dont_use: bool,
}

impl fmt::Debug for TargetCellRef<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TargetCellRef")
            .field("name", &self.name())
            .finish_non_exhaustive()
    }
}

impl<'a> TargetCellRef<'a> {
    fn record(self) -> &'a CellRecord {
        &self.arena.cells[self.local.index()]
    }

    #[must_use]
    /// Returns the library-unique cell name.
    pub fn name(self) -> &'a str {
        self.arena.name(self.record().name)
    }

    #[must_use]
    /// Returns the cell area, when characterized.
    pub fn area(self) -> Option<f64> {
        self.record().area
    }

    #[must_use]
    /// Returns whether mapping must exclude this cell.
    pub fn dont_use(self) -> bool {
        self.record().dont_use || self.dont_use
    }

    #[must_use]
    /// Returns the cell's specialized-usage restrictions.
    pub fn usage(self) -> TargetCellUsage {
        self.record().usage
    }

    #[must_use]
    /// Returns the integrated clock-gate form, when declared.
    pub fn clock_gate(self) -> Option<TargetClockGateKind> {
        self.record().clock_gate
    }

    #[must_use]
    /// Returns the exact memory-macro contract, when this cell implements one.
    pub fn memory(self) -> Option<&'a TargetMemory> {
        self.record()
            .memory
            .and_then(|index| self.arena.memories.get(index as usize))
    }

    #[must_use]
    /// Finds the pin serving the requested clock-gate role.
    pub fn clock_gate_pin(self, role: TargetClockGateRole) -> Option<TargetPinRef<'a>> {
        self.pins().find(|pin| pin.clock_gate_role() == Some(role))
    }

    #[must_use]
    /// Returns whether the cell is available to general-purpose mapping.
    pub fn is_synthesis_eligible(self) -> bool {
        !self.dont_use() && self.usage().is_general_purpose()
    }

    #[must_use]
    /// Compares complete cell content across arenas.
    pub fn content_eq(self, other: Self) -> bool {
        self.name() == other.name()
            && self.area() == other.area()
            && self.dont_use() == other.dont_use()
            && self.usage() == other.usage()
            && self.memory() == other.memory()
            && self.pins().len() == other.pins().len()
            && self
                .pins()
                .zip(other.pins())
                .all(|(left, right)| left.content_eq(right))
            && self.sequential().len() == other.sequential().len()
            && self
                .sequential()
                .zip(other.sequential())
                .all(|(left, right)| left.content_eq(right))
    }

    #[must_use]
    /// Compares the target-binding semantics shared by compatible PVT views.
    /// Characterized delays, transitions, capacitances, power, and area are
    /// deliberately excluded because those are scenario response data.
    pub fn mapping_eq(self, other: Self) -> bool {
        self.name() == other.name()
            && self.dont_use() == other.dont_use()
            && self.usage() == other.usage()
            && self.clock_gate() == other.clock_gate()
            && self.memory() == other.memory()
            && self.pins().len() == other.pins().len()
            && self
                .pins()
                .zip(other.pins())
                .all(|(left, right)| left.mapping_eq(right))
            && self.sequential().len() == other.sequential().len()
            && self
                .sequential()
                .zip(other.sequential())
                .all(|(left, right)| left.content_eq(right))
    }

    /// Iterates over pins in library order.
    ///
    /// # Panics
    ///
    /// Panics only if a sealed pin range cannot be represented by the arena's
    /// typed pin ID; sealing validates that capacity.
    #[must_use]
    pub fn pins(self) -> impl Clone + ExactSizeIterator<Item = TargetPinRef<'a>> {
        let arena = self.arena;
        self.record().pins.indices().map(move |index| TargetPinRef {
            arena,
            id: TargetPinId::new(index).expect("sealed target pin ID"),
        })
    }

    /// Iterates over sequential groups in library order.
    ///
    /// # Panics
    ///
    /// Panics only if a sealed sequential range cannot be represented by its
    /// typed arena ID.
    #[must_use]
    pub fn sequential(self) -> impl Clone + ExactSizeIterator<Item = TargetSequentialRef<'a>> {
        let arena = self.arena;
        self.record()
            .sequential
            .indices()
            .map(move |index| TargetSequentialRef {
                arena,
                id: TargetSequentialId::new(index).expect("sealed target sequential ID"),
            })
    }
}

#[derive(Clone, Copy)]
/// Borrowed view of one target-cell pin.
pub struct TargetPinRef<'a> {
    pub(super) arena: &'a TargetCellArena,
    pub(super) id: TargetPinId,
}

impl fmt::Debug for TargetPinRef<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TargetPinRef")
            .field("id", &self.id)
            .field("name", &self.name())
            .finish_non_exhaustive()
    }
}

impl<'a> TargetPinRef<'a> {
    fn record(self) -> &'a PinRecord {
        &self.arena.pins[self.id.slot()]
    }

    #[must_use]
    /// Returns the pin name within its cell.
    pub fn name(self) -> &'a str {
        self.arena.name(self.record().name)
    }

    #[must_use]
    /// Returns the pin signal-flow direction.
    pub fn direction(self) -> TargetPinDirection {
        self.record().direction
    }

    #[must_use]
    /// Returns the output logic function, when defined.
    pub fn function(self) -> Option<BooleanFunctionRef<'a>> {
        self.record()
            .function
            .map(|id| BooleanFunctionRef::new(self.arena, id))
    }

    #[must_use]
    /// Returns the high-impedance control expression, when defined.
    pub fn three_state(self) -> Option<BooleanFunctionRef<'a>> {
        self.record()
            .three_state
            .map(|id| BooleanFunctionRef::new(self.arena, id))
    }

    #[must_use]
    /// Returns the default input capacitance, when characterized.
    pub fn capacitance(self) -> Option<f64> {
        self.record().capacitance
    }

    #[must_use]
    /// Returns the rising-edge input capacitance, when characterized.
    pub fn rise_capacitance(self) -> Option<f64> {
        self.record().rise_capacitance
    }

    #[must_use]
    /// Returns the falling-edge input capacitance, when characterized.
    pub fn fall_capacitance(self) -> Option<f64> {
        self.record().fall_capacitance
    }

    #[must_use]
    /// Returns the edge-specific capacitance, falling back to the default.
    pub fn capacitance_at(self, edge: TimingEdge) -> Option<f64> {
        match edge {
            TimingEdge::Rise => self.rise_capacitance(),
            TimingEdge::Fall => self.fall_capacitance(),
        }
        .or(self.capacitance())
    }

    #[must_use]
    /// Returns the greatest characterized edge capacitance.
    pub fn max_capacitance(self) -> Option<f64> {
        match (
            self.capacitance_at(TimingEdge::Rise),
            self.capacitance_at(TimingEdge::Fall),
        ) {
            (Some(rise), Some(fall)) => Some(rise.max(fall)),
            (Some(value), None) | (None, Some(value)) => Some(value),
            (None, None) => None,
        }
    }

    #[must_use]
    /// Returns the scalar input-capacitance load contributed to a design net.
    ///
    /// Liberty permits an input or inout pin to omit capacitance, in which case
    /// it contributes no scalar capacitive load. Output pins never contribute
    /// receiver load through this model.
    pub fn design_input_capacitance(self) -> f64 {
        if matches!(
            self.direction(),
            TargetPinDirection::Input | TargetPinDirection::Inout
        ) {
            self.max_capacitance().unwrap_or(0.0)
        } else {
            0.0
        }
    }

    #[must_use]
    /// Returns the edge-specific input-capacitance load contributed to a net.
    pub fn design_input_capacitance_at(self, edge: TimingEdge) -> f64 {
        if matches!(
            self.direction(),
            TargetPinDirection::Input | TargetPinDirection::Inout
        ) {
            self.capacitance_at(edge).unwrap_or(0.0)
        } else {
            0.0
        }
    }

    #[must_use]
    /// Returns the slew-dependent receiver-capacitance model, when characterized.
    pub fn receiver_capacitance(self) -> Option<&'a PinReceiverCapacitanceModel> {
        self.record()
            .receiver_capacitance
            .map(|id| &self.arena.receiver_models[id.slot()])
    }

    #[must_use]
    /// Returns the explicit abstract fanout load, when declared.
    pub fn fanout_load(self) -> Option<f64> {
        self.record().fanout_load
    }

    #[must_use]
    /// Returns the abstract fanout load contributed to a design net.
    ///
    /// An input or inout without an explicit Liberty `fanout_load` contributes
    /// one fanout unit. Output pins contribute no sink load.
    pub fn design_fanout_load(self) -> f64 {
        if matches!(
            self.direction(),
            TargetPinDirection::Input | TargetPinDirection::Inout
        ) {
            self.fanout_load().unwrap_or(1.0)
        } else {
            0.0
        }
    }

    #[must_use]
    /// Returns the sequential input role, when declared.
    pub fn next_state_type(self) -> Option<TargetNextStateType> {
        self.record().next_state_type
    }

    #[must_use]
    /// Returns the integrated clock-gate role, when declared.
    pub fn clock_gate_role(self) -> Option<TargetClockGateRole> {
        self.record().clock_gate_role
    }

    /// Iterates over timing arcs terminating at this pin.
    ///
    /// # Panics
    ///
    /// Panics only if a sealed timing-arc range cannot be represented by its
    /// typed arena ID.
    #[must_use]
    pub fn timing_arcs(self) -> impl Clone + ExactSizeIterator<Item = TargetTimingArcRef<'a>> {
        let arena = self.arena;
        self.record()
            .timing_arcs
            .indices()
            .map(move |index| TargetTimingArcRef {
                arena,
                id: TargetTimingArcId::new(index).expect("sealed target timing arc ID"),
            })
    }

    fn content_eq(self, other: Self) -> bool {
        self.name() == other.name()
            && self.direction() == other.direction()
            && functions_equal(self.function(), other.function())
            && functions_equal(self.three_state(), other.three_state())
            && self.capacitance() == other.capacitance()
            && self.rise_capacitance() == other.rise_capacitance()
            && self.fall_capacitance() == other.fall_capacitance()
            && self.receiver_capacitance() == other.receiver_capacitance()
            && self.fanout_load() == other.fanout_load()
            && self.next_state_type() == other.next_state_type()
            && self.timing_arcs().len() == other.timing_arcs().len()
            && self
                .timing_arcs()
                .zip(other.timing_arcs())
                .all(|(left, right)| left.content_eq(right))
    }

    fn mapping_eq(self, other: Self) -> bool {
        self.name() == other.name()
            && self.direction() == other.direction()
            && functions_equal(self.function(), other.function())
            && functions_equal(self.three_state(), other.three_state())
            && self.next_state_type() == other.next_state_type()
            && self.clock_gate_role() == other.clock_gate_role()
    }
}

#[derive(Clone, Copy)]
/// Borrowed view of one target timing arc.
pub struct TargetTimingArcRef<'a> {
    pub(super) arena: &'a TargetCellArena,
    pub(super) id: TargetTimingArcId,
}

impl fmt::Debug for TargetTimingArcRef<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TargetTimingArcRef")
            .field("id", &self.id)
            .field("related_pin", &self.related_pin())
            .finish_non_exhaustive()
    }
}

impl<'a> TargetTimingArcRef<'a> {
    fn record(self) -> &'a TimingArcRecord {
        &self.arena.timing_arcs[self.id.slot()]
    }

    #[must_use]
    /// Returns the related source or control pin name.
    pub fn related_pin(self) -> &'a str {
        self.arena.name(self.record().related_pin)
    }

    #[must_use]
    /// Returns the semantic timing-arc type.
    pub fn timing_type(self) -> TargetTimingType {
        self.record().timing_type
    }

    #[must_use]
    /// Returns the input-to-output unateness relationship.
    pub fn timing_sense(self) -> crate::TimingSense {
        self.record().timing_sense
    }

    #[must_use]
    /// Returns the propagation and slew model, when characterized.
    pub fn delay_model(self) -> Option<&'a ArcDelayModel> {
        self.record()
            .delay_model
            .map(|id| &self.arena.delay_models[id.slot()])
    }

    #[must_use]
    /// Returns the rising-edge constraint table, when characterized.
    pub fn rise_constraint(self) -> Option<&'a LookupTable> {
        self.record()
            .rise_constraint
            .map(|id| &self.arena.tables[id.slot()])
    }

    #[must_use]
    /// Returns the falling-edge constraint table, when characterized.
    pub fn fall_constraint(self) -> Option<&'a LookupTable> {
        self.record()
            .fall_constraint
            .map(|id| &self.arena.tables[id.slot()])
    }

    #[must_use]
    /// Returns the scalar fallback propagation delay.
    pub fn default_delay(self) -> Option<f64> {
        self.delay_model().and_then(ArcDelayModel::default_delay)
    }

    #[must_use]
    /// Returns the scalar fallback output transition.
    pub fn default_transition(self) -> Option<f64> {
        self.delay_model()
            .and_then(ArcDelayModel::default_transition)
    }

    #[must_use]
    /// Interpolates propagation delay for an output edge.
    pub fn delay_at(
        self,
        output_edge: TimingEdge,
        input_transition: Option<f64>,
        output_load: Option<f64>,
    ) -> Option<f64> {
        self.delay_model()
            .and_then(|model| model.delay_at(output_edge, input_transition, output_load))
    }

    #[must_use]
    /// Interpolates output transition time for an output edge.
    pub fn transition_at(
        self,
        output_edge: TimingEdge,
        input_transition: Option<f64>,
        output_load: Option<f64>,
    ) -> Option<f64> {
        self.delay_model()
            .and_then(|model| model.transition_at(output_edge, input_transition, output_load))
    }

    #[must_use]
    /// Interpolates effective receiver capacitance for an edge pair.
    pub fn receiver_capacitance_at(
        self,
        input_edge: TimingEdge,
        output_edge: TimingEdge,
        input_transition: Option<f64>,
        output_load: Option<f64>,
    ) -> Option<f64> {
        self.delay_model().and_then(|model| {
            model.receiver_capacitance_at(input_edge, output_edge, input_transition, output_load)
        })
    }

    #[must_use]
    /// Interpolates a timing-check constraint for the data edge.
    pub fn constraint_at(
        self,
        data_edge: TimingEdge,
        clock_transition: Option<f64>,
        data_transition: Option<f64>,
    ) -> Option<f64> {
        match data_edge {
            TimingEdge::Rise => self.rise_constraint(),
            TimingEdge::Fall => self.fall_constraint(),
        }
        .and_then(|table| table.value_at(clock_transition, data_transition))
    }

    fn content_eq(self, other: Self) -> bool {
        self.related_pin() == other.related_pin()
            && self.timing_type() == other.timing_type()
            && self.timing_sense() == other.timing_sense()
            && self.delay_model() == other.delay_model()
            && self.rise_constraint() == other.rise_constraint()
            && self.fall_constraint() == other.fall_constraint()
    }
}

#[derive(Clone, Copy)]
/// Borrowed view of one sequential state description.
pub struct TargetSequentialRef<'a> {
    pub(super) arena: &'a TargetCellArena,
    pub(super) id: TargetSequentialId,
}

impl fmt::Debug for TargetSequentialRef<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TargetSequentialRef")
            .field("id", &self.id)
            .field("kind", &self.kind())
            .finish_non_exhaustive()
    }
}

impl<'a> TargetSequentialRef<'a> {
    fn record(self) -> &'a SequentialRecord {
        &self.arena.sequential[self.id.slot()]
    }

    #[must_use]
    /// Returns whether the group represents a flip-flop or latch.
    pub fn kind(self) -> TargetSequentialKind {
        self.record().kind
    }

    /// Iterates over state-variable names in declaration order.
    #[must_use]
    pub fn state_variables(self) -> impl Clone + ExactSizeIterator<Item = &'a str> {
        let arena = self.arena;
        self.record()
            .state_variables
            .indices()
            .map(move |index| arena.name(arena.state_variables[index]))
    }

    fn function(self, id: Option<TargetFunctionId>) -> Option<BooleanFunctionRef<'a>> {
        id.map(|id| BooleanFunctionRef::new(self.arena, id))
    }

    #[must_use]
    /// Returns the clock or gate-control expression.
    pub fn clocked_on(self) -> Option<BooleanFunctionRef<'a>> {
        self.function(self.record().clocked_on)
    }

    #[must_use]
    /// Returns the next-state data expression.
    pub fn next_state(self) -> Option<BooleanFunctionRef<'a>> {
        self.function(self.record().next_state)
    }

    #[must_use]
    /// Returns the optional latch-enable expression.
    pub fn enable(self) -> Option<BooleanFunctionRef<'a>> {
        self.function(self.record().enable)
    }

    #[must_use]
    /// Returns the asynchronous clear expression.
    pub fn clear(self) -> Option<BooleanFunctionRef<'a>> {
        self.function(self.record().clear)
    }

    #[must_use]
    /// Returns the asynchronous preset expression.
    pub fn preset(self) -> Option<BooleanFunctionRef<'a>> {
        self.function(self.record().preset)
    }

    fn content_eq(self, other: Self) -> bool {
        self.kind() == other.kind()
            && self.state_variables().eq(other.state_variables())
            && functions_equal(self.clocked_on(), other.clocked_on())
            && functions_equal(self.next_state(), other.next_state())
            && functions_equal(self.enable(), other.enable())
            && functions_equal(self.clear(), other.clear())
            && functions_equal(self.preset(), other.preset())
    }
}

#[derive(Clone, Copy)]
/// Borrowed node in an arena-backed Boolean expression.
pub struct BooleanFunctionRef<'a> {
    pub(super) arena: &'a TargetCellArena,
    pub(super) id: TargetFunctionId,
}

#[derive(Debug, Clone, Copy)]
/// Operator and operands of one arena-backed Boolean-expression node.
pub enum BooleanFunctionKind<'a> {
    /// Boolean constant.
    Const(bool),
    /// Named pin or state variable.
    Pin(&'a str),
    /// Logical negation.
    Not(BooleanFunctionRef<'a>),
    /// Logical conjunction.
    And(BooleanFunctionRef<'a>, BooleanFunctionRef<'a>),
    /// Logical disjunction.
    Or(BooleanFunctionRef<'a>, BooleanFunctionRef<'a>),
    /// Logical exclusive OR.
    Xor(BooleanFunctionRef<'a>, BooleanFunctionRef<'a>),
    /// Logical implication.
    Imp(BooleanFunctionRef<'a>, BooleanFunctionRef<'a>),
    /// Logical equivalence.
    Iff(BooleanFunctionRef<'a>, BooleanFunctionRef<'a>),
    /// Conditional expression.
    Cond(
        BooleanFunctionRef<'a>,
        BooleanFunctionRef<'a>,
        BooleanFunctionRef<'a>,
    ),
}

impl fmt::Debug for BooleanFunctionRef<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("BooleanFunctionRef")
            .field(&self.id)
            .finish()
    }
}

impl<'a> BooleanFunctionRef<'a> {
    const fn new(arena: &'a TargetCellArena, id: TargetFunctionId) -> Self {
        Self { arena, id }
    }

    fn node(self) -> FunctionNode {
        self.arena.functions[self.id.slot()]
    }

    fn child(self, id: TargetFunctionId) -> Self {
        Self::new(self.arena, id)
    }

    #[must_use]
    /// Returns this node's operator and borrowed operands.
    pub fn kind(self) -> BooleanFunctionKind<'a> {
        match self.node() {
            FunctionNode::Const(value) => BooleanFunctionKind::Const(value),
            FunctionNode::Pin(name) => BooleanFunctionKind::Pin(self.arena.name(name)),
            FunctionNode::Not(argument) => BooleanFunctionKind::Not(self.child(argument)),
            FunctionNode::And(left, right) => {
                BooleanFunctionKind::And(self.child(left), self.child(right))
            }
            FunctionNode::Or(left, right) => {
                BooleanFunctionKind::Or(self.child(left), self.child(right))
            }
            FunctionNode::Xor(left, right) => {
                BooleanFunctionKind::Xor(self.child(left), self.child(right))
            }
            FunctionNode::Imp(left, right) => {
                BooleanFunctionKind::Imp(self.child(left), self.child(right))
            }
            FunctionNode::Iff(left, right) => {
                BooleanFunctionKind::Iff(self.child(left), self.child(right))
            }
            FunctionNode::Cond(condition, when_true, when_false) => BooleanFunctionKind::Cond(
                self.child(condition),
                self.child(when_true),
                self.child(when_false),
            ),
        }
    }

    /// Evaluates the expression using `lookup` for named pins.
    ///
    /// Returns `None` if the callback cannot resolve a referenced name.
    pub fn eval(self, lookup: &mut impl FnMut(&str) -> Option<bool>) -> Option<bool> {
        match self.node() {
            FunctionNode::Const(value) => Some(value),
            FunctionNode::Pin(name) => lookup(self.arena.name(name)),
            FunctionNode::Not(argument) => Some(!self.child(argument).eval(lookup)?),
            FunctionNode::And(left, right) => {
                Some(self.child(left).eval(lookup)? & self.child(right).eval(lookup)?)
            }
            FunctionNode::Or(left, right) => {
                Some(self.child(left).eval(lookup)? | self.child(right).eval(lookup)?)
            }
            FunctionNode::Xor(left, right) => {
                Some(self.child(left).eval(lookup)? ^ self.child(right).eval(lookup)?)
            }
            FunctionNode::Imp(left, right) => {
                Some(!self.child(left).eval(lookup)? | self.child(right).eval(lookup)?)
            }
            FunctionNode::Iff(left, right) => {
                Some(self.child(left).eval(lookup)? == self.child(right).eval(lookup)?)
            }
            FunctionNode::Cond(condition, when_true, when_false) => {
                if self.child(condition).eval(lookup)? {
                    self.child(when_true).eval(lookup)
                } else {
                    self.child(when_false).eval(lookup)
                }
            }
        }
    }

    /// Evaluates the complete truth table for `inputs`, with the first input
    /// occupying the least-significant assignment bit.
    ///
    /// Returns `None` when the function references an unknown input or when
    /// more than six inputs would exceed the returned 64-bit table.
    #[must_use]
    pub fn truth_table_bits(self, inputs: &[&str]) -> Option<u64> {
        if inputs.len() > 6 {
            return None;
        }
        let mut bits = 0u64;
        for assignment in 0..(1usize << inputs.len()) {
            let value = self.eval(&mut |name| {
                let index = inputs.iter().position(|input| *input == name)?;
                Some(((assignment >> index) & 1) == 1)
            })?;
            if value {
                bits |= 1u64 << assignment;
            }
        }
        Some(bits)
    }

    #[must_use]
    /// Returns a `(pin, polarity)` pair when this is a single literal.
    pub fn as_literal(self) -> Option<(&'a str, bool)> {
        match self.node() {
            FunctionNode::Pin(name) => Some((self.arena.name(name), true)),
            FunctionNode::Not(argument) => match self.child(argument).node() {
                FunctionNode::Pin(name) => Some((self.arena.name(name), false)),
                _ => None,
            },
            _ => None,
        }
    }

    /// Visits every pin occurrence in expression order.
    pub fn for_each_pin(self, visitor: &mut impl FnMut(&'a str)) {
        match self.node() {
            FunctionNode::Const(_) => {}
            FunctionNode::Pin(name) => visitor(self.arena.name(name)),
            FunctionNode::Not(argument) => self.child(argument).for_each_pin(visitor),
            FunctionNode::And(left, right)
            | FunctionNode::Or(left, right)
            | FunctionNode::Xor(left, right)
            | FunctionNode::Imp(left, right)
            | FunctionNode::Iff(left, right) => {
                self.child(left).for_each_pin(visitor);
                self.child(right).for_each_pin(visitor);
            }
            FunctionNode::Cond(condition, when_true, when_false) => {
                self.child(condition).for_each_pin(visitor);
                self.child(when_true).for_each_pin(visitor);
                self.child(when_false).for_each_pin(visitor);
            }
        }
    }

    pub(super) fn first_unknown(self, names: &BTreeSet<&str>) -> Option<&'a str> {
        match self.node() {
            FunctionNode::Const(_) => None,
            FunctionNode::Pin(name) => {
                let name = self.arena.name(name);
                (!names.contains(name)).then_some(name)
            }
            FunctionNode::Not(argument) => self.child(argument).first_unknown(names),
            FunctionNode::And(left, right)
            | FunctionNode::Or(left, right)
            | FunctionNode::Xor(left, right)
            | FunctionNode::Imp(left, right)
            | FunctionNode::Iff(left, right) => self
                .child(left)
                .first_unknown(names)
                .or_else(|| self.child(right).first_unknown(names)),
            FunctionNode::Cond(condition, when_true, when_false) => self
                .child(condition)
                .first_unknown(names)
                .or_else(|| self.child(when_true).first_unknown(names))
                .or_else(|| self.child(when_false).first_unknown(names)),
        }
    }

    #[must_use]
    pub(crate) fn semantic_eq(self, other: Self) -> bool {
        match (self.node(), other.node()) {
            (FunctionNode::Const(left), FunctionNode::Const(right)) => left == right,
            (FunctionNode::Pin(left), FunctionNode::Pin(right)) => {
                self.arena.name(left) == other.arena.name(right)
            }
            (FunctionNode::Not(left), FunctionNode::Not(right)) => {
                self.child(left).semantic_eq(other.child(right))
            }
            (FunctionNode::And(ll, lr), FunctionNode::And(rl, rr))
            | (FunctionNode::Or(ll, lr), FunctionNode::Or(rl, rr))
            | (FunctionNode::Xor(ll, lr), FunctionNode::Xor(rl, rr))
            | (FunctionNode::Imp(ll, lr), FunctionNode::Imp(rl, rr))
            | (FunctionNode::Iff(ll, lr), FunctionNode::Iff(rl, rr)) => {
                self.child(ll).semantic_eq(other.child(rl))
                    && self.child(lr).semantic_eq(other.child(rr))
            }
            (FunctionNode::Cond(lc, lt, lf), FunctionNode::Cond(rc, rt, rf)) => {
                self.child(lc).semantic_eq(other.child(rc))
                    && self.child(lt).semantic_eq(other.child(rt))
                    && self.child(lf).semantic_eq(other.child(rf))
            }
            _ => false,
        }
    }
}

impl PartialEq for BooleanFunctionRef<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.semantic_eq(*other)
    }
}

impl Eq for BooleanFunctionRef<'_> {}

fn functions_equal(
    left: Option<BooleanFunctionRef<'_>>,
    right: Option<BooleanFunctionRef<'_>>,
) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left.semantic_eq(right),
        (None, None) => true,
        _ => false,
    }
}
