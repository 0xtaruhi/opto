// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use std::collections::BTreeSet;

fn registry() -> CommandRegistry {
    let mut registry = CommandRegistry::new();
    registry.register_group(crate::commands::ALL).unwrap();
    registry
}

#[test]
fn public_command_names_are_unique() {
    let registry = registry();
    let mut names = BTreeSet::new();
    for command in registry.iter() {
        let spec = command.spec();
        assert!(names.insert(spec.name), "duplicate command '{}'", spec.name);
    }
}

#[test]
fn commands_and_groups_are_explicit_registration_units() {
    let mut registry = CommandRegistry::new();
    registry.register(crate::commands::ECHO).unwrap();
    assert!(registry.find("echo").is_some());
    assert!(registry.find("help").is_none());
    assert!(registry.find("create_clock").is_none());

    registry.register(crate::commands::SET_LOAD).unwrap();
    assert!(registry.find("set_load").is_some());
    assert!(registry.find("set_drive").is_none());
    assert!(registry.find("set_input_transition").is_none());

    let mut registry = CommandRegistry::new();
    registry.register_group(crate::commands::SDC).unwrap();
    assert!(registry.find("create_clock").is_some());
    assert!(registry.find("set_drive").is_some());
    assert!(registry.find("report_area").is_none());
}

#[test]
fn duplicate_group_registration_is_atomic() {
    let mut registry = CommandRegistry::new();
    registry.register(crate::commands::CREATE_CLOCK).unwrap();
    let count = registry.iter().count();
    let error = registry
        .register_group(crate::commands::SDC)
        .expect_err("duplicate command must reject the whole group");
    assert!(error.to_string().contains("already registered"));
    assert_eq!(registry.iter().count(), count);
    assert!(registry.find("set_load").is_none());
}

#[test]
fn sdc_group_exactly_matches_sdc_eligible_commands() {
    let all = registry();
    let mut sdc = CommandRegistry::new();
    sdc.register_group(crate::commands::SDC).unwrap();
    for command in all.iter() {
        let spec = command.spec();
        assert_eq!(
            sdc.find(spec.name).is_some(),
            spec.sdc_since.is_some() || spec.name == "read_sdc",
            "SDC group membership mismatch for '{}'",
            spec.name
        );
    }
}

#[test]
fn every_command_uses_typed_option_identity() {
    let registry = registry();
    for command in registry.iter() {
        let spec = command.spec();
        for option in &command.syntax().options {
            assert_ne!(
                option.id,
                OptionId::Untracked,
                "untracked option identity for '{} {}'",
                spec.name,
                option.name,
            );
        }
    }
}

#[test]
fn derived_command_structs_are_registration_units() {
    let write = crate::command::design::WriteHdlArgs::command_specs();
    assert_eq!(
        write.iter().map(|spec| spec.name).collect::<Vec<_>>(),
        ["write_hdl"]
    );
    assert!(write.iter().all(|spec| spec.sdc_since.is_none()));

    let create_clock = crate::command::timing::CreateClockArgs::<'static>::command_specs();
    assert_eq!(create_clock.len(), 1);
    assert_eq!(create_clock[0].name, "create_clock");
    assert_eq!(create_clock[0].sdc_since, Some(SdcVersion::V1_0));

    let registry = registry();
    let write_syntax = registry.find("write_hdl").unwrap().syntax();
    let hierarchy = write_syntax
        .options
        .iter()
        .find(|option| option.name == "-hierarchy")
        .expect("derived hierarchy option");
    assert_ne!(hierarchy.id, OptionId::Untracked);
}

#[test]
fn help_is_derived_from_the_public_catalog() {
    let registry = registry();
    let help = registry.help_text();
    for command in registry.iter() {
        let spec = command.spec();
        assert!(
            help.split_whitespace().any(|word| word == spec.name),
            "help omitted command '{}'",
            spec.name
        );
    }
}

#[test]
fn completion_options_are_unique_per_command() {
    let registry = registry();
    for command in registry.iter() {
        let spec = command.syntax();
        let mut options = BTreeSet::new();
        for option in spec.options.iter().chain(&spec.unsupported_options) {
            assert!(
                options.insert(option.name),
                "duplicate completion option '{}' for '{}'",
                option.name,
                command.spec().name
            );
        }
    }
}

#[test]
fn help_is_an_exact_view_of_registered_declarative_syntax() {
    let registry = registry();
    for command in registry.iter() {
        let syntax = command.syntax();
        let name = command.spec().name;
        let help = registry
            .command_help_text(name)
            .expect("registered command has help");
        for (option, unsupported) in syntax.options.iter().map(|option| (option, false)).chain(
            syntax
                .unsupported_options
                .iter()
                .map(|option| (option, true)),
        ) {
            assert!(
                help.contains(option.name),
                "help for '{}' omitted '{}'",
                name,
                option.name
            );
            let line = help
                .lines()
                .find(|line| line.split_whitespace().next() == Some(option.name))
                .expect("declared option has a help line");
            assert_eq!(
                line.contains("(not implemented)"),
                unsupported,
                "help status mismatch for '{} {}'",
                name,
                option.name
            );
        }
    }
}

#[test]
fn invocation_preflight_matches_audited_command_contracts() {
    struct Case {
        command: &'static str,
        args: &'static [&'static str],
        expected_error: Option<&'static str>,
    }

    let cases = [
        Case {
            command: "read_sdc",
            args: &["-syntax_only", "-version", "2.2", "constraints.sdc"],
            expected_error: None,
        },
        Case {
            command: "read_sdc",
            args: &["constraints.sdc", "extra.sdc"],
            expected_error: Some("wrong number of arguments"),
        },
        Case {
            command: "read_sdc",
            args: &["-version"],
            expected_error: Some("missing value for -version"),
        },
        Case {
            command: "read_sdc",
            args: &["-version", "9.9", "constraints.sdc"],
            expected_error: Some("value for -version must be 1.0"),
        },
        Case {
            command: "read_parasitics",
            args: &["-complete_with", "invented", "design.spef"],
            expected_error: Some("value for -complete_with must be none"),
        },
        Case {
            command: "read_parasitics",
            args: &["-elmore", "-arnoldi", "design.spef"],
            expected_error: Some("-elmore and -arnoldi are mutually exclusive"),
        },
        Case {
            command: "report_power",
            args: &["-cell", "-net"],
            expected_error: Some("-cell and -net are mutually exclusive"),
        },
        Case {
            command: "redirect",
            args: &["out.rpt", "report_area"],
            expected_error: None,
        },
        Case {
            command: "redirect",
            args: &["-file", "out.rpt", "report_area"],
            expected_error: None,
        },
        Case {
            command: "redirect",
            args: &["-variable", "captured"],
            expected_error: Some("wrong number of arguments"),
        },
        Case {
            command: "redirect",
            args: &["out.rpt", "report_area", "extra"],
            expected_error: Some("wrong number of arguments"),
        },
        Case {
            command: "set_input_transition",
            args: &["0.2", "ports"],
            expected_error: None,
        },
        Case {
            command: "set_input_transition",
            args: &["-rise", "0.2", "ports"],
            expected_error: None,
        },
        Case {
            command: "set_load",
            args: &["1.0"],
            expected_error: Some("wrong number of arguments"),
        },
        Case {
            command: "set_load",
            args: &["-max", "1.0", "ports"],
            expected_error: None,
        },
        Case {
            command: "set_max_transition",
            args: &["-0.2", "ports"],
            expected_error: None,
        },
        Case {
            command: "set_max_transition",
            args: &["0.2", "ports", "-data_path"],
            expected_error: Some("unexpected option '-data_path' after object list"),
        },
        Case {
            command: "set_max_fanout",
            args: &["2", "-data_path", "clocks"],
            expected_error: Some("unsupported option '-data_path'"),
        },
        Case {
            command: "report_area",
            args: &[],
            expected_error: None,
        },
        Case {
            command: "report_area",
            args: &["unexpected"],
            expected_error: Some("wrong number of arguments"),
        },
        Case {
            command: "create_clock",
            args: &["-period"],
            expected_error: Some("missing value for -period"),
        },
        Case {
            command: "create_clock",
            args: &["-period", "-1", "-name", "clk"],
            expected_error: None,
        },
    ];

    let registry = registry();
    for case in cases {
        let command = registry.find(case.command).unwrap();
        let result = validate_invocation(command, case.args);
        match case.expected_error {
            None => assert!(
                result.is_ok(),
                "{} {:?}: {result:?}",
                case.command,
                case.args
            ),
            Some(expected) => {
                let error = result.expect_err("invalid invocation must fail preflight");
                assert!(
                    error.to_string().contains(expected),
                    "{} {:?}: {error}",
                    case.command,
                    case.args
                );
            }
        }
    }

    let elaborate = registry.find("elaborate").unwrap();
    let create_clock = registry.find("create_clock").unwrap();
    let set_max_transition = registry.find("set_max_transition").unwrap();
    assert!(validate_invocation(elaborate, &["top"]).is_ok());
    assert!(
        validate_sdc_invocation(create_clock, &["-name", "clk"])
            .unwrap_err()
            .to_string()
            .contains("missing -period")
    );
    assert!(
        validate_sdc_invocation(create_clock, &["-period", "5"])
            .unwrap_err()
            .to_string()
            .contains("missing -name")
    );
    assert!(
        validate_sdc_invocation(set_max_transition, &["-data_path", "0.2", "ports"])
            .unwrap_err()
            .to_string()
            .contains("value must precede options")
    );
    assert_eq!(set_max_transition.syntax().leading_positionals, 1);
    assert_eq!(
        registry
            .find("set_max_fanout")
            .unwrap()
            .syntax()
            .leading_positionals,
        1
    );
    assert_eq!(
        registry
            .find("set_max_delay")
            .unwrap()
            .syntax()
            .leading_positionals,
        1
    );
}
