// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Columnar arrival and required-time state for dense timing nets.
//!
//! Each `(net, edge)` slot keeps its common first state in structure-of-arrays
//! columns and allocates ordered overflow only for additional tags. Logical row
//! materialization exists at worker, publication, and rollback boundaries; the
//! resident model never becomes an object-per-net container.

use super::*;

const EMPTY_STATE_ID: u32 = u32::MAX;
const EDGES_PER_NET: usize = 2;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::analysis) struct ArrivalEdge(SmallVec<[ArrivalState; 1]>);

impl ArrivalEdge {
    pub(in crate::analysis) fn len(&self) -> usize {
        self.0.len()
    }

    pub(in crate::analysis) fn iter(&self) -> std::slice::Iter<'_, ArrivalState> {
        self.0.iter()
    }

    pub(in crate::analysis) fn iter_mut(&mut self) -> std::slice::IterMut<'_, ArrivalState> {
        self.0.iter_mut()
    }

    pub(in crate::analysis) fn push(&mut self, state: ArrivalState) {
        self.0.push(state);
    }
}

impl<'a> IntoIterator for &'a ArrivalEdge {
    type Item = &'a ArrivalState;
    type IntoIter = std::slice::Iter<'a, ArrivalState>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::analysis) struct ArrivalRow([ArrivalEdge; EDGES_PER_NET]);

impl ArrivalRow {
    pub(in crate::analysis) fn new() -> Self {
        Self::default()
    }

    pub(in crate::analysis) fn iter(&self) -> std::slice::Iter<'_, ArrivalEdge> {
        self.0.iter()
    }
}

impl std::ops::Index<usize> for ArrivalRow {
    type Output = ArrivalEdge;

    fn index(&self, index: usize) -> &Self::Output {
        &self.0[index]
    }
}

impl std::ops::IndexMut<usize> for ArrivalRow {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.0[index]
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::analysis) struct RequiredEdge(SmallVec<[RequiredState; 1]>);

impl RequiredEdge {
    pub(in crate::analysis) fn len(&self) -> usize {
        self.0.len()
    }

    pub(in crate::analysis) fn iter(&self) -> std::slice::Iter<'_, RequiredState> {
        self.0.iter()
    }

    pub(in crate::analysis) fn iter_mut(&mut self) -> std::slice::IterMut<'_, RequiredState> {
        self.0.iter_mut()
    }

    pub(in crate::analysis) fn push(&mut self, state: RequiredState) {
        self.0.push(state);
    }
}

impl<'a> IntoIterator for &'a RequiredEdge {
    type Item = &'a RequiredState;
    type IntoIter = std::slice::Iter<'a, RequiredState>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::analysis) struct RequiredRow([RequiredEdge; EDGES_PER_NET]);

impl RequiredRow {
    pub(in crate::analysis) fn new() -> Self {
        Self::default()
    }

    pub(in crate::analysis) fn iter(&self) -> std::slice::Iter<'_, RequiredEdge> {
        self.0.iter()
    }
}

impl std::ops::Index<usize> for RequiredRow {
    type Output = RequiredEdge;

    fn index(&self, index: usize) -> &Self::Output {
        &self.0[index]
    }
}

impl std::ops::IndexMut<usize> for RequiredRow {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.0[index]
    }
}

#[derive(Debug, Clone, Copy)]
struct ArrivalOverflow {
    tag: u32,
    origin: u32,
    delay: f64,
    transition: f64,
    transition_valid: bool,
}

#[derive(Debug, Clone, Copy)]
struct RequiredOverflow {
    tag: u32,
    required: f64,
}

#[derive(Debug)]
struct ArrivalPathSlots {
    primary: Vec<u32>,
    overflow: BTreeMap<u32, Vec<u32>>,
}

#[derive(Debug)]
/// Dense arrival columns with optional path tracking and sparse multi-tag overflow.
pub(in crate::analysis) struct ArrivalSlotStore {
    net_count: usize,
    tags: Vec<u32>,
    origins: Vec<u32>,
    delays: Vec<f64>,
    transitions: Vec<f64>,
    transition_valid: Vec<u64>,
    overflow: BTreeMap<u32, Vec<ArrivalOverflow>>,
    paths: Option<ArrivalPathSlots>,
}

impl ArrivalSlotStore {
    pub(in crate::analysis) fn new(
        net_count: usize,
        track_paths: bool,
    ) -> Result<Self, crate::TimingError> {
        let slot_count = state_slot_count(net_count)?;
        Ok(Self {
            net_count,
            tags: filled_vec(slot_count, EMPTY_STATE_ID, "arrival tag columns")?,
            origins: filled_vec(slot_count, 0, "arrival origin columns")?,
            delays: filled_vec(slot_count, 0.0, "arrival delay columns")?,
            transitions: filled_vec(slot_count, 0.0, "arrival transition columns")?,
            transition_valid: filled_vec(
                slot_count.div_ceil(64),
                0,
                "arrival transition validity",
            )?,
            overflow: BTreeMap::new(),
            paths: if track_paths {
                Some(ArrivalPathSlots {
                    primary: filled_vec(slot_count, EMPTY_STATE_ID, "arrival path columns")?,
                    overflow: BTreeMap::new(),
                })
            } else {
                None
            },
        })
    }

    pub(in crate::analysis) fn len(&self) -> usize {
        self.net_count
    }

    pub(in crate::analysis) fn row(&self, net: usize) -> Option<ArrivalRow> {
        (net < self.net_count).then(|| {
            let mut row = ArrivalRow::new();
            for edge in 0..EDGES_PER_NET {
                row[edge].0.extend(self.states(net, edge));
            }
            row
        })
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "row replacement consumes the logical row to match DependencyRowStore semantics, \
                  then scatters it into compact columnar storage"
    )]
    /// Atomically scatters one logical row and returns its rollback value.
    pub(in crate::analysis) fn replace_row(
        &mut self,
        net: usize,
        row: ArrivalRow,
    ) -> Option<ArrivalRow> {
        let previous = self.row(net)?;
        self.write_row(net, &row);
        Some(previous)
    }

    fn write_row(&mut self, net: usize, row: &ArrivalRow) {
        for edge in 0..EDGES_PER_NET {
            self.write_edge(net, edge, &row[edge]);
        }
    }

    pub(in crate::analysis) fn states(&self, net: usize, edge: usize) -> ArrivalStateIter<'_> {
        let slot = self.slot(net, edge);
        let primary = (self.tags[slot] != EMPTY_STATE_ID).then(|| self.primary_state(slot));
        let overflow = self
            .overflow
            .get(&compact_slot(slot))
            .map(Vec::as_slice)
            .unwrap_or_default();
        let overflow_paths = self
            .paths
            .as_ref()
            .and_then(|paths| paths.overflow.get(&compact_slot(slot)))
            .map(Vec::as_slice);
        debug_assert!(
            overflow_paths.is_none_or(|paths| paths.len() == overflow.len()),
            "arrival overflow path columns remain row-aligned"
        );
        ArrivalStateIter {
            primary,
            overflow,
            overflow_paths,
            position: 0,
        }
    }

    pub(in crate::analysis) fn push_empty(&mut self) -> Result<(), crate::TimingError> {
        if self.net_count >= (u32::MAX as usize) / EDGES_PER_NET {
            return Err(state_capacity("arrival slot columns"));
        }
        try_reserve(&mut self.tags, EDGES_PER_NET, "arrival tag columns")?;
        try_reserve(&mut self.origins, EDGES_PER_NET, "arrival origin columns")?;
        try_reserve(&mut self.delays, EDGES_PER_NET, "arrival delay columns")?;
        try_reserve(
            &mut self.transitions,
            EDGES_PER_NET,
            "arrival transition columns",
        )?;
        let next_slots = self.tags.len() + EDGES_PER_NET;
        let validity_words = next_slots.div_ceil(64);
        if validity_words > self.transition_valid.len() {
            let additional = validity_words - self.transition_valid.len();
            try_reserve(
                &mut self.transition_valid,
                additional,
                "arrival transition validity",
            )?;
        }
        if let Some(paths) = &mut self.paths {
            try_reserve(&mut paths.primary, EDGES_PER_NET, "arrival path columns")?;
        }
        self.net_count += 1;
        for _ in 0..EDGES_PER_NET {
            self.tags.push(EMPTY_STATE_ID);
            self.origins.push(0);
            self.delays.push(0.0);
            self.transitions.push(0.0);
            if let Some(paths) = &mut self.paths {
                paths.primary.push(EMPTY_STATE_ID);
            }
        }
        self.transition_valid
            .resize(self.tags.len().div_ceil(64), 0);
        Ok(())
    }

    pub(in crate::analysis) fn pop(&mut self) {
        let Some(net) = self.net_count.checked_sub(1) else {
            return;
        };
        for edge in 0..EDGES_PER_NET {
            let slot = self.slot(net, edge);
            self.set_transition_valid(slot, false);
            self.overflow.remove(&compact_slot(slot));
            if let Some(paths) = &mut self.paths {
                paths.overflow.remove(&compact_slot(slot));
            }
        }
        self.net_count = net;
        let slot_count = net * EDGES_PER_NET;
        self.tags.truncate(slot_count);
        self.origins.truncate(slot_count);
        self.delays.truncate(slot_count);
        self.transitions.truncate(slot_count);
        self.transition_valid.truncate(slot_count.div_ceil(64));
        if let Some(paths) = &mut self.paths {
            paths.primary.truncate(slot_count);
        }
    }

    pub(in crate::analysis) fn path_ids_mut(&mut self) -> Option<impl Iterator<Item = &mut u32>> {
        self.paths.as_mut().map(|paths| {
            paths
                .primary
                .iter_mut()
                .chain(paths.overflow.values_mut().flat_map(|row| row.iter_mut()))
                .filter(|path| **path != EMPTY_STATE_ID)
        })
    }

    pub(in crate::analysis) fn owned_memory_bytes(&self) -> usize {
        let dense = opto_core::resident::slice_bytes::<u32>(self.tags.capacity())
            .saturating_add(opto_core::resident::slice_bytes::<u32>(
                self.origins.capacity(),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<f64>(
                self.delays.capacity(),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<f64>(
                self.transitions.capacity(),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<u64>(
                self.transition_valid.capacity(),
            ));
        let overflow = super::btree_memory_bytes::<u32, Vec<ArrivalOverflow>>(self.overflow.len())
            .saturating_add(
                self.overflow
                    .values()
                    .map(|row| opto_core::resident::slice_bytes::<ArrivalOverflow>(row.capacity()))
                    .sum(),
            );
        let paths = self.paths.as_ref().map_or(0, |paths| {
            opto_core::resident::slice_bytes::<u32>(paths.primary.capacity())
                .saturating_add(super::btree_memory_bytes::<u32, Vec<u32>>(
                    paths.overflow.len(),
                ))
                .saturating_add(
                    paths
                        .overflow
                        .values()
                        .map(|row| opto_core::resident::slice_bytes::<u32>(row.capacity()))
                        .sum(),
                )
        });
        dense.saturating_add(overflow).saturating_add(paths)
    }

    fn slot(&self, net: usize, edge: usize) -> usize {
        debug_assert!(net < self.net_count && edge < EDGES_PER_NET);
        net * EDGES_PER_NET + edge
    }

    fn primary_state(&self, slot: usize) -> ArrivalState {
        ArrivalState {
            tag: TagId(self.tags[slot]),
            origin: OriginId(self.origins[slot]),
            delay: self.delays[slot],
            transition: self
                .transition_is_valid(slot)
                .then_some(self.transitions[slot]),
            path: self.paths.as_ref().and_then(|paths| {
                (paths.primary[slot] != EMPTY_STATE_ID).then_some(PathId(paths.primary[slot]))
            }),
        }
    }

    fn write_edge(&mut self, net: usize, edge: usize, states: &ArrivalEdge) {
        let slot = self.slot(net, edge);
        self.tags[slot] = EMPTY_STATE_ID;
        self.set_transition_valid(slot, false);
        self.overflow.remove(&compact_slot(slot));
        if let Some(paths) = &mut self.paths {
            paths.primary[slot] = EMPTY_STATE_ID;
            paths.overflow.remove(&compact_slot(slot));
        }
        debug_assert!(
            self.paths.is_some() || states.iter().all(|state| state.path.is_none()),
            "scalar arrival stores cannot retain predecessor paths"
        );
        let mut states = states.iter();
        let Some(primary) = states.next() else {
            return;
        };
        self.tags[slot] = primary.tag.0;
        self.origins[slot] = primary.origin.0;
        self.delays[slot] = primary.delay;
        if let Some(transition) = primary.transition {
            self.transitions[slot] = transition;
            self.set_transition_valid(slot, true);
        }
        if let Some(paths) = &mut self.paths {
            paths.primary[slot] = primary.path.map_or(EMPTY_STATE_ID, |path| path.0);
        }
        let mut overflow = Vec::new();
        let mut overflow_paths = self.paths.as_ref().map(|_| Vec::new());
        for state in states {
            overflow.push(ArrivalOverflow {
                tag: state.tag.0,
                origin: state.origin.0,
                delay: state.delay,
                transition: state.transition.unwrap_or(0.0),
                transition_valid: state.transition.is_some(),
            });
            if let Some(paths) = &mut overflow_paths {
                paths.push(state.path.map_or(EMPTY_STATE_ID, |path| path.0));
            }
        }
        if overflow.is_empty() {
            return;
        }
        self.overflow.insert(compact_slot(slot), overflow);
        if let (Some(paths), Some(overflow_paths)) = (&mut self.paths, overflow_paths) {
            paths.overflow.insert(compact_slot(slot), overflow_paths);
        }
    }

    fn transition_is_valid(&self, slot: usize) -> bool {
        self.transition_valid[slot / 64] & (1_u64 << (slot % 64)) != 0
    }

    fn set_transition_valid(&mut self, slot: usize, valid: bool) {
        let mask = 1_u64 << (slot % 64);
        if valid {
            self.transition_valid[slot / 64] |= mask;
        } else {
            self.transition_valid[slot / 64] &= !mask;
        }
    }
}

pub(in crate::analysis) struct ArrivalStateIter<'a> {
    primary: Option<ArrivalState>,
    overflow: &'a [ArrivalOverflow],
    overflow_paths: Option<&'a [u32]>,
    position: usize,
}

impl Iterator for ArrivalStateIter<'_> {
    type Item = ArrivalState;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(primary) = self.primary.take() {
            return Some(primary);
        }
        let position = self.position;
        let state = *self.overflow.get(position)?;
        self.position += 1;
        let path = self
            .overflow_paths
            .and_then(|paths| paths.get(position))
            .filter(|&&path| path != EMPTY_STATE_ID)
            .map(|&path| PathId(path));
        Some(ArrivalState {
            tag: TagId(state.tag),
            origin: OriginId(state.origin),
            delay: state.delay,
            transition: state.transition_valid.then_some(state.transition),
            path,
        })
    }
}

#[derive(Debug)]
/// Dense required-time columns with sparse multi-tag overflow.
pub(in crate::analysis) struct RequiredSlotStore {
    net_count: usize,
    tags: Vec<u32>,
    requireds: Vec<f64>,
    overflow: BTreeMap<u32, Vec<RequiredOverflow>>,
}

impl RequiredSlotStore {
    pub(in crate::analysis) fn new(net_count: usize) -> Result<Self, crate::TimingError> {
        let slot_count = state_slot_count(net_count)?;
        Ok(Self {
            net_count,
            tags: filled_vec(slot_count, EMPTY_STATE_ID, "required tag columns")?,
            requireds: filled_vec(slot_count, 0.0, "required value columns")?,
            overflow: BTreeMap::new(),
        })
    }

    pub(in crate::analysis) fn len(&self) -> usize {
        self.net_count
    }

    pub(in crate::analysis) fn row(&self, net: usize) -> Option<RequiredRow> {
        (net < self.net_count).then(|| {
            let mut row = RequiredRow::new();
            for edge in 0..EDGES_PER_NET {
                row[edge].0.extend(self.states(net, edge));
            }
            row
        })
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "row replacement consumes the logical row to match DependencyRowStore semantics, \
                  then scatters it into compact columnar storage"
    )]
    /// Atomically scatters one logical row and returns its rollback value.
    pub(in crate::analysis) fn replace_row(
        &mut self,
        net: usize,
        row: RequiredRow,
    ) -> Option<RequiredRow> {
        let previous = self.row(net)?;
        self.write_row(net, &row);
        Some(previous)
    }

    fn write_row(&mut self, net: usize, row: &RequiredRow) {
        for edge in 0..EDGES_PER_NET {
            let slot = self.slot(net, edge);
            self.tags[slot] = EMPTY_STATE_ID;
            self.overflow.remove(&compact_slot(slot));
            let mut states = row[edge].iter();
            let Some(primary) = states.next() else {
                continue;
            };
            self.tags[slot] = primary.tag.0;
            self.requireds[slot] = primary.required;
            let overflow = states
                .map(|state| RequiredOverflow {
                    tag: state.tag.0,
                    required: state.required,
                })
                .collect::<Vec<_>>();
            if !overflow.is_empty() {
                self.overflow.insert(compact_slot(slot), overflow);
            }
        }
    }

    pub(in crate::analysis) fn states(&self, net: usize, edge: usize) -> RequiredStateIter<'_> {
        let slot = self.slot(net, edge);
        RequiredStateIter {
            primary: (self.tags[slot] != EMPTY_STATE_ID).then_some(RequiredState {
                tag: TagId(self.tags[slot]),
                required: self.requireds[slot],
            }),
            overflow: self
                .overflow
                .get(&compact_slot(slot))
                .map(Vec::as_slice)
                .unwrap_or_default(),
            position: 0,
        }
    }

    pub(in crate::analysis) fn push_empty(&mut self) -> Result<(), crate::TimingError> {
        if self.net_count >= (u32::MAX as usize) / EDGES_PER_NET {
            return Err(state_capacity("required slot columns"));
        }
        try_reserve(&mut self.tags, EDGES_PER_NET, "required tag columns")?;
        try_reserve(&mut self.requireds, EDGES_PER_NET, "required value columns")?;
        self.net_count += 1;
        for _ in 0..EDGES_PER_NET {
            self.tags.push(EMPTY_STATE_ID);
            self.requireds.push(0.0);
        }
        Ok(())
    }

    pub(in crate::analysis) fn pop(&mut self) {
        let Some(net) = self.net_count.checked_sub(1) else {
            return;
        };
        for edge in 0..EDGES_PER_NET {
            self.overflow.remove(&compact_slot(self.slot(net, edge)));
        }
        self.net_count = net;
        self.tags.truncate(net * EDGES_PER_NET);
        self.requireds.truncate(net * EDGES_PER_NET);
    }

    pub(in crate::analysis) fn owned_memory_bytes(&self) -> usize {
        opto_core::resident::slice_bytes::<u32>(self.tags.capacity())
            .saturating_add(opto_core::resident::slice_bytes::<f64>(
                self.requireds.capacity(),
            ))
            .saturating_add(super::btree_memory_bytes::<u32, Vec<RequiredOverflow>>(
                self.overflow.len(),
            ))
            .saturating_add(
                self.overflow
                    .values()
                    .map(|row| opto_core::resident::slice_bytes::<RequiredOverflow>(row.capacity()))
                    .sum(),
            )
    }

    fn slot(&self, net: usize, edge: usize) -> usize {
        debug_assert!(net < self.net_count && edge < EDGES_PER_NET);
        net * EDGES_PER_NET + edge
    }
}

pub(in crate::analysis) struct RequiredStateIter<'a> {
    primary: Option<RequiredState>,
    overflow: &'a [RequiredOverflow],
    position: usize,
}

impl Iterator for RequiredStateIter<'_> {
    type Item = RequiredState;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(primary) = self.primary.take() {
            return Some(primary);
        }
        let state = *self.overflow.get(self.position)?;
        self.position += 1;
        Some(RequiredState {
            tag: TagId(state.tag),
            required: state.required,
        })
    }
}

impl opto_runtime::DependencyRowStore<ArrivalRow> for ArrivalSlotStore {
    fn len(&self) -> usize {
        self.len()
    }

    fn get(&self, row: usize) -> Option<ArrivalRow> {
        self.row(row)
    }

    fn replace(&mut self, row: usize, value: ArrivalRow) -> Option<(ArrivalRow, bool)> {
        let previous = self.row(row)?;
        let changed = previous != value;
        self.write_row(row, &value);
        Some((previous, changed))
    }
}

impl opto_runtime::DependencyRowStore<RequiredRow> for RequiredSlotStore {
    fn len(&self) -> usize {
        self.len()
    }

    fn get(&self, row: usize) -> Option<RequiredRow> {
        self.row(row)
    }

    fn replace(&mut self, row: usize, value: RequiredRow) -> Option<(RequiredRow, bool)> {
        let previous = self.row(row)?;
        let changed = previous != value;
        self.write_row(row, &value);
        Some((previous, changed))
    }
}

fn compact_slot(slot: usize) -> u32 {
    u32::try_from(slot).expect("slot-store capacity checks keep every slot representable as u32")
}

fn state_slot_count(net_count: usize) -> Result<usize, crate::TimingError> {
    net_count
        .checked_mul(EDGES_PER_NET)
        .filter(|&count| u32::try_from(count).is_ok())
        .ok_or_else(|| state_capacity("timing propagation slot columns"))
}

fn filled_vec<T: Clone>(
    len: usize,
    value: T,
    resource: &'static str,
) -> Result<Vec<T>, crate::TimingError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(len)
        .map_err(|_| state_capacity(resource))?;
    values.resize(len, value);
    Ok(values)
}

fn try_reserve<T>(
    values: &mut Vec<T>,
    additional: usize,
    resource: &'static str,
) -> Result<(), crate::TimingError> {
    values
        .try_reserve_exact(additional)
        .map_err(|_| state_capacity(resource))
}

fn state_capacity(resource: &'static str) -> crate::TimingError {
    crate::TimingAnalysisError::Capacity { resource }.into()
}
