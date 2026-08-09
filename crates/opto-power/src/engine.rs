// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Cached full and activity-only incremental power analysis.
//!
//! A cache is reusable only for the same runtime, timing-model `Arc`, and
//! Arc-identified compact electrical snapshot. Activity changes then update the
//! affected combinational cones without rebuilding model-derived topology.

use crate::analysis::{PowerAnalysisState, PowerUpdateCounts};
use crate::{ActivityAnnotations, PowerAnalysis, PowerError};
use opto_runtime::ExecutionContext;
use opto_timing::{TimingElectricalSnapshot, TimingModel};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
/// Monotonic counters describing power-cache effectiveness.
pub struct PowerEngineMetrics {
    /// Complete analyses that replaced incompatible or absent cache state.
    pub full_updates: u64,
    /// Activity-only incremental analyses.
    pub incremental_updates: u64,
    /// Requests served by an unchanged cached result.
    pub cache_hits: u64,
    /// Nets recomputed across updates.
    pub recomputed_nets: u64,
    /// Cells recomputed across updates.
    pub recomputed_cells: u64,
}

#[derive(Debug, Default)]
/// Thread-safe owner of the generation-keyed power-analysis cache.
pub struct PowerEngine {
    state: Mutex<EngineState>,
}

#[derive(Debug, Default)]
struct EngineState {
    cache: Option<PowerCache>,
    metrics: PowerEngineMetrics,
}

#[derive(Debug)]
struct PowerCache {
    runtime: ExecutionContext,
    model: Arc<TimingModel>,
    electrical: TimingElectricalSnapshot,
    annotations: ActivityAnnotations,
    analysis: PowerAnalysisState,
}

impl PowerEngine {
    #[must_use]
    /// Creates an engine with no cached model or accumulated metrics.
    pub fn new() -> Self {
        Self::default()
    }

    /// Computes power, reusing or incrementally updating compatible state.
    ///
    /// # Errors
    ///
    /// Returns an error for generation mismatch, invalid activity/model data,
    /// analysis failure, or poisoned engine state.
    ///
    /// # Panics
    ///
    /// Panics if a cache observed as present and compatible disappears while
    /// the engine holds its exclusive state lock, indicating internal logic
    /// corruption.
    pub fn analyze(
        &self,
        runtime: &ExecutionContext,
        model: Arc<TimingModel>,
        electrical: TimingElectricalSnapshot,
        annotations: ActivityAnnotations,
    ) -> Result<PowerAnalysis, PowerError> {
        let mut state = self.state.lock().map_err(|_| PowerError::EnginePoisoned)?;
        let reusable = state.cache.as_ref().is_some_and(|cache| {
            cache.runtime.is_same_runtime(runtime)
                && Arc::ptr_eq(&cache.model, &model)
                && cache.electrical.is_same_snapshot(&electrical)
        });
        if reusable {
            let cache = state
                .cache
                .as_ref()
                .expect("reusable power cache was just tested");
            if cache.annotations == annotations {
                let analysis = cache.analysis.analysis.clone();
                record_update(&mut state.metrics, UpdateKind::Hit)?;
                return Ok(analysis);
            }

            let (counts, analysis) = {
                let cache = state
                    .cache
                    .as_mut()
                    .expect("reusable power cache was just tested");
                let counts = cache.analysis.update_activities(
                    runtime,
                    &model,
                    &electrical,
                    &cache.annotations,
                    &annotations,
                )?;
                cache.annotations = annotations;
                (counts, cache.analysis.analysis.clone())
            };
            record_update(&mut state.metrics, UpdateKind::Incremental(counts))?;
            return Ok(analysis);
        }

        let analysis = PowerAnalysis::analyze_state(runtime, &model, &electrical, &annotations)?;
        let counts = PowerUpdateCounts {
            nets: model.net_count(),
            cells: model.instance_count(),
        };
        let output = analysis.analysis.clone();
        state.cache = Some(PowerCache {
            runtime: runtime.clone(),
            model,
            electrical,
            annotations,
            analysis,
        });
        record_update(&mut state.metrics, UpdateKind::Full(counts))?;
        Ok(output)
    }

    /// Returns a snapshot of monotonic cache metrics.
    ///
    /// # Errors
    ///
    /// Returns [`PowerError::EnginePoisoned`] if a prior panic poisoned the
    /// cache-state mutex.
    pub fn metrics(&self) -> Result<PowerEngineMetrics, PowerError> {
        self.state
            .lock()
            .map(|state| state.metrics)
            .map_err(|_| PowerError::EnginePoisoned)
    }

    /// Drops cached derived state and recovers from mutex poisoning.
    pub fn clear(&self) {
        match self.state.lock() {
            Ok(mut state) => state.cache = None,
            Err(poisoned) => {
                *poisoned.into_inner() = EngineState::default();
                self.state.clear_poison();
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum UpdateKind {
    Full(PowerUpdateCounts),
    Incremental(PowerUpdateCounts),
    Hit,
}

fn record_update(metrics: &mut PowerEngineMetrics, update: UpdateKind) -> Result<(), PowerError> {
    let counts = match update {
        UpdateKind::Full(counts) => {
            metrics.full_updates = checked_add(metrics.full_updates, 1, "full_updates")?;
            counts
        }
        UpdateKind::Incremental(counts) => {
            metrics.incremental_updates =
                checked_add(metrics.incremental_updates, 1, "incremental_updates")?;
            counts
        }
        UpdateKind::Hit => {
            metrics.cache_hits = checked_add(metrics.cache_hits, 1, "cache_hits")?;
            return Ok(());
        }
    };
    metrics.recomputed_nets = checked_add(
        metrics.recomputed_nets,
        u64::try_from(counts.nets).map_err(|_| PowerError::MetricOverflow {
            metric: "recomputed_nets",
        })?,
        "recomputed_nets",
    )?;
    metrics.recomputed_cells = checked_add(
        metrics.recomputed_cells,
        u64::try_from(counts.cells).map_err(|_| PowerError::MetricOverflow {
            metric: "recomputed_cells",
        })?,
        "recomputed_cells",
    )?;
    Ok(())
}

fn checked_add(left: u64, right: u64, metric: &'static str) -> Result<u64, PowerError> {
    left.checked_add(right)
        .ok_or(PowerError::MetricOverflow { metric })
}

#[cfg(test)]
mod tests {
    use super::*;
    use opto_db::ObjectUid;
    use opto_library::{
        ArcDelayModel, LookupTable, NldmTimingModel, PowerCell, PowerLibraryUnits, TargetCell,
        TargetCellUsage, TargetPin, TargetPinDirection, TargetTimingArc, TargetTimingType,
        TimingSense,
    };
    use opto_timing::{
        DelayType, DesignId, TimingContext, TimingDesign, TimingEngine, TimingLibrary,
    };

    fn runtime() -> ExecutionContext {
        ExecutionContext::new(&opto_runtime::ExecutionConfig { max_threads: 1 }).unwrap()
    }

    fn test_model(name: &str, uid: u64) -> Arc<TimingModel> {
        test_model_with_leakage(name, uid, 1.0)
    }

    fn test_model_with_leakage(name: &str, uid: u64, leakage: f64) -> Arc<TimingModel> {
        let mut library = TimingLibrary::default();
        library.power.units = PowerLibraryUnits {
            time_seconds: Some(1e-9),
            capacitance_farads: Some(1e-12),
            voltage_volts: Some(1.0),
            leakage_power_watts: Some(1e-9),
            nominal_voltage: Some(1.0),
        };
        library.power.cells = vec![PowerCell {
            name: "BUF".to_string(),
            cell_leakage_power: Some(leakage),
            leakage_power: Vec::new(),
            pins: Vec::new(),
        }]
        .into();
        library.cells = vec![TargetCell {
            name: "BUF".to_string(),
            area: Some(1.0),
            dont_use: false,
            usage: TargetCellUsage::default(),
            pins: vec![
                TargetPin {
                    name: "A".to_string(),
                    direction: TargetPinDirection::Input,
                    function: None,
                    three_state: None,
                    capacitance: None,
                    rise_capacitance: None,
                    fall_capacitance: None,
                    receiver_capacitance: None,
                    fanout_load: None,
                    next_state_type: None,
                    timing_arcs: Vec::new(),
                    clock_gate_role: None,
                },
                TargetPin {
                    name: "Y".to_string(),
                    direction: TargetPinDirection::Output,
                    function: None,
                    three_state: None,
                    capacitance: None,
                    rise_capacitance: None,
                    fall_capacitance: None,
                    receiver_capacitance: None,
                    fanout_load: None,
                    next_state_type: None,
                    timing_arcs: vec![TargetTimingArc {
                        related_pin: "A".to_string(),
                        timing_type: TargetTimingType::Combinational,
                        timing_sense: TimingSense::PositiveUnate,
                        delay_model: Some(ArcDelayModel::Nldm(NldmTimingModel::new(
                            Some(LookupTable::scalar(0.1)),
                            Some(LookupTable::scalar(0.1)),
                            None,
                            None,
                        ))),
                        rise_constraint: None,
                        fall_constraint: None,
                    }],
                    clock_gate_role: None,
                },
            ],
            sequential: Vec::new(),
            clock_gate: None,
            memory: None,
        }]
        .into();
        Arc::new(
            TimingModel::new(
                TimingDesign {
                    id: DesignId::from_uid(ObjectUid::from_raw(uid).unwrap()),
                    name: name.to_string(),
                    ports: Vec::new(),
                    instances: Vec::new(),
                },
                library,
            )
            .unwrap(),
        )
    }

    fn states(model: &Arc<TimingModel>) -> TimingElectricalSnapshot {
        TimingEngine::new(ExecutionContext::default())
            .electrical_snapshot(&TimingContext::default(), Arc::clone(model), DelayType::Max)
            .unwrap()
    }

    #[test]
    fn rejects_activity_from_another_timing_generation() {
        let model = test_model("top", 1);
        let other = test_model("other", 2);
        let annotations = ActivityAnnotations::new(other.generation(), []).unwrap();
        let runtime = runtime();

        assert!(matches!(
            PowerEngine::new().analyze(&runtime, Arc::clone(&model), states(&model), annotations),
            Err(PowerError::GenerationMismatch)
        ));
    }

    #[test]
    fn rejects_timing_states_from_the_same_topology_with_another_power_view() {
        let first = test_model_with_leakage("top", 1, 1.0);
        let second = test_model_with_leakage("top", 1, 2.0);
        assert_ne!(first.generation(), second.generation());
        let annotations = ActivityAnnotations::new(second.generation(), []).unwrap();

        assert!(matches!(
            PowerEngine::new().analyze(
                &runtime(),
                Arc::clone(&second),
                states(&first),
                annotations,
            ),
            Err(PowerError::GenerationMismatch)
        ));
    }

    #[test]
    fn changing_timing_generation_invalidates_the_power_cache() {
        let first = test_model("first", 1);
        let second = test_model("second", 2);
        let engine = PowerEngine::new();
        let runtime = runtime();
        let first_states = states(&first);
        let first_annotations = ActivityAnnotations::new(first.generation(), []).unwrap();
        let analysis = engine
            .analyze(
                &runtime,
                Arc::clone(&first),
                first_states.clone(),
                first_annotations.clone(),
            )
            .unwrap();
        assert_eq!(
            engine
                .analyze(
                    &runtime,
                    Arc::clone(&first),
                    first_states,
                    first_annotations,
                )
                .unwrap(),
            analysis
        );
        engine
            .analyze(
                &runtime,
                Arc::clone(&second),
                states(&second),
                ActivityAnnotations::new(second.generation(), []).unwrap(),
            )
            .unwrap();

        assert_eq!(engine.metrics().unwrap().full_updates, 2);
        assert_eq!(engine.metrics().unwrap().cache_hits, 1);
    }

    #[test]
    fn failed_incremental_update_preserves_cache_without_changing_metrics() {
        let model = test_model("top", 1);
        let other = test_model("other", 2);
        let timing_nets = states(&model);
        let annotations = ActivityAnnotations::new(model.generation(), []).unwrap();
        let runtime = runtime();
        let engine = PowerEngine::new();
        engine
            .analyze(
                &runtime,
                Arc::clone(&model),
                timing_nets.clone(),
                annotations.clone(),
            )
            .unwrap();

        assert!(matches!(
            engine.analyze(
                &runtime,
                Arc::clone(&model),
                timing_nets.clone(),
                ActivityAnnotations::new(other.generation(), []).unwrap(),
            ),
            Err(PowerError::GenerationMismatch)
        ));
        assert_eq!(engine.metrics().unwrap().full_updates, 1);

        engine
            .analyze(&runtime, model, timing_nets, annotations)
            .unwrap();
        assert_eq!(engine.metrics().unwrap().full_updates, 1);
        assert_eq!(engine.metrics().unwrap().incremental_updates, 0);
        assert_eq!(engine.metrics().unwrap().cache_hits, 1);
    }
}
