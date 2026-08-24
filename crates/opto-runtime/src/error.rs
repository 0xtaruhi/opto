// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::indexed::TaskKey;
use thiserror::Error;

#[derive(Debug, Error)]
/// Runtime-wide configuration, cancellation, or scheduling failure.
pub enum RuntimeError {
    /// Execution was configured without a worker.
    #[error("execution context requires at least one worker thread")]
    NoWorkerThreads,
    /// Remote execution was configured without an endpoint.
    #[error("remote executor requires at least one worker endpoint")]
    NoRemoteWorkers,
    /// Rayon could not create the worker pool.
    #[error("cannot create worker pool: {0}")]
    WorkerPool(#[source] rayon::ThreadPoolBuildError),
    /// Two ordered tasks use the same deterministic key.
    #[error("duplicate execution task key {0}")]
    DuplicateTaskKey(TaskKey),
    /// One task cannot fit the configured private-memory admission limit.
    #[error("execution task {task} estimates {estimated} memory units, above the limit {limit}")]
    TaskMemoryExceedsLimit {
        /// Rejected stable task key.
        task: TaskKey,
        /// Declared peak private-memory units.
        estimated: u64,
        /// Configured in-flight limit in the same units.
        limit: u64,
    },
    /// A dense dependency plan violates a structural invariant.
    #[error("invalid execution dependency plan: {detail}")]
    InvalidDependencyPlan {
        /// Static diagnostic describing the violated invariant.
        detail: &'static str,
    },
    /// An internal scheduler channel or task-state invariant was violated.
    #[error("execution scheduler invariant failed: {detail}")]
    SchedulerInvariant {
        /// Static diagnostic describing the violated invariant.
        detail: &'static str,
    },
    /// Every deterministic remote attempt failed for one packet.
    #[error("remote execution task {task} failed after {attempts} attempt(s): {detail}")]
    RemoteTask {
        /// Stable packet key whose attempts failed.
        task: TaskKey,
        /// Number of worker attempts performed.
        attempts: usize,
        /// Last retryable failure or the first fatal failure.
        detail: Box<str>,
    },
    /// Indexed chunking was requested with a zero grain.
    #[error("indexed analysis chunk size must be greater than zero")]
    ZeroChunkSize,
    /// Cooperative cancellation stopped the operation.
    #[error("operation cancelled")]
    Cancelled,
}
