// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::schema::{Assertions, Case, CaseKind, CaseSpec, Expectation, Flow};
use super::{RunMode, output_directory, run_regression_case, workspace_root};
use std::fmt::Write as _;
use std::path::PathBuf;

const WIDTHS: [usize; 8] = [1, 2, 3, 4, 8, 16, 32, 64];

pub(super) fn run(mode: RunMode) {
    assert!(matches!(mode, RunMode::Presubmit | RunMode::Equivalence));
    let output = output_directory(match mode {
        RunMode::Presubmit => "semantic-matrix",
        RunMode::Equivalence => "semantic-matrix-equivalence",
        _ => unreachable!(),
    });
    if output.exists() {
        std::fs::remove_dir_all(&output).expect("remove semantic matrix output");
    }
    let source_root = output.join("sources");
    std::fs::create_dir_all(&source_root).expect("create semantic matrix sources");
    let library = workspace_root().join("qualification/libraries/opto_test.lib");
    let opto = PathBuf::from(env!("CARGO_BIN_EXE_opto"));
    let yosys = (mode == RunMode::Equivalence).then(|| super::required_executable("OPTO_YOSYS"));
    let batches = generated_batches();
    let coverage_points = batches.iter().map(|batch| batch.points).sum::<usize>();
    assert!(
        coverage_points >= 200,
        "semantic matrix unexpectedly shrank"
    );
    for batch in batches {
        let batch_root = source_root.join(batch.id);
        std::fs::create_dir_all(&batch_root).expect("create semantic batch source directory");
        let source = batch_root.join("top.sv");
        std::fs::write(&source, batch.source).expect("write generated semantic source");
        let descriptor = batch_root.join("generated.toml");
        let mut spec = CaseSpec {
            format: super::schema::FORMAT_VERSION,
            id: format!("generated-{}", batch.id),
            kind: CaseKind::Regression,
            covers: vec![format!(
                "Generated {} semantic matrix with {} labeled points.",
                batch.id, batch.points
            )],
            category: Some("generated_semantics".to_string()),
            class: None,
            scenario: None,
            language: "sverilog".to_string(),
            top: "top".to_string(),
            sources: vec![PathBuf::from("top.sv")],
            equivalence_sources: Vec::new(),
            flow: Flow::Synth,
            library: Some(PathBuf::from("qualification/libraries/opto_test.lib")),
            library_key: None,
            equivalence: true,
            equivalence_initial_state: None,
            sequential: false,
            report_timing: false,
            clock_period: None,
            expected_area: None,
            area_tolerance: None,
            expected_cells: None,
            cell_count_tolerance: None,
            expected_cell_histogram: std::collections::BTreeMap::new(),
            expected_worst_slack: None,
            worst_slack_tolerance: None,
            expected_total_negative_slack: None,
            total_negative_slack_tolerance: None,
            maximum_violating_paths: None,
            maximum_wall_seconds: None,
            maximum_cpu_seconds: None,
            maximum_peak_rss_kib: None,
            expect: Expectation::Pass,
            expect_log: Vec::new(),
            defines: Vec::new(),
            constraints: Vec::new(),
            script: None,
            assertions: Assertions {
                ports: None,
                nets: None,
                cells: Some(1),
            },
            source_root: None,
            revision: None,
            manifest: None,
            configs: None,
            root_environment: None,
            manifest_environment: None,
            report_environment: None,
            config_environment: None,
        };
        std::fs::write(
            &descriptor,
            toml::to_string_pretty(&spec).expect("serialize generated semantic descriptor"),
        )
        .expect("write generated semantic descriptor");
        spec.library = Some(library.clone());
        let case = Case {
            path: descriptor,
            spec,
        };
        eprintln!("RUN  {} ({} semantic points)", case.spec.id, batch.points);
        run_regression_case(&case, &opto, yosys.as_deref(), &output, mode);
        eprintln!("PASS {}", case.spec.id);
    }
    eprintln!("PASS semantic matrix: {coverage_points} points");
}

struct Batch {
    id: &'static str,
    points: usize,
    source: String,
}

fn generated_batches() -> Vec<Batch> {
    vec![
        bitwise_batch(),
        arithmetic_batch(false),
        arithmetic_batch(true),
        context_sizing_batch(),
        comparison_batch(false),
        comparison_batch(true),
        shift_batch(),
        reduction_batch(),
        selection_batch(),
    ]
}

fn context_sizing_batch() -> Batch {
    let mut outputs = Vec::new();
    let mut assignments = Vec::new();
    for width in WIDTHS.into_iter().filter(|width| *width >= 2) {
        let narrow = width - 1;
        for (name, zero) in [("unsigned", "d0"), ("signed", "sd0")] {
            let signal = format!("context_{name}_{width}");
            outputs.push(format!("output logic [{width}:0] {signal}"));
            assignments.push(format!(
                "assign {signal} = ($signed(signed_a[{}:0]) + $signed(signed_b[{}:0])) + {width}'{zero};",
                narrow - 1,
                width - 1
            ));
        }
    }
    Batch {
        id: "context-sizing",
        points: outputs.len(),
        source: finish_module(module_prefix(), &outputs, &assignments),
    }
}

fn module_prefix() -> String {
    "// SPDX-FileCopyrightText: 2026 Zhengyi Zhang\n\
     // SPDX-License-Identifier: GPL-3.0-only\n\n\
     module top (\n\
       input logic [63:0] a, b,\n\
       input logic signed [63:0] signed_a, signed_b,\n\
       input logic [5:0] shift,\n\
       input logic select,\n"
        .to_string()
}

fn finish_module(mut source: String, outputs: &[String], assignments: &[String]) -> String {
    for (index, output) in outputs.iter().enumerate() {
        let suffix = if index + 1 == outputs.len() { "" } else { "," };
        writeln!(source, "  {output}{suffix}").unwrap();
    }
    source.push_str(");\n");
    for assignment in assignments {
        writeln!(source, "  {assignment}").unwrap();
    }
    source.push_str("endmodule\n");
    source
}

fn bitwise_batch() -> Batch {
    let mut outputs = Vec::new();
    let mut assignments = Vec::new();
    for width in WIDTHS {
        for (name, operator) in [("and", "&"), ("or", "|"), ("xor", "^")] {
            let signal = format!("{name}_{width}");
            outputs.push(format!("output logic [{}:0] {signal}", width - 1));
            assignments.push(format!(
                "assign {signal} = a[{}:0] {operator} b[{}:0];",
                width - 1,
                width - 1
            ));
        }
    }
    Batch {
        id: "bitwise",
        points: outputs.len(),
        source: finish_module(module_prefix(), &outputs, &assignments),
    }
}

fn arithmetic_batch(signed: bool) -> Batch {
    let mut outputs = Vec::new();
    let mut assignments = Vec::new();
    let prefix = if signed { "signed" } else { "unsigned" };
    let left = if signed { "signed_a" } else { "a" };
    let right = if signed { "signed_b" } else { "b" };
    for width in WIDTHS {
        for (name, operator) in [("add", "+"), ("sub", "-")] {
            let signal = format!("{prefix}_{name}_{width}");
            let signed_keyword = if signed { " signed" } else { "" };
            outputs.push(format!("output logic{signed_keyword} [{width}:0] {signal}"));
            let left = format!("{left}[{}:0]", width - 1);
            let right = format!("{right}[{}:0]", width - 1);
            let left = if signed {
                format!("$signed({left})")
            } else {
                left
            };
            let right = if signed {
                format!("$signed({right})")
            } else {
                right
            };
            assignments.push(format!("assign {signal} = {left} {operator} {right};"));
        }
    }
    Batch {
        id: if signed {
            "arithmetic-signed"
        } else {
            "arithmetic-unsigned"
        },
        points: outputs.len(),
        source: finish_module(module_prefix(), &outputs, &assignments),
    }
}

fn comparison_batch(signed: bool) -> Batch {
    let mut outputs = Vec::new();
    let mut assignments = Vec::new();
    let prefix = if signed { "signed" } else { "unsigned" };
    let left = if signed { "signed_a" } else { "a" };
    let right = if signed { "signed_b" } else { "b" };
    for width in WIDTHS {
        for (name, operator) in [
            ("eq", "=="),
            ("ne", "!="),
            ("lt", "<"),
            ("le", "<="),
            ("gt", ">"),
            ("ge", ">="),
        ] {
            let signal = format!("{prefix}_{name}_{width}");
            outputs.push(format!("output logic {signal}"));
            let left = format!("{left}[{}:0]", width - 1);
            let right = format!("{right}[{}:0]", width - 1);
            let left = if signed {
                format!("$signed({left})")
            } else {
                left
            };
            let right = if signed {
                format!("$signed({right})")
            } else {
                right
            };
            assignments.push(format!(
                "assign {signal} = ({left} {operator} {right}) ? 1'b1 : 1'b0;"
            ));
        }
    }
    Batch {
        id: if signed {
            "comparison-signed"
        } else {
            "comparison-unsigned"
        },
        points: outputs.len(),
        source: finish_module(module_prefix(), &outputs, &assignments),
    }
}

fn shift_batch() -> Batch {
    let mut outputs = Vec::new();
    let mut assignments = Vec::new();
    for width in WIDTHS {
        for (name, expression) in [
            ("left", format!("a[{}:0] << shift", width - 1)),
            ("right", format!("a[{}:0] >> shift", width - 1)),
            (
                "arithmetic",
                format!("$signed(signed_a[{}:0]) >>> shift", width - 1),
            ),
        ] {
            let signal = format!("shift_{name}_{width}");
            let signed_keyword = if name == "arithmetic" && width > 1 {
                " signed"
            } else {
                ""
            };
            outputs.push(format!(
                "output logic{signed_keyword} [{}:0] {signal}",
                width - 1
            ));
            assignments.push(format!("assign {signal} = {expression};"));
        }
    }
    Batch {
        id: "shifts",
        points: outputs.len(),
        source: finish_module(module_prefix(), &outputs, &assignments),
    }
}

fn reduction_batch() -> Batch {
    let mut outputs = Vec::new();
    let mut assignments = Vec::new();
    for width in WIDTHS {
        for (name, operator) in [("and", "&"), ("or", "|"), ("xor", "^")] {
            let signal = format!("reduce_{name}_{width}");
            outputs.push(format!("output logic {signal}"));
            assignments.push(format!("assign {signal} = {operator}a[{}:0];", width - 1));
        }
    }
    Batch {
        id: "reductions",
        points: outputs.len(),
        source: finish_module(module_prefix(), &outputs, &assignments),
    }
}

fn selection_batch() -> Batch {
    let mut outputs = Vec::new();
    let mut assignments = Vec::new();
    for width in WIDTHS {
        let mux = format!("mux_{width}");
        outputs.push(format!("output logic [{}:0] {mux}", width - 1));
        assignments.push(format!(
            "assign {mux} = select ? a[{}:0] : b[{}:0];",
            width - 1,
            width - 1
        ));
        let concat = format!("concat_{width}");
        outputs.push(format!("output logic [{}:0] {concat}", width * 2 - 1));
        assignments.push(format!(
            "assign {concat} = {{a[{}:0], b[{}:0]}};",
            width - 1,
            width - 1
        ));
        let extend = format!("extend_{width}");
        outputs.push(format!("output logic signed [64:0] {extend}"));
        assignments.push(format!(
            "assign {extend} = $signed(signed_a[{}:0]);",
            width - 1
        ));
    }
    Batch {
        id: "selection-concat-casts",
        points: outputs.len(),
        source: finish_module(module_prefix(), &outputs, &assignments),
    }
}
