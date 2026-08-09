// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::test_library::{
    ClockToQArc, TimingArc, TimingCell, TimingConstraintArc, test_cells, test_instance,
    test_target_cells,
};
use crate::{
    ClockSpec, CornerSelection, EdgeQualifier, EdgeSelection, ExceptionCorner, ExceptionFilter,
    LatencySide, LookupTable, PathException, TargetCell, TargetSequential, TargetSequentialKind,
    TargetTimingType, TimingConnection, TimingInstance, TimingInstanceId, TimingObjectBindings,
    TimingSense, test_clock_id, test_design_id, test_port, test_port_id,
};
use opto_core::ObjectUid;
use opto_library::BooleanFunction;

#[test]
fn analyzes_register_to_register_setup_path() {
    let (timing, design, library) = sequential_fixture();
    let model = crate::test_timing_model(&design, &library);
    let analysis = analyze_timing(&timing, &model, &ReportTimingOptions::default()).unwrap();

    assert_eq!(analysis.startpoint(), "launch_reg");
    assert_eq!(analysis.endpoint(), "capture_reg");
    assert!(
        analysis
            .path_instances()
            .any(|instance| instance == TimingInstanceId::from_raw(1))
    );
    assert!((analysis.arrival() - 0.07).abs() < 1e-12);
    assert!((analysis.required().unwrap() - 0.98).abs() < 1e-12);
    assert!((analysis.slack().unwrap() - 0.91).abs() < 1e-12);
    assert_eq!(
        analysis.startpoint_description(),
        "rising edge-triggered flip-flop clocked by clk"
    );
    assert_eq!(
        analysis.endpoint_description(),
        "rising edge-triggered flip-flop clocked by clk"
    );
    assert_eq!(analysis.path_group(), Some("clk"));
    for point in ["launch_reg/CP (DFF)", "launch_reg/Q (DFF)", "U_INV/Y (INV)"] {
        assert!(analysis.steps().iter().any(|step| step.point() == point));
    }
    assert_eq!(analysis.endpoint_object(), "capture_reg/D");
    assert!(matches!(
        analysis.requirement(),
        Some(TimingRequirement::Setup { .. })
    ));
}

#[test]
fn analyzes_register_to_register_hold_path() {
    let (timing, design, library) = sequential_fixture();
    let options = ReportTimingOptions {
        delay_type: DelayType::Min,
        ..ReportTimingOptions::default()
    };
    let model = crate::test_timing_model(&design, &library);
    let analysis = analyze_timing(&timing, &model, &options).unwrap();

    assert_eq!(analysis.startpoint(), "launch_reg");
    assert_eq!(analysis.endpoint(), "capture_reg");
    assert!((analysis.arrival() - 0.07).abs() < 1e-12);
    assert!((analysis.required().unwrap() - 0.01).abs() < 1e-12);
    assert!((analysis.slack().unwrap() - 0.06).abs() < 1e-12);
    assert_eq!(analysis.delay_type(), DelayType::Min);
    assert!(matches!(
        analysis.requirement(),
        Some(TimingRequirement::Hold { .. })
    ));
}

#[test]
fn clock_latency_and_uncertainty_change_setup_slack() {
    let (mut timing, design, library) = sequential_fixture();
    let clock = test_clock_id(100);
    timing
        .set_clock_latency(
            0.2,
            true,
            EdgeSelection::Both,
            CornerSelection::Max,
            LatencySide::Late,
            &[clock],
        )
        .unwrap();
    timing
        .set_clock_latency(
            0.1,
            true,
            EdgeSelection::Both,
            CornerSelection::Max,
            LatencySide::Early,
            &[clock],
        )
        .unwrap();
    timing
        .set_clock_uncertainty(
            0.05,
            &[clock],
            EdgeSelection::Both,
            &[clock],
            EdgeSelection::Both,
            ExceptionCorner::Setup,
        )
        .unwrap();

    let analysis = analyze_timing(
        &timing,
        &crate::test_timing_model(&design, &library),
        &ReportTimingOptions::default(),
    )
    .unwrap();

    assert!((analysis.arrival() - 0.27).abs() < 1e-12);
    assert!((analysis.required().unwrap() - 1.13).abs() < 1e-12);
    assert!((analysis.slack().unwrap() - 0.86).abs() < 1e-12);
}

#[test]
fn multicycle_setup_and_hold_adjust_capture_requirements() {
    let (mut timing, design, library) = sequential_fixture();
    let path = || PathException {
        kind: PathExceptionKind::MultiCycle {
            cycles: 2,
            use_end_clock: true,
        },
        from: ExceptionFilter::new([TimingEndpoint::Clock(test_clock_id(100))]),
        through: Vec::new().into_boxed_slice(),
        to: ExceptionFilter::new([TimingEndpoint::Clock(test_clock_id(100))]),
        edges: EdgeQualifier::default(),
        corner: ExceptionCorner::Setup,
        ignore_clock_latency: false,
        comment: "two-cycle setup".to_string(),
    };
    timing.set_path_exception(path()).unwrap();
    let model = crate::test_timing_model(&design, &library);

    let setup = analyze_timing(&timing, &model, &ReportTimingOptions::default()).unwrap();
    assert!((setup.required().unwrap() - 1.98).abs() < 1e-12);
    assert!(matches!(
        setup.path_exception().map(crate::TimingPathException::kind),
        Some(PathExceptionKind::MultiCycle { cycles: 2, .. })
    ));

    let hold = analyze_timing(
        &timing,
        &model,
        &ReportTimingOptions {
            delay_type: DelayType::Min,
            ..ReportTimingOptions::default()
        },
    )
    .unwrap();
    assert!((hold.required().unwrap() - 1.01).abs() < 1e-12);

    timing
        .set_path_exception(PathException {
            kind: PathExceptionKind::MultiCycle {
                cycles: 1,
                use_end_clock: false,
            },
            corner: ExceptionCorner::Hold,
            comment: "paired hold".to_string(),
            ..path()
        })
        .unwrap();
    let hold = analyze_timing(
        &timing,
        &model,
        &ReportTimingOptions {
            delay_type: DelayType::Min,
            ..ReportTimingOptions::default()
        },
    )
    .unwrap();
    assert!((hold.required().unwrap() - 0.01).abs() < 1e-12);
}

#[test]
fn ordered_through_points_must_complete_in_graph_order() {
    let (timing, design, library) = sequential_fixture();
    let input_pin = crate::PinId::from_uid(ObjectUid::from_raw(30_001).unwrap());
    let output_pin = crate::PinId::from_uid(ObjectUid::from_raw(30_002).unwrap());
    let build_model = || {
        let mut model = crate::test_timing_model(&design, &library);
        let mut bindings = TimingObjectBindings::builder();
        bindings.bind_pin("U_INV/A", input_pin).unwrap();
        bindings.bind_pin("U_INV/Y", output_pin).unwrap();
        model.set_object_bindings(bindings.finish().unwrap());
        model
    };
    let exception = |through: [crate::PinId; 2]| PathException {
        kind: PathExceptionKind::MaxDelay { delay: 0.2 },
        from: ExceptionFilter::new([TimingEndpoint::Clock(test_clock_id(100))]),
        through: through
            .map(|pin| ExceptionFilter::new([TimingEndpoint::Pin(pin)]))
            .into(),
        to: ExceptionFilter::new([TimingEndpoint::Clock(test_clock_id(100))]),
        edges: EdgeQualifier::new(
            EdgeSelection::Both,
            [EdgeSelection::Both, EdgeSelection::Both],
            EdgeSelection::Both,
            EdgeSelection::Both,
        ),
        corner: ExceptionCorner::Setup,
        ignore_clock_latency: false,
        comment: String::new(),
    };

    let mut ordered = timing.clone();
    ordered
        .set_path_exception(exception([input_pin, output_pin]))
        .unwrap();
    let analysis =
        analyze_timing(&ordered, &build_model(), &ReportTimingOptions::default()).unwrap();
    assert!((analysis.required().unwrap() - 0.18).abs() < 1e-12);

    let mut reversed = timing;
    reversed
        .set_path_exception(exception([output_pin, input_pin]))
        .unwrap();
    let analysis =
        analyze_timing(&reversed, &build_model(), &ReportTimingOptions::default()).unwrap();
    assert!((analysis.required().unwrap() - 0.98).abs() < 1e-12);
    assert!(analysis.path_exception().is_none());
}

#[test]
fn register_path_filters_match_instance_names_and_pin_objects() {
    let (timing, design, library) = sequential_fixture();
    let options = ReportTimingOptions {
        from: vec!["launch_reg/Q".to_string()],
        to: vec!["capture_reg/D".to_string()],
        ..ReportTimingOptions::default()
    };
    let model = crate::test_timing_model(&design, &library);
    let analysis = analyze_timing(&timing, &model, &options).unwrap();

    assert_eq!(analysis.startpoint(), "launch_reg");
    assert_eq!(analysis.endpoint(), "capture_reg");
}

#[test]
fn preserves_independent_launch_contexts_through_reconvergence() {
    let merge = TimingCell {
        name: "AND2".to_string(),
        arcs: vec![
            TimingArc::scalar("A", "Y", 0.2),
            TimingArc::scalar("B", "Y", 0.2),
            TimingArc::scalar("C", "Y", 0.3),
        ],
        ..TimingCell::default()
    };
    let (mut timing, mut design, library) = sequential_fixture_with(vec![merge]);
    timing
        .set_max_delay(
            0.1,
            vec![TimingEndpoint::Port(test_port_id("d"))],
            vec![TimingEndpoint::Port(test_port_id("z"))],
        )
        .unwrap();
    design
        .ports
        .push(test_port("z", TimingPortDirection::Output));
    design
        .ports
        .push(test_port("e", TimingPortDirection::Input));
    design.instances.push(test_instance(
        3,
        "U_MERGE",
        "AND2",
        [("A", "d"), ("B", "launch_q"), ("C", "e"), ("Y", "z")],
    ));
    let model = crate::test_timing_model(&design, &library);
    let paths = analyze_timing_paths(&timing, &model, &ReportTimingOptions::default()).unwrap();
    let expanded = analyze_timing_paths(
        &timing,
        &model,
        &ReportTimingOptions {
            max_paths: 2,
            ..ReportTimingOptions::default()
        },
    )
    .unwrap();
    let analysis = analyze_timing(&timing, &model, &ReportTimingOptions::default()).unwrap();

    assert_eq!(paths.len(), 1);
    assert_eq!(expanded.len(), 2);
    for pair in expanded.windows(2) {
        assert!(!super::paths::path_is_worse(&pair[1], &pair[0]));
    }
    assert_eq!(analysis.startpoint(), "d");
    assert_eq!(analysis.endpoint(), "z");
    assert!((analysis.arrival() - 0.2).abs() < 1e-12);
    assert_eq!(analysis.required(), Some(0.1));
    assert!((analysis.slack().unwrap() + 0.1).abs() < 1e-12);
}

#[test]
fn report_path_limit_is_global_and_must_be_positive() {
    let (timing, design, library) = sequential_fixture();
    let error = analyze_timing_paths(
        &timing,
        &crate::test_timing_model(&design, &library),
        &ReportTimingOptions {
            max_paths: 0,
            ..ReportTimingOptions::default()
        },
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("report_timing: max_paths must be greater than zero")
    );
}

#[test]
fn propagates_time_borrowing_through_two_phase_latches() {
    let timing = latch_timing_context();
    let design = latch_timing_design(vec![
        dff_instance(0, "launch_reg", "d", "launch_q"),
        buffer_instance(1, "U_SLOW", "BUF_SLOW", "launch_q", "high_d"),
        latch_instance(2, "borrow_latch", "LATCH_H", "high_d", "high_q"),
        buffer_instance(3, "U_FAST", "BUF_FAST", "high_q", "low_d"),
        latch_instance(4, "capture_latch", "LATCH_L", "low_d", "q"),
    ]);
    let library = latch_timing_library();
    let model = crate::test_timing_model(&design, &library);

    let analysis = analyze_timing(
        &timing,
        &model,
        &ReportTimingOptions {
            to: vec!["capture_latch/D".to_string()],
            ..ReportTimingOptions::default()
        },
    )
    .unwrap();

    assert_eq!(analysis.startpoint(), "launch_reg");
    assert_eq!(analysis.endpoint(), "capture_latch");
    assert!((analysis.arrival() - 0.55).abs() < 1e-12);
    assert!((analysis.required().unwrap() - 0.98).abs() < 1e-12);
    assert!((analysis.time_borrowed().unwrap() - 0.05).abs() < 1e-12);
    assert!(
        analysis
            .path_instances()
            .any(|instance| instance == TimingInstanceId::from_raw(2))
    );
    assert!(
        analysis
            .steps()
            .iter()
            .any(|step| step.point() == "borrow_latch/Q (LATCH_H)")
    );
    assert_eq!(
        analysis.endpoint_description(),
        "falling level-sensitive latch enabled by clk"
    );
}

#[test]
fn data_stable_before_opening_restarts_at_the_latch_boundary() {
    let timing = latch_timing_context();
    let design = latch_timing_design(vec![
        dff_instance(0, "launch_reg", "d", "launch_q"),
        latch_instance(1, "gate_latch", "LATCH_L", "launch_q", "gate_q"),
        buffer_instance(2, "U_LATE", "BUF_LATE", "gate_q", "capture_d"),
        latch_instance(3, "capture_latch", "LATCH_H", "capture_d", "q"),
    ]);
    let library = latch_timing_library();
    let model = crate::test_timing_model(&design, &library);
    let options = ReportTimingOptions {
        from: vec!["gate_latch/Q".to_string()],
        to: vec!["capture_latch/D".to_string()],
        ..ReportTimingOptions::default()
    };

    let analysis = analyze_timing(&timing, &model, &options).unwrap();

    assert_eq!(analysis.startpoint(), "gate_latch");
    assert_eq!(analysis.endpoint(), "capture_latch");
    assert!((analysis.arrival() - 1.14).abs() < 1e-12);
    assert!((analysis.required().unwrap() - 1.48).abs() < 1e-12);
    assert!((analysis.time_borrowed().unwrap() - 0.14).abs() < 1e-12);
    assert!(
        !analysis
            .path_instances()
            .any(|instance| instance == TimingInstanceId::from_raw(0))
    );
    assert_eq!(
        analysis.startpoint_description(),
        "falling level-sensitive latch enabled by clk"
    );
}

#[test]
fn hold_analysis_respects_the_latch_opening_boundary() {
    let timing = latch_timing_context();
    let design = latch_timing_design(vec![
        dff_instance(0, "launch_reg", "d", "launch_q"),
        latch_instance(1, "gate_latch", "LATCH_L", "launch_q", "gate_q"),
        buffer_instance(2, "U_LATE", "BUF_LATE", "gate_q", "capture_d"),
        latch_instance(3, "capture_latch", "LATCH_H", "capture_d", "q"),
    ]);
    let library = latch_timing_library();
    let model = crate::test_timing_model(&design, &library);
    let options = ReportTimingOptions {
        from: vec!["gate_latch/Q".to_string()],
        to: vec!["capture_latch/D".to_string()],
        delay_type: DelayType::Min,
        ..ReportTimingOptions::default()
    };

    let analysis = analyze_timing(&timing, &model, &options).unwrap();

    assert_eq!(analysis.startpoint(), "gate_latch");
    assert_eq!(analysis.endpoint(), "capture_latch");
    assert!((analysis.arrival() - 1.14).abs() < 1e-12);
    assert!((analysis.required().unwrap() - 0.51).abs() < 1e-12);
    assert!(analysis.time_borrowed().is_none());
}

#[test]
fn rejects_latch_timing_without_an_enable_to_q_opening_arc() {
    let design = latch_timing_design(vec![latch_instance(
        0,
        "capture_latch",
        "LATCH_H",
        "d",
        "q",
    )]);
    let mut latch = latch_timing_cell("LATCH_H", true);
    latch
        .pins
        .iter_mut()
        .find(|pin| pin.name == "Q")
        .unwrap()
        .timing_arcs
        .retain(|arc| !matches!(arc.timing_type, TargetTimingType::ClockToQ(_)));
    let library = TimingLibrary {
        cells: vec![latch].into(),
        ..TimingLibrary::default()
    };

    let error = TimingModel::new(design, library).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("has no rising-edge enable-to-Q timing arc"),
        "{error}"
    );
}

#[test]
fn models_constant_latch_controls_explicitly() {
    let library = TimingLibrary {
        cells: vec![latch_timing_cell("LATCH_H", true)].into(),
        ..TimingLibrary::default()
    };

    let model_with_enable = constant_enable_latch_model(true, library.clone());
    let data = model_with_enable.graph.net_id("d").unwrap();
    let output = model_with_enable.graph.net_id("q").unwrap();
    assert!(
        model_with_enable.graph.outgoing[data]
            .iter()
            .map(|&arc| model_with_enable.graph.arc(arc))
            .any(|arc| {
                arc.to.index() == output && matches!(arc.kind, GraphArcKind::Combinational)
            })
    );

    let model_without_enable = constant_enable_latch_model(false, library);
    let data = model_without_enable.graph.net_id("d").unwrap();
    let output = model_without_enable.graph.net_id("q").unwrap();
    assert!(
        model_without_enable.graph.outgoing[data]
            .iter()
            .map(|&arc| model_without_enable.graph.arc(arc))
            .all(|arc| arc.to.index() != output)
    );
}

#[test]
fn diagnoses_cyclic_latch_transparency_separately_from_combinational_loops() {
    let design = latch_timing_design(vec![
        latch_instance(0, "left", "LATCH_H", "right_q", "q"),
        latch_instance(1, "right", "LATCH_H", "q", "right_q"),
    ]);
    let library = TimingLibrary {
        cells: vec![latch_timing_cell("LATCH_H", true)].into(),
        ..TimingLibrary::default()
    };

    let error = TimingModel::new(design, library).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("cyclic latch transparency path detected"),
        "{error}"
    );
}

#[test]
fn asynchronous_and_three_state_relations_enter_the_propagation_graph() {
    let relations = [
        ("clear", TargetTimingType::Clear),
        ("preset", TargetTimingType::Preset),
        ("enable", TargetTimingType::ThreeStateEnable),
        ("disable", TargetTimingType::ThreeStateDisable),
    ];
    let mut cell = test_target_cells(vec![TimingCell {
        name: "CONTROLLED_OUTPUT".to_string(),
        arcs: relations
            .iter()
            .map(|(pin, _)| TimingArc::scalar(*pin, "Q", 0.01))
            .collect(),
        ..TimingCell::default()
    }])
    .pop()
    .unwrap();
    let output = cell.pins.iter_mut().find(|pin| pin.name == "Q").unwrap();
    for (arc, (_, timing_type)) in output.timing_arcs.iter_mut().zip(relations) {
        arc.timing_type = timing_type;
    }
    let design = TimingDesign {
        id: test_design_id(),
        name: "top".to_string(),
        ports: relations
            .iter()
            .map(|(name, _)| TimingPort {
                id: test_port_id(name),
                name: (*name).to_string(),
                net: crate::TimingNet::named(*name),
                direction: TimingPortDirection::Input,
            })
            .chain(std::iter::once(test_port("q", TimingPortDirection::Output)))
            .collect(),
        instances: vec![TimingInstance {
            id: TimingInstanceId::from_raw(0),
            name: "controlled".to_string(),
            cell: "CONTROLLED_OUTPUT".to_string(),
            connections: relations
                .iter()
                .map(|(name, _)| TimingConnection {
                    pin: (*name).to_string(),
                    net: (*name).to_string(),
                })
                .chain(std::iter::once(TimingConnection {
                    pin: "Q".to_string(),
                    net: "q".to_string(),
                }))
                .collect(),
        }],
    };
    let model = TimingModel::new(
        design,
        TimingLibrary {
            cells: vec![cell].into(),
            ..TimingLibrary::default()
        },
    )
    .unwrap();
    let output_net = model.graph.net_id("q").unwrap();
    for (name, timing_type) in relations {
        let input_net = model.graph.net_id(name).unwrap();
        let graph_arc = model.graph.outgoing[input_net]
            .iter()
            .map(|&arc| model.graph.arc(arc))
            .find(|arc| arc.to.index() == output_net)
            .unwrap();
        let (_, library_arc) = model
            .graph
            .cell_pin_arc(
                &model.library,
                graph_arc.instance,
                graph_arc.pin,
                graph_arc.arc,
            )
            .unwrap();
        assert_eq!(library_arc.timing_type(), timing_type);
    }
}

fn sequential_fixture() -> (TimingContext, TimingDesign, TimingLibrary) {
    sequential_fixture_with(Vec::new())
}

fn sequential_fixture_with(
    mut extra_cells: Vec<TimingCell>,
) -> (TimingContext, TimingDesign, TimingLibrary) {
    let mut timing = TimingContext::new();
    timing
        .create_clock(
            test_clock_id(100),
            ClockSpec::new("clk", 1.0, vec![test_port_id("clk")], None).unwrap(),
        )
        .unwrap();
    timing
        .set_input_transition(0.05, &[test_port_id("clk")])
        .unwrap();

    let dff = TimingCell {
        name: "DFF".to_string(),
        arcs: Vec::new(),
        clock_to_q: vec![ClockToQArc {
            clock_edge: TimingEdge::Rise,
            arc: TimingArc {
                from_pin: "CP".to_string(),
                to_pin: "Q".to_string(),
                timing_sense: TimingSense::NonUnate,
                cell_rise: Some(LookupTable::scalar(0.06)),
                cell_fall: Some(LookupTable::scalar(0.06)),
                rise_transition: Some(LookupTable::scalar(0.04)),
                fall_transition: Some(LookupTable::scalar(0.04)),
            },
        }],
        constraints: vec![
            TimingConstraintArc {
                data_pin: "D".to_string(),
                clock_pin: "CP".to_string(),
                clock_edge: TimingEdge::Rise,
                kind: TimingCheckKind::Setup,
                rise_constraint: Some(LookupTable::scalar(0.02)),
                fall_constraint: Some(LookupTable::scalar(0.02)),
            },
            TimingConstraintArc {
                data_pin: "D".to_string(),
                clock_pin: "CP".to_string(),
                clock_edge: TimingEdge::Rise,
                kind: TimingCheckKind::Hold,
                rise_constraint: Some(LookupTable::scalar(0.01)),
                fall_constraint: Some(LookupTable::scalar(0.01)),
            },
        ],
        pin_capacitance: BTreeMap::from([("D".to_string(), 0.01), ("CP".to_string(), 0.02)]),
    };
    let inverter = TimingCell {
        name: "INV".to_string(),
        arcs: vec![TimingArc {
            from_pin: "A".to_string(),
            to_pin: "Y".to_string(),
            timing_sense: TimingSense::NegativeUnate,
            cell_rise: Some(LookupTable::scalar(0.01)),
            cell_fall: Some(LookupTable::scalar(0.01)),
            rise_transition: Some(LookupTable::scalar(0.03)),
            fall_transition: Some(LookupTable::scalar(0.03)),
        }],
        clock_to_q: Vec::new(),
        constraints: Vec::new(),
        pin_capacitance: BTreeMap::from([("A".to_string(), 0.01)]),
    };
    let design = TimingDesign {
        id: test_design_id(),
        name: "top".to_string(),
        ports: vec![
            test_port("clk", TimingPortDirection::Input),
            test_port("d", TimingPortDirection::Input),
            test_port("q", TimingPortDirection::Output),
        ],
        instances: vec![
            dff_instance(0, "launch_reg", "d", "launch_q"),
            test_instance(1, "U_INV", "INV", [("A", "launch_q"), ("Y", "capture_d")]),
            dff_instance(2, "capture_reg", "capture_d", "q"),
        ],
    };
    let mut cells = vec![dff, inverter];
    cells.append(&mut extra_cells);
    let library = TimingLibrary {
        name: Some("demo".to_string()),
        operating_conditions: Some("typical".to_string()),
        wire_load: Some("ZeroWireload".to_string()),
        wire_load_mode: Some("segmented".to_string()),
        wire_load_model: None,
        units: crate::TimingLibraryUnits::default(),
        power: opto_library::PowerLibrary::default(),
        cells: test_cells(cells),
    };
    (timing, design, library)
}

fn dff_instance(id: u32, name: &str, data: &str, output: &str) -> TimingInstance {
    test_instance(id, name, "DFF", [("D", data), ("CP", "clk"), ("Q", output)])
}

fn latch_timing_context() -> TimingContext {
    let mut timing = TimingContext::new();
    timing
        .create_clock(
            test_clock_id(100),
            ClockSpec::new("clk", 1.0, vec![test_port_id("clk")], None).unwrap(),
        )
        .unwrap();
    timing
}

fn latch_timing_design(instances: Vec<TimingInstance>) -> TimingDesign {
    TimingDesign {
        id: test_design_id(),
        name: "top".to_string(),
        ports: vec![
            test_port("clk", TimingPortDirection::Input),
            test_port("d", TimingPortDirection::Input),
            test_port("q", TimingPortDirection::Output),
        ],
        instances,
    }
}

fn latch_timing_library() -> TimingLibrary {
    let dff = TimingCell {
        name: "DFF".to_string(),
        arcs: Vec::new(),
        clock_to_q: vec![ClockToQArc {
            clock_edge: TimingEdge::Rise,
            arc: TimingArc {
                from_pin: "CP".to_string(),
                to_pin: "Q".to_string(),
                timing_sense: TimingSense::NonUnate,
                cell_rise: Some(LookupTable::scalar(0.06)),
                cell_fall: Some(LookupTable::scalar(0.06)),
                rise_transition: None,
                fall_transition: None,
            },
        }],
        constraints: Vec::new(),
        pin_capacitance: BTreeMap::new(),
    };
    let buffer = |name: &str, delay: f64| TimingCell {
        name: name.to_string(),
        arcs: vec![TimingArc::scalar("A", "Y", delay)],
        ..TimingCell::default()
    };
    let mut cells = test_target_cells(vec![
        dff,
        buffer("BUF_SLOW", 0.30),
        buffer("BUF_FAST", 0.15),
        buffer("BUF_LATE", 0.60),
    ]);
    cells.push(latch_timing_cell("LATCH_H", true));
    cells.push(latch_timing_cell("LATCH_L", false));
    TimingLibrary {
        name: Some("latch-demo".to_string()),
        operating_conditions: None,
        wire_load: None,
        wire_load_mode: None,
        wire_load_model: None,
        units: crate::TimingLibraryUnits::default(),
        power: opto_library::PowerLibrary::default(),
        cells: cells.into(),
    }
}

fn latch_timing_cell(name: &str, active_high: bool) -> TargetCell {
    let open_edge = if active_high {
        TimingEdge::Rise
    } else {
        TimingEdge::Fall
    };
    let close_edge = if active_high {
        TimingEdge::Fall
    } else {
        TimingEdge::Rise
    };
    let mut cell = test_target_cells(vec![TimingCell {
        name: name.to_string(),
        arcs: vec![TimingArc::scalar("D", "Q", 0.04)],
        clock_to_q: vec![ClockToQArc {
            clock_edge: open_edge,
            arc: TimingArc {
                from_pin: "E".to_string(),
                to_pin: "Q".to_string(),
                timing_sense: TimingSense::NonUnate,
                cell_rise: Some(LookupTable::scalar(0.04)),
                cell_fall: Some(LookupTable::scalar(0.04)),
                rise_transition: None,
                fall_transition: None,
            },
        }],
        constraints: vec![
            TimingConstraintArc {
                data_pin: "D".to_string(),
                clock_pin: "E".to_string(),
                clock_edge: close_edge,
                kind: TimingCheckKind::Setup,
                rise_constraint: Some(LookupTable::scalar(0.02)),
                fall_constraint: Some(LookupTable::scalar(0.02)),
            },
            TimingConstraintArc {
                data_pin: "D".to_string(),
                clock_pin: "E".to_string(),
                clock_edge: close_edge,
                kind: TimingCheckKind::Hold,
                rise_constraint: Some(LookupTable::scalar(0.01)),
                fall_constraint: Some(LookupTable::scalar(0.01)),
            },
        ],
        pin_capacitance: BTreeMap::new(),
    }])
    .pop()
    .unwrap();
    cell.pins
        .iter_mut()
        .find(|pin| pin.name == "Q")
        .unwrap()
        .function = Some(BooleanFunction::Pin("IQ".to_string()));
    cell.sequential = vec![TargetSequential {
        kind: TargetSequentialKind::Latch,
        state_variables: vec!["IQ".to_string(), "IQN".to_string()],
        clocked_on: None,
        next_state: Some(BooleanFunction::Pin("D".to_string())),
        enable: Some(if active_high {
            BooleanFunction::Pin("E".to_string())
        } else {
            BooleanFunction::Not(Box::new(BooleanFunction::Pin("E".to_string())))
        }),
        clear: None,
        preset: None,
    }];
    cell
}

fn buffer_instance(id: u32, name: &str, cell: &str, input: &str, output: &str) -> TimingInstance {
    test_instance(id, name, cell, [("A", input), ("Y", output)])
}

fn latch_instance(id: u32, name: &str, cell: &str, data: &str, output: &str) -> TimingInstance {
    test_instance(id, name, cell, [("D", data), ("E", "clk"), ("Q", output)])
}

fn constant_enable_latch_model(enable: bool, library: TimingLibrary) -> TimingModel {
    let mut latch = latch_instance(0, "gate_latch", "LATCH_H", "d", "q");
    latch
        .connections
        .iter_mut()
        .find(|connection| connection.pin == "E")
        .unwrap()
        .net = crate::model::constant_net_name(enable).to_string();
    TimingModel::new(latch_timing_design(vec![latch]), library).unwrap()
}
