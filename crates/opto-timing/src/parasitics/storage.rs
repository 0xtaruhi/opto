// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Canonical contiguous storage for validated parasitic annotations.
//!
//! Names are interned, nets are sorted, and each net owns adjacent ranges in
//! the node, resistor, and connection arenas. This keeps checkpoint validation
//! and analysis scans deterministic without object-level allocation.

use super::{
    ParasiticAnalysisOptions, ParasiticAnnotationRow, ParasiticAnnotationSummary,
    ParasiticDelayModel, RcConnectionRole, RcNetwork, TimingEdge, TimingLibraryUnits,
    checked_count, invalid_net, network,
};
use crate::TimingModelError;
use opto_core::{NameId, NameTable};
use serde::{Deserialize, Serialize};
use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;
use std::mem::size_of;
use std::sync::{Arc, OnceLock};

const PARASITICS_FINGERPRINT_DOMAIN: &[u8] = b"opto/synthesis-parasitics/v2\0";

mod builder;
mod fingerprint;
mod runs;
mod validation;

use fingerprint::{
    fingerprint_f64, fingerprint_f64_pair, fingerprint_optional_f64_pair, fingerprint_value,
};
use runs::{compact_logical_store, insert_run, logical_nets_cover};

/// Semantic identity of compact parasitic annotations consumed by synthesis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ParasiticsFingerprint([u8; 32]);

impl ParasiticsFingerprint {
    #[must_use]
    /// Returns the stable 256-bit digest.
    pub const fn bytes(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Immutable compact database of net-level parasitic annotations.
///
/// Checkpoints preserve the validated immutable run structure so saving never
/// requires an O(N) rebuild. The cached semantic fingerprint walks the merged
/// logical view and makes equality independent of that physical history.
pub struct Parasitics {
    capacitance_unit_farads: f64,
    time_unit_seconds: f64,
    net_count: u32,
    base_override_count: u32,
    base: Arc<ParasiticStore>,
    runs: Box<[ParasiticRun]>,
    #[serde(skip)]
    fingerprint: Arc<OnceLock<ParasiticsFingerprint>>,
}

impl PartialEq for Parasitics {
    fn eq(&self, other: &Self) -> bool {
        self.content_fingerprint() == other.content_fingerprint()
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ParasiticStore {
    names: NameTable,
    nets: Box<[ParasiticNet]>,
    nodes: Box<[ParasiticNode]>,
    resistors: Box<[ParasiticResistor]>,
    connections: Box<[ParasiticConnection]>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ParasiticRun {
    weight: u64,
    store: Arc<ParasiticStore>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ParasiticNetId {
    index: u32,
    store: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct ParasiticNet {
    name: NameId,
    total_capacitance: f64,
    load_annotated: bool,
    delay_model: ParasiticDelayModel,
    pin_capacitance_included: bool,
    node_start: u32,
    node_count: u32,
    resistor_start: u32,
    resistor_count: u32,
    connection_start: u32,
    connection_count: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct ParasiticNode {
    name: NameId,
    ground_capacitance_farads: f64,
}

#[derive(Debug, Serialize, Deserialize)]
struct ParasiticResistor {
    first: u32,
    second: u32,
    resistance_ohms: f64,
}

#[derive(Debug, Serialize, Deserialize)]
struct ParasiticConnection {
    object: NameId,
    node: u32,
    role: RcConnectionRole,
    pin_capacitance_farads: [f64; 2],
    delay: Option<[f64; 2]>,
    transition: Option<[f64; 2]>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ParasiticNetRef<'a> {
    store: &'a ParasiticStore,
    net: &'a ParasiticNet,
}

struct LogicalNetIter<'a> {
    stores: Box<[&'a ParasiticStore]>,
    pending: BinaryHeap<Reverse<(&'a str, Reverse<usize>, usize)>>,
}

#[derive(Default)]
struct ParasiticStoreBuilder {
    names: NameTable,
    nets: Vec<ParasiticNet>,
    nodes: Vec<ParasiticNode>,
    resistors: Vec<ParasiticResistor>,
    connections: Vec<ParasiticConnection>,
}

impl Parasitics {
    #[must_use]
    /// Hashes the canonical serialized representation.
    ///
    /// # Panics
    ///
    /// Panics if a previously validated compact store cannot be serialized into
    /// the infallible in-memory hash writer.
    pub fn content_fingerprint(&self) -> ParasiticsFingerprint {
        *self.fingerprint.get_or_init(|| {
            let mut digest = blake3::Hasher::new();
            digest.update(PARASITICS_FINGERPRINT_DOMAIN);
            self.write_fingerprint(&mut digest)
                .expect("validated compact parasitics are serializable");
            ParasiticsFingerprint(*digest.finalize().as_bytes())
        })
    }

    /// Validates the canonical dense representation before checkpoint state is
    /// published. Store validation is allocation-free; validating the merged
    /// view uses O(k) scratch for the geometrically bounded number of runs.
    /// Every persisted ID, numeric value, and appended arena range is covered.
    ///
    /// # Errors
    ///
    /// Returns an invalid-parasitic error for any noncanonical name, range,
    /// ordering, unit, electrical value, connection, or run invariant; also
    /// returns a capacity error if merged-view validation scratch cannot grow.
    pub fn validate_checkpoint(&self) -> Result<(), crate::TimingError> {
        self.base.validate_checkpoint()?;
        let mut previous_weight = None;
        for run in &self.runs {
            run.store.validate_checkpoint()?;
            if run.store.nets.is_empty() || run.weight != run.store.work_weight()? {
                return Err(invalid_net(
                    "<database>",
                    "parasitic run has invalid weight or empty storage",
                ));
            }
            if previous_weight.is_some_and(|older| older <= run.weight.saturating_mul(2)) {
                return Err(invalid_net(
                    "<database>",
                    "parasitic runs are not geometrically size-ordered",
                ));
            }
            previous_weight = Some(run.weight);
        }
        if self.base.nets.is_empty() && !self.runs.is_empty() {
            return Err(invalid_net(
                "<database>",
                "parasitic runs require a nonempty base",
            ));
        }
        if checked_count(self.logical_nets().count(), "parasitic net arena")? != self.net_count {
            return Err(invalid_net(
                "<database>",
                "parasitic logical net count is not canonical",
            ));
        }
        let overridden = self
            .base
            .nets
            .iter()
            .filter(|net| {
                self.base.name(net.name).is_some_and(|name| {
                    self.runs
                        .iter()
                        .any(|run| run.store.net_index(name).is_some())
                })
            })
            .count();
        if checked_count(overridden, "parasitic base override count")? != self.base_override_count
            || (!self.base.nets.is_empty() && overridden == self.base.nets.len())
        {
            return Err(invalid_net(
                "<database>",
                "parasitic base overrides are not canonical or require promotion",
            ));
        }
        if self.is_empty() {
            return Ok(());
        }
        if !self.time_unit_seconds.is_finite()
            || self.time_unit_seconds <= 0.0
            || !self.capacitance_unit_farads.is_finite()
            || self.capacitance_unit_farads <= 0.0
        {
            return Err(invalid_net(
                "<database>",
                "parasitic units must be positive and finite",
            ));
        }
        Ok(())
    }

    /// Validates, analyzes, sorts, and packs imported RC networks.
    ///
    /// # Errors
    ///
    /// Returns an error for missing library units, duplicate net names,
    /// invalid/passive-network topology, unsupported coupling, numeric failure,
    /// or compact arena overflow.
    pub fn from_rc_networks(
        mut networks: Vec<RcNetwork>,
        units: TimingLibraryUnits,
        options: ParasiticAnalysisOptions,
    ) -> Result<Self, crate::TimingError> {
        let time_unit = required_unit(units.time_seconds, "Liberty time")?;
        let capacitance_unit = required_unit(units.capacitance_farads, "Liberty capacitance")?;
        let net_count = checked_count(networks.len(), "parasitic net arena")?;
        networks.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        for pair in networks.windows(2) {
            if pair[0].name == pair[1].name {
                return Err(invalid_net(
                    &pair[0].name,
                    "parasitic input contains duplicate net blocks",
                ));
            }
        }
        let mut builder = ParasiticStoreBuilder::default();
        for network in networks {
            builder.push_computed(network::compute(
                network,
                time_unit,
                capacitance_unit,
                options,
            )?)?;
        }
        Ok(Self {
            capacitance_unit_farads: capacitance_unit,
            time_unit_seconds: time_unit,
            net_count,
            base_override_count: 0,
            base: Arc::new(builder.finish()),
            runs: Box::new([]),
            fingerprint: Arc::new(OnceLock::new()),
        })
    }

    /// Combines net-level annotations without object-level heap state. Normal reads replace nets
    /// present in `new`; incremental reads install the new RC/load state but retain already
    /// annotated responses for matching connections.
    ///
    /// # Errors
    ///
    /// Returns an error when the two stores use different exact Liberty unit
    /// scales, contain invalid compact records, or cannot allocate the merged
    /// run and name storage.
    pub fn overlay(
        &self,
        mut new: Self,
        retain_existing: bool,
    ) -> Result<Self, crate::TimingError> {
        if self.is_empty() {
            return Ok(new);
        }
        if new.is_empty() {
            return Ok(self.clone());
        }
        if !same_float_bits(self.time_unit_seconds, new.time_unit_seconds)
            || !same_float_bits(self.capacitance_unit_farads, new.capacitance_unit_farads)
        {
            return Err(invalid_net(
                "<library>",
                "incremental parasitics use different Liberty units",
            ));
        }

        let covers_all = logical_nets_cover(new.logical_nets(), self.logical_nets());
        if covers_all {
            if !retain_existing {
                return Ok(new);
            }
            if new.runs.is_empty()
                && let Some(store) = Arc::get_mut(&mut new.base)
            {
                self.retain_store_responses(store)?;
                new.fingerprint = Arc::new(OnceLock::new());
                return Ok(new);
            }
        }

        let incoming = self.prepare_incoming_store(new, retain_existing)?;
        if covers_all {
            let net_count = checked_count(incoming.nets.len(), "parasitic net arena")?;
            return Ok(self.with_storage(incoming, Box::new([]), net_count, 0));
        }
        let (net_count, base_override_count) = self.merged_counts(&incoming)?;

        let mut runs = self.runs.to_vec();
        insert_run(&mut runs, incoming)?;
        if usize::try_from(base_override_count).is_ok_and(|count| count == self.base.nets.len()) {
            let promoted = compact_logical_store(LogicalNetIter::from_runs(&runs))?;
            let base = Arc::new(promoted);
            let net_count = checked_count(base.nets.len(), "parasitic net arena")?;
            return Ok(self.with_storage(base, Box::new([]), net_count, 0));
        }
        Ok(self.with_storage(
            Arc::clone(&self.base),
            runs.into_boxed_slice(),
            net_count,
            base_override_count,
        ))
    }

    #[must_use]
    /// Returns whether the database contains no annotated nets.
    pub fn is_empty(&self) -> bool {
        self.net_count == 0
    }

    /// Summarizes annotations newly contributed relative to `previous`.
    ///
    /// # Errors
    ///
    /// Returns an error if persisted compact IDs or counts are invalid.
    pub fn annotation_summary(
        &self,
        previous: Option<&Self>,
    ) -> Result<ParasiticAnnotationSummary, crate::TimingError> {
        let mut summary = ParasiticAnnotationSummary::default();
        for net in self.logical_nets() {
            if !net.net.load_annotated {
                summary.skipped_nets += 1;
                continue;
            }
            let net_name = net.required_name(net.net.name)?;
            let connections = net.connections()?;
            let drivers = connections
                .iter()
                .filter(|connection| connection.role == RcConnectionRole::Driver)
                .count();
            let previous_net = previous.and_then(|parasitics| parasitics.net(net_name));
            let mut net_has_delay = false;
            for connection in connections
                .iter()
                .filter(|connection| connection.role == RcConnectionRole::Sink)
            {
                if connection.delay.is_none() {
                    continue;
                }
                net_has_delay = true;
                let object = net.required_name(connection.object)?;
                if previous_net.is_some_and(|net| {
                    net.connection(object)
                        .is_some_and(|old| old.delay.is_some())
                }) {
                    continue;
                }
                summary.pin_to_pin_delays = summary
                    .pin_to_pin_delays
                    .checked_add(drivers)
                    .ok_or_else(|| invalid_net(net_name, "annotation count overflow"))?;
            }
            summary.annotated_nets += usize::from(net_has_delay);
        }
        Ok(summary)
    }

    /// Materializes deterministic driver-to-sink report rows.
    ///
    /// # Errors
    ///
    /// Returns an error if the compact database contains an invalid name or
    /// arena range.
    pub fn annotation_rows(&self) -> Result<Vec<ParasiticAnnotationRow>, crate::TimingError> {
        let mut rows = Vec::new();
        for net in self.logical_nets() {
            if !net.net.load_annotated {
                continue;
            }
            let connections = net.connections()?;
            let drivers = connections
                .iter()
                .filter(|connection| connection.role == RcConnectionRole::Driver)
                .map(|connection| net.required_name(connection.object))
                .collect::<Result<Vec<_>, _>>()?;
            let pin_capacitance = if net.net.pin_capacitance_included {
                0.0
            } else {
                connections
                    .iter()
                    .map(|connection| {
                        (connection.pin_capacitance_farads[0]
                            + connection.pin_capacitance_farads[1])
                            * 0.5
                    })
                    .sum::<f64>()
                    / self.capacitance_unit_farads
            };
            let load = net.net.total_capacitance + pin_capacitance;
            let net_name = net.required_name(net.net.name)?;
            for connection in connections
                .iter()
                .filter(|connection| connection.role == RcConnectionRole::Sink)
            {
                let sink = net.required_name(connection.object)?;
                for driver in &drivers {
                    rows.push(ParasiticAnnotationRow {
                        net: net_name.to_string(),
                        from: (*driver).to_string(),
                        to: sink.to_string(),
                        delay: connection.delay,
                        load,
                    });
                }
            }
        }
        Ok(rows)
    }

    pub(crate) fn net(&self, name: &str) -> Option<ParasiticNetRef<'_>> {
        self.net_by_id(self.net_id(name)?)
    }

    pub(crate) fn net_id(&self, name: &str) -> Option<ParasiticNetId> {
        for (run_index, run) in self.runs.iter().enumerate().rev() {
            if let Some(index) = run.store.net_index(name) {
                return Some(ParasiticNetId {
                    index,
                    store: u32::try_from(run_index).ok()?.checked_add(1)?,
                });
            }
        }
        self.base
            .net_index(name)
            .map(|index| ParasiticNetId { index, store: 0 })
    }

    pub(crate) fn net_by_id(&self, id: ParasiticNetId) -> Option<ParasiticNetRef<'_>> {
        if id.store == 0 {
            self.base.net_ref(id.index)
        } else {
            let run = usize::try_from(id.store.checked_sub(1)?).ok()?;
            self.runs.get(run)?.store.net_ref(id.index)
        }
    }

    pub(crate) fn net_names(&self) -> impl Iterator<Item = &str> {
        self.logical_nets().filter_map(ParasiticNetRef::name)
    }

    fn logical_nets(&self) -> LogicalNetIter<'_> {
        LogicalNetIter::from_database(self)
    }

    fn retain_store_responses(
        &self,
        replacement: &mut ParasiticStore,
    ) -> Result<(), crate::TimingError> {
        let names = &replacement.names;
        let nets = &replacement.nets;
        let connections = &mut replacement.connections;
        for net in nets {
            let net_name = required_store_name(names, net.name)?;
            let Some(existing) = self.net(net_name) else {
                continue;
            };
            let range = checked_range(
                net.connection_start,
                net.connection_count,
                connections.len(),
            )?;
            for connection in &mut connections[range] {
                let object = required_store_name(names, connection.object)?;
                let Some(old) = existing.connection_with_role(connection.role, object) else {
                    continue;
                };
                if old.delay.is_some() || old.transition.is_some() {
                    connection.delay = old.delay;
                    connection.transition = old.transition;
                }
            }
        }
        Ok(())
    }

    fn prepare_incoming_store(
        &self,
        mut new: Self,
        retain_existing: bool,
    ) -> Result<Arc<ParasiticStore>, crate::TimingError> {
        if new.runs.is_empty() {
            if !retain_existing {
                return Ok(new.base);
            }
            if let Some(store) = Arc::get_mut(&mut new.base) {
                self.retain_store_responses(store)?;
                return Ok(new.base);
            }
        }
        let mut builder = ParasiticStoreBuilder::default();
        for replacement in new.logical_nets() {
            let retained = if retain_existing {
                self.net(replacement.required_name(replacement.net.name)?)
            } else {
                None
            };
            builder.push_ref(replacement, retained)?;
        }
        Ok(Arc::new(builder.finish()))
    }

    fn with_storage(
        &self,
        base: Arc<ParasiticStore>,
        runs: Box<[ParasiticRun]>,
        net_count: u32,
        base_override_count: u32,
    ) -> Self {
        Self {
            capacitance_unit_farads: self.capacitance_unit_farads,
            time_unit_seconds: self.time_unit_seconds,
            net_count,
            base_override_count,
            base,
            runs,
            fingerprint: Arc::new(OnceLock::new()),
        }
    }

    fn merged_counts(&self, incoming: &ParasiticStore) -> Result<(u32, u32), crate::TimingError> {
        let mut count = self.net_count;
        let mut overridden = self.base_override_count;
        for net in &incoming.nets {
            let name = required_store_name(&incoming.names, net.name)?;
            if self.net(name).is_none() {
                count = count
                    .checked_add(1)
                    .ok_or_else(|| capacity("parasitic net arena"))?;
            }
            if self.base.net_index(name).is_some()
                && !self
                    .runs
                    .iter()
                    .any(|run| run.store.net_index(name).is_some())
            {
                overridden = overridden
                    .checked_add(1)
                    .ok_or_else(|| capacity("parasitic base override count"))?;
            }
        }
        Ok((count, overridden))
    }

    fn write_fingerprint(
        &self,
        writer: &mut blake3::Hasher,
    ) -> Result<(), opto_archive::ArchiveError> {
        fingerprint_f64(writer, self.capacitance_unit_farads)?;
        fingerprint_f64(writer, self.time_unit_seconds)?;
        fingerprint_value(writer, &self.net_count)?;
        for net in self.logical_nets() {
            fingerprint_value(writer, net.name().expect("validated parasitic net name"))?;
            fingerprint_f64(writer, net.net.total_capacitance)?;
            fingerprint_value(writer, &net.net.load_annotated)?;
            fingerprint_value(writer, &net.net.delay_model)?;
            fingerprint_value(writer, &net.net.pin_capacitance_included)?;
            let nodes = net.nodes().expect("validated parasitic node range");
            fingerprint_value(writer, &(nodes.len() as u64))?;
            for node in nodes {
                fingerprint_value(
                    writer,
                    net.store
                        .name(node.name)
                        .expect("validated parasitic node name"),
                )?;
                fingerprint_f64(writer, node.ground_capacitance_farads)?;
            }
            let resistors = net.resistors().expect("validated parasitic resistor range");
            fingerprint_value(writer, &(resistors.len() as u64))?;
            for resistor in resistors {
                fingerprint_value(writer, &(resistor.first - net.net.node_start))?;
                fingerprint_value(writer, &(resistor.second - net.net.node_start))?;
                fingerprint_f64(writer, resistor.resistance_ohms)?;
            }
            let connections = net
                .connections()
                .expect("validated parasitic connection range");
            fingerprint_value(writer, &(connections.len() as u64))?;
            for connection in connections {
                fingerprint_value(
                    writer,
                    net.store
                        .name(connection.object)
                        .expect("validated parasitic connection name"),
                )?;
                fingerprint_value(writer, &(connection.node - net.net.node_start))?;
                fingerprint_value(writer, &connection.role)?;
                fingerprint_f64_pair(writer, connection.pin_capacitance_farads)?;
                fingerprint_optional_f64_pair(writer, connection.delay)?;
                fingerprint_optional_f64_pair(writer, connection.transition)?;
            }
        }
        Ok(())
    }
}

fn canonical_range(
    start: u32,
    count: u32,
    expected_start: usize,
    arena_len: usize,
    kind: &str,
) -> Result<std::ops::Range<usize>, crate::TimingError> {
    if start as usize != expected_start {
        return Err(invalid_net(
            "<database>",
            format!("parasitic {kind} ranges are not contiguous"),
        ));
    }
    checked_range(start, count, arena_len)
}

fn valid_response(response: Option<[f64; 2]>) -> bool {
    response.is_none_or(|values| {
        values
            .into_iter()
            .all(|value| value.is_finite() && value >= 0.0)
    })
}

impl ParasiticStore {
    fn name(&self, id: NameId) -> Option<&str> {
        (id != NameId::default())
            .then(|| self.names.resolve(id))
            .flatten()
    }

    fn net_index(&self, name: &str) -> Option<u32> {
        let mut start = 0usize;
        let mut end = self.nets.len();
        while start < end {
            let middle = start + (end - start) / 2;
            match self.name(self.nets.get(middle)?.name)?.cmp(name) {
                Ordering::Less => start = middle + 1,
                Ordering::Greater => end = middle,
                Ordering::Equal => return u32::try_from(middle).ok(),
            }
        }
        None
    }

    fn net_ref(&self, index: u32) -> Option<ParasiticNetRef<'_>> {
        self.net_ref_at(usize::try_from(index).ok()?)
    }

    fn net_ref_at(&self, index: usize) -> Option<ParasiticNetRef<'_>> {
        Some(ParasiticNetRef {
            store: self,
            net: self.nets.get(index)?,
        })
    }
}

impl<'a> ParasiticNetRef<'a> {
    #[cfg(test)]
    pub(crate) fn total_capacitance(self) -> f64 {
        self.net.total_capacitance
    }

    pub(crate) fn pin_capacitance_included(self) -> bool {
        self.net.load_annotated && self.net.pin_capacitance_included
    }

    pub(crate) fn annotated_capacitance(self) -> Option<f64> {
        self.net
            .load_annotated
            .then_some(self.net.total_capacitance)
    }

    #[cfg(test)]
    pub(super) fn delay_model(self) -> ParasiticDelayModel {
        self.net.delay_model
    }

    pub(crate) fn sink_delay(self, object: &str, edge: TimingEdge) -> Option<f64> {
        self.connection(object)
            .and_then(|connection| connection.delay)
            .map(|delay| delay[edge.index()])
    }

    /// Looks up an `instance/pin` sink without materializing the joined name.
    pub(crate) fn sink_delay_parts(
        self,
        instance: &str,
        pin: &str,
        edge: TimingEdge,
    ) -> Option<f64> {
        self.connection_parts(instance, pin)
            .and_then(|connection| connection.delay)
            .map(|delay| delay[edge.index()])
    }

    #[cfg(test)]
    pub(crate) fn sink_transition(self, object: &str, edge: TimingEdge) -> Option<f64> {
        self.connection(object)
            .and_then(|connection| connection.transition)
            .map(|transition| transition[edge.index()])
    }

    /// Looks up an `instance/pin` sink transition without joining the name.
    pub(crate) fn sink_transition_parts(
        self,
        instance: &str,
        pin: &str,
        edge: TimingEdge,
    ) -> Option<f64> {
        self.connection_parts(instance, pin)
            .and_then(|connection| connection.transition)
            .map(|transition| transition[edge.index()])
    }

    pub(crate) fn sink_names(self) -> impl Iterator<Item = &'a str> + 'a {
        self.connections()
            .unwrap_or_default()
            .iter()
            .filter(|connection| connection.role == RcConnectionRole::Sink)
            .filter_map(|connection| self.store.name(connection.object))
    }

    fn connection(self, object: &str) -> Option<&'a ParasiticConnection> {
        self.connection_with_role(RcConnectionRole::Sink, object)
    }

    fn connection_parts(self, instance: &str, pin: &str) -> Option<&'a ParasiticConnection> {
        self.connections()
            .ok()?
            .binary_search_by(|connection| {
                connection.role.cmp(&RcConnectionRole::Sink).then_with(|| {
                    self.store
                        .name(connection.object)
                        .map_or(Ordering::Less, |candidate| {
                            candidate.bytes().cmp(
                                instance
                                    .bytes()
                                    .chain(std::iter::once(b'/'))
                                    .chain(pin.bytes()),
                            )
                        })
                })
            })
            .ok()
            .and_then(|index| self.connections().ok()?.get(index))
    }

    fn connection_with_role(
        self,
        role: RcConnectionRole,
        object: &str,
    ) -> Option<&'a ParasiticConnection> {
        let connections = self.connections().ok()?;
        let index = connections
            .binary_search_by(|connection| {
                connection.role.cmp(&role).then_with(|| {
                    self.store
                        .name(connection.object)
                        .map_or(Ordering::Less, |candidate| candidate.cmp(object))
                })
            })
            .ok()?;
        connections.get(index)
    }

    fn name(self) -> Option<&'a str> {
        self.store.name(self.net.name)
    }

    fn required_name(self, id: NameId) -> Result<&'a str, crate::TimingError> {
        self.store
            .name(id)
            .ok_or_else(|| invalid_net("<database>", "parasitic name ID is out of bounds"))
    }

    fn nodes(self) -> Result<&'a [ParasiticNode], crate::TimingError> {
        let range = checked_range(
            self.net.node_start,
            self.net.node_count,
            self.store.nodes.len(),
        )?;
        Ok(&self.store.nodes[range])
    }

    fn resistors(self) -> Result<&'a [ParasiticResistor], crate::TimingError> {
        let range = checked_range(
            self.net.resistor_start,
            self.net.resistor_count,
            self.store.resistors.len(),
        )?;
        Ok(&self.store.resistors[range])
    }

    fn connections(self) -> Result<&'a [ParasiticConnection], crate::TimingError> {
        let range = checked_range(
            self.net.connection_start,
            self.net.connection_count,
            self.store.connections.len(),
        )?;
        Ok(&self.store.connections[range])
    }
}

fn same_float_bits(left: f64, right: f64) -> bool {
    left.to_bits() == right.to_bits()
}

fn required_store_name(names: &NameTable, id: NameId) -> Result<&str, crate::TimingError> {
    if id == NameId::default() {
        return Err(invalid_net(
            "<database>",
            "parasitic records must not reference the reserved empty name",
        ));
    }
    names
        .resolve(id)
        .ok_or_else(|| invalid_net("<database>", "parasitic name ID is out of bounds"))
}

fn rebase_node(node: u32, old_start: u32, new_start: u32) -> Result<u32, crate::TimingError> {
    node.checked_sub(old_start)
        .and_then(|local| new_start.checked_add(local))
        .ok_or_else(|| capacity("parasitic node arena"))
}

fn required_unit(unit: Option<f64>, name: &str) -> Result<f64, crate::TimingError> {
    unit.filter(|unit| unit.is_finite() && *unit > 0.0)
        .ok_or_else(|| invalid_net("<library>", format!("{name} unit is required")))
}

fn checked_start(value: usize, resource: &'static str) -> Result<u32, crate::TimingError> {
    u32::try_from(value).map_err(|_| capacity(resource))
}

fn weighted_bytes<T>(len: usize) -> Result<u64, crate::TimingError> {
    let row_bytes = u64::try_from(size_of::<T>()).map_err(|_| capacity("parasitic run weight"))?;
    u64::try_from(len)
        .ok()
        .and_then(|len| len.checked_mul(row_bytes))
        .ok_or_else(|| capacity("parasitic run weight"))
}

fn checked_range(
    start: u32,
    count: u32,
    length: usize,
) -> Result<std::ops::Range<usize>, crate::TimingError> {
    let start = start as usize;
    let end = start
        .checked_add(count as usize)
        .filter(|end| *end <= length)
        .ok_or_else(|| invalid_net("<database>", "parasitic arena range is out of bounds"))?;
    Ok(start..end)
}

fn capacity(resource: &'static str) -> crate::TimingError {
    TimingModelError::Capacity { resource }.into()
}

#[cfg(test)]
mod checkpoint_tests {
    use super::*;

    fn valid_parasitics() -> Parasitics {
        let mut names = NameTable::new();
        let net = names.intern("net").unwrap();
        let node = names.intern("node").unwrap();
        let pin = names.intern("pin").unwrap();
        Parasitics {
            capacitance_unit_farads: 1e-12,
            time_unit_seconds: 1e-9,
            net_count: 1,
            base_override_count: 0,
            base: Arc::new(ParasiticStore {
                names,
                nets: Box::new([ParasiticNet {
                    name: net,
                    total_capacitance: 0.1,
                    load_annotated: true,
                    delay_model: ParasiticDelayModel::Elmore,
                    pin_capacitance_included: false,
                    node_start: 0,
                    node_count: 1,
                    resistor_start: 0,
                    resistor_count: 0,
                    connection_start: 0,
                    connection_count: 1,
                }]),
                nodes: Box::new([ParasiticNode {
                    name: node,
                    ground_capacitance_farads: 0.1e-12,
                }]),
                resistors: Box::new([]),
                connections: Box::new([ParasiticConnection {
                    object: pin,
                    node: 0,
                    role: RcConnectionRole::Driver,
                    pin_capacitance_farads: [0.0; 2],
                    delay: None,
                    transition: None,
                }]),
            }),
            runs: Box::new([]),
            fingerprint: Arc::new(OnceLock::new()),
        }
    }

    #[test]
    fn checkpoint_validation_covers_dense_parasitic_references() {
        let mut parasitics = valid_parasitics();
        assert!(parasitics.validate_checkpoint().is_ok());

        Arc::get_mut(&mut parasitics.base).unwrap().connections[0].node = 1;
        assert!(parasitics.validate_checkpoint().is_err());

        let mut parasitics = valid_parasitics();
        Arc::get_mut(&mut parasitics.base).unwrap().nets[0].node_start = 1;
        assert!(parasitics.validate_checkpoint().is_err());

        let mut parasitics = valid_parasitics();
        Arc::get_mut(&mut parasitics.base).unwrap().nodes[0].name = NameId::from_index(99).unwrap();
        assert!(parasitics.validate_checkpoint().is_err());
    }
}
