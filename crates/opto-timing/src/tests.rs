// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Public timing-context state, identity, constraint, and top-level STA tests.
//!
//! Propagation semantics belong to `analysis::tests`; incremental edit and
//! rollback mechanics belong to `engine::tests`.

use super::test_library::{TimingArc, TimingCell, test_cells, test_instance, test_target_cells};
use super::*;
use crate::{
    test_clock_id as clock_id, test_design_id as design_id, test_object_uid as object_uid,
    test_port_id as port_id,
};

fn generation_library(delay: f64) -> TimingLibrary {
    TimingLibrary {
        units: test_library_units(),
        cells: test_cells(vec![TimingCell {
            name: "BUF".to_string(),
            arcs: vec![TimingArc::scalar("A", "Y", delay)],
            ..TimingCell::default()
        }]),
        ..TimingLibrary::default()
    }
}

fn generation_design(design: DesignId, port: PortId) -> TimingDesign {
    TimingDesign {
        id: design,
        name: "top".to_string(),
        ports: vec![TimingPort {
            id: port,
            name: "n".to_string(),
            net: TimingNet::named("n"),
            direction: TimingPortDirection::Input,
        }],
        instances: Vec::new(),
    }
}

fn generation_parasitics(capacitance_farads: f64) -> Parasitics {
    Parasitics::from_rc_networks(
        vec![RcNetwork {
            name: "n".to_string(),
            total_capacitance_farads: capacitance_farads,
            connections: vec![RcConnection {
                node: "driver".to_string(),
                object: "n".to_string(),
                role: RcConnectionRole::Driver,
                pin_capacitance_farads: [0.0; 2],
            }],
            capacitors: vec![RcCapacitor {
                first: "driver".to_string(),
                second: None,
                capacitance_farads,
            }],
            resistors: Vec::new(),
            source_waveforms: [None, None],
        }],
        test_library_units(),
        ParasiticAnalysisOptions::default(),
    )
    .unwrap()
}

#[test]
fn timing_generation_covers_every_analysis_input() {
    let design = generation_design(design_id(), port_id("n"));
    let library = generation_library(0.1);
    let parasitics = generation_parasitics(1e-15);
    let baseline =
        TimingModel::new_with_parasitics(design.clone(), library.clone(), parasitics.clone())
            .unwrap();

    let changed_delay = TimingModel::new_with_parasitics(
        design.clone(),
        generation_library(0.2),
        parasitics.clone(),
    )
    .unwrap();
    let changed_capacitance = TimingModel::new_with_parasitics(
        design.clone(),
        library.clone(),
        generation_parasitics(2e-15),
    )
    .unwrap();
    let mut power_library = library.clone();
    power_library.power.units.nominal_voltage = Some(0.9);
    let changed_power =
        TimingModel::new_with_parasitics(design, power_library, parasitics).unwrap();

    assert_ne!(baseline.generation(), changed_delay.generation());
    assert_ne!(baseline.generation(), changed_capacitance.generation());
    assert_ne!(baseline.generation(), changed_power.generation());
}

#[test]
fn timing_generation_covers_typed_design_and_port_identity() {
    let library = generation_library(0.1);
    let baseline = TimingModel::new(
        generation_design(design_id(), port_id("n")),
        library.clone(),
    )
    .unwrap();
    let changed_design = TimingModel::new(
        generation_design(DesignId::from_uid(object_uid(2)), port_id("n")),
        library.clone(),
    )
    .unwrap();
    let changed_port = TimingModel::new(
        generation_design(design_id(), PortId::from_uid(object_uid(3))),
        library,
    )
    .unwrap();

    assert_ne!(baseline.generation(), changed_design.generation());
    assert_ne!(baseline.generation(), changed_port.generation());
}

#[test]
fn creates_and_reports_clock_sources() {
    let mut timing = TimingContext::new();
    timing
        .create_clock(
            clock_id(100),
            ClockSpec::new("sys_clk", 10.0, vec![port_id("clk")], Some((0.0, 5.0))).unwrap(),
        )
        .unwrap();

    assert_eq!(timing.clock_count(), 1);
    let rows = timing.clock_report(|id| (id == port_id("clk")).then(|| "clk".to_string()));
    assert_eq!(
        rows,
        vec![ClockReportRow {
            name: "sys_clk".to_string(),
            period: 10.0,
            waveform: Some((0.0, 5.0)),
            sources: vec!["clk".to_string()],
        }]
    );
}

#[test]
fn replaces_clock_by_name() {
    let mut timing = TimingContext::new();
    timing
        .create_clock(
            clock_id(100),
            ClockSpec::new("clk", 10.0, Vec::new(), None).unwrap(),
        )
        .unwrap();
    timing
        .create_clock(
            clock_id(100),
            ClockSpec::new("clk", 4.0, Vec::new(), None).unwrap(),
        )
        .unwrap();

    assert_eq!(timing.clock_count(), 1);
    assert_eq!(
        timing.clock_report(|_| None),
        vec![ClockReportRow {
            name: "clk".to_string(),
            period: 4.0,
            waveform: None,
            sources: Vec::new(),
        }]
    );
}

#[test]
fn generated_clock_rejects_a_self_referential_master() {
    let mut timing = TimingContext::new();
    let clock = clock_id(100);
    timing
        .create_clock(
            clock,
            ClockSpec::new("clk", 10.0, vec![port_id("clk")], None).unwrap(),
        )
        .unwrap();
    let error = timing
        .create_generated_clock(
            clock,
            "clk".to_string(),
            vec![port_id("gclk")],
            GeneratedClock {
                master: clock,
                source: port_id("clk"),
                divide_by: Some(2),
                multiply_by: None,
                duty_cycle: None,
                invert: false,
                edges: None,
                edge_shift: None,
                combinational: false,
                comment: String::new(),
            },
            false,
        )
        .unwrap_err();

    assert!(matches!(
        error,
        TimingError::Constraint(ConstraintError::InvalidGeneratedClockOptions)
    ));
}

#[test]
fn synthesis_budget_queries_use_timing_object_matching() {
    let mut timing = TimingContext::new();
    timing
        .create_clock(
            clock_id(100),
            ClockSpec::new("fast", 2.0, vec![port_id("clk_a")], None).unwrap(),
        )
        .unwrap();
    timing
        .create_clock(
            clock_id(101),
            ClockSpec::new("slow", 5.0, vec![port_id("clk_b")], None).unwrap(),
        )
        .unwrap();
    timing
        .set_max_delay(
            3.0,
            Vec::new(),
            vec![TimingEndpoint::Port(port_id("result"))],
        )
        .unwrap();
    timing
        .set_max_delay(
            1.0,
            Vec::new(),
            vec![TimingEndpoint::Port(port_id("other"))],
        )
        .unwrap();

    assert_eq!(timing.minimum_clock_period_on(port_id("clk_a")), Some(2.0));
    assert_eq!(timing.minimum_clock_period_on(port_id("clk_b")), Some(5.0));
    assert_eq!(timing.minimum_synthesis_delay(), Some(1.0));
    assert_eq!(
        timing.minimum_max_delay_to(TimingEndpoint::Port(port_id("result"))),
        Some(3.0)
    );
    assert_eq!(
        timing.minimum_max_delay_to(TimingEndpoint::Port(port_id("unconstrained"))),
        None
    );
}

#[test]
fn clock_transition_tracks_edge_and_delay_type() {
    let mut timing = TimingContext::new();
    timing
        .create_clock(
            clock_id(100),
            ClockSpec::new("clk", 1.0, Vec::new(), None).unwrap(),
        )
        .unwrap();

    timing
        .set_clock_transition(
            0.05,
            EdgeSelection::Rise,
            CornerSelection::Max,
            &[clock_id(100)],
        )
        .unwrap();

    let clock = timing.clocks().into_iter().next().unwrap();
    assert_eq!(
        clock.transition(TimingEdge::Rise, DelayType::Max),
        Some(0.05)
    );
    assert_eq!(clock.transition(TimingEdge::Fall, DelayType::Max), None);
    assert_eq!(clock.transition(TimingEdge::Rise, DelayType::Min), None);
}

#[test]
fn sdc_unset_commands_clear_selected_slots() {
    let mut timing = TimingContext::new();
    let clock = clock_id(100);
    let port = port_id("a");
    timing
        .create_clock(
            clock,
            ClockSpec::new("clk", 10.0, Vec::new(), None).unwrap(),
        )
        .unwrap();
    timing
        .set_clock_transition(0.05, EdgeSelection::Rise, CornerSelection::Max, &[clock])
        .unwrap();
    timing.unset_clock_transition(&[clock]).unwrap();
    assert_eq!(
        timing
            .clocks()
            .into_iter()
            .next()
            .unwrap()
            .transition(TimingEdge::Rise, DelayType::Max),
        None
    );

    timing
        .set_clock_latency(
            -0.1,
            true,
            EdgeSelection::Rise,
            CornerSelection::Max,
            LatencySide::Early,
            &[clock],
        )
        .unwrap();
    timing.unset_clock_latency(true, &[clock]).unwrap();
    assert_eq!(
        timing.clocks().into_iter().next().unwrap().source_latency(
            TimingEdge::Rise,
            DelayType::Max,
            true
        ),
        0.0
    );

    timing
        .set_clock_uncertainty(
            0.2,
            &[clock],
            EdgeSelection::Both,
            &[clock],
            EdgeSelection::Both,
            ExceptionCorner::Setup,
        )
        .unwrap();
    timing
        .unset_clock_uncertainty(
            &[clock],
            EdgeSelection::Rise,
            &[clock],
            EdgeSelection::Both,
            ExceptionCorner::Setup,
        )
        .unwrap();
    assert_eq!(
        timing.clock_uncertainty(
            clock,
            TimingEdge::Rise,
            clock,
            TimingEdge::Rise,
            DelayType::Max,
        ),
        0.0
    );
    assert_eq!(
        timing.clock_uncertainty(
            clock,
            TimingEdge::Fall,
            clock,
            TimingEdge::Rise,
            DelayType::Max,
        ),
        0.2
    );
    timing
        .unset_clock_uncertainty(
            &[clock],
            EdgeSelection::Both,
            &[clock],
            EdgeSelection::Both,
            ExceptionCorner::Setup,
        )
        .unwrap();

    timing
        .set_io_delay(
            IoDelaySpec {
                kind: IoDelayKind::Input,
                delay: 1.0,
                clock: Some(clock),
                clock_edge: TimingEdge::Rise,
                edges: EdgeSelection::Rise,
                corners: CornerSelection::Max,
                source_latency_included: false,
                network_latency_included: false,
                add_delay: false,
            },
            &[port],
        )
        .unwrap();
    timing
        .set_io_delay(
            IoDelaySpec {
                kind: IoDelayKind::Input,
                delay: 2.0,
                clock: Some(clock),
                clock_edge: TimingEdge::Rise,
                edges: EdgeSelection::Fall,
                corners: CornerSelection::Min,
                source_latency_included: false,
                network_latency_included: false,
                add_delay: true,
            },
            &[port],
        )
        .unwrap();
    timing
        .unset_io_delay(
            IoDelayKind::Input,
            Some(clock),
            TimingEdge::Rise,
            EdgeSelection::Rise,
            CornerSelection::Max,
            &[port],
        )
        .unwrap();
    assert_eq!(
        timing.input_delays(port)[0].delay(TimingEdge::Rise, DelayType::Max),
        None
    );
    assert_eq!(
        timing.input_delays(port)[0].delay(TimingEdge::Fall, DelayType::Min),
        Some(2.0)
    );
}

#[test]
fn removed_port_ids_invalidate_every_constraint_without_name_rebinding() {
    let mut timing = TimingContext::new();
    let old_port = port_id("clk");
    let clock = clock_id(100);
    timing
        .create_clock(
            clock,
            ClockSpec::new("clk", 2.0, vec![old_port], None).unwrap(),
        )
        .unwrap();
    timing.set_input_transition(0.1, &[old_port]).unwrap();
    timing.set_load(0.2, &[old_port]).unwrap();
    timing
        .set_max_delay(
            1.0,
            vec![TimingEndpoint::Port(old_port)],
            vec![TimingEndpoint::Clock(clock)],
        )
        .unwrap();
    timing
        .set_max_transition(
            0.3,
            &[TimingObject::port(
                old_port,
                design_id(),
                TimingPortDirection::Input,
            )],
            DesignRuleScope::All,
        )
        .unwrap();

    timing
        .remove_objects(&std::collections::BTreeSet::from([
            opto_db::AnyObjectId::Port(old_port),
        ]))
        .unwrap();

    assert!(
        timing
            .clocks()
            .into_iter()
            .next()
            .unwrap()
            .sources
            .is_empty()
    );
    assert!(timing.input_transitions.is_empty());
    assert!(timing.loads.is_empty());
    assert!(timing.path_exceptions().is_empty());
    assert!(
        timing
            .design_rule_constraints(DesignRuleKind::MaxTransition)
            .is_empty()
    );

    let recreated = PortId::from_uid(object_uid(old_port.uid().get().get() + 1));
    assert_ne!(old_port, recreated);
    assert_eq!(timing.minimum_clock_period_on(recreated), None);
}

#[test]
fn object_removal_visits_only_the_referenced_row() {
    let mut timing = TimingContext::new();
    for raw in 1_000..11_000 {
        let port = PortId::from_uid(object_uid(raw));
        timing
            .set_max_delay(2.0, Vec::new(), vec![TimingEndpoint::Port(port)])
            .unwrap();
    }
    let removed = PortId::from_uid(object_uid(50_000));
    timing
        .set_max_delay(1.0, Vec::new(), vec![TimingEndpoint::Port(removed)])
        .unwrap();

    let prepared = timing
        .prepare_object_removal(&std::collections::BTreeSet::from([
            opto_db::AnyObjectId::Port(removed),
        ]))
        .unwrap();

    assert_eq!(prepared.inspected_rows(), 1);
    timing.apply_object_removal(prepared).unwrap();
    assert_eq!(timing.path_exceptions().len(), 10_000);
}

#[test]
fn object_removal_handles_ten_thousand_reverse_references() {
    let mut timing = TimingContext::new();
    let removed = PortId::from_uid(object_uid(55_000));
    for delay in 0..10_000 {
        timing
            .set_max_delay(
                f64::from(delay),
                Vec::new(),
                vec![TimingEndpoint::Port(removed)],
            )
            .unwrap();
    }

    let prepared = timing
        .prepare_object_removal(&std::collections::BTreeSet::from([
            opto_db::AnyObjectId::Port(removed),
        ]))
        .unwrap();

    assert_eq!(prepared.inspected_rows(), 10_000);
    timing.apply_object_removal(prepared).unwrap();
    assert!(timing.path_exceptions().is_empty());
}

#[test]
fn prepared_object_removal_is_bound_to_one_context_and_revision() {
    let removed = PortId::from_uid(object_uid(56_000));
    let build_context = || {
        let mut timing = TimingContext::new();
        timing
            .set_max_delay(1.0, Vec::new(), vec![TimingEndpoint::Port(removed)])
            .unwrap();
        timing
    };
    let removed_set = std::collections::BTreeSet::from([opto_db::AnyObjectId::Port(removed)]);
    let original = build_context();

    let mut unrelated = build_context();
    let unrelated_before = unrelated.clone();
    let error = unrelated
        .validate_object_removal(original.prepare_object_removal(&removed_set).unwrap())
        .unwrap_err();
    assert!(matches!(error, TimingError::ObjectRemovalOwnerMismatch));
    assert_eq!(unrelated, unrelated_before);

    let mut cloned = original.clone();
    let cloned_before = cloned.clone();
    let error = cloned
        .apply_object_removal(original.prepare_object_removal(&removed_set).unwrap())
        .unwrap_err();
    assert!(matches!(error, TimingError::ObjectRemovalOwnerMismatch));
    assert_eq!(cloned, cloned_before);
    let cloned_token = cloned.prepare_object_removal(&removed_set).unwrap();
    cloned.apply_object_removal(cloned_token).unwrap();
    assert!(cloned.path_exceptions().is_empty());

    let encoded = opto_archive::to_bytes(&original).unwrap();
    let mut restored: TimingContext = opto_archive::from_bytes(&encoded).unwrap();
    let restored_before = restored.clone();
    let error = restored
        .apply_object_removal(original.prepare_object_removal(&removed_set).unwrap())
        .unwrap_err();
    assert!(matches!(error, TimingError::ObjectRemovalOwnerMismatch));
    assert_eq!(restored, restored_before);

    let mut stale = build_context();
    let stale_token = stale.prepare_object_removal(&removed_set).unwrap();
    stale
        .set_load(0.5, &[PortId::from_uid(object_uid(56_001))])
        .unwrap();
    let stale_before = stale.clone();
    let error = stale.validate_object_removal(stale_token).unwrap_err();
    assert!(matches!(error, TimingError::StaleObjectRemoval { .. }));
    assert_eq!(stale, stale_before);
}

#[test]
fn dropping_validated_object_removal_cancels_the_edit_and_releases_the_context() {
    let removed = PortId::from_uid(object_uid(57_000));
    let retained = PortId::from_uid(object_uid(57_001));
    let removed_set = std::collections::BTreeSet::from([opto_db::AnyObjectId::Port(removed)]);
    let mut timing = TimingContext::new();
    timing
        .set_max_delay(1.0, Vec::new(), vec![TimingEndpoint::Port(removed)])
        .unwrap();

    let prepared = timing.prepare_object_removal(&removed_set).unwrap();
    let validated = timing.validate_object_removal(prepared).unwrap();
    drop(validated);

    timing.set_load(0.5, &[retained]).unwrap();
    assert_eq!(timing.path_exceptions().len(), 1);
    assert_eq!(timing.load_on(retained), Some(0.5));

    let prepared = timing.prepare_object_removal(&removed_set).unwrap();
    timing.validate_object_removal(prepared).unwrap().commit();
    assert!(timing.path_exceptions().is_empty());
    assert_eq!(timing.load_on(retained), Some(0.5));
}

#[test]
fn timing_checkpoint_rolls_back_only_touched_rows_in_a_large_context() {
    let mut timing = TimingContext::new();
    for raw in 70_000..80_000 {
        timing
            .set_max_delay(
                2.0,
                Vec::new(),
                vec![TimingEndpoint::Port(PortId::from_uid(object_uid(raw)))],
            )
            .unwrap();
    }
    let target = PortId::from_uid(object_uid(80_001));
    let clock = clock_id(80_002);
    timing
        .create_clock(
            clock,
            ClockSpec::new("checkpoint_clock", 3.0, vec![target], None).unwrap(),
        )
        .unwrap();
    timing.set_input_transition(0.1, &[target]).unwrap();
    timing.set_load(0.2, &[target]).unwrap();
    timing
        .set_max_delay(1.0, Vec::new(), vec![TimingEndpoint::Port(target)])
        .unwrap();
    let object = TimingObject::port(target, design_id(), TimingPortDirection::Input);
    timing
        .set_max_transition(0.3, std::slice::from_ref(&object), DesignRuleScope::All)
        .unwrap();
    timing
        .set_max_capacitance(0.4, std::slice::from_ref(&object), DesignRuleScope::All)
        .unwrap();
    timing.set_max_fanout(4.0, &[object]).unwrap();
    let baseline = timing.clone();
    let baseline_fingerprint = timing.synthesis_fingerprint();
    let baseline_capacity = timing.path_exception_slot_capacity();

    let checkpoint = timing.checkpoint();
    timing
        .remove_objects(&std::collections::BTreeSet::from([
            opto_db::AnyObjectId::Port(target),
        ]))
        .unwrap();
    let added = PortId::from_uid(object_uid(80_003));
    timing.set_load(0.8, &[added]).unwrap();
    timing
        .set_max_delay(0.9, Vec::new(), vec![TimingEndpoint::Port(added)])
        .unwrap();
    timing
        .set_clock_transition(0.05, EdgeSelection::Both, CornerSelection::Both, &[clock])
        .unwrap();

    let (depth, journal_len) = timing.transaction_metrics();
    assert_eq!(depth, 1);
    assert!(
        journal_len <= 10,
        "unexpected inverse journal size {journal_len}"
    );
    timing.rollback_checkpoint(checkpoint).unwrap();

    assert_eq!(timing, baseline);
    assert_eq!(timing.synthesis_fingerprint(), baseline_fingerprint);
    assert_eq!(timing.path_exception_slot_capacity(), baseline_capacity);
    assert_eq!(timing.transaction_metrics(), (0, 0));

    timing
        .remove_objects(&std::collections::BTreeSet::from([
            opto_db::AnyObjectId::Port(target),
        ]))
        .unwrap();
    assert_eq!(timing.path_exceptions().len(), 10_000);
}

#[test]
fn timing_checkpoints_support_nested_commit_and_stale_detection() {
    let mut timing = TimingContext::new();
    let first = PortId::from_uid(object_uid(81_000));
    let second = PortId::from_uid(object_uid(81_001));
    let baseline = timing.clone();

    let outer = timing.checkpoint();
    timing.set_load(0.1, &[first]).unwrap();
    let inner = timing.checkpoint();
    timing.set_input_transition(0.2, &[second]).unwrap();
    timing.commit_checkpoint(inner).unwrap();
    assert_eq!(timing.transaction_metrics(), (1, 2));
    timing.rollback_checkpoint(outer).unwrap();
    assert_eq!(timing, baseline);
    assert_eq!(timing.transaction_metrics(), (0, 0));

    let committed = timing.checkpoint();
    timing.set_load(0.25, &[first]).unwrap();
    timing.commit_checkpoint(committed).unwrap();
    assert_eq!(timing.load_on(first), Some(0.25));
    assert_eq!(timing.transaction_metrics(), (0, 0));

    let outer = timing.checkpoint();
    timing.set_load(0.3, &[first]).unwrap();
    let stale_inner = timing.checkpoint();
    timing.set_load(0.4, &[second]).unwrap();
    timing.rollback_checkpoint(outer).unwrap();
    let before = timing.clone();
    let error = timing.rollback_checkpoint(stale_inner).unwrap_err();
    assert!(matches!(error, TimingError::StaleCheckpoint));
    assert_eq!(timing, before);

    let foreign = TimingContext::new();
    let foreign_before = foreign.clone();
    let checkpoint = timing.checkpoint();
    let error = foreign.validate_checkpoint(&checkpoint).unwrap_err();
    assert!(matches!(error, TimingError::CheckpointOwnerMismatch));
    assert_eq!(foreign, foreign_before);
    timing.rollback_checkpoint(checkpoint).unwrap();
    assert!(std::mem::size_of::<TimingCheckpoint>() <= 64);
}

#[test]
fn timing_rows_reuse_slots_and_serde_rebuilds_reverse_references() {
    let mut timing = TimingContext::new();
    let first = PortId::from_uid(object_uid(60_000));
    let second = PortId::from_uid(object_uid(60_001));
    let third = PortId::from_uid(object_uid(60_002));
    for (delay, port) in [(1.0, first), (2.0, second)] {
        timing
            .set_max_delay(delay, Vec::new(), vec![TimingEndpoint::Port(port)])
            .unwrap();
    }
    timing
        .remove_objects(&std::collections::BTreeSet::from([
            opto_db::AnyObjectId::Port(first),
        ]))
        .unwrap();
    timing
        .set_max_delay(3.0, Vec::new(), vec![TimingEndpoint::Port(third)])
        .unwrap();

    assert_eq!(timing.path_exception_slot_capacity(), 2);
    assert_eq!(
        timing
            .path_exceptions()
            .into_iter()
            .map(|constraint| match constraint.kind {
                PathExceptionKind::MaxDelay { delay } => delay,
                _ => panic!("test inserted only maximum-delay exceptions"),
            })
            .collect::<Vec<_>>(),
        vec![2.0, 3.0]
    );
    let fingerprint = timing.synthesis_fingerprint();
    let encoded = opto_archive::to_bytes(&timing).unwrap();
    let mut restored: TimingContext = opto_archive::from_bytes(&encoded).unwrap();
    assert_eq!(restored, timing);
    assert_eq!(restored.synthesis_fingerprint(), fingerprint);

    restored
        .remove_objects(&std::collections::BTreeSet::from([
            opto_db::AnyObjectId::Port(second),
        ]))
        .unwrap();
    assert_eq!(
        restored
            .path_exceptions()
            .into_iter()
            .next()
            .unwrap()
            .to
            .objects(),
        &[TimingEndpoint::Port(third)]
    );
}

#[test]
fn rejects_invalid_clock_periods() {
    let err = ClockSpec::new("clk", 0.0, Vec::new(), None).unwrap_err();
    assert!(matches!(
        &err,
        TimingError::Constraint(ConstraintError::InvalidClockPeriod { period })
            if *period == 0.0
    ));
    assert_eq!(err.to_string(), "create_clock: invalid period '0'");
}

#[test]
fn reports_max_combinational_timing_path() {
    let mut timing = TimingContext::new();
    let library = TimingLibrary {
        name: Some("demo_lib".to_string()),
        operating_conditions: Some("typical".to_string()),
        wire_load: Some("ZeroWireload".to_string()),
        wire_load_mode: Some("segmented".to_string()),
        wire_load_model: None,
        units: TimingLibraryUnits::default(),
        power: opto_library::PowerLibrary::default(),
        cells: test_cells(vec![TimingCell {
            name: "AND2".to_string(),
            arcs: vec![
                TimingArc::scalar("A", "Y", 0.10),
                TimingArc::scalar("B", "Y", 0.25),
            ],
            ..TimingCell::default()
        }]),
    };
    let design = TimingDesign {
        id: design_id(),
        name: "top".to_string(),
        ports: vec![
            test_port("a", TimingPortDirection::Input),
            test_port("b", TimingPortDirection::Input),
            test_port("y", TimingPortDirection::Output),
        ],
        instances: vec![test_instance(
            0,
            "U1",
            "AND2",
            [("A", "a"), ("B", "b"), ("Y", "y")],
        )],
    };

    let analysis = test_analyze_timing(
        &timing,
        &test_timing_model(&design, &library),
        &ReportTimingOptions::default(),
    );

    assert_eq!(analysis.startpoint(), "b");
    assert_eq!(analysis.startpoint_description(), "input port");
    assert_eq!(analysis.endpoint(), "y");
    assert_eq!(analysis.endpoint_description(), "output port");
    assert_eq!(analysis.design(), "top");
    assert_eq!(analysis.library().name(), Some("demo_lib"));
    assert_eq!(analysis.library().operating_conditions(), Some("typical"));
    assert_eq!(analysis.library().wire_load(), Some("ZeroWireload"));
    assert_eq!(analysis.library().wire_load_mode(), Some("segmented"));
    for point in ["input external delay", "b (in)", "U1/Y (AND2)"] {
        assert!(analysis.steps().iter().any(|step| step.point() == point));
    }
    assert_eq!(analysis.endpoint_object(), "y");
    assert!((analysis.arrival() - 0.25).abs() < 1e-12);

    let case_endpoint = TimingEndpoint::Port(port_id("b"));
    timing
        .set_case_analysis(CaseAnalysisValue::Zero, &[case_endpoint])
        .unwrap();
    let case_analysis = test_analyze_timing(
        &timing,
        &test_timing_model(&design, &library),
        &ReportTimingOptions::default(),
    );
    assert_eq!(case_analysis.startpoint(), "a");
    assert!((case_analysis.arrival() - 0.1).abs() < 1e-12);
    timing.unset_case_analysis(&[case_endpoint]).unwrap();

    let cell = CellId::from_uid(object_uid(90));
    let mut bindings = TimingObjectBindings::builder();
    bindings.bind_cell("U1", cell).unwrap();
    let bindings = bindings.finish().unwrap();
    let mut disabled_model = test_timing_model(&design, &library);
    disabled_model.set_object_bindings(bindings);
    let disabled = DisabledTiming {
        target: TimingEndpoint::Cell(cell),
        from: Some("B".to_string()),
        to: Some("Y".to_string()),
    };
    timing
        .set_disable_timing(std::slice::from_ref(&disabled))
        .unwrap();
    let disabled_analysis =
        test_analyze_timing(&timing, &disabled_model, &ReportTimingOptions::default());
    assert_eq!(disabled_analysis.startpoint(), "a");
    assert!((disabled_analysis.arrival() - 0.1).abs() < 1e-12);
    timing
        .unset_disable_timing(std::slice::from_ref(&disabled))
        .unwrap();

    timing
        .set_timing_derate(
            2.0,
            LatencySide::Late,
            EdgeSelection::Both,
            DesignRuleScope::DataPath,
            &[TimingDerateKind::CellDelay],
        )
        .unwrap();
    let derated = test_analyze_timing(
        &timing,
        &test_timing_model(&design, &library),
        &ReportTimingOptions::default(),
    );
    assert_eq!(derated.startpoint(), "b");
    assert!((derated.arrival() - 0.5).abs() < 1e-12);
    timing.unset_timing_derate().unwrap();

    timing
        .set_max_delay(
            0.2,
            vec![TimingEndpoint::Port(port_id("b"))],
            vec![TimingEndpoint::Port(port_id("y"))],
        )
        .unwrap();
    let analysis = test_analyze_timing(
        &timing,
        &test_timing_model(&design, &library),
        &ReportTimingOptions::default(),
    );
    assert_eq!(analysis.startpoint(), "b");
    assert_eq!(analysis.endpoint(), "y");
    assert_eq!(analysis.arrival(), 0.25);
    assert_eq!(analysis.required(), Some(0.2));
    assert!((analysis.slack().unwrap() + 0.05).abs() < 1e-12);

    timing
        .set_path_exception(PathException {
            kind: PathExceptionKind::MinDelay { delay: 0.3 },
            from: ExceptionFilter::new([TimingEndpoint::Port(port_id("b"))]),
            through: Vec::new().into_boxed_slice(),
            to: ExceptionFilter::new([TimingEndpoint::Port(port_id("y"))]),
            edges: EdgeQualifier::default(),
            corner: ExceptionCorner::Hold,
            ignore_clock_latency: false,
            comment: "minimum path budget".to_string(),
        })
        .unwrap();
    let analysis = test_analyze_timing(
        &timing,
        &test_timing_model(&design, &library),
        &ReportTimingOptions {
            delay_type: DelayType::Min,
            ..ReportTimingOptions::default()
        },
    );
    assert_eq!(analysis.startpoint(), "b");
    assert_eq!(analysis.required(), Some(0.3));
    assert!((analysis.slack().unwrap() + 0.05).abs() < 1e-12);
    assert!(matches!(
        analysis.path_exception().map(TimingPathException::kind),
        Some(PathExceptionKind::MinDelay { delay }) if *delay == 0.3
    ));

    timing
        .set_path_exception(PathException {
            kind: PathExceptionKind::FalsePath,
            from: ExceptionFilter::new([TimingEndpoint::Port(port_id("b"))]),
            through: Vec::new().into_boxed_slice(),
            to: ExceptionFilter::new([TimingEndpoint::Port(port_id("y"))]),
            edges: EdgeQualifier::default(),
            corner: ExceptionCorner::Setup,
            ignore_clock_latency: false,
            comment: "exclude b".to_string(),
        })
        .unwrap();
    let analysis = test_analyze_timing(
        &timing,
        &test_timing_model(&design, &library),
        &ReportTimingOptions::default(),
    );
    assert_eq!(analysis.startpoint(), "a");
    assert!(analysis.path_exception().is_none());
}

#[test]
fn wire_load_resistance_uses_liberty_units() {
    let library = TimingLibrary {
        wire_load: Some("wl".to_string()),
        wire_load_model: Some(
            WireLoadModel::new("wl".to_string(), 0.0, 1.0, 1.0, vec![(1.0, 1.0)]).unwrap(),
        ),
        units: TimingLibraryUnits {
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
    let design = TimingDesign {
        id: design_id(),
        name: "top".to_string(),
        ports: vec![
            test_port("a", TimingPortDirection::Input),
            test_port("y", TimingPortDirection::Output),
        ],
        instances: vec![test_instance(0, "U1", "BUF", [("A", "a"), ("Y", "y")])],
    };

    let analysis = test_analyze_timing(
        &TimingContext::new(),
        &test_timing_model(&design, &library),
        &ReportTimingOptions::default(),
    );

    assert!((analysis.arrival() - 0.102).abs() < 1e-12);
}

#[test]
fn aliased_output_reports_its_endpoint_specific_requirement() {
    let shared = crate::TimingNet::named("shared");
    let design = TimingDesign {
        id: design_id(),
        name: "top".to_string(),
        ports: vec![
            TimingPort {
                id: port_id("a"),
                name: "a".to_string(),
                net: shared.clone(),
                direction: TimingPortDirection::Input,
            },
            TimingPort {
                id: port_id("tight"),
                name: "tight".to_string(),
                net: shared.clone(),
                direction: TimingPortDirection::Output,
            },
            TimingPort {
                id: port_id("relaxed"),
                name: "relaxed".to_string(),
                net: shared,
                direction: TimingPortDirection::Output,
            },
        ],
        instances: Vec::new(),
    };
    let mut timing = TimingContext::new();
    timing
        .set_max_delay(
            20.0,
            Vec::new(),
            vec![TimingEndpoint::Port(port_id("tight"))],
        )
        .unwrap();
    timing
        .set_max_delay(
            50.0,
            Vec::new(),
            vec![TimingEndpoint::Port(port_id("relaxed"))],
        )
        .unwrap();

    let analysis = test_analyze_timing(
        &timing,
        &test_timing_model(&design, &generation_library(0.1)),
        &ReportTimingOptions {
            to: vec!["relaxed".to_string()],
            ..ReportTimingOptions::default()
        },
    );

    assert_eq!(analysis.endpoint(), "relaxed");
    assert_eq!(analysis.required(), Some(50.0));
    assert_eq!(analysis.slack(), Some(50.0));
}

#[test]
fn propagates_ccs_scalar_delay_through_sta() {
    let current = SampledWaveformGrid::new(
        "CCS",
        vec![1.0],
        vec![1.0],
        vec![SampledWaveform {
            reference_time: 0.0,
            coordinates: vec![0.0, 1.0],
            values: vec![1.0, 1.0],
        }],
    )
    .unwrap();
    let model = ArcDelayModel::Ccs(
        CcsTimingModel::new(
            TimingThresholds::default(),
            1.0,
            NldmTimingModel::new(
                Some(LookupTable::scalar(0.5)),
                None,
                Some(LookupTable::scalar(0.6)),
                None,
            ),
            ReceiverCapacitanceModel::default(),
            Some(current),
            None,
        )
        .unwrap(),
    );
    let library = TimingLibrary {
        cells: vec![TargetCell {
            name: "BUF".to_string(),
            area: Some(1.0),
            dont_use: false,
            usage: opto_library::TargetCellUsage::default(),
            pins: vec![
                TargetPin {
                    name: "A".to_string(),
                    direction: TargetPinDirection::Input,
                    function: None,
                    three_state: None,
                    capacitance: Some(1.0),
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
                    function: Some(opto_library::BooleanFunction::parse("A").unwrap()),
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
                        delay_model: Some(model),
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
        .into(),
        ..TimingLibrary::default()
    };
    let design = TimingDesign {
        id: design_id(),
        name: "top".to_string(),
        ports: vec![
            test_port("a", TimingPortDirection::Input),
            test_port("y", TimingPortDirection::Output),
        ],
        instances: vec![test_instance(0, "U1", "BUF", [("A", "a"), ("Y", "y")])],
    };

    let analysis = test_analyze_timing(
        &TimingContext::new(),
        &test_timing_model(&design, &library),
        &ReportTimingOptions::default(),
    );

    assert!((analysis.arrival() - 0.5).abs() < 1e-12);
}

#[test]
fn lumped_sta_uses_static_pin_capacitance_instead_of_ccs_receiver_capacitance() {
    let mut cells = test_target_cells(vec![
        TimingCell {
            name: "SRC".to_string(),
            arcs: vec![TimingArc {
                from_pin: "A".to_string(),
                to_pin: "Y".to_string(),
                timing_sense: TimingSense::PositiveUnate,
                cell_rise: Some(LookupTable::new(
                    Vec::new(),
                    vec![0.0, 20.0],
                    vec![0.1, 0.5],
                )),
                cell_fall: None,
                rise_transition: Some(LookupTable::scalar(1.0)),
                fall_transition: None,
            }],
            ..TimingCell::default()
        },
        TimingCell {
            name: "SINK".to_string(),
            arcs: vec![TimingArc::scalar("A", "Y", 0.1)],
            clock_to_q: Vec::new(),
            constraints: Vec::new(),
            pin_capacitance: BTreeMap::from([("A".to_string(), 1.0)]),
        },
    ]);
    let receiver = ReceiverCapacitanceModel {
        segment_1_rise: Some(LookupTable::scalar(10.0)),
        segment_1_fall: None,
        segment_2_rise: Some(LookupTable::scalar(10.0)),
        segment_2_fall: None,
    };
    cells[1]
        .pins
        .iter_mut()
        .find(|pin| pin.name == "A")
        .unwrap()
        .receiver_capacitance = Some(PinReceiverCapacitanceModel::Ccs(receiver));
    let library = TimingLibrary {
        cells: cells.into(),
        ..TimingLibrary::default()
    };
    let design = TimingDesign {
        id: design_id(),
        name: "top".to_string(),
        ports: vec![
            test_port("a", TimingPortDirection::Input),
            test_port("y", TimingPortDirection::Output),
        ],
        instances: vec![
            test_instance(0, "U1", "SRC", [("A", "a"), ("Y", "n")]),
            test_instance(1, "U2", "SINK", [("A", "n"), ("Y", "y")]),
        ],
    };

    let analysis = test_analyze_timing(
        &TimingContext::new(),
        &test_timing_model(&design, &library),
        &ReportTimingOptions::default(),
    );

    let source = analysis
        .steps()
        .iter()
        .find(|step| step.point() == "U1/Y (SRC)")
        .unwrap();
    assert!((source.path() - 0.12).abs() < 1e-12);
}

#[test]
fn elmore_parasitics_add_sink_delay_to_sta() {
    let library = TimingLibrary {
        units: TimingLibraryUnits {
            time_seconds: Some(1e-12),
            capacitance_farads: Some(1e-15),
            resistance_ohms: None,
        },
        cells: test_cells(vec![
            TimingCell {
                name: "BUF".to_string(),
                arcs: vec![TimingArc::scalar("A", "Y", 0.1)],
                ..TimingCell::default()
            },
            TimingCell {
                name: "SINK".to_string(),
                arcs: vec![TimingArc::scalar("A", "Y", 0.1)],
                clock_to_q: Vec::new(),
                constraints: Vec::new(),
                pin_capacitance: BTreeMap::from([("A".to_string(), 1.0)]),
            },
        ]),
        ..TimingLibrary::default()
    };
    let design = TimingDesign {
        id: design_id(),
        name: "top".to_string(),
        ports: vec![
            test_port("a", TimingPortDirection::Input),
            test_port("z", TimingPortDirection::Output),
        ],
        instances: vec![
            test_instance(0, "U1", "BUF", [("A", "a"), ("Y", "n")]),
            test_instance(1, "U2", "SINK", [("A", "n"), ("Y", "z")]),
        ],
    };
    let parasitics = Parasitics::from_rc_networks(
        vec![RcNetwork {
            name: "n".to_string(),
            total_capacitance_farads: 1e-15,
            connections: vec![
                RcConnection {
                    node: "U1:Y".to_string(),
                    object: "U1/Y".to_string(),
                    role: RcConnectionRole::Driver,
                    pin_capacitance_farads: [0.0; 2],
                },
                RcConnection {
                    node: "U2:A".to_string(),
                    object: "U2/A".to_string(),
                    role: RcConnectionRole::Sink,
                    pin_capacitance_farads: [1e-15; 2],
                },
            ],
            capacitors: vec![RcCapacitor {
                first: "U2:A".to_string(),
                second: None,
                capacitance_farads: 1e-15,
            }],
            resistors: vec![RcResistor {
                first: "U1:Y".to_string(),
                second: "U2:A".to_string(),
                resistance_ohms: 1e3,
            }],
            source_waveforms: [None, None],
        }],
        library.units,
        ParasiticAnalysisOptions {
            delay_model: ParasiticDelayModel::Elmore,
            ..ParasiticAnalysisOptions::default()
        },
    )
    .unwrap();
    let model = TimingModel::new_with_parasitics(design, library, parasitics).unwrap();

    let analysis = test_analyze_timing(
        &TimingContext::new(),
        &model,
        &ReportTimingOptions::default(),
    );

    assert!((analysis.arrival() - 2.2).abs() < 1e-12);
    let sink_step = analysis
        .arrival
        .steps
        .iter()
        .find(|step| step.point == "U2/A (SINK)")
        .expect("parasitic delay must be reported at the sink pin");
    assert!((sink_step.incr - 2.0).abs() < 1e-12);
}

#[test]
fn bus_constraints_apply_to_expanded_port_bits() {
    let mut timing = TimingContext::new();
    timing.set_input_transition(1.0, &[port_id("a")]).unwrap();
    timing.set_load(10.0, &[port_id("y")]).unwrap();
    timing
        .set_max_delay(
            0.3,
            vec![TimingEndpoint::Port(port_id("a"))],
            vec![TimingEndpoint::Port(port_id("y"))],
        )
        .unwrap();
    let library = TimingLibrary {
        cells: test_cells(vec![TimingCell {
            name: "BUF".to_string(),
            arcs: vec![TimingArc {
                from_pin: "A".to_string(),
                to_pin: "Y".to_string(),
                timing_sense: TimingSense::PositiveUnate,
                cell_rise: Some(LookupTable::new(
                    vec![0.0, 1.0],
                    vec![0.0, 10.0],
                    vec![0.1, 0.2, 0.3, 0.4],
                )),
                cell_fall: None,
                rise_transition: None,
                fall_transition: None,
            }],
            ..TimingCell::default()
        }]),
        ..TimingLibrary::default()
    };
    let design = TimingDesign {
        id: design_id(),
        name: "top".to_string(),
        ports: vec![
            TimingPort {
                id: port_id("a"),
                name: "a[3]".to_string(),
                net: crate::TimingNet::named("a[3]"),
                direction: TimingPortDirection::Input,
            },
            TimingPort {
                id: port_id("y"),
                name: "y[3]".to_string(),
                net: crate::TimingNet::named("y[3]"),
                direction: TimingPortDirection::Output,
            },
        ],
        instances: vec![test_instance(
            0,
            "U1",
            "BUF",
            [("A", "a[3]"), ("Y", "y[3]")],
        )],
    };

    let analysis = test_analyze_timing(
        &timing,
        &test_timing_model(&design, &library),
        &ReportTimingOptions::default(),
    );

    assert_eq!(analysis.arrival(), 0.4);
    assert_eq!(analysis.required(), Some(0.3));
    assert!((analysis.slack().unwrap() + 0.1).abs() < 1e-12);
}

#[test]
fn report_timing_uses_input_transition_and_load_constraints() {
    let mut timing = TimingContext::new();
    timing.set_input_transition(1.0, &[port_id("a")]).unwrap();
    timing.set_load(10.0, &[port_id("y")]).unwrap();
    let library = TimingLibrary {
        cells: test_cells(vec![TimingCell {
            name: "BUF".to_string(),
            arcs: vec![TimingArc {
                from_pin: "A".to_string(),
                to_pin: "Y".to_string(),
                timing_sense: TimingSense::PositiveUnate,
                cell_rise: Some(LookupTable::new(
                    vec![0.0, 1.0],
                    vec![0.0, 10.0],
                    vec![0.1, 0.2, 0.3, 0.4],
                )),
                cell_fall: None,
                rise_transition: None,
                fall_transition: None,
            }],
            ..TimingCell::default()
        }]),
        ..TimingLibrary::default()
    };
    let design = TimingDesign {
        id: design_id(),
        name: "top".to_string(),
        ports: vec![
            test_port("a", TimingPortDirection::Input),
            test_port("y", TimingPortDirection::Output),
        ],
        instances: vec![test_instance(0, "U1", "BUF", [("A", "a"), ("Y", "y")])],
    };

    let analysis = test_analyze_timing(
        &timing,
        &test_timing_model(&design, &library),
        &ReportTimingOptions::default(),
    );

    assert!((analysis.arrival() - 0.4).abs() < 1e-12);
}

#[test]
fn report_timing_uses_downstream_pin_capacitance_as_load() {
    let timing = TimingContext::new();
    let library = TimingLibrary {
        cells: test_cells(vec![
            TimingCell {
                name: "SRC".to_string(),
                arcs: vec![TimingArc {
                    from_pin: "A".to_string(),
                    to_pin: "Y".to_string(),
                    timing_sense: TimingSense::PositiveUnate,
                    cell_rise: Some(LookupTable::new(vec![0.0], vec![0.0, 20.0], vec![0.1, 0.5])),
                    cell_fall: None,
                    rise_transition: None,
                    fall_transition: None,
                }],
                clock_to_q: Vec::new(),
                constraints: Vec::new(),
                pin_capacitance: BTreeMap::from([("A".to_string(), 1.0)]),
            },
            TimingCell {
                name: "SINK".to_string(),
                arcs: vec![TimingArc::scalar("A", "Y", 0.1)],
                clock_to_q: Vec::new(),
                constraints: Vec::new(),
                pin_capacitance: BTreeMap::from([("A".to_string(), 10.0)]),
            },
        ]),
        ..TimingLibrary::default()
    };
    let design = TimingDesign {
        id: design_id(),
        name: "top".to_string(),
        ports: vec![
            test_port("a", TimingPortDirection::Input),
            test_port("y1", TimingPortDirection::Output),
            test_port("y2", TimingPortDirection::Output),
        ],
        instances: vec![
            test_instance(0, "U1", "SRC", [("A", "a"), ("Y", "n")]),
            test_instance(1, "U2", "SINK", [("A", "n"), ("Y", "y1")]),
            test_instance(2, "U3", "SINK", [("A", "n"), ("Y", "y2")]),
        ],
    };

    let analysis = test_analyze_timing(
        &timing,
        &test_timing_model(&design, &library),
        &ReportTimingOptions {
            from: vec!["a".to_string()],
            to: vec!["y1".to_string()],
            ..ReportTimingOptions::default()
        },
    );

    let source = analysis
        .steps()
        .iter()
        .find(|step| step.point() == "U1/Y (SRC)")
        .unwrap();
    assert!((source.path() - 0.5).abs() < 1e-12);
}

#[test]
fn report_timing_propagates_negative_unate_edges() {
    let timing = TimingContext::new();
    let library = TimingLibrary {
        cells: test_cells(vec![TimingCell {
            name: "INV".to_string(),
            arcs: vec![TimingArc {
                from_pin: "A".to_string(),
                to_pin: "Y".to_string(),
                timing_sense: TimingSense::NegativeUnate,
                cell_rise: None,
                cell_fall: Some(LookupTable::scalar(0.3)),
                rise_transition: None,
                fall_transition: Some(LookupTable::scalar(0.7)),
            }],
            ..TimingCell::default()
        }]),
        ..TimingLibrary::default()
    };
    let design = TimingDesign {
        id: design_id(),
        name: "top".to_string(),
        ports: vec![
            test_port("a", TimingPortDirection::Input),
            test_port("y", TimingPortDirection::Output),
        ],
        instances: vec![test_instance(0, "U1", "INV", [("A", "a"), ("Y", "y")])],
    };

    let analysis = test_analyze_timing(
        &timing,
        &test_timing_model(&design, &library),
        &ReportTimingOptions::default(),
    );

    assert!((analysis.arrival() - 0.3).abs() < 1e-12);
    let inverter = analysis
        .steps()
        .iter()
        .find(|step| step.point() == "U1/Y (INV)")
        .unwrap();
    assert_eq!(inverter.edge(), TimingEdge::Fall);
    assert_eq!(analysis.endpoint_object(), "y");
    assert_eq!(analysis.endpoint_edge(), TimingEdge::Fall);
}

#[test]
fn report_timing_propagates_arc_transition_to_downstream_delay() {
    let timing = TimingContext::new();
    let library = TimingLibrary {
        cells: test_cells(vec![
            TimingCell {
                name: "SRC".to_string(),
                arcs: vec![TimingArc {
                    from_pin: "A".to_string(),
                    to_pin: "Y".to_string(),
                    timing_sense: TimingSense::PositiveUnate,
                    cell_rise: Some(LookupTable::scalar(0.1)),
                    cell_fall: None,
                    rise_transition: Some(LookupTable::scalar(1.0)),
                    fall_transition: None,
                }],
                ..TimingCell::default()
            },
            TimingCell {
                name: "SINK".to_string(),
                arcs: vec![TimingArc {
                    from_pin: "A".to_string(),
                    to_pin: "Y".to_string(),
                    timing_sense: TimingSense::PositiveUnate,
                    cell_rise: Some(LookupTable::new(vec![0.0, 1.0], vec![0.0], vec![0.1, 0.4])),
                    cell_fall: None,
                    rise_transition: None,
                    fall_transition: None,
                }],
                ..TimingCell::default()
            },
        ]),
        ..TimingLibrary::default()
    };
    let design = TimingDesign {
        id: design_id(),
        name: "top".to_string(),
        ports: vec![
            test_port("a", TimingPortDirection::Input),
            test_port("y", TimingPortDirection::Output),
        ],
        instances: vec![
            test_instance(0, "U1", "SRC", [("A", "a"), ("Y", "n")]),
            test_instance(1, "U2", "SINK", [("A", "n"), ("Y", "y")]),
        ],
    };

    let analysis = test_analyze_timing(
        &timing,
        &test_timing_model(&design, &library),
        &ReportTimingOptions::default(),
    );

    for point in ["U1/Y (SRC)", "U2/Y (SINK)"] {
        assert!(analysis.steps().iter().any(|step| step.point() == point));
    }
    assert!((analysis.arrival() - 0.5).abs() < 1e-12);
}
