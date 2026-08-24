// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Deterministic parallel execution.
//!
//! [`ExecutionContext`] owns the worker pool and cancellation state. Indexed
//! jobs separate immutable planning from ordered publication: workers may
//! evaluate tasks in parallel, while results are committed in stable task-key
//! order.

mod config;
mod context;
mod dependency;
mod error;
mod indexed;
mod remote;

pub use config::ExecutionConfig;
pub use context::{ExecutionContext, ExecutionMetrics};
pub use dependency::{
    DependencyActivation, DependencyDirection, DependencyEffects, DependencyExecution,
    DependencyPlan, DependencyPublication, DependencyPublicationPlan, DependencyRowStore,
    DependencyWorklist,
};
pub use error::RuntimeError;
pub use indexed::{DependencyRun, Task, TaskKey, indexed_worker_count};
pub use remote::{RemoteExecutor, RemotePacket, RemoteResult, RemoteWorker, RemoteWorkerError};
#[cfg(test)]
mod tests;
