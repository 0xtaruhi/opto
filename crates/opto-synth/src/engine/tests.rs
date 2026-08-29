// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn test_span() -> word::SourceSpan {
    word::SourceSpan::stable("test")
}

fn structural(module: word::WordModule) -> RtlModule {
    RtlModule::structural(module).unwrap()
}

const OUTER_STAGES: [StageId; 7] = [
    StageId::NORMALIZATION,
    StageId::REGIONAL_PLANNING,
    StageId::LOGIC_LOWERING,
    StageId::INITIAL_MAPPING,
    StageId::MAPPED_NETLIST,
    StageId::POSTMAP_OPTIMIZATION,
    StageId::FINALIZATION,
];

fn cell(name: &str, area: f64) -> opto_library::TargetCell {
    opto_library::TargetCell {
        name: name.to_string(),
        area: Some(area),
        dont_use: false,
        usage: opto_library::TargetCellUsage::default(),
        pins: Vec::new(),
        sequential: Vec::new(),
        clock_gate: None,
        memory: None,
    }
}

fn timed_dff() -> opto_library::TargetCell {
    let pin =
        |name: &str,
         direction: opto_library::TargetPinDirection,
         function: Option<&str>,
         timing_arcs: Vec<opto_library::TargetTimingArc>| opto_library::TargetPin {
            name: name.to_string(),
            direction,
            function: function
                .map(|function| opto_library::BooleanFunction::parse(function).unwrap()),
            three_state: None,
            capacitance: None,
            rise_capacitance: None,
            fall_capacitance: None,
            receiver_capacitance: None,
            fanout_load: None,
            next_state_type: None,
            timing_arcs,
            clock_gate_role: None,
        };
    let scalar_delay = opto_library::ArcDelayModel::Nldm(opto_library::NldmTimingModel::new(
        Some(opto_library::LookupTable::scalar(0.3)),
        Some(opto_library::LookupTable::scalar(0.3)),
        Some(opto_library::LookupTable::scalar(0.04)),
        Some(opto_library::LookupTable::scalar(0.04)),
    ));
    opto_library::TargetCell {
        name: "DFF".to_string(),
        area: Some(1.0),
        dont_use: false,
        usage: opto_library::TargetCellUsage::default(),
        pins: vec![
            pin(
                "D",
                opto_library::TargetPinDirection::Input,
                None,
                vec![opto_library::TargetTimingArc {
                    related_pin: "CP".to_string(),
                    timing_type: opto_library::TargetTimingType::Check {
                        kind: opto_library::TimingCheckKind::Setup,
                        clock_edge: opto_library::TimingEdge::Rise,
                    },
                    timing_sense: opto_library::TimingSense::NonUnate,
                    delay_model: None,
                    rise_constraint: Some(opto_library::LookupTable::scalar(0.2)),
                    fall_constraint: Some(opto_library::LookupTable::scalar(0.2)),
                }],
            ),
            pin(
                "CP",
                opto_library::TargetPinDirection::Input,
                None,
                Vec::new(),
            ),
            pin(
                "Q",
                opto_library::TargetPinDirection::Output,
                Some("IQ"),
                vec![opto_library::TargetTimingArc {
                    related_pin: "CP".to_string(),
                    timing_type: opto_library::TargetTimingType::ClockToQ(
                        opto_library::TimingEdge::Rise,
                    ),
                    timing_sense: opto_library::TimingSense::PositiveUnate,
                    delay_model: Some(scalar_delay),
                    rise_constraint: None,
                    fall_constraint: None,
                }],
            ),
        ],
        sequential: vec![opto_library::TargetSequential {
            kind: opto_library::TargetSequentialKind::FlipFlop,
            state_variables: vec!["IQ".to_string()],
            clocked_on: Some(opto_library::BooleanFunction::parse("CP").unwrap()),
            next_state: Some(opto_library::BooleanFunction::parse("D").unwrap()),
            enable: None,
            clear: None,
            preset: None,
        }],
        clock_gate: None,
        memory: None,
    }
}

fn timed_and(name: &str, area: f64, delay: f64) -> opto_library::TargetCell {
    let input = |name: &str| opto_library::TargetPin {
        name: name.to_string(),
        direction: opto_library::TargetPinDirection::Input,
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
    };
    let arc = |related_pin: &str| opto_library::TargetTimingArc {
        related_pin: related_pin.to_string(),
        timing_type: opto_library::TargetTimingType::Combinational,
        timing_sense: opto_library::TimingSense::PositiveUnate,
        delay_model: Some(opto_library::ArcDelayModel::Nldm(
            opto_library::NldmTimingModel::new(
                Some(opto_library::LookupTable::scalar(delay)),
                Some(opto_library::LookupTable::scalar(delay)),
                Some(opto_library::LookupTable::scalar(0.04)),
                Some(opto_library::LookupTable::scalar(0.04)),
            ),
        )),
        rise_constraint: None,
        fall_constraint: None,
    };
    let mut cell = cell(name, area);
    cell.pins = vec![
        input("A"),
        input("B"),
        opto_library::TargetPin {
            name: "Y".to_string(),
            direction: opto_library::TargetPinDirection::Output,
            function: Some(opto_library::BooleanFunction::parse("A&B").unwrap()),
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
    ];
    cell
}

#[test]
fn technology_mapping_progress_uses_final_register_to_register_timing() {
    let mut module = word::WordModule::new("registered_path");
    let bit = word::WordType::bits(1).unwrap();
    let clock_port = module
        .add_port("clk", word::PortDirection::Input, bit, test_span())
        .unwrap();
    let output_port = module
        .add_port("q", word::PortDirection::Output, bit, test_span())
        .unwrap();
    let clock = module
        .read_signal(module.port(clock_port).unwrap().signal, test_span())
        .unwrap();
    let zero = module
        .constant(
            opto_ir::ConstBits::from_bin_str("0").unwrap(),
            bit,
            test_span(),
        )
        .unwrap();
    let first = module
        .register(
            word::RegisterOp {
                name: None,
                d: zero,
                clock,
                edge: word::Edge::Pos,
                enable: None,
                resets: Vec::new(),
            },
            test_span(),
        )
        .unwrap();
    let second = module
        .register(
            word::RegisterOp {
                name: None,
                d: first,
                clock,
                edge: word::Edge::Pos,
                enable: None,
                resets: Vec::new(),
            },
            test_span(),
        )
        .unwrap();
    module
        .connect(
            word::LValue::signal(module.port(output_port).unwrap().signal),
            second,
            test_span(),
        )
        .unwrap();

    let dff = timed_dff();
    let mut request = SynthesisRequest::unconstrained(
        structural(module),
        SynthesisOptions {
            target_cells: vec![dff.clone()].into(),
        },
    );
    let clock_id = opto_timing::PortId::from_uid(opto_core::ObjectUid::from_raw(2).unwrap());
    let mut timing = opto_timing::TimingContext::new();
    timing
        .create_clock(
            opto_timing::ClockId::from_uid(opto_core::ObjectUid::from_raw(4).unwrap()),
            opto_timing::ClockSpec::new("clk", 0.4, vec![clock_id], None).unwrap(),
        )
        .unwrap();
    request.scenarios = ScenarioSet::single(
        Arc::new(timing),
        Arc::new(opto_timing::TimingLibrary {
            cells: vec![dff].into(),
            ..opto_timing::TimingLibrary::default()
        }),
        opto_timing::Parasitics::default(),
    );
    let mut mapping_progress = None;
    let result = SynthesisEngine::new()
        .synthesize(request, crate::test_runtime(), &mut |progress| {
            if matches!(
                progress,
                crate::SynthesisProgress::Candidate {
                    phase: crate::OptimizationPhase::TechnologyMapping,
                    ..
                }
            ) {
                mapping_progress = Some(progress);
            }
        })
        .unwrap();
    let progress = mapping_progress.expect("technology mapping emitted no candidate progress");
    let final_timing = result
        .timing()
        .expect("registered path has no timing summary");

    let crate::SynthesisProgress::Candidate {
        timing: Some(timing),
        ..
    } = progress
    else {
        panic!("technology mapping candidate has no timing measurements");
    };
    assert!((timing.worst_slack.unwrap() + 0.1).abs() < 1e-12);
    assert_eq!(timing.worst_slack, final_timing.slack);
    assert_eq!(
        timing.total_negative_slack.to_bits(),
        final_timing.tns.to_bits()
    );
    assert_eq!(timing.violations, final_timing.violating_paths);
}

fn exact_feedback_mapping_request(effort: SynthesisEffort) -> SynthesisRequest<'static> {
    let mut module = word::WordModule::new("exact_feedback");
    let bit = word::WordType::bits(1).unwrap();
    let clock_port = module
        .add_port("clk", word::PortDirection::Input, bit, test_span())
        .unwrap();
    let a_port = module
        .add_port("a", word::PortDirection::Input, bit, test_span())
        .unwrap();
    let b_port = module
        .add_port("b", word::PortDirection::Input, bit, test_span())
        .unwrap();
    let c_port = module
        .add_port("c", word::PortDirection::Input, bit, test_span())
        .unwrap();
    let output_port = module
        .add_port("q", word::PortDirection::Output, bit, test_span())
        .unwrap();
    let clock = module
        .read_signal(module.port(clock_port).unwrap().signal, test_span())
        .unwrap();
    let a = module
        .read_signal(module.port(a_port).unwrap().signal, test_span())
        .unwrap();
    let b = module
        .read_signal(module.port(b_port).unwrap().signal, test_span())
        .unwrap();
    let c = module
        .read_signal(module.port(c_port).unwrap().signal, test_span())
        .unwrap();
    let first_state = module
        .register(
            word::RegisterOp {
                name: None,
                d: a,
                clock,
                edge: word::Edge::Pos,
                enable: None,
                resets: Vec::new(),
            },
            test_span(),
        )
        .unwrap();
    let first = module
        .binary(word::BinaryOp::BitAnd, first_state, b, test_span())
        .unwrap();
    let second = module
        .binary(word::BinaryOp::BitAnd, first, c, test_span())
        .unwrap();
    let second_state = module
        .register(
            word::RegisterOp {
                name: None,
                d: second,
                clock,
                edge: word::Edge::Pos,
                enable: None,
                resets: Vec::new(),
            },
            test_span(),
        )
        .unwrap();
    module
        .connect(
            word::LValue::signal(module.port(output_port).unwrap().signal),
            second_state,
            test_span(),
        )
        .unwrap();

    let mut dff = timed_dff();
    dff.pins[0].capacitance = Some(0.1);
    let mapping_cells = vec![
        dff.clone(),
        timed_and("SMALL_AND", 1.0, 1.0),
        timed_and("FAST_AND", 10.0, 0.2),
    ];
    let mut request = SynthesisRequest::unconstrained(
        structural(module),
        SynthesisOptions {
            target_cells: mapping_cells.clone().into(),
        },
    );
    request.effort = effort;
    let clock_id = opto_timing::PortId::from_uid(opto_core::ObjectUid::from_raw(2).unwrap());
    let mut timing = opto_timing::TimingContext::new();
    timing
        .create_clock(
            opto_timing::ClockId::from_uid(opto_core::ObjectUid::from_raw(7).unwrap()),
            opto_timing::ClockSpec::new("clk", 6.0, vec![clock_id], None).unwrap(),
        )
        .unwrap();
    let exact_cells = vec![
        dff,
        timed_and("SMALL_AND", 1.0, 5.0),
        mapping_cells[2].clone(),
    ];
    request.scenarios = ScenarioSet::single(
        Arc::new(timing),
        Arc::new(opto_timing::TimingLibrary {
            cells: exact_cells.into(),
            ..opto_timing::TimingLibrary::default()
        }),
        opto_timing::Parasitics::default(),
    );
    request
}

fn initial_mapping_cell_types(
    request: SynthesisRequest<'static>,
) -> (usize, usize, Option<f64>, Vec<String>) {
    let engine = SynthesisEngine::new();
    let runtime = crate::test_runtime();
    let mut observer = |_| {};
    let input = SynthesisInput::new(request).unwrap();
    let design_id = input.environment.design_id;
    let mut execution = SynthesisExecution {
        engine: &engine,
        runtime,
        observer: &mut observer,
        design_id,
    };
    let normalized = normalize(&mut execution, input).unwrap();
    let planned = plan_regions_with_partition_policy(
        &execution,
        normalized,
        crate::regional::region_graph::RegionPartitionPolicy::with_target_work(1),
    )
    .unwrap();
    let lowered = lowering::lower_logic(&execution, planned).unwrap();
    let mut mapped = map_initial_logic(&mut execution, lowered).unwrap();
    let mut cells = mapped
        .mapped
        .netlist
        .cell_ids()
        .map(|cell| mapped.mapped.netlist.cell_type(cell).unwrap().to_string())
        .collect::<Vec<_>>();
    cells.sort();
    let wns = mapped
        .timing
        .as_mut()
        .and_then(|timing| timing.metrics().unwrap().analysis.wns());
    (
        mapped.ledger.regional_epochs,
        mapped.regions.regions().len(),
        wns,
        cells,
    )
}

#[test]
fn exact_boundary_feedback_reselects_a_compiled_regional_cover() {
    let (single_epoch, initial_regions, initial_wns, initial) =
        initial_mapping_cell_types(exact_feedback_mapping_request(SynthesisEffort::Low));
    let (corrected_epochs, corrected_regions, corrected_wns, corrected) =
        initial_mapping_cell_types(exact_feedback_mapping_request(SynthesisEffort::Medium));

    assert_eq!(single_epoch, 1);
    assert_eq!(
        initial.iter().filter(|cell| *cell == "SMALL_AND").count(),
        2,
        "single-epoch regions={initial_regions} wns={initial_wns:?} cells={initial:?}"
    );
    assert!(!initial.iter().any(|cell| cell == "FAST_AND"));
    assert!(initial_wns.is_some_and(|wns| wns < 0.0));
    assert!(
        corrected_epochs > 1,
        "corrected regions={corrected_regions} wns={corrected_wns:?} cells={corrected:?}"
    );
    assert_eq!(
        corrected.iter().filter(|cell| *cell == "FAST_AND").count(),
        1,
        "corrected regions={corrected_regions} epochs={corrected_epochs} \
         wns={corrected_wns:?} cells={corrected:?}"
    );
    assert_eq!(
        corrected.iter().filter(|cell| *cell == "SMALL_AND").count(),
        1,
        "corrected regions={corrected_regions} epochs={corrected_epochs} \
         wns={corrected_wns:?} cells={corrected:?}"
    );
    assert!(
        corrected_wns.is_some_and(|wns| wns >= 0.0),
        "initial regions={initial_regions} wns={initial_wns:?} cells={initial:?}; \
         corrected regions={corrected_regions} epochs={corrected_epochs} \
         wns={corrected_wns:?} cells={corrected:?}"
    );
}

fn target_options() -> SynthesisOptions {
    SynthesisOptions {
        target_cells: vec![cell("UNUSED", 1.0)].into(),
    }
}

fn inverter_cell() -> opto_library::TargetCell {
    let mut inverter = cell("INV", 1.0);
    inverter.pins = vec![
        opto_library::TargetPin {
            name: "A".to_string(),
            direction: opto_library::TargetPinDirection::Input,
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
        opto_library::TargetPin {
            name: "Z".to_string(),
            direction: opto_library::TargetPinDirection::Output,
            function: Some(opto_library::BooleanFunction::parse("!A").unwrap()),
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
    ];
    inverter
}

fn inverter_options() -> SynthesisOptions {
    SynthesisOptions {
        target_cells: vec![inverter_cell()].into(),
    }
}

fn tri_state_options() -> SynthesisOptions {
    let inverter = inverter_cell();
    let input = |name: &str| opto_library::TargetPin {
        name: name.to_string(),
        direction: opto_library::TargetPinDirection::Input,
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
    };
    let mut tri_state = cell("TBUF", 1.0);
    tri_state.pins = vec![
        input("A"),
        input("E"),
        opto_library::TargetPin {
            name: "Y".to_string(),
            direction: opto_library::TargetPinDirection::Output,
            function: Some(opto_library::BooleanFunction::parse("A").unwrap()),
            three_state: Some(opto_library::BooleanFunction::parse("!E").unwrap()),
            capacitance: None,
            rise_capacitance: None,
            fall_capacitance: None,
            receiver_capacitance: None,
            fanout_load: None,
            next_state_type: None,
            timing_arcs: Vec::new(),
            clock_gate_role: None,
        },
    ];
    SynthesisOptions {
        target_cells: vec![inverter, tri_state].into(),
    }
}

#[test]
fn tri_state_inputs_preserve_their_combinational_cover() {
    let mut module = word::WordModule::new("tri_state_input_logic");
    let bit = word::WordType::bits(1).unwrap();
    let data_port = module
        .add_port("data", word::PortDirection::Input, bit, test_span())
        .unwrap();
    let enable_port = module
        .add_port("enable", word::PortDirection::Input, bit, test_span())
        .unwrap();
    let pad_port = module
        .add_port("pad", word::PortDirection::Inout, bit, test_span())
        .unwrap();
    let pad = module.port(pad_port).unwrap().signal;
    module
        .set_signal_resolution(pad, word::SignalResolution::TriState)
        .unwrap();
    let data = module
        .read_signal(module.port(data_port).unwrap().signal, test_span())
        .unwrap();
    let enable = module
        .read_signal(module.port(enable_port).unwrap().signal, test_span())
        .unwrap();
    let inverted = module
        .unary(word::UnaryOp::BitNot, data, test_span())
        .unwrap();
    let driver = module
        .tri_state(
            inverted,
            word::Enable {
                value: enable,
                active_high: true,
            },
            test_span(),
        )
        .unwrap();
    module
        .connect(word::LValue::signal(pad), driver, test_span())
        .unwrap();

    let result = SynthesisEngine::new()
        .synthesize(
            SynthesisRequest::unconstrained(structural(module), tri_state_options()),
            crate::test_runtime(),
            &mut |_| {},
        )
        .unwrap();
    let mut cell_types = result
        .mapped()
        .cell_ids()
        .map(|cell| result.mapped().cell_type(cell).unwrap())
        .collect::<Vec<_>>();
    cell_types.sort_unstable();

    assert_eq!(cell_types, ["INV", "TBUF"]);
}

#[test]
fn synthesize_rejects_an_empty_mapping_library() {
    let request = SynthesisRequest::unconstrained(
        structural(word::WordModule::new("top")),
        SynthesisOptions {
            target_cells: opto_library::TargetCellSet::default(),
        },
    );
    assert!(
        validate_mapping_library(&request)
            .unwrap_err()
            .to_string()
            .contains("non-empty target library")
    );
}

#[test]
fn mapping_library_accepts_resolution_library_supersets() {
    let target = cell("TARGET", 1.0);
    let mut request = SynthesisRequest::unconstrained(
        structural(word::WordModule::new("top")),
        SynthesisOptions {
            target_cells: vec![target.clone()].into(),
        },
    );
    let library = Arc::new(opto_timing::TimingLibrary {
        cells: vec![target, cell("LINK_ONLY", 2.0)].into(),
        ..opto_timing::TimingLibrary::default()
    });
    request.scenarios = ScenarioSet::single(
        Arc::new(opto_timing::TimingContext::default()),
        library,
        opto_timing::Parasitics::default(),
    );

    validate_mapping_library(&request).unwrap();
}

#[test]
fn mapping_library_accepts_pvt_data_but_rejects_mapping_incompatibility() {
    let target = cell("TARGET", 1.0);
    let mut request = SynthesisRequest::unconstrained(
        structural(word::WordModule::new("top")),
        SynthesisOptions {
            target_cells: vec![target].into(),
        },
    );
    let library = Arc::new(opto_timing::TimingLibrary {
        cells: vec![cell("OTHER", 1.0)].into(),
        ..opto_timing::TimingLibrary::default()
    });
    request.scenarios = ScenarioSet::single(
        Arc::new(opto_timing::TimingContext::default()),
        library,
        opto_timing::Parasitics::default(),
    );
    assert!(
        validate_mapping_library(&request)
            .unwrap_err()
            .to_string()
            .contains("absent")
    );

    let library = Arc::new(opto_timing::TimingLibrary {
        cells: vec![cell("TARGET", 2.0)].into(),
        ..opto_timing::TimingLibrary::default()
    });
    request.scenarios = ScenarioSet::single(
        Arc::new(opto_timing::TimingContext::default()),
        Arc::clone(&library),
        opto_timing::Parasitics::default(),
    );
    validate_mapping_library(&request).unwrap();

    let mut incompatible = cell("TARGET", 2.0);
    incompatible.dont_use = true;
    let library = Arc::new(opto_timing::TimingLibrary {
        cells: vec![incompatible].into(),
        ..opto_timing::TimingLibrary::default()
    });
    request.scenarios = ScenarioSet::single(
        Arc::new(opto_timing::TimingContext::default()),
        library,
        opto_timing::Parasitics::default(),
    );
    assert!(
        validate_mapping_library(&request)
            .unwrap_err()
            .to_string()
            .contains("incompatible")
    );
}

#[test]
fn mapping_context_cache_retains_recent_technologies() {
    let engine = SynthesisEngine::new();
    let technologies = (0..=TARGET_MAPPING_CONTEXT_CAPACITY)
        .map(|index| SynthesisOptions {
            target_cells: vec![cell(&format!("CELL_{index}"), 1.0)].into(),
        })
        .collect::<Vec<_>>();
    let contexts = technologies[..TARGET_MAPPING_CONTEXT_CAPACITY]
        .iter()
        .map(|options| engine.mapping_context(options))
        .collect::<Vec<_>>();
    assert!(Arc::ptr_eq(
        &contexts[0],
        &engine.mapping_context(&technologies[0])
    ));
    engine.mapping_context(&technologies[TARGET_MAPPING_CONTEXT_CAPACITY]);
    assert!(Arc::ptr_eq(
        &contexts[0],
        &engine.mapping_context(&technologies[0])
    ));
    assert!(!Arc::ptr_eq(
        &contexts[1],
        &engine.mapping_context(&technologies[1])
    ));
    let contexts = engine.mapping_contexts.read().unwrap();
    assert_eq!(contexts.len(), TARGET_MAPPING_CONTEXT_CAPACITY);
    assert_eq!(
        contexts.last().unwrap().0.bytes(),
        technologies[1].target_cells.content_fingerprint().bytes()
    );
}

#[test]
fn invalidating_an_artifact_retains_its_incremental_snapshot() {
    let runtime = ExecutionContext::new(&opto_runtime::ExecutionConfig { max_threads: 1 }).unwrap();
    let mut module = word::WordModule::new("top");
    let bit = word::WordType::bits(1).unwrap();
    let input = module
        .add_port("a", word::PortDirection::Input, bit, test_span())
        .unwrap();
    let output = module
        .add_port("y", word::PortDirection::Output, bit, test_span())
        .unwrap();
    let value = module
        .read_signal(module.port(input).unwrap().signal, test_span())
        .unwrap();
    module
        .connect(
            word::LValue::signal(module.port(output).unwrap().signal),
            value,
            test_span(),
        )
        .unwrap();
    let result = SynthesisEngine::new()
        .synthesize(
            SynthesisRequest::unconstrained(structural(module), target_options()),
            &runtime,
            &mut |_| {},
        )
        .unwrap();
    let fingerprint = result.source_snapshot().semantic_fingerprint();
    let snapshot = result.into_incremental_snapshot();
    assert_eq!(snapshot.source().semantic_fingerprint(), fingerprint);
    snapshot.validate_checkpoint().unwrap();
}

fn regional_cache_module() -> RtlModule {
    let mut module = word::WordModule::new("regional_cache");
    let bit = word::WordType::bits(1).unwrap();
    let input = module
        .add_port("a", word::PortDirection::Input, bit, test_span())
        .unwrap();
    let output = module
        .add_port("y", word::PortDirection::Output, bit, test_span())
        .unwrap();
    let input = module
        .read_signal(module.port(input).unwrap().signal, test_span())
        .unwrap();
    let inverted = module
        .unary(word::UnaryOp::BitNot, input, test_span())
        .unwrap();
    module
        .connect(
            word::LValue::signal(module.port(output).unwrap().signal),
            inverted,
            test_span(),
        )
        .unwrap();
    structural(module)
}

#[test]
fn prior_artifact_reuses_current_region_decisions() {
    let engine = SynthesisEngine::new();
    let options = inverter_options();
    let cold = engine
        .synthesize(
            SynthesisRequest::unconstrained(regional_cache_module(), options.clone()),
            crate::test_runtime(),
            &mut |_| {},
        )
        .unwrap();
    let mut request = SynthesisRequest::unconstrained(regional_cache_module(), options);
    request.previous_incremental = Some(cold.incremental_snapshot());
    let warm = engine
        .synthesize(request, crate::test_runtime(), &mut |_| {})
        .unwrap();

    assert_eq!(cold.metrics().regional_decision_hits, 0);
    assert_eq!(cold.metrics().regional_decision_misses, 1);
    assert_eq!(warm.metrics().regional_decision_hits, 1);
    assert_eq!(warm.metrics().regional_decision_misses, 0);
    let mut cold_verilog = Vec::new();
    let mut warm_verilog = Vec::new();
    opto_formats::write_mapped_verilog(&mut cold_verilog, cold.mapped()).unwrap();
    opto_formats::write_mapped_verilog(&mut warm_verilog, warm.mapped()).unwrap();
    assert_eq!(cold_verilog, warm_verilog);
}

fn synthesis_outer_events(
    options: SynthesisOptions,
) -> Vec<(StageId, crate::SynthesisProgressStatus)> {
    let engine = SynthesisEngine::new();
    let mut source = word::WordModule::new("top");
    let bit = word::WordType::new(1, false, word::LogicStateKind::FourState).unwrap();
    let input = source
        .add_port("a", word::PortDirection::Input, bit, test_span())
        .unwrap();
    let output = source
        .add_port("y", word::PortDirection::Output, bit, test_span())
        .unwrap();
    let value = source
        .read_signal(source.port(input).unwrap().signal, test_span())
        .unwrap();
    source
        .connect(
            word::LValue::signal(source.port(output).unwrap().signal),
            value,
            test_span(),
        )
        .unwrap();
    let mut events = Vec::new();
    engine
        .synthesize(
            SynthesisRequest::unconstrained(structural(source), options),
            crate::test_runtime(),
            &mut |progress| {
                if let crate::SynthesisProgress::Stage { stage, status } = progress
                    && OUTER_STAGES.contains(&stage)
                {
                    events.push((stage, status));
                }
            },
        )
        .unwrap();
    events
}

#[test]
fn fixed_pipeline_always_runs_postmap() {
    let expected = OUTER_STAGES
        .into_iter()
        .flat_map(|stage| {
            [
                (stage, crate::SynthesisProgressStatus::Started),
                (stage, crate::SynthesisProgressStatus::Completed),
            ]
        })
        .collect::<Vec<_>>();
    let options = SynthesisOptions {
        target_cells: vec![cell("UNUSED", 1.0)].into(),
    };

    assert_eq!(synthesis_outer_events(options), expected);
}

#[test]
fn failed_stage_emits_a_terminal_failure_event() {
    let engine = SynthesisEngine::new();
    let mut events = Vec::new();
    let error = engine
        .synthesize(
            SynthesisRequest::unconstrained(
                structural(word::WordModule::new("top")),
                target_options(),
            ),
            crate::test_runtime(),
            &mut |progress| {
                if let crate::SynthesisProgress::Stage { stage, status } = progress
                    && OUTER_STAGES.contains(&stage)
                {
                    events.push((stage, status));
                }
            },
        )
        .unwrap_err();

    assert!(error.to_string().contains("has no ports"));
    assert_eq!(
        events,
        [
            (
                StageId::NORMALIZATION,
                crate::SynthesisProgressStatus::Started,
            ),
            (
                StageId::NORMALIZATION,
                crate::SynthesisProgressStatus::Failed,
            ),
        ]
    );
}
