// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::error::RuntimeError;
use std::collections::BTreeSet;
use std::sync::Arc;

/// A sealed exact-dependency topology over a dense item arena.
///
/// Unlike a levelized wave plan, this stores only exact edges. A worklist
/// releases an item as soon as every dependency that participates in that
/// worklist is complete, so unrelated deep cones never impose a global
/// barrier.
#[derive(Debug, Clone)]
pub struct DependencyPlan {
    predecessors: Arc<CsrEdges>,
    successors: Arc<CsrEdges>,
    positions: Arc<[u32]>,
}

#[derive(Debug)]
struct CsrEdges {
    rows: opto_core::PackedRows<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Traversal direction through a sealed dependency plan.
pub enum DependencyDirection {
    /// Dependencies complete before their consumers.
    Forward,
    /// Consumers complete before their dependencies.
    Reverse,
}

#[derive(Debug)]
/// Mutable execution state for one traversal of a [`DependencyPlan`].
pub struct DependencyWorklist<'plan> {
    plan: &'plan DependencyPlan,
    direction: DependencyDirection,
    disabled_dependencies: Box<[(usize, usize)]>,
    state: Box<[ItemState]>,
    unresolved: Box<[u32]>,
    ready: BTreeSet<(u32, usize)>,
    pending: usize,
    running: usize,
}

/// A sealed, exclusive mapping from dependency items to mutable output rows.
///
/// Publication plans are validated before execution: every row is in range
/// and has exactly one owner. This lets workers complete in any physical order
/// while the coordinator publishes only disjoint rows.
#[derive(Debug)]
pub struct DependencyPublicationPlan {
    item_count: usize,
    row_count: usize,
    rows: PublicationRows,
}

#[derive(Debug)]
enum PublicationRows {
    Identity,
    Sparse(CsrEdges),
}

/// Rows produced by one dependency item.
///
/// Row identifiers are checked against the item's sealed publication plan
/// before any row is modified.
#[derive(Debug)]
pub enum DependencyPublication<R> {
    /// Publishes no row.
    None,
    /// Publishes exactly one row.
    Row {
        /// Destination row in the sealed publication plan.
        row: usize,
        /// Value to publish into the destination row.
        value: R,
    },
    /// Publishes several rows in the plan's required row order.
    Rows(Box<[(usize, R)]>),
}

pub(crate) enum DependencyPublicationIter<R> {
    None,
    Row(Option<(usize, R)>),
    Rows(std::vec::IntoIter<(usize, R)>),
}

/// Monotonic mask controlling which scheduled dependency items execute.
#[derive(Debug)]
pub struct DependencyActivation {
    active: Option<Box<[bool]>>,
}

/// Stable rollback journal of values replaced during row publication.
#[derive(Debug, Default)]
pub struct DependencyEffects<R> {
    entries: Vec<DependencyEffect<R>>,
}

#[derive(Debug)]
struct DependencyEffect<R> {
    item: usize,
    row: usize,
    previous: R,
}

/// Deterministic summary of items and rows changed by one execution.
#[derive(Debug)]
pub struct DependencyExecution {
    published_items: Vec<usize>,
    changed_items: Vec<usize>,
    changed_rows: Vec<usize>,
}

/// Mutable dense row owner used by dependency publication.
///
/// Implementations may store rows column-wise or otherwise encode them
/// compactly; publication only requires exact owned-row materialization and a
/// transactional replacement operation.
pub trait DependencyRowStore<R: PartialEq> {
    /// Number of rows covered by this owner.
    fn len(&self) -> usize;

    /// Materializes one row for coordinator-side task preparation.
    fn get(&self, row: usize) -> Option<R>;

    /// Replaces one row and returns its previous value plus exact change flag.
    fn replace(&mut self, row: usize, value: R) -> Option<(R, bool)>;

    /// Restores a journaled row.
    fn rollback(&mut self, row: usize, previous: R) -> bool {
        self.replace(row, previous).is_some()
    }

    /// Whether this owner has no rows.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<R: Clone + PartialEq> DependencyRowStore<R> for [R] {
    fn len(&self) -> usize {
        <[R]>::len(self)
    }

    fn get(&self, row: usize) -> Option<R> {
        <[R]>::get(self, row).cloned()
    }

    fn replace(&mut self, row: usize, value: R) -> Option<(R, bool)> {
        let target = <[R]>::get_mut(self, row)?;
        let changed = *target != value;
        Some((std::mem::replace(target, value), changed))
    }
}

impl<R: Clone + PartialEq, const N: usize> DependencyRowStore<R> for [R; N] {
    fn len(&self) -> usize {
        N
    }

    fn get(&self, row: usize) -> Option<R> {
        self.as_slice().get(row).cloned()
    }

    fn replace(&mut self, row: usize, value: R) -> Option<(R, bool)> {
        <[R] as DependencyRowStore<R>>::replace(self.as_mut_slice(), row, value)
    }
}

impl<R: Clone + PartialEq> DependencyRowStore<R> for Vec<R> {
    fn len(&self) -> usize {
        self.as_slice().len()
    }

    fn get(&self, row: usize) -> Option<R> {
        self.as_slice().get(row).cloned()
    }

    fn replace(&mut self, row: usize, value: R) -> Option<(R, bool)> {
        self.as_mut_slice().replace(row, value)
    }
}

impl<R: Clone + PartialEq> DependencyRowStore<R> for Box<[R]> {
    fn len(&self) -> usize {
        self.as_ref().len()
    }

    fn get(&self, row: usize) -> Option<R> {
        self.as_ref().get(row).cloned()
    }

    fn replace(&mut self, row: usize, value: R) -> Option<(R, bool)> {
        self.as_mut().replace(row, value)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum ItemState {
    #[default]
    Absent,
    Pending,
    Running,
    Complete,
}

impl DependencyPublicationPlan {
    /// Creates the compact one-item-to-one-row publication plan.
    #[must_use]
    pub const fn identity(row_count: usize) -> Self {
        Self {
            item_count: row_count,
            row_count,
            rows: PublicationRows::Identity,
        }
    }

    /// Seals a sparse item-to-row mapping and rejects conflicting owners.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::InvalidDependencyPlan`] when an item or row lies
    /// outside its declared arena, an item is repeated, or two items own the
    /// same publication row.
    pub fn sparse(
        item_count: usize,
        row_count: usize,
        rows: impl IntoIterator<Item = (usize, usize)>,
    ) -> Result<Self, RuntimeError> {
        let mut rows = rows.into_iter().collect::<Vec<_>>();
        if rows
            .iter()
            .any(|&(item, row)| item >= item_count || row >= row_count)
        {
            return Err(invalid(
                "dependency publication row is outside its item or row arena",
            ));
        }
        rows.sort_unstable();
        if rows.windows(2).any(|rows| rows[0] == rows[1]) {
            return Err(invalid(
                "dependency publication item declares the same row more than once",
            ));
        }
        let mut owners = vec![usize::MAX; row_count];
        for &(item, row) in &rows {
            if std::mem::replace(&mut owners[row], item) != usize::MAX {
                return Err(invalid(
                    "dependency publication row has more than one owner",
                ));
            }
        }
        Ok(Self {
            item_count,
            row_count,
            rows: PublicationRows::Sparse(CsrEdges::seal(item_count, &rows)?),
        })
    }

    pub(crate) fn item_count(&self) -> usize {
        self.item_count
    }

    pub(crate) fn row_count(&self) -> usize {
        self.row_count
    }

    pub(crate) fn owned_row_count(&self, item: usize) -> usize {
        match &self.rows {
            PublicationRows::Identity => usize::from(item < self.item_count),
            PublicationRows::Sparse(rows) => rows.row(item).len(),
        }
    }

    pub(crate) fn owned_row(&self, item: usize, position: usize) -> Option<usize> {
        match &self.rows {
            PublicationRows::Identity => (item < self.item_count && position == 0).then_some(item),
            PublicationRows::Sparse(rows) => rows.row(item).get(position).copied(),
        }
    }

    /// Checks both the row set and its sealed per-item order.
    ///
    /// Publication order is part of the plan contract: accepting a permutation
    /// here would make column-wise stores observe callback-local ordering.
    pub(crate) fn owns_publication<R>(
        &self,
        item: usize,
        publication: &DependencyPublication<R>,
    ) -> bool {
        match publication {
            DependencyPublication::None => self.owned_row_count(item) == 0,
            DependencyPublication::Row { row, .. } => {
                self.owned_row_count(item) == 1 && self.owned_row(item, 0) == Some(*row)
            }
            DependencyPublication::Rows(rows) => {
                rows.len() == self.owned_row_count(item)
                    && rows
                        .iter()
                        .enumerate()
                        .all(|(position, (row, _))| self.owned_row(item, position) == Some(*row))
            }
        }
    }
}

impl<R> DependencyPublication<R> {
    #[must_use]
    /// Creates an empty publication.
    pub const fn none() -> Self {
        Self::None
    }

    #[must_use]
    /// Creates a single-row publication.
    pub const fn row(row: usize, value: R) -> Self {
        Self::Row { row, value }
    }

    #[must_use]
    /// Collects a multi-row publication.
    pub fn rows(rows: impl IntoIterator<Item = (usize, R)>) -> Self {
        Self::Rows(rows.into_iter().collect())
    }

    pub(crate) fn into_rows(self) -> DependencyPublicationIter<R> {
        match self {
            Self::None => DependencyPublicationIter::None,
            Self::Row { row, value } => DependencyPublicationIter::Row(Some((row, value))),
            Self::Rows(rows) => DependencyPublicationIter::Rows(Vec::from(rows).into_iter()),
        }
    }
}

impl<R> Iterator for DependencyPublicationIter<R> {
    type Item = (usize, R);

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::None => None,
            Self::Row(row) => row.take(),
            Self::Rows(rows) => rows.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.len();
        (len, Some(len))
    }
}

impl<R> ExactSizeIterator for DependencyPublicationIter<R> {
    fn len(&self) -> usize {
        match self {
            Self::None => 0,
            Self::Row(row) => usize::from(row.is_some()),
            Self::Rows(rows) => rows.len(),
        }
    }
}

impl DependencyActivation {
    /// Publishes every scheduled item.
    #[must_use]
    pub const fn all() -> Self {
        Self { active: None }
    }

    /// Publishes `seeds` initially and activates an exact dependent only after
    /// a changed predecessor row has been published.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::InvalidDependencyPlan`] if a seed is outside the
    /// item arena.
    pub fn on_change(
        item_count: usize,
        seeds: impl IntoIterator<Item = usize>,
    ) -> Result<Self, RuntimeError> {
        let mut active = vec![false; item_count].into_boxed_slice();
        for item in seeds {
            let Some(slot) = active.get_mut(item) else {
                return Err(invalid(
                    "dependency activation seed is outside the item arena",
                ));
            };
            *slot = true;
        }
        Ok(Self {
            active: Some(active),
        })
    }

    pub(crate) fn contains(&self, item: usize) -> bool {
        self.active.as_ref().is_none_or(|active| active[item])
    }

    fn activate(&mut self, item: usize) {
        if let Some(active) = &mut self.active {
            active[item] = true;
        }
    }

    pub(crate) fn item_count(&self) -> Option<usize> {
        self.active.as_ref().map(DependencyRowStore::len)
    }
}

impl<R> DependencyEffects<R> {
    #[must_use]
    /// Creates an empty rollback journal.
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Consumes effects in deterministic `(item, row)` order.
    pub fn into_entries(self) -> impl Iterator<Item = (usize, usize, R)> {
        self.entries
            .into_iter()
            .map(|entry| (entry.item, entry.row, entry.previous))
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    pub(crate) fn reserve_exact(&mut self, additional: usize) {
        self.entries.reserve_exact(additional);
    }

    pub(crate) fn push(&mut self, item: usize, row: usize, previous: R) {
        self.entries.push(DependencyEffect {
            item,
            row,
            previous,
        });
    }

    /// Orders the journal independently of worker completion order.
    pub(crate) fn stabilize(&mut self) {
        self.entries
            .sort_unstable_by_key(|entry| (entry.item, entry.row));
    }

    /// Restores all replaced rows and consumes the journal.
    ///
    /// Entries are replayed in reverse stabilized order so repeated writes, if
    /// introduced by a future publication mode, still recover the pre-run
    /// value rather than an intermediate value.
    pub(crate) fn rollback<S>(&mut self, rows: &mut S)
    where
        R: PartialEq,
        S: DependencyRowStore<R> + ?Sized,
    {
        for entry in self.entries.drain(..).rev() {
            let restored = rows.rollback(entry.row, entry.previous);
            debug_assert!(restored, "journaled dependency row remains in bounds");
        }
    }
}

impl DependencyExecution {
    #[must_use]
    /// Returns items whose closures produced a publication.
    pub fn published_items(&self) -> &[usize] {
        &self.published_items
    }

    #[must_use]
    /// Returns items that changed at least one row.
    pub fn changed_items(&self) -> &[usize] {
        &self.changed_items
    }

    #[must_use]
    /// Returns destination rows whose values changed.
    pub fn changed_rows(&self) -> &[usize] {
        &self.changed_rows
    }

    /// Seals execution summaries into deterministic item and row order.
    pub(crate) fn new(
        mut published_items: Vec<usize>,
        mut changed_items: Vec<usize>,
        mut changed_rows: Vec<usize>,
    ) -> Self {
        published_items.sort_unstable();
        changed_items.sort_unstable();
        changed_rows.sort_unstable();
        Self {
            published_items,
            changed_items,
            changed_rows,
        }
    }
}

impl CsrEdges {
    /// Builds one CSR edge table, removing duplicate edges.
    ///
    /// Rows are bucketed with a counting pass rather than a comparison sort over
    /// every edge. The packed-row builder already groups entries by row, so a
    /// global sort only ever bought duplicate removal, and duplicates can only
    /// occur inside one row: a multi-pin instance contributes the same source
    /// net once per arc. Sorting each row instead keeps the result identical and
    /// deterministic while turning `O(E log E)` into `O(E + R)` plus tiny
    /// per-row sorts. Plan construction runs on every incremental region edit,
    /// so this is a hot path in post-map optimization.
    fn seal(row_count: usize, edges: &[(usize, usize)]) -> Result<Self, RuntimeError> {
        if edges.len() > u32::MAX as usize {
            return Err(invalid("dependency edge count exceeds 32-bit capacity"));
        }
        let mut offsets = vec![0u32; row_count + 1];
        for &(row, _) in edges {
            let slot = offsets
                .get_mut(row + 1)
                .ok_or_else(|| invalid("dependency edge row is outside the item arena"))?;
            *slot = slot
                .checked_add(1)
                .ok_or_else(|| invalid("dependency edge count exceeds 32-bit capacity"))?;
        }
        for row in 1..offsets.len() {
            offsets[row] += offsets[row - 1];
        }
        let mut cursors = offsets[..row_count].to_vec();
        let mut values = vec![0usize; edges.len()];
        for &(row, value) in edges {
            let cursor = &mut cursors[row];
            values[*cursor as usize] = value;
            *cursor += 1;
        }
        let mut deduped = Vec::with_capacity(edges.len());
        for row in 0..row_count {
            let start = offsets[row] as usize;
            let end = offsets[row + 1] as usize;
            let slice = &mut values[start..end];
            slice.sort_unstable();
            let mut previous = None;
            for &value in slice.iter() {
                if previous.replace(value) != Some(value) {
                    deduped.push((row, value));
                }
            }
        }
        Ok(Self {
            rows: opto_core::PackedRows::try_from_entries(row_count, deduped)
                .map_err(|_| invalid("dependency edge row is outside the item arena"))?,
        })
    }

    fn row(&self, item: usize) -> &[usize] {
        self.rows.row(item)
    }
}

impl DependencyPlan {
    /// Stable identity of the shared immutable dependency topology.
    #[must_use]
    pub fn shared_identity(&self) -> usize {
        Arc::as_ptr(&self.positions).cast::<u32>() as usize
    }

    /// Allocation identities and bytes for exact cross-owner deduplication.
    #[must_use]
    pub fn shared_components(&self) -> [(usize, usize); 3] {
        [
            (
                Arc::as_ptr(&self.predecessors) as usize,
                self.predecessors.owned_memory_bytes(),
            ),
            (
                Arc::as_ptr(&self.successors) as usize,
                self.successors.owned_memory_bytes(),
            ),
            (
                Arc::as_ptr(&self.positions).cast::<u32>() as usize,
                opto_core::resident::slice_bytes::<u32>(self.positions.len()),
            ),
        ]
    }

    /// Deterministic resident bytes owned by the sealed CSR topology.
    #[must_use]
    pub fn owned_memory_bytes(&self) -> usize {
        self.predecessors
            .owned_memory_bytes()
            .saturating_add(self.successors.owned_memory_bytes())
            .saturating_add(opto_core::resident::slice_bytes::<u32>(
                self.positions.len(),
            ))
    }

    /// Seals exact predecessor edges from a caller-verified topological order.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] for incomplete, duplicate, out-of-range, or
    /// non-topological inputs, or compact adjacency capacity failure.
    pub fn from_topological_order<I, F>(
        item_count: usize,
        order: &[usize],
        dependencies: F,
    ) -> Result<Self, RuntimeError>
    where
        I: IntoIterator<Item = usize>,
        F: Fn(usize) -> I,
    {
        if order.len() != item_count {
            return Err(invalid("topological order does not cover the item arena"));
        }
        let mut positions = vec![u32::MAX; item_count];
        for (position, &item) in order.iter().enumerate() {
            let slot = positions
                .get_mut(item)
                .ok_or_else(|| invalid("topological order contains an invalid item"))?;
            if *slot != u32::MAX {
                return Err(invalid("topological order contains a duplicate item"));
            }
            *slot = u32::try_from(position)
                .map_err(|_| invalid("dependency item count exceeds 32-bit capacity"))?;
        }

        let mut predecessors = Vec::new();
        for &item in order {
            for dependency in dependencies(item) {
                if dependency >= item_count {
                    return Err(invalid("dependency is outside the item arena"));
                }
                if positions[dependency] >= positions[item] {
                    return Err(invalid("dependency is not topologically ordered"));
                }
                predecessors.push((item, dependency));
            }
        }
        let successors = predecessors
            .iter()
            .map(|&(item, dependency)| (dependency, item))
            .collect::<Vec<_>>();
        Ok(Self {
            predecessors: Arc::new(CsrEdges::seal(item_count, &predecessors)?),
            successors: Arc::new(CsrEdges::seal(item_count, &successors)?),
            positions: positions.into(),
        })
    }

    /// Creates a worklist for the dependency closure of `seeds`.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] when a seed is outside the item arena.
    pub fn worklist(
        &self,
        direction: DependencyDirection,
        seeds: impl IntoIterator<Item = usize>,
    ) -> Result<DependencyWorklist<'_>, RuntimeError> {
        self.worklist_masked(direction, seeds, std::iter::empty())
    }

    /// Creates a worklist after removing selected exact dependency edges.
    ///
    /// Disabled pairs use forward `(item, dependency)` orientation regardless
    /// of traversal direction. This admits runtime value boundaries, such as
    /// an explicitly annotated net, without rebuilding the sealed CSR plan.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::InvalidDependencyPlan`] if a seed or disabled
    /// edge endpoint lies outside the plan, a disabled edge is not present, or
    /// the selected closure cannot be represented within the compact counters.
    pub fn worklist_masked(
        &self,
        direction: DependencyDirection,
        seeds: impl IntoIterator<Item = usize>,
        disabled_dependencies: impl IntoIterator<Item = (usize, usize)>,
    ) -> Result<DependencyWorklist<'_>, RuntimeError> {
        let mut disabled_dependencies = disabled_dependencies.into_iter().collect::<Vec<_>>();
        disabled_dependencies.sort_unstable();
        disabled_dependencies.dedup();
        if disabled_dependencies.iter().any(|&(item, dependency)| {
            item >= self.positions.len()
                || dependency >= self.positions.len()
                || self
                    .predecessors
                    .row(item)
                    .binary_search(&dependency)
                    .is_err()
        }) {
            return Err(invalid(
                "disabled edge is not an exact dependency in the plan",
            ));
        }
        let mut worklist = DependencyWorklist {
            plan: self,
            direction,
            disabled_dependencies: disabled_dependencies.into_boxed_slice(),
            state: vec![ItemState::Absent; self.positions.len()].into_boxed_slice(),
            unresolved: vec![0; self.positions.len()].into_boxed_slice(),
            ready: BTreeSet::new(),
            pending: 0,
            running: 0,
        };
        for seed in seeds {
            worklist.schedule(seed)?;
        }
        Ok(worklist)
    }

    fn dependencies(&self, direction: DependencyDirection, item: usize) -> &[usize] {
        match direction {
            DependencyDirection::Forward => self.predecessors.row(item),
            DependencyDirection::Reverse => self.successors.row(item),
        }
    }
}

impl CsrEdges {
    fn owned_memory_bytes(&self) -> usize {
        self.rows.owned_memory_bytes()
    }
}

impl DependencyWorklist<'_> {
    pub(crate) fn item_count(&self) -> usize {
        self.state.len()
    }

    pub(crate) fn scheduled_count(&self) -> usize {
        self.pending
    }

    pub(crate) fn scheduled_owned_rows(&self, plan: &DependencyPublicationPlan) -> usize {
        self.state
            .iter()
            .enumerate()
            .filter(|(_, state)| **state != ItemState::Absent)
            .map(|(item, _)| plan.owned_row_count(item))
            .sum()
    }

    /// Activates the exact enabled dependents of a changed item.
    ///
    /// Disabled runtime boundaries must be respected here as well as during
    /// dependency counting; otherwise an incremental traversal can escape its
    /// caller-declared boundary.
    pub(crate) fn activate_dependents(&self, item: usize, activation: &mut DependencyActivation) {
        let dependents = match self.direction {
            DependencyDirection::Forward => self.plan.successors.row(item),
            DependencyDirection::Reverse => self.plan.predecessors.row(item),
        };
        for &dependent in dependents {
            if self.dependent_is_enabled(item, dependent) {
                activation.activate(dependent);
            }
        }
    }

    /// Adds an item and its unresolved closure without crossing completed work.
    ///
    /// Scheduling across the running/completed frontier would require
    /// reopening already published consumers, so it is rejected instead of
    /// producing a traversal whose result depends on insertion timing.
    pub(crate) fn schedule(&mut self, item: usize) -> Result<bool, RuntimeError> {
        if item >= self.state.len() {
            return Err(invalid("scheduled item is outside the dependency plan"));
        }
        let crosses_completed_frontier = match self.direction {
            DependencyDirection::Forward => self.plan.successors.row(item),
            DependencyDirection::Reverse => self.plan.predecessors.row(item),
        }
        .iter()
        .any(|&dependent| {
            self.dependent_is_enabled(item, dependent)
                && matches!(
                    self.state[dependent],
                    ItemState::Running | ItemState::Complete
                )
        });
        if crosses_completed_frontier {
            return Err(invalid(
                "scheduled dependency crosses the completed dependency frontier",
            ));
        }
        let state = self
            .state
            .get_mut(item)
            .expect("item range was validated above");
        if *state != ItemState::Absent {
            return Ok(false);
        }
        *state = ItemState::Pending;
        self.pending += 1;

        let dependencies = self.plan.dependencies(self.direction, item);
        self.unresolved[item] = u32::try_from(
            dependencies
                .iter()
                .filter(|&&dependency| {
                    self.dependency_is_enabled(item, dependency)
                        && matches!(
                            self.state[dependency],
                            ItemState::Pending | ItemState::Running
                        )
                })
                .count(),
        )
        .map_err(|_| invalid("dependency count exceeds 32-bit capacity"))?;

        let dependents = match self.direction {
            DependencyDirection::Forward => self.plan.successors.row(item),
            DependencyDirection::Reverse => self.plan.predecessors.row(item),
        };
        for &dependent in dependents {
            if !self.dependent_is_enabled(item, dependent)
                || self.state[dependent] != ItemState::Pending
            {
                continue;
            }
            if self.unresolved[dependent] == 0 {
                let key = self.ready_key(dependent);
                self.ready.remove(&key);
            }
            self.unresolved[dependent] = self.unresolved[dependent]
                .checked_add(1)
                .ok_or_else(|| invalid("dependency count exceeds 32-bit capacity"))?;
        }
        if self.unresolved[item] == 0 {
            self.ready.insert(self.ready_key(item));
        }
        Ok(true)
    }

    /// Claims every currently ready item in deterministic topological order.
    ///
    /// Claiming never completes an item or releases its successors. Call
    /// [`Self::finish`] for each physical completion; that exact completion,
    /// rather than a batch barrier, makes newly satisfied successors ready.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::InvalidDependencyPlan`] when the scheduled
    /// subgraph has pending items but neither a ready nor running item, which
    /// indicates a cycle or corrupted dependency state.
    pub fn claim_ready(&mut self) -> Result<Option<Vec<usize>>, RuntimeError> {
        self.claim_ready_bounded(usize::MAX)
    }

    /// Claims at most `limit` ready items in stable topological order.
    pub(crate) fn claim_ready_bounded(
        &mut self,
        limit: usize,
    ) -> Result<Option<Vec<usize>>, RuntimeError> {
        if limit == 0 {
            return Err(invalid("dependency ready claim limit must be nonzero"));
        }
        if self.pending == 0 {
            return Ok(None);
        }
        if self.ready.is_empty() {
            if self.running != 0 {
                return Ok(Some(Vec::new()));
            }
            return Err(invalid("scheduled dependency subgraph contains a cycle"));
        }
        let claimed = limit.min(self.ready.len());
        let mut ready = Vec::with_capacity(claimed);
        for _ in 0..claimed {
            let (_, item) = self
                .ready
                .pop_first()
                .expect("bounded ready count was measured above");
            ready.push(item);
        }
        for &item in &ready {
            debug_assert_eq!(self.state[item], ItemState::Pending);
            self.state[item] = ItemState::Running;
        }
        self.running += ready.len();
        Ok(Some(ready))
    }

    /// Commits one claimed item's completion and immediately releases exact
    /// successors whose last unresolved dependency is this item.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::InvalidDependencyPlan`] if `item` is outside the
    /// plan, was not claimed, or releasing its successors violates a compact
    /// dependency counter invariant.
    pub fn finish(&mut self, item: usize) -> Result<(), RuntimeError> {
        let state = self
            .state
            .get_mut(item)
            .ok_or_else(|| invalid("completed item is outside the dependency plan"))?;
        if *state != ItemState::Running {
            return Err(invalid("completed item was not claimed"));
        }
        *state = ItemState::Complete;
        self.running -= 1;
        self.pending -= 1;
        let dependents = match self.direction {
            DependencyDirection::Forward => self.plan.successors.row(item),
            DependencyDirection::Reverse => self.plan.predecessors.row(item),
        };
        for &dependent in dependents {
            if !self.dependent_is_enabled(item, dependent)
                || self.state[dependent] != ItemState::Pending
            {
                continue;
            }
            self.unresolved[dependent] = self.unresolved[dependent]
                .checked_sub(1)
                .ok_or_else(|| invalid("dependency completion underflow"))?;
            if self.unresolved[dependent] == 0 {
                self.ready.insert(self.ready_key(dependent));
            }
        }
        Ok(())
    }

    fn ready_key(&self, item: usize) -> (u32, usize) {
        let position = self.plan.positions[item];
        let order = match self.direction {
            DependencyDirection::Forward => position,
            DependencyDirection::Reverse => u32::MAX - position,
        };
        (order, item)
    }

    fn dependency_is_enabled(&self, item: usize, dependency: usize) -> bool {
        let edge = match self.direction {
            DependencyDirection::Forward => (item, dependency),
            DependencyDirection::Reverse => (dependency, item),
        };
        self.disabled_dependencies.binary_search(&edge).is_err()
    }

    fn dependent_is_enabled(&self, item: usize, dependent: usize) -> bool {
        self.dependency_is_enabled(dependent, item)
    }
}

fn invalid(detail: &'static str) -> RuntimeError {
    RuntimeError::InvalidDependencyPlan { detail }
}
