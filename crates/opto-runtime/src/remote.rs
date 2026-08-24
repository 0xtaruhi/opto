// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Deterministic packet dispatch across fallible remote workers.

use crate::{ExecutionConfig, ExecutionContext, RuntimeError, Task, TaskKey};
use std::sync::Arc;

/// One opaque, self-contained request ordered by a stable task key.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RemotePacket {
    key: TaskKey,
    payload: Box<[u8]>,
    estimated_work: u64,
    estimated_memory: u64,
}

impl RemotePacket {
    /// Creates a packet whose payload is interpreted by the selected worker ABI.
    #[must_use]
    pub fn new(key: TaskKey, payload: impl Into<Box<[u8]>>) -> Self {
        Self {
            key,
            payload: payload.into(),
            estimated_work: 1,
            estimated_memory: 1,
        }
    }

    /// Attaches deterministic scheduling estimates to the packet.
    #[must_use]
    pub const fn with_estimates(mut self, work: u64, memory: u64) -> Self {
        self.estimated_work = work;
        self.estimated_memory = memory;
        self
    }

    /// Returns the stable logical request key.
    #[must_use]
    pub const fn key(&self) -> TaskKey {
        self.key
    }

    /// Returns the worker-ABI payload.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
}

/// One opaque remote response paired with its request key.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RemoteResult {
    key: TaskKey,
    payload: Box<[u8]>,
}

impl RemoteResult {
    /// Returns the stable request key.
    #[must_use]
    pub const fn key(&self) -> TaskKey {
        self.key
    }

    /// Returns the worker response payload.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Consumes the response and returns its payload.
    #[must_use]
    pub fn into_payload(self) -> Box<[u8]> {
        self.payload
    }
}

/// Failure classification returned by a remote worker transport.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RemoteWorkerError {
    /// The request may be replayed on the next deterministic worker.
    #[error("retryable remote worker failure: {0}")]
    Retryable(Box<str>),
    /// Replaying the request cannot change the outcome.
    #[error("fatal remote worker failure: {0}")]
    Fatal(Box<str>),
}

/// Transport endpoint for one compatible remote worker.
pub trait RemoteWorker: std::fmt::Debug + Send + Sync {
    /// Executes one opaque packet under the worker's versioned algorithm ABI.
    ///
    /// # Errors
    ///
    /// Returns [`RemoteWorkerError::Retryable`] only when replay is safe. A
    /// fatal error stops retry immediately.
    fn execute(&self, request: &[u8]) -> Result<Box<[u8]>, RemoteWorkerError>;
}

/// Ordered packet executor with deterministic worker rotation and retry.
#[derive(Debug)]
pub struct RemoteExecutor {
    runtime: ExecutionContext,
    workers: Box<[Arc<dyn RemoteWorker>]>,
}

impl RemoteExecutor {
    /// Creates an executor over a non-empty compatible worker set.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::NoRemoteWorkers`] when no endpoint is supplied,
    /// or the local scheduling-pool construction error.
    pub fn new(
        workers: impl IntoIterator<Item = Arc<dyn RemoteWorker>>,
    ) -> Result<Self, RuntimeError> {
        let workers = workers.into_iter().collect::<Vec<_>>().into_boxed_slice();
        if workers.is_empty() {
            return Err(RuntimeError::NoRemoteWorkers);
        }
        let runtime = ExecutionContext::new(&ExecutionConfig {
            max_threads: workers.len(),
        })?;
        Ok(Self { runtime, workers })
    }

    /// Dispatches packets in parallel and returns responses in stable key order.
    ///
    /// A retryable failure rotates to the next worker. Each worker is attempted
    /// at most once for a packet; a fatal failure stops that packet immediately.
    /// Packet identity, result order, and failure selection never depend on
    /// completion order.
    ///
    /// # Errors
    ///
    /// Returns the first failure in stable task-key order after deterministic
    /// retry is exhausted.
    pub fn execute(&self, packets: Vec<RemotePacket>) -> Result<Vec<RemoteResult>, RuntimeError> {
        let tasks = packets
            .into_iter()
            .map(|packet| {
                let work = packet.estimated_work;
                let memory = packet.estimated_memory;
                Task::new(packet.key, packet)
                    .with_estimated_work(work)
                    .with_estimated_memory(memory)
            })
            .collect();
        self.runtime.map_ordered_composite(tasks, |packet, _| {
            let start = worker_start(packet.key, self.workers.len());
            let mut last = None;
            for attempt in 0..self.workers.len() {
                let worker = &self.workers[(start + attempt) % self.workers.len()];
                match worker.execute(&packet.payload) {
                    Ok(payload) => {
                        return Ok(RemoteResult {
                            key: packet.key,
                            payload,
                        });
                    }
                    Err(RemoteWorkerError::Retryable(detail)) => last = Some(detail),
                    Err(RemoteWorkerError::Fatal(detail)) => {
                        return Err(RuntimeError::RemoteTask {
                            task: packet.key,
                            attempts: attempt + 1,
                            detail,
                        });
                    }
                }
            }
            Err(RuntimeError::RemoteTask {
                task: packet.key,
                attempts: self.workers.len(),
                detail: last.unwrap_or_else(|| "remote worker returned no result".into()),
            })
        })
    }
}

fn worker_start(key: TaskKey, workers: usize) -> usize {
    let mixed = key.ordinal() ^ (u64::from(key.domain()) << 32);
    usize::try_from(mixed % workers as u64).expect("worker index fits usize")
}
