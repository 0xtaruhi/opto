// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Incremental timing generations, regional edits, rollback, and optimization.
//!
//! Exact propagation semantics remain owned by `analysis::tests`; this suite
//! proves equivalence to full recomputation and transactional engine behavior.

use super::*;
use crate::test_library::{ClockToQArc, TimingArc, TimingCell, test_cells, test_instance};
use crate::{
    ArcDelayModel, ClockSpec, DesignRuleScope, LookupTable, NldmTimingModel, TargetCell, TargetPin,
    TargetPinDirection, TargetTimingArc, TargetTimingType, TimingCheckKind, TimingDesign,
    TimingEdge, TimingEndpoint, TimingInstance, TimingInstanceId, TimingLibrary, TimingObject,
    TimingObjectKind, TimingPort, TimingPortDirection, TimingRegionDelta, TimingRequirement,
    TimingSense, test_clock_id, test_design_id, test_port, test_port_id,
};
use opto_library::BooleanFunction;
use std::collections::{BTreeMap, BTreeSet};

fn timing_object(raw: u64, _name: &str, kind: TimingObjectKind) -> TimingObject {
    let uid = crate::test_object_uid(raw);
    match kind {
        TimingObjectKind::Design => TimingObject::design(opto_db::DesignId::from_uid(uid)),
        TimingObjectKind::Port(direction) => {
            TimingObject::port(opto_db::PortId::from_uid(uid), test_design_id(), direction)
        }
        TimingObjectKind::Clock => TimingObject::clock(opto_db::ClockId::from_uid(uid)),
        TimingObjectKind::Cell => TimingObject::cell(opto_db::CellId::from_uid(uid)),
        TimingObjectKind::Pin => TimingObject::pin(opto_db::PinId::from_uid(uid)),
        TimingObjectKind::Net => TimingObject::net(opto_db::NetId::from_uid(uid)),
    }
}

fn assert_summary_matches_quality(incremental: &IncrementalTiming) {
    let quality = incremental.quality().unwrap();
    assert_eq!(quality.generation(), incremental.model.generation());
    assert_eq!(incremental.net_states().generation(), quality.generation());
    let summary = incremental.quality_summary().unwrap();
    assert_eq!(summary.arrival().to_bits(), quality.arrival().to_bits());
    assert_eq!(
        summary.wns().map(f64::to_bits),
        quality.wns().map(f64::to_bits)
    );
    assert_eq!(summary.tns().to_bits(), quality.tns().to_bits());
    assert_eq!(summary.violating_paths(), quality.violating_paths());
}

#[test]
fn incrementally_recomputes_dirty_cones_and_exception_tags() {
    let mut timing = TimingContext::new();
    let model = Arc::new(chain_model());
    let engine = TimingEngine::new(ExecutionContext::default());
    let options = ReportTimingOptions::default();

    let initial = engine
        .analyze(&timing, Arc::clone(&model), &options)
        .unwrap();
    assert!((initial.arrival() - 0.3).abs() < 1e-12);
    assert_eq!(
        engine.metrics().unwrap(),
        TimingEngineMetrics {
            full_updates: 1,
            incremental_updates: 0,
            cache_hits: 0,
            recomputed_nets: 4,
        }
    );

    timing.set_load(10.0, &[test_port_id("y")]).unwrap();
    let incremental = engine
        .analyze(&timing, Arc::clone(&model), &options)
        .unwrap();
    let exact = TimingEngine::analyze_once(&timing, &model, &options).unwrap();
    assert!((incremental.arrival() - 0.6).abs() < 1e-12);
    assert_eq!(incremental.arrival(), exact.arrival());
    assert_eq!(
        engine.metrics().unwrap(),
        TimingEngineMetrics {
            full_updates: 1,
            incremental_updates: 1,
            cache_hits: 0,
            recomputed_nets: 5,
        }
    );

    timing
        .set_max_delay(
            0.5,
            vec![TimingEndpoint::Port(test_port_id("a"))],
            vec![TimingEndpoint::Port(test_port_id("y"))],
        )
        .unwrap();
    let constrained = engine
        .analyze(&timing, Arc::clone(&model), &options)
        .unwrap();
    assert_eq!(constrained.required(), Some(0.5));
    assert_eq!(
        engine.metrics().unwrap(),
        TimingEngineMetrics {
            full_updates: 1,
            incremental_updates: 2,
            cache_hits: 0,
            recomputed_nets: 9,
        }
    );

    engine.analyze(&timing, model, &options).unwrap();
    assert_eq!(engine.metrics().unwrap().cache_hits, 1);

    let min_options = ReportTimingOptions {
        delay_type: DelayType::Min,
        ..ReportTimingOptions::default()
    };
    let model = Arc::new(chain_model());
    let second_engine = TimingEngine::new(ExecutionContext::default());
    second_engine
        .analyze(&timing, Arc::clone(&model), &options)
        .unwrap();
    second_engine
        .analyze(&timing, Arc::clone(&model), &min_options)
        .unwrap();
    second_engine.analyze(&timing, model, &options).unwrap();
    assert_eq!(
        second_engine.metrics().unwrap(),
        TimingEngineMetrics {
            full_updates: 2,
            incremental_updates: 0,
            cache_hits: 1,
            recomputed_nets: 8,
        }
    );
}

fn buf_instance(id: u32, name: &str, cell: &str, input: &str, output: &str) -> TimingInstance {
    test_instance(id, name, cell, [("A", input), ("Y", output)])
}

#[test]
fn incrementally_recomputes_cell_replacements_and_rollbacks() {
    let timing = TimingContext::new();
    let mut incremental =
        IncrementalTiming::new(timing, chain_model(), ReportTimingOptions::default()).unwrap();

    assert_summary_matches_quality(&incremental);
    assert!((incremental.analyze().unwrap().arrival() - 0.3).abs() < 1e-12);
    let mut delta = TimingRegionDelta::new();
    delta
        .set_instance(buf_instance(1, "U1", "FAST_BUF", "n1", "n2"))
        .unwrap();
    let edit = incremental.apply_region_delta(delta).unwrap();
    assert!(edit.recomputed_nets() < 4);
    assert_summary_matches_quality(&incremental);
    assert!((incremental.analyze().unwrap().arrival() - 0.25).abs() < 1e-12);

    incremental.rollback(edit).unwrap();
    assert_summary_matches_quality(&incremental);
    assert!((incremental.analyze().unwrap().arrival() - 0.3).abs() < 1e-12);
}

#[test]
fn multi_seed_closures_match_full_recomputation_in_every_execution_mode() {
    for (first, last) in [("FAST_BUF", "BUF"), ("BUF", "FAST_BUF")] {
        let base = long_chain_model();
        let mut rebuilt = base.design().to_owned();
        rebuilt.instances[0].cell = first.to_string();
        rebuilt.instances[3].cell = last.to_string();
        let reference = IncrementalTiming::new(
            TimingContext::new(),
            TimingModel::new(rebuilt, base.library().clone()).unwrap(),
            ReportTimingOptions::default(),
        )
        .unwrap();

        for threads in [None, Some(1), Some(4)] {
            let model = long_chain_model();
            let mut incremental = match threads {
                None => IncrementalTiming::new(
                    TimingContext::new(),
                    model,
                    ReportTimingOptions::default(),
                )
                .unwrap(),
                Some(max_threads) => IncrementalTiming::new_for_optimization(
                    TimingContext::new(),
                    model,
                    ReportTimingOptions::default(),
                    opto_runtime::ExecutionContext::new(&opto_runtime::ExecutionConfig {
                        max_threads,
                    })
                    .unwrap(),
                )
                .unwrap(),
            };
            let mut delta = TimingRegionDelta::new();
            delta
                .set_instance(buf_instance(0, "U0", first, "a", "n1"))
                .unwrap();
            delta
                .set_instance(buf_instance(3, "U3", last, "n3", "y"))
                .unwrap();
            let edit = match threads {
                None => incremental.apply_region_delta(delta).unwrap(),
                Some(_) => incremental.apply_optimization_region_delta(delta).unwrap(),
            };
            incremental.commit(edit).unwrap();
            incremental
                .instances_with_slack_at_most(f64::INFINITY)
                .unwrap();

            assert_eq!(incremental.net_states(), reference.net_states());
            assert_eq!(
                incremental.quality_summary().unwrap(),
                reference.quality_summary().unwrap()
            );
        }
    }
}

#[test]
fn region_generation_retains_analysis_input_identity() {
    let base = chain_model();
    let mut power_library = base.library().clone();
    power_library.power.units.nominal_voltage = Some(0.9);
    let power_model = TimingModel::new(base.design().to_owned(), power_library).unwrap();
    let mut baseline =
        IncrementalTiming::new(TimingContext::new(), base, ReportTimingOptions::default()).unwrap();
    let mut changed = IncrementalTiming::new(
        TimingContext::new(),
        power_model,
        ReportTimingOptions::default(),
    )
    .unwrap();
    assert_ne!(baseline.model.generation(), changed.model.generation());

    for incremental in [&mut baseline, &mut changed] {
        let mut delta = TimingRegionDelta::new();
        delta
            .set_instance(buf_instance(1, "U1", "FAST_BUF", "n1", "n2"))
            .unwrap();
        let edit = incremental.apply_region_delta(delta).unwrap();
        incremental.commit(edit).unwrap();
    }

    assert_ne!(baseline.model.generation(), changed.model.generation());
}

#[test]
fn endpoint_shape_changes_splice_closure_and_roll_back() {
    let clocked = || {
        let mut timing = TimingContext::new();
        timing
            .create_clock(
                test_clock_id(1),
                ClockSpec::new("sys", 10.0, vec![test_port_id("clk")], None).unwrap(),
            )
            .unwrap();
        timing
    };
    let rebuilt = |mutate: &dyn Fn(&mut Vec<TimingInstance>)| {
        let base = setup_chain_model();
        let mut design = base.design().to_owned();
        mutate(&mut design.instances);
        let reference = IncrementalTiming::new(
            clocked(),
            TimingModel::new(design, base.library).unwrap(),
            ReportTimingOptions::default(),
        )
        .unwrap();
        reference.quality_summary().unwrap()
    };
    let mut incremental = IncrementalTiming::new(
        clocked(),
        setup_chain_model(),
        ReportTimingOptions::default(),
    )
    .unwrap();
    let baseline = incremental.quality_summary().unwrap();

    let added_ff = test_instance(7, "U_ff3", "DFF", [("CK", "clk"), ("D", "n1"), ("Q", "q3")]);
    let mut delta = TimingRegionDelta::new();
    delta.set_instance(added_ff.clone()).unwrap();
    let edit = incremental.apply_optimization_region_delta(delta).unwrap();
    assert_eq!(
        incremental.quality_summary().unwrap(),
        rebuilt(&|instances| instances.push(added_ff.clone()))
    );

    incremental.rollback(edit).unwrap();
    assert_eq!(incremental.quality_summary().unwrap(), baseline);

    let swapped_buf = buf_instance(2, "U_ff2", "BUF", "n1", "y");
    let mut delta = TimingRegionDelta::new();
    delta.set_instance(swapped_buf.clone()).unwrap();
    let edit = incremental.apply_optimization_region_delta(delta).unwrap();
    assert_eq!(
        incremental.quality_summary().unwrap(),
        rebuilt(&|instances| instances[2] = swapped_buf.clone())
    );

    incremental.rollback(edit).unwrap();
    assert_eq!(incremental.quality_summary().unwrap(), baseline);
}

#[test]
fn committed_endpoint_shape_changes_reuse_closure_slots() {
    let mut timing = TimingContext::new();
    timing
        .create_clock(
            test_clock_id(1),
            ClockSpec::new("sys", 10.0, vec![test_port_id("clk")], None).unwrap(),
        )
        .unwrap();
    let mut incremental =
        IncrementalTiming::new(timing, setup_chain_model(), ReportTimingOptions::default())
            .unwrap();
    let initial_slots = incremental.closure_slot_counts();

    let mut remove_endpoint = TimingRegionDelta::new();
    remove_endpoint
        .set_instance(buf_instance(2, "U_ff2", "BUF", "n1", "y"))
        .unwrap();
    let edit = incremental
        .apply_optimization_region_delta(remove_endpoint)
        .unwrap();
    incremental.commit(edit).unwrap();
    assert_eq!(
        incremental.closure_slot_counts(),
        (initial_slots.0, initial_slots.1 + 1)
    );

    let restored_ff = test_instance(2, "U_ff2", "DFF", [("CK", "clk"), ("D", "n1"), ("Q", "y")]);
    let mut restore_endpoint = TimingRegionDelta::new();
    restore_endpoint.set_instance(restored_ff.clone()).unwrap();
    let edit = incremental
        .apply_optimization_region_delta(restore_endpoint)
        .unwrap();
    incremental.rollback(edit).unwrap();
    assert_eq!(
        incremental.closure_slot_counts(),
        (initial_slots.0, initial_slots.1 + 1)
    );

    let mut restore_endpoint = TimingRegionDelta::new();
    restore_endpoint.set_instance(restored_ff).unwrap();
    let edit = incremental
        .apply_optimization_region_delta(restore_endpoint)
        .unwrap();
    incremental.commit(edit).unwrap();
    assert_eq!(incremental.closure_slot_counts(), initial_slots);
    assert_summary_matches_quality(&incremental);
}

#[test]
fn cyclic_optimization_delta_is_rejected_instead_of_diverging() {
    let mut incremental = IncrementalTiming::new_for_optimization(
        TimingContext::new(),
        chain_model(),
        ReportTimingOptions::default(),
        opto_runtime::ExecutionContext::default(),
    )
    .unwrap();
    let before = incremental.quality_summary().unwrap();

    let mut delta = TimingRegionDelta::new();
    delta
        .set_instance(buf_instance(7, "U_loop", "BUF", "n2", "n1"))
        .unwrap();
    let error = incremental.apply_optimization_region_delta(delta);
    assert!(error.is_err());
    assert_eq!(incremental.quality_summary().unwrap(), before);
}

#[test]
fn duplicate_instance_ids_are_rejected_at_model_construction() {
    let base = chain_model();
    let mut design = base.design().to_owned();
    let mut duplicate = design.instances[0].clone();
    duplicate.name = "U_duplicate".to_string();
    design.instances.push(duplicate);
    assert!(TimingModel::new(design, base.library).is_err());
}

#[test]
fn optimization_engine_matches_scalar_quality_without_path_nodes() {
    let timing = TimingContext::new();
    let regular = IncrementalTiming::new(
        timing.clone(),
        chain_model(),
        ReportTimingOptions::default(),
    )
    .unwrap();
    let optimization = IncrementalTiming::new_for_optimization(
        timing,
        chain_model(),
        ReportTimingOptions::default(),
        opto_runtime::ExecutionContext::default(),
    )
    .unwrap();

    assert_eq!(
        optimization.quality_summary().unwrap(),
        regular.quality_summary().unwrap()
    );
    let optimization_path = optimization.analyze().unwrap();
    let regular_path = regular.analyze().unwrap();
    assert_eq!(optimization_path.startpoint(), regular_path.startpoint());
    assert_eq!(optimization_path.endpoint(), regular_path.endpoint());
    assert_eq!(optimization_path.arrival(), regular_path.arrival());
    assert_eq!(
        optimization_path
            .steps()
            .iter()
            .map(|step| (
                step.point(),
                step.kind(),
                step.increment().to_bits(),
                step.path().to_bits(),
            ))
            .collect::<Vec<_>>(),
        regular_path
            .steps()
            .iter()
            .map(|step| (
                step.point(),
                step.kind(),
                step.increment().to_bits(),
                step.path().to_bits(),
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn parallel_optimization_summary_matches_sequential_launch_timing() {
    let mut timing = TimingContext::new();
    timing
        .create_clock(
            test_clock_id(1),
            ClockSpec::new("sys", 10.0, vec![test_port_id("clk")], None).unwrap(),
        )
        .unwrap();
    let regular = IncrementalTiming::new(
        timing.clone(),
        setup_chain_model_with_ids([10, 20, 30]),
        ReportTimingOptions::default(),
    )
    .unwrap();
    let optimization = IncrementalTiming::new_for_optimization(
        timing,
        setup_chain_model_with_ids([10, 20, 30]),
        ReportTimingOptions::default(),
        opto_runtime::ExecutionContext::default(),
    )
    .unwrap();

    assert_eq!(
        optimization.quality_summary().unwrap(),
        regular.quality_summary().unwrap()
    );
    assert_eq!(
        optimization.net_state("q1").unwrap(),
        regular.net_state("q1").unwrap()
    );
}

#[test]
fn stable_instance_ids_do_not_depend_on_dense_design_order() {
    let mut incremental = IncrementalTiming::new(
        TimingContext::new(),
        chain_model_with_ids([3, 10, 42]),
        ReportTimingOptions::default(),
    )
    .unwrap();

    assert_eq!(
        incremental.instance_cell(TimingInstanceId::from_raw(10)),
        Some("BUF")
    );
    let mut delta = TimingRegionDelta::new();
    delta
        .set_instance(buf_instance(10, "U1", "FAST_BUF", "n1", "n2"))
        .unwrap();
    let edit = incremental.apply_region_delta(delta).unwrap();
    assert!((incremental.analyze().unwrap().arrival() - 0.25).abs() < 1e-12);
    incremental.rollback(edit).unwrap();
    assert!((incremental.analyze().unwrap().arrival() - 0.3).abs() < 1e-12);
}

#[test]
fn generic_region_delta_removes_and_reconnects_multiple_instances_atomically() {
    let mut incremental = IncrementalTiming::new(
        TimingContext::new(),
        chain_model_with_ids([3, 10, 42]),
        ReportTimingOptions::default(),
    )
    .unwrap();
    let before = incremental.quality().unwrap();
    let before_order = incremental
        .model
        .design
        .instances()
        .map(|instance| instance.id)
        .collect::<Vec<_>>();
    let mut delta = crate::TimingRegionDelta::new();
    delta
        .remove_instance(TimingInstanceId::from_raw(10))
        .unwrap();
    delta
        .set_instance(test_instance(42, "U2", "BUF", [("A", "n1"), ("Y", "y")]))
        .unwrap();

    let edit = incremental.apply_region_delta(delta).unwrap();
    assert!(edit.recomputed_nets() < 4);
    assert_summary_matches_quality(&incremental);
    assert!((incremental.analyze().unwrap().arrival() - 0.2).abs() < 1e-12);
    assert!(
        incremental
            .instance_cell(TimingInstanceId::from_raw(10))
            .is_none()
    );

    incremental.rollback(edit).unwrap();
    assert_summary_matches_quality(&incremental);
    let after = incremental.quality().unwrap();
    assert_eq!(after.arrival(), before.arrival());
    assert_eq!(after.wns(), before.wns());
    assert_eq!(after.tns(), before.tns());
    assert_eq!(
        incremental
            .model
            .design
            .instances()
            .map(|instance| instance.id)
            .collect::<Vec<_>>(),
        before_order
    );
    assert_eq!(
        incremental.instance_cell(TimingInstanceId::from_raw(10)),
        Some("BUF")
    );
}

#[test]
fn incrementally_inserts_and_rolls_back_a_buffer_branch() {
    let mut incremental = IncrementalTiming::new(
        TimingContext::new(),
        buffer_model(),
        ReportTimingOptions::default(),
    )
    .unwrap();
    assert!((incremental.analyze().unwrap().arrival() - 0.5).abs() < 1e-12);

    let mut delta = TimingRegionDelta::new();
    delta
        .set_instance(buf_instance(1, "U1", "BUF", "buffer_net_0", "y"))
        .unwrap();
    delta
        .set_instance(buf_instance(
            2,
            "U_buffer_0",
            "FAST_BUF",
            "n1",
            "buffer_net_0",
        ))
        .unwrap();
    let edit = incremental.apply_region_delta(delta).unwrap();
    assert!(edit.recomputed_nets() < 4);
    assert!((incremental.analyze().unwrap().arrival() - 0.25).abs() < 1e-12);

    incremental.rollback(edit).unwrap();
    assert!((incremental.analyze().unwrap().arrival() - 0.5).abs() < 1e-12);
}

#[test]
fn net_observability_reports_design_rule_violations() {
    let mut timing = TimingContext::new();
    let design = timing_object(1, "top", TimingObjectKind::Design);
    timing
        .set_max_capacitance(5.0, std::slice::from_ref(&design), DesignRuleScope::All)
        .unwrap();
    timing.set_max_fanout(0.5, &[design]).unwrap();
    let incremental =
        IncrementalTiming::new(timing, buffer_model(), ReportTimingOptions::default()).unwrap();

    let state = incremental.net_state("n1").unwrap();
    assert_eq!(state.capacitance, 10.0);
    assert_eq!(state.fanout, 1.0);
    let pin = incremental
        .pin_state(TimingInstanceId::from_raw(1), "A")
        .unwrap();
    assert_eq!(pin.name, "U1/A");
    assert_eq!(pin.net, "n1");
    assert_eq!(pin.capacitance, 10.0);
    assert_eq!(pin.fanout_load, 1.0);
    assert!(incremental.pin_states().contains(&pin));
    let violations = incremental.design_rule_violations();
    let summary = incremental.design_rule_summary();
    assert_eq!(summary.violations(), violations.len());
    assert_eq!(
        summary.worst_ratio(),
        violations
            .iter()
            .map(|violation| violation.actual / violation.limit)
            .max_by(f64::total_cmp)
            .unwrap()
    );
    assert_eq!(
        summary.total_excess(),
        violations
            .iter()
            .map(|violation| violation.actual - violation.limit)
            .sum::<f64>()
    );
    let n1_violations = violations
        .iter()
        .filter(|violation| violation.object == "n1")
        .count();
    assert_eq!(n1_violations, 2);
}

#[test]
fn zero_design_rule_limit_keeps_total_excess_finite() {
    let mut timing = TimingContext::new();
    let design = timing_object(1, "top", TimingObjectKind::Design);
    timing.set_max_fanout(0.0, &[design]).unwrap();
    let incremental =
        IncrementalTiming::new(timing, buffer_model(), ReportTimingOptions::default()).unwrap();

    let violations = incremental.design_rule_violations();
    let summary = incremental.design_rule_summary();
    assert!(summary.worst_ratio().is_infinite());
    assert!(summary.total_excess().is_finite());
    assert_eq!(
        summary.total_excess(),
        violations
            .iter()
            .map(|violation| violation.actual - violation.limit)
            .sum::<f64>()
    );
}

#[test]
fn pin_swap_updates_and_rolls_back_graph_loads() {
    let mut incremental = IncrementalTiming::new(
        TimingContext::new(),
        pin_swap_model(),
        ReportTimingOptions::default(),
    )
    .unwrap();
    assert_eq!(incremental.net_state("a").unwrap().capacitance, 10.0);
    assert_eq!(incremental.net_state("b").unwrap().capacitance, 1.0);

    let mut delta = TimingRegionDelta::new();
    delta
        .set_instance(test_instance(
            0,
            "U0",
            "AND2",
            [("A", "b"), ("B", "a"), ("Y", "y")],
        ))
        .unwrap();
    let edit = incremental.apply_region_delta(delta).unwrap();
    assert_eq!(incremental.net_state("a").unwrap().capacitance, 1.0);
    assert_eq!(incremental.net_state("b").unwrap().capacitance, 10.0);

    incremental.rollback(edit).unwrap();
    assert_eq!(incremental.net_state("a").unwrap().capacitance, 10.0);
    assert_eq!(incremental.net_state("b").unwrap().capacitance, 1.0);
}

#[test]
fn clock_scoped_design_rules_distinguish_clock_and_data_paths() {
    let mut data_timing = TimingContext::new();
    data_timing
        .create_clock(
            test_clock_id(100),
            ClockSpec::new("sys", 10.0, vec![test_port_id("clk")], None).unwrap(),
        )
        .unwrap();
    data_timing
        .set_max_capacitance(
            0.05,
            &[timing_object(100, "sys", TimingObjectKind::Clock)],
            DesignRuleScope::DataPath,
        )
        .unwrap();
    let data = IncrementalTiming::new(data_timing, clocked_model(), ReportTimingOptions::default())
        .unwrap();
    let data_objects = data
        .design_rule_violations()
        .into_iter()
        .map(|violation| violation.object)
        .collect::<BTreeSet<_>>();
    assert!(data_objects.contains("q"));
    assert!(!data_objects.contains("clk"));

    let mut clock_timing = TimingContext::new();
    clock_timing
        .create_clock(
            test_clock_id(100),
            ClockSpec::new("sys", 10.0, vec![test_port_id("clk")], None).unwrap(),
        )
        .unwrap();
    clock_timing
        .set_max_capacitance(
            0.05,
            &[timing_object(100, "sys", TimingObjectKind::Clock)],
            DesignRuleScope::ClockPath,
        )
        .unwrap();
    let clock = IncrementalTiming::new(
        clock_timing,
        clocked_model(),
        ReportTimingOptions::default(),
    )
    .unwrap();
    let clock_objects = clock
        .design_rule_violations()
        .into_iter()
        .map(|violation| violation.object)
        .collect::<BTreeSet<_>>();
    assert!(clock_objects.contains("clk"));
    assert!(!clock_objects.contains("q"));
}

#[test]
fn multi_sink_region_edit_rolls_back_all_timing_and_pin_state() {
    let mut incremental = IncrementalTiming::new(
        TimingContext::new(),
        clocked_model(),
        ReportTimingOptions::default(),
    )
    .unwrap();
    let before = incremental.net_state("q").unwrap();
    let sinks = [
        (TimingInstanceId::from_raw(1), "A".to_string()),
        (TimingInstanceId::from_raw(2), "A".to_string()),
    ];

    let mut delta = TimingRegionDelta::new();
    delta
        .set_instance(buf_instance(1, "U_buf", "BUF", "q_branch", "y"))
        .unwrap();
    delta
        .set_instance(buf_instance(2, "U_buf2", "BUF", "q_branch", "y2"))
        .unwrap();
    delta
        .set_instance(buf_instance(3, "U_branch", "BUF", "q", "q_branch"))
        .unwrap();
    let edit = incremental.apply_region_delta(delta).unwrap();
    assert_eq!(incremental.net_state("q").unwrap().fanout, 1.0);
    assert_eq!(incremental.net_state("q_branch").unwrap().fanout, 2.0);
    assert_eq!(
        incremental
            .pin_state(TimingInstanceId::from_raw(2), "A")
            .unwrap()
            .net,
        "q_branch"
    );

    incremental.rollback(edit).unwrap();
    assert_eq!(incremental.net_state("q").unwrap(), before);
    assert!(incremental.net_state("q_branch").is_none());
    for (sink, pin) in sinks {
        assert_eq!(incremental.pin_state(sink, &pin).unwrap().net, "q");
    }
}

#[test]
fn required_times_propagate_backward_through_the_max_delay_cone() {
    let mut timing = TimingContext::new();
    timing
        .set_max_delay(
            0.5,
            Vec::new(),
            vec![TimingEndpoint::Port(test_port_id("y"))],
        )
        .unwrap();
    let incremental =
        IncrementalTiming::new(timing, chain_model(), ReportTimingOptions::default()).unwrap();

    for (net, arrival, required) in [
        ("a", 0.0, 0.2),
        ("n1", 0.1, 0.3),
        ("n2", 0.2, 0.4),
        ("y", 0.3, 0.5),
    ] {
        let state = incremental.net_state(net).unwrap();
        assert!((state.arrival.unwrap() - arrival).abs() < 1e-12, "{net}");
        assert!((state.required.unwrap() - required).abs() < 1e-12, "{net}");
        assert!((state.slack.unwrap() - 0.2).abs() < 1e-12, "{net}");
    }
}

#[test]
fn fanout_required_times_keep_the_tightest_endpoint_requirement() {
    let mut timing = TimingContext::new();
    timing
        .set_max_delay(
            0.5,
            Vec::new(),
            vec![TimingEndpoint::Port(test_port_id("y1"))],
        )
        .unwrap();
    timing
        .set_max_delay(
            0.3,
            Vec::new(),
            vec![TimingEndpoint::Port(test_port_id("y2"))],
        )
        .unwrap();
    let incremental = IncrementalTiming::new(
        timing,
        fanout_required_model(),
        ReportTimingOptions::default(),
    )
    .unwrap();

    for (net, required, slack) in [
        ("a", 0.1, 0.1),
        ("n1", 0.2, 0.1),
        ("y1", 0.5, 0.3),
        ("y2", 0.3, 0.1),
    ] {
        let state = incremental.net_state(net).unwrap();
        assert!((state.required.unwrap() - required).abs() < 1e-12, "{net}");
        assert!((state.slack.unwrap() - slack).abs() < 1e-12, "{net}");
    }
}

#[test]
fn timing_quality_sums_each_violating_endpoint_instead_of_each_path_group() {
    let mut timing = TimingContext::new();
    timing
        .set_max_delay(
            0.15,
            Vec::new(),
            vec![TimingEndpoint::Port(test_port_id("y1"))],
        )
        .unwrap();
    timing
        .set_max_delay(
            0.05,
            Vec::new(),
            vec![TimingEndpoint::Port(test_port_id("y2"))],
        )
        .unwrap();
    let incremental = IncrementalTiming::new(
        timing,
        fanout_required_model(),
        ReportTimingOptions::default(),
    )
    .unwrap();

    let quality = incremental.quality().unwrap();
    assert!((quality.wns().unwrap() - -0.15).abs() < 1e-12);
    assert!((quality.tns() - -0.20).abs() < 1e-12);
    assert_eq!(quality.violating_paths(), 2);
    assert_summary_matches_quality(&incremental);
}

#[test]
fn critical_instance_frontier_contains_the_whole_violating_cone() {
    let mut timing = TimingContext::new();
    timing
        .set_max_delay(
            0.25,
            Vec::new(),
            vec![TimingEndpoint::Port(test_port_id("y"))],
        )
        .unwrap();
    let mut incremental =
        IncrementalTiming::new(timing, chain_model(), ReportTimingOptions::default()).unwrap();

    assert_eq!(
        incremental.instances_with_slack_at_most(0.0).unwrap(),
        [0, 1, 2].map(TimingInstanceId::from_raw)
    );
}

#[test]
fn setup_required_times_flow_backward_from_check_endpoints() {
    let mut timing = TimingContext::new();
    timing
        .create_clock(
            test_clock_id(1),
            ClockSpec::new("sys", 10.0, vec![test_port_id("clk")], None).unwrap(),
        )
        .unwrap();
    let incremental =
        IncrementalTiming::new(timing, setup_chain_model(), ReportTimingOptions::default())
            .unwrap();
    assert_summary_matches_quality(&incremental);

    let data = incremental.net_state("n1").unwrap();
    assert!((data.arrival.unwrap() - 0.2).abs() < 1e-12);
    assert!((data.required.unwrap() - 9.6).abs() < 1e-12);
    assert!((data.slack.unwrap() - 9.4).abs() < 1e-12);
    let launch = incremental.net_state("q1").unwrap();
    assert!((launch.arrival.unwrap() - 0.1).abs() < 1e-12);
    assert!((launch.required.unwrap() - 9.5).abs() < 1e-12);
    assert!((launch.slack.unwrap() - 9.4).abs() < 1e-12);
}

#[test]
fn sequential_arrival_uses_stable_sparse_instance_ids() {
    let mut timing = TimingContext::new();
    timing
        .create_clock(
            test_clock_id(1),
            ClockSpec::new("sys", 10.0, vec![test_port_id("clk")], None).unwrap(),
        )
        .unwrap();
    let incremental = IncrementalTiming::new(
        timing,
        setup_chain_model_with_ids([10, 20, 30]),
        ReportTimingOptions::default(),
    )
    .unwrap();

    assert!((incremental.net_state("q1").unwrap().arrival.unwrap() - 0.1).abs() < 1e-12);
}

#[test]
fn hold_required_times_flow_backward_from_check_endpoints() {
    let mut timing = TimingContext::new();
    timing
        .create_clock(
            test_clock_id(1),
            ClockSpec::new("sys", 10.0, vec![test_port_id("clk")], None).unwrap(),
        )
        .unwrap();
    let incremental = IncrementalTiming::new(
        timing,
        setup_chain_model(),
        ReportTimingOptions {
            delay_type: DelayType::Min,
            ..ReportTimingOptions::default()
        },
    )
    .unwrap();

    let data = incremental.net_state("n1").unwrap();
    assert!((data.arrival.unwrap() - 0.2).abs() < 1e-12);
    assert!((data.required.unwrap() - 0.05).abs() < 1e-12);
    assert!((data.slack.unwrap() - 0.15).abs() < 1e-12);
    let launch = incremental.net_state("q1").unwrap();
    assert!((launch.required.unwrap() - -0.05).abs() < 1e-12);
    assert!((launch.slack.unwrap() - 0.15).abs() < 1e-12);
}

#[test]
fn recovery_and_removal_checks_are_first_class_timing_endpoints() {
    let mut timing = TimingContext::new();
    timing
        .create_clock(
            test_clock_id(1),
            ClockSpec::new("sys", 10.0, vec![test_port_id("clk")], None).unwrap(),
        )
        .unwrap();
    let recovery_checks = crate::ScenarioCheckSet {
        setup: false,
        hold: false,
        recovery: true,
        removal: false,
        pulse_width: false,
        max_transition: false,
        max_capacitance: false,
        max_fanout: false,
    };
    let recovery = IncrementalTiming::new(
        timing.clone(),
        asynchronous_check_model(TimingCheckKind::Recovery, 0.4),
        ReportTimingOptions {
            checks: recovery_checks,
            ..ReportTimingOptions::default()
        },
    )
    .unwrap();
    let recovery_path = recovery.analyze().unwrap();
    assert!(matches!(
        recovery_path.requirement(),
        Some(TimingRequirement::Recovery { .. })
    ));
    assert!((recovery.net_state("async").unwrap().required.unwrap() - 9.6).abs() < 1e-12);
    assert!((recovery.quality_summary().unwrap().wns().unwrap() - 9.5).abs() < 1e-12);

    let removal_checks = crate::ScenarioCheckSet {
        recovery: false,
        removal: true,
        ..recovery_checks
    };
    let removal = IncrementalTiming::new(
        timing,
        asynchronous_check_model(TimingCheckKind::Removal, 0.05),
        ReportTimingOptions {
            delay_type: DelayType::Min,
            checks: removal_checks,
            ..ReportTimingOptions::default()
        },
    )
    .unwrap();
    let removal_path = removal.analyze().unwrap();
    assert!(matches!(
        removal_path.requirement(),
        Some(TimingRequirement::Removal { .. })
    ));
    assert!((removal.net_state("async").unwrap().required.unwrap() - 0.05).abs() < 1e-12);
    assert!((removal.quality_summary().unwrap().wns().unwrap() - 0.05).abs() < 1e-12);
}

#[test]
fn minimum_pulse_width_checks_both_clock_polarities() {
    let mut timing = TimingContext::new();
    timing
        .create_clock(
            test_clock_id(1),
            ClockSpec::new("sys", 10.0, vec![test_port_id("clk")], None).unwrap(),
        )
        .unwrap();
    let checks = crate::ScenarioCheckSet {
        setup: false,
        hold: false,
        recovery: false,
        removal: false,
        pulse_width: true,
        max_transition: false,
        max_capacitance: false,
        max_fanout: false,
    };
    let incremental = IncrementalTiming::new(
        timing,
        pulse_width_model(6.0),
        ReportTimingOptions {
            checks,
            ..ReportTimingOptions::default()
        },
    )
    .unwrap();
    let path = incremental.analyze().unwrap();
    assert!(matches!(
        path.requirement(),
        Some(TimingRequirement::PulseWidth { .. })
    ));
    let quality = incremental.quality_summary().unwrap();
    assert_eq!(quality.wns(), Some(-1.0));
    assert_eq!(quality.tns(), -1.0);
    assert_eq!(quality.violating_paths(), 1);
    assert_summary_matches_quality(&incremental);
}

#[test]
fn incremental_region_edits_update_the_backward_required_cone() {
    let mut timing = TimingContext::new();
    timing
        .set_max_delay(
            0.5,
            Vec::new(),
            vec![TimingEndpoint::Port(test_port_id("y"))],
        )
        .unwrap();
    let base = chain_model();
    let mut replaced = base.design().to_owned();
    replaced.instances[1].cell = "FAST_BUF".to_string();
    let reference = IncrementalTiming::new(
        timing.clone(),
        TimingModel::new(replaced, base.library().clone()).unwrap(),
        ReportTimingOptions::default(),
    )
    .unwrap();

    let mut incremental =
        IncrementalTiming::new(timing, base, ReportTimingOptions::default()).unwrap();
    let before = incremental.net_states();
    let mut delta = TimingRegionDelta::new();
    delta
        .set_instance(buf_instance(1, "U1", "FAST_BUF", "n1", "n2"))
        .unwrap();
    let edit = incremental.apply_region_delta(delta).unwrap();
    assert_eq!(incremental.net_states(), reference.net_states());

    incremental.rollback(edit).unwrap();
    assert_eq!(incremental.net_states(), before);
}

#[test]
fn optimization_edits_synchronize_required_times_after_commit_not_speculation() {
    let mut timing = TimingContext::new();
    timing
        .set_max_delay(
            0.5,
            Vec::new(),
            vec![TimingEndpoint::Port(test_port_id("y"))],
        )
        .unwrap();
    let base = chain_model();
    let mut accepted_design = base.design().to_owned();
    accepted_design.instances[1].cell = "FAST_BUF".to_string();
    let reference = IncrementalTiming::new(
        timing.clone(),
        TimingModel::new(accepted_design, base.library().clone()).unwrap(),
        ReportTimingOptions::default(),
    )
    .unwrap();

    let mut incremental =
        IncrementalTiming::new(timing, base, ReportTimingOptions::default()).unwrap();
    let mut accepted = TimingRegionDelta::new();
    accepted
        .set_instance(buf_instance(1, "U1", "FAST_BUF", "n1", "n2"))
        .unwrap();
    let accepted = incremental
        .apply_optimization_region_delta(accepted)
        .unwrap();
    incremental.commit(accepted).unwrap();
    assert_eq!(
        incremental.quality_summary().unwrap(),
        reference.quality_summary().unwrap()
    );

    let mut rejected = TimingRegionDelta::new();
    rejected
        .set_instance(buf_instance(2, "U2", "FAST_BUF", "n2", "y"))
        .unwrap();
    let rejected = incremental
        .apply_optimization_region_delta(rejected)
        .unwrap();
    incremental.rollback(rejected).unwrap();
    assert_eq!(
        incremental.quality_summary().unwrap(),
        reference.quality_summary().unwrap()
    );

    incremental.instances_with_slack_at_most(0.0).unwrap();
    assert_eq!(incremental.net_states(), reference.net_states());
}

#[test]
fn structural_optimization_remaps_check_endpoints_without_a_global_rebuild() {
    let mut timing = TimingContext::new();
    timing
        .create_clock(
            test_clock_id(100),
            ClockSpec::new("sys", 1.0, vec![test_port_id("clk")], None).unwrap(),
        )
        .unwrap();
    let replacement = test_instance(2, "U_ff2", "DFF", [("CK", "clk"), ("D", "n2"), ("Q", "y")]);
    let base = setup_chain_model();
    let mut rebuilt_design = base.design().to_owned();
    rebuilt_design.instances[2] = replacement.clone();
    rebuilt_design
        .instances
        .push(buf_instance(3, "U_inserted", "BUF", "n1", "n2"));
    let rebuilt = IncrementalTiming::new(
        timing.clone(),
        TimingModel::new(rebuilt_design, base.library().clone()).unwrap(),
        ReportTimingOptions::default(),
    )
    .unwrap();

    for threads in [None, Some(1), Some(4)] {
        let mut incremental = match threads {
            None => IncrementalTiming::new(
                timing.clone(),
                setup_chain_model(),
                ReportTimingOptions::default(),
            )
            .unwrap(),
            Some(max_threads) => IncrementalTiming::new_for_optimization(
                timing.clone(),
                setup_chain_model(),
                ReportTimingOptions::default(),
                opto_runtime::ExecutionContext::new(&opto_runtime::ExecutionConfig { max_threads })
                    .unwrap(),
            )
            .unwrap(),
        };
        let before = incremental.quality_summary().unwrap();
        let mut delta = TimingRegionDelta::new();
        delta
            .set_instance(buf_instance(3, "U_inserted", "BUF", "n1", "n2"))
            .unwrap();
        delta.set_instance(replacement.clone()).unwrap();

        let edit = incremental.apply_optimization_region_delta(delta).unwrap();
        if threads.is_none() {
            assert_summary_matches_quality(&incremental);
        }
        assert_eq!(
            incremental.quality_summary().unwrap(),
            rebuilt.quality_summary().unwrap()
        );
        assert_ne!(incremental.quality_summary().unwrap(), before);

        incremental.rollback(edit).unwrap();
        assert_eq!(incremental.quality_summary().unwrap(), before);
        if threads.is_none() {
            assert_summary_matches_quality(&incremental);
        }
    }
}

#[test]
fn structural_fanout_tree_updates_every_endpoint_in_optimization_mode() {
    let library = TimingLibrary {
        wire_load: Some("test".to_string()),
        wire_load_model: Some(
            opto_library::WireLoadModel::new(
                "test".to_string(),
                0.0,
                1.0,
                0.0,
                vec![(1.0, 1.0), (2.0, 2.0), (4.0, 20.0)],
            )
            .unwrap(),
        ),
        units: opto_library::TimingLibraryUnits {
            time_seconds: Some(1e-9),
            capacitance_farads: Some(1e-15),
            resistance_ohms: Some(1e3),
        },
        cells: test_cells(vec![TimingCell {
            name: "BUF".to_string(),
            arcs: vec![TimingArc::scalar("A", "Y", 0.1)],
            clock_to_q: Vec::new(),
            constraints: Vec::new(),
            pin_capacitance: BTreeMap::from([("A".to_string(), 2.0)]),
        }]),
        ..TimingLibrary::default()
    };
    let outputs = ["y1", "y2", "y3", "y4"];
    let design = TimingDesign {
        id: test_design_id(),
        name: "top".to_string(),
        ports: std::iter::once(test_port("a", TimingPortDirection::Input))
            .chain(outputs.into_iter().map(|name| TimingPort {
                id: test_port_id(name),
                name: name.to_string(),
                net: crate::TimingNet::named(name),
                direction: TimingPortDirection::Output,
            }))
            .collect(),
        instances: std::iter::once(buf_instance(0, "U0", "BUF", "a", "n1"))
            .chain(outputs.into_iter().enumerate().map(|(index, output)| {
                buf_instance(
                    u32::try_from(index + 1).unwrap(),
                    &format!("U{}", index + 1),
                    "BUF",
                    "n1",
                    output,
                )
            }))
            .collect(),
    };
    let mut timing = TimingContext::new();
    timing
        .set_max_delay(
            0.25,
            Vec::new(),
            outputs
                .into_iter()
                .map(|name| TimingEndpoint::Port(test_port_id(name)))
                .collect(),
        )
        .unwrap();

    let replacement_inputs = ["t0", "t0", "t1", "t1"];
    let mut rebuilt_design = design.clone();
    for (instance, input) in rebuilt_design.instances[1..]
        .iter_mut()
        .zip(replacement_inputs)
    {
        instance.connections[0].net = input.to_string();
    }
    rebuilt_design
        .instances
        .push(buf_instance(5, "B0", "BUF", "n1", "t0"));
    rebuilt_design
        .instances
        .push(buf_instance(6, "B1", "BUF", "n1", "t1"));
    let reference = IncrementalTiming::new(
        timing.clone(),
        TimingModel::new(rebuilt_design, library.clone()).unwrap(),
        ReportTimingOptions::default(),
    )
    .unwrap();

    for max_threads in [1, 4] {
        let mut incremental = IncrementalTiming::new_for_optimization(
            timing.clone(),
            TimingModel::new(design.clone(), library.clone()).unwrap(),
            ReportTimingOptions::default(),
            opto_runtime::ExecutionContext::new(&opto_runtime::ExecutionConfig { max_threads })
                .unwrap(),
        )
        .unwrap();
        let before = incremental.quality_summary().unwrap();
        let mut delta = TimingRegionDelta::new();
        for (index, (&output, input)) in outputs.iter().zip(replacement_inputs).enumerate() {
            delta
                .set_instance(buf_instance(
                    u32::try_from(index + 1).unwrap(),
                    &format!("U{}", index + 1),
                    "BUF",
                    input,
                    output,
                ))
                .unwrap();
        }
        delta
            .set_instance(buf_instance(5, "B0", "BUF", "n1", "t0"))
            .unwrap();
        delta
            .set_instance(buf_instance(6, "B1", "BUF", "n1", "t1"))
            .unwrap();

        let edit = incremental.apply_optimization_region_delta(delta).unwrap();
        assert_eq!(
            incremental.quality_summary().unwrap(),
            reference.quality_summary().unwrap()
        );
        assert!(incremental.quality_summary().unwrap().wns().unwrap() > before.wns().unwrap());
        incremental.rollback(edit).unwrap();
        assert_eq!(incremental.quality_summary().unwrap(), before);
    }
}

#[test]
fn optimization_closure_uses_current_slew_dependent_endpoint_requirement() {
    use crate::test_library::TimingConstraintArc;

    let buffer = |name: &str, transition: f64| TimingCell {
        name: name.to_string(),
        arcs: vec![TimingArc {
            from_pin: "A".to_string(),
            to_pin: "Y".to_string(),
            timing_sense: TimingSense::PositiveUnate,
            cell_rise: Some(LookupTable::scalar(0.1)),
            cell_fall: Some(LookupTable::scalar(0.1)),
            rise_transition: Some(LookupTable::scalar(transition)),
            fall_transition: Some(LookupTable::scalar(transition)),
        }],
        ..TimingCell::default()
    };
    let setup_constraint = LookupTable::new(Vec::new(), vec![0.1, 1.0], vec![0.1, 1.0]);
    let library = TimingLibrary {
        cells: test_cells(vec![
            TimingCell {
                name: "DFF".to_string(),
                arcs: Vec::new(),
                clock_to_q: vec![ClockToQArc {
                    clock_edge: TimingEdge::Rise,
                    arc: TimingArc::scalar("CK", "Q", 0.1),
                }],
                constraints: vec![TimingConstraintArc {
                    data_pin: "D".to_string(),
                    clock_pin: "CK".to_string(),
                    clock_edge: TimingEdge::Rise,
                    kind: TimingCheckKind::Setup,
                    rise_constraint: Some(setup_constraint.clone()),
                    fall_constraint: Some(setup_constraint),
                }],
                pin_capacitance: BTreeMap::new(),
            },
            buffer("SLOW", 1.0),
            buffer("FAST", 0.1),
        ]),
        ..TimingLibrary::default()
    };
    let capture = |cell: &str| TimingDesign {
        id: test_design_id(),
        name: "top".to_string(),
        ports: vec![
            test_port("clk", TimingPortDirection::Input),
            test_port("y", TimingPortDirection::Output),
        ],
        instances: vec![
            test_instance(0, "U_launch", "DFF", [("CK", "clk"), ("Q", "q")]),
            buf_instance(1, "U_buf", cell, "q", "d"),
            test_instance(
                2,
                "U_capture",
                "DFF",
                [("CK", "clk"), ("D", "d"), ("Q", "y")],
            ),
        ],
    };
    let mut timing = TimingContext::new();
    timing
        .create_clock(
            test_clock_id(100),
            ClockSpec::new("sys", 1.0, vec![test_port_id("clk")], None).unwrap(),
        )
        .unwrap();
    let reference = IncrementalTiming::new(
        timing.clone(),
        TimingModel::new(capture("FAST"), library.clone()).unwrap(),
        ReportTimingOptions::default(),
    )
    .unwrap();

    for max_threads in [1, 4] {
        let mut incremental = IncrementalTiming::new_for_optimization(
            timing.clone(),
            TimingModel::new(capture("SLOW"), library.clone()).unwrap(),
            ReportTimingOptions::default(),
            opto_runtime::ExecutionContext::new(&opto_runtime::ExecutionConfig { max_threads })
                .unwrap(),
        )
        .unwrap();
        let before = incremental.quality_summary().unwrap();
        let mut delta = TimingRegionDelta::new();
        delta
            .set_instance(buf_instance(1, "U_buf", "FAST", "q", "d"))
            .unwrap();

        let edit = incremental.apply_optimization_region_delta(delta).unwrap();
        assert_eq!(
            incremental.quality_summary().unwrap(),
            reference.quality_summary().unwrap()
        );
        assert!(incremental.quality_summary().unwrap().wns() > before.wns());
        incremental.rollback(edit).unwrap();
        assert_eq!(incremental.quality_summary().unwrap(), before);
    }
}

fn fanout_required_model() -> TimingModel {
    let library = TimingLibrary {
        cells: test_cells(vec![TimingCell {
            name: "BUF".to_string(),
            arcs: vec![TimingArc::scalar("A", "Y", 0.1)],
            ..TimingCell::default()
        }]),
        ..TimingLibrary::default()
    };
    let design = TimingDesign {
        id: test_design_id(),
        name: "top".to_string(),
        ports: vec![
            test_port("a", TimingPortDirection::Input),
            test_port("y1", TimingPortDirection::Output),
            test_port("y2", TimingPortDirection::Output),
        ],
        instances: [
            (0, "U0", "a", "n1"),
            (1, "U1", "n1", "y1"),
            (2, "U2", "n1", "y2"),
        ]
        .into_iter()
        .map(|(id, name, input, output)| buf_instance(id, name, "BUF", input, output))
        .collect(),
    };
    TimingModel::new(design, library).unwrap()
}

fn setup_chain_model() -> TimingModel {
    setup_chain_model_with_ids([0, 1, 2])
}

fn asynchronous_check_model(kind: TimingCheckKind, constraint: f64) -> TimingModel {
    use crate::test_library::TimingConstraintArc;

    let library = TimingLibrary {
        cells: test_cells(vec![TimingCell {
            name: "DFF_ASYNC".to_string(),
            arcs: Vec::new(),
            clock_to_q: vec![ClockToQArc {
                clock_edge: TimingEdge::Rise,
                arc: TimingArc::scalar("CK", "Q", 0.1),
            }],
            constraints: vec![TimingConstraintArc {
                data_pin: "RN".to_string(),
                clock_pin: "CK".to_string(),
                clock_edge: TimingEdge::Rise,
                kind,
                rise_constraint: Some(LookupTable::scalar(constraint)),
                fall_constraint: Some(LookupTable::scalar(constraint)),
            }],
            pin_capacitance: BTreeMap::new(),
        }]),
        ..TimingLibrary::default()
    };
    let design = TimingDesign {
        id: test_design_id(),
        name: "top".to_string(),
        ports: vec![
            test_port("clk", TimingPortDirection::Input),
            test_port("y", TimingPortDirection::Output),
        ],
        instances: vec![
            test_instance(0, "U_launch", "DFF_ASYNC", [("CK", "clk"), ("Q", "async")]),
            test_instance(
                1,
                "U_capture",
                "DFF_ASYNC",
                [("CK", "clk"), ("RN", "async"), ("Q", "y")],
            ),
        ],
    };
    TimingModel::new(design, library).unwrap()
}

fn pulse_width_model(constraint: f64) -> TimingModel {
    let clock_pin = TargetPin {
        name: "CK".to_string(),
        direction: TargetPinDirection::Input,
        function: None,
        three_state: None,
        capacitance: None,
        rise_capacitance: None,
        fall_capacitance: None,
        receiver_capacitance: None,
        fanout_load: None,
        next_state_type: None,
        clock_gate_role: None,
        timing_arcs: vec![TargetTimingArc {
            related_pin: String::new(),
            timing_type: TargetTimingType::MinPulseWidth,
            timing_sense: TimingSense::NonUnate,
            delay_model: None,
            rise_constraint: Some(LookupTable::scalar(constraint)),
            fall_constraint: Some(LookupTable::scalar(constraint)),
        }],
    };
    let library = TimingLibrary {
        cells: vec![TargetCell {
            name: "CLK_SINK".to_string(),
            area: Some(1.0),
            dont_use: false,
            usage: opto_library::TargetCellUsage::default(),
            pins: vec![clock_pin],
            sequential: Vec::new(),
            clock_gate: None,
            memory: None,
        }]
        .into(),
        ..TimingLibrary::default()
    };
    let design = TimingDesign {
        id: test_design_id(),
        name: "top".to_string(),
        ports: vec![test_port("clk", TimingPortDirection::Input)],
        instances: vec![test_instance(0, "U_sink", "CLK_SINK", [("CK", "clk")])],
    };
    TimingModel::new(design, library).unwrap()
}

fn setup_chain_model_with_ids(instance_ids: [u32; 3]) -> TimingModel {
    use crate::test_library::TimingConstraintArc;

    let library = TimingLibrary {
        cells: test_cells(vec![
            TimingCell {
                name: "DFF".to_string(),
                arcs: Vec::new(),
                clock_to_q: vec![ClockToQArc {
                    clock_edge: TimingEdge::Rise,
                    arc: TimingArc::scalar("CK", "Q", 0.1),
                }],
                constraints: vec![
                    TimingConstraintArc {
                        data_pin: "D".to_string(),
                        clock_pin: "CK".to_string(),
                        clock_edge: TimingEdge::Rise,
                        kind: TimingCheckKind::Setup,
                        rise_constraint: Some(LookupTable::scalar(0.4)),
                        fall_constraint: Some(LookupTable::scalar(0.4)),
                    },
                    TimingConstraintArc {
                        data_pin: "D".to_string(),
                        clock_pin: "CK".to_string(),
                        clock_edge: TimingEdge::Rise,
                        kind: TimingCheckKind::Hold,
                        rise_constraint: Some(LookupTable::scalar(0.05)),
                        fall_constraint: Some(LookupTable::scalar(0.05)),
                    },
                ],
                pin_capacitance: BTreeMap::new(),
            },
            TimingCell {
                name: "BUF".to_string(),
                arcs: vec![TimingArc::scalar("A", "Y", 0.1)],
                ..TimingCell::default()
            },
        ]),
        ..TimingLibrary::default()
    };
    let design = TimingDesign {
        id: test_design_id(),
        name: "top".to_string(),
        ports: vec![
            test_port("clk", TimingPortDirection::Input),
            test_port("y", TimingPortDirection::Output),
        ],
        instances: vec![
            test_instance(
                instance_ids[0],
                "U_ff1",
                "DFF",
                [("CK", "clk"), ("Q", "q1")],
            ),
            buf_instance(instance_ids[1], "U_buf", "BUF", "q1", "n1"),
            test_instance(
                instance_ids[2],
                "U_ff2",
                "DFF",
                [("CK", "clk"), ("D", "n1"), ("Q", "y")],
            ),
        ],
    };
    TimingModel::new(design, library).unwrap()
}

fn mapped_chain_fixture() -> (
    opto_ir::mapped::MappedNetlist,
    TimingLibrary,
    crate::PortBindings,
    [opto_ir::mapped::NetId; 4],
) {
    use opto_ir::mapped::{ConnectionSignal, MappedBuilder, PortDirection};

    let library = TimingLibrary {
        cells: test_cells(vec![TimingCell {
            name: "BUF".to_string(),
            arcs: vec![TimingArc::scalar("A", "Y", 0.1)],
            clock_to_q: Vec::new(),
            constraints: Vec::new(),
            pin_capacitance: BTreeMap::from([("A".to_string(), 10.0)]),
        }]),
        ..TimingLibrary::default()
    };
    let mut builder = MappedBuilder::new("top", opto_ir::RevisionId::INITIAL).unwrap();
    let a = builder.add_net(Some("a")).unwrap();
    let mid = builder.add_net(Some("mid")).unwrap();
    let y = builder.add_net(Some("y")).unwrap();
    let spare = builder.add_net(Some("spare")).unwrap();
    builder.add_port("a", PortDirection::Input, &[a]).unwrap();
    builder.add_port("y", PortDirection::Output, &[y]).unwrap();
    builder
        .add_cell(
            "U0",
            "BUF",
            None,
            &[
                ("A".to_string(), None, ConnectionSignal::Net(a)),
                ("Y".to_string(), None, ConnectionSignal::Net(mid)),
            ],
        )
        .unwrap();
    builder
        .add_cell(
            "U1",
            "BUF",
            None,
            &[
                ("A".to_string(), None, ConnectionSignal::Net(mid)),
                ("Y".to_string(), None, ConnectionSignal::Net(y)),
            ],
        )
        .unwrap();
    let mapped = builder.freeze().unwrap();
    let port_bindings = crate::PortBindings::new([test_port_id("a"), test_port_id("y")]);
    (mapped, library, port_bindings, [a, mid, y, spare])
}

fn mapped_incremental(
    mapped: &opto_ir::mapped::MappedNetlist,
    library: TimingLibrary,
    port_bindings: &crate::PortBindings,
    timing: TimingContext,
) -> IncrementalTiming {
    let model = TimingModel::from_mapped(mapped, test_design_id(), port_bindings, library).unwrap();
    IncrementalTiming::new(timing, model, ReportTimingOptions::default()).unwrap()
}

fn mapped_binding(model: &TimingModel, name: &str) -> Option<opto_ir::mapped::NetId> {
    let net = model.graph.net_id(name)?;
    model.mapped_net(crate::TimingNetId::from_index(net).unwrap())
}

#[test]
fn from_mapped_binds_connected_nets_and_reports_mapped_ids_in_violations() {
    let (mapped, library, port_ids, [a, mid, y, spare]) = mapped_chain_fixture();
    let mut timing = TimingContext::new();
    timing
        .set_max_capacitance(
            5.0,
            &[TimingObject::design(test_design_id())],
            DesignRuleScope::All,
        )
        .unwrap();
    let incremental = mapped_incremental(&mapped, library, &port_ids, timing);

    assert_eq!(mapped_binding(&incremental.model, "a"), Some(a));
    assert_eq!(mapped_binding(&incremental.model, "mid"), Some(mid));
    assert_eq!(mapped_binding(&incremental.model, "y"), Some(y));
    assert!(incremental.model.mapped_timing_net(spare).is_none());

    let violation = incremental
        .design_rule_violations()
        .into_iter()
        .find(|violation| violation.object == "a")
        .unwrap();
    assert_eq!(violation.mapped_net, Some(a));
    assert_eq!(
        incremental.model.mapped_net(violation.net),
        violation.mapped_net
    );
}

#[test]
fn region_delta_binds_added_nets_and_cells() {
    use opto_ir::mapped::{CellSpec, ConnectionRef, RegionDelta};

    let (mut mapped, library, port_ids, [_, mid, _, _]) = mapped_chain_fixture();
    let mut incremental = mapped_incremental(&mapped, library, &port_ids, TimingContext::new());

    let snapshot = mapped.snapshot_region([], [mid]).unwrap();
    let mut delta = RegionDelta::new(snapshot);
    let tap = delta.add_net(Some("tap".to_string())).unwrap();
    delta
        .add_cell(
            CellSpec::new("U2", "BUF", None)
                .connect("A", None, ConnectionRef::Net(mid))
                .connect("Y", None, ConnectionRef::NewNet(tap)),
        )
        .unwrap();
    let applied = mapped.apply_region_delta(delta).unwrap();
    let tap = applied.added_net(tap).unwrap();
    let timing_delta =
        TimingRegionDelta::from_mapped_region(&mapped, &applied, incremental.model()).unwrap();
    let edit = incremental.apply_region_delta(timing_delta).unwrap();
    incremental.commit(edit).unwrap();

    assert_eq!(mapped_binding(&incremental.model, "tap"), Some(tap));
    assert_eq!(mapped_binding(&incremental.model, "mid"), Some(mid));
}

#[test]
fn region_delta_rebinds_renamed_nets_and_unbinds_removed_nets() {
    use opto_ir::mapped::{CellId, ConnectionRef, PinId, RegionDelta};

    let (mut mapped, library, port_ids, [_, mid, y, _]) = mapped_chain_fixture();
    let mut incremental = mapped_incremental(&mapped, library, &port_ids, TimingContext::new());
    let cells = [
        CellId::from_index(0).unwrap(),
        CellId::from_index(1).unwrap(),
    ];

    let snapshot = mapped.snapshot_region(cells, [mid]).unwrap();
    let mut delta = RegionDelta::new(snapshot);
    delta
        .rename_net(mid, Some("mid_renamed".to_string()))
        .unwrap();
    let applied = mapped.apply_region_delta(delta).unwrap();
    let timing_delta =
        TimingRegionDelta::from_mapped_region(&mapped, &applied, incremental.model()).unwrap();
    let edit = incremental.apply_region_delta(timing_delta).unwrap();
    incremental.commit(edit).unwrap();

    assert_eq!(mapped_binding(&incremental.model, "mid_renamed"), Some(mid));
    assert_eq!(mapped_binding(&incremental.model, "mid"), None);

    let u0_output = PinId::from_index(1).unwrap();
    let snapshot = mapped.snapshot_region(cells, [mid, y]).unwrap();
    let mut delta = RegionDelta::new(snapshot);
    delta.remove_cell(cells[1]).unwrap();
    delta
        .reconnect_pin(u0_output, ConnectionRef::Net(y))
        .unwrap();
    delta.remove_net(mid).unwrap();
    let applied = mapped.apply_region_delta(delta).unwrap();
    let timing_delta =
        TimingRegionDelta::from_mapped_region(&mapped, &applied, incremental.model()).unwrap();
    let edit = incremental.apply_region_delta(timing_delta).unwrap();
    incremental.commit(edit).unwrap();

    assert!(incremental.model.mapped_timing_net(mid).is_none());
    assert_eq!(mapped_binding(&incremental.model, "mid_renamed"), None);
    assert_eq!(mapped_binding(&incremental.model, "y"), Some(y));
    assert!((incremental.analyze().unwrap().arrival() - 0.1).abs() < 1e-12);
}

#[test]
fn rejected_region_delta_rollback_restores_bindings() {
    use opto_ir::mapped::{CellSpec, ConnectionRef, RegionDelta};

    let (mut mapped, library, port_ids, [_, mid, _, _]) = mapped_chain_fixture();
    let mut incremental = mapped_incremental(&mapped, library, &port_ids, TimingContext::new());
    let before_timing_to_mapped = incremental.model.timing_to_mapped_net.clone();
    let before_mapped_to_timing = incremental.model.mapped_to_timing_net.clone();

    let snapshot = mapped.snapshot_region([], [mid]).unwrap();
    let mut delta = RegionDelta::new(snapshot);
    let tap = delta.add_net(Some("tap".to_string())).unwrap();
    delta
        .add_cell(
            CellSpec::new("U2", "BUF", None)
                .connect("A", None, ConnectionRef::Net(mid))
                .connect("Y", None, ConnectionRef::NewNet(tap)),
        )
        .unwrap();
    let applied = mapped.apply_region_delta(delta).unwrap();
    let timing_delta =
        TimingRegionDelta::from_mapped_region(&mapped, &applied, incremental.model()).unwrap();
    let edit = incremental.apply_region_delta(timing_delta).unwrap();

    incremental.rollback(edit).unwrap();
    mapped.rollback_region_delta(applied).unwrap();

    assert!(incremental.model.graph.net_id("tap").is_none());
    assert_eq!(
        incremental.model.timing_to_mapped_net,
        before_timing_to_mapped
    );
    assert_eq!(
        incremental.model.mapped_to_timing_net,
        before_mapped_to_timing
    );
}

#[test]
fn disconnected_mapped_nets_update_identity_without_timing_propagation() {
    use opto_ir::mapped::RegionDelta;

    let (mut mapped, library, port_ids, [_, _, _, spare]) = mapped_chain_fixture();
    let model = TimingModel::from_mapped(&mapped, test_design_id(), &port_ids, library).unwrap();
    assert!(model.mapped_timing_net(spare).is_none());
    let mut incremental =
        IncrementalTiming::new(TimingContext::new(), model, ReportTimingOptions::default())
            .unwrap();

    let snapshot = mapped.snapshot_region([], [spare]).unwrap();
    let mut delta = RegionDelta::new(snapshot);
    delta
        .rename_net(spare, Some("spare_renamed".to_string()))
        .unwrap();
    let applied = mapped.apply_region_delta(delta).unwrap();
    let timing_delta =
        TimingRegionDelta::from_mapped_region(&mapped, &applied, incremental.model()).unwrap();
    let edit = incremental.apply_region_delta(timing_delta).unwrap();

    assert_eq!(edit.recomputed_nets(), 0);
    assert!(incremental.model.mapped_timing_net(spare).is_none());

    incremental.rollback(edit).unwrap();
    mapped.rollback_region_delta(applied).unwrap();
    assert_eq!(mapped.net_name(spare), Some("spare"));
    assert!(incremental.model.mapped_timing_net(spare).is_none());
}

#[test]
fn region_delta_unbinds_a_live_net_that_becomes_disconnected() {
    use opto_ir::mapped::{CellId, ConnectionRef, PinId, RegionDelta};

    let (mut mapped, library, port_ids, [_, mid, y, _]) = mapped_chain_fixture();
    let mut incremental = mapped_incremental(&mapped, library, &port_ids, TimingContext::new());
    let cells = [
        CellId::from_index(0).unwrap(),
        CellId::from_index(1).unwrap(),
    ];
    assert_eq!(mapped_binding(&incremental.model, "mid"), Some(mid));

    let snapshot = mapped.snapshot_region(cells, [mid, y]).unwrap();
    let mut delta = RegionDelta::new(snapshot);
    delta.remove_cell(cells[1]).unwrap();
    delta
        .reconnect_pin(PinId::from_index(1).unwrap(), ConnectionRef::Net(y))
        .unwrap();
    let applied = mapped.apply_region_delta(delta).unwrap();
    assert!(mapped.is_live_net(mid));
    assert_eq!(mapped.pins_on_net(mid).unwrap().count(), 0);
    let timing_delta =
        TimingRegionDelta::from_mapped_region(&mapped, &applied, incremental.model()).unwrap();
    let edit = incremental.apply_region_delta(timing_delta).unwrap();

    assert!(incremental.model.mapped_timing_net(mid).is_none());
    assert_eq!(mapped_binding(&incremental.model, "mid"), None);

    incremental.rollback(edit).unwrap();
    mapped.rollback_region_delta(applied).unwrap();
    assert_eq!(mapped_binding(&incremental.model, "mid"), Some(mid));
}

#[test]
fn duplicate_mapped_net_names_are_rejected_instead_of_aliased() {
    use opto_ir::mapped::{ConnectionSignal, MappedBuilder, PortDirection};

    let library = TimingLibrary {
        cells: test_cells(vec![TimingCell {
            name: "BUF".to_string(),
            arcs: vec![TimingArc::scalar("A", "Y", 0.1)],
            ..TimingCell::default()
        }]),
        ..TimingLibrary::default()
    };
    let mut builder = MappedBuilder::new("top", opto_ir::RevisionId::INITIAL).unwrap();
    let a = builder.add_net(Some("a")).unwrap();
    let first = builder.add_net(Some("n")).unwrap();
    let second = builder.add_net(Some("n")).unwrap();
    builder.add_port("a", PortDirection::Input, &[a]).unwrap();
    builder
        .add_cell(
            "U0",
            "BUF",
            None,
            &[
                ("A".to_string(), None, ConnectionSignal::Net(a)),
                ("Y".to_string(), None, ConnectionSignal::Net(first)),
            ],
        )
        .unwrap();
    builder
        .add_cell(
            "U1",
            "BUF",
            None,
            &[("A".to_string(), None, ConnectionSignal::Net(second))],
        )
        .unwrap();
    let mapped = builder.freeze().unwrap();
    let port_bindings = crate::PortBindings::new([test_port_id("a")]);

    let error =
        TimingModel::from_mapped(&mapped, test_design_id(), &port_bindings, library).unwrap_err();
    assert!(error.to_string().contains("aliases"), "{error}");
}

fn pin_swap_model() -> TimingModel {
    let arc = |related_pin: &str| TargetTimingArc {
        related_pin: related_pin.to_string(),
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
    };
    let library = TimingLibrary {
        cells: vec![TargetCell {
            dont_use: false,
            usage: opto_library::TargetCellUsage::default(),
            name: "AND2".to_string(),
            area: Some(1.0),
            pins: vec![
                TargetPin {
                    name: "A".to_string(),
                    direction: TargetPinDirection::Input,
                    function: None,
                    three_state: None,
                    capacitance: Some(10.0),
                    rise_capacitance: None,
                    fall_capacitance: None,
                    receiver_capacitance: None,
                    fanout_load: Some(1.0),
                    next_state_type: None,
                    timing_arcs: Vec::new(),
                    clock_gate_role: None,
                },
                TargetPin {
                    name: "B".to_string(),
                    direction: TargetPinDirection::Input,
                    function: None,
                    three_state: None,
                    capacitance: Some(1.0),
                    rise_capacitance: None,
                    fall_capacitance: None,
                    receiver_capacitance: None,
                    fanout_load: Some(1.0),
                    next_state_type: None,
                    timing_arcs: Vec::new(),
                    clock_gate_role: None,
                },
                TargetPin {
                    name: "Y".to_string(),
                    direction: TargetPinDirection::Output,
                    function: Some(BooleanFunction::parse("A&B").unwrap()),
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
            sequential: Vec::new(),
            clock_gate: None,
            memory: None,
        }]
        .into(),
        ..TimingLibrary::default()
    };
    let design = TimingDesign {
        id: test_design_id(),
        name: "top".to_string(),
        ports: vec![
            test_port("a", TimingPortDirection::Input),
            test_port("b", TimingPortDirection::Input),
            test_port("y", TimingPortDirection::Output),
        ],
        instances: vec![test_instance(
            0,
            "U0",
            "AND2",
            [("A", "a"), ("B", "b"), ("Y", "y")],
        )],
    };
    TimingModel::new(design, library).unwrap()
}

fn chain_model() -> TimingModel {
    chain_model_with_ids([0, 1, 2])
}

fn long_chain_model() -> TimingModel {
    let base = chain_model();
    let design = TimingDesign {
        id: test_design_id(),
        name: "top".to_string(),
        ports: vec![
            test_port("a", TimingPortDirection::Input),
            test_port("y", TimingPortDirection::Output),
        ],
        instances: [
            (0, "U0", "a", "n1"),
            (1, "U1", "n1", "n2"),
            (2, "U2", "n2", "n3"),
            (3, "U3", "n3", "y"),
        ]
        .into_iter()
        .map(|(id, name, input, output)| buf_instance(id, name, "BUF", input, output))
        .collect(),
    };
    TimingModel::new(design, base.library).unwrap()
}

fn chain_model_with_ids(ids: [u32; 3]) -> TimingModel {
    let library = TimingLibrary {
        cells: test_cells(vec![
            TimingCell {
                name: "BUF".to_string(),
                arcs: vec![TimingArc {
                    from_pin: "A".to_string(),
                    to_pin: "Y".to_string(),
                    timing_sense: TimingSense::PositiveUnate,
                    cell_rise: Some(LookupTable::new(vec![0.0], vec![0.0, 10.0], vec![0.1, 0.4])),
                    cell_fall: Some(LookupTable::new(vec![0.0], vec![0.0, 10.0], vec![0.1, 0.4])),
                    rise_transition: None,
                    fall_transition: None,
                }],
                ..TimingCell::default()
            },
            TimingCell {
                name: "FAST_BUF".to_string(),
                arcs: vec![TimingArc::scalar("A", "Y", 0.05)],
                ..TimingCell::default()
            },
        ]),
        ..TimingLibrary::default()
    };
    let design = TimingDesign {
        id: test_design_id(),
        name: "top".to_string(),
        ports: vec![
            test_port("a", TimingPortDirection::Input),
            test_port("y", TimingPortDirection::Output),
        ],
        instances: [
            (ids[0], "U0", "a", "n1"),
            (ids[1], "U1", "n1", "n2"),
            (ids[2], "U2", "n2", "y"),
        ]
        .into_iter()
        .map(|(id, name, input, output)| buf_instance(id, name, "BUF", input, output))
        .collect(),
    };
    TimingModel::new(design, library).unwrap()
}

#[test]
fn timing_region_delta_merges_identical_overlaps_and_rejects_conflicts() {
    let instance = buf_instance(1, "U1", "BUF", "a", "y");
    let mut first = TimingRegionDelta::new();
    first.set_instance(instance.clone()).unwrap();
    let mut identical = TimingRegionDelta::new();
    identical.set_instance(instance).unwrap();
    first.merge(identical).unwrap();

    let mut conflicting = TimingRegionDelta::new();
    conflicting
        .set_instance(buf_instance(1, "U1", "FAST_BUF", "a", "y"))
        .unwrap();
    assert!(first.merge(conflicting).is_err());
}

fn buffer_model() -> TimingModel {
    let mut sink_capacitance = BTreeMap::new();
    sink_capacitance.insert("A".to_string(), 10.0);
    let library = TimingLibrary {
        cells: test_cells(vec![
            TimingCell {
                name: "BUF".to_string(),
                arcs: vec![TimingArc {
                    from_pin: "A".to_string(),
                    to_pin: "Y".to_string(),
                    timing_sense: TimingSense::PositiveUnate,
                    cell_rise: Some(LookupTable::new(vec![0.0], vec![0.0, 10.0], vec![0.1, 0.4])),
                    cell_fall: Some(LookupTable::new(vec![0.0], vec![0.0, 10.0], vec![0.1, 0.4])),
                    rise_transition: None,
                    fall_transition: None,
                }],
                clock_to_q: Vec::new(),
                constraints: Vec::new(),
                pin_capacitance: sink_capacitance,
            },
            TimingCell {
                name: "FAST_BUF".to_string(),
                arcs: vec![TimingArc::scalar("A", "Y", 0.05)],
                ..TimingCell::default()
            },
        ]),
        ..TimingLibrary::default()
    };
    let design = TimingDesign {
        id: test_design_id(),
        name: "top".to_string(),
        ports: vec![
            test_port("a", TimingPortDirection::Input),
            test_port("y", TimingPortDirection::Output),
        ],
        instances: [(0, "U0", "a", "n1"), (1, "U1", "n1", "y")]
            .into_iter()
            .map(|(id, name, input, output)| buf_instance(id, name, "BUF", input, output))
            .collect(),
    };
    TimingModel::new(design, library).unwrap()
}

fn clocked_model() -> TimingModel {
    let library = TimingLibrary {
        cells: test_cells(vec![
            TimingCell {
                name: "DFF".to_string(),
                arcs: Vec::new(),
                clock_to_q: vec![ClockToQArc {
                    clock_edge: TimingEdge::Rise,
                    arc: TimingArc::scalar("CK", "Q", 0.1),
                }],
                constraints: Vec::new(),
                pin_capacitance: BTreeMap::from([("CK".to_string(), 0.1), ("D".to_string(), 0.1)]),
            },
            TimingCell {
                name: "BUF".to_string(),
                arcs: vec![TimingArc::scalar("A", "Y", 0.1)],
                clock_to_q: Vec::new(),
                constraints: Vec::new(),
                pin_capacitance: BTreeMap::from([("A".to_string(), 0.1)]),
            },
        ]),
        ..TimingLibrary::default()
    };
    let design = TimingDesign {
        id: test_design_id(),
        name: "top".to_string(),
        ports: vec![
            test_port("clk", TimingPortDirection::Input),
            test_port("d", TimingPortDirection::Input),
            test_port("y", TimingPortDirection::Output),
            test_port("y2", TimingPortDirection::Output),
        ],
        instances: vec![
            test_instance(0, "U_ff", "DFF", [("CK", "clk"), ("D", "d"), ("Q", "q")]),
            buf_instance(1, "U_buf", "BUF", "q", "y"),
            buf_instance(2, "U_buf2", "BUF", "q", "y2"),
        ],
    };
    TimingModel::new(design, library).unwrap()
}
