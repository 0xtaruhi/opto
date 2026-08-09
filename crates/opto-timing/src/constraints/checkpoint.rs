// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Serialized form of the constraint context and its restore path.

use super::*;

/// Serialization-only timing state.
///
/// Deserializing this type builds only the primary row vectors and maps. The
/// ordered arenas and reverse indexes are rebuilt explicitly by [`Self::restore`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimingContextCheckpoint {
    revision: RevisionId,
    clocks: Vec<Clock>,
    input_transitions: BTreeMap<PortId, PortValueSlots>,
    loads: BTreeMap<PortId, PortValueSlots>,
    resistances: BTreeMap<TimingEndpoint, PortValueSlots>,
    input_delays: BTreeMap<PortId, Vec<IoDelay>>,
    output_delays: BTreeMap<PortId, Vec<IoDelay>>,
    clock_uncertainties: BTreeMap<ClockUncertaintyKey, f64>,
    case_analysis: BTreeMap<TimingEndpoint, CaseAnalysisValue>,
    disabled_timing: BTreeSet<DisabledTiming>,
    timing_derates: TimingDerates,
    path_exceptions: Vec<PathException>,
    max_transitions: Vec<DesignRuleConstraint>,
    max_capacitances: Vec<DesignRuleConstraint>,
    max_fanouts: Vec<DesignRuleConstraint>,
}

impl From<&TimingContext> for TimingContextCheckpoint {
    fn from(timing: &TimingContext) -> Self {
        Self {
            revision: timing.revision,
            clocks: timing.clocks.iter().cloned().collect(),
            input_transitions: timing.input_transitions.clone(),
            loads: timing.loads.clone(),
            resistances: timing.resistances.clone(),
            input_delays: timing.input_delays.clone(),
            output_delays: timing.output_delays.clone(),
            clock_uncertainties: timing.clock_uncertainties.clone(),
            case_analysis: timing.case_analysis.clone(),
            disabled_timing: timing.disabled_timing.clone(),
            timing_derates: timing.timing_derates,
            path_exceptions: timing.path_exceptions.iter().cloned().collect(),
            max_transitions: timing.max_transitions.iter().cloned().collect(),
            max_capacitances: timing.max_capacitances.iter().cloned().collect(),
            max_fanouts: timing.max_fanouts.iter().cloned().collect(),
        }
    }
}

impl From<TimingContext> for TimingContextCheckpoint {
    fn from(timing: TimingContext) -> Self {
        Self {
            revision: timing.revision,
            clocks: timing.clocks.into_values(),
            input_transitions: timing.input_transitions,
            loads: timing.loads,
            resistances: timing.resistances,
            input_delays: timing.input_delays,
            output_delays: timing.output_delays,
            clock_uncertainties: timing.clock_uncertainties,
            case_analysis: timing.case_analysis,
            disabled_timing: timing.disabled_timing,
            timing_derates: timing.timing_derates,
            path_exceptions: timing.path_exceptions.into_values(),
            max_transitions: timing.max_transitions.into_values(),
            max_capacitances: timing.max_capacitances.into_values(),
            max_fanouts: timing.max_fanouts.into_values(),
        }
    }
}

impl TimingContextCheckpoint {
    /// Restores a live context and rebuilds all derived indexes.
    ///
    /// # Errors
    ///
    /// Returns an error if row identities, reverse references, or compact arena
    /// capacities are invalid.
    pub fn restore(self) -> Result<TimingContext, crate::TimingError> {
        let mut timing = TimingContext {
            owner: Arc::new(()),
            revision: self.revision,
            clocks: OrderedArena::from_values(self.clocks)?,
            input_transitions: self.input_transitions,
            loads: self.loads,
            resistances: self.resistances,
            input_delays: self.input_delays,
            output_delays: self.output_delays,
            clock_uncertainties: self.clock_uncertainties,
            case_analysis: self.case_analysis,
            disabled_timing: self.disabled_timing,
            timing_derates: self.timing_derates,
            path_exceptions: OrderedArena::from_values(self.path_exceptions)?,
            max_transitions: OrderedArena::from_values(self.max_transitions)?,
            max_capacitances: OrderedArena::from_values(self.max_capacitances)?,
            max_fanouts: OrderedArena::from_values(self.max_fanouts)?,
            clock_slots: BTreeMap::new(),
            references: BTreeMap::new(),
            transactions: Vec::new(),
            journal: Vec::new(),
        };
        timing
            .rebuild_indexes()
            .map_err(|detail| crate::ConstraintError::InvalidCheckpoint { detail })?;
        Ok(timing)
    }
}

impl Serialize for TimingContext {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        TimingContextRef {
            revision: self.revision,
            clocks: &self.clocks,
            input_transitions: &self.input_transitions,
            loads: &self.loads,
            resistances: &self.resistances,
            input_delays: &self.input_delays,
            output_delays: &self.output_delays,
            clock_uncertainties: &self.clock_uncertainties,
            case_analysis: &self.case_analysis,
            disabled_timing: &self.disabled_timing,
            timing_derates: self.timing_derates,
            path_exceptions: &self.path_exceptions,
            max_transitions: &self.max_transitions,
            max_capacitances: &self.max_capacitances,
            max_fanouts: &self.max_fanouts,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TimingContext {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        TimingContextCheckpoint::deserialize(deserializer)?
            .restore()
            .map_err(D::Error::custom)
    }
}
