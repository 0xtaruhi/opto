// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{DependencyActivation, DependencyEffects, DependencyPublicationPlan, Range};

/// One dependency publication run's sealed policy.
#[derive(Debug)]
pub struct DependencyRun<'a, R> {
    pub(super) plan: &'a DependencyPublicationPlan,
    pub(super) activation: DependencyActivation,
    pub(super) effects: Option<&'a mut DependencyEffects<R>>,
}

impl<'a, R> DependencyRun<'a, R> {
    #[must_use]
    /// Binds a publication plan and activation mask.
    pub const fn new(
        plan: &'a DependencyPublicationPlan,
        activation: DependencyActivation,
    ) -> Self {
        Self {
            plan,
            activation,
            effects: None,
        }
    }

    #[must_use]
    /// Records previous values for caller-controlled rollback.
    pub fn record_effects(mut self, effects: &'a mut DependencyEffects<R>) -> Self {
        self.effects = Some(effects);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// Stable logical order key for deterministic parallel task publication.
pub struct TaskKey {
    domain: u32,
    ordinal: u64,
}

impl TaskKey {
    #[must_use]
    /// Creates a key from a caller-defined domain and ordinal.
    pub const fn new(domain: u32, ordinal: u64) -> Self {
        Self { domain, ordinal }
    }
}

impl std::fmt::Display for TaskKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}:{}", self.domain, self.ordinal)
    }
}

#[derive(Debug)]
/// Ordered task input.
pub struct Task<T> {
    pub(crate) key: TaskKey,
    pub(crate) input: T,
    pub(crate) estimated_work: u64,
}

impl<T> Task<T> {
    #[must_use]
    /// Creates an ordered task.
    pub const fn new(key: TaskKey, input: T) -> Self {
        Self {
            key,
            input,
            estimated_work: 1,
        }
    }

    #[must_use]
    /// Attaches a relative work estimate for hierarchical scheduling.
    pub const fn with_estimated_work(mut self, estimated_work: u64) -> Self {
        self.estimated_work = estimated_work;
        self
    }
}

/// Splits an indexed input into deterministic, ordered tasks of at most
/// `grain_size` items. Keeping the task grain independent from the worker
/// count lets the work-stealing scheduler absorb uneven per-item costs.
#[must_use]
pub(crate) fn range_tasks(
    domain: u32,
    item_count: usize,
    grain_size: usize,
) -> Vec<Task<Range<usize>>> {
    assert!(grain_size > 0, "range task grain must be greater than zero");
    (0..item_count)
        .step_by(grain_size)
        .enumerate()
        .map(|(ordinal, start)| {
            Task::new(
                TaskKey::new(domain, ordinal as u64),
                start..(start + grain_size).min(item_count),
            )
        })
        .collect()
}

pub(super) const MIN_PARALLEL_INDEXED_ITEMS: usize = 64;
const MIN_INDEXED_ITEMS_PER_WORKER: usize = 32;

/// Number of worker-local states used by dense indexed execution.
#[must_use]
pub const fn indexed_worker_count(item_count: usize, maximum_threads: usize) -> usize {
    if item_count == 0 {
        0
    } else if maximum_threads > 1 && item_count >= MIN_PARALLEL_INDEXED_ITEMS {
        let useful_workers = item_count.div_ceil(MIN_INDEXED_ITEMS_PER_WORKER);
        if maximum_threads < useful_workers {
            maximum_threads
        } else {
            useful_workers
        }
    } else {
        1
    }
}

#[must_use]
pub(super) fn parallel_grain(item_count: usize, worker_count: usize) -> usize {
    item_count.div_ceil(worker_count.max(1)).max(1)
}
