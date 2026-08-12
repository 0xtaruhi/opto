// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{load_suite, workspace_root};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub(super) fn validate() {
    let root = workspace_root();
    assert_exact_inventory(
        &root,
        "qualification/cases",
        &["qualification/suites/presubmit.toml"],
    );
    assert_exact_inventory(
        &root,
        "qualification/upstream",
        &[
            "qualification/suites/upstream-ibex.toml",
            "qualification/suites/upstream-cva6.toml",
        ],
    );
    assert_exact_inventory(
        &root,
        "benchmarks/qor/cases",
        &[
            "benchmarks/qor/suites/presubmit.toml",
            "benchmarks/qor/suites/extended.toml",
            "benchmarks/qor/suites/weekly.toml",
        ],
    );
    let public = suite_cases(&root, "benchmarks/qor/suites/public.toml");
    let weekly = suite_cases(&root, "benchmarks/qor/suites/weekly.toml");
    assert!(
        public.is_subset(&weekly),
        "the public QoR suite must be a subset of the weekly suite"
    );
}

fn assert_exact_inventory(root: &Path, corpus: &str, suites: &[&str]) {
    let mut discovered = BTreeSet::new();
    collect_descriptors(&root.join(corpus), &mut discovered);
    let mut registered = BTreeSet::new();
    for suite in suites {
        for path in suite_cases(root, suite) {
            assert!(
                registered.insert(path.clone()),
                "case descriptor is registered more than once: {}",
                path.display()
            );
        }
    }
    let orphaned = discovered.difference(&registered).collect::<Vec<_>>();
    let missing = registered.difference(&discovered).collect::<Vec<_>>();
    assert!(
        orphaned.is_empty() && missing.is_empty(),
        "qualification inventory mismatch for {corpus}; orphaned={orphaned:?}, missing={missing:?}"
    );
}

fn suite_cases(root: &Path, relative: &str) -> BTreeSet<PathBuf> {
    let (_, cases) = load_suite(root, &root.join(relative));
    cases.into_iter().map(|case| case.path).collect()
}

fn collect_descriptors(directory: &Path, descriptors: &mut BTreeSet<PathBuf>) {
    for entry in std::fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
    {
        let path = entry.expect("read qualification inventory entry").path();
        if path.is_dir() {
            collect_descriptors(&path, descriptors);
        } else if path.file_name().is_some_and(|name| name == "case.toml") {
            descriptors.insert(path);
        }
    }
}
