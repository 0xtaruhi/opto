// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn shell_runs_basic_tcl_expression() {
    let args = ShellArgs {
        command: Some("set a 1; expr {$a + 1}".to_string()),
        ..ShellArgs::default()
    };
    assert_eq!(
        Shell::run(args, Session::new(), test_commands()).unwrap(),
        0
    );
}

#[test]
fn shell_registers_only_the_supplied_command_surface() {
    let args = ShellArgs {
        command: Some("help".to_string()),
        ..ShellArgs::default()
    };
    let mut registry = CommandRegistry::new();
    registry.register(commands::ECHO).unwrap();
    let error = Shell::run(args, Session::new(), registry).unwrap_err();
    assert!(error.to_string().contains("invalid command name \"help\""));

    let args = ShellArgs {
        command: Some(
            "if {![llength [info commands create_clock]]} {error missing}; if {[llength [info commands report_area]]} {error extra}"
                .to_string(),
        ),
        ..ShellArgs::default()
    };
    let mut registry = CommandRegistry::new();
    registry.register_group(commands::SDC).unwrap();
    assert_eq!(Shell::run(args, Session::new(), registry).unwrap(), 0);
}

#[test]
fn tcl_preserves_typed_command_errors_until_the_runtime_boundary() {
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    let error = runtime.eval("synth").unwrap_err();

    assert!(error.to_string().contains("no current design"));
}

#[test]
fn tcl_does_not_reuse_a_typed_error_after_catch_handles_it() {
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    let error = runtime
        .eval("catch {synth}; error {later Tcl failure}")
        .unwrap_err();

    assert!(matches!(
        error,
        ShellError::Source { message, .. } if message == "later Tcl failure"
    ));
}

#[test]
fn synthesize_events_render_structured_progress() {
    let started = synthesis_event_text(&SynthesisEvent::Started {
        design: "top".to_string(),
        effort: SynthesisEffort::High,
        parallelism: 4,
    });
    assert_eq!(
        started,
        "Beginning technology mapping for 'top' with 4 workers (high effort).\n"
    );

    assert_eq!(
        synthesis_event_text(&SynthesisEvent::ArtifactCompleted {
            design: "top".to_string(),
            metrics: Box::new(opto_session::SynthesisMetrics {
                source_change: opto_session::SourceChangeMetrics {
                    operations: 4,
                    changed_operations: 1,
                    boundaries: 3,
                    changed_boundaries: 2,
                    ..opto_session::SourceChangeMetrics::default()
                },
                mapped_cells: 2,
                mapped_nets: 3,
                ..opto_session::SynthesisMetrics::default()
            }),
        }),
        concat!(
            "Synthesis artifact for 'top' is complete; preparing the mapped object ",
            "index.\n"
        )
    );
    assert_eq!(
        synthesis_event_text(&SynthesisEvent::DesignInformationUpdateStarted {
            design: "top".to_string(),
            effort: SynthesisEffort::High,
        }),
        "Publishing mapped design information for 'top'.\n"
    );
    let completed = synthesis_event_text(&SynthesisEvent::Completed {
        design: "top".to_string(),
        synthesized: true,
    });
    assert_eq!(completed, "Optimization complete.\n");

    let reused = synthesis_event_text(&SynthesisEvent::Completed {
        design: "top".to_string(),
        synthesized: false,
    });
    assert_eq!(
        reused,
        "Information: Design 'top' is unchanged; reusing synthesized root artifact.\n"
    );
}

#[test]
fn shell_exit_returns_requested_code() {
    let args = ShellArgs {
        command: Some("exit 7".to_string()),
        ..ShellArgs::default()
    };
    assert_eq!(
        Shell::run(args, Session::new(), test_commands()).unwrap(),
        7
    );
}

#[test]
fn read_parasitics_accepts_delay_reduction_modes() {
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    let default = runtime.eval("read_parasitics demo.spef").unwrap_err();
    assert!(default.to_string().contains("demo.spef"));

    let arnoldi = runtime
        .eval("read_parasitics -arnoldi demo.spef")
        .unwrap_err();
    assert!(arnoldi.to_string().contains("demo.spef"));

    let options = runtime
        .eval(
            "read_parasitics -increment -pin_cap_included -net_cap_only -complete_with none -path top -strip_path spef -syntax_only -verbose demo.spef",
        )
        .unwrap_err();
    assert!(options.to_string().contains("demo.spef"));

    let conflict = runtime
        .eval("read_parasitics -elmore -arnoldi demo.spef")
        .unwrap_err();
    assert!(conflict.to_string().contains("mutually exclusive"));
}

#[test]
fn shell_sources_script_without_loading_init_files() {
    let script = temp_script_path("opto-source-basic.tcl");
    std::fs::write(&script, "set sourced yes\nexpr {$sourced eq {yes}}\n").unwrap();
    let args = ShellArgs {
        script: Some(script.clone()),
        no_init: true,
        ..ShellArgs::default()
    };
    let result = Shell::run(args, Session::new(), test_commands());
    std::fs::remove_file(script).unwrap();
    assert_eq!(result.unwrap(), 0);
}

#[test]
fn database_settings_are_typed_and_round_trip() {
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    let value = runtime
        .eval("set_db synth_effort high; set_db clock_gating true; list [get_db synth_effort] [get_db clock_gating]")
        .unwrap();
    assert!(matches!(value, EvalResult::Complete(value) if value == "high true"));
}

#[test]
fn shell_exit_stops_sourced_script() {
    let script = temp_script_path("opto-source-exit.tcl");
    std::fs::write(&script, "exit 5\necho should_not_run\n").unwrap();
    let args = ShellArgs {
        command: Some(format!("source {}", tcl_path_word(&script))),
        no_init: true,
        ..ShellArgs::default()
    };
    let result = Shell::run(args, Session::new(), test_commands());
    std::fs::remove_file(script).unwrap();
    assert_eq!(result.unwrap(), 5);
}

#[test]
fn redirect_variable_captures_command_result() {
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    let result = runtime
        .eval("redirect -variable captured {echo hello}; set captured")
        .unwrap();

    match result {
        EvalResult::Complete(value) => assert_eq!(value, "hello\n"),
        EvalResult::Exit(code) => panic!("unexpected exit {code}"),
    }
}

#[test]
fn redirect_file_captures_command_result() {
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();
    let path = temp_script_path("opto-redirect-file.rpt");

    let result = runtime
        .eval(&format!(
            "redirect -file {} {{echo hello}}",
            tcl_path_word(&path)
        ))
        .unwrap();

    assert!(matches!(result, EvalResult::Complete(value) if value.is_empty()));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello\n");
    std::fs::remove_file(path).unwrap();
}

#[test]
fn redirect_rejects_unimplemented_channel_target() {
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    let err = runtime
        .eval("redirect -channel stdout {echo hello}")
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("redirect: option '-channel' is not implemented")
    );
}

#[test]
fn clock_gating_settings_validate_values() {
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    runtime
        .eval("set_db clock_gating_minimum_bitwidth 8")
        .unwrap();
    runtime
        .eval("set_db clock_gating_latch_based false")
        .unwrap();

    let width = runtime
        .eval("set_db clock_gating_minimum_bitwidth 0")
        .unwrap_err();
    assert!(width.to_string().contains("must be at least 1"));

    let cell = runtime
        .eval("set_db clock_gating_latch_based flop")
        .unwrap_err();
    assert!(cell.to_string().contains("expected boolean"));

    let unsupported = runtime
        .eval("set_db clock_gating_max_fanout 8")
        .unwrap_err();
    assert!(unsupported.to_string().contains("clock_gating_max_fanout"));
}

#[test]
fn synth_has_a_deliberately_small_argument_surface() {
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    for option in ["-gate_clock", "-scan"] {
        let error = runtime.eval(&format!("synth {option}")).unwrap_err();
        assert!(error.to_string().contains(option));
    }
}
