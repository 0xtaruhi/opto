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

fn target_options() -> SynthesisOptions {
    SynthesisOptions {
        target_cells: vec![cell("UNUSED", 1.0)].into(),
    }
}

fn inverter_options() -> SynthesisOptions {
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
    SynthesisOptions {
        target_cells: vec![inverter].into(),
    }
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
                if OUTER_STAGES.contains(&progress.stage) && progress.optimization.is_none() {
                    events.push((progress.stage, progress.status));
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
                if OUTER_STAGES.contains(&progress.stage) && progress.optimization.is_none() {
                    events.push((progress.stage, progress.status));
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
