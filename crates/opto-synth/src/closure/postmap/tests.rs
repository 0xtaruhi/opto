// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Transactional post-map repair contracts.
//!
//! This suite owns candidate isolation, incremental timing/power evaluation,
//! atomic mapped edits, rollback, and deterministic closure ordering. Region
//! identity and initial technology cover remain owned by their domains.

use super::candidate::{CandidateDisposition, PostmapCandidate};
use super::candidates::sizing_regions;
use super::session::{CandidateEvaluation, ClosureBaseline, evaluate_candidate};
use super::sizing::SizingFrontier;
use super::*;
use crate::artifact::implementation::{InitialCellOwner, OriginSetId};
use crate::closure::mapped_timing::MappedTimingTransaction;
use crate::closure::objective::mapped_physical_objective;
use crate::{
    BooleanFunction, OptimizationPhase, SynthesisEffort, TargetCell, TargetPin, TargetPinDirection,
    TargetTimingArc, TargetTimingType,
};
use opto_ir::mapped::{CellId, RegionDelta};
use opto_ir::word::{LValue, PortDirection, SourceSpan, WordModule, WordType};
use opto_library::{LookupTable, TimingSense};
use opto_runtime::{ExecutionConfig, ExecutionContext};
use opto_timing::{TimingObject, TimingPortDirection};
use std::collections::BTreeMap;
use std::sync::Arc;

fn test_span() -> SourceSpan {
    SourceSpan::stable("test")
}

fn object_uid(raw: u64) -> opto_core::ObjectUid {
    opto_core::ObjectUid::from_raw(raw).unwrap()
}

fn assert_close(actual: f64, expected: f64) {
    let scale = actual.abs().max(expected.abs()).max(1.0);
    assert!(
        (actual - expected).abs() <= 1.0e-12 * scale,
        "expected {expected}, got {actual}"
    );
}

fn design_id() -> opto_timing::DesignId {
    opto_timing::DesignId::from_uid(object_uid(1))
}

fn port_id(raw: u64) -> opto_timing::PortId {
    opto_timing::PortId::from_uid(object_uid(raw))
}

fn port_bindings(mapped: &MappedNetlist) -> opto_timing::PortBindings {
    opto_timing::PortBindings::new(
        mapped
            .ports()
            .iter()
            .enumerate()
            .map(|(index, _)| port_id(index as u64 + 2)),
    )
}

fn fanout_load_profile(
    mapped: &MappedNetlist,
    options: &SynthesisOptions,
) -> MappedFanoutLoadProfile {
    MappedFanoutLoadProfile::build(mapped, &options.target_cells).unwrap()
}

fn scenario_set(library: &TimingLibrary) -> opto_timing::ScenarioSet {
    opto_timing::ScenarioSet::single(
        std::sync::Arc::new(TimingContext::default()),
        std::sync::Arc::new(library.clone()),
        opto_timing::Parasitics::default(),
    )
}

fn scenario_set_with_input_activity(library: &TimingLibrary) -> opto_timing::ScenarioSet {
    let mut power = library.power.clone();
    power.units = opto_library::PowerLibraryUnits {
        time_seconds: Some(1e-9),
        capacitance_farads: Some(1e-12),
        voltage_volts: Some(1.0),
        leakage_power_watts: Some(1e-9),
        nominal_voltage: Some(1.0),
    };
    let view = opto_timing::ScenarioPowerView::new(
        std::sync::Arc::new(power),
        vec![(
            opto_timing::ScenarioActivityTarget::Port(port_id(2)),
            opto_timing::ScenarioSwitchingActivity::new(0.5, 0.2, 0.5).unwrap(),
        )],
    )
    .unwrap();
    opto_timing::ScenarioSet::new(vec![
        opto_timing::Scenario::single(
            std::sync::Arc::new(TimingContext::default()),
            std::sync::Arc::new(library.clone()),
            opto_timing::Parasitics::default(),
        )
        .with_power(view),
    ])
    .unwrap()
}

fn runtime() -> ExecutionContext {
    ExecutionContext::new(&ExecutionConfig { max_threads: 1 }).unwrap()
}

#[derive(Debug)]
struct TestPowerEvaluator;

impl crate::SynthesisPowerEvaluator for TestPowerEvaluator {
    fn dynamic_power_watts(
        &self,
        _runtime: &ExecutionContext,
        scenario: &opto_timing::Scenario,
        _model: &TimingModel,
        electrical: &dyn Fn() -> Result<opto_timing::TimingElectricalSnapshot, String>,
    ) -> Result<Option<f64>, String> {
        if scenario.power().activities().is_empty() {
            return Ok(None);
        }
        electrical()?;
        Ok(Some(1.0))
    }
}

fn test_power_evaluator() -> std::sync::Arc<dyn crate::SynthesisPowerEvaluator> {
    std::sync::Arc::new(TestPowerEvaluator)
}

/// Standard wiring for a post-map optimization test.
struct PostmapRun<'a> {
    mapped: &'a mut MappedNetlist,
    implementations: &'a mut ImplementationDb,
    options: &'a SynthesisOptions,
    scenarios: ScenarioSet,
}

/// Runs post-map optimization with the default configuration and no observer.
fn run_postmap(run: PostmapRun<'_>) -> PostmapOutcome {
    run_postmap_observed(run, &mut |_| {})
}

fn run_postmap_observed(
    run: PostmapRun<'_>,
    observer: &mut dyn FnMut(SynthesisProgress),
) -> PostmapOutcome {
    let PostmapRun {
        mapped,
        implementations,
        options,
        scenarios,
    } = run;
    let port_bindings = port_bindings(mapped);
    let timing = MmmcTiming::new(
        mapped,
        design_id(),
        &port_bindings,
        &Arc::new(opto_timing::TimingObjectBindings::new()),
        &scenarios,
        &crate::ReferencePortMap::new(),
        crate::test_runtime(),
    )
    .unwrap()
    .unwrap();
    let catalog = PostmapCellCatalog::new(options);
    let profile = fanout_load_profile(mapped, options);
    let connectivity = crate::mapping::materialize::FrozenObservableConnectivity::capture(
        mapped,
        &options.target_cells,
        &crate::ReferencePortMap::new(),
    )
    .unwrap();
    optimize_mapped_netlist(
        PostmapRequest {
            mapped,
            implementations,
            timing: Some(timing),
            options,
            catalog: &catalog,
            scenarios: &scenarios,
            fanout_load_profile: &profile,
            policy: SynthesisEffort::High.policy(),
            runtime: crate::test_runtime(),
            power_evaluator: test_power_evaluator(),
            connectivity: &connectivity,
        },
        crate::SynthesisConfig::default(),
        observer,
    )
    .unwrap()
}

/// Builds a single-scenario set from a timing context and library.
fn single_scenario(timing: &TimingContext, library: TimingLibrary) -> ScenarioSet {
    ScenarioSet::single(
        std::sync::Arc::new(timing.clone()),
        std::sync::Arc::new(library),
        opto_timing::Parasitics::default(),
    )
}

#[test]
fn mmmc_power_uses_complete_explicit_input_activity() {
    let cells = fanout_cells();
    let options = SynthesisOptions {
        target_cells: cells.clone().into(),
    };
    let timing_library = TimingLibrary {
        cells: cells.into(),
        ..TimingLibrary::default()
    };
    let scenarios = scenario_set_with_input_activity(&timing_library);
    let (mapped, _) = mapped_design(&fanout_module(), &options);
    let runtime = runtime();
    let timing = MmmcTiming::new(
        &mapped,
        design_id(),
        &port_bindings(&mapped),
        &Arc::new(opto_timing::TimingObjectBindings::new()),
        &scenarios,
        &crate::ReferencePortMap::new(),
        &runtime,
    )
    .unwrap()
    .unwrap();
    let power = MmmcPower::new(&timing, &scenarios, &runtime, test_power_evaluator()).unwrap();
    assert!(
        power
            .committed()
            .dynamic_watts()
            .is_some_and(|power| power > 0.0)
    );
}

#[test]
fn mmmc_installs_internal_cell_exception_bindings() {
    let cells = vec![and_cell("AND", 1.0, [1.0, 1.0])];
    let options = SynthesisOptions {
        target_cells: cells.clone().into(),
    };
    let library = TimingLibrary {
        cells: cells.into(),
        ..TimingLibrary::default()
    };
    let (mapped, _) = mapped_design(&mapped_and_module("AND"), &options);
    let cell = opto_timing::CellId::from_uid(object_uid(100));
    let mut timing = TimingContext::new();
    timing
        .set_disable_timing(&[opto_timing::DisabledTiming {
            target: opto_timing::TimingEndpoint::Cell(cell),
            from: None,
            to: None,
        }])
        .unwrap();
    let scenarios = ScenarioSet::single(
        std::sync::Arc::new(timing),
        std::sync::Arc::new(library),
        opto_timing::Parasitics::default(),
    );
    let mut bindings = opto_timing::TimingObjectBindings::builder();
    bindings.bind_cell("U0", cell).unwrap();
    let bindings = bindings.finish().unwrap();
    let runtime = runtime();
    let mut analysis = MmmcTiming::new(
        &mapped,
        design_id(),
        &port_bindings(&mapped),
        &Arc::new(bindings),
        &scenarios,
        &crate::ReferencePortMap::new(),
        &runtime,
    )
    .unwrap()
    .unwrap();

    let metrics = analysis.metrics().unwrap();

    assert_eq!(
        metrics.analysis.violating_paths(),
        0,
        "disabled internal cell must remove every timing path"
    );
    assert_eq!(metrics.analysis.wns(), None);
    assert_close(metrics.analysis.arrival(), 0.0);
}

fn named_port_id(mapped: &MappedNetlist, name: &str) -> opto_timing::PortId {
    let index = mapped
        .ports()
        .iter()
        .enumerate()
        .find(|(index, _)| {
            let id = opto_ir::mapped::PortId::from_index(*index).unwrap();
            mapped.port_name(id) == Some(name)
        })
        .map(|(index, _)| index)
        .expect("test port exists");
    port_id(index as u64 + 2)
}

fn mapped_design(
    module: &WordModule,
    options: &SynthesisOptions,
) -> (MappedNetlist, ImplementationDb) {
    let references = crate::target_cell_reference_ports(&options.target_cells);
    let source_instances = crate::artifact::provenance::SourceInstanceProvenance::capture(module);
    let output = crate::mapping::build_test_substrate(
        module,
        options,
        &std::collections::BTreeSet::new(),
        &references,
        &source_instances,
        opto_ir::RevisionId::INITIAL,
    )
    .unwrap();
    let implementations = ImplementationDb::empty(output.netlist.cell_slot_count());
    (output.netlist, implementations)
}

fn design_object(_name: &str) -> TimingObject {
    TimingObject::design(design_id())
}

#[test]
fn sizing_frontier_expands_in_deterministic_order() {
    assert_eq!(
        SizingFrontier::WorstPath.next(),
        Some(SizingFrontier::AllViolations)
    );
    assert_eq!(SizingFrontier::AllViolations.next(), None);
}

#[test]
fn sizing_uses_incremental_sta_to_fix_load_dependent_slack() {
    let cells = vec![
        and_cell("AND_SMALL", 1.0, [1.0, 3.0]),
        and_cell("AND_FAST", 3.0, [0.8, 1.0]),
    ];
    let options = SynthesisOptions {
        target_cells: cells.clone().into(),
    };
    let timing_library = TimingLibrary {
        cells: cells.into(),
        ..TimingLibrary::default()
    };
    let module = mapped_and_module("AND_SMALL");
    let (mut mapped, mut implementations) = mapped_design(&module, &options);
    let mut timing = TimingContext::new();
    timing
        .set_max_delay(
            1.5,
            Vec::new(),
            vec![opto_timing::TimingEndpoint::Port(named_port_id(
                &mapped, "y",
            ))],
        )
        .unwrap();
    timing
        .set_load(10.0, &[named_port_id(&mapped, "y")])
        .unwrap();

    let outcome = run_postmap(PostmapRun {
        mapped: &mut mapped,
        implementations: &mut implementations,
        options: &options,
        scenarios: single_scenario(&timing, timing_library),
    });

    assert_eq!(outcome.replacements, 1);
    let mut timing = outcome.timing.unwrap();
    assert!(timing.summary().unwrap().slack.unwrap() >= 0.0);
    assert_eq!(
        mapped.cell_type(opto_ir::mapped::CellId::from_index(0).unwrap()),
        Some("AND_FAST")
    );
}

#[test]
fn sizing_recovers_area_without_breaking_met_timing() {
    let cells = vec![
        and_cell("AND_SMALL", 1.0, [1.0, 3.0]),
        and_cell("AND_FAST", 3.0, [0.8, 1.0]),
    ];
    let options = SynthesisOptions {
        target_cells: cells.clone().into(),
    };
    let timing_library = TimingLibrary {
        cells: cells.into(),
        ..TimingLibrary::default()
    };
    let module = mapped_and_module("AND_FAST");
    let (mut mapped, mut implementations) = mapped_design(&module, &options);
    let mut timing = TimingContext::new();
    timing
        .set_max_delay(
            3.5,
            Vec::new(),
            vec![opto_timing::TimingEndpoint::Port(named_port_id(
                &mapped, "y",
            ))],
        )
        .unwrap();
    timing
        .set_load(10.0, &[named_port_id(&mapped, "y")])
        .unwrap();

    let outcome = run_postmap(PostmapRun {
        mapped: &mut mapped,
        implementations: &mut implementations,
        options: &options,
        scenarios: single_scenario(&timing, timing_library),
    });

    assert_eq!(outcome.replacements, 1);
    let mut timing = outcome.timing.unwrap();
    assert!(timing.summary().unwrap().slack.unwrap() >= 0.0);
    assert_eq!(
        mapped.cell_type(opto_ir::mapped::CellId::from_index(0).unwrap()),
        Some("AND_SMALL")
    );
}

#[test]
fn timing_preparation_and_area_recovery_share_one_postmap_flow() {
    let cells = vec![
        and_cell("AND_SMALL", 1.0, [1.0, 3.0]),
        and_cell("AND_FAST", 3.0, [0.8, 1.0]),
    ];
    let options = SynthesisOptions {
        target_cells: cells.clone().into(),
    };
    let timing_library = TimingLibrary {
        cells: cells.into(),
        ..TimingLibrary::default()
    };
    let module = mapped_and_module("AND_FAST");
    let (mut mapped, mut implementations) = mapped_design(&module, &options);
    let timing = TimingContext::new();

    let outcome = run_postmap(PostmapRun {
        mapped: &mut mapped,
        implementations: &mut implementations,
        options: &options,
        scenarios: single_scenario(&timing, timing_library),
    });

    assert_eq!(outcome.replacements, 1);
    assert!(outcome.timing.is_some());
    assert_eq!(
        mapped.cell_type(opto_ir::mapped::CellId::from_index(0).unwrap()),
        Some("AND_SMALL")
    );
}

#[test]
fn cleanup_sizing_commits_independent_gates_as_one_forest() {
    let cells = vec![
        and_cell("AND_SMALL", 1.0, [1.0, 3.0]),
        and_cell("AND_FAST", 3.0, [0.8, 1.0]),
    ];
    let options = SynthesisOptions {
        target_cells: cells.clone().into(),
    };
    let timing_library = TimingLibrary {
        cells: cells.into(),
        ..TimingLibrary::default()
    };
    let module = parallel_and_module("AND_FAST", 3);
    let (mut mapped, mut implementations) = mapped_design(&module, &options);
    let mut timing = TimingContext::new();
    timing
        .set_max_delay(
            0.5,
            Vec::new(),
            vec![opto_timing::TimingEndpoint::Port(named_port_id(
                &mapped, "y2",
            ))],
        )
        .unwrap();
    let mut cleanup_updates = 0usize;
    let mut cleanup_updates_with_timing = 0usize;
    let mut cleanup_started = false;

    let outcome = run_postmap_observed(
        PostmapRun {
            mapped: &mut mapped,
            implementations: &mut implementations,
            options: &options,
            scenarios: single_scenario(&timing, timing_library),
        },
        &mut |progress| {
            if let SynthesisProgress::Candidate { phase, timing, .. } = progress {
                match phase {
                    OptimizationPhase::RegisterOptimization => cleanup_started = true,
                    OptimizationPhase::TradeoffSizing if cleanup_started => {
                        cleanup_updates += 1;
                        cleanup_updates_with_timing += usize::from(timing.is_some());
                    }
                    _ => {}
                }
            }
        },
    );

    assert_eq!(outcome.replacements, 1);
    assert_eq!(cleanup_updates, 3);
    assert_eq!(cleanup_updates_with_timing, cleanup_updates);
    assert_eq!(
        mapped.cell_type(mapped_cell_by_name(&mapped, "U0")),
        Some("AND_SMALL")
    );
    assert_eq!(
        mapped.cell_type(mapped_cell_by_name(&mapped, "U1")),
        Some("AND_SMALL")
    );
    assert_eq!(
        mapped.cell_type(mapped_cell_by_name(&mapped, "U2")),
        Some("AND_FAST")
    );
}

#[test]
fn mapped_resynthesis_seeds_region_owned_cells_in_a_clean_netlist() {
    let cells = fanout_cells();
    let options = SynthesisOptions {
        target_cells: cells.clone().into(),
    };
    let timing_library = TimingLibrary {
        cells: cells.into(),
        ..TimingLibrary::default()
    };
    let (mut mapped, _) = mapped_design(&fanout_module(), &options);
    let owner = InitialCellOwner::Region(crate::RegionAnchorId::from_bytes_for_test([1; 32]));
    let mut implementations = ImplementationDb::new(
        mapped.generation_id(),
        Vec::new().into_boxed_slice(),
        vec![OriginSetId::EMPTY; mapped.cell_slot_count()],
        vec![0, 0],
        Vec::new(),
        vec![Some(owner); mapped.cell_slot_count()],
    )
    .unwrap();

    let outcome = run_postmap(PostmapRun {
        mapped: &mut mapped,
        implementations: &mut implementations,
        options: &options,
        scenarios: single_scenario(&TimingContext::new(), timing_library),
    });

    assert_eq!(outcome.replacements, 1);
    assert_eq!(mapped.cell_count(), 3);
    assert!(
        mapped
            .cell_ids()
            .all(|cell| mapped.cell_name(cell) != Some("U0"))
    );
}

#[test]
fn buffering_partitions_fanout_without_moving_the_violation() {
    let mut cells = vec![
        unary_cell("DRV", "A", "Y", 10.0, 0.0, [0.1, 0.1]),
        unary_cell("SINK", "I", "O", 10.0, 1.0, [0.1, 0.1]),
        unary_cell("ISO", "B", "Z", 1.0, 1.0, [0.05, 0.05]),
    ];
    cells[1].pins[0].fanout_load = Some(1.5);
    cells[2].pins[0].fanout_load = Some(1.0);
    let options = SynthesisOptions {
        target_cells: cells.clone().into(),
    };
    let timing_library = TimingLibrary {
        cells: cells.clone().into(),
        ..TimingLibrary::default()
    };
    let module = fanout_module();
    let (mut mapped, mut implementations) = mapped_design(&module, &options);
    let mut timing = TimingContext::new();
    timing.set_max_fanout(2.5, &[design_object("top")]).unwrap();

    let outcome = run_postmap(PostmapRun {
        mapped: &mut mapped,
        implementations: &mut implementations,
        options: &options,
        scenarios: single_scenario(&timing, timing_library),
    });

    assert_eq!(outcome.replacements, 3);
    assert_eq!(mapped.cell_count(), 5);
    let model = TimingModel::from_mapped(
        &mapped,
        design_id(),
        &port_bindings(&mapped),
        TimingLibrary {
            cells: cells.into(),
            ..TimingLibrary::default()
        },
    )
    .unwrap();
    let incremental =
        IncrementalTiming::new(timing, model, ReportTimingOptions::default()).unwrap();
    assert!(incremental.design_rule_violations().is_empty());
}

fn fanout_cells() -> Vec<TargetCell> {
    let mut cells = vec![
        unary_cell("DRV", "A", "Y", 10.0, 0.0, [0.1, 0.1]),
        unary_cell("SINK", "I", "O", 10.0, 1.0, [0.1, 0.1]),
    ];
    cells[1].pins[0].fanout_load = Some(1.0);
    cells
}

fn mapped_net_by_name(mapped: &MappedNetlist, name: &str) -> opto_ir::mapped::NetId {
    mapped
        .net_ids()
        .find(|&net| mapped.net_name(net) == Some(name))
        .unwrap()
}

fn mapped_cell_by_name(mapped: &MappedNetlist, name: &str) -> CellId {
    mapped
        .cell_ids()
        .find(|&cell| mapped.cell_name(cell) == Some(name))
        .unwrap()
}

#[test]
fn electrical_legalization_buffers_before_residual_cloning() {
    let cells = fanout_cells();
    let options = SynthesisOptions {
        target_cells: cells.clone().into(),
    };
    let timing_library = TimingLibrary {
        cells: cells.clone().into(),
        ..TimingLibrary::default()
    };
    let module = fanout_module();
    let (mut mapped, mut implementations) = mapped_design(&module, &options);
    let mut timing = TimingContext::new();
    timing.set_max_fanout(2.5, &[design_object("top")]).unwrap();

    let outcome = run_postmap(PostmapRun {
        mapped: &mut mapped,
        implementations: &mut implementations,
        options: &options,
        scenarios: single_scenario(&timing, timing_library),
    });

    assert_eq!(outcome.replacements, 2);
    assert_eq!(mapped.cell_count(), 4);
    let buffer = mapped
        .cell_ids()
        .find(|&cell| {
            mapped
                .cell_name(cell)
                .is_some_and(|name| name.starts_with("U_electrical_buffer_"))
        })
        .unwrap();
    assert_eq!(mapped.cell_type(buffer), Some("DRV"));
    assert!(
        mapped
            .cell_ids()
            .all(|cell| !mapped.cell_name(cell).unwrap().starts_with("U_clone"))
    );
    let model = TimingModel::from_mapped(
        &mapped,
        design_id(),
        &port_bindings(&mapped),
        TimingLibrary {
            cells: cells.into(),
            ..TimingLibrary::default()
        },
    )
    .unwrap();
    let incremental =
        IncrementalTiming::new(timing, model, ReportTimingOptions::default()).unwrap();
    assert!(incremental.design_rule_violations().is_empty());
}

#[test]
fn small_critical_fanout_defers_to_driver_cloning() {
    let mut cells = fanout_cells();
    cells.push(unary_cell("BUF", "A", "Y", 1.0, 0.1, [0.1, 1.0]));
    let options = SynthesisOptions {
        target_cells: cells.clone().into(),
    };
    let timing_library = TimingLibrary {
        cells: cells.into(),
        // This fixture requires a shared resistive trunk to make cloning beneficial.
        wire_load_tree: opto_library::WireLoadTree::WorstCase,
        wire_load_model: Some(
            opto_library::WireLoadModel::new(
                "test".to_string(),
                1.0,
                1.0,
                1.0,
                vec![(1.0, 1.0), (2.0, 2.0)],
            )
            .unwrap(),
        ),
        ..TimingLibrary::default()
    };
    let (mut mapped, mut implementations) = mapped_design(&fanout_module(), &options);
    let mut timing = TimingContext::new();
    timing
        .set_max_delay(
            1.0,
            Vec::new(),
            vec![opto_timing::TimingEndpoint::Port(named_port_id(
                &mapped, "y0",
            ))],
        )
        .unwrap();
    let scenarios = ScenarioSet::single(
        std::sync::Arc::new(timing),
        std::sync::Arc::new(timing_library),
        opto_timing::Parasitics::default(),
    );
    let mut phases = Vec::new();

    let outcome = run_postmap_observed(
        PostmapRun {
            mapped: &mut mapped,
            implementations: &mut implementations,
            options: &options,
            scenarios,
        },
        &mut |event| {
            if let SynthesisProgress::Candidate { phase, .. } = event {
                phases.push(phase);
            }
        },
    );

    assert!(outcome.replacements >= 1);
    assert_eq!(
        phases.first(),
        Some(&OptimizationPhase::CriticalFanoutCloning)
    );
    assert!(
        mapped
            .cell_ids()
            .any(|cell| mapped.cell_name(cell).unwrap().starts_with("U_clone"))
    );
    assert!(
        mapped
            .cell_ids()
            .all(|cell| !mapped.cell_name(cell).unwrap().starts_with("U_buffer_tree"))
    );
}

#[test]
fn fanout_tree_search_never_crosses_the_characterized_load_domain() {
    let mut buffer = unary_cell("BUF", "A", "Y", 1.0, 0.3, [0.0, 0.0]);
    let load_limited_table = || LookupTable::new(Vec::new(), vec![0.0, 0.5], vec![0.0, 0.0]);
    buffer.pins[1].timing_arcs[0].delay_model = Some(opto_library::ArcDelayModel::Nldm(
        opto_library::NldmTimingModel::new(
            Some(load_limited_table()),
            Some(load_limited_table()),
            None,
            None,
        ),
    ));
    let mut cells = fanout_cells();
    cells[1].pins[0].capacitance = Some(64.0);
    cells.push(buffer);
    let options = SynthesisOptions {
        target_cells: cells.clone().into(),
    };
    let timing_library = TimingLibrary {
        cells: cells.into(),
        wire_load_model: Some(
            opto_library::WireLoadModel::new(
                "test".to_string(),
                1.0,
                0.0,
                0.0,
                vec![(1.0, 1.0), (64.0, 64.0)],
            )
            .unwrap(),
        ),
        ..TimingLibrary::default()
    };
    let scenarios = scenario_set(&timing_library);
    let (mapped, _) = mapped_design(&fanout_module(), &options);
    let shared = mapped_net_by_name(&mapped, "n1");
    let model = TimingModel::from_mapped(
        &mapped,
        design_id(),
        &port_bindings(&mapped),
        timing_library,
    )
    .unwrap();
    let incremental =
        IncrementalTiming::new(TimingContext::new(), model, ReportTimingOptions::default())
            .unwrap();
    let state = incremental.mapped_net_state(shared).unwrap();
    let net_states = scenarios
        .analysis_views()
        .map(|(view, _, _)| crate::closure::mmmc::MmmcNetState {
            view,
            state: Some(state.clone()),
        })
        .collect::<Vec<_>>();

    let strategy = buffering::select_fanout_tree_strategy(
        &mapped,
        &options.target_cells,
        &scenarios,
        &[2],
        &buffering::FanoutSinks::new(
            buffering::net_sink_pins(&mapped, &options.target_cells, shared)
                .unwrap()
                .into_iter()
                .map(|(pin, _)| pin)
                .collect(),
            &[],
        ),
        &net_states,
    )
    .unwrap();

    assert!(strategy.is_none());
}

#[test]
fn independent_small_critical_fanouts_commit_as_one_clone_forest_transaction() {
    let mut cells = fanout_cells();
    cells.push(unary_cell("BUF", "A", "Y", 1.0, 0.1, [0.1, 1.0]));
    let options = SynthesisOptions {
        target_cells: cells.clone().into(),
    };
    let timing_library = TimingLibrary {
        cells: cells.into(),
        // This fixture requires a shared resistive trunk to make cloning beneficial.
        wire_load_tree: opto_library::WireLoadTree::WorstCase,
        wire_load_model: Some(
            opto_library::WireLoadModel::new(
                "test".to_string(),
                1.0,
                1.0,
                1.0,
                vec![(1.0, 1.0), (2.0, 2.0)],
            )
            .unwrap(),
        ),
        ..TimingLibrary::default()
    };
    let (mut mapped, mut implementations) = mapped_design(&two_fanout_module(), &options);
    let mut timing = TimingContext::new();
    timing
        .set_max_delay(
            1.0,
            Vec::new(),
            vec![
                opto_timing::TimingEndpoint::Port(named_port_id(&mapped, "y0_0")),
                opto_timing::TimingEndpoint::Port(named_port_id(&mapped, "y1_0")),
            ],
        )
        .unwrap();
    let scenarios = ScenarioSet::single(
        std::sync::Arc::new(timing),
        std::sync::Arc::new(timing_library),
        opto_timing::Parasitics::default(),
    );
    let mut phases = Vec::new();

    run_postmap_observed(
        PostmapRun {
            mapped: &mut mapped,
            implementations: &mut implementations,
            options: &options,
            scenarios,
        },
        &mut |event| {
            if let SynthesisProgress::Candidate { phase, .. } = event {
                phases.push(phase);
            }
        },
    );

    assert_eq!(
        phases
            .iter()
            .filter(|&&phase| phase == OptimizationPhase::CriticalFanoutCloning)
            .count(),
        1
    );
    for ordinal in 0..2 {
        assert!(mapped.cell_ids().any(|cell| {
            mapped
                .cell_name(cell)
                .unwrap()
                .starts_with(&format!("U_clone_0_{ordinal}"))
        }));
    }
}

#[test]
fn critical_fanout_analysis_is_deterministic_and_retains_the_complete_net() {
    let cells = fanout_cells();
    let options = SynthesisOptions {
        target_cells: cells.into(),
    };
    let module = fanout_module();
    let (mapped, _) = mapped_design(&module, &options);
    let critical = mapped
        .cell_ids()
        .filter(|&cell| mapped.cell_type(cell) == Some("SINK"))
        .collect::<Vec<_>>();
    let shared = mapped_net_by_name(&mapped, "n1");

    let first =
        buffering::critical_fanouts(&mapped, &options.target_cells, critical.clone(), [shared])
            .unwrap();
    let second =
        buffering::critical_fanouts(&mapped, &options.target_cells, critical, [shared]).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].clone_branch.len(), 2);
    assert_eq!(first[0].sinks.len(), 3);
}

#[test]
fn fanout_load_profile_is_complete_and_generation_stamped() {
    let cells = fanout_cells();
    let options = SynthesisOptions {
        target_cells: cells.into(),
    };
    let (mut mapped, _) = mapped_design(&fanout_module(), &options);
    let shared = mapped_net_by_name(&mapped, "n1");
    let profile = fanout_load_profile(&mapped, &options);
    let row = profile.row(shared).unwrap();

    assert_eq!(row.sinks(), 3);
    assert_close(row.fanout_load(), 3.0);
    assert_close(row.pin_capacitance(), 3.0);
    profile.validate(&mapped).unwrap();

    let driver = mapped_cell_by_name(&mapped, "U0");
    let mut edit = RegionDelta::new(mapped.snapshot_region([driver], []).unwrap());
    edit.rename_cell(driver, "U0_renamed").unwrap();
    mapped.apply_region_delta(edit).unwrap();
    assert!(profile.validate(&mapped).is_err());
}

#[test]
fn fanout_tree_reserves_the_critical_branch_before_planning() {
    let pins = (0..6)
        .map(|index| opto_ir::mapped::PinId::from_index(index).unwrap())
        .collect::<Vec<_>>();
    let sinks = buffering::FanoutSinks::new(pins.clone(), &[pins[3], pins[0]]);
    assert_eq!(sinks.buffered, vec![pins[1], pins[2], pins[4], pins[5]]);
    assert_eq!(sinks.direct, vec![pins[0], pins[3]]);
    // Four buffered sinks require two leaves; removing protected pins from a
    // preselected three-leaf tree would disagree with its priced buffer count.
    assert_eq!(
        buffering::fanout_tree_buffer_count(
            sinks.buffered.len(),
            buffering::FanoutTreeStrategy {
                buffer_index: 0,
                branching_factor: 2
            }
        )
        .unwrap(),
        2
    );
}

#[test]
fn fanout_tree_defers_a_small_critical_net_to_driver_cloning() {
    let options = SynthesisOptions {
        target_cells: fanout_cells().into(),
    };
    let (mapped, _) = mapped_design(&fanout_module(), &options);
    let shared = mapped_net_by_name(&mapped, "n1");
    let pins = buffering::net_sink_pins(&mapped, &options.target_cells, shared)
        .unwrap()
        .into_iter()
        .map(|(pin, _)| pin)
        .collect::<Vec<_>>();
    let sinks = buffering::FanoutSinks::new(pins.clone(), &pins[..1]);
    assert!(
        buffering::select_fanout_tree_strategy(
            &mapped,
            &options.target_cells,
            &scenario_set(&TimingLibrary::default()),
            &[],
            &sinks,
            &[],
        )
        .unwrap()
        .is_none()
    );
}

#[test]
fn critical_fanout_does_not_widen_the_sta_net_frontier() {
    let cells = fanout_cells();
    let options = SynthesisOptions {
        target_cells: cells.into(),
    };
    let (mapped, _) = mapped_design(&fanout_module(), &options);
    let critical = mapped
        .cell_ids()
        .filter(|&cell| mapped.cell_type(cell) == Some("SINK"))
        .collect::<Vec<_>>();
    let unrelated = mapped_net_by_name(&mapped, "a");

    let fanouts =
        buffering::critical_fanouts(&mapped, &options.target_cells, critical, [unrelated]).unwrap();

    assert!(fanouts.is_empty());
}

#[test]
fn fanout_tree_selection_respects_wire_topology_and_units() {
    use opto_library::{TimingLibraryUnits, WireLoadTree};
    let femtofarad_units = TimingLibraryUnits {
        time_seconds: Some(1e-9),
        capacitance_farads: Some(1e-15),
        resistance_ohms: Some(1e3),
    };
    for (tree, units, beneficial) in [
        (WireLoadTree::Balanced, TimingLibraryUnits::default(), false),
        (WireLoadTree::WorstCase, TimingLibraryUnits::default(), true),
        (WireLoadTree::WorstCase, femtofarad_units, false),
    ] {
        let mut cells = fanout_cells();
        cells.push(unary_cell("BUF", "A", "Y", 1.0, 0.1, [0.1, 1.0]));
        let options = SynthesisOptions {
            target_cells: cells.clone().into(),
        };
        let library = TimingLibrary {
            cells: cells.into(),
            wire_load_tree: tree,
            wire_load_model: Some(
                opto_library::WireLoadModel::new("test".into(), 1.0, 1.0, 1.0, Vec::new()).unwrap(),
            ),
            units,
            ..TimingLibrary::default()
        };
        let scenarios = scenario_set(&library);
        let (mapped, _) = mapped_design(&fanout_module(), &options);
        let net = mapped_net_by_name(&mapped, "n1");
        let model =
            TimingModel::from_mapped(&mapped, design_id(), &port_bindings(&mapped), library)
                .unwrap();
        let timing =
            IncrementalTiming::new(TimingContext::new(), model, ReportTimingOptions::default())
                .unwrap();
        let state = timing.mapped_net_state(net).unwrap();
        let net_states = scenarios
            .analysis_views()
            .map(|(view, _, delay_type)| {
                let mut state = state.clone();
                // Exercise setup-driven selection without inventing an early constraint.
                state.slack = (delay_type == opto_timing::DelayType::Max).then_some(-1.0);
                crate::closure::mmmc::MmmcNetState {
                    view,
                    state: Some(state),
                }
            })
            .collect::<Vec<_>>();
        let sinks = buffering::net_sink_pins(&mapped, &options.target_cells, net)
            .unwrap()
            .into_iter()
            .map(|(pin, _)| pin)
            .collect::<Vec<_>>();
        let selection = buffering::select_fanout_tree_strategy(
            &mapped,
            &options.target_cells,
            &scenarios,
            &[2],
            &buffering::FanoutSinks::new(sinks, &[]),
            &net_states,
        )
        .unwrap();
        // A linear balanced model already has independent equal-R branches;
        // extra stages cannot reduce its wire delay. Small RC also cannot pay
        // for a buffer's characterized cell delay, even with a shared trunk.
        assert_eq!(selection.is_some(), beneficial, "{tree:?} {units:?}");
    }
}

#[test]
fn fanout_forest_is_one_atomic_balanced_edit() {
    for (sink_count, protected_count) in [(3, 0), (6, 2)] {
        let mut cells = fanout_cells();
        cells.push(unary_cell("BUF", "A", "Y", 1.0, 0.1, [0.1, 1.0]));
        let options = SynthesisOptions {
            target_cells: cells.clone().into(),
        };
        let timing_library = TimingLibrary {
            cells: cells.into(),
            wire_load_tree: opto_library::WireLoadTree::WorstCase,
            wire_load_model: Some(
                opto_library::WireLoadModel::new(
                    "test".to_string(),
                    1.0,
                    1.0,
                    1.0,
                    vec![(1.0, 1.0), (2.0, 2.0)],
                )
                .unwrap(),
            ),
            ..TimingLibrary::default()
        };
        let (mut mapped, _) = mapped_design(&fanout_module_with_sinks(sink_count), &options);
        let shared = mapped_net_by_name(&mapped, "n1");
        let mut timing = TimingContext::new();
        timing
            .set_max_delay(
                1.0,
                Vec::new(),
                vec![opto_timing::TimingEndpoint::Port(named_port_id(
                    &mapped, "y0",
                ))],
            )
            .unwrap();
        let scenarios = ScenarioSet::single(
            Arc::new(timing.clone()),
            Arc::new(timing_library.clone()),
            opto_timing::Parasitics::default(),
        );
        let model = TimingModel::from_mapped(
            &mapped,
            design_id(),
            &port_bindings(&mapped),
            timing_library,
        )
        .unwrap();
        let incremental =
            IncrementalTiming::new(timing, model, ReportTimingOptions::default()).unwrap();
        let net_state = incremental.mapped_net_state(shared).unwrap();
        let net_states = scenarios
            .analysis_views()
            .map(|(view, _, _)| crate::closure::mmmc::MmmcNetState {
                view,
                state: Some(net_state.clone()),
            })
            .collect::<Vec<_>>();
        let sinks = buffering::net_sink_pins(&mapped, &options.target_cells, shared)
            .unwrap()
            .into_iter()
            .map(|(pin, _)| pin)
            .collect::<Vec<_>>();
        let selection = buffering::select_fanout_tree_strategy(
            &mapped,
            &options.target_cells,
            &scenarios,
            &[2],
            &buffering::FanoutSinks::new(sinks.clone(), &sinks[..protected_count]),
            &net_states,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            selection.leaf_groups.iter().map(Vec::len).sum::<usize>(),
            sink_count - protected_count
        );
        let planned_buffers =
            buffering::fanout_tree_buffer_count(sink_count - protected_count, selection.strategy)
                .unwrap();
        let implementations = crate::ImplementationDb::empty(mapped.cell_slot_count());
        let candidate = buffering::fanout_forest_delta(
            &mapped,
            &implementations,
            &options.target_cells,
            &[buffering::FanoutTreePlan {
                net: shared,
                leaf_groups: selection.leaf_groups,
                strategy: selection.strategy,
                namespace: 0,
                ordinal: 0,
            }],
        )
        .unwrap()
        .unwrap();
        let applied = mapped.apply_region_delta(candidate.delta).unwrap();

        assert_eq!(applied.added_cells().count(), planned_buffers);
        assert_eq!(
            buffering::net_sink_pins(&mapped, &options.target_cells, shared)
                .unwrap()
                .len(),
            2 + protected_count
        );
        for (index, &sink) in sinks.iter().enumerate() {
            assert_eq!(
                mapped.connection(sink).unwrap().signal
                    == opto_ir::mapped::ConnectionSignal::Net(shared),
                index < protected_count,
            );
        }
    }
}

#[test]
fn clone_candidates_are_deterministic() {
    let cells = fanout_cells();
    let options = SynthesisOptions {
        target_cells: cells.into(),
    };
    let (mapped, _) = mapped_design(&fanout_module(), &options);
    let net = mapped_net_by_name(&mapped, "n1");
    let sinks = buffering::net_sink_pins(&mapped, &options.target_cells, net).unwrap();
    let branch = vec![sinks[0].0];

    let first = cloning::clone_driver_delta(
        &mapped,
        &options.target_cells,
        net,
        &branch,
        "U_clone_0",
        "_clone_net_0",
    )
    .unwrap();
    let second = cloning::clone_driver_delta(
        &mapped,
        &options.target_cells,
        net,
        &branch,
        "U_clone_0",
        "_clone_net_0",
    )
    .unwrap();

    assert!(first.is_some());
    assert_eq!(first, second);
}

#[test]
fn clone_history_blocks_committed_sources_and_clone_products_only() {
    let cells = fanout_cells();
    let options = SynthesisOptions {
        target_cells: cells.into(),
    };
    let (mut mapped, _) = mapped_design(&fanout_module(), &options);
    let net = mapped_net_by_name(&mapped, "n1");
    let source = mapped
        .pins_on_net(net)
        .unwrap()
        .find_map(|pin| {
            let owner = mapped.pin_owner(pin)?;
            (mapped.cell_type(owner) == Some("DRV")).then_some(owner)
        })
        .unwrap();
    let unrelated = mapped.cell_ids().find(|&cell| cell != source).unwrap();
    let branch = buffering::net_sink_pins(&mapped, &options.target_cells, net)
        .unwrap()
        .into_iter()
        .take(2)
        .map(|(pin, _)| pin)
        .collect::<Vec<_>>();
    let clone_addition_start = mapped.cell_slot_count();
    let candidate = cloning::clone_driver_delta(
        &mapped,
        &options.target_cells,
        net,
        &branch,
        "U_clone_test",
        "_clone_net_test",
    )
    .unwrap()
    .unwrap();
    let applied = mapped.apply_region_delta(candidate.delta).unwrap();
    let clone = applied.added_cells().next().unwrap().1;
    let mut history = std::collections::BTreeSet::new();

    cloning::record_clone_history(&mapped, clone_addition_start, [source], &mut history);

    assert!(history.contains(&source));
    assert!(history.contains(&clone));
    assert!(!history.contains(&unrelated));
}

#[test]
fn residual_clones_commit_as_one_atomic_forest() {
    let cells = fanout_cells();
    let options = SynthesisOptions {
        target_cells: cells.into(),
    };
    let (mut mapped, _) = mapped_design(&two_fanout_module(), &options);
    let mut moved = Vec::new();
    let plans = (0..2)
        .map(|ordinal| {
            let net = mapped_net_by_name(&mapped, &format!("trunk{ordinal}"));
            let branch = buffering::net_sink_pins(&mapped, &options.target_cells, net)
                .unwrap()
                .into_iter()
                .take(1)
                .map(|(pin, _)| pin)
                .collect::<Vec<_>>();
            moved.push((net, branch[0]));
            cloning::CloneBranchPlan {
                net,
                branch,
                instance_name: format!("U_clone_{ordinal}"),
                net_name: format!("_clone_net_{ordinal}"),
            }
        })
        .collect::<Vec<_>>();

    let implementations = crate::ImplementationDb::empty(mapped.cell_slot_count());
    let candidate = cloning::clone_driver_forest_delta(
        &mapped,
        &implementations,
        &options.target_cells,
        &plans,
    )
    .unwrap()
    .unwrap();
    let applied = mapped.apply_region_delta(candidate.delta).unwrap();

    assert_eq!(applied.added_cells().count(), 2);
    for (net, pin) in moved {
        assert_ne!(
            mapped.connection(pin).unwrap().signal,
            opto_ir::mapped::ConnectionSignal::Net(net)
        );
    }
}

#[test]
fn sizing_forest_replaces_multiple_cells_atomically() {
    let mut cells = fanout_cells();
    cells.push(unary_cell("DRV_FAST", "A", "Y", 12.0, 0.0, [0.05, 0.05]));
    let options = SynthesisOptions {
        target_cells: cells.into(),
    };
    let (mut mapped, _) = mapped_design(&two_fanout_module(), &options);
    let choices = (0..2)
        .map(|tree| (mapped_cell_by_name(&mapped, &format!("U{tree}_driver")), 2))
        .collect::<Vec<_>>();

    let candidate = sizing::sizing_forest_delta(&mapped, &options.target_cells, &choices).unwrap();
    let applied = mapped.apply_region_delta(candidate.delta).unwrap();

    assert_eq!(applied.affected_cells().count(), 2);
    for (cell, _) in choices {
        assert_eq!(mapped.cell_type(cell), Some("DRV_FAST"));
    }
}

#[test]
fn rejected_clone_rolls_back_mapped_and_timing_state() {
    let cells = fanout_cells();
    let options = SynthesisOptions {
        target_cells: cells.clone().into(),
    };
    let timing_library = TimingLibrary {
        cells: cells.into(),
        ..TimingLibrary::default()
    };
    let scenarios = scenario_set(&timing_library);
    let (mut mapped, mut implementations) = mapped_design(&fanout_module(), &options);
    let mapped_port_ids = port_bindings(&mapped);
    let model =
        TimingModel::from_mapped(&mapped, design_id(), &mapped_port_ids, timing_library).unwrap();
    let mut incremental = MmmcTiming::from_owner_for_test(
        IncrementalTiming::new(TimingContext::new(), model, ReportTimingOptions::default())
            .unwrap(),
        MmmcViewPolicy {
            timing: true,
            checks: opto_timing::ScenarioCheckSet::ALL,
        },
    );
    let metrics = incremental.metrics().unwrap();
    let analysis = metrics.analysis;
    assert!(metrics.design_rules.is_empty());
    let design_rule_summary = metrics.design_rule_summary;
    let physical = mapped_physical_objective(&mapped, &options.target_cells, &scenarios).unwrap();
    let runtime = runtime();
    let mut power =
        MmmcPower::new(&incremental, &scenarios, &runtime, test_power_evaluator()).unwrap();
    let connectivity = crate::mapping::materialize::FrozenObservableConnectivity::capture(
        &mapped,
        &options.target_cells,
        &crate::ReferencePortMap::new(),
    )
    .unwrap();

    let net = mapped_net_by_name(&mapped, "n1");
    let sinks = buffering::net_sink_pins(&mapped, &options.target_cells, net).unwrap();
    let branch = vec![sinks[0].0];
    let candidate = cloning::clone_driver_delta(
        &mapped,
        &options.target_cells,
        net,
        &branch,
        "U_clone_0",
        "_clone_net_0",
    )
    .unwrap()
    .unwrap();

    let before_cells = mapped.cell_count();
    let before_nets = mapped.net_count();
    let before_signal = mapped.connection(branch[0]).unwrap().signal;
    let disposition = evaluate_candidate(
        CandidateEvaluation {
            mapped: &mut mapped,
            implementations: &mut implementations,
            timing: Some(&mut incremental),
            power: Some(&mut power),
            library: &options.target_cells,
            scenarios: &scenarios,
            physical,
            closure: Some(ClosureBaseline {
                analysis: &analysis,
                design_rule_summary,
            }),
            operation: "post-map timing transaction",
            connectivity: &connectivity,
        },
        candidate,
    )
    .unwrap();

    assert!(matches!(disposition, CandidateDisposition::Rejected));
    assert_eq!(mapped.cell_count(), before_cells);
    assert_eq!(mapped.net_count(), before_nets);
    assert_eq!(mapped.connection(branch[0]).unwrap().signal, before_signal);
    let after = incremental.metrics().unwrap().analysis;
    assert_eq!(after.arrival().to_bits(), analysis.arrival().to_bits());
}

#[test]
fn stale_clone_candidates_are_skipped() {
    let cells = fanout_cells();
    let options = SynthesisOptions {
        target_cells: cells.clone().into(),
    };
    let timing_library = TimingLibrary {
        cells: cells.into(),
        ..TimingLibrary::default()
    };
    let scenarios = scenario_set(&timing_library);
    let (mut mapped, mut implementations) = mapped_design(&fanout_module(), &options);
    let mapped_port_ids = port_bindings(&mapped);
    let model =
        TimingModel::from_mapped(&mapped, design_id(), &mapped_port_ids, timing_library).unwrap();
    let mut incremental = MmmcTiming::from_owner_for_test(
        IncrementalTiming::new(TimingContext::new(), model, ReportTimingOptions::default())
            .unwrap(),
        MmmcViewPolicy {
            timing: true,
            checks: opto_timing::ScenarioCheckSet::ALL,
        },
    );
    let metrics = incremental.metrics().unwrap();
    let analysis = metrics.analysis;
    let design_rule_summary = metrics.design_rule_summary;
    let physical = mapped_physical_objective(&mapped, &options.target_cells, &scenarios).unwrap();
    let runtime = runtime();
    let mut power =
        MmmcPower::new(&incremental, &scenarios, &runtime, test_power_evaluator()).unwrap();
    let connectivity = crate::mapping::materialize::FrozenObservableConnectivity::capture(
        &mapped,
        &options.target_cells,
        &crate::ReferencePortMap::new(),
    )
    .unwrap();

    let net = mapped_net_by_name(&mapped, "n1");
    let sinks = buffering::net_sink_pins(&mapped, &options.target_cells, net).unwrap();
    let branch = vec![sinks[0].0];
    let candidate = cloning::clone_driver_delta(
        &mapped,
        &options.target_cells,
        net,
        &branch,
        "U_clone_0",
        "_clone_net_0",
    )
    .unwrap()
    .unwrap();

    let driver = mapped_cell_by_name(&mapped, "U0");
    let snapshot = mapped.snapshot_region([driver], []).unwrap();
    let mut conflicting = RegionDelta::new(snapshot);
    conflicting.rename_cell(driver, "U0_touched").unwrap();
    mapped.apply_region_delta(conflicting).unwrap();

    let disposition = evaluate_candidate(
        CandidateEvaluation {
            mapped: &mut mapped,
            implementations: &mut implementations,
            timing: Some(&mut incremental),
            power: Some(&mut power),
            library: &options.target_cells,
            scenarios: &scenarios,
            physical,
            closure: Some(ClosureBaseline {
                analysis: &analysis,
                design_rule_summary,
            }),
            operation: "post-map timing transaction",
            connectivity: &connectivity,
        },
        candidate,
    )
    .unwrap();
    assert!(matches!(disposition, CandidateDisposition::Stale));
}

#[test]
fn cloning_requires_a_unique_combinational_driver_and_a_strict_sink_subset() {
    use opto_library::{TargetSequential, TargetSequentialKind};

    let cells = fanout_cells();
    let options = SynthesisOptions {
        target_cells: cells.clone().into(),
    };
    let (mapped, _) = mapped_design(&fanout_module(), &options);
    let net = mapped_net_by_name(&mapped, "n1");
    let sinks = buffering::net_sink_pins(&mapped, &options.target_cells, net).unwrap();
    let branch = vec![sinks[0].0];

    let every_sink = sinks.iter().map(|&(pin, _)| pin).collect::<Vec<_>>();
    assert_eq!(
        cloning::clone_driver_delta(
            &mapped,
            &options.target_cells,
            net,
            &every_sink,
            "U_clone_0",
            "_clone_net_0",
        )
        .unwrap(),
        None
    );

    let mut tristate_cells = cells.clone();
    tristate_cells[0].pins[1].three_state = Some(BooleanFunction::parse("A").unwrap());
    assert_eq!(
        cloning::clone_driver_delta(
            &mapped,
            &tristate_cells.into(),
            net,
            &branch,
            "U_clone_0",
            "_clone_net_0",
        )
        .unwrap(),
        None
    );

    let mut sequential_cells = cells.clone();
    sequential_cells[0].sequential.push(TargetSequential {
        kind: TargetSequentialKind::FlipFlop,
        state_variables: Vec::new(),
        clocked_on: None,
        next_state: None,
        enable: None,
        clear: None,
        preset: None,
    });
    assert_eq!(
        cloning::clone_driver_delta(
            &mapped,
            &sequential_cells.into(),
            net,
            &branch,
            "U_clone_0",
            "_clone_net_0",
        )
        .unwrap(),
        None
    );

    let mut builder =
        opto_ir::mapped::MappedBuilder::new("top", opto_ir::RevisionId::INITIAL).unwrap();
    let a = builder.add_net(Some("a")).unwrap();
    let shared = builder.add_net(Some("n")).unwrap();
    let out = builder.add_net(Some("o")).unwrap();
    for name in ["U0", "U0b"] {
        builder
            .add_cell(
                name,
                "DRV",
                Some(0),
                &[
                    (
                        "A".to_string(),
                        Some(0),
                        opto_ir::mapped::ConnectionSignal::Net(a),
                    ),
                    (
                        "Y".to_string(),
                        Some(1),
                        opto_ir::mapped::ConnectionSignal::Net(shared),
                    ),
                ],
            )
            .unwrap();
    }
    builder
        .add_cell(
            "U1",
            "SINK",
            Some(1),
            &[
                (
                    "I".to_string(),
                    Some(0),
                    opto_ir::mapped::ConnectionSignal::Net(shared),
                ),
                (
                    "O".to_string(),
                    Some(1),
                    opto_ir::mapped::ConnectionSignal::Net(out),
                ),
            ],
        )
        .unwrap();
    let multi_driver = builder.freeze().unwrap();
    let sink = buffering::net_sink_pins(&multi_driver, &options.target_cells, shared).unwrap()[0].0;
    assert_eq!(
        cloning::clone_driver_delta(
            &multi_driver,
            &options.target_cells,
            shared,
            &[sink],
            "U_clone_0",
            "_clone_net_0",
        )
        .unwrap(),
        None
    );
}

#[test]
fn cloning_inherits_driver_operator_provenance() {
    use crate::artifact::MappedCellSource;
    use crate::artifact::provenance::ProvenanceBuilder;
    use crate::planning::operator::ArchitectureDecisions;
    use opto_ir::mapped::{ConnectionSignal, MappedBuilder};
    use opto_ir::word::BinaryOp;

    let mut module = WordModule::new("top");
    let ty = WordType::bits(1).unwrap();
    let a = module
        .add_port("a", PortDirection::Input, ty, test_span())
        .unwrap();
    let b = module
        .add_port("b", PortDirection::Input, ty, test_span())
        .unwrap();
    let inputs = [a, b].map(|port| {
        module
            .read_signal(module.port(port).unwrap().signal, test_span())
            .unwrap()
    });
    let result = module
        .binary(BinaryOp::Add, inputs[0], inputs[1], test_span())
        .unwrap();
    let output = module
        .add_port("y", PortDirection::Output, ty, test_span())
        .unwrap();
    module
        .connect(
            LValue::signal(module.port(output).unwrap().signal),
            result,
            test_span(),
        )
        .unwrap();
    let plan = ArchitectureDecisions::for_module(&module).unwrap();
    let operator = plan.operators()[0].id();
    let mut provenance = ProvenanceBuilder::new(&module, &plan).unwrap();
    let origins = provenance
        .origins_for_operation_cover(&module, &[result], &inputs)
        .unwrap();
    let sink_instances = ["U1", "U2"].map(|name| {
        module
            .add_instance(name, "SINK", Vec::new(), test_span())
            .unwrap()
    });
    let mut builder = MappedBuilder::new("top", opto_ir::RevisionId::INITIAL).unwrap();
    let a_net = builder.add_net(Some("a")).unwrap();
    let split = builder.add_net(Some("n")).unwrap();
    let outputs = [
        builder.add_net(Some("o1")).unwrap(),
        builder.add_net(Some("o2")).unwrap(),
    ];
    let driver = builder
        .add_cell(
            "U0",
            "DRV",
            Some(0),
            &[
                ("A".to_string(), Some(0), ConnectionSignal::Net(a_net)),
                ("Y".to_string(), Some(1), ConnectionSignal::Net(split)),
            ],
        )
        .unwrap();
    let sinks = [0, 1].map(|index| {
        builder
            .add_cell(
                ["U1", "U2"][index],
                "SINK",
                Some(1),
                &[
                    ("I".to_string(), Some(0), ConnectionSignal::Net(split)),
                    (
                        "O".to_string(),
                        Some(1),
                        ConnectionSignal::Net(outputs[index]),
                    ),
                ],
            )
            .unwrap()
    });
    let mut mapped = builder.freeze().unwrap();
    let synthesis_regions = crate::SynthesisRegionGraph::build(&module).unwrap();
    let owner = synthesis_regions.regions()[0].id();
    let mut implementations = provenance
        .finish(
            &synthesis_regions,
            &module,
            &mapped,
            &[
                (driver, MappedCellSource::Region { origins, owner }),
                (sinks[0], MappedCellSource::Instance(sink_instances[0])),
                (sinks[1], MappedCellSource::Instance(sink_instances[1])),
            ],
        )
        .unwrap();

    let library: opto_library::TargetCellSet = fanout_cells().into();
    let branch = buffering::net_sink_pins(&mapped, &library, split).unwrap()[0].0;
    let candidate = cloning::clone_driver_delta(
        &mapped,
        &library,
        split,
        &[branch],
        "U_clone_0",
        "_clone_net_0",
    )
    .unwrap()
    .unwrap();
    let PostmapCandidate {
        delta,
        implementation,
        guard: _,
    } = candidate;
    let applied = mapped.apply_region_delta(delta).unwrap();
    let clone = applied.added_cells().next().unwrap().1;
    let prepared = implementations
        .prepare_region_edit(&mapped, &applied, &implementation)
        .unwrap();
    implementations.commit_region_edit(prepared).unwrap();

    assert_eq!(
        implementations.operators_for_cell(clone),
        Some(std::slice::from_ref(&operator))
    );
    assert_eq!(
        implementations.operators_for_cell(driver),
        Some(std::slice::from_ref(&operator))
    );
    let region = implementations.region_for_operator(operator).unwrap();
    assert!(region.mapped_cells().contains(&clone));
    assert!(region.mapped_cells().contains(&driver));
}

#[test]
fn pin_swap_moves_a_constrained_net_to_the_lower_load_symmetric_pin() {
    let mut cell = and_cell("AND2", 1.0, [0.1, 0.1]);
    cell.pins[0].capacitance = Some(10.0);
    cell.pins[1].capacitance = Some(1.0);
    let mut symmetric = cell.clone();
    symmetric.pins[0].capacitance = symmetric.pins[1].capacitance;
    let cells = opto_library::TargetCellSet::from(vec![cell.clone(), symmetric.clone()]);
    assert!(super::candidates::pin_swap_changes_timing(
        cells.get(0).unwrap(),
        "A",
        "B"
    ));
    assert!(!super::candidates::pin_swap_changes_timing(
        cells.get(1).unwrap(),
        "A",
        "B"
    ));
    let options = SynthesisOptions {
        target_cells: vec![cell.clone()].into(),
    };
    let timing_library = TimingLibrary {
        cells: vec![cell].into(),
        ..TimingLibrary::default()
    };
    let module = mapped_and_module("AND2");
    let (mut mapped, mut implementations) = mapped_design(&module, &options);
    let mapped_cell = opto_ir::mapped::CellId::from_index(0).unwrap();
    let original = mapped_connections(&mapped, mapped_cell);
    let mut timing = TimingContext::new();
    timing
        .set_max_capacitance(
            5.0,
            &[TimingObject::port(
                port_id(2),
                design_id(),
                TimingPortDirection::Input,
            )],
            opto_timing::DesignRuleScope::All,
        )
        .unwrap();

    let outcome = run_postmap(PostmapRun {
        mapped: &mut mapped,
        implementations: &mut implementations,
        options: &options,
        scenarios: single_scenario(&timing, timing_library),
    });

    let updated = mapped_connections(&mapped, mapped_cell);
    assert_eq!(outcome.replacements, 1);
    assert_eq!(updated["A"], original["B"]);
    assert_eq!(updated["B"], original["A"]);
}

#[test]
fn pin_swap_forest_rewires_multiple_cells_atomically() {
    let mut cell = and_cell("AND2", 1.0, [0.1, 0.1]);
    cell.pins[0].capacitance = Some(10.0);
    cell.pins[1].capacitance = Some(1.0);
    let options = SynthesisOptions {
        target_cells: vec![cell].into(),
    };
    let (mut mapped, _) = mapped_design(&parallel_and_module("AND2", 2), &options);
    let cells = (0..2)
        .map(|index| mapped_cell_by_name(&mapped, &format!("U{index}")))
        .collect::<Vec<_>>();
    let originals = cells
        .iter()
        .map(|&cell| mapped_connections(&mapped, cell))
        .collect::<Vec<_>>();
    let plans = cells
        .iter()
        .map(|&cell| sizing::pin_swap_plan(&mapped, cell, "A", "B").unwrap())
        .collect::<Vec<_>>();

    let candidate = sizing::pin_swap_forest_delta(&mapped, &plans).unwrap();
    let applied = mapped.apply_region_delta(candidate.delta).unwrap();

    assert_eq!(applied.affected_cells().count(), 2);
    for (cell, original) in cells.into_iter().zip(originals) {
        let updated = mapped_connections(&mapped, cell);
        assert_eq!(updated["A"], original["B"]);
        assert_eq!(updated["B"], original["A"]);
    }
}

fn mapped_connections(
    mapped: &MappedNetlist,
    cell: opto_ir::mapped::CellId,
) -> BTreeMap<String, opto_ir::mapped::ConnectionSignal> {
    mapped
        .connections(cell)
        .unwrap()
        .iter()
        .map(|connection| {
            (
                mapped.pin_name(connection).unwrap().to_string(),
                connection.signal,
            )
        })
        .collect()
}

#[test]
fn parallel_sizing_candidate_generation_is_deterministic() {
    let options = SynthesisOptions {
        target_cells: vec![
            and_cell("AND_SMALL", 1.0, [1.0, 3.0]),
            and_cell("AND_FAST", 3.0, [0.8, 1.0]),
        ]
        .into(),
    };
    let instances = [
        CellId::from_index(0).unwrap(),
        CellId::from_index(1).unwrap(),
        CellId::from_index(0).unwrap(),
    ];
    let mut builder =
        opto_ir::mapped::MappedBuilder::new("top", opto_ir::RevisionId::INITIAL).unwrap();
    builder.add_cell("U0", "AND_SMALL", Some(0), &[]).unwrap();
    builder.add_cell("U1", "AND_FAST", Some(1), &[]).unwrap();
    let mapped = builder.freeze().unwrap();
    let serial = ExecutionContext::new(&ExecutionConfig { max_threads: 1 }).unwrap();
    let parallel = ExecutionContext::new(&ExecutionConfig { max_threads: 4 }).unwrap();
    let catalog = PostmapCellCatalog::new(&options);

    let serial_regions =
        sizing_regions(&serial, instances, &mapped, &options, &catalog, false, None).unwrap();
    let parallel_regions = sizing_regions(
        &parallel, instances, &mapped, &options, &catalog, false, None,
    )
    .unwrap();

    assert_eq!(serial_regions, parallel_regions);
    assert_eq!(serial_regions.len(), 2);
}

#[test]
fn net_only_region_updates_identity_without_recomputing_timing() {
    let cells = vec![and_cell("AND2", 1.0, [0.1, 0.1])];
    let timing_library = TimingLibrary {
        cells: cells.into(),
        ..TimingLibrary::default()
    };
    let mut builder =
        opto_ir::mapped::MappedBuilder::new("top", opto_ir::RevisionId::INITIAL).unwrap();
    let spare = builder.add_net(Some("spare")).unwrap();
    let mut mapped = builder.freeze().unwrap();
    let model = TimingModel::from_mapped(
        &mapped,
        design_id(),
        &opto_timing::PortBindings::new([]),
        timing_library,
    )
    .unwrap();
    let mut timing =
        IncrementalTiming::new(TimingContext::new(), model, ReportTimingOptions::default())
            .unwrap();
    let snapshot = mapped.snapshot_region([], [spare]).unwrap();
    let mut delta = RegionDelta::new(snapshot);
    delta
        .rename_net(spare, Some("renamed".to_string()))
        .unwrap();

    let transaction = MappedTimingTransaction::begin_optimization(
        &mut mapped,
        std::slice::from_mut(&mut timing),
        delta,
    )
    .unwrap()
    .expect("fresh net-only edit must not be stale");
    assert_eq!(
        transaction
            .timing_edit()
            .expect("affected mapped nets carry explicit timing identity")
            .recomputed_nets(),
        0
    );
    assert_eq!(transaction.mapped().net_name(spare), Some("renamed"));

    transaction.rollback().unwrap();
    assert_eq!(mapped.net_name(spare), Some("spare"));
}

fn mapped_and_module(cell: &str) -> WordModule {
    let mut module = WordModule::new("top");
    let ty = WordType::bits(1).unwrap();
    let a = module
        .add_port("a", PortDirection::Input, ty, test_span())
        .unwrap();
    let b = module
        .add_port("b", PortDirection::Input, ty, test_span())
        .unwrap();
    let y = module
        .add_port("y", PortDirection::Output, ty, test_span())
        .unwrap();
    let net = module.add_wire("n", ty, test_span()).unwrap();
    let a = module
        .read_signal(module.port(a).unwrap().signal, test_span())
        .unwrap();
    let b = module
        .read_signal(module.port(b).unwrap().signal, test_span())
        .unwrap();
    let net_value = module.read_signal(net, test_span()).unwrap();
    module
        .add_instance(
            "U0",
            cell,
            vec![
                ("A".to_string(), a, test_span()),
                ("B".to_string(), b, test_span()),
                ("Y".to_string(), net_value, test_span()),
            ],
            test_span(),
        )
        .unwrap();
    module
        .connect(
            LValue::signal(module.port(y).unwrap().signal),
            net_value,
            test_span(),
        )
        .unwrap();
    module
}

fn parallel_and_module(cell: &str, instances: usize) -> WordModule {
    let mut module = WordModule::new("top");
    let ty = WordType::bits(1).unwrap();
    for index in 0..instances {
        let a = module
            .add_port(format!("a{index}"), PortDirection::Input, ty, test_span())
            .unwrap();
        let b = module
            .add_port(format!("b{index}"), PortDirection::Input, ty, test_span())
            .unwrap();
        let y = module
            .add_port(format!("y{index}"), PortDirection::Output, ty, test_span())
            .unwrap();
        let net = module
            .add_wire(format!("n{index}"), ty, test_span())
            .unwrap();
        let a = module
            .read_signal(module.port(a).unwrap().signal, test_span())
            .unwrap();
        let b = module
            .read_signal(module.port(b).unwrap().signal, test_span())
            .unwrap();
        let net = module.read_signal(net, test_span()).unwrap();
        module
            .add_instance(
                format!("U{index}"),
                cell,
                vec![
                    ("A".to_string(), a, test_span()),
                    ("B".to_string(), b, test_span()),
                    ("Y".to_string(), net, test_span()),
                ],
                test_span(),
            )
            .unwrap();
        module
            .connect(
                LValue::signal(module.port(y).unwrap().signal),
                net,
                test_span(),
            )
            .unwrap();
    }
    module
}

fn and_cell(name: &str, area: f64, delays: [f64; 2]) -> TargetCell {
    let arc = |related_pin: &str| TargetTimingArc {
        related_pin: related_pin.to_string(),
        timing_type: TargetTimingType::Combinational,
        timing_sense: TimingSense::PositiveUnate,
        delay_model: Some(opto_library::ArcDelayModel::Nldm(
            opto_library::NldmTimingModel::new(
                Some(LookupTable::new(
                    Vec::new(),
                    vec![0.0, 10.0],
                    delays.to_vec(),
                )),
                Some(LookupTable::new(
                    Vec::new(),
                    vec![0.0, 10.0],
                    delays.to_vec(),
                )),
                None,
                None,
            ),
        )),
        rise_constraint: None,
        fall_constraint: None,
    };
    TargetCell {
        dont_use: false,
        usage: opto_library::TargetCellUsage::default(),
        name: name.to_string(),
        area: Some(area),
        sequential: Vec::new(),
        pins: vec![
            TargetPin {
                name: "A".to_string(),
                direction: TargetPinDirection::Input,
                function: None,
                three_state: None,
                capacitance: Some(0.1),
                rise_capacitance: None,
                fall_capacitance: None,
                receiver_capacitance: None,
                fanout_load: None,
                next_state_type: None,
                timing_arcs: Vec::new(),
                clock_gate_role: None,
            },
            TargetPin {
                name: "B".to_string(),
                direction: TargetPinDirection::Input,
                function: None,
                three_state: None,
                capacitance: Some(0.1),
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
                function: Some(BooleanFunction::parse("A B").unwrap()),
                three_state: None,
                capacitance: None,
                rise_capacitance: None,
                fall_capacitance: None,
                receiver_capacitance: None,
                fanout_load: None,
                next_state_type: None,
                timing_arcs: vec![arc("A"), arc("B")],
                clock_gate_role: None,
            },
        ],
        clock_gate: None,
        memory: None,
    }
}

fn fanout_module() -> WordModule {
    fanout_module_with_sinks(3)
}

fn fanout_module_with_sinks(sink_count: usize) -> WordModule {
    let mut module = WordModule::new("top");
    let ty = WordType::bits(1).unwrap();
    let a = module
        .add_port("a", PortDirection::Input, ty, test_span())
        .unwrap();
    let outputs = (0..sink_count)
        .map(|index| {
            module
                .add_port(format!("y{index}"), PortDirection::Output, ty, test_span())
                .unwrap()
        })
        .collect::<Vec<_>>();
    let n1 = module.add_wire("n1", ty, test_span()).unwrap();
    let a = module
        .read_signal(module.port(a).unwrap().signal, test_span())
        .unwrap();
    let n1_value = module.read_signal(n1, test_span()).unwrap();
    module
        .add_instance(
            "U0",
            "DRV",
            vec![
                ("A".into(), a, test_span()),
                ("Y".into(), n1_value, test_span()),
            ],
            test_span(),
        )
        .unwrap();
    for (index, output) in outputs.into_iter().enumerate() {
        let net = module
            .add_wire(format!("n{}", index + 2), ty, test_span())
            .unwrap();
        let net_value = module.read_signal(net, test_span()).unwrap();
        module
            .add_instance(
                format!("U{}", index + 1),
                "SINK",
                vec![
                    ("I".into(), n1_value, test_span()),
                    ("O".into(), net_value, test_span()),
                ],
                test_span(),
            )
            .unwrap();
        module
            .connect(
                LValue::signal(module.port(output).unwrap().signal),
                net_value,
                test_span(),
            )
            .unwrap();
    }
    module
}

fn two_fanout_module() -> WordModule {
    let mut module = WordModule::new("top");
    let ty = WordType::bits(1).unwrap();
    for tree in 0..2 {
        let input = module
            .add_port(format!("a{tree}"), PortDirection::Input, ty, test_span())
            .unwrap();
        let input = module
            .read_signal(module.port(input).unwrap().signal, test_span())
            .unwrap();
        let trunk = module
            .add_wire(format!("trunk{tree}"), ty, test_span())
            .unwrap();
        let trunk = module.read_signal(trunk, test_span()).unwrap();
        module
            .add_instance(
                format!("U{tree}_driver"),
                "DRV",
                vec![
                    ("A".into(), input, test_span()),
                    ("Y".into(), trunk, test_span()),
                ],
                test_span(),
            )
            .unwrap();
        for branch in 0..3 {
            let output = module
                .add_port(
                    format!("y{tree}_{branch}"),
                    PortDirection::Output,
                    ty,
                    test_span(),
                )
                .unwrap();
            let leaf = module
                .add_wire(format!("leaf{tree}_{branch}"), ty, test_span())
                .unwrap();
            let leaf = module.read_signal(leaf, test_span()).unwrap();
            module
                .add_instance(
                    format!("U{tree}_sink_{branch}"),
                    "SINK",
                    vec![
                        ("I".into(), trunk, test_span()),
                        ("O".into(), leaf, test_span()),
                    ],
                    test_span(),
                )
                .unwrap();
            module
                .connect(
                    LValue::signal(module.port(output).unwrap().signal),
                    leaf,
                    test_span(),
                )
                .unwrap();
        }
    }
    module
}

fn unary_cell(
    name: &str,
    input: &str,
    output: &str,
    area: f64,
    capacitance: f64,
    delays: [f64; 2],
) -> TargetCell {
    TargetCell {
        dont_use: false,
        usage: opto_library::TargetCellUsage::default(),
        name: name.to_string(),
        area: Some(area),
        sequential: Vec::new(),
        pins: vec![
            TargetPin {
                name: input.to_string(),
                direction: TargetPinDirection::Input,
                function: None,
                three_state: None,
                capacitance: Some(capacitance),
                rise_capacitance: None,
                fall_capacitance: None,
                receiver_capacitance: None,
                fanout_load: None,
                next_state_type: None,
                timing_arcs: Vec::new(),
                clock_gate_role: None,
            },
            TargetPin {
                name: output.to_string(),
                direction: TargetPinDirection::Output,
                function: Some(BooleanFunction::parse(input).unwrap()),
                three_state: None,
                capacitance: None,
                rise_capacitance: None,
                fall_capacitance: None,
                receiver_capacitance: None,
                fanout_load: None,
                next_state_type: None,
                timing_arcs: vec![TargetTimingArc {
                    related_pin: input.to_string(),
                    timing_type: TargetTimingType::Combinational,
                    timing_sense: TimingSense::PositiveUnate,
                    delay_model: Some(opto_library::ArcDelayModel::Nldm(
                        opto_library::NldmTimingModel::new(
                            Some(LookupTable::new(
                                Vec::new(),
                                vec![0.0, 10.0],
                                delays.to_vec(),
                            )),
                            Some(LookupTable::new(
                                Vec::new(),
                                vec![0.0, 10.0],
                                delays.to_vec(),
                            )),
                            None,
                            None,
                        ),
                    )),
                    rise_constraint: None,
                    fall_constraint: None,
                }],
                clock_gate_role: None,
            },
        ],
        clock_gate: None,
        memory: None,
    }
}
