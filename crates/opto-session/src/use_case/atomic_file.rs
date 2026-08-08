// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Durable same-directory file replacement shared by product writers.

use crate::SessionError;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) fn write_atomically(
    path: &Path,
    operation: &'static str,
    write: impl FnOnce(&mut File) -> Result<(), SessionError>,
) -> Result<(), SessionError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().ok_or_else(|| {
        SessionError::state(format!(
            "{operation}: output path '{}' has no file name",
            path.display()
        ))
    })?;
    let mut temporary = None;
    for _ in 0..32 {
        let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".{}.opto-tmp-{}-{sequence}",
            file_name.to_string_lossy(),
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => {
                temporary = Some((candidate, file));
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(SessionError::Io {
                    operation,
                    path: candidate,
                    source,
                });
            }
        }
    }
    let (temporary_path, mut file) = temporary.ok_or_else(|| {
        SessionError::state(format!(
            "{operation}: could not allocate a temporary output file"
        ))
    })?;
    let result = write(&mut file).and_then(|()| {
        file.sync_all().map_err(|source| SessionError::Io {
            operation,
            path: temporary_path.clone(),
            source,
        })
    });
    drop(file);
    let result = result.and_then(|()| {
        fs::rename(&temporary_path, path).map_err(|source| SessionError::Io {
            operation,
            path: PathBuf::from(path),
            source,
        })
    });
    let result = result.and_then(|()| sync_directory(parent, path, operation));
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary_path);
        return Err(error);
    }
    Ok(())
}

#[cfg(unix)]
fn sync_directory(parent: &Path, path: &Path, operation: &'static str) -> Result<(), SessionError> {
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| SessionError::Io {
            operation,
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(not(unix))]
fn sync_directory(
    _parent: &Path,
    _path: &Path,
    _operation: &'static str,
) -> Result<(), SessionError> {
    Ok(())
}
