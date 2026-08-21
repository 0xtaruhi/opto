// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Shared deterministic scheduler and cancellation domain.

use crate::config::ExecutionConfig;
use crate::error::RuntimeError;
use crate::indexed::Task;
use rayon::prelude::*;
use std::any::Any;
use std::num::{NonZeroU64, NonZeroUsize};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Instant;

type CompositeCompletion<O, E> = Result<Result<O, E>, Box<dyn Any + Send>>;

#[derive(Debug, Clone)]
/// Cloneable handle to one scheduler and cancellation domain.
pub struct ExecutionContext {
    pub(crate) inner: Arc<ExecutionContextInner>,
    parallelism_limit: Option<NonZeroUsize>,
    memory_limit: Option<NonZeroU64>,
}

#[derive(Debug)]
pub(crate) struct ExecutionContextInner {
    pub(crate) pool: rayon::ThreadPool,
    pub(crate) cancelled: AtomicBool,
    pub(crate) completed_task_callbacks: AtomicU64,
    pub(crate) completed_batches: AtomicU64,
    pub(crate) composite_batches: AtomicU64,
    pub(crate) composite_active_nanoseconds: AtomicU64,
    pub(crate) composite_wall_nanoseconds: AtomicU64,
    pub(crate) composite_estimated_work: AtomicU64,
    pub(crate) composite_peak_ready_tasks: AtomicU64,
    pub(crate) composite_peak_admitted_memory: AtomicU64,
}

impl Default for ExecutionContext {
    fn default() -> Self {
        match Self::new(&ExecutionConfig::default()) {
            Ok(context) => context,
            Err(error) => panic!("failed to create Opto execution context: {error}"),
        }
    }
}

impl ExecutionContext {
    /// Creates a deterministic worker pool.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] for zero workers or worker-pool construction
    /// failure.
    pub fn new(config: &ExecutionConfig) -> Result<Self, RuntimeError> {
        if config.max_threads == 0 {
            return Err(RuntimeError::NoWorkerThreads);
        }
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(config.max_threads)
            .thread_name(|index| format!("opto-worker-{index}"))
            .build()
            .map_err(RuntimeError::WorkerPool)?;
        Ok(Self {
            inner: Arc::new(ExecutionContextInner {
                pool,
                cancelled: AtomicBool::new(false),
                completed_task_callbacks: AtomicU64::new(0),
                completed_batches: AtomicU64::new(0),
                composite_batches: AtomicU64::new(0),
                composite_active_nanoseconds: AtomicU64::new(0),
                composite_wall_nanoseconds: AtomicU64::new(0),
                composite_estimated_work: AtomicU64::new(0),
                composite_peak_ready_tasks: AtomicU64::new(0),
                composite_peak_admitted_memory: AtomicU64::new(0),
            }),
            parallelism_limit: None,
            memory_limit: None,
        })
    }

    /// Returns a handle sharing this scheduler while using at most `maximum`
    /// workers.
    #[must_use]
    pub fn with_parallelism_limit(&self, maximum: NonZeroUsize) -> Self {
        let mut limited = self.clone();
        limited.parallelism_limit = Some(
            self.parallelism_limit
                .map_or(maximum, |current| current.min(maximum)),
        );
        limited
    }

    /// Returns a handle that admits composite tasks only while their summed
    /// private-memory estimates fit `maximum`.
    #[must_use]
    pub fn with_memory_limit(&self, maximum: NonZeroU64) -> Self {
        let mut limited = self.clone();
        limited.memory_limit = Some(
            self.memory_limit
                .map_or(maximum, |current| current.min(maximum)),
        );
        limited
    }

    #[must_use]
    /// Returns the effective worker limit for this handle.
    pub fn parallelism(&self) -> usize {
        self.parallelism_limit.map_or_else(
            || self.inner.pool.current_num_threads(),
            |limit| limit.get().min(self.inner.pool.current_num_threads()),
        )
    }

    /// Returns whether both handles share one scheduler.
    #[must_use]
    pub fn is_same_runtime(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// Requests cooperative cancellation.
    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::Release);
    }

    #[must_use]
    /// Returns whether cancellation is currently requested.
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
    }

    #[must_use]
    /// Returns point-in-time scheduler counters.
    pub fn metrics(&self) -> ExecutionMetrics {
        ExecutionMetrics {
            completed_task_callbacks: self.inner.completed_task_callbacks.load(Ordering::Relaxed),
            completed_batches: self.inner.completed_batches.load(Ordering::Relaxed),
            composite_batches: self.inner.composite_batches.load(Ordering::Relaxed),
            composite_active_nanoseconds: self
                .inner
                .composite_active_nanoseconds
                .load(Ordering::Relaxed),
            composite_wall_nanoseconds: self
                .inner
                .composite_wall_nanoseconds
                .load(Ordering::Relaxed),
            composite_estimated_work: self.inner.composite_estimated_work.load(Ordering::Relaxed),
            composite_peak_ready_tasks: self
                .inner
                .composite_peak_ready_tasks
                .load(Ordering::Relaxed),
            composite_peak_admitted_memory: self
                .inner
                .composite_peak_admitted_memory
                .load(Ordering::Relaxed),
        }
    }

    /// Evaluates tasks in parallel and returns results in stable key order.
    ///
    /// # Errors
    ///
    /// Returns the callback error selected by stable task-key order, or a
    /// converted [`RuntimeError`] if cancellation prevents completion.
    pub fn map_ordered<I, O, E, F>(&self, tasks: Vec<Task<I>>, operation: F) -> Result<Vec<O>, E>
    where
        I: Send,
        O: Send,
        E: From<RuntimeError> + Send,
        F: Fn(I) -> Result<O, E> + Send + Sync,
    {
        self.map_ordered_in_scope(tasks, operation)
    }

    /// Evaluates weighted moldable tasks on one shared work-stealing pool.
    ///
    /// The heaviest tasks launch first with at most one outer callback per
    /// worker. Every callback receives the complete shared runtime: when the
    /// ready outer queue is deep, workers naturally execute separate tasks;
    /// when it drains, nested Rayon work can occupy the idle workers. Returned
    /// values and errors remain ordered by [`TaskKey`](crate::TaskKey).
    ///
    /// # Errors
    ///
    /// Returns the callback error selected by stable task-key order, or a
    /// converted [`RuntimeError`] for cancellation or scheduler failure.
    pub fn map_ordered_composite<I, O, E, F>(
        &self,
        tasks: Vec<Task<I>>,
        operation: F,
    ) -> Result<Vec<O>, E>
    where
        I: Send,
        O: Send,
        E: From<RuntimeError> + Send,
        F: Fn(I, &ExecutionContext) -> Result<O, E> + Send + Sync,
    {
        let tasks = self.prepare_ordered_tasks(tasks)?;
        if tasks.is_empty() {
            return Ok(Vec::new());
        }
        let _batch = CompositeBatchTimer::start(&self.inner, &tasks);
        let workers = self.parallelism().max(1);
        let memory_limit = self.memory_limit.map_or(u64::MAX, NonZeroU64::get);
        if let Some(task) = tasks
            .iter()
            .find(|task| task.estimated_memory > memory_limit)
        {
            return Err(RuntimeError::TaskMemoryExceedsLimit {
                task: task.key,
                estimated: task.estimated_memory,
                limit: memory_limit,
            }
            .into());
        }
        if workers == 1 {
            return tasks
                .into_iter()
                .map(|task| {
                    let started = Instant::now();
                    let result = operation(task.input, self);
                    record_active_time(&self.inner, started);
                    self.inner
                        .completed_task_callbacks
                        .fetch_add(1, Ordering::Relaxed);
                    result
                })
                .collect();
        }

        let outer_parallelism = workers.min(tasks.len());
        let inner_runtime = self;
        let mut launch_order = tasks
            .iter()
            .enumerate()
            .map(|(index, task)| (std::cmp::Reverse(task.estimated_work), task.key, index))
            .collect::<Vec<_>>();
        launch_order.sort_unstable();
        let launch_order = launch_order
            .into_iter()
            .map(|(_, _, index)| index)
            .collect::<Vec<_>>();
        let task_memory = tasks
            .iter()
            .map(|task| task.estimated_memory)
            .collect::<Vec<_>>();
        let mut tasks = tasks.into_iter().map(Some).collect::<Vec<_>>();
        let mut results = std::iter::repeat_with(|| None)
            .take(tasks.len())
            .collect::<Vec<Option<CompositeCompletion<O, E>>>>();
        let (sender, receiver) = mpsc::channel();
        self.inner
            .pool
            .in_place_scope(|scope| -> Result<(), RuntimeError> {
                let mut launch = |index: usize| -> Result<(), RuntimeError> {
                    let task = tasks[index]
                        .take()
                        .ok_or(RuntimeError::SchedulerInvariant {
                            detail: "composite task was launched more than once",
                        })?;
                    let sender = sender.clone();
                    let operation = &operation;
                    let inner_runtime = &inner_runtime;
                    let completed = &self.inner.completed_task_callbacks;
                    let metrics = &self.inner;
                    scope.spawn(move |_| {
                        let started = Instant::now();
                        let result = catch_unwind(AssertUnwindSafe(|| {
                            if inner_runtime.is_cancelled() {
                                Err(RuntimeError::Cancelled.into())
                            } else {
                                operation(task.input, inner_runtime)
                            }
                        }));
                        record_active_time(metrics, started);
                        completed.fetch_add(1, Ordering::Relaxed);
                        let _ = sender.send((index, result));
                    });
                    Ok(())
                };
                let mut next = 0usize;
                let mut active = 0usize;
                let mut admitted_memory = 0u64;
                while active < outer_parallelism
                    && let Some(&index) = launch_order.get(next)
                    && admitted_memory.saturating_add(task_memory[index]) <= memory_limit
                {
                    launch(index)?;
                    admitted_memory = admitted_memory.saturating_add(task_memory[index]);
                    self.inner
                        .composite_peak_admitted_memory
                        .fetch_max(admitted_memory, Ordering::Relaxed);
                    active += 1;
                    next += 1;
                }
                for _ in 0..launch_order.len() {
                    let (index, result) =
                        receiver
                            .recv()
                            .map_err(|_| RuntimeError::SchedulerInvariant {
                                detail: "composite worker stopped without returning its result",
                            })?;
                    results[index] = Some(result);
                    admitted_memory = admitted_memory.saturating_sub(task_memory[index]);
                    active -= 1;
                    while active < outer_parallelism
                        && let Some(&index) = launch_order.get(next)
                        && admitted_memory.saturating_add(task_memory[index]) <= memory_limit
                    {
                        launch(index)?;
                        admitted_memory = admitted_memory.saturating_add(task_memory[index]);
                        self.inner
                            .composite_peak_admitted_memory
                            .fetch_max(admitted_memory, Ordering::Relaxed);
                        active += 1;
                        next += 1;
                    }
                }
                Ok(())
            })
            .map_err(E::from)?;
        let mut outputs = Vec::with_capacity(results.len());
        for result in results {
            match result.ok_or_else(|| {
                E::from(RuntimeError::SchedulerInvariant {
                    detail: "composite task produced no ordered result",
                })
            })? {
                Ok(result) => outputs.push(result?),
                Err(payload) => resume_unwind(payload),
            }
        }
        Ok(outputs)
    }

    /// Evaluates outer tasks in parallel and gives each callback a serial view
    /// of the same pool for nested deterministic work.
    ///
    /// # Errors
    ///
    /// Returns the callback error selected by stable task-key order, or a
    /// converted [`RuntimeError`] if cancellation prevents completion.
    pub fn map_ordered_nested<I, O, E, F>(
        &self,
        tasks: Vec<Task<I>>,
        operation: F,
    ) -> Result<Vec<O>, E>
    where
        I: Send,
        O: Send,
        E: From<RuntimeError> + Send,
        F: Fn(I, &ExecutionContext) -> Result<O, E> + Send + Sync,
    {
        let nested = self.with_parallelism_limit(NonZeroUsize::MIN);
        self.map_ordered_in_scope(tasks, |input| operation(input, &nested))
    }

    /// Executes one ordered batch on this handle's existing pool view.
    ///
    /// This is the common implementation for public schedulers. Callbacks may
    /// complete in any order, but the pre-sorted task vector fixes both returned
    /// values and error selection; cancellation is checked before admission and
    /// again immediately before each callback.
    pub(crate) fn map_ordered_in_scope<I, O, E, F>(
        &self,
        tasks: Vec<Task<I>>,
        operation: F,
    ) -> Result<Vec<O>, E>
    where
        I: Send,
        O: Send,
        E: From<RuntimeError> + Send,
        F: Fn(I) -> Result<O, E> + Send + Sync,
    {
        let tasks = self.prepare_ordered_tasks(tasks)?;
        if self.is_cancelled() {
            return Err(RuntimeError::Cancelled.into());
        }

        if tasks.len() <= 1 || self.parallelism() == 1 {
            return tasks
                .into_iter()
                .map(|task| {
                    if self.is_cancelled() {
                        return Err(RuntimeError::Cancelled.into());
                    }
                    let result = operation(task.input);
                    self.inner
                        .completed_task_callbacks
                        .fetch_add(1, Ordering::Relaxed);
                    result
                })
                .collect();
        }

        let results = self.inner.pool.install(|| {
            tasks
                .into_par_iter()
                .map(|task| {
                    if self.is_cancelled() {
                        return Err(RuntimeError::Cancelled.into());
                    }
                    let result = operation(task.input);
                    self.inner
                        .completed_task_callbacks
                        .fetch_add(1, Ordering::Relaxed);
                    result
                })
                .collect::<Vec<_>>()
        });
        results.into_iter().collect()
    }

    fn prepare_ordered_tasks<I, E>(&self, mut tasks: Vec<Task<I>>) -> Result<Vec<Task<I>>, E>
    where
        E: From<RuntimeError>,
    {
        tasks.sort_unstable_by_key(|task| task.key);
        if let Some(duplicate) = tasks
            .windows(2)
            .find(|pair| pair[0].key == pair[1].key)
            .map(|pair| pair[0].key)
        {
            return Err(RuntimeError::DuplicateTaskKey(duplicate).into());
        }
        if self.is_cancelled() {
            return Err(RuntimeError::Cancelled.into());
        }
        Ok(tasks)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
/// Point-in-time execution counters for one runtime.
pub struct ExecutionMetrics {
    /// Task callbacks that returned.
    pub completed_task_callbacks: u64,
    /// Successful indexed analyses, chunk commits, and dependency publications.
    pub completed_batches: u64,
    /// Completed invocations of the hierarchical composite scheduler.
    pub composite_batches: u64,
    /// Sum of wall time spent inside composite task callbacks.
    pub composite_active_nanoseconds: u64,
    /// Sum of end-to-end wall time for composite scheduler invocations.
    pub composite_wall_nanoseconds: u64,
    /// Sum of declared work admitted to composite scheduler invocations.
    pub composite_estimated_work: u64,
    /// Largest ready composite task batch observed by this runtime.
    pub composite_peak_ready_tasks: u64,
    /// Largest simultaneous declared private-memory admission.
    pub composite_peak_admitted_memory: u64,
}

struct CompositeBatchTimer<'a> {
    metrics: &'a ExecutionContextInner,
    started: Instant,
}

impl<'a> CompositeBatchTimer<'a> {
    fn start<I>(metrics: &'a ExecutionContextInner, tasks: &[Task<I>]) -> Self {
        metrics.composite_batches.fetch_add(1, Ordering::Relaxed);
        metrics.composite_peak_ready_tasks.fetch_max(
            u64::try_from(tasks.len()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        metrics.composite_estimated_work.fetch_add(
            tasks.iter().fold(0_u64, |total, task| {
                total.saturating_add(task.estimated_work)
            }),
            Ordering::Relaxed,
        );
        Self {
            metrics,
            started: Instant::now(),
        }
    }
}

impl Drop for CompositeBatchTimer<'_> {
    fn drop(&mut self) {
        self.metrics
            .composite_wall_nanoseconds
            .fetch_add(elapsed_nanoseconds(self.started), Ordering::Relaxed);
    }
}

fn record_active_time(metrics: &ExecutionContextInner, started: Instant) {
    metrics
        .composite_active_nanoseconds
        .fetch_add(elapsed_nanoseconds(started), Ordering::Relaxed);
}

fn elapsed_nanoseconds(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}
