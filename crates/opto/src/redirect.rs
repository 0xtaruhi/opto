// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RedirectTargetKind {
    File,
    Variable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RedirectOptions {
    pub(crate) target_kind: RedirectTargetKind,
    pub(crate) target: String,
    pub(crate) script: String,
    pub(crate) append: bool,
    pub(crate) tee: bool,
}

impl RedirectOptions {
    pub(crate) fn write_output(&self, output: &str) -> Result<(), crate::ShellError> {
        let mut options = OpenOptions::new();
        options.create(true);
        if self.append {
            options.append(true);
        } else {
            options.write(true).truncate(true);
        }
        let mut file = options
            .open(&self.target)
            .map_err(|source| crate::ShellError::Io {
                path: PathBuf::from(&self.target),
                source,
            })?;
        file.write_all(output.as_bytes())
            .map_err(|source| crate::ShellError::Io {
                path: PathBuf::from(&self.target),
                source,
            })
    }
}

pub(crate) fn command_output(value: &str) -> String {
    if value.is_empty() || value.ends_with('\n') {
        value.to_string()
    } else {
        format!("{value}\n")
    }
}
