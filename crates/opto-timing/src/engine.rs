// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Shared timing engine and generation-keyed propagation cache.
//!
//! Cached propagation is reusable only when the model `Arc`, constraint
//! context, startpoint filter, and delay type match. Endpoint filters affect
//! path extraction rather than forward propagation and therefore do not enter
//! the cache basis.

mod incremental;

pub use incremental::{IncrementalTiming, IncrementalTimingMemory, RegionEdit};

use crate::analysis::{
    PropagationState, all_net_timing_states, analyze_propagation_paths, analyze_timing_paths,
    analyze_timing_quality, electrical_snapshot, propagate_all_with_path_tracking,
    propagation_net_count, update_propagation, worst_analysis,
};
use crate::{
    DelayType, ReportTimingOptions, TimingAnalysis, TimingContext, TimingElectricalSnapshot,
    TimingModel, TimingNetStates, TimingQuality,
};
use opto_runtime::ExecutionContext;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
/// Monotonic counters describing propagation-cache effectiveness.
pub struct TimingEngineMetrics {
    /// Full-graph propagation runs.
    pub full_updates: u64,
    /// Dirty-cone propagation updates.
    pub incremental_updates: u64,
    /// Requests served without propagation.
    pub cache_hits: u64,
    /// Total nets recomputed by full and incremental runs.
    pub recomputed_nets: u64,
}

#[derive(Debug)]
/// Thread-safe owner of timing propagation caches.
pub struct TimingEngine {
    runtime: ExecutionContext,
    state: Mutex<EngineState>,
}

#[derive(Debug, Default)]
struct EngineState {
    caches: Vec<PropagationCache>,
    metrics: TimingEngineMetrics,
}

#[derive(Debug)]
struct PropagationCache {
    model: Arc<TimingModel>,
    timing: TimingContext,
    basis: PropagationBasis,
    propagation: PropagationState,
    electrical: Option<TimingElectricalSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PropagationBasis {
    from: Vec<String>,
    delay_type: DelayType,
}

impl PropagationBasis {
    fn from_options(options: &ReportTimingOptions) -> Self {
        Self {
            from: options.from.clone(),
            delay_type: options.delay_type,
        }
    }

    fn matches(&self, options: &ReportTimingOptions) -> bool {
        self.delay_type == options.delay_type && self.from == options.from
    }
}

#[derive(Debug, Clone, Copy)]
enum UpdateKind {
    Full(usize),
    Incremental(usize),
    Hit,
}

impl TimingEngine {
    /// Creates an empty engine using `runtime` for bounded parallel work.
    #[must_use]
    pub fn new(runtime: ExecutionContext) -> Self {
        Self {
            runtime,
            state: Mutex::new(EngineState::default()),
        }
    }

    /// Returns generation-stamped state for every timing net.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid constraints/model state, propagation
    /// failure, or poisoned engine state.
    pub fn net_states(
        &self,
        timing: &TimingContext,
        model: Arc<TimingModel>,
        delay_type: DelayType,
    ) -> Result<TimingNetStates, crate::TimingError> {
        let generation = model.generation();
        let options = ReportTimingOptions {
            delay_type,
            ..ReportTimingOptions::default()
        };
        self.with_propagation(timing, model, &options, |model, propagation, _| {
            Ok(TimingNetStates::new(
                generation,
                all_net_timing_states(timing, model, propagation, delay_type),
            ))
        })
    }

    /// Returns the compact immutable electrical state consumed by power.
    ///
    /// Repeated requests for an unchanged model, constraint revision, and
    /// delay type clone the same [`Arc`]-backed snapshot. The snapshot contains
    /// no report names or arrival/required/slack DTOs.
    ///
    /// # Errors
    ///
    /// Returns an error when topology validation, propagation, compact snapshot
    /// allocation, runtime execution, or cache locking fails.
    pub fn electrical_snapshot(
        &self,
        timing: &TimingContext,
        model: Arc<TimingModel>,
        delay_type: DelayType,
    ) -> Result<TimingElectricalSnapshot, crate::TimingError> {
        let options = ReportTimingOptions {
            delay_type,
            ..ReportTimingOptions::default()
        };
        self.with_propagation(timing, model, &options, |model, propagation, cached| {
            if let Some(snapshot) = cached {
                return Ok(snapshot.clone());
            }
            let snapshot = electrical_snapshot(timing, model, propagation, delay_type)?;
            *cached = Some(snapshot.clone());
            Ok(snapshot)
        })
    }

    /// Analyzes and returns the worst path selected by `options`.
    ///
    /// # Errors
    ///
    /// Returns an error when propagation or path reconstruction fails, including
    /// when no reportable path exists.
    pub fn analyze(
        &self,
        timing: &TimingContext,
        model: Arc<TimingModel>,
        options: &ReportTimingOptions,
    ) -> Result<TimingAnalysis, crate::TimingError> {
        worst_analysis(self.analyze_paths(timing, model, options)?)
    }

    /// Computes detailed endpoint-slack quality and the worst path.
    ///
    /// This one-shot operation does not retain report-path propagation state.
    ///
    /// # Errors
    ///
    /// Returns an error when cache locking, propagation, path reconstruction,
    /// runtime execution, or monotonic metric accounting fails.
    pub fn quality(
        &self,
        timing: &TimingContext,
        model: &Arc<TimingModel>,
        options: &ReportTimingOptions,
    ) -> Result<TimingQuality, crate::TimingError> {
        self.discard_incompatible_caches(model)?;
        let quality = analyze_timing_quality(timing, model, options)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| crate::TimingError::EnginePoisoned)?;
        record_update(
            &mut state.metrics,
            UpdateKind::Full(propagation_net_count(model)),
        )?;
        Ok(quality)
    }

    /// Analyze every path selected by `options`.
    ///
    /// # Errors
    ///
    /// Returns an error when propagation or path reconstruction fails.
    pub fn analyze_paths(
        &self,
        timing: &TimingContext,
        model: Arc<TimingModel>,
        options: &ReportTimingOptions,
    ) -> Result<Vec<TimingAnalysis>, crate::TimingError> {
        if !options.from.is_empty() {
            self.discard_incompatible_caches(&model)?;
            let analyses = analyze_timing_paths(timing, &model, options)?;
            let mut state = self
                .state
                .lock()
                .map_err(|_| crate::TimingError::EnginePoisoned)?;
            record_update(
                &mut state.metrics,
                UpdateKind::Full(propagation_net_count(&model)),
            )?;
            return Ok(analyses);
        }

        self.with_propagation(timing, model, options, |model, propagation, _| {
            analyze_propagation_paths(timing, model, options, propagation)
        })
    }

    #[cfg(test)]
    pub(crate) fn analyze_once(
        timing: &TimingContext,
        model: &TimingModel,
        options: &ReportTimingOptions,
    ) -> Result<TimingAnalysis, crate::TimingError> {
        crate::analysis::analyze_timing(timing, model, options)
    }

    /// Returns a snapshot of monotonic engine counters.
    ///
    /// # Errors
    ///
    /// Returns [`crate::TimingError::EnginePoisoned`] if an earlier panic poisoned the
    /// engine-state mutex.
    pub fn metrics(&self) -> Result<TimingEngineMetrics, crate::TimingError> {
        self.state
            .lock()
            .map(|state| state.metrics)
            .map_err(|_| crate::TimingError::EnginePoisoned)
    }

    /// Drops all retained propagation caches.
    ///
    /// If the cache mutex was poisoned, clearing also restores usable engine
    /// state and resets metrics.
    pub fn clear(&self) {
        match self.state.lock() {
            Ok(mut state) => state.caches.clear(),
            Err(poisoned) => {
                *poisoned.into_inner() = EngineState::default();
                self.state.clear_poison();
            }
        }
    }

    fn with_propagation<T>(
        &self,
        timing: &TimingContext,
        model: Arc<TimingModel>,
        options: &ReportTimingOptions,
        materialize: impl FnOnce(
            &TimingModel,
            &PropagationState,
            &mut Option<TimingElectricalSnapshot>,
        ) -> Result<T, crate::TimingError>,
    ) -> Result<T, crate::TimingError> {
        let net_count = propagation_net_count(&model);
        let mut state = self
            .state
            .lock()
            .map_err(|_| crate::TimingError::EnginePoisoned)?;
        let cache_index = state
            .caches
            .iter()
            .position(|cache| Arc::ptr_eq(&cache.model, &model) && cache.basis.matches(options));

        if let Some(cache_index) = cache_index {
            if state.caches[cache_index].timing == *timing {
                let cache = &mut state.caches[cache_index];
                let output = materialize(&model, &cache.propagation, &mut cache.electrical)?;
                record_update(&mut state.metrics, UpdateKind::Hit)?;
                return Ok(output);
            }

            let mut cache = state.caches.remove(cache_index);
            let dirty = update_propagation(
                &cache.timing,
                timing,
                &model,
                options,
                &mut cache.propagation,
            )?;
            cache.timing = timing.clone();
            cache.electrical = None;
            let output = materialize(&model, &cache.propagation, &mut cache.electrical)?;
            state.caches.push(cache);
            record_update(
                &mut state.metrics,
                if dirty == 0 {
                    UpdateKind::Hit
                } else {
                    UpdateKind::Incremental(dirty)
                },
            )?;
            return Ok(output);
        }

        // Discard only derived state that cannot satisfy this request before
        // constructing its replacement. Independent bases of the same model
        // remain reusable.
        state
            .caches
            .retain(|cache| Arc::ptr_eq(&cache.model, &model) && !cache.basis.matches(options));
        let propagation =
            propagate_all_with_path_tracking(timing, &model, options, true, Some(&self.runtime))?;
        let mut electrical = None;
        let output = materialize(&model, &propagation, &mut electrical)?;
        state.caches.push(PropagationCache {
            model,
            timing: timing.clone(),
            basis: PropagationBasis::from_options(options),
            propagation,
            electrical,
        });
        record_update(&mut state.metrics, UpdateKind::Full(net_count))?;
        Ok(output)
    }

    fn discard_incompatible_caches(
        &self,
        model: &Arc<TimingModel>,
    ) -> Result<(), crate::TimingError> {
        self.state
            .lock()
            .map_err(|_| crate::TimingError::EnginePoisoned)?
            .caches
            .retain(|cache| Arc::ptr_eq(&cache.model, model));
        Ok(())
    }
}

fn record_update(
    metrics: &mut TimingEngineMetrics,
    update: UpdateKind,
) -> Result<(), crate::TimingError> {
    match update {
        UpdateKind::Full(nets) => {
            metrics.full_updates = checked_increment(metrics.full_updates, "full-update")?;
            metrics.recomputed_nets =
                checked_add_nets(metrics.recomputed_nets, nets, "recomputed-net")?;
        }
        UpdateKind::Incremental(nets) => {
            metrics.incremental_updates =
                checked_increment(metrics.incremental_updates, "incremental-update")?;
            metrics.recomputed_nets =
                checked_add_nets(metrics.recomputed_nets, nets, "recomputed-net")?;
        }
        UpdateKind::Hit => {
            metrics.cache_hits = checked_increment(metrics.cache_hits, "cache-hit")?;
        }
    }
    Ok(())
}

fn checked_increment(value: u64, metric: &'static str) -> Result<u64, crate::TimingError> {
    value
        .checked_add(1)
        .ok_or_else(|| crate::TimingEngineError::MetricOverflow { metric }.into())
}

fn checked_add_nets(
    value: u64,
    nets: usize,
    metric: &'static str,
) -> Result<u64, crate::TimingError> {
    let nets =
        u64::try_from(nets).map_err(|_| crate::TimingEngineError::MetricOverflow { metric })?;
    value
        .checked_add(nets)
        .ok_or_else(|| crate::TimingEngineError::MetricOverflow { metric }.into())
}

#[cfg(test)]
mod tests;
