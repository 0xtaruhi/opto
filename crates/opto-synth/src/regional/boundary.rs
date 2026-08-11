// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::{RegionBoundaryPort, RegionPortDirection, RegionRevision, SynthesisEffort};
use opto_timing::{ClockId, ScenarioGeneration, ScenarioId, TimingEdge};
use serde::{Deserialize, Deserializer, Serialize};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

const CONTRACT_DOMAIN: &[u8] = b"opto/regional/boundary-contract/v1\0";
const CONTEXT_KEY_DOMAIN: &[u8] = b"opto/regional/context-key/v1\0";
const SEARCH_ABI: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize)]
#[repr(transparent)]
/// Finite floating-point quantity with a deterministic total order.
pub struct FiniteValue(f64);

impl FiniteValue {
    /// Validates a numeric contract or cost value.
    ///
    /// # Errors
    ///
    /// Returns [`BoundaryContractError::NonFiniteValue`] for NaN or infinity.
    pub fn new(value: f64) -> Result<Self, BoundaryContractError> {
        if value.is_finite() {
            Ok(Self(value))
        } else {
            Err(BoundaryContractError::NonFiniteValue)
        }
    }

    #[must_use]
    /// Return the validated finite value without changing its units.
    pub const fn get(self) -> f64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for FiniteValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = f64::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl PartialEq for FiniteValue {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for FiniteValue {}

impl PartialOrd for FiniteValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FiniteValue {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
/// Rise/fall lanes that must never be collapsed into one scalar.
pub struct RiseFall<T> {
    /// Value associated with a rising transition.
    pub rise: T,
    /// Value associated with a falling transition.
    pub fall: T,
}

impl<T> RiseFall<T> {
    #[must_use]
    /// Construct correlated rise and fall lanes.
    pub const fn new(rise: T, fall: T) -> Self {
        Self { rise, fall }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
/// Correlated early/late values for one timing lane.
pub struct EarlyLate<T> {
    /// Minimum-delay corner value.
    pub early: T,
    /// Maximum-delay corner value.
    pub late: T,
}

impl<T> EarlyLate<T> {
    #[must_use]
    /// Construct correlated minimum- and maximum-delay lanes.
    pub const fn new(early: T, late: T) -> Self {
        Self { early, late }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(transparent)]
/// Dense identity of one interned timing-path semantic tag.
pub struct TimingTagId(u32);

impl TimingTagId {
    #[must_use]
    /// Return the dense interner index.
    pub const fn raw(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// Timing or electrical check distinguished at a physical boundary pin.
pub enum BoundaryCheckKind {
    /// Synchronous setup check.
    Setup,
    /// Synchronous hold check.
    Hold,
    /// Asynchronous recovery check.
    Recovery,
    /// Asynchronous removal check.
    Removal,
    /// Minimum high- or low-pulse-width check.
    PulseWidth,
    /// Maximum signal-transition check.
    MaxTransition,
    /// Maximum capacitive-load check.
    MaxCapacitance,
    /// Maximum fanout-load check.
    MaxFanout,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
/// Complete path semantics for a boundary lane.
pub struct TimingTag {
    /// Launch clock, or `None` for a clockless path origin.
    pub launch_clock: Option<ClockId>,
    /// Capture clock, or `None` for a clockless path endpoint.
    pub capture_clock: Option<ClockId>,
    /// Active edge at the path origin.
    pub launch_edge: TimingEdge,
    /// Active edge at the path endpoint.
    pub capture_edge: TimingEdge,
    /// Timing or electrical constraint represented by the lane.
    pub check: BoundaryCheckKind,
    /// Stable path-group name used for constraint and report grouping.
    pub path_group: Arc<str>,
    /// Canonical equivalence class of the applied path exception.
    pub exception_class: u32,
}

#[derive(Debug, Clone, Default)]
/// Canonical interner for sparse timing tags.
pub struct TimingTagInterner {
    tags: Vec<TimingTag>,
    ids: BTreeMap<TimingTag, TimingTagId>,
}

impl TimingTagInterner {
    #[must_use]
    /// Create an empty interner whose first inserted tag receives ID zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the canonical dense ID for `tag`, inserting it when absent.
    ///
    /// # Errors
    ///
    /// Returns [`BoundaryContractError::Capacity`] after the 32-bit ID domain is
    /// exhausted.
    pub fn intern(&mut self, tag: TimingTag) -> Result<TimingTagId, BoundaryContractError> {
        if let Some(&id) = self.ids.get(&tag) {
            return Ok(id);
        }
        let raw = u32::try_from(self.tags.len())
            .map_err(|_| BoundaryContractError::Capacity("timing tag"))?;
        let id = TimingTagId(raw);
        self.tags.push(tag.clone());
        self.ids.insert(tag, id);
        Ok(id)
    }

    #[must_use]
    /// Resolve an ID allocated by this interner.
    pub fn get(&self, id: TimingTagId) -> Option<&TimingTag> {
        self.tags.get(id.raw() as usize)
    }

    #[must_use]
    /// Return interned tags in dense-ID order.
    pub fn tags(&self) -> &[TimingTag] {
        &self.tags
    }
}

type OptionalLane = RiseFall<Option<FiniteValue>>;
type OptionalEarlyLateLane = EarlyLate<OptionalLane>;

/// One MMMC analysis corner.
///
/// The corner decides how a set of samples reduces to its worst case, so every
/// boundary reduction in synthesis states which corner it is reducing for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Corner {
    /// Minimum-delay analysis. The worst arrival is the earliest one.
    Early,
    /// Maximum-delay analysis. The worst arrival is the latest one.
    Late,
}

impl Corner {
    /// Reduces arrival samples to this corner's worst case.
    ///
    /// A boundary port covers several nets. The pessimistic arrival across them
    /// is the latest in a max corner and the *earliest* in a min corner, which
    /// is the same rule the timing engine applies across a net's own tags.
    pub(crate) fn worst_arrival(self, values: impl Iterator<Item = f64>) -> Option<f64> {
        match self {
            Self::Early => values.min_by(f64::total_cmp),
            Self::Late => values.max_by(f64::total_cmp),
        }
    }
}

/// Reduces upper-bounded samples such as transition and load.
///
/// These are design-rule quantities: the conservative value is the maximum in
/// every corner, so this reduction takes no [`Corner`].
pub(crate) fn worst_upper_bound(values: impl Iterator<Item = f64>) -> Option<f64> {
    values.max_by(f64::total_cmp)
}

pub(super) fn early_late_lane(value: Option<FiniteValue>) -> OptionalEarlyLateLane {
    EarlyLate::new(RiseFall::new(value, value), RiseFall::new(value, value))
}

pub(super) fn early_late_timing_lane(
    early: Option<FiniteValue>,
    late: Option<FiniteValue>,
) -> OptionalEarlyLateLane {
    EarlyLate::new(RiseFall::new(early, early), RiseFall::new(late, late))
}

/// Projects a path-timing value onto the lanes one check constrains.
///
/// Both the contract and the measured response go through this function, so a
/// lane is populated on one side exactly when it is populated on the other.
pub(crate) fn path_timing_lane(
    check: BoundaryCheckKind,
    early: Option<FiniteValue>,
    late: Option<FiniteValue>,
) -> OptionalEarlyLateLane {
    match check {
        BoundaryCheckKind::Setup | BoundaryCheckKind::Recovery => {
            early_late_timing_lane(None, late)
        }
        BoundaryCheckKind::Hold | BoundaryCheckKind::Removal => early_late_timing_lane(early, None),
        BoundaryCheckKind::PulseWidth
        | BoundaryCheckKind::MaxTransition
        | BoundaryCheckKind::MaxCapacitance
        | BoundaryCheckKind::MaxFanout => early_late_timing_lane(None, None),
    }
}

/// Projects a single transition limit onto the lanes one check constrains.
pub(super) fn input_transition_lane(
    check: BoundaryCheckKind,
    transition: Option<FiniteValue>,
) -> OptionalEarlyLateLane {
    match check {
        BoundaryCheckKind::PulseWidth | BoundaryCheckKind::MaxTransition => {
            early_late_lane(transition)
        }
        _ => path_timing_lane(check, transition, transition),
    }
}

/// Projects a measured per-corner transition onto the lanes one check
/// constrains, mirroring [`input_transition_lane`].
pub(crate) fn measured_transition_lane(
    check: BoundaryCheckKind,
    early: Option<FiniteValue>,
    late: Option<FiniteValue>,
) -> OptionalEarlyLateLane {
    match check {
        BoundaryCheckKind::PulseWidth | BoundaryCheckKind::MaxTransition => {
            early_late_timing_lane(early, late)
        }
        _ => path_timing_lane(check, early, late),
    }
}

/// Keeps `value` only when this lane's check is `expected`.
pub(super) fn check_value_lane(
    check: BoundaryCheckKind,
    expected: BoundaryCheckKind,
    value: Option<FiniteValue>,
) -> OptionalLane {
    let value = (check == expected).then_some(value).flatten();
    RiseFall::new(value, value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
/// Input-side arrival and transition limits for one sparse path lane.
pub struct BoundaryInputContract {
    /// Arrival time by early/late corner and rise/fall transition.
    pub arrival: OptionalEarlyLateLane,
    /// Input transition by early/late corner and rise/fall transition.
    pub transition: OptionalEarlyLateLane,
    /// Scenario-specific switching activity when power analysis is enabled.
    pub activity: Option<opto_timing::ScenarioSwitchingActivity>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
/// Output-side required times, loads, and electrical maxima.
pub struct BoundaryOutputContract {
    /// Required time by early/late corner and rise/fall transition.
    pub required: OptionalEarlyLateLane,
    /// Effective capacitive load by early/late corner.
    pub capacitance: EarlyLate<Option<FiniteValue>>,
    /// Effective fanout load by early/late corner.
    pub fanout_load: EarlyLate<Option<FiniteValue>>,
    /// Maximum transition constraint by rise/fall transition.
    pub maximum_transition: OptionalLane,
    /// Maximum capacitance constraint by rise/fall transition.
    pub maximum_capacitance: OptionalLane,
    /// Maximum fanout constraint shared by both transitions.
    pub maximum_fanout: Option<FiniteValue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
/// One explicitly present scenario/tag row in a boundary contract.
pub struct BoundaryContractRow {
    /// Analysis scenario owning this sparse row.
    pub scenario: ScenarioId,
    /// Interned path semantics for this row.
    pub timing_tag: TimingTagId,
    /// Input-side data; present only for input boundary ports.
    pub input: Option<BoundaryInputContract>,
    /// Output-side data; present only for output boundary ports.
    pub output: Option<BoundaryOutputContract>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
/// Semantic generation of one immutable boundary contract.
pub struct ContractGeneration([u8; 32]);

impl ContractGeneration {
    #[must_use]
    /// Return the canonical digest of the sealed contract.
    pub const fn bytes(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Immutable per-epoch contract for one typed region port.
pub struct BoundaryContract {
    port: RegionBoundaryPort,
    epoch: u32,
    scenario_generation: ScenarioGeneration,
    rows: Arc<[BoundaryContractRow]>,
    generation: ContractGeneration,
}

impl BoundaryContract {
    /// Validates sparse row identity, direction, and ordering before sealing.
    ///
    /// # Errors
    ///
    /// Returns [`BoundaryContractError`] for duplicate scenario/tag rows or for
    /// row payloads that do not match the boundary-port direction.
    pub fn new(
        port: RegionBoundaryPort,
        epoch: u32,
        scenario_generation: ScenarioGeneration,
        mut rows: Vec<BoundaryContractRow>,
    ) -> Result<Self, BoundaryContractError> {
        rows.sort_by_key(|row| (row.scenario, row.timing_tag));
        for pair in rows.windows(2) {
            if (pair[0].scenario, pair[0].timing_tag) == (pair[1].scenario, pair[1].timing_tag) {
                return Err(BoundaryContractError::DuplicateRow {
                    scenario: pair[0].scenario,
                    tag: pair[0].timing_tag,
                });
            }
        }
        for row in &rows {
            let shape_is_valid = match port.direction() {
                RegionPortDirection::Input => row.input.is_some() && row.output.is_none(),
                RegionPortDirection::Output => row.input.is_none() && row.output.is_some(),
            };
            if !shape_is_valid {
                return Err(BoundaryContractError::DirectionMismatch);
            }
        }
        let generation = seal_contract(port, epoch, scenario_generation, &rows);
        Ok(Self {
            port,
            epoch,
            scenario_generation,
            rows: rows.into(),
            generation,
        })
    }

    #[must_use]
    /// Return the typed region port constrained by this contract.
    pub const fn port(&self) -> RegionBoundaryPort {
        self.port
    }

    #[must_use]
    /// Return the optimization epoch in which this contract was created.
    pub const fn epoch(&self) -> u32 {
        self.epoch
    }

    #[must_use]
    /// Return the scenario-set generation used to derive the rows.
    pub const fn scenario_generation(&self) -> ScenarioGeneration {
        self.scenario_generation
    }

    #[must_use]
    /// Return sparse rows sorted by `(scenario, timing_tag)`.
    pub fn rows(&self) -> &[BoundaryContractRow] {
        &self.rows
    }

    #[must_use]
    /// Return the content digest used by regional context keys.
    pub const fn generation(&self) -> ContractGeneration {
        self.generation
    }
}

fn seal_contract(
    port: RegionBoundaryPort,
    epoch: u32,
    scenarios: ScenarioGeneration,
    rows: &[BoundaryContractRow],
) -> ContractGeneration {
    let mut digest = blake3::Hasher::new();
    digest.update(CONTRACT_DOMAIN);
    digest.update(&port.semantic_key());
    digest.update(&[port.direction() as u8]);
    digest.update(&epoch.to_le_bytes());
    digest.update(&scenarios.bytes());
    digest.update(&(rows.len() as u64).to_le_bytes());
    for row in rows {
        digest.update(&row.scenario.raw().to_le_bytes());
        digest.update(&row.timing_tag.raw().to_le_bytes());
        hash_input(&mut digest, row.input);
        hash_output(&mut digest, row.output);
    }
    ContractGeneration(*digest.finalize().as_bytes())
}

fn hash_input(digest: &mut blake3::Hasher, input: Option<BoundaryInputContract>) {
    digest.update(&[u8::from(input.is_some())]);
    if let Some(input) = input {
        hash_early_late_lane(digest, input.arrival);
        hash_early_late_lane(digest, input.transition);
        match input.activity {
            Some(activity) => {
                digest.update(&[1]);
                digest.update(&activity.static_probability().to_bits().to_le_bytes());
                digest.update(&activity.toggle_rate().to_bits().to_le_bytes());
                digest.update(&activity.rise_ratio().to_bits().to_le_bytes());
            }
            None => {
                digest.update(&[0]);
            }
        }
    }
}

fn hash_output(digest: &mut blake3::Hasher, output: Option<BoundaryOutputContract>) {
    digest.update(&[u8::from(output.is_some())]);
    if let Some(output) = output {
        hash_early_late_lane(digest, output.required);
        hash_optional(digest, output.capacitance.early);
        hash_optional(digest, output.capacitance.late);
        hash_optional(digest, output.fanout_load.early);
        hash_optional(digest, output.fanout_load.late);
        hash_lane(digest, output.maximum_transition);
        hash_lane(digest, output.maximum_capacitance);
        hash_value(digest, output.maximum_fanout);
    }
}

fn hash_early_late_lane(digest: &mut blake3::Hasher, values: OptionalEarlyLateLane) {
    hash_lane(digest, values.early);
    hash_lane(digest, values.late);
}

fn hash_lane(digest: &mut blake3::Hasher, values: OptionalLane) {
    hash_value(digest, values.rise);
    hash_value(digest, values.fall);
}

fn hash_optional(digest: &mut blake3::Hasher, value: Option<FiniteValue>) {
    hash_value(digest, value);
}

fn hash_value(digest: &mut blake3::Hasher, value: Option<FiniteValue>) {
    match value {
        Some(value) => {
            digest.update(&[1]);
            digest.update(&value.get().to_bits().to_le_bytes());
        }
        None => {
            digest.update(&[0]);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
/// Complete context identity for timing/power evaluation of one local plan.
pub struct RegionContextKey([u8; 32]);

impl RegionContextKey {
    /// Seals all context that can change regional plan response or selection.
    #[must_use]
    pub fn seal(
        local: RegionRevision,
        contracts: &[BoundaryContract],
        scenarios: ScenarioGeneration,
        target_fingerprint: [u8; 32],
        effort: SynthesisEffort,
        predecessor_summaries: &[[u8; 32]],
    ) -> Self {
        let mut digest = blake3::Hasher::new();
        digest.update(CONTEXT_KEY_DOMAIN);
        digest.update(&SEARCH_ABI.to_le_bytes());
        digest.update(&local.bytes());
        digest.update(&scenarios.bytes());
        digest.update(&target_fingerprint);
        digest.update(&[match effort {
            SynthesisEffort::Low => 0,
            SynthesisEffort::Medium => 1,
            SynthesisEffort::High => 2,
        }]);
        digest.update(&(contracts.len() as u64).to_le_bytes());
        for contract in contracts {
            digest.update(&contract.generation().bytes());
        }
        digest.update(&(predecessor_summaries.len() as u64).to_le_bytes());
        for summary in predecessor_summaries {
            digest.update(summary);
        }
        Self(*digest.finalize().as_bytes())
    }

    #[must_use]
    /// Return the canonical digest of the complete regional search context.
    pub const fn bytes(self) -> [u8; 32] {
        self.0
    }

    #[cfg(test)]
    pub(crate) const fn from_bytes_for_test(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Invalid boundary contract or numeric lane.
pub enum BoundaryContractError {
    /// A timing, load, power, or cost lane contains NaN or infinity.
    NonFiniteValue,
    /// A row carries input data for an output port or vice versa.
    DirectionMismatch,
    /// Two sparse rows use the same scenario and timing tag.
    DuplicateRow {
        /// Scenario shared by both rows.
        scenario: ScenarioId,
        /// Interned timing tag shared by both rows.
        tag: TimingTagId,
    },
    /// A named dense table exceeded its 32-bit index domain.
    Capacity(&'static str),
}

impl fmt::Display for BoundaryContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFiniteValue => formatter.write_str("boundary value is not finite"),
            Self::DirectionMismatch => {
                formatter.write_str("boundary row does not match its port direction")
            }
            Self::DuplicateRow { scenario, tag } => write!(
                formatter,
                "boundary row ({}, {}) is duplicated",
                scenario.raw(),
                tag.raw()
            ),
            Self::Capacity(resource) => write!(formatter, "{resource} exceeds 32-bit capacity"),
        }
    }
}

impl std::error::Error for BoundaryContractError {}

pub(super) fn synthesis_error(error: &BoundaryContractError) -> crate::SynthError {
    crate::SynthError::invalid(error.to_string())
}
