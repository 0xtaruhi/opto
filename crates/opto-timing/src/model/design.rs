// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Compact immutable timing design with sparse region replacements.
//!
//! The sealed base interns instance, cell, and pin names once. Region edits own
//! only changed rows and a hash-narrowed exact-name index; unchanged sibling
//! views continue sharing the base. Compaction is the explicit boundary that
//! rebuilds a new base and invalidates no stable instance ID.

use super::{TimingDesign, TimingInstance, TimingPort};
use opto_core::{NameId, NameTable};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Debug, Clone, Copy)]
struct CompactInstanceRow {
    id: crate::TimingInstanceId,
    name: NameId,
    cell: NameId,
    connection_start: u32,
    connection_len: u32,
}

#[derive(Debug, Clone, Copy)]
struct CompactConnectionRow {
    pin: NameId,
}

#[derive(Debug, Clone, Copy)]
struct InstanceNameRow {
    name: NameId,
    position: u32,
}

#[derive(Debug)]
struct CompactTimingDesign {
    id: crate::DesignId,
    name: String,
    ports: Vec<TimingPort>,
    names: NameTable,
    instances: Box<[CompactInstanceRow]>,
    connections: Box<[CompactConnectionRow]>,
    instance_names: Box<[InstanceNameRow]>,
}

impl CompactTimingDesign {
    fn instance(&self, position: usize) -> Option<TimingInstanceView<'_>> {
        let row = *self.instances.get(position)?;
        Some(self.view(row))
    }

    fn view(&self, row: CompactInstanceRow) -> TimingInstanceView<'_> {
        let start = row.connection_start as usize;
        let end = start + row.connection_len as usize;
        TimingInstanceView {
            id: row.id,
            name: self.resolve(row.name),
            cell: self.resolve(row.cell),
            connections: TimingConnectionRows::Compact {
                names: &self.names,
                rows: &self.connections[start..end],
            },
        }
    }

    fn positions_for_name(&self, name: &str) -> impl Iterator<Item = usize> + '_ {
        let name = self.names.get(name);
        let start = name.map_or(self.instance_names.len(), |name| {
            self.instance_names.partition_point(|row| row.name < name)
        });
        self.instance_names[start..]
            .iter()
            .take_while(move |row| Some(row.name) == name)
            .map(|row| row.position as usize)
    }

    fn resolve(&self, name: NameId) -> &str {
        self.names
            .resolve(name)
            .expect("compact timing design name IDs remain live")
    }

    fn memory_bytes(&self) -> usize {
        opto_core::resident::allocation_bytes(self.name.len())
            .saturating_add(
                opto_core::resident::slice_bytes::<TimingPort>(self.ports.len()).saturating_add(
                    self.ports
                        .iter()
                        .map(|port| {
                            opto_core::resident::allocation_bytes(port.name.len()).saturating_add(
                                opto_core::resident::allocation_bytes(port.net.name().len()),
                            )
                        })
                        .sum(),
                ),
            )
            .saturating_add(self.names.owned_memory_bytes())
            .saturating_add(opto_core::resident::slice_bytes::<CompactInstanceRow>(
                self.instances.len(),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<CompactConnectionRow>(
                self.connections.len(),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<InstanceNameRow>(
                self.instance_names.len(),
            ))
    }
}

pub(super) struct CompactTimingDesignBuilder {
    names: NameTable,
    instances: Vec<CompactInstanceRow>,
    connections: Vec<CompactConnectionRow>,
    instance_names: Vec<InstanceNameRow>,
}

impl CompactTimingDesignBuilder {
    pub(super) fn new(instance_capacity: usize) -> Self {
        Self {
            names: NameTable::new(),
            instances: Vec::with_capacity(instance_capacity),
            connections: Vec::new(),
            instance_names: Vec::with_capacity(instance_capacity),
        }
    }

    pub(super) fn push<'a>(
        &mut self,
        id: crate::TimingInstanceId,
        name: &str,
        cell: &str,
        pins: impl ExactSizeIterator<Item = &'a str>,
    ) -> Result<(), crate::TimingError> {
        let position = u32::try_from(self.instances.len()).map_err(|_| design_capacity())?;
        let connection_start =
            u32::try_from(self.connections.len()).map_err(|_| connection_capacity())?;
        let connection_len = u32::try_from(pins.len()).map_err(|_| connection_capacity())?;
        connection_start
            .checked_add(connection_len)
            .ok_or_else(connection_capacity)?;
        let name = self.intern(name)?;
        let cell = self.intern(cell)?;
        for pin in pins {
            let pin = self.intern(pin)?;
            self.connections.push(CompactConnectionRow { pin });
        }
        self.instances.push(CompactInstanceRow {
            id,
            name,
            cell,
            connection_start,
            connection_len,
        });
        self.instance_names.push(InstanceNameRow { name, position });
        Ok(())
    }

    fn finish(
        mut self,
        id: crate::DesignId,
        name: String,
        ports: Vec<TimingPort>,
    ) -> CompactTimingDesign {
        self.names.compact();
        self.instances.shrink_to_fit();
        self.connections.shrink_to_fit();
        self.instance_names
            .sort_unstable_by_key(|row| (row.name, row.position));
        self.instance_names.shrink_to_fit();
        CompactTimingDesign {
            id,
            name,
            ports,
            names: self.names,
            instances: self.instances.into_boxed_slice(),
            connections: self.connections.into_boxed_slice(),
            instance_names: self.instance_names.into_boxed_slice(),
        }
    }

    fn intern(&mut self, name: &str) -> Result<NameId, crate::TimingError> {
        self.names.intern(name).map_err(|_| name_capacity())
    }
}

/// Borrowed compact timing-instance row.
#[derive(Clone, Copy)]
pub(crate) struct TimingInstanceView<'a> {
    pub(crate) id: crate::TimingInstanceId,
    pub(crate) name: &'a str,
    pub(crate) cell: &'a str,
    connections: TimingConnectionRows<'a>,
}

#[derive(Clone, Copy)]
enum TimingConnectionRows<'a> {
    Compact {
        names: &'a NameTable,
        rows: &'a [CompactConnectionRow],
    },
    Owned(&'a [String]),
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TimingConnectionView<'a> {
    pub(crate) pin: &'a str,
}

#[derive(Debug, Clone)]
pub(crate) struct OwnedTimingInstance {
    pub(crate) id: crate::TimingInstanceId,
    pub(crate) name: String,
    pub(crate) cell: String,
    pins: Vec<String>,
}

impl OwnedTimingInstance {
    pub(crate) fn from_timing(instance: TimingInstance) -> Self {
        Self {
            id: instance.id,
            name: instance.name,
            cell: instance.cell,
            pins: instance
                .connections
                .into_iter()
                .map(|connection| connection.pin)
                .collect(),
        }
    }

    fn memory_bytes(&self) -> usize {
        opto_core::resident::allocation_bytes(self.name.len())
            .saturating_add(opto_core::resident::allocation_bytes(self.cell.len()))
            .saturating_add(opto_core::resident::slice_bytes::<String>(self.pins.len()))
            .saturating_add(
                self.pins
                    .iter()
                    .map(|pin| opto_core::resident::allocation_bytes(pin.len()))
                    .sum(),
            )
    }
}

impl<'a> TimingInstanceView<'a> {
    fn owned(instance: &'a OwnedTimingInstance) -> Self {
        Self {
            id: instance.id,
            name: &instance.name,
            cell: &instance.cell,
            connections: TimingConnectionRows::Owned(&instance.pins),
        }
    }

    pub(crate) fn connection_count(self) -> usize {
        match self.connections {
            TimingConnectionRows::Compact { rows, .. } => rows.len(),
            TimingConnectionRows::Owned(rows) => rows.len(),
        }
    }

    pub(crate) fn connections(self) -> impl ExactSizeIterator<Item = TimingConnectionView<'a>> {
        (0..self.connection_count()).map(move |index| {
            self.connection(index)
                .expect("compact timing connection index is in bounds")
        })
    }

    pub(crate) fn connection(self, index: usize) -> Option<TimingConnectionView<'a>> {
        match self.connections {
            TimingConnectionRows::Compact { names, rows } => {
                let row = rows.get(index)?;
                Some(TimingConnectionView {
                    pin: names.resolve(row.pin)?,
                })
            }
            TimingConnectionRows::Owned(rows) => {
                let row = rows.get(index)?;
                Some(TimingConnectionView { pin: row })
            }
        }
    }

    pub(crate) fn to_owned_row(self) -> OwnedTimingInstance {
        OwnedTimingInstance {
            id: self.id,
            name: self.name.to_string(),
            cell: self.cell.to_string(),
            pins: self
                .connections()
                .map(|connection| connection.pin.to_string())
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct OverrideNameRow {
    hash: u64,
    position: u32,
}

/// Shared immutable compact design with sparse owned replacements for region edits.
#[derive(Debug)]
pub(crate) struct SharedTimingDesign {
    base: Arc<CompactTimingDesign>,
    overrides: BTreeMap<usize, OwnedTimingInstance>,
    override_names: Vec<OverrideNameRow>,
    changed: Vec<u64>,
    len: usize,
}

impl SharedTimingDesign {
    /// Consumes an owned construction design into the compact shared base.
    pub(crate) fn seal(design: TimingDesign) -> Result<Self, crate::TimingError> {
        let TimingDesign {
            id,
            name,
            ports,
            instances,
        } = design;
        let mut builder = CompactTimingDesignBuilder::new(instances.len());
        for instance in instances {
            builder.push(
                instance.id,
                &instance.name,
                &instance.cell,
                instance
                    .connections
                    .iter()
                    .map(|connection| connection.pin.as_str()),
            )?;
        }
        Ok(Self::from_builder(builder, id, name, ports))
    }

    pub(super) fn from_builder(
        builder: CompactTimingDesignBuilder,
        id: crate::DesignId,
        name: String,
        ports: Vec<TimingPort>,
    ) -> Self {
        let base = builder.finish(id, name, ports);
        let len = base.instances.len();
        Self {
            base: Arc::new(base),
            overrides: BTreeMap::new(),
            override_names: Vec::new(),
            changed: Vec::new(),
            len,
        }
    }

    /// Forks only a quiescent base with no unsealed sparse replacements.
    pub(crate) fn fork_shared(&self) -> Option<Self> {
        self.overrides.is_empty().then(|| Self {
            base: Arc::clone(&self.base),
            overrides: BTreeMap::new(),
            override_names: Vec::new(),
            changed: Vec::new(),
            len: self.len,
        })
    }

    pub(crate) fn id(&self) -> crate::DesignId {
        self.base.id
    }

    pub(crate) fn name(&self) -> &str {
        &self.base.name
    }

    pub(crate) fn ports(&self) -> &[TimingPort] {
        &self.base.ports
    }

    pub(crate) fn instance_count(&self) -> usize {
        self.len
    }

    pub(crate) fn instance(&self, position: usize) -> Option<TimingInstanceView<'_>> {
        if position >= self.len {
            return None;
        }
        if !self.row_changed(position) {
            return self.base.instance(position);
        }
        self.overrides.get(&position).map(TimingInstanceView::owned)
    }

    pub(crate) fn instances(&self) -> impl ExactSizeIterator<Item = TimingInstanceView<'_>> {
        (0..self.len).map(|position| {
            self.instance(position)
                .expect("live timing design rows remain populated")
        })
    }

    /// Replaces one dense position while returning an owned rollback row.
    pub(crate) fn replace(
        &mut self,
        position: usize,
        replacement: OwnedTimingInstance,
    ) -> Option<OwnedTimingInstance> {
        let old = self.instance(position)?.to_owned_row();
        self.set_override(position, replacement);
        Some(old)
    }

    pub(crate) fn swap_remove(&mut self, position: usize) -> Option<OwnedTimingInstance> {
        let removed = self.instance(position)?.to_owned_row();
        let last = self.len.checked_sub(1)?;
        if position != last {
            let moved = self
                .instance(last)
                .expect("last live timing design row exists")
                .to_owned_row();
            self.set_override(position, moved);
        }
        self.remove_override(last);
        self.mark_changed(last, false);
        self.len = last;
        Some(removed)
    }

    pub(crate) fn push(&mut self, instance: OwnedTimingInstance) -> Result<(), crate::TimingError> {
        if self.len >= u32::MAX as usize {
            return Err(design_capacity());
        }
        let position = self.len;
        self.len += 1;
        self.set_override(position, instance);
        Ok(())
    }

    pub(crate) fn pop(&mut self) -> Option<OwnedTimingInstance> {
        let last = self.len.checked_sub(1)?;
        let removed = self.instance(last)?.to_owned_row();
        self.remove_override(last);
        self.mark_changed(last, false);
        self.len = last;
        Some(removed)
    }

    /// Seals all live rows into a new shared base without changing stable IDs.
    ///
    /// Returns the byte size of the replaced base for construction high-water
    /// accounting; it is not allocator telemetry or retained memory.
    pub(crate) fn compact(&mut self) -> Result<usize, crate::TimingError> {
        if self.overrides.is_empty() && self.len == self.base.instances.len() {
            return Ok(0);
        }
        let replaced_base_bytes = self.shared_memory_bytes();
        let mut builder = CompactTimingDesignBuilder::new(self.len);
        for instance in self.instances() {
            builder.push(
                instance.id,
                instance.name,
                instance.cell,
                instance.connections().map(|connection| connection.pin),
            )?;
        }
        self.base = Arc::new(builder.finish(
            self.base.id,
            self.base.name.clone(),
            self.base.ports.clone(),
        ));
        self.overrides.clear();
        self.override_names.clear();
        self.override_names.shrink_to_fit();
        self.changed.clear();
        self.changed.shrink_to_fit();
        Ok(replaced_base_bytes)
    }

    pub(crate) fn instance_id(&self, name: &str) -> Option<crate::TimingInstanceId> {
        self.instance_position(name)
            .and_then(|position| self.instance(position))
            .map(|instance| instance.id)
    }

    pub(crate) fn instance_position(&self, name: &str) -> Option<usize> {
        let hash = instance_name_hash(name);
        let start = self.override_names.partition_point(|row| row.hash < hash);
        let mut best = self.override_names[start..]
            .iter()
            .take_while(|row| row.hash == hash)
            .filter_map(|row| {
                let position = row.position as usize;
                self.overrides
                    .get(&position)
                    .filter(|instance| position < self.len && instance.name == name)
                    .map(|instance| (position, instance.id))
            })
            .min_by_key(|&(position, _)| position);
        for position in self.base.positions_for_name(name) {
            if position >= self.len || self.row_changed(position) {
                continue;
            }
            let id = self.base.instances[position].id;
            if best.is_none_or(|(best, _)| position < best) {
                best = Some((position, id));
            }
        }
        best.map(|(position, _)| position)
    }

    pub(crate) fn shared_identity(&self) -> usize {
        Arc::as_ptr(&self.base) as usize
    }

    pub(crate) fn shared_memory_bytes(&self) -> usize {
        std::mem::size_of::<CompactTimingDesign>().saturating_add(self.base.memory_bytes())
    }

    pub(crate) fn exclusive_memory_bytes(&self) -> usize {
        self.overrides
            .values()
            .map(OwnedTimingInstance::memory_bytes)
            .sum::<usize>()
            .saturating_add(opto_core::resident::slice_bytes::<u64>(self.changed.len()))
            .saturating_add(super::btree_memory_bytes::<usize, OwnedTimingInstance>(
                self.overrides.len(),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<OverrideNameRow>(
                self.override_names.len(),
            ))
    }

    fn row_changed(&self, position: usize) -> bool {
        self.changed
            .get(position / 64)
            .is_some_and(|word| word & (1_u64 << (position % 64)) != 0)
    }

    fn set_override(&mut self, position: usize, instance: OwnedTimingInstance) {
        self.remove_override(position);
        let row = OverrideNameRow {
            hash: instance_name_hash(&instance.name),
            position: u32::try_from(position)
                .expect("timing design row positions were capacity-validated"),
        };
        let index = self
            .override_names
            .binary_search(&row)
            .unwrap_or_else(|index| index);
        self.override_names.insert(index, row);
        self.overrides.insert(position, instance);
        self.mark_changed(position, true);
    }

    fn remove_override(&mut self, position: usize) {
        let Some(instance) = self.overrides.remove(&position) else {
            return;
        };
        let row = OverrideNameRow {
            hash: instance_name_hash(&instance.name),
            position: u32::try_from(position)
                .expect("timing design row positions were capacity-validated"),
        };
        let index = self
            .override_names
            .binary_search(&row)
            .expect("every sparse timing row has one exact-name index entry");
        self.override_names.remove(index);
    }

    fn mark_changed(&mut self, position: usize, changed: bool) {
        let word = position / 64;
        if changed && word >= self.changed.len() {
            self.changed.resize(word + 1, 0);
        }
        let Some(bits) = self.changed.get_mut(word) else {
            return;
        };
        let mask = 1_u64 << (position % 64);
        if changed {
            *bits |= mask;
        } else {
            *bits &= !mask;
            while self.changed.last() == Some(&0) {
                self.changed.pop();
            }
        }
    }
}

fn name_capacity() -> crate::TimingError {
    crate::TimingModelError::Capacity {
        resource: "timing design name arena",
    }
    .into()
}

fn design_capacity() -> crate::TimingError {
    crate::TimingModelError::Capacity {
        resource: "timing design row arena",
    }
    .into()
}

fn connection_capacity() -> crate::TimingError {
    crate::TimingModelError::Capacity {
        resource: "timing connection arena",
    }
    .into()
}

fn instance_name_hash(name: &str) -> u64 {
    let digest = blake3::hash(name.as_bytes());
    u64::from_le_bytes(
        digest.as_bytes()[..8]
            .try_into()
            .expect("BLAKE3 digest width"),
    )
}

/// Lightweight borrowed view over a shared design and its sparse instance rows.
#[derive(Debug, Clone, Copy)]
pub struct TimingDesignView<'a> {
    pub(crate) model: &'a super::TimingModel,
}

impl<'a> TimingDesignView<'a> {
    #[must_use]
    /// Returns the persistent design identity.
    pub fn id(self) -> crate::DesignId {
        self.model.design.id()
    }

    #[must_use]
    /// Returns the design name.
    pub fn name(self) -> &'a str {
        self.model.design.name()
    }

    #[must_use]
    /// Returns design ports in stable order.
    pub fn ports(self) -> &'a [TimingPort] {
        self.model.design.ports()
    }

    #[must_use]
    /// Materializes an owned timing design.
    pub fn to_owned(self) -> TimingDesign {
        self.model.owned_design()
    }
}
