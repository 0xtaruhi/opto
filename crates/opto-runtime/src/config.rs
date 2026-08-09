// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

/// Worker-pool configuration.
#[derive(Debug)]
pub struct ExecutionConfig {
    /// Maximum number of Rayon worker threads; must be nonzero.
    pub max_threads: usize,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            max_threads: std::thread::available_parallelism().map_or(1, usize::from),
        }
    }
}
