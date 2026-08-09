// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Exercises checkpoint decoding and session restoration with arbitrary bytes.

#![no_main]

use libfuzzer_sys::fuzz_target;
use std::path::PathBuf;
use std::sync::OnceLock;

fn input_path() -> &'static PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        std::env::temp_dir().join(format!("opto-checkpoint-fuzz-{}.ock", std::process::id()))
    })
}

fuzz_target!(|data: &[u8]| {
    let path = input_path();
    if std::fs::write(path, data).is_err() {
        return;
    }
    let mut session = opto_session::Session::new();
    let _ = session.read_checkpoint_file(path);
});
