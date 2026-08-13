// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Explicit sparse multi-mode, multi-corner synthesis inputs.
//!
//! A scenario is an intentional binding, not one cell in an implicit
//! mode-by-corner Cartesian product. The set owns a stable semantic generation
//! so regional caches and boundary contracts never key themselves from session
//! revision numbers or arena positions.

use crate::{Parasitics, TimingContext, TimingLibrary};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

const SCENARIO_GENERATION_DOMAIN: &[u8] = b"opto/timing/scenario-set/v1\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
/// Dense identity of one scenario and analysis polarity in a sealed scenario set.
///
/// IDs are assigned by canonical `ScenarioId` order, with max then min for
/// each scenario. Consumers must validate or resolve them through the owning
/// [`ScenarioSet`] rather than inferring scenario identity from the integer.
pub struct AnalysisViewId(u32);

impl AnalysisViewId {
    #[must_use]
    /// Creates an analysis-view ID from its stored value.
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    #[must_use]
    /// Returns the stored numeric value.
    pub const fn raw(self) -> u32 {
        self.0
    }

    #[must_use]
    /// Returns the zero-based dense index.
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

impl fmt::Display for AnalysisViewId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(transparent)]
/// Stable identity of one explicitly configured analysis scenario.
pub struct ScenarioId(u32);

impl ScenarioId {
    /// Creates an explicit scenario identity.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the stable integer encoding.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "these fields form an orthogonal analysis-check mask, not mutually exclusive states; \
              enums would incorrectly prevent valid combinations"
)]
/// Timing and electrical checks enabled for one scenario.
///
/// The flags are explicit even when the current command surface constructs a
/// single default scenario. This prevents adding a second scenario from
/// silently changing which analyses exist.
pub struct ScenarioCheckSet {
    /// Enables setup checks.
    pub setup: bool,
    /// Enables hold checks.
    pub hold: bool,
    /// Enables recovery checks.
    pub recovery: bool,
    /// Enables removal checks.
    pub removal: bool,
    /// Enables pulse-width checks.
    pub pulse_width: bool,
    /// Enables maximum-transition checks.
    pub max_transition: bool,
    /// Enables maximum-capacitance checks.
    pub max_capacitance: bool,
    /// Enables maximum-fanout checks.
    pub max_fanout: bool,
}

impl ScenarioCheckSet {
    /// Setup-only data-path analysis used by compact `QoR` summaries.
    pub const SETUP: Self = Self {
        setup: true,
        hold: false,
        recovery: false,
        removal: false,
        pulse_width: false,
        max_transition: false,
        max_capacitance: false,
        max_fanout: false,
    };

    /// All timing and electrical checks required by the synthesis contract.
    pub const ALL: Self = Self {
        setup: true,
        hold: true,
        recovery: true,
        removal: true,
        pulse_width: true,
        max_transition: true,
        max_capacitance: true,
        max_fanout: true,
    };

    fn bits(self) -> u8 {
        u8::from(self.setup)
            | (u8::from(self.hold) << 1)
            | (u8::from(self.recovery) << 2)
            | (u8::from(self.removal) << 3)
            | (u8::from(self.pulse_width) << 4)
            | (u8::from(self.max_transition) << 5)
            | (u8::from(self.max_capacitance) << 6)
            | (u8::from(self.max_fanout) << 7)
    }
}

impl Default for ScenarioCheckSet {
    fn default() -> Self {
        Self::ALL
    }
}

#[derive(Debug, Clone)]
/// Explicit power-library and switching-activity identity for one scenario.
/// Missing activity suppresses dynamic-power ranking.
pub struct ScenarioPowerView {
    library: Arc<opto_library::PowerLibrary>,
    library_fingerprint: [u8; 32],
    activities: Arc<[(ScenarioActivityTarget, ScenarioSwitchingActivity)]>,
    activity_fingerprint: Option<[u8; 32]>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// Stable pre-mapping target of an explicit switching-activity annotation.
pub enum ScenarioActivityTarget {
    /// Persistent design-port target.
    Port(crate::PortId),
    /// Persistent design-net target.
    Net(crate::NetId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
/// Validated switching activity stored without non-total floating comparison.
pub struct ScenarioSwitchingActivity {
    static_probability: u64,
    toggle_rate: u64,
    rise_ratio: u64,
}

impl<'de> Deserialize<'de> for ScenarioSwitchingActivity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Representation {
            static_probability: u64,
            toggle_rate: u64,
            rise_ratio: u64,
        }

        let representation = Representation::deserialize(deserializer)?;
        Self::new(
            f64::from_bits(representation.static_probability),
            f64::from_bits(representation.toggle_rate),
            f64::from_bits(representation.rise_ratio),
        )
        .ok_or_else(|| serde::de::Error::custom("invalid switching activity"))
    }
}

impl ScenarioSwitchingActivity {
    #[must_use]
    /// Creates validated switching activity.
    pub fn new(static_probability: f64, toggle_rate: f64, rise_ratio: f64) -> Option<Self> {
        (static_probability.is_finite()
            && (0.0..=1.0).contains(&static_probability)
            && toggle_rate.is_finite()
            && toggle_rate >= 0.0
            && rise_ratio.is_finite()
            && (0.0..=1.0).contains(&rise_ratio))
        .then_some(Self {
            static_probability: static_probability.to_bits(),
            toggle_rate: toggle_rate.to_bits(),
            rise_ratio: rise_ratio.to_bits(),
        })
    }

    #[must_use]
    /// Returns the probability of logic one.
    pub const fn static_probability(self) -> f64 {
        f64::from_bits(self.static_probability)
    }

    #[must_use]
    /// Returns transitions per unit time.
    pub const fn toggle_rate(self) -> f64 {
        f64::from_bits(self.toggle_rate)
    }

    #[must_use]
    /// Returns the fraction of transitions that rise.
    pub const fn rise_ratio(self) -> f64 {
        f64::from_bits(self.rise_ratio)
    }
}

impl ScenarioPowerView {
    /// Creates a power view with canonical activity ordering.
    ///
    /// # Errors
    ///
    /// Returns [`ScenarioSetError::DuplicateActivityTarget`] when more than one
    /// activity record names the same persistent net or pin.
    pub fn new(
        library: Arc<opto_library::PowerLibrary>,
        activities: Vec<(ScenarioActivityTarget, ScenarioSwitchingActivity)>,
    ) -> Result<Self, ScenarioSetError> {
        let library_fingerprint = library.content_fingerprint().bytes();
        let mut activities = activities;
        activities.sort_by(|left, right| left.0.cmp(&right.0));
        if activities.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err(ScenarioSetError::DuplicateActivityTarget);
        }
        let activity_fingerprint = (!activities.is_empty()).then(|| {
            let mut digest = blake3::Hasher::new();
            digest.update(b"opto/timing/scenario-activity/v1\0");
            for (target, activity) in &activities {
                match target {
                    ScenarioActivityTarget::Port(port) => {
                        digest.update(&[0]);
                        digest.update(&port.uid().get().get().to_le_bytes());
                    }
                    ScenarioActivityTarget::Net(net) => {
                        digest.update(&[1]);
                        digest.update(&net.uid().get().get().to_le_bytes());
                    }
                }
                digest.update(&activity.static_probability.to_le_bytes());
                digest.update(&activity.toggle_rate.to_le_bytes());
                digest.update(&activity.rise_ratio.to_le_bytes());
            }
            *digest.finalize().as_bytes()
        });
        Ok(Self {
            library,
            library_fingerprint,
            activities: activities.into(),
            activity_fingerprint,
        })
    }

    #[must_use]
    /// Returns the power-library fingerprint.
    pub const fn library_fingerprint(&self) -> [u8; 32] {
        self.library_fingerprint
    }

    #[must_use]
    /// Returns the optional activity fingerprint.
    pub const fn activity_fingerprint(&self) -> Option<[u8; 32]> {
        self.activity_fingerprint
    }

    #[must_use]
    /// Returns the characterized power library.
    pub fn library(&self) -> &Arc<opto_library::PowerLibrary> {
        &self.library
    }

    #[must_use]
    /// Returns canonical activity annotations.
    pub fn activities(&self) -> &[(ScenarioActivityTarget, ScenarioSwitchingActivity)] {
        &self.activities
    }

    #[must_use]
    /// Looks up activity for one target.
    pub fn activity(&self, target: &ScenarioActivityTarget) -> Option<ScenarioSwitchingActivity> {
        self.activities
            .binary_search_by(|(candidate, _)| candidate.cmp(target))
            .ok()
            .map(|index| self.activities[index].1)
    }
}

#[derive(Debug, Clone)]
/// Correlated early/late timing-library and parasitic views for one scenario.
pub struct ScenarioTimingViews {
    early_library: Arc<TimingLibrary>,
    late_library: Arc<TimingLibrary>,
    early_parasitics: Parasitics,
    late_parasitics: Parasitics,
}

impl ScenarioTimingViews {
    #[must_use]
    /// Creates early and late characterized timing views.
    pub fn new(
        early_library: Arc<TimingLibrary>,
        late_library: Arc<TimingLibrary>,
        early_parasitics: Parasitics,
        late_parasitics: Parasitics,
    ) -> Self {
        Self {
            early_library,
            late_library,
            early_parasitics,
            late_parasitics,
        }
    }
}

#[derive(Debug, Clone)]
/// One complete and intentional mode/corner binding.
pub struct Scenario {
    id: ScenarioId,
    name: Arc<str>,
    constraints: Arc<TimingContext>,
    early_library: Arc<TimingLibrary>,
    late_library: Arc<TimingLibrary>,
    early_parasitics: Parasitics,
    late_parasitics: Parasitics,
    power: ScenarioPowerView,
    checks: ScenarioCheckSet,
}

impl Scenario {
    /// Constructs one explicit scenario from complete early and late views.
    #[must_use]
    pub fn new(
        id: ScenarioId,
        name: impl Into<Arc<str>>,
        constraints: Arc<TimingContext>,
        timing: ScenarioTimingViews,
        power: ScenarioPowerView,
        checks: ScenarioCheckSet,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            constraints,
            early_library: timing.early_library,
            late_library: timing.late_library,
            early_parasitics: timing.early_parasitics,
            late_parasitics: timing.late_parasitics,
            power,
            checks,
        }
    }

    /// Constructs the single logical scenario used by today's verified Tcl
    /// surface while retaining the sparse representation internally.
    ///
    /// # Panics
    ///
    /// Panics only if constructing an activity view with no activity records is
    /// rejected, which would violate [`ScenarioPowerView::new`]'s contract.
    #[must_use]
    pub fn single(
        constraints: Arc<TimingContext>,
        library: Arc<TimingLibrary>,
        parasitics: Parasitics,
    ) -> Self {
        let power = ScenarioPowerView::new(Arc::new(library.power.clone()), Vec::new())
            .expect("an empty activity view is valid");
        Self::new(
            ScenarioId::from_raw(0),
            "default",
            constraints,
            ScenarioTimingViews::new(
                Arc::clone(&library),
                library,
                parasitics.clone(),
                parasitics,
            ),
            power,
            ScenarioCheckSet::ALL,
        )
    }

    /// Rebinds the explicit power and activity view before the scenario set is sealed.
    #[must_use]
    pub fn with_power(mut self, power: ScenarioPowerView) -> Self {
        self.power = power;
        self
    }

    #[must_use]
    /// Returns the stable scenario identity.
    pub const fn id(&self) -> ScenarioId {
        self.id
    }

    #[must_use]
    /// Returns the scenario name.
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    /// Returns the scenario constraint context.
    pub fn constraints(&self) -> &Arc<TimingContext> {
        &self.constraints
    }

    #[must_use]
    /// Returns the early timing library.
    pub fn early_library(&self) -> &Arc<TimingLibrary> {
        &self.early_library
    }

    #[must_use]
    /// Returns the late timing library.
    pub fn late_library(&self) -> &Arc<TimingLibrary> {
        &self.late_library
    }

    #[must_use]
    /// Returns the early parasitic view.
    pub const fn early_parasitics(&self) -> &Parasitics {
        &self.early_parasitics
    }

    #[must_use]
    /// Returns the late parasitic view.
    pub const fn late_parasitics(&self) -> &Parasitics {
        &self.late_parasitics
    }

    #[must_use]
    /// Returns the scenario power view.
    pub const fn power(&self) -> &ScenarioPowerView {
        &self.power
    }

    #[must_use]
    /// Returns enabled analysis checks.
    pub const fn checks(&self) -> ScenarioCheckSet {
        self.checks
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
/// Semantic identity of a complete sparse scenario set.
pub struct ScenarioGeneration([u8; 32]);

impl ScenarioGeneration {
    #[must_use]
    /// Returns the generation digest.
    pub const fn bytes(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Debug, Clone)]
/// Canonically ordered, explicitly sparse scenario bindings.
pub struct ScenarioSet {
    scenarios: Arc<[Scenario]>,
    generation: ScenarioGeneration,
}

impl ScenarioSet {
    /// Validates and seals an explicit sparse set.
    ///
    /// # Errors
    ///
    /// Empty sets, empty names, and duplicate identities or names are rejected.
    pub fn new(mut scenarios: Vec<Scenario>) -> Result<Self, ScenarioSetError> {
        if scenarios.is_empty() {
            return Err(ScenarioSetError::Empty);
        }
        if (scenarios.len() as u64) > (u64::from(u32::MAX) + 1).div_ceil(2) {
            return Err(ScenarioSetError::AnalysisViewCapacity);
        }
        scenarios.sort_by_key(Scenario::id);
        let mut names = BTreeSet::new();
        for pair in scenarios.windows(2) {
            if pair[0].id() == pair[1].id() {
                return Err(ScenarioSetError::DuplicateId(pair[0].id()));
            }
        }
        for scenario in &scenarios {
            if scenario.name().trim().is_empty() {
                return Err(ScenarioSetError::EmptyName(scenario.id()));
            }
            if !names.insert(scenario.name().to_string()) {
                return Err(ScenarioSetError::DuplicateName(scenario.name().to_string()));
            }
        }
        let generation = seal_generation(&scenarios);
        Ok(Self {
            scenarios: scenarios.into(),
            generation,
        })
    }

    /// Seals the current one-scenario command environment.
    ///
    /// # Panics
    ///
    /// Panics only if the built-in nonempty, uniquely named default scenario is
    /// rejected by [`ScenarioSet::new`].
    #[must_use]
    pub fn single(
        constraints: Arc<TimingContext>,
        library: Arc<TimingLibrary>,
        parasitics: Parasitics,
    ) -> Self {
        Self::new(vec![Scenario::single(constraints, library, parasitics)])
            .expect("the built-in single scenario is valid")
    }

    #[must_use]
    /// Returns scenarios in canonical order.
    pub fn scenarios(&self) -> &[Scenario] {
        &self.scenarios
    }

    #[must_use]
    /// Looks up a scenario by ID.
    pub fn get(&self, id: ScenarioId) -> Option<&Scenario> {
        self.scenarios
            .binary_search_by_key(&id, Scenario::id)
            .ok()
            .map(|index| &self.scenarios[index])
    }

    #[must_use]
    /// Returns the semantic scenario-set generation.
    pub const fn generation(&self) -> ScenarioGeneration {
        self.generation
    }

    #[must_use]
    /// Resolves a scenario/polarity pair to its deterministic dense view ID.
    pub fn analysis_view_id(
        &self,
        scenario: ScenarioId,
        delay_type: crate::DelayType,
    ) -> Option<AnalysisViewId> {
        let scenario = self
            .scenarios
            .binary_search_by_key(&scenario, Scenario::id)
            .ok()?;
        Some(analysis_view_id(scenario, delay_type))
    }

    #[must_use]
    /// Validates and resolves a view ID to its scenario and polarity.
    pub fn analysis_view(&self, id: AnalysisViewId) -> Option<(&Scenario, crate::DelayType)> {
        let scenario = self.scenarios.get(id.index() / 2)?;
        let delay_type = if id.raw().is_multiple_of(2) {
            crate::DelayType::Max
        } else {
            crate::DelayType::Min
        };
        Some((scenario, delay_type))
    }

    /// Iterates every canonical scenario/polarity identity, including views
    /// whose library may later prove uncharacterized.
    ///
    /// # Panics
    ///
    /// Panics if validated scenario cardinality no longer fits compact view IDs
    /// or if the canonical dense view range contains a hole.
    #[must_use]
    pub fn analysis_views(
        &self,
    ) -> impl ExactSizeIterator<Item = (AnalysisViewId, &Scenario, crate::DelayType)> + '_ {
        (0..self.scenarios.len() * 2).map(|view| {
            let id = AnalysisViewId(
                u32::try_from(view)
                    .expect("scenario-set construction validates analysis-view capacity"),
            );
            let (scenario, delay_type) = self
                .analysis_view(id)
                .expect("canonical analysis-view range belongs to the scenario set");
            (id, scenario, delay_type)
        })
    }
}

fn analysis_view_id(scenario: usize, delay_type: crate::DelayType) -> AnalysisViewId {
    let polarity = match delay_type {
        crate::DelayType::Max => 0,
        crate::DelayType::Min => 1,
    };
    AnalysisViewId(
        u32::try_from(scenario * 2 + polarity)
            .expect("scenario-set construction validates analysis-view capacity"),
    )
}

fn seal_generation(scenarios: &[Scenario]) -> ScenarioGeneration {
    let mut digest = blake3::Hasher::new();
    let mut constraint_fingerprints = BTreeMap::new();
    digest.update(SCENARIO_GENERATION_DOMAIN);
    digest.update(&(scenarios.len() as u64).to_le_bytes());
    for scenario in scenarios {
        let constraints = scenario.constraints();
        let constraints_fingerprint = *constraint_fingerprints
            .entry(Arc::as_ptr(constraints))
            .or_insert_with(|| constraints.synthesis_fingerprint());

        digest.update(&scenario.id().raw().to_le_bytes());
        digest.update(&(scenario.name().len() as u64).to_le_bytes());
        digest.update(scenario.name().as_bytes());
        digest.update(&constraints_fingerprint.bytes());
        digest.update(&scenario.early_library().analysis_fingerprint().bytes());
        digest.update(&scenario.late_library().analysis_fingerprint().bytes());
        digest.update(&scenario.early_parasitics().content_fingerprint().bytes());
        digest.update(&scenario.late_parasitics().content_fingerprint().bytes());
        digest.update(&scenario.power().library_fingerprint());
        match scenario.power().activity_fingerprint() {
            Some(activity) => {
                digest.update(&[1]);
                digest.update(&activity);
            }
            None => {
                digest.update(&[0]);
            }
        }
        digest.update(&[scenario.checks().bits()]);
    }
    ScenarioGeneration(*digest.finalize().as_bytes())
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Invalid sparse scenario-set construction.
pub enum ScenarioSetError {
    /// No scenario was provided.
    Empty,
    /// A scenario name is empty.
    EmptyName(ScenarioId),
    /// A scenario ID is repeated.
    DuplicateId(ScenarioId),
    /// A scenario name is repeated.
    DuplicateName(String),
    /// A power activity target is repeated.
    DuplicateActivityTarget,
    /// Analysis-view ID capacity was exhausted.
    AnalysisViewCapacity,
}

impl fmt::Display for ScenarioSetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("scenario set is empty"),
            Self::EmptyName(id) => write!(formatter, "scenario {} has an empty name", id.raw()),
            Self::DuplicateId(id) => write!(formatter, "scenario ID {} is duplicated", id.raw()),
            Self::DuplicateName(name) => write!(formatter, "scenario name '{name}' is duplicated"),
            Self::DuplicateActivityTarget => {
                formatter.write_str("scenario switching-activity target is duplicated")
            }
            Self::AnalysisViewCapacity => {
                formatter.write_str("scenario set exceeds the u32 analysis-view ID capacity")
            }
        }
    }
}

impl std::error::Error for ScenarioSetError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn scenario(id: u32, name: &str) -> Scenario {
        Scenario::single(
            Arc::new(TimingContext::default()),
            Arc::new(TimingLibrary::default()),
            Parasitics::default(),
        )
        .with_identity(ScenarioId::from_raw(id), name)
    }

    impl Scenario {
        fn with_identity(mut self, id: ScenarioId, name: &str) -> Self {
            self.id = id;
            self.name = Arc::from(name);
            self
        }
    }

    #[test]
    fn sparse_identity_is_canonical_but_not_cartesian() {
        let set =
            ScenarioSet::new(vec![scenario(19, "func-slow"), scenario(3, "scan-fast")]).unwrap();
        assert_eq!(
            set.scenarios()
                .iter()
                .map(|scenario| scenario.id().raw())
                .collect::<Vec<_>>(),
            vec![3, 19]
        );
        assert_eq!(set.scenarios().len(), 2);
    }

    #[test]
    fn generation_covers_explicit_identity_and_checks() {
        let original = ScenarioSet::new(vec![scenario(1, "func")]).unwrap();
        let renamed = ScenarioSet::new(vec![scenario(1, "scan")]).unwrap();
        let mut changed = scenario(1, "func");
        changed.checks.hold = false;
        let changed = ScenarioSet::new(vec![changed]).unwrap();
        assert_ne!(original.generation(), renamed.generation());
        assert_ne!(original.generation(), changed.generation());

        let activity = ScenarioSet::new(vec![
            scenario(1, "func").with_power(
                ScenarioPowerView::new(
                    Arc::new(opto_library::PowerLibrary {
                        units: opto_library::PowerLibraryUnits {
                            nominal_voltage: Some(0.7),
                            ..opto_library::PowerLibraryUnits::default()
                        },
                        ..opto_library::PowerLibrary::default()
                    }),
                    vec![(
                        ScenarioActivityTarget::Net(crate::NetId::from_uid(
                            opto_core::ObjectUid::from_raw(1).unwrap(),
                        )),
                        ScenarioSwitchingActivity::new(0.5, 0.2, 0.5).unwrap(),
                    )],
                )
                .unwrap(),
            ),
        ])
        .unwrap();
        assert_ne!(original.generation(), activity.generation());
    }

    #[test]
    fn duplicate_identity_is_rejected() {
        let error = ScenarioSet::new(vec![scenario(7, "a"), scenario(7, "b")]).unwrap_err();
        assert_eq!(
            error,
            ScenarioSetError::DuplicateId(ScenarioId::from_raw(7))
        );
    }
}
