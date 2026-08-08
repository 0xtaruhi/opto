// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::qualification_support::{
    RunMode, run_generated_differential, run_mapping_fixture_gate, run_named_suite,
    run_real_medium_gate, run_semantic_matrix, run_sv_tests, run_yosys_tests, validate_inventory,
};

#[test]
fn qualification_inventory_is_complete() {
    validate_inventory();
}

#[test]
fn presubmit_mapping_fixtures_are_host_independent() {
    run_mapping_fixture_gate("benchmarks/qor/suites/presubmit.toml");
}

#[test]
fn presubmit_corpus() {
    run_named_suite("qualification/suites/presubmit.toml", RunMode::Presubmit);
}

#[test]
fn generated_semantic_matrix() {
    run_semantic_matrix(RunMode::Presubmit);
}

#[test]
#[ignore = "requires OPTO_YOSYS and runs end-to-end CEC"]
fn presubmit_equivalence() {
    run_named_suite("qualification/suites/presubmit.toml", RunMode::Equivalence);
}

#[test]
#[ignore = "requires OPTO_YOSYS and proves every generated semantic point"]
fn generated_semantic_equivalence() {
    run_semantic_matrix(RunMode::Equivalence);
}

#[test]
#[ignore = "requires OPTO_YOSYS and proves fixed-seed generated designs"]
fn generated_differential() {
    run_generated_differential();
}

#[test]
#[ignore = "requires a pinned external Ibex checkout"]
fn upstream_ibex() {
    run_named_suite("qualification/suites/upstream-ibex.toml", RunMode::Upstream);
}

#[test]
#[ignore = "requires a pinned external CVA6 checkout"]
fn upstream_cva6() {
    run_named_suite("qualification/suites/upstream-cva6.toml", RunMode::Upstream);
}

#[test]
#[ignore = "requires the pinned external CHIPS Alliance sv-tests checkout"]
fn systemverilog_conformance() {
    run_sv_tests();
}

#[test]
#[ignore = "requires the pinned external Yosys checkout"]
fn yosys_rtl_qualification() {
    run_yosys_tests();
}

#[test]
#[ignore = "requires OPTO_YOSYS and OPTO_LIBRARY_SKY130_HD"]
fn public_qor() {
    run_named_suite("benchmarks/qor/suites/public.toml", RunMode::Qor);
}

#[test]
#[ignore = "requires OPTO_YOSYS"]
fn presubmit_qor() {
    run_named_suite("benchmarks/qor/suites/presubmit.toml", RunMode::Qor);
}

#[test]
#[ignore = "requires OPTO_YOSYS"]
fn extended_qor() {
    run_named_suite("benchmarks/qor/suites/extended.toml", RunMode::Qor);
}

#[test]
#[ignore = "requires two optimized Opto binaries, pinned real RTL and a public Liberty"]
fn real_medium_qor_regression() {
    run_real_medium_gate("benchmarks/real/gate.toml");
}
