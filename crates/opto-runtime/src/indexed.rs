// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::context::ExecutionContext;
use crate::dependency::{
    DependencyActivation, DependencyEffects, DependencyExecution, DependencyPublication,
    DependencyPublicationPlan, DependencyRowStore, DependencyWorklist,
};
use crate::error::RuntimeError;
use rayon::prelude::*;
use std::any::Any;
use std::cmp::Ordering as CmpOrdering;
use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::ops::Range;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::atomic::Ordering;
use std::sync::mpsc;

mod model;

pub(crate) use model::range_tasks;
pub use model::{DependencyRun, Task, TaskKey, indexed_worker_count};
use model::{MIN_PARALLEL_INDEXED_ITEMS, parallel_grain};

fn validate_publication_execution(
    worklist: &DependencyWorklist<'_>,
    plan: &DependencyPublicationPlan,
    row_count: usize,
    activation: &DependencyActivation,
) -> Result<(), RuntimeError> {
    if plan.item_count() != worklist.item_count() {
        return Err(RuntimeError::InvalidDependencyPlan {
            detail: "dependency publication and worklist item arenas differ",
        });
    }
    if plan.row_count() != row_count {
        return Err(RuntimeError::InvalidDependencyPlan {
            detail: "dependency publication plan does not cover the mutable row arena",
        });
    }
    if activation
        .item_count()
        .is_some_and(|count| count != worklist.item_count())
    {
        return Err(RuntimeError::InvalidDependencyPlan {
            detail: "dependency activation and worklist item arenas differ",
        });
    }
    Ok(())
}

struct PublicationCommit<'a, 'plan, R: PartialEq, S: DependencyRowStore<R> + ?Sized> {
    worklist: &'a mut DependencyWorklist<'plan>,
    plan: &'a DependencyPublicationPlan,
    rows: &'a mut S,
    activation: &'a mut DependencyActivation,
    effects: &'a mut DependencyEffects<R>,
    published_items: &'a mut Vec<usize>,
    changed_items: &'a mut Vec<usize>,
    changed_rows: &'a mut Vec<usize>,
}

impl<R: PartialEq, S: DependencyRowStore<R> + ?Sized> PublicationCommit<'_, '_, R, S> {
    fn publish(
        &mut self,
        item: usize,
        publication: DependencyPublication<R>,
    ) -> Result<(), RuntimeError> {
        if !self.plan.owns_publication(item, &publication) {
            return Err(RuntimeError::InvalidDependencyPlan {
                detail: "worker publication does not match its sealed row ownership",
            });
        }
        let mut item_changed = false;
        for (row, value) in publication.into_rows() {
            let (previous, changed) =
                self.rows
                    .replace(row, value)
                    .ok_or(RuntimeError::InvalidDependencyPlan {
                        detail: "worker publication row is outside its mutable owner",
                    })?;
            self.effects.push(item, row, previous);
            if changed {
                item_changed = true;
                self.changed_rows.push(row);
            }
        }
        self.published_items.push(item);
        if item_changed {
            self.changed_items.push(item);
            self.worklist.activate_dependents(item, self.activation);
        }
        self.worklist.finish(item)
    }
}

struct PublicationCoordinator<'rows, 'plan, R, S: ?Sized> {
    worklist: DependencyWorklist<'plan>,
    plan: &'plan DependencyPublicationPlan,
    rows: &'rows mut S,
    activation: DependencyActivation,
    journal: DependencyEffects<R>,
    published_items: Vec<usize>,
    changed_items: Vec<usize>,
    changed_rows: Vec<usize>,
    ran: bool,
}

impl<R, S> PublicationCoordinator<'_, '_, R, S>
where
    R: PartialEq,
    S: DependencyRowStore<R> + ?Sized,
{
    fn publish<E>(
        &mut self,
        failure: &mut Option<(usize, DependencyFailure<E>)>,
        item: usize,
        output: DependencyPublication<R>,
    ) where
        E: From<RuntimeError>,
    {
        let result = (PublicationCommit {
            worklist: &mut self.worklist,
            plan: self.plan,
            rows: self.rows,
            activation: &mut self.activation,
            effects: &mut self.journal,
            published_items: &mut self.published_items,
            changed_items: &mut self.changed_items,
            changed_rows: &mut self.changed_rows,
        })
        .publish(item, output);
        if let Err(error) = result {
            retain_stable_failure(failure, item, DependencyFailure::Error(E::from(error)));
        }
    }

    fn finish(self) -> (DependencyExecution, DependencyEffects<R>, bool) {
        (
            DependencyExecution::new(self.published_items, self.changed_items, self.changed_rows),
            self.journal,
            self.ran,
        )
    }
}

enum DependencyFailure<E> {
    Error(E),
    Panic(Box<dyn Any + Send>),
}

fn retain_stable_failure<E>(
    failure: &mut Option<(usize, DependencyFailure<E>)>,
    item: usize,
    error: DependencyFailure<E>,
) {
    if failure
        .as_ref()
        .is_none_or(|(failed_item, _)| item < *failed_item)
    {
        *failure = Some((item, error));
    }
}

fn resolve_dependency_failure<E>(failure: Option<(usize, DependencyFailure<E>)>) -> Result<(), E> {
    match failure {
        None => Ok(()),
        Some((_, DependencyFailure::Error(error))) => Err(error),
        Some((_, DependencyFailure::Panic(payload))) => resume_unwind(payload),
    }
}

impl ExecutionContext {
    /// Analyzes dense item IDs concurrently and commits each result in
    /// ascending ID order. At most one item per worker is in flight, so this
    /// is the streaming counterpart to [`Self::analyze_indexed`].
    ///
    /// # Errors
    ///
    /// Returns the selected analysis or commit callback error. Cancellation is
    /// converted through `E: From<RuntimeError>`; already-analyzed values after
    /// the first stable failure are discarded rather than committed.
    pub fn commit_indexed<O, E, F, C>(
        &self,
        item_count: usize,
        analyze: F,
        mut commit: C,
    ) -> Result<(), E>
    where
        O: Send,
        E: From<RuntimeError> + Send,
        F: Fn(usize) -> Result<O, E> + Send + Sync,
        C: FnMut(usize, O) -> Result<(), E>,
    {
        if self.is_cancelled() {
            return Err(RuntimeError::Cancelled.into());
        }
        let worker_count = indexed_worker_count(item_count, self.parallelism());
        if worker_count <= 1 {
            for index in 0..item_count {
                if self.is_cancelled() {
                    return Err(RuntimeError::Cancelled.into());
                }
                commit(index, analyze(index)?)?;
            }
        } else {
            self.inner.pool.in_place_scope(|scope| -> Result<(), E> {
                let (sender, receiver) = mpsc::sync_channel(worker_count);
                for index in 0..worker_count {
                    let sender = sender.clone();
                    let analyze = &analyze;
                    scope.spawn(move |_| {
                        let result = catch_unwind(AssertUnwindSafe(|| analyze(index)));
                        let _ = sender.send((index, result));
                    });
                }
                let mut next_launch = worker_count;
                let mut next_commit = 0usize;
                let mut pending = BTreeMap::new();
                while next_commit < item_count {
                    let (index, result) =
                        receiver
                            .recv()
                            .map_err(|_| RuntimeError::InvalidDependencyPlan {
                                detail: "indexed worker stopped without publishing its result",
                            })?;
                    pending.insert(index, result);
                    while let Some(result) = pending.remove(&next_commit) {
                        let output = match result {
                            Ok(result) => result?,
                            Err(payload) => resume_unwind(payload),
                        };
                        commit(next_commit, output)?;
                        next_commit += 1;
                        if next_launch < item_count {
                            let index = next_launch;
                            let sender = sender.clone();
                            let analyze = &analyze;
                            scope.spawn(move |_| {
                                let result = catch_unwind(AssertUnwindSafe(|| analyze(index)));
                                let _ = sender.send((index, result));
                            });
                            next_launch += 1;
                        }
                    }
                }
                Ok(())
            })?;
        }
        if item_count != 0 {
            self.inner.completed_batches.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }

    fn publish_dependency_rows_serial<R, S, I, E, P, F>(
        &self,
        coordinator: &mut PublicationCoordinator<'_, '_, R, S>,
        prepare: &P,
        analyze: &F,
    ) -> Result<(), E>
    where
        R: PartialEq,
        S: DependencyRowStore<R> + ?Sized,
        E: From<RuntimeError>,
        P: Fn(&S, usize) -> Result<I, E>,
        F: Fn(I) -> Result<DependencyPublication<R>, E>,
    {
        let mut failure = None;
        while let Some(items) = coordinator.worklist.claim_ready_bounded(1)? {
            if items.is_empty() {
                break;
            }
            for item in items {
                if !coordinator.activation.contains(item) {
                    coordinator.worklist.finish(item)?;
                    continue;
                }
                coordinator.ran = true;
                if self.is_cancelled() {
                    return Err(RuntimeError::Cancelled.into());
                }
                let output = catch_unwind(AssertUnwindSafe(|| {
                    prepare(coordinator.rows, item).and_then(analyze)
                }));
                match output {
                    Err(payload) => {
                        retain_stable_failure(
                            &mut failure,
                            item,
                            DependencyFailure::Panic(payload),
                        );
                    }
                    Ok(Err(error)) => {
                        retain_stable_failure(&mut failure, item, DependencyFailure::Error(error));
                    }
                    Ok(Ok(output)) => coordinator.publish(&mut failure, item, output),
                }
            }
        }
        resolve_dependency_failure(failure)
    }

    fn publish_dependency_rows_parallel<R, S, I, E, P, F>(
        &self,
        coordinator: &mut PublicationCoordinator<'_, '_, R, S>,
        worker_count: usize,
        prepare: &P,
        analyze: &F,
    ) -> Result<(), E>
    where
        R: PartialEq + Send,
        S: DependencyRowStore<R> + ?Sized,
        I: Send,
        E: From<RuntimeError> + Send,
        P: Fn(&S, usize) -> Result<I, E> + Send,
        F: Fn(I) -> Result<DependencyPublication<R>, E> + Send + Sync,
    {
        self.inner.pool.in_place_scope(|scope| -> Result<(), E> {
            let (sender, receiver) = mpsc::sync_channel(worker_count);
            let mut running = 0usize;
            let mut failure = None;
            let mut cancelled = false;
            loop {
                cancelled |= self.is_cancelled();
                let available = worker_count.saturating_sub(running);
                let items = if cancelled || available == 0 {
                    Vec::new()
                } else {
                    coordinator
                        .worklist
                        .claim_ready_bounded(available)?
                        .unwrap_or_default()
                };
                let claimed_any = !items.is_empty();
                for item in items {
                    if !coordinator.activation.contains(item) {
                        coordinator.worklist.finish(item)?;
                        continue;
                    }
                    coordinator.ran = true;
                    match catch_unwind(AssertUnwindSafe(|| prepare(coordinator.rows, item))) {
                        Ok(Ok(input)) => {
                            let sender = sender.clone();
                            scope.spawn(move |_| {
                                let result = catch_unwind(AssertUnwindSafe(|| analyze(input)));
                                let _ = sender.send((item, result));
                            });
                            running += 1;
                        }
                        Ok(Err(error)) => retain_stable_failure(
                            &mut failure,
                            item,
                            DependencyFailure::Error(error),
                        ),
                        Err(payload) => retain_stable_failure(
                            &mut failure,
                            item,
                            DependencyFailure::Panic(payload),
                        ),
                    }
                }
                if running == 0 {
                    if claimed_any {
                        continue;
                    }
                    break;
                }
                let (item, result) = receiver
                    .recv()
                    .expect("dependency worker senders live until scoped tasks finish");
                running -= 1;
                cancelled |= self.is_cancelled();
                match result {
                    Err(payload) => {
                        retain_stable_failure(
                            &mut failure,
                            item,
                            DependencyFailure::Panic(payload),
                        );
                    }
                    Ok(Err(error)) => {
                        retain_stable_failure(&mut failure, item, DependencyFailure::Error(error));
                    }
                    Ok(Ok(output)) if !cancelled => {
                        coordinator.publish(&mut failure, item, output);
                    }
                    Ok(Ok(_)) => {}
                }
            }
            resolve_dependency_failure(failure)?;
            if cancelled {
                Err(RuntimeError::Cancelled.into())
            } else {
                Ok(())
            }
        })
    }

    /// Publishes exclusively owned dependency rows without wave barriers.
    ///
    /// `prepare` snapshots committed rows on the coordinator. Workers return
    /// owned publications, whose row identifiers must exactly match `plan`.
    /// Changed rows activate exact dependents before completion releases them.
    /// Optional rollback effects are stabilized by `(item, row)` even when
    /// execution fails.
    ///
    /// # Errors
    ///
    /// Returns a converted [`RuntimeError`] when the worklist, publication plan,
    /// activation mask, or returned row ownership is inconsistent, and returns
    /// callback errors from `prepare` or `analyze`. Any committed rows are
    /// rolled back before an error is returned.
    ///
    /// # Panics
    ///
    /// Resumes a panic from `prepare`, `analyze`, or row storage only after
    /// restoring every row recorded by the publication journal.
    pub fn publish_dependency_rows<R, S, I, E, P, F>(
        &self,
        worklist: DependencyWorklist<'_>,
        rows: &mut S,
        run: DependencyRun<'_, R>,
        prepare: P,
        analyze: F,
    ) -> Result<DependencyExecution, E>
    where
        R: PartialEq + Send,
        S: DependencyRowStore<R> + ?Sized,
        I: Send,
        E: From<RuntimeError> + Send,
        P: Fn(&S, usize) -> Result<I, E> + Send,
        F: Fn(I) -> Result<DependencyPublication<R>, E> + Send + Sync,
    {
        let DependencyRun {
            plan,
            activation,
            mut effects,
        } = run;
        validate_publication_execution(&worklist, plan, rows.len(), &activation)
            .map_err(E::from)?;
        let execution_plan = self.dependency_execution_plan(&worklist, plan)?;
        if let Some(effects) = effects.as_deref_mut() {
            effects.clear();
        }
        if self.is_cancelled() {
            return Err(RuntimeError::Cancelled.into());
        }
        let mut journal = DependencyEffects::new();
        journal.reserve_exact(execution_plan.scheduled_rows);
        let mut coordinator = PublicationCoordinator {
            worklist,
            plan,
            rows,
            activation,
            journal,
            published_items: Vec::with_capacity(execution_plan.scheduled_items),
            changed_items: Vec::with_capacity(execution_plan.scheduled_items),
            changed_rows: Vec::with_capacity(execution_plan.scheduled_rows),
            ran: false,
        };
        let result = catch_unwind(AssertUnwindSafe(|| -> Result<(), E> {
            if execution_plan.worker_count <= 1 {
                self.publish_dependency_rows_serial(&mut coordinator, &prepare, &analyze)?;
            } else {
                self.publish_dependency_rows_parallel(
                    &mut coordinator,
                    execution_plan.worker_count,
                    &prepare,
                    &analyze,
                )?;
            }
            Ok(())
        }));
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                coordinator.journal.rollback(coordinator.rows);
                return Err(error);
            }
            Err(payload) => {
                coordinator.journal.rollback(coordinator.rows);
                resume_unwind(payload);
            }
        }
        coordinator.journal.stabilize();
        let (execution, journal, ran) = coordinator.finish();
        if let Some(effects) = effects {
            *effects = journal;
        }
        if ran {
            self.inner.completed_batches.fetch_add(1, Ordering::Relaxed);
        }
        Ok(execution)
    }

    /// Analyzes dense item IDs and returns results in ascending ID order.
    ///
    /// # Errors
    ///
    /// Returns a caller error or [`RuntimeError`] for cancellation.
    pub fn analyze_indexed<O, E, F>(&self, item_count: usize, analyze: F) -> Result<Vec<O>, E>
    where
        O: Send,
        E: From<RuntimeError> + Send,
        F: Fn(usize) -> Result<O, E> + Send + Sync,
    {
        let worker_count = indexed_worker_count(item_count, self.parallelism());
        self.analyze_indexed_grained(
            item_count,
            worker_count > 1,
            parallel_grain(item_count, worker_count),
            analyze,
        )
    }

    /// Analyzes dense item IDs with an explicit minimum Rayon shard grain.
    ///
    /// This variant is intended for callers whose individual items are known
    /// to be substantial or uneven. A grain of one keeps every item available
    /// to work stealing; larger grains amortize scheduler overhead. Results
    /// remain in ascending item order regardless of execution order.
    ///
    /// # Errors
    ///
    /// Returns a caller error or [`RuntimeError`] for cancellation.
    pub fn analyze_indexed_with_grain<O, E, F>(
        &self,
        item_count: usize,
        minimum_grain: NonZeroUsize,
        analyze: F,
    ) -> Result<Vec<O>, E>
    where
        O: Send,
        E: From<RuntimeError> + Send,
        F: Fn(usize) -> Result<O, E> + Send + Sync,
    {
        self.analyze_indexed_grained(
            item_count,
            self.parallelism() > 1 && item_count > minimum_grain.get(),
            minimum_grain.get(),
            analyze,
        )
    }

    fn analyze_indexed_grained<O, E, F>(
        &self,
        item_count: usize,
        parallel: bool,
        minimum_grain: usize,
        analyze: F,
    ) -> Result<Vec<O>, E>
    where
        O: Send,
        E: From<RuntimeError> + Send,
        F: Fn(usize) -> Result<O, E> + Send + Sync,
    {
        if self.is_cancelled() {
            return Err(RuntimeError::Cancelled.into());
        }
        let values = if parallel {
            self.inner.pool.install(|| {
                (0..item_count)
                    .into_par_iter()
                    .with_min_len(minimum_grain)
                    .map(|index| {
                        if self.is_cancelled() {
                            Err(RuntimeError::Cancelled.into())
                        } else {
                            analyze(index)
                        }
                    })
                    .collect::<Result<Vec<_>, E>>()
            })?
        } else {
            (0..item_count)
                .map(|index| {
                    if self.is_cancelled() {
                        Err(RuntimeError::Cancelled.into())
                    } else {
                        analyze(index)
                    }
                })
                .collect::<Result<Vec<_>, E>>()?
        };
        if item_count != 0 {
            self.inner.completed_batches.fetch_add(1, Ordering::Relaxed);
        }
        Ok(values)
    }

    /// Variant of [`Self::analyze_indexed`] with shard-local scratch state.
    /// The runtime creates the state once per internal work shard, allowing
    /// passes to reuse buffers or analyzers without knowing shard boundaries.
    ///
    /// # Errors
    ///
    /// Returns the analysis callback error, or cancellation converted through
    /// `E: From<RuntimeError>`.
    pub fn analyze_indexed_with<S, O, E, I, F>(
        &self,
        item_count: usize,
        initialize: I,
        analyze: F,
    ) -> Result<Vec<O>, E>
    where
        S: Send,
        O: Send,
        E: From<RuntimeError> + Send,
        I: Fn() -> S + Send + Sync,
        F: Fn(&mut S, usize) -> Result<O, E> + Send + Sync,
    {
        if self.is_cancelled() {
            return Err(RuntimeError::Cancelled.into());
        }
        let worker_count = indexed_worker_count(item_count, self.parallelism());
        let values = if worker_count > 1 {
            let grain = parallel_grain(item_count, worker_count);
            self.inner.pool.install(|| {
                (0..item_count)
                    .into_par_iter()
                    .with_min_len(grain)
                    .map_init(&initialize, |state, index| {
                        if self.is_cancelled() {
                            Err(RuntimeError::Cancelled.into())
                        } else {
                            analyze(state, index)
                        }
                    })
                    .collect::<Result<Vec<_>, E>>()
            })?
        } else {
            let mut state = initialize();
            (0..item_count)
                .map(|index| {
                    if self.is_cancelled() {
                        Err(RuntimeError::Cancelled.into())
                    } else {
                        analyze(&mut state, index)
                    }
                })
                .collect::<Result<Vec<_>, E>>()?
        };
        if item_count != 0 {
            self.inner.completed_batches.fetch_add(1, Ordering::Relaxed);
        }
        Ok(values)
    }

    /// Analyzes deterministic chunks and commits each chunk before the next
    /// one is materialized.
    ///
    /// # Errors
    ///
    /// Returns a converted [`RuntimeError`] for a zero chunk size or
    /// cancellation, and propagates errors from either callback. Chunks committed
    /// before the failing chunk remain committed.
    pub fn analyze_indexed_chunks<O, E, F, C>(
        &self,
        item_count: usize,
        maximum_chunk_items: usize,
        analyze: F,
        mut commit: C,
    ) -> Result<(), E>
    where
        O: Send,
        E: From<RuntimeError> + Send,
        F: Fn(usize) -> Result<O, E> + Send + Sync,
        C: FnMut(Range<usize>, Vec<O>) -> Result<(), E>,
    {
        if maximum_chunk_items == 0 {
            return Err(RuntimeError::ZeroChunkSize.into());
        }
        if item_count == 0 {
            return Ok(());
        }
        let chunk_items = maximum_chunk_items.min(item_count);
        for start in (0..item_count).step_by(chunk_items) {
            if self.is_cancelled() {
                return Err(RuntimeError::Cancelled.into());
            }
            let range = start..(start + chunk_items).min(item_count);
            let worker_count = indexed_worker_count(range.len(), self.parallelism());
            let values = if worker_count > 1 {
                let grain = parallel_grain(range.len(), worker_count);
                self.inner.pool.install(|| {
                    range
                        .clone()
                        .into_par_iter()
                        .with_min_len(grain)
                        .map(|index| {
                            if self.is_cancelled() {
                                Err(RuntimeError::Cancelled.into())
                            } else {
                                analyze(index)
                            }
                        })
                        .collect::<Result<Vec<_>, E>>()
                })?
            } else {
                range
                    .clone()
                    .map(|index| {
                        if self.is_cancelled() {
                            Err(RuntimeError::Cancelled.into())
                        } else {
                            analyze(index)
                        }
                    })
                    .collect::<Result<Vec<_>, E>>()?
            };
            commit(range, values)?;
            self.inner.completed_batches.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Folds a dense arena into shard-local accumulators. The returned shards
    /// are ordered by their input ranges so callers can merge them
    /// deterministically without handling scheduling or partition sizes.
    ///
    /// # Errors
    ///
    /// Returns the fold callback error selected in stable shard order, or
    /// cancellation converted through `E: From<RuntimeError>`.
    pub fn fold_indexed<S, E, I, F>(
        &self,
        item_count: usize,
        initialize: I,
        fold: F,
    ) -> Result<Vec<S>, E>
    where
        S: Send,
        E: From<RuntimeError> + Send,
        I: Fn() -> S + Send + Sync,
        F: Fn(&mut S, usize) -> Result<(), E> + Send + Sync,
    {
        self.map_ordered_in_scope(self.indexed_tasks(item_count), |range| {
            let mut state = initialize();
            for index in range {
                fold(&mut state, index)?;
            }
            Ok(state)
        })
    }

    /// Sorts an analysis arena in place using the shared worker pool. The
    /// result follows `Ord` exactly and is therefore independent of worker
    /// count even though equal elements may be reordered.
    pub fn sort_unstable<T>(&self, values: &mut [T])
    where
        T: Ord + Send,
    {
        if self.parallelism() == 1 || values.len() < MIN_PARALLEL_INDEXED_ITEMS {
            values.sort_unstable();
        } else {
            self.inner.pool.install(|| values.par_sort_unstable());
        }
    }

    /// Sorts an analysis arena in place with a caller-provided total ordering.
    /// The comparator must describe the same ordering regardless of worker
    /// count; the resulting element sequence is then deterministic whenever
    /// the ordering includes a stable tie-breaker.
    pub fn sort_unstable_by<T, F>(&self, values: &mut [T], compare: F)
    where
        T: Send,
        F: Fn(&T, &T) -> CmpOrdering + Send + Sync,
    {
        if self.parallelism() == 1 || values.len() < MIN_PARALLEL_INDEXED_ITEMS {
            values.sort_unstable_by(compare);
        } else {
            self.inner
                .pool
                .install(|| values.par_sort_unstable_by(compare));
        }
    }

    fn indexed_tasks(&self, item_count: usize) -> Vec<Task<Range<usize>>> {
        let worker_count = indexed_worker_count(item_count, self.parallelism());
        let grain = if worker_count == 1 {
            item_count.max(1)
        } else {
            parallel_grain(item_count, worker_count)
        };
        range_tasks(0, item_count, grain)
    }

    fn dependency_execution_plan(
        &self,
        worklist: &DependencyWorklist<'_>,
        plan: &DependencyPublicationPlan,
    ) -> Result<DependencyExecutionPlan, RuntimeError> {
        if plan.item_count() != worklist.item_count() {
            return Err(RuntimeError::InvalidDependencyPlan {
                detail: "dependency publication and worklist item arenas differ",
            });
        }
        let scheduled_items = worklist.scheduled_count();
        let scheduled_rows = worklist.scheduled_owned_rows(plan);
        Ok(DependencyExecutionPlan {
            worker_count: scheduled_items.min(self.parallelism()),
            scheduled_items,
            scheduled_rows,
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct DependencyExecutionPlan {
    worker_count: usize,
    scheduled_items: usize,
    scheduled_rows: usize,
}
