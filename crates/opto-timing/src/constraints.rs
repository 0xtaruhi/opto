// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Persistent timing constraints, transactions, and synthesis fingerprints.
//!
//! [`TimingContext`] stores object-ID-based constraints independently of
//! command spelling. Mutations advance a revision and maintain reverse indexes
//! so object deletion can be prepared, validated, and committed atomically.

use super::*;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::{BTreeMap, BTreeSet};

mod arbitration;
mod checkpoint;
mod commands;
mod index;
mod model;

pub use checkpoint::TimingContextCheckpoint;
pub use model::*;

pub(crate) use arbitration::*;
mod storage;

pub(crate) use index::bus_base_name;
use storage::{ArenaInsertion, ArenaRemoval, OrderedArena, RawSlot};
pub use storage::{TimingRowIter, TimingRows};

const TIMING_FINGERPRINT_DOMAIN: &[u8] = b"opto/synthesis-timing/v1\0";

enum TimingContextOwner {}
enum TimingTransactionOwner {}

/// Typed outcome of a timing-constraint mutation.
///
/// Presentation layers may render this for their command protocol; the timing
/// domain itself does not own Tcl result strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstraintChange {
    /// The requested state was already present.
    Unchanged,
    /// The persistent timing state changed.
    Changed,
}

/// Semantic identity of the timing inputs consumed by synthesis.
///
/// Session revisions deliberately do not participate: two contexts with the
/// same constraints have the same fingerprint even if they were constructed
/// by different command histories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TimingFingerprint([u8; 32]);

impl TimingFingerprint {
    #[must_use]
    /// Returns the stable 256-bit digest.
    pub const fn bytes(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Validated clock stored in a [`TimingContext`].
pub struct Clock {
    /// Persistent clock-object identity.
    pub id: ClockId,
    /// User-visible clock name.
    pub name: String,
    /// Clock period in timing-library units.
    pub period: f64,
    /// Source ports driven by the ideal clock.
    pub sources: Box<[PortId]>,
    /// Optional `(rise, fall)` edge times within one period.
    pub waveform: Option<(f64, f64)>,
    /// User-supplied SDC comment.
    pub comment: String,
    transitions: [[Option<f64>; 2]; 2],
    source_latencies: [[[Option<f64>; 2]; 2]; 2],
    network_latencies: [[Option<f64>; 2]; 2],
    propagated: bool,
    /// Generated-clock derivation metadata.
    pub generated: Option<GeneratedClock>,
}

#[derive(Debug, Clone, PartialEq)]
/// Inputs used to create or replace a clock constraint.
pub struct ClockSpec {
    /// Nonempty user-visible clock name.
    pub name: String,
    /// Positive finite period in timing-library units.
    pub period: f64,
    /// Source ports driven by the clock.
    pub sources: Vec<PortId>,
    /// Optional rising and falling edge times within one period.
    pub waveform: Option<(f64, f64)>,
    /// User-supplied SDC comment.
    pub comment: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Parameters retained for one generated clock.
pub struct GeneratedClock {
    /// Master clock used to derive period and waveform.
    pub master: ClockId,
    /// Master clock source port named by `-source`.
    pub source: PortId,
    /// Optional master-period divisor.
    pub divide_by: Option<u32>,
    /// Optional master-frequency multiplier.
    pub multiply_by: Option<u32>,
    /// Optional duty cycle percentage.
    pub duty_cycle: Option<f64>,
    /// Whether the generated waveform is inverted.
    pub invert: bool,
    /// Optional three-edge transformation.
    pub edges: Option<[u32; 3]>,
    /// Optional shifts applied to transformed edges.
    pub edge_shift: Option<[f64; 3]>,
    /// Whether only combinational paths connect source and target.
    pub combinational: bool,
    /// User-supplied SDC comment.
    pub comment: String,
}

impl Clock {
    pub(crate) fn edge_time(&self, edge: TimingEdge) -> f64 {
        match (edge, self.waveform) {
            (TimingEdge::Rise, Some((rise, _))) => rise,
            (TimingEdge::Fall, Some((_, fall))) => fall,
            (TimingEdge::Rise, None) => 0.0,
            (TimingEdge::Fall, None) => self.period / 2.0,
        }
    }

    pub(crate) fn next_edge_after(&self, edge: TimingEdge, time: f64) -> f64 {
        let phase = self.edge_time(edge);
        let cycles = ((time - phase) / self.period).floor() + 1.0;
        phase + cycles.max(0.0) * self.period
    }

    pub(crate) fn edge_at_or_after(&self, edge: TimingEdge, time: f64) -> f64 {
        let phase = self.edge_time(edge);
        let cycles = ((time - phase) / self.period).ceil().max(0.0);
        phase + cycles * self.period
    }

    pub(crate) fn edge_at_or_before(&self, edge: TimingEdge, time: f64) -> Option<f64> {
        let phase = self.edge_time(edge);
        if time < phase {
            return None;
        }
        let cycles = ((time - phase) / self.period).floor();
        Some(phase + cycles * self.period)
    }

    pub(crate) fn transition(&self, edge: TimingEdge, delay_type: DelayType) -> Option<f64> {
        self.transitions[delay_type.index()][edge.index()]
    }

    pub(crate) fn source_latency(
        &self,
        edge: TimingEdge,
        delay_type: DelayType,
        early: bool,
    ) -> f64 {
        self.source_latencies[delay_type.index()][usize::from(!early)][edge.index()].unwrap_or(0.0)
    }

    pub(crate) fn network_latency(&self, edge: TimingEdge, delay_type: DelayType) -> f64 {
        self.network_latencies[delay_type.index()][edge.index()].unwrap_or(0.0)
    }

    pub(crate) const fn is_propagated(&self) -> bool {
        self.propagated
    }
}

impl ClockSpec {
    /// Validates a clock specification.
    ///
    /// # Errors
    ///
    /// Rejects empty names, nonpositive or non-finite periods, and waveforms
    /// whose nonnegative edges are unordered or exceed the period.
    pub fn new(
        name: impl Into<String>,
        period: f64,
        sources: Vec<PortId>,
        waveform: Option<(f64, f64)>,
    ) -> Result<Self, crate::TimingError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(crate::ConstraintError::EmptyClockName.into());
        }
        if !period.is_finite() || period <= 0.0 {
            return Err(crate::ConstraintError::InvalidClockPeriod { period }.into());
        }
        if let Some((rise, fall)) = waveform {
            if !rise.is_finite() || !fall.is_finite() || rise < 0.0 || fall < 0.0 || rise >= fall {
                return Err(crate::ConstraintError::InvalidClockWaveform { rise, fall }.into());
            }
            if fall > period {
                return Err(
                    crate::ConstraintError::ClockWaveformExceedsPeriod { fall, period }.into(),
                );
            }
        }

        Ok(Self {
            name,
            period,
            sources,
            waveform,
            comment: String::new(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// One clock-relative or unclocked input/output port-delay row.
///
/// Delay slots are indexed by [`DelayType`] and [`TimingEdge`]. A row is
/// uniquely identified within one port by its clock and clock edge.
pub struct IoDelay {
    /// Optional reference clock. `None` denotes an unclocked port delay.
    pub clock: Option<ClockId>,
    /// Reference-clock edge used when `clock` is present.
    pub clock_edge: TimingEdge,
    delays: [[Option<f64>; 2]; 2],
    /// The stated delay already includes source clock latency.
    pub source_latency_included: bool,
    /// The stated delay already includes clock-network latency.
    pub network_latency_included: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(crate) struct PortValueSlots([[Option<f64>; 2]; 2]);

impl PortValueSlots {
    const fn empty() -> Self {
        Self([[None, None], [None, None]])
    }

    pub(crate) fn value(self, edge: TimingEdge, delay_type: DelayType) -> Option<f64> {
        self.0[delay_type.index()][edge.index()]
    }

    pub(crate) fn maximum(self) -> Option<f64> {
        self.0
            .into_iter()
            .flatten()
            .flatten()
            .max_by(f64::total_cmp)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Selects input or output port-delay storage.
pub enum IoDelayKind {
    /// Input delay.
    Input,
    /// Output delay.
    Output,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Relationship class accepted by `set_clock_groups`.
pub enum ClockGroupKind {
    /// Mutually exclusive by logical mode.
    LogicallyExclusive,
    /// Physically exclusive clock sources.
    PhysicallyExclusive,
    /// Asynchronous clock domains.
    Asynchronous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Logic or transition value imposed by `set_case_analysis`.
pub enum CaseAnalysisValue {
    /// Stable logic zero.
    Zero,
    /// Stable logic one.
    One,
    /// Rising transition.
    Rise,
    /// Falling transition.
    Fall,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
/// One disabled timing-arc selection.
pub struct DisabledTiming {
    /// Target cell, pin, port, or net.
    pub target: TimingEndpoint,
    /// Optional source pin pattern.
    pub from: Option<String>,
    /// Optional destination pin pattern.
    pub to: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Delay category selected by `set_timing_derate`.
pub enum TimingDerateKind {
    /// Interconnect delay.
    NetDelay,
    /// Cell propagation delay.
    CellDelay,
    /// Cell timing-check constraint.
    CellCheck,
}

impl TimingDerateKind {
    const fn index(self) -> usize {
        match self {
            Self::NetDelay => 0,
            Self::CellDelay => 1,
            Self::CellCheck => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
struct TimingDerates([[[[f64; 2]; 2]; 2]; 3]);

impl Default for TimingDerates {
    fn default() -> Self {
        Self([[[[1.0; 2]; 2]; 2]; 3])
    }
}

impl ClockGroupKind {
    const fn marker(self) -> &'static str {
        match self {
            Self::LogicallyExclusive => "logical",
            Self::PhysicallyExclusive => "physical",
            Self::Asynchronous => "asynchronous",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct ClockUncertaintyKey {
    from: ClockId,
    from_edge: EdgeSelection,
    to: ClockId,
    to_edge: EdgeSelection,
    delay_type: DelayType,
}

impl IoDelay {
    fn new(
        clock: Option<ClockId>,
        clock_edge: TimingEdge,
        source_latency_included: bool,
        network_latency_included: bool,
    ) -> Self {
        Self {
            clock,
            clock_edge,
            delays: [[None, None], [None, None]],
            source_latency_included,
            network_latency_included,
        }
    }

    pub(crate) fn delay(&self, edge: TimingEdge, delay_type: DelayType) -> Option<f64> {
        self.delays[delay_type.index()][edge.index()]
    }
}

#[derive(Debug)]
/// Mutable, revisioned store of timing constraints.
///
/// Constraints reference permanent database IDs. Nested transactions use an
/// append-only undo journal, while serialized checkpoints contain only primary
/// rows and rebuild derived indexes during restore.
pub struct TimingContext {
    owner: opto_core::OwnerToken<TimingContextOwner>,
    pub(crate) revision: RevisionId,
    clocks: OrderedArena<Clock>,
    pub(crate) input_transitions: BTreeMap<PortId, PortValueSlots>,
    pub(crate) loads: BTreeMap<PortId, PortValueSlots>,
    resistances: BTreeMap<TimingEndpoint, PortValueSlots>,
    input_delays: BTreeMap<PortId, Vec<IoDelay>>,
    output_delays: BTreeMap<PortId, Vec<IoDelay>>,
    clock_uncertainties: BTreeMap<ClockUncertaintyKey, f64>,
    case_analysis: BTreeMap<TimingEndpoint, CaseAnalysisValue>,
    disabled_timing: BTreeSet<DisabledTiming>,
    timing_derates: TimingDerates,
    path_exceptions: OrderedArena<PathException>,
    max_transitions: OrderedArena<DesignRuleConstraint>,
    max_capacitances: OrderedArena<DesignRuleConstraint>,
    max_fanouts: OrderedArena<DesignRuleConstraint>,
    clock_slots: BTreeMap<ClockId, ClockSlot>,
    references: BTreeMap<opto_db::AnyObjectId, BTreeSet<TimingReference>>,
    transactions: Vec<opto_core::OwnerToken<TimingTransactionOwner>>,
    journal: Vec<TimingUndo>,
}

/// Validated revision transition for removing object-bound constraints.
///
/// Preparing and validating this token are the only fallible phases of
/// removal. Applying a validated token performs deterministic ownership
/// updates without allocating a full timing-context rollback snapshot.
#[derive(Debug)]
pub struct PreparedTimingObjectRemoval {
    owner: opto_core::OwnerToken<TimingContextOwner>,
    base_revision: RevisionId,
    revision: Option<RevisionId>,
    clocks: Vec<RowEdit<ClockSlot, Clock>>,
    input_transitions: Vec<PortId>,
    loads: Vec<PortId>,
    resistances: Vec<TimingEndpoint>,
    input_delays: Vec<MapEdit<PortId, Vec<IoDelay>>>,
    output_delays: Vec<MapEdit<PortId, Vec<IoDelay>>>,
    clock_uncertainties: Vec<MapEdit<ClockUncertaintyKey, f64>>,
    case_analysis: Vec<TimingEndpoint>,
    disabled_timing: Vec<DisabledTiming>,
    path_exceptions: Vec<RowEdit<PathExceptionSlot, PathException>>,
    max_transitions: Vec<RowEdit<MaxTransitionSlot, DesignRuleConstraint>>,
    max_capacitances: Vec<RowEdit<MaxCapacitanceSlot, DesignRuleConstraint>>,
    max_fanouts: Vec<RowEdit<MaxFanoutSlot, DesignRuleConstraint>>,
    references: BTreeMap<opto_db::AnyObjectId, BTreeSet<TimingReference>>,
    #[cfg(test)]
    inspected_rows: usize,
}

/// A prepared object-removal edit whose owner and base revision were checked.
///
/// Session transactions obtain this token before their final fallible owner
/// edit, then consume it through the infallible commit path.
#[derive(Debug)]
#[must_use = "a validated timing edit has no effect unless it is committed"]
pub struct ValidatedTimingObjectRemoval<'a> {
    timing: &'a mut TimingContext,
    prepared: PreparedTimingObjectRemoval,
}

impl ValidatedTimingObjectRemoval<'_> {
    /// Commits the edit while the exclusive validation borrow still proves
    /// that its owner and base revision cannot have changed.
    pub fn commit(self) {
        self.timing.commit_object_removal(self.prepared);
    }
}

/// O(1) marker for one nested timing transaction.
#[derive(Debug)]
#[must_use = "a timing checkpoint must be committed or rolled back"]
pub struct TimingCheckpoint {
    owner: opto_core::OwnerToken<TimingContextOwner>,
    identity: opto_core::OwnerToken<TimingTransactionOwner>,
    journal_len: usize,
    revision: RevisionId,
}

#[derive(Debug)]
enum TimingUndo {
    ClockInserted(ArenaInsertion),
    ClockRemoved(ArenaRemoval<Clock>),
    ClockReplaced {
        slot: ClockSlot,
        previous: Clock,
    },
    InputTransition {
        port: PortId,
        previous: Option<PortValueSlots>,
    },
    Load {
        port: PortId,
        previous: Option<PortValueSlots>,
    },
    Resistance {
        endpoint: TimingEndpoint,
        previous: Option<PortValueSlots>,
    },
    InputDelays {
        port: PortId,
        previous: Option<Vec<IoDelay>>,
    },
    OutputDelays {
        port: PortId,
        previous: Option<Vec<IoDelay>>,
    },
    ClockUncertainty {
        key: ClockUncertaintyKey,
        previous: Option<f64>,
    },
    CaseAnalysis {
        endpoint: TimingEndpoint,
        previous: Option<CaseAnalysisValue>,
    },
    DisabledTimingInserted(DisabledTiming),
    DisabledTimingRemoved(DisabledTiming),
    TimingDerates(TimingDerates),
    PathExceptionInserted(ArenaInsertion),
    PathExceptionRemoved(ArenaRemoval<PathException>),
    PathExceptionReplaced {
        slot: PathExceptionSlot,
        previous: PathException,
    },
    DesignRuleInserted {
        kind: DesignRuleKind,
        insertion: ArenaInsertion,
    },
    DesignRuleRemoved {
        kind: DesignRuleKind,
        removal: ArenaRemoval<DesignRuleConstraint>,
    },
    DesignRuleReplaced {
        kind: DesignRuleKind,
        slot: RawSlot,
        previous: DesignRuleConstraint,
    },
}

#[derive(Debug)]
struct RowEdit<I, T> {
    slot: I,
    replacement: Option<T>,
}

#[derive(Debug)]
struct MapEdit<K, V> {
    key: K,
    replacement: Option<V>,
}

macro_rules! timing_slots {
    ($($name:ident),+ $(,)?) => {
        $(
            #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
            struct $name(RawSlot);

            impl TimingSlot for $name {
                fn from_raw(raw: RawSlot) -> Self {
                    Self(raw)
                }

                fn raw(self) -> RawSlot {
                    self.0
                }
            }
        )+
    };
}

trait TimingSlot: Copy {
    fn from_raw(raw: RawSlot) -> Self;
    fn raw(self) -> RawSlot;
}

timing_slots!(MaxTransitionSlot, MaxCapacitanceSlot, MaxFanoutSlot,);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ClockSlot(RawSlot);

impl TimingSlot for ClockSlot {
    fn from_raw(raw: RawSlot) -> Self {
        Self(raw)
    }

    fn raw(self) -> RawSlot {
        self.0
    }
}

impl ClockSlot {
    pub(crate) fn index(self) -> usize {
        self.0.index()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PathExceptionSlot(RawSlot);

impl TimingSlot for PathExceptionSlot {
    fn from_raw(raw: RawSlot) -> Self {
        Self(raw)
    }

    fn raw(self) -> RawSlot {
        self.0
    }
}

impl PathExceptionSlot {
    pub(crate) fn index(self) -> usize {
        self.0.index()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum TimingReference {
    Clock(ClockSlot),
    ClockSource(ClockSlot),
    GeneratedClockMaster(ClockSlot),
    GeneratedClockSource(ClockSlot),
    InputTransition,
    Load,
    Resistance,
    InputDelay(PortId),
    OutputDelay(PortId),
    ClockUncertainty(ClockId, ClockId),
    CaseAnalysis,
    DisabledTiming(TimingEndpoint),
    PathExceptionFrom(PathExceptionSlot),
    PathExceptionThrough(PathExceptionSlot),
    PathExceptionTo(PathExceptionSlot),
    MaxTransition(MaxTransitionSlot),
    MaxCapacitance(MaxCapacitanceSlot),
    MaxFanout(MaxFanoutSlot),
}

trait DesignRuleSlot: TimingSlot + Ord {
    fn reference(self) -> TimingReference;
}

macro_rules! design_rule_slots {
    ($(($slot:ident, $reference:ident)),+ $(,)?) => {
        $(
            impl DesignRuleSlot for $slot {
                fn reference(self) -> TimingReference {
                    TimingReference::$reference(self)
                }
            }
        )+
    };
}

design_rule_slots!(
    (MaxTransitionSlot, MaxTransition),
    (MaxCapacitanceSlot, MaxCapacitance),
    (MaxFanoutSlot, MaxFanout),
);

impl Default for TimingContext {
    fn default() -> Self {
        Self {
            owner: opto_core::OwnerToken::fresh(),
            revision: RevisionId::INITIAL,
            clocks: OrderedArena::default(),
            input_transitions: BTreeMap::new(),
            loads: BTreeMap::new(),
            resistances: BTreeMap::new(),
            input_delays: BTreeMap::new(),
            output_delays: BTreeMap::new(),
            clock_uncertainties: BTreeMap::new(),
            case_analysis: BTreeMap::new(),
            disabled_timing: BTreeSet::new(),
            timing_derates: TimingDerates::default(),
            path_exceptions: OrderedArena::default(),
            max_transitions: OrderedArena::default(),
            max_capacitances: OrderedArena::default(),
            max_fanouts: OrderedArena::default(),
            clock_slots: BTreeMap::new(),
            references: BTreeMap::new(),
            transactions: Vec::new(),
            journal: Vec::new(),
        }
    }
}

impl Clone for TimingContext {
    fn clone(&self) -> Self {
        Self {
            owner: opto_core::OwnerToken::fresh(),
            revision: self.revision,
            clocks: self.clocks.clone(),
            input_transitions: self.input_transitions.clone(),
            loads: self.loads.clone(),
            resistances: self.resistances.clone(),
            input_delays: self.input_delays.clone(),
            output_delays: self.output_delays.clone(),
            clock_uncertainties: self.clock_uncertainties.clone(),
            case_analysis: self.case_analysis.clone(),
            disabled_timing: self.disabled_timing.clone(),
            timing_derates: self.timing_derates,
            path_exceptions: self.path_exceptions.clone(),
            max_transitions: self.max_transitions.clone(),
            max_capacitances: self.max_capacitances.clone(),
            max_fanouts: self.max_fanouts.clone(),
            clock_slots: self.clock_slots.clone(),
            references: self.references.clone(),
            transactions: Vec::new(),
            journal: Vec::new(),
        }
    }
}

impl TimingContext {
    #[must_use]
    /// Deterministic logical resident size of this context as a standalone value.
    pub fn resident_memory_bytes(&self) -> usize {
        std::mem::size_of::<Self>().saturating_add(self.owned_memory_bytes())
    }

    pub(crate) fn arc_resident_memory_bytes(&self) -> usize {
        opto_core::resident::allocation_bytes(std::mem::size_of::<Self>())
            .saturating_add(self.owned_memory_bytes())
    }

    fn owned_memory_bytes(&self) -> usize {
        let delays = |rows: &BTreeMap<PortId, Vec<IoDelay>>| {
            btree_memory_bytes::<PortId, Vec<IoDelay>>(rows.len()).saturating_add(
                rows.values()
                    .map(|rows| opto_core::resident::slice_bytes::<IoDelay>(rows.len()))
                    .sum::<usize>(),
            )
        };
        let references = btree_memory_bytes::<opto_db::AnyObjectId, BTreeSet<TimingReference>>(
            self.references.len(),
        )
        .saturating_add(
            self.references
                .values()
                .map(|references| btree_set_memory_bytes::<TimingReference>(references.len()))
                .sum::<usize>(),
        );
        opto_core::resident::allocation_bytes(std::mem::size_of::<[usize; 2]>())
            .saturating_add(self.clocks.owned_memory_bytes(clock_nested_memory_bytes))
            .saturating_add(btree_memory_bytes::<PortId, PortValueSlots>(
                self.input_transitions.len(),
            ))
            .saturating_add(btree_memory_bytes::<PortId, PortValueSlots>(
                self.loads.len(),
            ))
            .saturating_add(btree_memory_bytes::<TimingEndpoint, PortValueSlots>(
                self.resistances.len(),
            ))
            .saturating_add(delays(&self.input_delays))
            .saturating_add(delays(&self.output_delays))
            .saturating_add(btree_memory_bytes::<ClockUncertaintyKey, f64>(
                self.clock_uncertainties.len(),
            ))
            .saturating_add(btree_memory_bytes::<TimingEndpoint, CaseAnalysisValue>(
                self.case_analysis.len(),
            ))
            .saturating_add(btree_set_memory_bytes::<DisabledTiming>(
                self.disabled_timing.len(),
            ))
            .saturating_add(
                self.disabled_timing
                    .iter()
                    .map(disabled_timing_nested_memory_bytes)
                    .sum::<usize>(),
            )
            .saturating_add(
                self.path_exceptions
                    .owned_memory_bytes(path_exception_nested_memory_bytes),
            )
            .saturating_add(
                self.max_transitions
                    .owned_memory_bytes(design_rule_nested_memory_bytes),
            )
            .saturating_add(
                self.max_capacitances
                    .owned_memory_bytes(design_rule_nested_memory_bytes),
            )
            .saturating_add(
                self.max_fanouts
                    .owned_memory_bytes(design_rule_nested_memory_bytes),
            )
            .saturating_add(btree_memory_bytes::<ClockId, ClockSlot>(
                self.clock_slots.len(),
            ))
            .saturating_add(references)
            .saturating_add(opto_core::resident::slice_bytes::<
                opto_core::OwnerToken<TimingTransactionOwner>,
            >(self.transactions.len()))
            .saturating_add(self.transactions.len().saturating_mul(
                opto_core::resident::allocation_bytes(std::mem::size_of::<[usize; 2]>()),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<TimingUndo>(
                self.journal.len(),
            ))
            .saturating_add(
                self.journal
                    .iter()
                    .map(timing_undo_nested_memory_bytes)
                    .sum::<usize>(),
            )
    }
}

fn clock_nested_memory_bytes(clock: &Clock) -> usize {
    opto_core::resident::allocation_bytes(clock.name.len())
        .saturating_add(opto_core::resident::allocation_bytes(clock.comment.len()))
        .saturating_add(opto_core::resident::slice_bytes::<PortId>(
            clock.sources.len(),
        ))
        .saturating_add(clock.generated.as_ref().map_or(0, |generated| {
            opto_core::resident::allocation_bytes(generated.comment.len())
        }))
}

fn disabled_timing_nested_memory_bytes(disabled: &DisabledTiming) -> usize {
    disabled
        .from
        .as_ref()
        .map_or(0, |name| opto_core::resident::allocation_bytes(name.len()))
        .saturating_add(
            disabled
                .to
                .as_ref()
                .map_or(0, |name| opto_core::resident::allocation_bytes(name.len())),
        )
}

fn filter_nested_memory_bytes(filter: &ExceptionFilter) -> usize {
    opto_core::resident::slice_bytes::<TimingEndpoint>(filter.objects.len())
}

fn path_exception_nested_memory_bytes(exception: &PathException) -> usize {
    filter_nested_memory_bytes(&exception.from)
        .saturating_add(opto_core::resident::slice_bytes::<ExceptionFilter>(
            exception.through.len(),
        ))
        .saturating_add(
            exception
                .through
                .iter()
                .map(filter_nested_memory_bytes)
                .sum::<usize>(),
        )
        .saturating_add(filter_nested_memory_bytes(&exception.to))
        .saturating_add(opto_core::resident::slice_bytes::<EdgeSelection>(
            exception.edges.through.len(),
        ))
        .saturating_add(opto_core::resident::allocation_bytes(
            exception.comment.len(),
        ))
}

fn design_rule_nested_memory_bytes(constraint: &DesignRuleConstraint) -> usize {
    opto_core::resident::slice_bytes::<TimingObject>(constraint.objects.len())
}

fn timing_undo_nested_memory_bytes(undo: &TimingUndo) -> usize {
    match undo {
        TimingUndo::ClockRemoved(removal) => clock_nested_memory_bytes(removal.value()),
        TimingUndo::ClockReplaced { previous, .. } => clock_nested_memory_bytes(previous),
        TimingUndo::InputDelays { previous, .. } | TimingUndo::OutputDelays { previous, .. } => {
            previous.as_ref().map_or(0, |rows| {
                opto_core::resident::slice_bytes::<IoDelay>(rows.len())
            })
        }
        TimingUndo::DisabledTimingInserted(disabled)
        | TimingUndo::DisabledTimingRemoved(disabled) => {
            disabled_timing_nested_memory_bytes(disabled)
        }
        TimingUndo::PathExceptionRemoved(removal) => {
            path_exception_nested_memory_bytes(removal.value())
        }
        TimingUndo::PathExceptionReplaced { previous, .. } => {
            path_exception_nested_memory_bytes(previous)
        }
        TimingUndo::DesignRuleRemoved { removal, .. } => {
            design_rule_nested_memory_bytes(removal.value())
        }
        TimingUndo::DesignRuleReplaced { previous, .. } => {
            design_rule_nested_memory_bytes(previous)
        }
        _ => 0,
    }
}

fn btree_memory_bytes<K, V>(len: usize) -> usize {
    opto_core::resident::slice_bytes::<(K, V, [usize; 4])>(len)
}

fn btree_set_memory_bytes<T>(len: usize) -> usize {
    opto_core::resident::slice_bytes::<(T, [usize; 4])>(len)
}

impl PartialEq for TimingContext {
    fn eq(&self, other: &Self) -> bool {
        self.revision == other.revision
            && self.clocks == other.clocks
            && self.input_transitions == other.input_transitions
            && self.loads == other.loads
            && self.resistances == other.resistances
            && self.input_delays == other.input_delays
            && self.output_delays == other.output_delays
            && self.clock_uncertainties == other.clock_uncertainties
            && self.case_analysis == other.case_analysis
            && self.disabled_timing == other.disabled_timing
            && self.timing_derates == other.timing_derates
            && self.path_exceptions == other.path_exceptions
            && self.max_transitions == other.max_transitions
            && self.max_capacitances == other.max_capacitances
            && self.max_fanouts == other.max_fanouts
    }
}

#[derive(Serialize)]
struct TimingContextRef<'a> {
    revision: RevisionId,
    clocks: &'a OrderedArena<Clock>,
    input_transitions: &'a BTreeMap<PortId, PortValueSlots>,
    loads: &'a BTreeMap<PortId, PortValueSlots>,
    resistances: &'a BTreeMap<TimingEndpoint, PortValueSlots>,
    input_delays: &'a BTreeMap<PortId, Vec<IoDelay>>,
    output_delays: &'a BTreeMap<PortId, Vec<IoDelay>>,
    clock_uncertainties: &'a BTreeMap<ClockUncertaintyKey, f64>,
    case_analysis: &'a BTreeMap<TimingEndpoint, CaseAnalysisValue>,
    disabled_timing: &'a BTreeSet<DisabledTiming>,
    timing_derates: TimingDerates,
    path_exceptions: &'a OrderedArena<PathException>,
    max_transitions: &'a OrderedArena<DesignRuleConstraint>,
    max_capacitances: &'a OrderedArena<DesignRuleConstraint>,
    max_fanouts: &'a OrderedArena<DesignRuleConstraint>,
}
