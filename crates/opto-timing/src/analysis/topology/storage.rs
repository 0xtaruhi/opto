// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use std::borrow::Cow;
use std::ops::Index;
use std::sync::Arc;

mod cells;

pub(super) use cells::*;

macro_rules! compact_graph_id {
    ($name:ident, $resource:literal) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[repr(transparent)]
        pub(in crate::analysis) struct $name(u32);

        impl $name {
            pub(in crate::analysis) fn from_index(
                index: usize,
            ) -> Result<Self, crate::TimingError> {
                u32::try_from(index).map(Self).map_err(|_| {
                    crate::TimingModelError::Capacity {
                        resource: $resource,
                    }
                    .into()
                })
            }

            pub(in crate::analysis) const fn index(self) -> usize {
                self.0 as usize
            }
        }
    };
}

compact_graph_id!(GraphArcId, "graph arc ID");
compact_graph_id!(GraphPinId, "graph pin ID");
compact_graph_id!(GraphLibraryArcId, "graph library arc ID");

/// Stable graph-arc slots with deterministic reuse after an edit commits.
///
/// Removed records remain readable until the owning region edit commits or
/// rolls back. A commit turns those slots into reusable tombstones; a rollback
/// discards only records allocated by that edit. Adjacency rows therefore keep
/// compact typed IDs without duplicating the arc payload.
#[derive(Debug, Default)]
pub(super) struct GraphArcArena {
    base: Arc<[GraphArcTopology]>,
    overlay: Vec<GraphArcTopology>,
    values: Vec<GraphArcValues>,
    live: Vec<u8>,
    free_overlay: Vec<GraphArcId>,
    live_count: usize,
}

impl GraphArcArena {
    pub(super) fn shared_identity(&self) -> usize {
        Arc::as_ptr(&self.base).cast::<GraphArcTopology>() as usize
    }

    pub(super) fn insert(
        &mut self,
        topology: GraphArcTopology,
        values: GraphArcValues,
    ) -> Result<GraphArcId, crate::TimingError> {
        let id = if let Some(id) = self.free_overlay.pop() {
            self.overlay[id.index() - self.base.len()] = topology;
            self.values[id.index()] = values;
            self.live[id.index()] = 1;
            id
        } else {
            let id = GraphArcId::from_index(self.len())?;
            self.overlay.push(topology);
            self.values.push(values);
            self.live.push(1);
            id
        };
        self.live_count += 1;
        Ok(id)
    }

    pub(super) fn get(&self, id: GraphArcId) -> Option<GraphArcRef<'_>> {
        if self.live.get(id.index()) != Some(&1) {
            return None;
        }
        let topology = if id.index() < self.base.len() {
            &self.base[id.index()]
        } else {
            &self.overlay[id.index() - self.base.len()]
        };
        Some(GraphArcRef {
            topology,
            values: &self.values[id.index()],
        })
    }

    pub(super) fn len(&self) -> usize {
        self.base.len() + self.overlay.len()
    }

    pub(super) fn seal_base(&mut self) {
        assert!(self.base.is_empty() && self.free_overlay.is_empty());
        self.base = std::mem::take(&mut self.overlay).into();
    }

    pub(super) fn shared_memory_bytes(&self) -> usize {
        opto_core::resident::slice_bytes::<GraphArcTopology>(self.base.len())
    }

    pub(super) fn fork_base_with(
        &self,
        mut values: impl FnMut(&GraphArcTopology) -> GraphArcValues,
    ) -> Option<Self> {
        if !self.overlay.is_empty() || self.live.iter().any(|&live| live != 1) {
            return None;
        }
        Some(Self {
            base: Arc::clone(&self.base),
            overlay: Vec::new(),
            values: self.base.iter().map(&mut values).collect(),
            live: vec![1; self.base.len()],
            free_overlay: Vec::new(),
            live_count: self.base.len(),
        })
    }

    pub(super) fn commit_removals(&mut self, removed: &[GraphArcId]) {
        for &id in removed {
            let live = self
                .live
                .get_mut(id.index())
                .expect("removed graph arc belongs to the current arena");
            assert_eq!(*live, 1, "removed graph arc is live until commit");
            *live = 0;
            self.live_count -= 1;
            if id.index() >= self.base.len() {
                self.free_overlay.push(id);
            }
        }
        self.normalize_free_list();
    }

    pub(super) fn rollback_allocations(&mut self, old_len: usize, allocated: &[GraphArcId]) {
        for &id in allocated {
            let live = self
                .live
                .get_mut(id.index())
                .expect("allocated graph arc belongs to the current arena");
            assert_eq!(*live, 1, "allocated graph arc remains live until rollback");
            *live = 0;
            self.live_count -= 1;
            if id.index() < old_len && id.index() >= self.base.len() {
                self.free_overlay.push(id);
            }
        }
        self.overlay
            .truncate(old_len.saturating_sub(self.base.len()));
        self.values.truncate(old_len);
        self.live.truncate(old_len);
        self.free_overlay.retain(|id| id.index() < old_len);
        self.normalize_free_list();
    }

    /// Packs live records and returns the old-to-new ID map when holes existed.
    pub(super) fn compact(
        &mut self,
    ) -> Result<Option<Vec<Option<GraphArcId>>>, crate::TimingError> {
        if self.free_overlay.is_empty() {
            self.overlay.shrink_to_fit();
            self.values.shrink_to_fit();
            self.live.shrink_to_fit();
            self.free_overlay.shrink_to_fit();
            return Ok(None);
        }
        let mut remap = vec![None; self.len()];
        for (index, slot) in remap.iter_mut().take(self.base.len()).enumerate() {
            if self.live[index] != 0 {
                *slot = Some(GraphArcId::from_index(index)?);
            }
        }
        let mut topology = Vec::with_capacity(self.overlay.len());
        let mut values = self.values[..self.base.len()].to_vec();
        for (offset, &arc) in self.overlay.iter().enumerate() {
            let old = self.base.len() + offset;
            let live = self.live[old];
            if live == 0 {
                continue;
            }
            let new = GraphArcId::from_index(self.base.len() + topology.len())?;
            remap[old] = Some(new);
            topology.push(arc);
            values.push(self.values[old]);
        }
        self.overlay = topology;
        self.values = values;
        self.live = self
            .live
            .iter()
            .take(self.base.len())
            .copied()
            .chain(std::iter::repeat_n(1, self.overlay.len()))
            .collect();
        self.free_overlay.clear();
        self.free_overlay.shrink_to_fit();
        Ok(Some(remap))
    }

    pub(super) fn owned_memory_bytes(&self) -> usize {
        self.shared_memory_bytes()
            .saturating_add(opto_core::resident::slice_bytes::<GraphArcTopology>(
                self.overlay.len(),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<GraphArcValues>(
                self.values.len(),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<u8>(self.live.len()))
            .saturating_add(opto_core::resident::slice_bytes::<GraphArcId>(
                self.free_overlay.len(),
            ))
    }

    fn normalize_free_list(&mut self) {
        self.free_overlay
            .sort_unstable_by(|left, right| right.cmp(left));
        self.free_overlay.dedup();
    }
}

/// Immutable dense base with an append-only sparse region layer.
///
/// Prepared views clone only the base `Arc`. Region edits append new dense IDs
/// and rollback truncates only the append layer. A deterministic compact seals
/// committed appends as a new immutable base without changing IDs.
#[derive(Debug, Clone)]
pub(in crate::analysis) struct SharedAppendVec<T> {
    base: Arc<[T]>,
    appended: Vec<T>,
}

impl<T> Default for SharedAppendVec<T> {
    fn default() -> Self {
        Self {
            base: Arc::from([]),
            appended: Vec::new(),
        }
    }
}

impl<T> SharedAppendVec<T> {
    pub(super) fn fork_shared(&self) -> Option<Self> {
        self.appended.is_empty().then(|| Self {
            base: Arc::clone(&self.base),
            appended: Vec::new(),
        })
    }

    pub(super) fn len(&self) -> usize {
        self.base.len() + self.appended.len()
    }

    pub(super) fn get(&self, index: usize) -> Option<&T> {
        if index < self.base.len() {
            self.base.get(index)
        } else {
            self.appended.get(index - self.base.len())
        }
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &T> {
        self.base.iter().chain(self.appended.iter())
    }

    pub(super) fn push(&mut self, value: T) {
        self.appended.push(value);
    }

    pub(super) fn truncate(&mut self, len: usize)
    where
        T: Clone,
    {
        if let Some(appended) = len.checked_sub(self.base.len()) {
            self.appended.truncate(appended);
            return;
        }
        // Sealing committed appends into a new base can move the base past a
        // rollback target recorded earlier in the same edit. Other prepared
        // views still share this base, so shorten a private copy instead of
        // mutating the shared allocation.
        self.base = Arc::from(&self.base[..len]);
        self.appended.clear();
    }

    pub(super) fn replace_base(&mut self, values: Vec<T>) {
        self.base = values.into();
        self.appended.clear();
    }

    pub(super) fn shared_identity(&self) -> usize {
        Arc::as_ptr(&self.base).cast::<T>() as usize
    }

    pub(super) fn shared_memory_bytes(&self) -> usize {
        opto_core::resident::slice_bytes::<T>(self.base.len())
    }
}

impl<T: Clone> SharedAppendVec<T> {
    pub(super) fn compact(&mut self) {
        if self.appended.is_empty() {
            return;
        }
        self.base = self.iter().cloned().collect::<Vec<_>>().into();
        self.appended.clear();
        self.appended.shrink_to_fit();
    }

    pub(super) fn retain(&mut self, mut keep: impl FnMut(&T) -> bool) {
        let values = self
            .iter()
            .filter(|value| keep(value))
            .cloned()
            .collect::<Vec<_>>();
        self.replace_base(values);
    }
}

impl<T> Index<usize> for SharedAppendVec<T> {
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        self.get(index).expect("shared append index is in bounds")
    }
}

impl<'a, T> IntoIterator for &'a SharedAppendVec<T> {
    type Item = &'a T;
    type IntoIter = std::iter::Chain<std::slice::Iter<'a, T>, std::slice::Iter<'a, T>>;

    fn into_iter(self) -> Self::IntoIter {
        self.base.iter().chain(self.appended.iter())
    }
}

/// Interned immutable net names with an append-only sparse region layer.
///
/// The sealed table owns base text in one compact byte arena and already
/// provides exact hash lookup, so a second name-sorted index is unnecessary.
/// Region edits retain owned text only for newly appended nets; rollback pops
/// those rows, while compaction rebuilds one sealed table without changing net
/// IDs. `empty_base_net` accounts for `NameTable`'s reserved empty spelling
/// without spending a persistent ID row per net.
#[derive(Debug, Clone)]
pub(crate) struct SharedNetNames {
    base: Arc<opto_core::NameTable>,
    base_len: usize,
    empty_base_net: Option<u32>,
    appended: Vec<String>,
    appended_by_name: Vec<u32>,
}

impl SharedNetNames {
    fn from_names<'a>(
        names: impl ExactSizeIterator<Item = Cow<'a, str>>,
    ) -> Result<Self, crate::TimingError> {
        let base_len = names.len();
        let mut table = opto_core::NameTable::new();
        let mut empty_base_net = None;
        for (net, name) in names.enumerate() {
            if name.is_empty() {
                let net = u32::try_from(net).map_err(|_| net_name_capacity_error())?;
                if empty_base_net.replace(net).is_some() {
                    return Err(duplicate_net_name(&name));
                }
                continue;
            }
            if table.get(&name).is_some() {
                return Err(duplicate_net_name(&name));
            }
            table.intern(&name).map_err(net_name_capacity)?;
        }
        table.compact();
        Ok(Self::seal(table, base_len, empty_base_net))
    }

    pub(super) fn fork_shared(&self) -> Option<Self> {
        self.appended.is_empty().then(|| Self {
            base: Arc::clone(&self.base),
            base_len: self.base_len,
            empty_base_net: self.empty_base_net,
            appended: Vec::new(),
            appended_by_name: Vec::new(),
        })
    }

    pub(super) fn len(&self) -> usize {
        self.base_len + self.appended.len()
    }

    pub(crate) fn get(&self, net: usize) -> Option<&str> {
        if net < self.base_len {
            let name = match self.empty_base_net {
                Some(empty) if net == empty as usize => opto_core::NameId::default(),
                Some(empty) if net > empty as usize => opto_core::NameId::from_index(net).ok()?,
                _ => opto_core::NameId::from_index(net + 1).ok()?,
            };
            self.base.resolve(name)
        } else {
            self.appended.get(net - self.base_len).map(String::as_str)
        }
    }

    pub(super) fn net_id(&self, name: &str) -> Option<usize> {
        self.base_net_id(name).or_else(|| {
            self.appended_by_name
                .binary_search_by(|&net| self[net as usize].cmp(name))
                .ok()
                .map(|position| self.appended_by_name[position] as usize)
        })
    }

    pub(super) fn iter(&self) -> impl ExactSizeIterator<Item = &str> {
        (0..self.len()).map(|net| {
            self.get(net)
                .expect("interned timing net name IDs remain contiguous")
        })
    }

    pub(super) fn push(&mut self, name: String) {
        debug_assert!(self.net_id(&name).is_none());
        let net = u32::try_from(self.len()).expect("timing net capacity was validated");
        let position = self
            .appended_by_name
            .binary_search_by(|&candidate| self[candidate as usize].cmp(&name))
            .expect_err("new region net name was checked before insertion");
        self.appended.push(name);
        self.appended_by_name.insert(position, net);
    }

    pub(super) fn pop(&mut self) -> Option<String> {
        let net = self.len().checked_sub(1)?;
        if net < self.base_len {
            return None;
        }
        // The sparse index is delta-bounded; base names never enter this scan.
        let position = self
            .appended_by_name
            .iter()
            .position(|&candidate| candidate as usize == net)
            .expect("appended net is present in the sparse name index");
        self.appended_by_name.remove(position);
        self.appended.pop()
    }

    pub(super) fn compact(&mut self) -> Result<(), crate::TimingError> {
        if self.appended.is_empty() {
            return Ok(());
        }
        let compacted = Self::from_names(self.iter().map(Cow::Borrowed))?;
        *self = compacted;
        Ok(())
    }

    pub(super) fn shared_identity(&self) -> usize {
        Arc::as_ptr(&self.base) as usize
    }

    pub(super) fn shared_memory_bytes(&self) -> usize {
        opto_core::resident::allocation_bytes(
            std::mem::size_of::<opto_core::NameTable>() + std::mem::size_of::<usize>() * 2,
        )
        .saturating_add(self.base.owned_memory_bytes())
    }

    pub(super) fn owned_memory_bytes(&self) -> usize {
        self.shared_memory_bytes()
            .saturating_add(opto_core::resident::slice_bytes::<String>(
                self.appended.len(),
            ))
            .saturating_add(
                self.appended
                    .iter()
                    .map(|name| opto_core::resident::allocation_bytes(name.len()))
                    .sum(),
            )
            .saturating_add(opto_core::resident::slice_bytes::<u32>(
                self.appended_by_name.len(),
            ))
    }

    fn base_net_id(&self, name: &str) -> Option<usize> {
        if name.is_empty() {
            return self.empty_base_net.map(|net| net as usize);
        }
        let raw = self.base.get(name)?.raw() as usize;
        let net = match self.empty_base_net {
            Some(empty) if raw > empty as usize => raw,
            _ => raw.checked_sub(1)?,
        };
        (net < self.base_len).then_some(net)
    }

    fn seal(table: opto_core::NameTable, base_len: usize, empty_base_net: Option<u32>) -> Self {
        Self {
            base: Arc::new(table),
            base_len,
            empty_base_net,
            appended: Vec::new(),
            appended_by_name: Vec::new(),
        }
    }
}

/// Streaming exact-name builder whose insertion order is the stable timing-net ID.
pub(crate) struct TimingNetNamesBuilder {
    names: opto_core::NameTable,
    len: usize,
    empty_net: Option<u32>,
}

impl TimingNetNamesBuilder {
    pub(crate) fn new() -> Self {
        Self {
            names: opto_core::NameTable::new(),
            len: 0,
            empty_net: None,
        }
    }

    pub(crate) fn intern(&mut self, name: &str) -> Result<crate::TimingNetId, crate::TimingError> {
        if name.is_empty() {
            if let Some(net) = self.empty_net {
                return Ok(crate::TimingNetId::from_raw(net));
            }
            let net = crate::TimingNetId::from_index(self.len)?;
            self.empty_net = Some(net.raw());
            self.len += 1;
            return Ok(net);
        }
        if let Some(name_id) = self.names.get(name) {
            return Ok(crate::TimingNetId::from_raw(self.net_for_name(name_id)?));
        }
        let net = crate::TimingNetId::from_index(self.len)?;
        let name_id = self.names.intern(name).map_err(net_name_capacity)?;
        debug_assert_eq!(
            self.net_for_name(name_id)
                .expect("new interned net name maps to a timing net"),
            net.raw()
        );
        self.len += 1;
        Ok(net)
    }

    pub(crate) fn finish(mut self) -> SharedNetNames {
        self.names.compact();
        SharedNetNames::seal(self.names, self.len, self.empty_net)
    }

    fn net_for_name(&self, name: opto_core::NameId) -> Result<u32, crate::TimingError> {
        let raw = name.raw();
        match self.empty_net {
            Some(empty) if raw > empty => Ok(raw),
            _ => raw.checked_sub(1).ok_or_else(net_name_capacity_error),
        }
    }
}

impl Index<usize> for SharedNetNames {
    type Output = str;

    fn index(&self, index: usize) -> &Self::Output {
        self.get(index).expect("shared net-name index is in bounds")
    }
}

fn duplicate_net_name(name: &str) -> crate::TimingError {
    crate::TimingModelError::InvalidMappedHierarchy {
        detail: format!("timing generation contains duplicate net name '{name}'"),
    }
    .into()
}

fn net_name_capacity(_: opto_core::NameError) -> crate::TimingError {
    net_name_capacity_error()
}

fn net_name_capacity_error() -> crate::TimingError {
    crate::TimingModelError::Capacity {
        resource: "net-name arena",
    }
    .into()
}

/// Stable instance-to-net rows backed by one shared CSR allocation.
///
/// Empty CSR rows are ambiguous by themselves: they can represent either a
/// live cell without connections or an unoccupied stable-ID slot. The shared
/// occupancy bitmap is therefore the source of truth for row liveness. Region
/// edits retain only changed rows in `RowArena`'s dirty pages and detach only
/// the touched page of occupancy words.
#[derive(Debug)]
pub(crate) struct InstanceNetArena {
    rows: opto_core::RowArena<crate::TimingNetId>,
    occupancy: opto_core::PagedCowVec<u64>,
    len: usize,
}

impl InstanceNetArena {
    pub(crate) fn builder(row_count: usize) -> Result<InstanceNetArenaBuilder, crate::TimingError> {
        InstanceNetArenaBuilder::new(row_count)
    }

    pub(super) fn fork_shared(&self) -> Option<Self> {
        Some(Self {
            rows: self.rows.fork_shared()?,
            occupancy: self.occupancy.fork_shared(),
            len: self.len,
        })
    }

    pub(super) fn compact(&mut self) -> Result<(), opto_core::PackedRowsError> {
        self.rows.compact()
    }

    pub(super) fn owned_memory_bytes(&self) -> usize {
        self.rows
            .owned_memory_bytes()
            .saturating_add(self.occupancy.owned_memory_bytes())
    }

    pub(super) fn shared_allocations(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.rows
            .shared_pages()
            .chain(self.occupancy.shared_pages())
    }

    fn is_occupied(&self, index: usize) -> bool {
        index < self.len
            && self
                .occupancy
                .get(index / 64)
                .is_some_and(|word| word & (1_u64 << (index % 64)) != 0)
    }

    pub(crate) fn get(&self, instance: TimingInstanceId) -> Option<&[crate::TimingNetId]> {
        let index = instance.raw() as usize;
        (index < self.len && self.is_occupied(index)).then(|| {
            self.rows
                .get(index)
                .expect("occupied timing instance has one CSR row")
        })
    }

    pub(super) fn insert(
        &mut self,
        instance: TimingInstanceId,
        nets: Box<[crate::TimingNetId]>,
    ) -> Result<Option<Box<[crate::TimingNetId]>>, crate::TimingError> {
        let index = instance.raw() as usize;
        let old = self.get(instance).map(<[crate::TimingNetId]>::to_vec);
        if index >= self.rows.len() {
            self.rows
                .try_reserve_rows(index + 1)
                .map_err(instance_net_capacity)?;
        }
        self.set_occupied(index, true)?;
        if index >= self.rows.len() {
            self.rows
                .resize_empty(index + 1)
                .map_err(instance_net_capacity)?;
        }
        self.rows.replace(index, nets.into_vec());
        self.len = self.len.max(index + 1);
        Ok(old.map(Vec::into_boxed_slice))
    }

    pub(super) fn remove(
        &mut self,
        instance: TimingInstanceId,
    ) -> Result<Option<Box<[crate::TimingNetId]>>, crate::TimingError> {
        let index = instance.raw() as usize;
        let old = self.get(instance).map(<[crate::TimingNetId]>::to_vec);
        if index < self.len {
            self.set_occupied(index, false)?;
            self.rows.replace(index, Vec::new());
        }
        Ok(old.map(Vec::into_boxed_slice))
    }

    pub(super) fn truncate(&mut self, len: usize) -> Result<(), crate::TimingError> {
        if len == self.len {
            return Ok(());
        }
        self.mask_occupancy_tail(len)?;
        self.rows.truncate_empty(len);
        self.len = len;
        self.occupancy.truncate(len.div_ceil(64));
        Ok(())
    }

    pub(super) fn trim(&mut self) -> Result<(), crate::TimingError> {
        let mut len = self.len;
        while len != 0 && !self.is_occupied(len - 1) {
            len -= 1;
        }
        if len != self.len {
            self.truncate(len)?;
        }
        Ok(())
    }

    pub(super) fn len(&self) -> usize {
        self.len
    }

    fn set_occupied(&mut self, index: usize, occupied: bool) -> Result<(), crate::TimingError> {
        let word_index = index / 64;
        if word_index >= self.occupancy.len() {
            self.occupancy
                .try_resize(word_index + 1)
                .map_err(instance_occupancy_capacity)?;
        }
        let mask = 1_u64 << (index % 64);
        let word = *self
            .occupancy
            .get(word_index)
            .expect("instance occupancy word was materialized");
        let word = if occupied { word | mask } else { word & !mask };
        self.occupancy
            .try_set(word_index, word)
            .map_err(instance_occupancy_capacity)?;
        Ok(())
    }

    fn mask_occupancy_tail(&mut self, len: usize) -> Result<(), crate::TimingError> {
        let remainder = len % 64;
        if remainder == 0 || len == 0 {
            return Ok(());
        }
        let word_index = len / 64;
        let Some(&word) = self.occupancy.get(word_index) else {
            return Ok(());
        };
        self.occupancy
            .try_set(word_index, word & ((1_u64 << remainder) - 1))
            .map_err(instance_occupancy_capacity)?;
        Ok(())
    }
}

/// Streaming stable-ID CSR builder; source rows are never retained beside the
/// final packed value arena.
pub(crate) struct InstanceNetArenaBuilder {
    rows: opto_core::RowArenaBuilder<crate::TimingNetId>,
    occupancy: opto_core::PagedCowVec<u64>,
    row_count: usize,
    next_row: usize,
}

impl InstanceNetArenaBuilder {
    fn new(row_count: usize) -> Result<Self, crate::TimingError> {
        let rows = opto_core::RowArenaBuilder::try_with_capacity(row_count)
            .map_err(instance_net_capacity)?;
        let occupancy_words = row_count.div_ceil(64);
        let mut occupancy = opto_core::PagedCowVec::new(0);
        occupancy
            .try_resize(occupancy_words)
            .map_err(instance_occupancy_capacity)?;
        Ok(Self {
            rows,
            occupancy,
            row_count,
            next_row: 0,
        })
    }

    pub(crate) fn push(
        &mut self,
        instance: TimingInstanceId,
        nets: impl ExactSizeIterator<Item = crate::TimingNetId>,
    ) -> Result<(), crate::TimingError> {
        let index = instance.raw() as usize;
        if index < self.next_row || index >= self.row_count {
            return Err(crate::TimingModelError::DuplicateInstanceId { id: instance.raw() }.into());
        }
        while self.next_row < index {
            self.rows
                .try_push_row(std::iter::empty())
                .map_err(instance_net_capacity)?;
            self.next_row += 1;
        }
        self.rows
            .try_push_row(nets)
            .map_err(instance_net_capacity)?;
        let word_index = index / 64;
        let word = self.occupancy.get(word_index).copied().unwrap_or(0) | (1_u64 << (index % 64));
        self.occupancy
            .try_set(word_index, word)
            .map_err(instance_occupancy_capacity)?;
        self.next_row += 1;
        Ok(())
    }

    pub(crate) fn finish(mut self) -> Result<InstanceNetArena, crate::TimingError> {
        while self.next_row < self.row_count {
            self.rows
                .try_push_row(std::iter::empty())
                .map_err(instance_net_capacity)?;
            self.next_row += 1;
        }
        Ok(InstanceNetArena {
            rows: self.rows.finish(),
            occupancy: self.occupancy,
            len: self.row_count,
        })
    }
}

fn instance_net_capacity(_: opto_core::PackedRowsError) -> crate::TimingError {
    crate::TimingModelError::Capacity {
        resource: "instance-net CSR",
    }
    .into()
}

impl Default for InstanceNetArena {
    fn default() -> Self {
        let rows = opto_core::RowArenaBuilder::try_with_capacity(0)
            .expect("empty packed timing instance rows fit")
            .finish();
        Self {
            rows,
            occupancy: opto_core::PagedCowVec::new(0),
            len: 0,
        }
    }
}

fn instance_occupancy_capacity(_: opto_core::CapacityError) -> crate::TimingError {
    crate::TimingModelError::Capacity {
        resource: "instance-net occupancy",
    }
    .into()
}
