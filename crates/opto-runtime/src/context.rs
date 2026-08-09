// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Shared deterministic scheduler and cancellation domain.

use crate::config::ExecutionConfig;
use crate::error::RuntimeError;
use crate::indexed::Task;
use rayon::prelude::*;
use std::any::Any;
use std::num::NonZeroUsize;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;

type CompositeCompletion<O, E> = Result<Result<O, E>, Box<dyn Any + Send>>;

#[derive(Debug, Clone)]
/// Cloneable handle to one scheduler and cancellation domain.
pub struct ExecutionContext {
    pub(crate) inner: Arc<ExecutionContextInner>,
    parallelism_limit: Option<NonZeroUsize>,
}

#[derive(Debug)]
pub(crate) struct ExecutionContextInner {
    pub(crate) pool: rayon::ThreadPool,
    pub(crate) cancelled: AtomicBool,
    pub(crate) completed_task_callbacks: AtomicU64,
    pub(crate) completed_batches: AtomicU64,
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
            }),
            parallelism_limit: None,
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

    /// Evaluates weighted composite tasks with bounded outer concurrency.
    ///
    /// The heaviest tasks launch first. At most the square root of the worker
    /// count are in flight, and every callback receives a shared-pool context
    /// sized for the complementary inner dimension. Completed outer slots are
    /// refilled immediately, while returned values and errors remain ordered by
    /// [`TaskKey`](crate::TaskKey). This keeps nested work stealable without admitting an
    /// unbounded number of large per-task working sets.
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
        let workers = self.parallelism().max(1);
        if workers == 1 {
            return tasks
                .into_iter()
                .map(|task| operation(task.input, self))
                .collect();
        }

        let outer_parallelism = workers.isqrt().max(1).min(tasks.len());
        let inner_parallelism = workers.div_ceil(outer_parallelism);
        let inner_runtime = self.with_parallelism_limit(
            NonZeroUsize::new(inner_parallelism).unwrap_or(NonZeroUsize::MIN),
        );
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
                    scope.spawn(move |_| {
                        let result = catch_unwind(AssertUnwindSafe(|| {
                            if inner_runtime.is_cancelled() {
                                Err(RuntimeError::Cancelled.into())
                            } else {
                                operation(task.input, inner_runtime)
                            }
                        }));
                        completed.fetch_add(1, Ordering::Relaxed);
                        let _ = sender.send((index, result));
                    });
                    Ok(())
                };
                let mut next = 0usize;
                for &index in launch_order.iter().take(outer_parallelism) {
                    launch(index)?;
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
                    if let Some(&index) = launch_order.get(next) {
                        launch(index)?;
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
/// Point-in-time execution counters for one runtime.
pub struct ExecutionMetrics {
    /// Task callbacks that returned.
    pub completed_task_callbacks: u64,
    /// Successful indexed analyses, chunk commits, and dependency publications.
    pub completed_batches: u64,
}
