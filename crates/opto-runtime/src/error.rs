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
    /// Rayon could not create the worker pool.
    #[error("cannot create worker pool: {0}")]
    WorkerPool(#[source] rayon::ThreadPoolBuildError),
    /// Two ordered tasks use the same deterministic key.
    #[error("duplicate execution task key {0}")]
    DuplicateTaskKey(TaskKey),
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
    /// Indexed chunking was requested with a zero grain.
    #[error("indexed analysis chunk size must be greater than zero")]
    ZeroChunkSize,
    /// Cooperative cancellation stopped the operation.
    #[error("operation cancelled")]
    Cancelled,
}
