// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Differential SAT-equivalence regression against an external Yosys build.
//!
//! The ignored test generates deterministic Boolean networks, derives a DNF
//! reference from each truth table, and checks that Opto and Yosys agree for
//! both equivalent and deliberately inverted outputs.

use opto_formal::{ProofOutcome, prove_logic_network_equivalence};
use opto_ir::logic::{Lit, LogicBuilder, LogicNetwork, NodeId, NodeKind};
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

struct GeneratedNetwork {
    network: LogicNetwork,
    output: Lit,
    verilog: String,
    input_count: usize,
}

#[test]
#[ignore = "requires OPTO_YOSYS for solver differential verification"]
fn opto_formal_agrees_with_yosys_sat() {
    let yosys = std::env::var("OPTO_YOSYS").expect("OPTO_YOSYS must name a Yosys executable");
    let cases = std::env::var("OPTO_FORMAL_DIFFERENTIAL_CASES")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(64);
    let directory = std::env::temp_dir().join(format!(
        "opto-formal-yosys-differential-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&directory).expect("create differential output directory");

    for seed in 0..cases {
        let implementation = random_network(seed);
        let truth = truth_table(&implementation.network, implementation.output);
        let (reference, reference_output) = dnf_network(implementation.input_count, truth);
        let should_match = seed % 2 == 0;
        let implementation_output = if should_match {
            implementation.output
        } else {
            implementation.output.inverted()
        };
        let outcome = prove_logic_network_equivalence(
            &reference,
            &[reference_output],
            &implementation.network,
            &[implementation_output],
        )
        .expect("generated logic miter is valid");
        assert_eq!(
            matches!(outcome, ProofOutcome::Proved(_)),
            should_match,
            "opto-formal verdict mismatch at seed {seed}"
        );

        let source = verilog_miter(&implementation, truth, !should_match);
        let path = directory.join(format!("case-{seed}.v"));
        std::fs::write(&path, source).expect("write differential Verilog");
        let yosys_proved = yosys_proves(&yosys, &path);
        assert_eq!(
            yosys_proved,
            should_match,
            "Yosys and opto-formal disagree at seed {seed}; input retained at {}",
            path.display()
        );
        std::fs::remove_file(path).expect("remove passing differential input");
    }
    std::fs::remove_dir(directory).expect("remove empty differential output directory");
}

fn random_network(seed: u64) -> GeneratedNetwork {
    let mut state = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let input_count = 2 + random_index(&mut state, 3);
    let mut builder = LogicBuilder::new();
    let mut values = Vec::new();
    let mut names = Vec::new();
    for input in 0..input_count {
        values.push(
            builder
                .input(u32::try_from(input).expect("the generator creates at most four inputs"))
                .expect("bounded input"),
        );
        names.push(format!("i{input}"));
    }
    let mut verilog = String::new();
    for gate in 0..(8 + random_index(&mut state, 17)) {
        let left_index = random_index(&mut state, values.len());
        let right_index = random_index(&mut state, values.len());
        let select_index = random_index(&mut state, values.len());
        let invert_left = random(&mut state) & 1 != 0;
        let invert_right = random(&mut state) & 1 != 0;
        let left = if invert_left {
            values[left_index].inverted()
        } else {
            values[left_index]
        };
        let right = if invert_right {
            values[right_index].inverted()
        } else {
            values[right_index]
        };
        let left_name = format!(
            "{}{}",
            if invert_left { "~" } else { "" },
            names[left_index]
        );
        let right_name = format!(
            "{}{}",
            if invert_right { "~" } else { "" },
            names[right_index]
        );
        let wire = format!("n{gate}");
        let (value, expression) = match random(&mut state) % 3 {
            0 => (
                builder
                    .and(
                        left,
                        right,
                        u32::try_from(gate).expect("the generator creates at most 24 gates"),
                    )
                    .expect("bounded gate"),
                format!("{left_name} & {right_name}"),
            ),
            1 => (
                builder
                    .xor(
                        left,
                        right,
                        u32::try_from(gate).expect("the generator creates at most 24 gates"),
                    )
                    .expect("bounded gate"),
                format!("{left_name} ^ {right_name}"),
            ),
            _ => (
                builder
                    .mux(
                        values[select_index],
                        left,
                        right,
                        u32::try_from(gate).expect("the generator creates at most 24 gates"),
                    )
                    .expect("bounded gate"),
                format!("{} ? {left_name} : {right_name}", names[select_index]),
            ),
        };
        let _ = writeln!(verilog, "  wire {wire} = {expression};");
        values.push(value);
        names.push(wire);
    }
    GeneratedNetwork {
        network: builder.freeze(),
        output: *values.last().expect("generated gate exists"),
        verilog: format!(
            "{verilog}  wire implementation = {};\n",
            names.last().unwrap()
        ),
        input_count,
    }
}

fn random(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    *state
}

fn random_index(state: &mut u64, upper_bound: usize) -> usize {
    let bound = u64::try_from(upper_bound).expect("test collection lengths fit u64");
    usize::try_from(random(state) % bound)
        .expect("the remainder is strictly below a usize-derived bound")
}

fn truth_table(network: &LogicNetwork, output: Lit) -> u64 {
    let input_count = (0..network.node_count())
        .filter_map(|index| NodeId::from_index(index).ok())
        .filter(|&node| network.kind(node) == Some(NodeKind::Input))
        .count();
    let mut truth = 0u64;
    for assignment in 0..(1usize << input_count) {
        let mut values = vec![false; network.node_count()];
        for index in 0..network.node_count() {
            let node = NodeId::from_index(index).expect("stored node index");
            values[index] = match network.kind(node).expect("stored node kind") {
                NodeKind::Constant => false,
                NodeKind::Input => {
                    let origin = network.origin(node).expect("input origin") as usize;
                    assignment & (1 << origin) != 0
                }
                NodeKind::And => {
                    literal_value(network.fanin(node, 0).unwrap(), &values)
                        && literal_value(network.fanin(node, 1).unwrap(), &values)
                }
                NodeKind::Xor => {
                    literal_value(network.fanin(node, 0).unwrap(), &values)
                        ^ literal_value(network.fanin(node, 1).unwrap(), &values)
                }
                NodeKind::Mux => {
                    if literal_value(network.fanin(node, 0).unwrap(), &values) {
                        literal_value(network.fanin(node, 1).unwrap(), &values)
                    } else {
                        literal_value(network.fanin(node, 2).unwrap(), &values)
                    }
                }
            };
        }
        if literal_value(output, &values) {
            truth |= 1 << assignment;
        }
    }
    truth
}

fn literal_value(literal: Lit, values: &[bool]) -> bool {
    values[literal.node().index()] ^ literal.is_inverted()
}

fn dnf_network(input_count: usize, truth: u64) -> (LogicNetwork, Lit) {
    let mut builder = LogicBuilder::new();
    let inputs = (0..input_count)
        .map(|index| {
            builder
                .input(u32::try_from(index).expect("truth-table input count fits u32"))
                .expect("bounded input")
        })
        .collect::<Vec<_>>();
    let mut output = Lit::FALSE;
    for assignment in 0..(1usize << input_count) {
        if truth & (1 << assignment) == 0 {
            continue;
        }
        let mut term = Lit::TRUE;
        for (index, input) in inputs.iter().copied().enumerate() {
            let literal = if assignment & (1 << index) == 0 {
                input.inverted()
            } else {
                input
            };
            term = builder.and(term, literal, 0).expect("bounded DNF");
        }
        output = builder.or(output, term, 0).expect("bounded DNF");
    }
    (builder.freeze(), output)
}

fn verilog_miter(generated: &GeneratedNetwork, truth: u64, invert: bool) -> String {
    let ports = (0..generated.input_count)
        .map(|index| format!("i{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let terms = (0..(1usize << generated.input_count))
        .filter(|assignment| truth & (1 << assignment) != 0)
        .map(|assignment| {
            (0..generated.input_count)
                .map(|index| {
                    format!(
                        "{}i{index}",
                        if assignment & (1 << index) == 0 {
                            "~"
                        } else {
                            ""
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join(" & ")
        })
        .collect::<Vec<_>>();
    let reference = if terms.is_empty() {
        "1'b0".to_string()
    } else {
        terms.join(" | ")
    };
    format!(
        "module miter(input {ports}, output bad);\n{}  wire reference = {reference};\n  assign bad = reference ^ {}implementation;\nendmodule\n",
        generated.verilog,
        if invert { "~" } else { "" }
    )
}

fn yosys_proves(yosys: &str, path: &Path) -> bool {
    let script = format!(
        "read_verilog {}; prep -top miter; sat -verify -prove bad 0",
        path.display()
    );
    Command::new(yosys)
        .args(["-q", "-p", &script])
        .status()
        .expect("run Yosys SAT")
        .success()
}
