// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

mod collections;
mod constraints;
mod core;
mod design_hdl;
mod directives;
mod frontend;
mod power;
mod reports;

fn test_commands() -> CommandRegistry {
    let mut registry = CommandRegistry::new();
    registry.register_group(commands::ALL).unwrap();
    registry
}

fn temp_script_path(name: &str) -> crate::test_support::TestPath {
    crate::test_support::TestPath::new(name)
}

fn test_target_setup() -> String {
    let library = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../qualification/libraries/opto_test.lib");
    format!("read_libs [list {}]", tcl_path_word(&library))
}

#[test]
fn tcl_path_words_preserve_paths_without_substitution() {
    let path = std::path::Path::new(
        "directory with spaces/$name/[command]/{braces}/\"quote\"/a\\b/line\nreturn\rcarriage\ttab",
    );
    let mut runtime = Runtime::new(Session::new()).unwrap();
    let result = runtime
        .eval(&format!("set path {}", tcl_path_word(path)))
        .unwrap();

    assert!(matches!(result, EvalResult::Complete(value) if value == tcl_path_text(path)));
}
