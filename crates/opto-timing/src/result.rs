// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    PathExceptionKind, TargetPinDirection, TimingEdge, TimingGeneration, TimingInstanceId,
    TimingNetId,
};
use std::ops::Deref;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Filters and formatting controls for a timing report.
pub struct ReportTimingOptions {
    /// Startpoint name filters.
    pub from: Vec<String>,
    /// Endpoint name filters.
    pub to: Vec<String>,
    /// Maximum-delay or minimum-delay analysis.
    pub delay_type: DelayType,
    /// Explicit timing and electrical checks enabled in this analysis view.
    pub checks: crate::ScenarioCheckSet,
    /// Decimal significant digits used in text output.
    pub significant_digits: usize,
}

impl Default for ReportTimingOptions {
    fn default() -> Self {
        Self {
            from: Vec::new(),
            to: Vec::new(),
            delay_type: DelayType::Max,
            checks: crate::ScenarioCheckSet::ALL,
            significant_digits: 3,
        }
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
)]
#[repr(u8)]
/// Analysis polarity for setup-like or hold-like propagation.
pub enum DelayType {
    /// Latest-arrival, setup-oriented analysis.
    #[default]
    Max,
    /// Earliest-arrival, hold-oriented analysis.
    Min,
}

impl DelayType {
    pub(crate) fn index(self) -> usize {
        self as usize
    }

    /// Return the stable report spelling.
    #[must_use]
    pub fn report_name(self) -> &'static str {
        match self {
            Self::Max => "max",
            Self::Min => "min",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
/// Timing and electrical state at one graph net.
pub struct NetTimingState {
    /// Timing-graph net identity.
    pub id: TimingNetId,
    /// User-visible net name.
    pub name: String,
    /// Worst propagated arrival in the timing library's time unit.
    pub arrival: Option<f64>,
    /// Tightest required time in the timing library's time unit.
    pub required: Option<f64>,
    /// Derived slack in the timing library's time unit.
    pub slack: Option<f64>,
    /// Propagated transition in the timing library's time unit.
    pub transition: Option<f64>,
    /// Total load in the timing library's capacitance unit.
    pub capacitance: f64,
    /// Total dimensionless abstract fanout load.
    pub fanout: f64,
}

#[derive(Debug, Clone, PartialEq)]
/// Generation-stamped snapshot of every net's timing state.
pub struct TimingNetStates {
    generation: TimingGeneration,
    rows: Box<[NetTimingState]>,
}

impl TimingNetStates {
    pub(crate) fn new(generation: TimingGeneration, rows: Vec<NetTimingState>) -> Self {
        Self {
            generation,
            rows: rows.into_boxed_slice(),
        }
    }

    #[must_use]
    /// Returns the model generation for which these rows are valid.
    pub fn generation(&self) -> TimingGeneration {
        self.generation
    }
}

impl Deref for TimingNetStates {
    type Target = [NetTimingState];

    fn deref(&self) -> &Self::Target {
        &self.rows
    }
}

impl IntoIterator for TimingNetStates {
    type Item = NetTimingState;
    type IntoIter = std::vec::IntoIter<NetTimingState>;

    fn into_iter(self) -> Self::IntoIter {
        self.rows.into_vec().into_iter()
    }
}

impl<'a> IntoIterator for &'a TimingNetStates {
    type Item = &'a NetTimingState;
    type IntoIter = std::slice::Iter<'a, NetTimingState>;

    fn into_iter(self) -> Self::IntoIter {
        self.rows.iter()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
/// Power-facing electrical values for one dense timing net.
pub struct TimingElectricalState {
    /// Total load in the timing library's capacitance unit.
    pub capacitance: f64,
    /// Worst propagated transition in the timing library's time unit.
    pub transition: Option<f64>,
}

#[derive(Debug)]
struct TimingElectricalData {
    generation: TimingGeneration,
    constraint_revision: opto_core::RevisionId,
    delay_type: DelayType,
    capacitances: Box<[f64]>,
    transitions: Box<[f64]>,
    transition_validity: Box<[u64]>,
}

#[derive(Debug, Clone)]
/// Immutable compact electrical snapshot consumed by power analysis.
///
/// The snapshot deliberately excludes report names, arrivals, required times,
/// and slack. Names remain owned by [`crate::TimingModel`], while the two
/// electrical columns use dense timing-net IDs. Cloning this value only clones
/// an [`Arc`]; [`Self::is_same_snapshot`] is therefore an O(1) cache key.
pub struct TimingElectricalSnapshot {
    data: Arc<TimingElectricalData>,
}

impl TimingElectricalSnapshot {
    pub(crate) fn try_from_dense(
        generation: TimingGeneration,
        constraint_revision: opto_core::RevisionId,
        delay_type: DelayType,
        net_count: usize,
        mut state: impl FnMut(usize) -> TimingElectricalState,
    ) -> Result<Self, crate::TimingError> {
        if net_count > u32::MAX as usize {
            return Err(crate::TimingAnalysisError::Capacity {
                resource: "electrical snapshot net index",
            }
            .into());
        }
        let validity_words =
            net_count
                .checked_add(63)
                .ok_or(crate::TimingAnalysisError::Capacity {
                    resource: "electrical snapshot validity index",
                })?
                / 64;
        let mut capacitances = Vec::new();
        let mut transitions = Vec::new();
        let mut transition_validity = Vec::new();
        capacitances.try_reserve_exact(net_count).map_err(|_| {
            crate::TimingAnalysisError::Capacity {
                resource: "electrical snapshot capacitance column",
            }
        })?;
        transitions.try_reserve_exact(net_count).map_err(|_| {
            crate::TimingAnalysisError::Capacity {
                resource: "electrical snapshot transition column",
            }
        })?;
        transition_validity
            .try_reserve_exact(validity_words)
            .map_err(|_| crate::TimingAnalysisError::Capacity {
                resource: "electrical snapshot transition validity",
            })?;
        transition_validity.resize(validity_words, 0u64);
        for net in 0..net_count {
            let electrical = state(net);
            capacitances.push(electrical.capacitance);
            transitions.push(electrical.transition.unwrap_or(0.0));
            if electrical.transition.is_some() {
                transition_validity[net / 64] |= 1u64 << (net % 64);
            }
        }
        Ok(Self {
            data: Arc::new(TimingElectricalData {
                generation,
                constraint_revision,
                delay_type,
                capacitances: capacitances.into_boxed_slice(),
                transitions: transitions.into_boxed_slice(),
                transition_validity: transition_validity.into_boxed_slice(),
            }),
        })
    }

    #[must_use]
    /// Returns whether two handles name the exact same immutable snapshot.
    pub fn is_same_snapshot(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.data, &other.data)
    }

    #[must_use]
    /// Returns the sealed model generation consumed by this snapshot.
    pub fn generation(&self) -> TimingGeneration {
        self.data.generation
    }

    #[must_use]
    /// Returns the constraint revision consumed by propagation.
    pub fn constraint_revision(&self) -> opto_core::RevisionId {
        self.data.constraint_revision
    }

    #[must_use]
    /// Returns the analysis polarity used for transition propagation.
    pub fn delay_type(&self) -> DelayType {
        self.data.delay_type
    }

    #[must_use]
    /// Number of dense timing-net rows.
    pub fn len(&self) -> usize {
        self.data.capacitances.len()
    }

    #[must_use]
    /// Returns whether no net state is stored.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[must_use]
    /// Returns compact electrical values for a dense timing-net ID.
    pub fn get(&self, net: TimingNetId) -> Option<TimingElectricalState> {
        let row = net.index();
        let &capacitance = self.data.capacitances.get(row)?;
        let transition = self
            .data
            .transition_validity
            .get(row / 64)
            .is_some_and(|word| word & (1u64 << (row % 64)) != 0)
            .then(|| self.data.transitions[row]);
        Some(TimingElectricalState {
            capacitance,
            transition,
        })
    }

    #[must_use]
    /// Resident bytes owned by the shared electrical columns.
    pub fn resident_memory_bytes(&self) -> usize {
        std::mem::size_of::<TimingElectricalData>()
            .saturating_add(opto_core::resident::slice_bytes::<f64>(
                self.data.capacitances.len(),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<f64>(
                self.data.transitions.len(),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<u64>(
                self.data.transition_validity.len(),
            ))
    }
}

#[derive(Debug, Clone, PartialEq)]
/// Timing and electrical state at one instance pin.
pub struct PinTimingState {
    /// Owning timing-instance identity.
    pub instance: TimingInstanceId,
    /// Owning instance name.
    pub name: String,
    /// Library pin name.
    pub pin: String,
    /// Connected net name.
    pub net: String,
    /// Pin signal-flow direction.
    pub direction: TargetPinDirection,
    /// Worst propagated arrival in the timing library's time unit.
    pub arrival: Option<f64>,
    /// Propagated transition in the timing library's time unit.
    pub transition: Option<f64>,
    /// Input or effective receiver load in the library's capacitance unit.
    pub capacitance: f64,
    /// Dimensionless abstract fanout contribution.
    pub fanout_load: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
/// Local delay impact estimated for a candidate cell replacement.
pub struct CellTimingEstimate {
    /// Estimated propagation delay in the timing library's time unit.
    pub delay: f64,
    /// Estimated output transition in the timing library's time unit.
    pub transition: f64,
    /// Replacement input load in the timing library's capacitance unit.
    pub input_capacitance: f64,
}

#[derive(Debug, Clone)]
pub(crate) struct Arrival {
    pub(crate) startpoint: String,
    pub(crate) startpoint_description: String,
    pub(crate) delay: f64,
    pub(crate) steps: Vec<PathStep>,
}

#[derive(Debug, Clone)]
pub(crate) struct LaunchClock {
    pub(crate) edge_time: f64,
    pub(crate) source_latency: f64,
}

#[derive(Debug, Clone)]
/// One point on a reconstructed timing path.
pub struct PathStep {
    pub(crate) point: String,
    pub(crate) incr: f64,
    pub(crate) path: f64,
    pub(crate) edge: TimingEdge,
    pub(crate) instance: Option<TimingInstanceId>,
    pub(crate) kind: PathStepKind,
    pub(crate) interconnect: Option<InterconnectPathContribution>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
/// Numeric terms used for one interconnect path increment.
pub struct InterconnectPathContribution {
    pub(crate) net: TimingNetId,
    pub(crate) fanout: f64,
    pub(crate) load: f64,
    pub(crate) resistance: f64,
    pub(crate) parasitic_delay: f64,
    pub(crate) derate: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Semantic source of one timing-path increment.
pub enum PathStepKind {
    /// Clock edge, source latency, or clock-network contribution.
    Clock,
    /// External input delay.
    InputDelay,
    /// A zero-delay port or cell-pin observation point.
    Point,
    /// Interconnect contribution from extracted parasitics or wire-load
    /// estimation.
    Interconnect,
    /// Characterized library cell timing arc.
    CellArc,
    /// A timing-check quantity such as pulse width.
    TimingCheck,
}

impl PathStep {
    /// Returns the reported timing point.
    #[must_use]
    pub fn point(&self) -> &str {
        &self.point
    }

    /// Returns incremental delay at this step.
    #[must_use]
    pub fn increment(&self) -> f64 {
        self.incr
    }

    /// Returns cumulative path delay.
    #[must_use]
    pub fn path(&self) -> f64 {
        self.path
    }

    /// Returns the transition edge.
    #[must_use]
    pub fn edge(&self) -> TimingEdge {
        self.edge
    }

    #[must_use]
    /// Returns the step category.
    pub const fn kind(&self) -> PathStepKind {
        self.kind
    }

    #[must_use]
    /// Returns the owning instance, when applicable.
    pub const fn instance(&self) -> Option<TimingInstanceId> {
        self.instance
    }

    #[must_use]
    /// Returns detailed interconnect contribution, when applicable.
    pub const fn interconnect(&self) -> Option<InterconnectPathContribution> {
        self.interconnect
    }
}

impl InterconnectPathContribution {
    #[must_use]
    /// Returns the timing-net identity.
    pub const fn net(self) -> TimingNetId {
        self.net
    }

    #[must_use]
    /// Returns abstract fanout load.
    pub const fn fanout(self) -> f64 {
        self.fanout
    }

    #[must_use]
    /// Returns capacitive load.
    pub const fn load(self) -> f64 {
        self.load
    }

    #[must_use]
    /// Returns effective resistance.
    pub const fn resistance(self) -> f64 {
        self.resistance
    }

    #[must_use]
    /// Returns raw parasitic delay.
    pub const fn parasitic_delay(self) -> f64 {
        self.parasitic_delay
    }

    #[must_use]
    /// Returns the applied delay derate.
    pub const fn derate(self) -> f64 {
        self.derate
    }

    #[must_use]
    /// Returns the unscaled RC wire delay.
    pub fn wire_delay(self) -> f64 {
        self.resistance * self.load
    }
}

impl PathStepKind {
    #[must_use]
    /// Returns the report category name.
    pub const fn report_name(self) -> &'static str {
        match self {
            Self::Clock => "clock",
            Self::InputDelay => "input delay",
            Self::Point => "point",
            Self::Interconnect => "interconnect",
            Self::CellArc => "cell arc",
            Self::TimingCheck => "timing check",
        }
    }
}

#[derive(Debug, Clone)]
/// One fully reconstructed timing path.
pub struct TimingAnalysis {
    pub(crate) design: String,
    pub(crate) library: TimingLibraryMetadata,
    pub(crate) delay_type: DelayType,
    pub(crate) endpoint_edge: TimingEdge,
    pub(crate) arrival: Arrival,
    pub(crate) endpoint: String,
    pub(crate) endpoint_object: String,
    pub(crate) endpoint_description: String,
    pub(crate) path_group: Option<String>,
    pub(crate) required: Option<f64>,
    pub(crate) requirement: Option<TimingRequirement>,
    pub(crate) path_exception: Option<TimingPathException>,
    pub(crate) time_borrowed: Option<f64>,
    pub(crate) significant_digits: usize,
}

#[derive(Debug, Clone)]
/// Detailed worst-path and endpoint-slack quality for one model generation.
pub struct TimingQuality {
    generation: TimingGeneration,
    worst: TimingAnalysis,
    path_count: usize,
    wns: Option<f64>,
    tns: f64,
    violating_paths: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
/// Allocation-free aggregate timing quality used by optimization loops.
pub struct TimingQualitySummary {
    arrival: f64,
    wns: Option<f64>,
    tns: f64,
    violating_paths: usize,
}

impl TimingQualitySummary {
    /// Builds a deterministic aggregate from independently analyzed timing views.
    ///
    /// This is intended for explicit sparse MMMC coordinators: arrival and
    /// worst slack retain the worst view, while negative slack and violation
    /// counts are accumulated in the caller's canonical view order.
    #[must_use]
    pub const fn aggregate(
        arrival: f64,
        wns: Option<f64>,
        tns: f64,
        violating_paths: usize,
    ) -> Self {
        Self {
            arrival,
            wns,
            tns,
            violating_paths,
        }
    }

    #[must_use]
    /// Returns the worst-path arrival in the timing library's time unit.
    pub fn arrival(self) -> f64 {
        self.arrival
    }

    #[must_use]
    /// Returns worst slack in the timing library's time unit.
    ///
    /// Returns `None` when no endpoint is constrained.
    pub fn wns(self) -> Option<f64> {
        self.wns
    }

    #[must_use]
    /// Returns total negative slack in the timing library's time unit.
    pub fn tns(self) -> f64 {
        self.tns
    }

    #[must_use]
    /// Returns the number of endpoints with negative slack.
    pub fn violating_paths(self) -> usize {
        self.violating_paths
    }
}

impl TimingQuality {
    pub(crate) fn from_endpoint_slacks(
        generation: TimingGeneration,
        analyses: Vec<TimingAnalysis>,
        endpoint_slacks: impl IntoIterator<Item = f64>,
    ) -> Result<Self, crate::TimingError> {
        let mut wns = None;
        let mut tns = 0.0;
        let mut path_count = 0usize;
        let mut violating_paths = 0usize;
        for slack in endpoint_slacks {
            path_count = path_count
                .checked_add(1)
                .ok_or(crate::TimingAnalysisError::Capacity {
                    resource: "timing path count",
                })?;
            wns = Some(wns.map_or(slack, |current: f64| current.min(slack)));
            if slack < 0.0 {
                tns += slack;
                violating_paths += 1;
            }
        }
        Ok(Self {
            generation,
            worst: crate::analysis::worst_analysis(analyses)?,
            path_count,
            wns,
            tns,
            violating_paths,
        })
    }

    #[must_use]
    /// Returns the analyzed model generation.
    pub const fn generation(&self) -> TimingGeneration {
        self.generation
    }

    pub(crate) fn into_worst(self) -> TimingAnalysis {
        self.worst
    }

    #[must_use]
    /// Returns worst slack in the timing library's time unit.
    ///
    /// Returns `None` when no endpoint is constrained.
    pub fn wns(&self) -> Option<f64> {
        self.wns
    }

    #[must_use]
    /// Returns the number of analyzed endpoint paths.
    pub fn path_count(&self) -> usize {
        self.path_count
    }

    #[must_use]
    /// Returns total negative slack in the timing library's time unit.
    pub fn tns(&self) -> f64 {
        self.tns
    }

    #[must_use]
    /// Returns the number of endpoints with negative slack.
    pub fn violating_paths(&self) -> usize {
        self.violating_paths
    }

    #[must_use]
    /// Returns the worst-path arrival in the timing library's time unit.
    pub fn arrival(&self) -> f64 {
        self.worst.arrival()
    }
}

#[derive(Debug, Clone)]
/// Constraint defining the required time of a timing path.
pub enum TimingRequirement {
    /// Explicit maximum path delay.
    MaxDelay,
    /// Explicit minimum path delay.
    MinDelay,
    /// Clock-relative output delay.
    OutputDelay,
    /// Setup check against a capture-clock edge.
    Setup {
        /// Capture clock name.
        clock: String,
        /// Capture edge polarity.
        clock_edge: TimingEdge,
        /// Absolute capture-edge time.
        capture_edge_time: f64,
        /// Capture-clock network delay.
        clock_network_delay: f64,
        /// Capture-clock endpoint.
        clock_point: String,
        /// Sequential cell type.
        cell: String,
        /// Library setup constraint.
        constraint: f64,
    },
    /// Hold check against a capture-clock edge.
    Hold {
        /// Capture clock name.
        clock: String,
        /// Capture edge polarity.
        clock_edge: TimingEdge,
        /// Absolute capture-edge time.
        capture_edge_time: f64,
        /// Capture-clock network delay.
        clock_network_delay: f64,
        /// Capture-clock endpoint.
        clock_point: String,
        /// Sequential cell type.
        cell: String,
        /// Library hold constraint.
        constraint: f64,
    },
    /// Recovery check on an asynchronous control against a clock edge.
    Recovery {
        /// Capture clock name.
        clock: String,
        /// Capture edge polarity.
        clock_edge: TimingEdge,
        /// Absolute capture-edge time.
        capture_edge_time: f64,
        /// Capture-clock network delay.
        clock_network_delay: f64,
        /// Capture-clock endpoint.
        clock_point: String,
        /// Sequential cell type.
        cell: String,
        /// Library recovery constraint.
        constraint: f64,
    },
    /// Removal check on an asynchronous control against a clock edge.
    Removal {
        /// Capture clock name.
        clock: String,
        /// Capture edge polarity.
        clock_edge: TimingEdge,
        /// Absolute capture-edge time.
        capture_edge_time: f64,
        /// Capture-clock network delay.
        clock_network_delay: f64,
        /// Capture-clock endpoint.
        clock_point: String,
        /// Sequential cell type.
        cell: String,
        /// Library removal constraint.
        constraint: f64,
    },
    /// Minimum high or low pulse-width check on a clock pin.
    PulseWidth {
        /// Clock name.
        clock: String,
        /// Edge starting the checked pulse.
        pulse_edge: TimingEdge,
        /// Clock-pin endpoint.
        clock_point: String,
        /// Sequential cell type.
        cell: String,
        /// Library minimum pulse-width constraint.
        constraint: f64,
    },
}

#[derive(Debug, Clone)]
/// Winning path exception selected for a reported endpoint path.
pub struct TimingPathException {
    pub(crate) index: u32,
    pub(crate) kind: PathExceptionKind,
    pub(crate) priority: u16,
    pub(crate) comment: String,
}

#[derive(Debug, Clone)]
/// Library metadata associated with a reconstructed path.
pub struct TimingLibraryMetadata {
    pub(crate) name: Option<String>,
    pub(crate) operating_conditions: Option<String>,
    pub(crate) wire_load: Option<String>,
    pub(crate) wire_load_mode: Option<String>,
}

#[derive(Debug, Clone)]
/// Structured diagnostics produced by `check_timing`.
pub struct CheckTimingAnalysis {
    pub(crate) no_clocks: bool,
    pub(crate) missing_input_delays: Vec<String>,
    pub(crate) unconstrained_endpoints: Vec<String>,
}

/// One clock row with source names resolved at the session boundary.
#[derive(Debug, Clone, PartialEq)]
pub struct ClockReportRow {
    /// Clock name.
    pub name: String,
    /// Clock period.
    pub period: f64,
    /// Optional rise/fall waveform.
    pub waveform: Option<(f64, f64)>,
    /// Resolved source object names.
    pub sources: Vec<String>,
}

impl TimingAnalysis {
    /// Returns the analyzed design name.
    #[must_use]
    pub fn design(&self) -> &str {
        &self.design
    }

    /// Returns selected library metadata.
    #[must_use]
    pub fn library(&self) -> &TimingLibraryMetadata {
        &self.library
    }

    /// Returns the minimum- or maximum-delay analysis kind.
    #[must_use]
    pub fn delay_type(&self) -> DelayType {
        self.delay_type
    }

    /// Returns the endpoint transition edge.
    #[must_use]
    pub fn endpoint_edge(&self) -> TimingEdge {
        self.endpoint_edge
    }

    /// Returns the startpoint name.
    #[must_use]
    pub fn startpoint(&self) -> &str {
        &self.arrival.startpoint
    }

    /// Returns the startpoint description.
    #[must_use]
    pub fn startpoint_description(&self) -> &str {
        &self.arrival.startpoint_description
    }

    /// Returns the endpoint name.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Returns the endpoint object name.
    #[must_use]
    pub fn endpoint_object(&self) -> &str {
        &self.endpoint_object
    }

    /// Returns the endpoint description.
    #[must_use]
    pub fn endpoint_description(&self) -> &str {
        &self.endpoint_description
    }

    /// Returns the path-group name, when assigned.
    #[must_use]
    pub fn path_group(&self) -> Option<&str> {
        self.path_group.as_deref()
    }

    /// Returns path steps in propagation order.
    #[must_use]
    pub fn steps(&self) -> &[PathStep] {
        &self.arrival.steps
    }

    /// Returns endpoint arrival in the timing library's time unit.
    #[must_use]
    pub fn arrival(&self) -> f64 {
        self.arrival.delay
    }

    /// Returns required time in the timing library's time unit, when constrained.
    #[must_use]
    pub fn required(&self) -> Option<f64> {
        self.required
    }

    /// Returns setup- or hold-oriented slack in the timing library's time unit.
    ///
    /// Returns `None` when the endpoint is unconstrained.
    #[must_use]
    pub fn slack(&self) -> Option<f64> {
        self.required.map(|required| match self.delay_type {
            DelayType::Max => required - self.arrival.delay,
            DelayType::Min => self.arrival.delay - required,
        })
    }

    /// Returns time borrowed through a transparent latch in the timing
    /// library's time unit, when applicable.
    #[must_use]
    pub fn time_borrowed(&self) -> Option<f64> {
        self.time_borrowed
    }

    /// Returns the endpoint timing requirement.
    #[must_use]
    pub fn requirement(&self) -> Option<&TimingRequirement> {
        self.requirement.as_ref()
    }

    /// Returns the winning path exception, when any.
    #[must_use]
    pub fn path_exception(&self) -> Option<&TimingPathException> {
        self.path_exception.as_ref()
    }

    /// Returns the recommended report precision.
    #[must_use]
    pub fn significant_digits(&self) -> usize {
        self.significant_digits
    }

    /// Iterates over instances traversed by the path.
    pub fn path_instances(&self) -> impl Iterator<Item = TimingInstanceId> + '_ {
        self.arrival.steps.iter().filter_map(|step| step.instance)
    }
}

impl TimingPathException {
    #[must_use]
    /// Returns the exception insertion index.
    pub fn index(&self) -> u32 {
        self.index
    }

    #[must_use]
    /// Returns the exception effect.
    pub fn kind(&self) -> &PathExceptionKind {
        &self.kind
    }

    #[must_use]
    /// Returns the arbitration priority.
    pub fn priority(&self) -> u16 {
        self.priority
    }

    #[must_use]
    /// Returns the user comment.
    pub fn comment(&self) -> &str {
        &self.comment
    }
}

impl TimingLibraryMetadata {
    /// Returns the selected library name.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Returns the selected operating conditions.
    #[must_use]
    pub fn operating_conditions(&self) -> Option<&str> {
        self.operating_conditions.as_deref()
    }

    /// Returns the selected wire-load model name.
    #[must_use]
    pub fn wire_load(&self) -> Option<&str> {
        self.wire_load.as_deref()
    }

    /// Returns the selected wire-load mode.
    #[must_use]
    pub fn wire_load_mode(&self) -> Option<&str> {
        self.wire_load_mode.as_deref()
    }
}

impl CheckTimingAnalysis {
    /// Returns whether the design has no clocks.
    #[must_use]
    pub fn no_clocks(&self) -> bool {
        self.no_clocks
    }

    /// Returns input ports missing delay constraints.
    #[must_use]
    pub fn missing_input_delays(&self) -> &[String] {
        &self.missing_input_delays
    }

    /// Returns endpoints without timing requirements.
    #[must_use]
    pub fn unconstrained_endpoints(&self) -> &[String] {
        &self.unconstrained_endpoints
    }
}
