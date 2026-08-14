// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn constraint_checkpoint_rejects_another_session_before_mutation() {
    let mut source = Session::new();
    let restore_checkpoint = source.constraint_checkpoint();
    let commit_checkpoint = source.constraint_checkpoint();
    let mut target = Session::new();
    target.set_clock_gating_enabled(true);
    let revision = target.revision();

    let error = target
        .restore_constraint_checkpoint(restore_checkpoint)
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "constraint checkpoint belongs to another session"
    );
    assert_eq!(target.revision(), revision);
    assert!(target.clock_gating_enabled());

    let error = target
        .commit_constraint_checkpoint(commit_checkpoint)
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "constraint checkpoint belongs to another session"
    );
    assert_eq!(target.revision(), revision);
    assert!(target.clock_gating_enabled());
}

#[test]
fn stale_nested_constraint_checkpoint_is_rejected_before_mutation() {
    let mut session = Session::new();
    let outer = session.constraint_checkpoint();
    let stale_inner = session.constraint_checkpoint();

    session.restore_constraint_checkpoint(outer).unwrap();
    session.set_clock_gating_enabled(true);
    let revision = session.revision();
    let error = session
        .restore_constraint_checkpoint(stale_inner)
        .unwrap_err();

    assert_eq!(
        error.to_string(),
        "timing: checkpoint is stale or no longer active"
    );
    assert_eq!(session.revision(), revision);
    assert!(session.clock_gating_enabled());
}

#[test]
fn constraint_checkpoint_restores_every_mutable_constraint_owner() {
    let mut session = Session::new();
    let stable_object = session
        .state
        .objects
        .intern(ObjectLocator::Clock {
            name: "stable_clock".to_string(),
        })
        .unwrap();
    let stable_member = session.collection_member_handle(stable_object);
    let checkpoint = session.constraint_checkpoint();
    let revision = session.state.revision;

    session.state.current_design = Some("transient".to_string());
    session
        .create_clock("transient_clock", 1.0, Vec::new(), None)
        .unwrap();
    let transient_object = session
        .state
        .objects
        .get(&ObjectLocator::Clock {
            name: "transient_clock".to_string(),
        })
        .unwrap();
    let transient_member = session.collection_member_handle(transient_object);
    session.restore_constraint_checkpoint(checkpoint).unwrap();

    assert_eq!(session.state.revision, revision);
    assert_eq!(session.current_design(), None);
    assert!(
        session
            .state
            .objects
            .get(&ObjectLocator::Clock {
                name: "transient_clock".to_string(),
            })
            .is_none()
    );
    assert!(session.state.timing.clocks().is_empty());
    assert_eq!(session.collection_len(&stable_member).unwrap(), 1);
    assert!(session.collection_len(&transient_member).is_err());

    session
        .create_clock("transient_clock", 1.0, Vec::new(), None)
        .unwrap();
    let replacement_object = session
        .state
        .objects
        .get(&ObjectLocator::Clock {
            name: "transient_clock".to_string(),
        })
        .unwrap();
    let replacement_member = session.collection_member_handle(replacement_object);
    assert_ne!(replacement_object, transient_object);
    assert_ne!(replacement_member, transient_member);
    assert!(session.collection_len(&transient_member).is_err());
    assert_eq!(session.collection_len(&replacement_member).unwrap(), 1);
}

#[test]
fn failed_generated_clock_creation_leaves_no_registry_object() {
    let mut session = Session::new();
    let mut design = DesignIndex::new("top");
    let clk = design.intern_name("clk").unwrap();
    design.ports.push(Port {
        name: clk,
        direction: Direction::Input,
        width: 1,
    });
    install_test_design(&mut session, design);
    session.set_current_design("top").unwrap();
    let sources = session
        .resolve_port_ids("create_clock", &["clk".to_string()])
        .unwrap();
    session
        .create_clock("master", 10.0, sources.clone(), None)
        .unwrap();
    let master = session
        .state
        .objects
        .get(&ObjectLocator::Clock {
            name: "master".to_string(),
        })
        .unwrap()
        .downcast::<opto_db::ClockObject>()
        .unwrap();

    let _error = session
        .create_generated_clock(
            "ghost",
            Vec::new(),
            GeneratedClock {
                master,
                source: sources[0],
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

    assert!(
        session
            .state
            .objects
            .get(&ObjectLocator::Clock {
                name: "ghost".to_string(),
            })
            .is_none()
    );
    assert!(!session.report_clock().contains("ghost"));
}

#[test]
fn constraint_commit_preserves_created_objects() {
    let mut session = Session::new();
    let checkpoint = session.constraint_checkpoint();
    session
        .create_clock("committed_clock", 1.0, Vec::new(), None)
        .unwrap();
    let committed_object = session
        .state
        .objects
        .get(&ObjectLocator::Clock {
            name: "committed_clock".to_string(),
        })
        .unwrap();
    let committed_member = session.collection_member_handle(committed_object);

    session.commit_constraint_checkpoint(checkpoint).unwrap();

    assert!(session.state.objects.resolve(committed_object).is_some());
    assert_eq!(session.collection_len(&committed_member).unwrap(), 1);
}

#[test]
fn checkpoint_install_invalidates_handles_even_when_ids_are_reused() {
    let mut session = Session::new();
    let mut design = DesignIndex::new("top");
    let port_name = design.intern_name("clk").unwrap();
    design.add_port(Port {
        name: port_name,
        direction: Direction::Input,
        width: 1,
    });
    install_test_design(&mut session, design);
    session.set_current_design("top").unwrap();

    let ports = session.get_ports("*").unwrap();
    let original = session.collection_handles(ports).join(" ");
    let original_object = session.collection_members(&original).unwrap()[0];
    let original_member = session.collection_member_handle(original_object);
    let checkpoint = temp_file("handle-generation.ock");
    session.write_checkpoint_file(&checkpoint).unwrap();

    session.read_checkpoint_file(&checkpoint).unwrap();
    std::fs::remove_file(checkpoint).unwrap();
    let ports = session.get_ports("*").unwrap();
    let replacement = session.collection_handles(ports).join(" ");
    let replacement_object = session.collection_members(&replacement).unwrap()[0];
    let replacement_member = session.collection_member_handle(replacement_object);

    assert_ne!(original, replacement);
    assert_ne!(original_member, replacement_member);
    assert!(session.collection_len(&original).is_err());
    assert!(session.collection_len(&original_member).is_err());
    assert_eq!(session.collection_len(&replacement).unwrap(), 1);
    assert_eq!(session.collection_len(&replacement_member).unwrap(), 1);
}

#[test]
fn current_design_switches_between_loaded_designs() {
    let mut session = Session::new();
    install_test_design(&mut session, DesignIndex::new("a"));
    install_test_design(&mut session, DesignIndex::new("b"));
    assert_eq!(session.set_current_design("b").unwrap(), "b");
    assert_eq!(session.current_design(), Some("b"));
}

#[test]
fn check_design_adds_its_command_context_exactly_once() {
    let mut session = Session::new();
    install_test_design(&mut session, DesignIndex::new("empty"));
    session.set_current_design("empty").unwrap();

    let error = session.check_design().unwrap_err();

    assert!(
        matches!(
            &error,
            SessionError::CheckDesign(opto_synth::CheckDesignError::NoPorts { design })
                if design == "empty"
        ),
        "unexpected error: {error:?}"
    );
    assert_eq!(
        error.to_string(),
        "check_design: design 'empty' has no ports"
    );
    assert!(!error.to_string().contains("synth:"));
}

#[test]
fn synthesis_context_uses_the_invoked_command_name() {
    let error = SessionError::synthesis(
        "synth",
        "broken",
        opto_synth::SynthError::InvalidDesign("invalid test design".to_string()),
    );

    assert_eq!(
        error.to_string(),
        "synth: design 'broken': invalid test design"
    );
}

#[test]
fn synth_preserves_structured_synthesis_diagnostics() {
    use opto_core::DiagnosticSource;

    let cycle = opto_synth::CombinationalCycle::new(
        "broken",
        3,
        vec![opto_synth::CombinationalCycleNode::new(
            "signal 'loop'",
            opto_ir::word::SourceSpan::located("broken.sv", Some(7), Some(11), "data assignment"),
        )],
        Vec::new(),
    );
    let error = SessionError::synthesis("synth", "broken", cycle.into());

    let diagnostic = error.diagnostic().unwrap();
    assert_eq!(diagnostic.code(), "OPT-SYN-001");
    assert_eq!(diagnostic.primary().unwrap().location().path(), "broken.sv");
}

#[test]
fn subsystem_errors_keep_stable_diagnostic_domains_through_session() {
    use opto_core::DiagnosticSource;

    let cases = [
        (
            SessionError::from(opto_hdl::HdlError::NoInputFiles),
            "OPT-HDL-001",
        ),
        (
            SessionError::from(opto_timing::TimingError::Analysis(
                opto_timing::TimingAnalysisError::InvalidMaxPaths,
            )),
            "OPT-TIM-200",
        ),
        (
            SessionError::from(opto_power::PowerError::MissingLibraryUnit {
                attribute: "time_unit",
            }),
            "OPT-PWR-001",
        ),
        (
            SessionError::from(opto_formats::FormatError::Unsupported(
                "unsupported test format".to_string(),
            )),
            "OPT-FMT-002",
        ),
        (
            SessionError::from(opto_library::LibraryError::UnsupportedInput {
                path: PathBuf::from("test.txt"),
            }),
            "OPT-LIB-003",
        ),
    ];

    for (error, expected_code) in cases {
        assert_eq!(error.diagnostic().unwrap().code(), expected_code);
    }
}

#[test]
fn successful_frontend_update_publishes_recoverable_diagnostics_once() {
    let mut session = Session::new();
    let update = DbUpdate {
        modules: vec![empty_rtl_module("top")],
        top: Some("top".to_string()),
        diagnostics: vec![opto_hdl::SlangDiagnostic {
            severity: opto_hdl::SlangDiagnosticSeverity::Warning,
            subsystem: 3,
            code: 1,
            message: "test frontend warning".to_string(),
            option_name: Some("test-warning".to_string()),
            location: None,
        }],
    };

    session.apply_db_update(update).unwrap();

    let diagnostics = session.take_diagnostics();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].code(), "OPT-HDL-S03-0001");
    assert_eq!(
        diagnostics[0].severity(),
        opto_core::DiagnosticSeverity::Warning
    );
    assert!(session.take_diagnostics().is_empty());
}

#[test]
fn frontend_batch_publishes_exactly_one_revision() {
    let mut session = Session::new();
    let initial = session.revision();
    let update = DbUpdate {
        modules: vec![empty_rtl_module("a"), empty_rtl_module("b")],
        top: Some("a".to_string()),
        diagnostics: Vec::new(),
    };

    session.apply_db_update(update).unwrap();

    assert_eq!(session.revision(), initial.next().unwrap());
    assert_eq!(design_names(&session), ["a".to_string(), "b".to_string()]);
}

#[test]
fn rejected_frontend_batch_does_not_publish_partial_state() {
    let mut session = Session::new();
    let initial = session.revision();
    let update = DbUpdate {
        modules: vec![empty_rtl_module("top"), empty_rtl_module("top")],
        top: Some("top".to_string()),
        diagnostics: Vec::new(),
    };

    assert_eq!(
        session.apply_db_update(update).unwrap_err().to_string(),
        "frontend returned duplicate design 'top'"
    );
    assert_eq!(session.revision(), initial);
    assert!(session.state.designs.keys().next().is_none());
}

#[test]
fn database_settings_have_typed_defaults() {
    let session = Session::new();

    assert_eq!(session.hdl_search_path(), [PathBuf::from(".")]);
    assert_eq!(session.lib_search_path(), [PathBuf::from(".")]);
    assert_eq!(session.synth_effort(), SynthesisEffort::Medium);
    assert!(session.clock_gating_enabled());
}

#[test]
fn port_collection_filters_by_pattern() {
    let mut session = Session::new();
    let mut design = DesignIndex::new("top");
    let clk = design.intern_name("clk").unwrap();
    let data = design.intern_name("data").unwrap();
    design.ports.push(Port {
        name: clk,
        direction: Direction::Input,
        width: 1,
    });
    design.ports.push(Port {
        name: data,
        direction: Direction::Input,
        width: 8,
    });
    install_test_design(&mut session, design);
    session.set_current_design("top").unwrap();
    let ports = session.get_ports("d*").unwrap();
    let handle = session.collection_handles(ports).join(" ");
    assert_eq!(
        session.collection_object_names(&handle).unwrap(),
        ["data".to_string()]
    );
}

#[test]
fn checkpoint_preserves_compact_elmore_parasitics() {
    use opto_timing::{
        ParasiticAnalysisOptions, Parasitics, RcCapacitor, RcConnection, RcConnectionRole,
        RcNetwork, RcResistor, TimingLibraryUnits,
    };

    let checkpoint = temp_file("parasitics-checkpoint.ock");
    let mut session = Session::new();
    install_test_design(&mut session, DesignIndex::new("top"));
    session.set_current_design("top").unwrap();
    let parasitics = Parasitics::from_rc_networks(
        vec![RcNetwork {
            name: "n".to_string(),
            total_capacitance_farads: 1.0,
            connections: vec![
                RcConnection {
                    node: "driver".to_string(),
                    object: "U1/Y".to_string(),
                    role: RcConnectionRole::Driver,
                    pin_capacitance_farads: [0.0; 2],
                },
                RcConnection {
                    node: "sink".to_string(),
                    object: "U2/A".to_string(),
                    role: RcConnectionRole::Sink,
                    pin_capacitance_farads: [0.0; 2],
                },
            ],
            capacitors: vec![RcCapacitor {
                first: "sink".to_string(),
                second: None,
                capacitance_farads: 1.0,
            }],
            resistors: vec![RcResistor {
                first: "driver".to_string(),
                second: "sink".to_string(),
                resistance_ohms: 1.0,
            }],
            source_waveforms: [None, None],
        }],
        TimingLibraryUnits {
            time_seconds: Some(1.0),
            capacitance_farads: Some(1.0),
            resistance_ohms: None,
        },
        ParasiticAnalysisOptions {
            delay_model: ParasiticDelayModel::Elmore,
            ..ParasiticAnalysisOptions::default()
        },
    )
    .unwrap();
    session
        .state
        .parasitics
        .publish("top".to_string(), parasitics)
        .unwrap();
    let expected = session.state.parasitics.clone();
    let expected_revision = session.state.parasitics.revision();
    session.write_checkpoint_file(&checkpoint).unwrap();

    let mut restored = Session::new();
    restored.read_checkpoint_file(&checkpoint).unwrap();
    std::fs::remove_file(checkpoint).unwrap();

    assert_eq!(restored.state.parasitics, expected);
    assert_eq!(restored.state.parasitics.revision(), expected_revision);
}

#[test]
fn parasitics_are_bound_to_the_design_that_imported_them() {
    use opto_timing::{
        ParasiticAnalysisOptions, Parasitics, RcConnection, RcConnectionRole, RcNetwork,
        TimingLibraryUnits,
    };

    let mut session = Session::new();
    install_test_design(&mut session, DesignIndex::new("top"));
    install_test_design(&mut session, DesignIndex::new("other"));
    let parasitics = Parasitics::from_rc_networks(
        vec![RcNetwork {
            name: "top_only_net".to_string(),
            total_capacitance_farads: 0.0,
            connections: vec![RcConnection {
                node: "driver".to_string(),
                object: "driver".to_string(),
                role: RcConnectionRole::Driver,
                pin_capacitance_farads: [0.0; 2],
            }],
            capacitors: Vec::new(),
            resistors: Vec::new(),
            source_waveforms: [None, None],
        }],
        TimingLibraryUnits {
            time_seconds: Some(1.0),
            capacitance_farads: Some(1.0),
            resistance_ohms: None,
        },
        ParasiticAnalysisOptions::default(),
    )
    .unwrap();
    session
        .state
        .parasitics
        .publish("top".to_string(), parasitics)
        .unwrap();

    session.set_current_design("other").unwrap();
    let current = session.current_design_name().unwrap();
    assert!(!session.state.parasitics.get("top").unwrap().1.is_empty());
    assert!(session.state.parasitics.get(current).is_none());
}
