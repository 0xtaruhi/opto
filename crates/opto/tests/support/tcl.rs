// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use std::path::Path;

pub(crate) fn tcl_path_text(path: &Path) -> String {
    let path = path.to_string_lossy();
    #[cfg(windows)]
    let path = path.replace('\\', "/");
    #[cfg(not(windows))]
    let path = path.into_owned();
    path
}

pub(crate) fn tcl_path_word(path: &Path) -> String {
    let path = tcl_path_text(path);
    let mut encoded = String::with_capacity(path.len() + 2);
    encoded.push('"');
    for character in path.chars() {
        match character {
            '\\' => encoded.push_str("\\\\"),
            '"' => encoded.push_str("\\\""),
            '$' => encoded.push_str("\\$"),
            '[' => encoded.push_str("\\["),
            '\n' => encoded.push_str("\\n"),
            '\r' => encoded.push_str("\\r"),
            '\t' => encoded.push_str("\\t"),
            character => encoded.push(character),
        }
    }
    encoded.push('"');
    encoded
}
