// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Typed constraint objects, selections and path exceptions.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
/// Endpoint class accepted by path constraints.
pub enum TimingEndpoint {
    /// Design port endpoint.
    Port(PortId),
    /// Cell-instance endpoint.
    Cell(CellId),
    /// Instance-pin endpoint.
    Pin(PinId),
    /// Logical-net endpoint.
    Net(NetId),
    /// Clock-domain endpoint.
    Clock(ClockId),
}

impl TimingEndpoint {
    #[must_use]
    /// Returns the type-erased persistent database identity.
    pub const fn object_id(self) -> opto_db::AnyObjectId {
        match self {
            Self::Port(id) => opto_db::AnyObjectId::Port(id),
            Self::Cell(id) => opto_db::AnyObjectId::Cell(id),
            Self::Pin(id) => opto_db::AnyObjectId::Pin(id),
            Self::Net(id) => opto_db::AnyObjectId::Net(id),
            Self::Clock(id) => opto_db::AnyObjectId::Clock(id),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Canonical object set used by one path-exception point.
///
/// An empty set is unrestricted. Nonempty sets are sorted and deduplicated by
/// [`Self::new`] so exception rows and propagated tag keys have deterministic
/// identities.
pub struct ExceptionFilter {
    pub(super) objects: Box<[TimingEndpoint]>,
}

impl ExceptionFilter {
    #[must_use]
    /// Creates an unrestricted point filter.
    pub fn unrestricted() -> Self {
        Self {
            objects: Vec::new().into_boxed_slice(),
        }
    }

    #[must_use]
    /// Creates a sorted, deduplicated filter.
    pub fn new(objects: impl IntoIterator<Item = TimingEndpoint>) -> Self {
        let mut objects = objects.into_iter().collect::<Vec<_>>();
        objects.sort_unstable();
        objects.dedup();
        Self {
            objects: objects.into_boxed_slice(),
        }
    }

    #[must_use]
    /// Returns the canonical selected endpoints.
    pub fn objects(&self) -> &[TimingEndpoint] {
        &self.objects
    }

    #[must_use]
    /// Returns whether the filter accepts every endpoint.
    pub fn is_unrestricted(&self) -> bool {
        self.objects.is_empty()
    }

    pub(crate) fn matches_any(&self, points: &[TimingEndpoint]) -> bool {
        self.is_unrestricted()
            || points
                .iter()
                .any(|point| self.objects.binary_search(point).is_ok())
    }

    pub(crate) fn contains_class(&self, predicate: impl Fn(TimingEndpoint) -> bool) -> bool {
        self.objects.iter().copied().any(predicate)
    }
}

impl Default for ExceptionFilter {
    fn default() -> Self {
        Self::unrestricted()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
/// Everything `set_input_delay` and `set_output_delay` state about one delay.
///
/// The SDC options travel together and are validated together, so they arrive
/// as one value instead of a dozen positional parameters.
pub struct IoDelaySpec {
    /// Whether the delay constrains inputs or outputs.
    pub kind: IoDelayKind,
    /// Delay in the timing library's time unit.
    pub delay: f64,
    /// Reference clock, or `None` for an unclocked delay.
    pub clock: Option<ClockId>,
    /// Reference clock edge.
    pub clock_edge: TimingEdge,
    /// Data edges the delay applies to.
    pub edges: EdgeSelection,
    /// Analysis corners the delay applies to.
    pub corners: CornerSelection,
    /// Whether the value already includes clock source latency.
    pub source_latency_included: bool,
    /// Whether the value already includes clock network latency.
    pub network_latency_included: bool,
    /// Whether to add to the existing rows instead of replacing them.
    pub add_delay: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
/// Minimum/maximum qualification shared by SDC value constraints.
///
/// SDC treats "neither flag given" and "both flags given" as the same
/// unqualified selection, so the flag pair collapses to one value here instead
/// of travelling separately through every command signature.
pub enum CornerSelection {
    /// Both analysis corners.
    #[default]
    Both,
    /// Minimum-delay analysis only.
    Min,
    /// Maximum-delay analysis only.
    Max,
}

impl CornerSelection {
    /// Build a selection from the `-min` and `-max` flags.
    #[must_use]
    pub const fn from_flags(min: bool, max: bool) -> Self {
        match (min, max) {
            (true, false) => Self::Min,
            (false, true) => Self::Max,
            _ => Self::Both,
        }
    }

    /// Whether this selection covers `delay_type`.
    #[must_use]
    pub const fn matches(self, delay_type: DelayType) -> bool {
        matches!(
            (self, delay_type),
            (Self::Both, _) | (Self::Min, DelayType::Min) | (Self::Max, DelayType::Max)
        )
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
/// Early/late qualification used by clock latency and derating.
pub enum LatencySide {
    /// Both the early and the late side.
    #[default]
    Both,
    /// Early side only.
    Early,
    /// Late side only.
    Late,
}

impl LatencySide {
    /// Build a selection from the `-early` and `-late` flags.
    #[must_use]
    pub const fn from_flags(early: bool, late: bool) -> Self {
        match (early, late) {
            (true, false) => Self::Early,
            (false, true) => Self::Late,
            _ => Self::Both,
        }
    }

    /// Whether this selection covers the early (`0`) or late (`1`) side.
    #[must_use]
    pub const fn covers(self, index: usize) -> bool {
        matches!(
            (self, index),
            (Self::Both, _) | (Self::Early, 0) | (Self::Late, 1)
        )
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
/// Rise/fall subset accepted at one exception point.
pub enum EdgeSelection {
    /// Both transition directions.
    #[default]
    Both,
    /// Rising transitions only.
    Rise,
    /// Falling transitions only.
    Fall,
}

impl EdgeSelection {
    /// Build a selection from the `-rise` and `-fall` flags.
    #[must_use]
    pub const fn from_flags(rise: bool, fall: bool) -> Self {
        match (rise, fall) {
            (true, false) => Self::Rise,
            (false, true) => Self::Fall,
            _ => Self::Both,
        }
    }

    pub(crate) const fn matches(self, edge: TimingEdge) -> bool {
        matches!(
            (self, edge),
            (Self::Both, _) | (Self::Rise, TimingEdge::Rise) | (Self::Fall, TimingEdge::Fall)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Edge qualifications attached to exception points and the final path edge.
pub struct EdgeQualifier {
    /// Start-point edge selection.
    pub from: EdgeSelection,
    /// Ordered through-point edge selections.
    pub through: Box<[EdgeSelection]>,
    /// End-point object edge selection.
    pub to: EdgeSelection,
    /// Final path transition selection.
    pub end: EdgeSelection,
}

impl EdgeQualifier {
    #[must_use]
    /// Creates a complete set of edge qualifiers.
    pub fn new(
        from: EdgeSelection,
        through: impl IntoIterator<Item = EdgeSelection>,
        to: EdgeSelection,
        end: EdgeSelection,
    ) -> Self {
        Self {
            from,
            through: through.into_iter().collect(),
            to,
            end,
        }
    }
}

impl Default for EdgeQualifier {
    fn default() -> Self {
        Self {
            from: EdgeSelection::Both,
            through: Vec::new().into_boxed_slice(),
            to: EdgeSelection::Both,
            end: EdgeSelection::Both,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
/// Timing-check corner selected by an exception command.
pub enum ExceptionCorner {
    /// Both setup and hold analyses.
    #[default]
    Both,
    /// Setup analysis only.
    Setup,
    /// Hold analysis only.
    Hold,
}

impl ExceptionCorner {
    pub(crate) const fn matches(self, delay_type: DelayType) -> bool {
        matches!(
            (self, delay_type),
            (Self::Both, _) | (Self::Setup, DelayType::Max) | (Self::Hold, DelayType::Min)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Effect applied after a path exception wins endpoint arbitration.
pub enum PathExceptionKind {
    /// Exclude matching paths from analysis.
    FalsePath,
    /// Change the permitted clock-cycle count.
    MultiCycle {
        /// Positive cycle multiplier.
        cycles: u32,
        /// Whether the multiplier is relative to the endpoint clock.
        use_end_clock: bool,
    },
    /// Override maximum path delay.
    MaxDelay {
        /// Maximum delay value.
        delay: f64,
    },
    /// Override minimum path delay.
    MinDelay {
        /// Minimum delay value.
        delay: f64,
    },
}

impl PathExceptionKind {
    #[must_use]
    /// Returns the human-readable exception kind.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::FalsePath => "false path",
            Self::MultiCycle { .. } => "multicycle path",
            Self::MaxDelay { .. } => "maximum delay",
            Self::MinDelay { .. } => "minimum delay",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// One false-path, multicycle, or path-delay exception.
pub struct PathException {
    /// Exception effect.
    pub kind: PathExceptionKind,
    /// Start-point filter.
    pub from: ExceptionFilter,
    /// Ordered through-point filters.
    pub through: Box<[ExceptionFilter]>,
    /// End-point filter.
    pub to: ExceptionFilter,
    /// Edge qualifications.
    pub edges: EdgeQualifier,
    /// Selected analysis corner.
    pub corner: ExceptionCorner,
    /// Whether clock latency is excluded from path delay.
    pub ignore_clock_latency: bool,
    /// Optional user comment.
    pub comment: String,
}

impl PathException {
    #[must_use]
    /// Returns whether the exception has no point or edge restriction.
    pub fn is_unrestricted(&self) -> bool {
        self.from.is_unrestricted()
            && self.through.is_empty()
            && self.to.is_unrestricted()
            && self.edges == EdgeQualifier::default()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Maximum transition, capacitance, or fanout constraint.
pub struct DesignRuleConstraint {
    /// Maximum permitted value.
    pub limit: f64,
    /// Explicit target objects; empty selects the active design.
    pub objects: Box<[TimingObject]>,
    /// Whether the rule applies to data paths, clock paths, or both.
    pub scope: DesignRuleScope,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
/// Typed database object accepted by timing constraint commands.
pub enum TimingObject {
    /// Design target.
    Design(DesignId),
    /// Port target with its owning design and signal direction.
    Port {
        /// Persistent port identity.
        id: PortId,
        /// Owning design identity.
        design: DesignId,
        /// Timing signal-flow direction.
        direction: TimingPortDirection,
    },
    /// Clock-object target.
    Clock(ClockId),
    /// Cell-instance target.
    Cell(CellId),
    /// Instance-pin target.
    Pin(PinId),
    /// Logical-net target.
    Net(NetId),
}

impl TimingObject {
    #[must_use]
    /// Returns the type-erased persistent database identity.
    pub const fn object_id(&self) -> opto_db::AnyObjectId {
        match self {
            Self::Design(id) => opto_db::AnyObjectId::Design(*id),
            Self::Port { id, .. } => opto_db::AnyObjectId::Port(*id),
            Self::Clock(id) => opto_db::AnyObjectId::Clock(*id),
            Self::Cell(id) => opto_db::AnyObjectId::Cell(*id),
            Self::Pin(id) => opto_db::AnyObjectId::Pin(*id),
            Self::Net(id) => opto_db::AnyObjectId::Net(*id),
        }
    }

    #[must_use]
    /// Constructs a design timing object.
    pub const fn design(id: DesignId) -> Self {
        Self::Design(id)
    }

    #[must_use]
    /// Constructs a port timing object with ownership metadata.
    pub fn port(id: PortId, design: DesignId, direction: TimingPortDirection) -> Self {
        Self::Port {
            id,
            design,
            direction,
        }
    }

    #[must_use]
    /// Constructs a clock timing object.
    pub const fn clock(id: ClockId) -> Self {
        Self::Clock(id)
    }

    #[must_use]
    /// Constructs a cell timing object.
    pub const fn cell(id: CellId) -> Self {
        Self::Cell(id)
    }

    #[must_use]
    /// Constructs a pin timing object.
    pub const fn pin(id: PinId) -> Self {
        Self::Pin(id)
    }

    #[must_use]
    /// Constructs a net timing object.
    pub const fn net(id: NetId) -> Self {
        Self::Net(id)
    }

    #[must_use]
    /// Returns the target class and port direction, if applicable.
    pub const fn kind(&self) -> TimingObjectKind {
        match self {
            Self::Design(_) => TimingObjectKind::Design,
            Self::Port { direction, .. } => TimingObjectKind::Port(*direction),
            Self::Clock(_) => TimingObjectKind::Clock,
            Self::Cell(_) => TimingObjectKind::Cell,
            Self::Pin(_) => TimingObjectKind::Pin,
            Self::Net(_) => TimingObjectKind::Net,
        }
    }

    #[must_use]
    /// Returns the owning design for design and port targets.
    pub const fn design_id(&self) -> Option<DesignId> {
        match self {
            Self::Design(id) => Some(*id),
            Self::Port { design, .. } => Some(*design),
            Self::Clock(_) | Self::Cell(_) | Self::Pin(_) | Self::Net(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
/// Runtime class of a [`TimingObject`].
pub enum TimingObjectKind {
    /// Design object.
    Design,
    /// Port with its timing direction.
    Port(TimingPortDirection),
    /// Clock object.
    Clock,
    /// Cell object.
    Cell,
    /// Pin object.
    Pin,
    /// Net object.
    Net,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
/// Path subset to which a design-rule constraint applies.
pub enum DesignRuleScope {
    /// Both clock and data paths.
    All,
    /// Data paths only.
    DataPath,
    /// Clock paths only.
    ClockPath,
    /// Explicitly both clock and data paths.
    ClockAndData,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
/// Kind of electrical design-rule limit.
pub enum DesignRuleKind {
    /// Maximum signal transition time.
    MaxTransition,
    /// Maximum capacitive load.
    MaxCapacitance,
    /// Maximum abstract fanout load.
    MaxFanout,
}

#[derive(Debug, Clone, PartialEq)]
/// One measured design-rule violation.
pub struct DesignRuleViolation {
    /// Violated rule kind.
    pub kind: DesignRuleKind,
    /// Graph-local timing-net identity.
    pub net: TimingNetId,
    /// Corresponding mapped-net identity, when available.
    pub mapped_net: Option<MappedNetId>,
    /// User-visible object name.
    pub object: String,
    /// Measured value.
    pub actual: f64,
    /// Active constraint limit.
    pub limit: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
/// Aggregate electrical design-rule quality metrics.
pub struct DesignRuleSummary {
    worst_ratio: f64,
    total_excess: f64,
    violations: usize,
}

impl DesignRuleSummary {
    /// Builds an aggregate electrical summary from canonical MMMC reductions.
    #[must_use]
    pub const fn aggregate(worst_ratio: f64, total_excess: f64, violations: usize) -> Self {
        Self {
            worst_ratio,
            total_excess,
            violations,
        }
    }

    pub(crate) const fn new(worst_ratio: f64, total_excess: f64, violations: usize) -> Self {
        Self {
            worst_ratio,
            total_excess,
            violations,
        }
    }

    #[must_use]
    /// Returns the largest `actual / limit` ratio.
    pub const fn worst_ratio(self) -> f64 {
        self.worst_ratio
    }

    #[must_use]
    /// Returns the sum of positive `actual - limit` excesses.
    pub const fn total_excess(self) -> f64 {
        self.total_excess
    }

    #[must_use]
    /// Returns the number of violating measurements.
    pub const fn violations(self) -> usize {
        self.violations
    }
}

#[cfg(test)]
mod checkpoint_tests {
    use super::*;

    #[test]
    fn raw_checkpoint_decodes_before_explicit_index_restore() {
        let mut timing = TimingContext::default();
        timing
            .create_clock(
                crate::test_clock_id(42),
                ClockSpec::new("clk", 10.0, vec![crate::test_port_id("clk")], None).unwrap(),
            )
            .unwrap();
        timing
            .set_input_transition(0.2, &[crate::test_port_id("data")])
            .unwrap();

        let checkpoint = TimingContextCheckpoint::from(&timing);
        let context_wire = opto_archive::to_bytes(&timing).unwrap();
        let checkpoint_wire = opto_archive::to_bytes(&checkpoint).unwrap();
        assert_eq!(checkpoint_wire, context_wire);

        let decoded: TimingContextCheckpoint = opto_archive::from_bytes(&checkpoint_wire).unwrap();
        assert_eq!(decoded.restore().unwrap(), timing);
    }
}
