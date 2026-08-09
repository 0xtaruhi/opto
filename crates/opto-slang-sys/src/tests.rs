// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

mod assignments;
mod compilation;
mod elaboration;
mod expressions;
mod inventory;
mod processes;
mod types;

use super::*;
use std::path::PathBuf;

fn compile_source(source: &NativeTestSource) -> SlangCompilation {
    compile(
        std::slice::from_ref(&source.path),
        &SlangCompileOptions::default(),
    )
    .expect("native slang should compile test source")
}

fn first_module(compilation: &SlangCompilation) -> SlangMaterializedModule<'_> {
    compilation
        .modules()
        .next()
        .expect("module should exist")
        .materialize()
        .expect("module should materialize")
}

fn module_named<'a>(compilation: &'a SlangCompilation, name: &str) -> SlangMaterializedModule<'a> {
    compilation
        .modules()
        .find(|module| module.name().expect("module name should be valid") == name)
        .unwrap_or_else(|| panic!("module {name:?} should exist"))
        .materialize()
        .expect("module should materialize")
}

fn materialized_modules(compilation: &SlangCompilation) -> Vec<SlangMaterializedModule<'_>> {
    compilation
        .modules()
        .map(|module| module.materialize().expect("module should materialize"))
        .collect()
}

fn is_signal(expression: SlangExpression<'_>, expected: &str) -> bool {
    matches!(
        expression.kind().unwrap(),
        SlangExpressionKind::Signal(SlangSignalRef { name, range: None }) if name == expected
    )
}

fn procedure_effects(procedure: SlangProcedure<'_>) -> Vec<SlangEffect<'_>> {
    procedure.blocks().flat_map(SlangBlock::effects).collect()
}

fn first_effect(procedure: SlangProcedure<'_>) -> SlangEffect<'_> {
    procedure_effects(procedure)
        .into_iter()
        .next()
        .expect("procedure should contain an effect")
}

fn entry_block(procedure: SlangProcedure<'_>) -> SlangBlock<'_> {
    procedure
        .block(procedure.entry())
        .expect("procedure entry should be valid")
}

fn first_branch(
    procedure: SlangProcedure<'_>,
) -> (
    SlangExpression<'_>,
    SlangEdgeTarget<'_>,
    SlangEdgeTarget<'_>,
) {
    procedure
        .blocks()
        .find_map(|block| match block.terminator().kind().unwrap() {
            SlangTerminatorKind::Branch {
                condition,
                then_edge,
                else_edge,
            } => Some((condition, then_edge, else_edge)),
            _ => None,
        })
        .expect("procedure should contain a branch")
}

fn first_switch(
    procedure: SlangProcedure<'_>,
) -> (
    SlangExpression<'_>,
    SlangSwitchArms<'_>,
    SlangEdgeTarget<'_>,
) {
    procedure
        .blocks()
        .find_map(|block| match block.terminator().kind().unwrap() {
            SlangTerminatorKind::Switch {
                selector,
                arms,
                default,
            } => Some((selector, arms, default)),
            _ => None,
        })
        .expect("procedure should contain a switch")
}

fn source_unit(source: &NativeTestSource, define: &str, value: &str) -> SlangSourceUnit {
    SlangSourceUnit {
        files: vec![source.snapshot()],
        dependencies: Vec::new(),
        include_paths: Vec::new(),
        defines: vec![SlangDefine {
            name: define.to_string(),
            value: Some(value.to_string()),
        }],
        language: SlangLanguage::SystemVerilog2017,
    }
}

fn assert_semantic_inventory<'a>(
    upstream: impl std::borrow::Borrow<std::collections::BTreeSet<&'a str>>,
    lowered: &[&str],
    rejected: &[&str],
) {
    let upstream = upstream.borrow();
    let lowered = lowered
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    let rejected = rejected
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    assert!(
        lowered.is_disjoint(&rejected),
        "semantic inventory classifies kinds as both lowered and rejected: {:?}",
        lowered.intersection(&rejected).collect::<Vec<_>>()
    );
    let classified = lowered.union(&rejected).copied().collect::<_>();
    assert_eq!(
        *upstream, classified,
        "Slang semantic kinds changed; classify every new kind as lowered or explicitly rejected"
    );
}

fn ast_visitor_case_kinds(switch_marker: &str) -> std::collections::BTreeSet<&'static str> {
    const VISITOR: &str = include_str!(concat!(
        env!("OPTO_SLANG_VENDOR_DIR_RESOLVED"),
        "/include/slang/ast/ASTVisitor.h"
    ));
    let start = VISITOR
        .find(switch_marker)
        .unwrap_or_else(|| panic!("missing AST visitor marker {switch_marker:?}"));
    let section = &VISITOR[start..];
    let end = section
        .find("#undef CASE")
        .expect("AST visitor switch should end with #undef CASE");
    section[..end]
        .lines()
        .filter_map(|line| {
            let arguments = line.trim().strip_prefix("CASE(")?;
            Some(
                arguments
                    .split_once(',')
                    .expect("AST visitor CASE should have a kind and type")
                    .0
                    .trim(),
            )
        })
        .collect()
}

fn operator_kinds(index: usize) -> std::collections::BTreeSet<&'static str> {
    const OPERATORS: &str = include_str!(concat!(
        env!("OPTO_SLANG_VENDOR_DIR_RESOLVED"),
        "/include/slang/ast/expressions/Operator.h"
    ));
    let section = OPERATORS
        .split("#define OP(x)")
        .nth(index + 1)
        .unwrap_or_else(|| panic!("missing Slang operator block {index}"));
    let end = section
        .find("SLANG_ENUM(")
        .expect("Slang operator block should end with SLANG_ENUM");
    section[..end]
        .lines()
        .filter_map(|line| {
            let kind = line.trim().strip_prefix("x(")?;
            let kind = kind.strip_suffix('\\').unwrap_or(kind).trim_end();
            kind.strip_suffix(')')
        })
        .collect()
}

struct NativeTestSource {
    dir: PathBuf,
    path: PathBuf,
}

impl NativeTestSource {
    fn new(source_text: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};

        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after UNIX_EPOCH")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "opto-slang-native-test-{}-{unique}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("failed to create native slang test directory");
        let path = dir.join("top.sv");
        std::fs::write(&path, source_text).expect("failed to write native slang test source");
        Self { dir, path }
    }

    fn snapshot(&self) -> SlangSourceFile {
        SlangSourceFile {
            path: self.path.clone(),
            text: std::fs::read_to_string(&self.path)
                .expect("failed to snapshot native slang test source"),
        }
    }
}

impl Drop for NativeTestSource {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).expect("failed to remove native slang test directory");
    }
}
