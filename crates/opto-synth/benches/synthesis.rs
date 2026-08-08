// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

#![allow(
    missing_docs,
    reason = "criterion generates an undocumented public benchmark-group entry point"
)]

//! Runtime regression benchmarks for the public synthesis entry point.
//!
//! Each case drives `SynthesisEngine::synthesis`, so one measurement
//! covers normalization, resource planning, cut enumeration, Boolean
//! rewriting, technology mapping, and post-map optimization. Shapes are chosen
//! so a regression in one of those phases moves a distinguishable case.

use criterion::{Criterion, criterion_group, criterion_main};
use opto_ir::rtl::RtlModule;
use opto_ir::word::{
    BinaryOp, LValue, LogicStateKind, PortDirection, SourceSpan, WordModule, WordType,
};
use opto_runtime::{ExecutionConfig, ExecutionContext};
use opto_synth::{SynthesisEngine, SynthesisOptions, SynthesisRequest};
use std::borrow::Cow;
use std::collections::BTreeSet;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate lives two levels below the workspace root")
        .to_path_buf()
}

fn target_cells() -> opto_library::TargetCellSet {
    let path = workspace_root().join("qualification/libraries/opto_test.lib");
    opto_library::read_lib_input(&path)
        .expect("benchmark Liberty input parses")
        .target_cells()
        .clone()
}

fn word_type(width: u32) -> WordType {
    WordType::new(width, false, LogicStateKind::FourState).expect("benchmark width is valid")
}

fn source(role: impl AsRef<[u8]>) -> SourceSpan {
    SourceSpan::stable(role)
}

fn input(module: &mut WordModule, name: &str, width: u32) -> opto_ir::word::ValueId {
    let source = source(format!("benchmark/input/{name}"));
    let port = module
        .add_port(name, PortDirection::Input, word_type(width), source.clone())
        .expect("benchmark port is unique");
    let signal = module.port(port).expect("port was just added").signal;
    module
        .read_signal(signal, source)
        .expect("benchmark input reads its own signal")
}

fn output(module: &mut WordModule, name: &str, width: u32, value: opto_ir::word::ValueId) {
    let source = source(format!("benchmark/output/{name}"));
    let port = module
        .add_port(
            name,
            PortDirection::Output,
            word_type(width),
            source.clone(),
        )
        .expect("benchmark port is unique");
    let signal = module.port(port).expect("port was just added").signal;
    module
        .connect(LValue::signal(signal), value, source)
        .expect("benchmark output drives its own signal");
}

/// A chain of dependent adds. Exercises arithmetic recognition, carry-chain
/// resource planning and deep-cone cut enumeration.
fn adder_chain(width: u32, stages: usize) -> WordModule {
    let mut module = WordModule::new("adder_chain");
    let mut accumulator = input(&mut module, "seed", width);
    for stage in 0..stages {
        let operand = input(&mut module, &format!("operand{stage}"), width);
        accumulator = module
            .binary(
                BinaryOp::Add,
                accumulator,
                operand,
                source(format!("benchmark/adder-chain/stage/{stage}")),
            )
            .expect("benchmark add is well typed");
    }
    output(&mut module, "sum", width, accumulator);
    module
}

/// A balanced tree of independent XOR/AND cones. Exercises Boolean rewriting
/// and cut-based covering on wide, shallow logic.
fn boolean_tree(width: u32, leaves: usize) -> WordModule {
    let mut module = WordModule::new("boolean_tree");
    let mut level = (0..leaves)
        .map(|leaf| input(&mut module, &format!("leaf{leaf}"), width))
        .collect::<Vec<_>>();
    let mut alternate = false;
    let mut depth = 0;
    while level.len() > 1 {
        let operator = if alternate {
            BinaryOp::BitAnd
        } else {
            BinaryOp::BitXor
        };
        alternate = !alternate;
        level = level
            .chunks(2)
            .enumerate()
            .map(|(pair_index, pair)| match pair {
                [left, right] => module
                    .binary(
                        operator,
                        *left,
                        *right,
                        source(format!(
                            "benchmark/boolean-tree/depth/{depth}/pair/{pair_index}"
                        )),
                    )
                    .expect("benchmark operation is well typed"),
                [single] => *single,
                _ => unreachable!("chunks(2) yields one or two elements"),
            })
            .collect();
        depth += 1;
    }
    output(&mut module, "y", width, level[0]);
    module
}

/// A product followed by dependent sums. Exercises multiplier architecture
/// selection and compressor-tree mapping.
fn multiply_accumulate(width: u32, terms: usize) -> WordModule {
    let mut module = WordModule::new("multiply_accumulate");
    let mut accumulator = input(&mut module, "seed", width);
    for term in 0..terms {
        let left = input(&mut module, &format!("left{term}"), width);
        let right = input(&mut module, &format!("right{term}"), width);
        let product = module
            .binary(
                BinaryOp::Mul,
                left,
                right,
                source(format!(
                    "benchmark/multiply-accumulate/term/{term}/multiply"
                )),
            )
            .expect("benchmark multiply is well typed");
        accumulator = module
            .binary(
                BinaryOp::Add,
                accumulator,
                product,
                source(format!("benchmark/multiply-accumulate/term/{term}/add")),
            )
            .expect("benchmark add is well typed");
    }
    output(&mut module, "acc", width, accumulator);
    module
}

fn synthesis_request<'a>(
    source: &'a RtlModule,
    cells: &opto_library::TargetCellSet,
) -> SynthesisRequest<'a> {
    let design_id = opto_timing::DesignId::from_uid(
        opto_core::ObjectUid::from_raw(1).expect("benchmark design identity is nonzero"),
    );
    let port_bindings = opto_timing::PortBindings::new(
        source.word().ports().iter().enumerate().map(|(index, _)| {
            let uid = u64::try_from(index)
                .ok()
                .and_then(|index| index.checked_add(2))
                .and_then(opto_core::ObjectUid::from_raw)
                .expect("benchmark port identity fits a permanent UID");
            opto_timing::PortId::from_uid(uid)
        }),
    );
    SynthesisRequest {
        base_revision: opto_ir::RevisionId::INITIAL,
        design_id,
        port_bindings,
        object_bindings: Arc::new(opto_timing::TimingObjectBindings::new()),
        source: Cow::Borrowed(source),
        design_references: Arc::new(BTreeSet::new()),
        reference_ports: Arc::new(opto_synth::ReferencePortMap::new()),
        options: SynthesisOptions {
            target_cells: cells.clone(),
        },
        effort: opto_synth::SynthesisEffort::Medium,
        clock_gating: None,
        scenarios: opto_timing::ScenarioSet::single(
            Arc::new(opto_timing::TimingContext::default()),
            Arc::new(opto_timing::TimingLibrary {
                cells: cells.clone(),
                ..opto_timing::TimingLibrary::default()
            }),
            opto_timing::Parasitics::default(),
        ),
        power_evaluator: Arc::new(opto_synth::NoPowerEvaluation),
        previous_incremental: None,
    }
}

fn run_synthesis(
    engine: &SynthesisEngine,
    runtime: &ExecutionContext,
    source: &RtlModule,
    cells: &opto_library::TargetCellSet,
) {
    let result = engine
        .synthesize(synthesis_request(source, cells), runtime, &mut |_| {})
        .expect("benchmark synthesis succeeds");
    black_box(result.mapped().cell_count());
}

fn benchmark_synthesis(criterion: &mut Criterion) {
    let cells = target_cells();
    let engine = SynthesisEngine::new();
    let runtime = ExecutionContext::new(&ExecutionConfig { max_threads: 1 })
        .expect("benchmark runtime starts");

    // Sizes are chosen so every case stays near or below 200 ms. The gate
    // detects a proportional slowdown, so a larger design would only make the
    // nightly comparison slower without making it more sensitive.
    let cases: [(&str, WordModule); 4] = [
        ("adder-chain/8x4", adder_chain(8, 4)),
        ("boolean-tree/1x64", boolean_tree(1, 64)),
        ("boolean-tree/8x32", boolean_tree(8, 32)),
        ("multiply-accumulate/6x2", multiply_accumulate(6, 2)),
    ];

    let mut group = criterion.benchmark_group("synth");
    for (name, module) in cases {
        let source = RtlModule::structural(module).expect("benchmark module is structural");
        group.bench_function(name, |bencher| {
            bencher.iter(|| run_synthesis(&engine, &runtime, &source, &cells));
        });
    }
    group.finish();
}

criterion_group!(benches, benchmark_synthesis);
criterion_main!(benches);
