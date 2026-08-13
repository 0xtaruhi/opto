// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use opto_core::{Diagnostic, DiagnosticSource, RevisionId};
use opto_db::ClockId;
use opto_ir::mapped::{NetId as MappedNetId, PinId as MappedPinId};
use thiserror::Error;

#[derive(Debug, Error)]
/// Top-level failure from a timing operation.
pub enum TimingError {
    /// Constraint validation or mutation failed.
    #[error(transparent)]
    Constraint(#[from] ConstraintError),
    /// Timing-graph construction or update failed.
    #[error(transparent)]
    Model(#[from] TimingModelError),
    /// Static timing analysis failed.
    #[error(transparent)]
    Analysis(#[from] TimingAnalysisError),
    /// Incremental engine bookkeeping failed.
    #[error(transparent)]
    Engine(#[from] TimingEngineError),
    /// Parallel runtime execution failed.
    #[error("timing: parallel runtime error: {0}")]
    Runtime(#[from] opto_runtime::RuntimeError),
    /// The timing-context revision counter is exhausted.
    #[error("timing: revision space exhausted")]
    Revision(#[source] opto_core::RevisionExhausted),
    /// The mapped netlist violates its own invariants.
    #[error("timing: mapped netlist error: {0}")]
    Mapped(#[source] opto_ir::mapped::MappedError),
    /// A previous panic poisoned shared engine state.
    #[error("timing engine state is poisoned")]
    EnginePoisoned,
    /// A prepared removal belongs to another timing context.
    #[error("timing: prepared object removal belongs to another context")]
    ObjectRemovalOwnerMismatch,
    /// A prepared removal targets an older context revision.
    #[error(
        "timing: prepared object removal revision {prepared:?} does not match current revision {current:?}"
    )]
    StaleObjectRemoval {
        /// Revision captured during preparation.
        prepared: RevisionId,
        /// Current context revision.
        current: RevisionId,
    },
    /// A checkpoint belongs to another timing context.
    #[error("timing: checkpoint belongs to another context")]
    CheckpointOwnerMismatch,
    /// A checkpoint has already been consumed or superseded.
    #[error("timing: checkpoint is stale or no longer active")]
    StaleCheckpoint,
    /// Both an operation and its compensating rollback failed.
    #[error("{operation} failed: {primary}; rollback also failed: {rollback}")]
    Rollback {
        /// Operation being rolled back.
        operation: &'static str,
        /// Original operation failure.
        #[source]
        primary: Box<TimingError>,
        /// Failure encountered during rollback.
        rollback: Box<TimingError>,
    },
}

impl DiagnosticSource for TimingError {
    fn diagnostic(&self) -> Option<Diagnostic> {
        let (code, internal) = match self {
            Self::Constraint(_) => ("OPT-TIM-001", false),
            Self::Model(_) => ("OPT-TIM-100", false),
            Self::Analysis(_) => ("OPT-TIM-200", false),
            Self::Runtime(_) => ("OPT-TIM-500", false),
            Self::Engine(_)
            | Self::Revision(_)
            | Self::Mapped(_)
            | Self::EnginePoisoned
            | Self::ObjectRemovalOwnerMismatch
            | Self::StaleObjectRemoval { .. }
            | Self::CheckpointOwnerMismatch
            | Self::StaleCheckpoint
            | Self::Rollback { .. } => ("OPT-TIM-900", true),
        };
        let mut diagnostic = Diagnostic::new(code, self.to_string());
        if internal {
            diagnostic = diagnostic.with_help(
                "retain the timing inputs and diagnostic code when reporting this internal consistency failure",
            );
        }
        Some(diagnostic)
    }
}

#[derive(Debug, Error)]
/// Invalid constraint input or checkpoint state.
pub enum ConstraintError {
    /// A serialized or in-memory checkpoint violates an invariant.
    #[error("timing context checkpoint is invalid: {detail}")]
    InvalidCheckpoint {
        /// Checkpoint invariant that was violated.
        detail: String,
    },
    /// A clock name is empty.
    #[error("create_clock: clock name cannot be empty")]
    EmptyClockName,
    /// A clock period is non-finite or nonpositive.
    #[error("create_clock: invalid period '{period}'")]
    InvalidClockPeriod {
        /// Rejected period in timing-library time units.
        period: f64,
    },
    /// Clock waveform edges are invalid.
    #[error("create_clock: invalid waveform '{{{rise} {fall}}}'")]
    InvalidClockWaveform {
        /// Requested rising-edge offset in timing-library time units.
        rise: f64,
        /// Requested falling-edge offset in timing-library time units.
        fall: f64,
    },
    /// The falling edge lies outside the clock period.
    #[error("create_clock: waveform fall edge {fall} exceeds period {period}")]
    ClockWaveformExceedsPeriod {
        /// Falling-edge offset in timing-library time units.
        fall: f64,
        /// Clock period in timing-library time units.
        period: f64,
    },
    /// A clock ID is not live.
    #[error("set_clock_transition: clock ID '{id:?}' not found")]
    ClockNotFound {
        /// Persistent clock ID that was not live in the context.
        id: ClockId,
    },
    /// Early/late applies only to source clock latency.
    #[error("set_clock_latency: -early/-late is only allowed with -source")]
    InvalidClockLatencySelection,
    /// Clock groups need at least two nonempty groups.
    #[error("set_clock_groups: at least two nonempty -group options are required")]
    InvalidClockGroups,
    /// Generated-clock transform options are inconsistent or invalid.
    #[error("create_generated_clock: invalid divide/multiply/edge transform options")]
    InvalidGeneratedClockOptions,
    /// Case analysis accepts only port and pin objects.
    #[error("set_case_analysis: only ports and pins are supported")]
    InvalidCaseAnalysisObject,
    /// Removing path exceptions requires at least one point restriction.
    #[error("unset_path_exceptions: -from, -through, or -to is required")]
    UnrestrictedPathExceptionRemoval,
    /// Timing derates must select exactly one early/late analysis side.
    #[error("set_timing_derate: exactly one of -early or -late is required")]
    InvalidTimingDerateSelection,
    /// SDC serialization encountered an object no longer present in the database.
    #[error("write_sdc: object '{object}' is not live")]
    UnresolvedSdcObject {
        /// Stable serialized locator of the missing object.
        object: String,
    },
    /// A maximum- or minimum-delay value is invalid.
    #[error("{command}: invalid delay '{delay}'")]
    InvalidPathDelay {
        /// SDC command validating the delay.
        command: &'static str,
        /// Rejected non-finite delay in timing-library time units.
        delay: f64,
    },
    /// A multicycle multiplier must be positive.
    #[error("set_multicycle_path: multiplier must be positive, got '{cycles}'")]
    InvalidMulticycle {
        /// Rejected zero cycle multiplier.
        cycles: u32,
    },
    /// A path exception that would select every path is not accepted.
    #[error("{command}: at least one -from, -through, or -to point is required")]
    UnrestrictedPathException {
        /// SDC command that would otherwise match every path.
        command: &'static str,
    },
    /// An ordered through point cannot be empty.
    #[error("{command}: -through resolved to an empty object set")]
    EmptyThroughFilter {
        /// SDC command containing the empty ordered point.
        command: &'static str,
    },
    /// The through-edge qualifiers must align with the ordered through points.
    #[error(
        "{command}: through qualifier count {edges} does not match through point count {filters}"
    )]
    ThroughEdgeCountMismatch {
        /// SDC command containing inconsistent qualifiers.
        command: &'static str,
        /// Number of ordered through-point filters.
        filters: usize,
        /// Number of through-edge qualifiers.
        edges: usize,
    },
    /// A compact propagated progress counter cannot represent the through list.
    #[error("{command}: too many ordered -through points ({count})")]
    TooManyThroughFilters {
        /// SDC command containing the oversized list.
        command: &'static str,
        /// Number of ordered through points.
        count: usize,
    },
    /// A path-specific option was applied to non-clock objects.
    #[error("{command}: -data_path/-clock_path requires clock objects")]
    ClockPathRequiresClockObjects {
        /// SDC command whose selection contained non-clock objects.
        command: &'static str,
    },
    /// A constraint command received a non-finite or otherwise invalid value.
    #[error("{command}: invalid value '{value}'")]
    InvalidValue {
        /// SDC command validating the numeric value.
        command: &'static str,
        /// Rejected numeric value.
        value: f64,
    },
    /// A constraint command requires at least one target.
    #[error("{command}: no objects specified")]
    NoObjects {
        /// SDC command invoked without a target collection.
        command: &'static str,
    },
}

#[derive(Debug, Error)]
/// Invalid mapped design or incremental timing-model update.
pub enum TimingModelError {
    /// A follower library would change the prepared graph structure.
    #[error("timing: library topology schema is incompatible with the prepared view")]
    IncompatibleTopologySchema,
    /// A mapped edit was paired with a different mapped-netlist owner.
    #[error("timing: mapped region edit belongs to another netlist generation")]
    ForeignMappedRegionEdit,
    /// An incremental region delta contains no changes.
    #[error("timing: region delta requires at least one update")]
    EmptyRegionDelta,
    /// A delta removes an instance that is not live.
    #[error("timing: region delta removes unknown instance {id}")]
    UnknownRemovedInstance {
        /// Stable instance ID absent from the live timing design.
        id: u32,
    },
    /// A replacement record disagrees with its map key.
    #[error("timing: region delta key {expected} disagrees with replacement ID {actual}")]
    ReplacementIdMismatch {
        /// ID named by the delta key.
        expected: u32,
        /// ID stored in the replacement.
        actual: u32,
    },
    /// Two mapped instances have the same stable ID.
    #[error("timing: design contains duplicate instance ID {id}")]
    DuplicateInstanceId {
        /// Stable instance ID claimed by more than one record.
        id: u32,
    },
    /// Two persistent objects of one class claim the same flat name.
    #[error("timing: duplicate persistent {kind} binding for one flat name")]
    DuplicateObjectBinding {
        /// Persistent object class whose flat-name binding collided.
        kind: &'static str,
    },
    /// Rollback metadata names an invalid instance position.
    #[error("timing: rollback instance position {position} exceeds design length {design_len}")]
    RollbackPositionOutOfBounds {
        /// Sparse instance position recorded by the rollback journal.
        position: usize,
        /// Current dense design length.
        design_len: usize,
    },
    /// A mapped net has no corresponding timing-graph net.
    #[error("timing: mapped net {mapped:?} has no graph net '{name}'")]
    MappedNetMissingGraphNet {
        /// Live mapped-net identity.
        mapped: MappedNetId,
        /// Expected timing-graph net name.
        name: String,
    },
    /// Multiple mapped nets alias one graph-net name.
    #[error("timing: graph net '{name}' aliases mapped nets {first:?} and {second:?}")]
    MappedNetAlias {
        /// Timing-graph net name claimed by both mapped nets.
        name: String,
        /// First mapped-net claimant.
        first: MappedNetId,
        /// Second mapped-net claimant.
        second: MappedNetId,
    },
    /// Rollback metadata refers to a missing graph-net ID.
    #[error("timing: rollback lost net ID {id}")]
    RollbackMissingNet {
        /// Graph-local net ID recorded by the rollback journal.
        id: u32,
    },
    /// A mapped port lacks a name.
    #[error("timing: mapped port {index} has no name")]
    MappedPortMissingName {
        /// Zero-based mapped-port position.
        index: usize,
    },
    /// Port records and typed bindings have different lengths.
    #[error("timing: mapped design declares {ports} ports but has {bindings} typed port bindings")]
    MappedPortBindingCount {
        /// Number of mapped port records.
        ports: usize,
        /// Number of typed port bindings.
        bindings: usize,
    },
    /// A mapped port lacks a persistent object ID.
    #[error("timing: mapped port '{name}' has no typed object ID")]
    MappedPortMissingObject {
        /// User-visible mapped port name.
        name: String,
    },
    /// A mapped port contains an invalid net range.
    #[error("timing: mapped port {index} has invalid net range")]
    MappedPortInvalidNetRange {
        /// Zero-based mapped-port position.
        index: usize,
    },
    /// A mapped cell lacks a name.
    #[error("timing: mapped cell {index} has no name")]
    MappedCellMissingName {
        /// Zero-based mapped-cell arena position.
        index: usize,
    },
    /// A mapped cell contains an invalid pin range.
    #[error("timing: mapped cell {index} has invalid pin range")]
    MappedCellInvalidPinRange {
        /// Zero-based mapped-cell arena position.
        index: usize,
    },
    /// A mapped cell contains an unnamed pin.
    #[error("timing: mapped cell {index} has unnamed pin")]
    MappedCellUnnamedPin {
        /// Zero-based mapped-cell arena position.
        index: usize,
    },
    /// A mapped cell lacks a target-cell type.
    #[error("timing: mapped cell {index} has no type")]
    MappedCellMissingType {
        /// Zero-based mapped-cell arena position.
        index: usize,
    },
    /// Mapped hierarchy metadata is inconsistent.
    #[error("timing: invalid mapped hierarchy: {detail}")]
    InvalidMappedHierarchy {
        /// Hierarchy invariant that was violated.
        detail: String,
    },
    /// A compact timing-graph resource exceeds 32-bit capacity.
    #[error("timing: {resource} exceeds 32-bit capacity")]
    Capacity {
        /// Compact arena or adjacency structure that overflowed.
        resource: &'static str,
    },
    /// A delta writes the same instance more than once.
    #[error("timing: region delta writes instance {id} more than once")]
    DuplicateInstanceUpdate {
        /// Stable instance ID written more than once by the delta.
        id: u32,
    },
    /// A delta writes the same mapped net more than once.
    #[error("timing: region delta writes mapped net {net:?} more than once")]
    DuplicateMappedNetUpdate {
        /// Mapped net written more than once by the delta.
        net: MappedNetId,
    },
    /// A live mapped net lacks an adjacency record.
    #[error("timing: live mapped net {net:?} has no adjacency record")]
    MissingNetAdjacency {
        /// Live mapped net lacking its connectivity row.
        net: MappedNetId,
    },
    /// A net references a pin with no owning instance or port.
    #[error("timing: mapped net {net:?} references ownerless pin {pin:?}")]
    OwnerlessPin {
        /// Mapped net containing the invalid adjacency.
        net: MappedNetId,
        /// Pin not owned by a live mapped cell or port.
        pin: MappedPinId,
    },
    /// An instance references no loaded target cell.
    #[error("timing: instance '{instance}' references unknown cell '{cell}'")]
    UnknownCell {
        /// Mapped instance referencing the cell.
        instance: String,
        /// Unresolved target-cell name.
        cell: String,
    },
    /// An instance cell name resolves to multiple link libraries.
    #[error(
        "timing: instance '{instance}' references ambiguous cell '{cell}' in the active library set"
    )]
    AmbiguousCell {
        /// Mapped instance referencing the cell.
        instance: String,
        /// Target-cell name supplied by multiple selected libraries.
        cell: String,
    },
    /// A latch output lacks an enable-to-Q opening arc.
    #[error(
        "timing: latch cell '{cell}' output '{output}' has no {edge}-edge enable-to-Q timing arc"
    )]
    MissingLatchOpeningArc {
        /// Target latch cell type.
        cell: String,
        /// Latch output pin lacking the arc.
        output: String,
        /// Required active enable edge.
        edge: &'static str,
    },
    /// A latch instance does not connect its enable pin.
    #[error("timing: latch instance '{instance}' has no connection for enable pin '{pin}'")]
    MissingLatchEnableConnection {
        /// Mapped latch instance.
        instance: String,
        /// Required enable pin name.
        pin: String,
    },
    /// An incremental region refers to an unknown net.
    #[error("timing: unknown region net '{name}'")]
    UnknownRegionNet {
        /// Net name absent from the timing design.
        name: String,
    },
    /// A parasitic net violates topology or numeric invariants.
    #[error("read_parasitics: net '{net}': {detail}")]
    InvalidParasiticNet {
        /// Parasitic network name.
        net: String,
        /// Topology or numeric invariant that was violated.
        detail: String,
    },
}

#[derive(Debug, Error)]
/// Failure while propagating, ordering, or reporting timing paths.
pub enum TimingAnalysisError {
    /// A timing report requested no paths.
    #[error("report_timing: max_paths must be greater than zero")]
    InvalidMaxPaths,
    /// No Liberty propagation arcs exist in the analyzed design.
    #[error("report_timing: no Liberty timing arcs found")]
    NoLibertyTimingArcs,
    /// A timing-cell index is outside the graph arena.
    #[error("report_timing: unknown timing cell index {index}")]
    UnknownTimingCell {
        /// Dense timing-cell index outside the analyzed model.
        index: usize,
    },
    /// Topological ordering is inconsistent after cycle detection.
    #[error("report_timing: graph topology is inconsistent after cycle detection")]
    InconsistentTopology,
    /// Combinational propagation contains a cycle.
    #[error("report_timing: combinational loop detected at net '{net}': {path}")]
    CombinationalLoop {
        /// Net at which the cycle was detected.
        net: String,
        /// Net names along one cycle, ending back at its first net.
        path: String,
    },
    /// Transparent latch propagation contains a cycle.
    #[error("report_timing: cyclic latch transparency path detected at net '{net}'")]
    LatchTransparencyLoop {
        /// Net at which the cycle was detected.
        net: String,
    },
    /// A proposed buffer would introduce a combinational cycle.
    #[error("timing: buffer insertion creates a combinational loop")]
    BufferInsertionLoop,
    /// Analysis produced no reportable timing paths.
    #[error("report_timing: no timing paths found")]
    NoTimingPaths,
    /// Incremental dirty-state refers to an invalid net index.
    #[error("timing: dirty net index {index} is out of range")]
    DirtyNetOutOfRange {
        /// Dense net index outside the incremental dirty-state arena.
        index: usize,
    },
    /// A compact analysis arena exceeds 32-bit capacity.
    #[error("timing: {resource} exceeds 32-bit capacity")]
    Capacity {
        /// Analysis arena or packed relation that overflowed.
        resource: &'static str,
    },
    /// A path operation requires at least one point.
    #[error("timing: cannot {operation} an empty path")]
    EmptyPath {
        /// Path operation requiring at least one timing point.
        operation: &'static str,
    },
    /// A path predecessor ID is unknown.
    #[error("timing: unknown path predecessor {id}")]
    UnknownPathPredecessor {
        /// Graph-local predecessor ID absent from the path arena.
        id: u32,
    },
    /// An arrival-tag ID is unknown.
    #[error("timing: unknown arrival tag {id}")]
    UnknownArrivalTag {
        /// Interned arrival-tag ID absent from the analysis state.
        id: u32,
    },
    /// An arrival-origin ID is unknown.
    #[error("timing: unknown arrival origin {id}")]
    UnknownArrivalOrigin {
        /// Interned arrival-origin ID absent from the analysis state.
        id: u32,
    },
    /// The graph refers to a missing primary input.
    #[error("timing: graph references unknown primary input {index}")]
    UnknownPrimaryInput {
        /// Dense primary-input position referenced by the graph.
        index: usize,
    },
    /// The graph refers to a missing instance.
    #[error("timing: graph references unknown instance {index}")]
    UnknownInstance {
        /// Dense instance position referenced by the graph.
        index: usize,
    },
    /// A clock-to-Q arc locator is invalid.
    #[error("timing: graph references unknown clock-to-Q arc {instance}:{pin}:{arc}")]
    UnknownClockToQArc {
        /// Dense instance position.
        instance: usize,
        /// Pin position within the target cell.
        pin: usize,
        /// Timing-arc position within the pin.
        arc: usize,
    },
    /// A sequential graph edge refers to a non-clock-to-Q arc.
    #[error("timing: sequential graph references a non-clock-to-Q arc")]
    NonClockToQArc,
    /// A general timing-arc locator is invalid.
    #[error("timing: graph references unknown arc {instance}:{pin}:{arc}")]
    UnknownArc {
        /// Dense instance position.
        instance: usize,
        /// Pin position within the target cell.
        pin: usize,
        /// Timing-arc position within the pin.
        arc: usize,
    },
    /// An arrival tag refers to a missing path exception.
    #[error("timing: arrival tag references unknown path exception {index}")]
    UnknownPathException {
        /// Stable insertion index of the missing exception row.
        index: u32,
    },
    /// A propagated exception progress state was not interned at its launch.
    #[error("timing: path-exception tag transition was not pre-interned")]
    UnknownArrivalTagTransition,
}

#[derive(Debug, Error)]
/// Failure in incremental timing-engine bookkeeping.
pub enum TimingEngineError {
    /// An operation would cross a live speculative region transaction.
    #[error("timing: cannot {operation} while a region edit is active")]
    ActiveRegionEdit {
        /// Operation rejected while speculative state is live.
        operation: &'static str,
    },
    /// An operation requires a live speculative region transaction.
    #[error("timing: cannot {operation} without an active region edit")]
    NoActiveRegionEdit {
        /// Operation requiring an active speculative edit.
        operation: &'static str,
    },
    /// A monotonic engine metric exceeded its integer capacity.
    #[error("timing {metric} metric overflow")]
    MetricOverflow {
        /// Monotonic counter that exceeded its integer representation.
        metric: &'static str,
    },
}
