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

fn temp_script_path(name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "{}-{}-{name}",
        env!("CARGO_PKG_NAME"),
        std::process::id()
    ));
    path
}

fn test_target_setup() -> String {
    let library = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../qualification/libraries/opto_test.lib");
    format!("read_libs [list {}]", library.display())
}
