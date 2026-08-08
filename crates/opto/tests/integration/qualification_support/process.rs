// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

#![allow(
    unsafe_code,
    reason = "this Unix-only test adapter calls wait4 to capture child resource usage"
)]

use super::schema::Metrics;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::Instant;

const TOOL_OVERRIDE_PREFIX: &str = "OPTO_";

pub(super) struct CommandOutput {
    pub(super) status: ExitStatus,
    pub(super) metrics: Metrics,
}

pub(super) fn run(
    program: &Path,
    arguments: impl IntoIterator<Item = impl AsRef<OsStr>>,
    environment: &BTreeMap<String, PathBuf>,
    log: &Path,
    timed: bool,
) -> CommandOutput {
    let output =
        File::create(log).unwrap_or_else(|error| panic!("create {}: {error}", log.display()));
    let error_output = output
        .try_clone()
        .unwrap_or_else(|error| panic!("clone {}: {error}", log.display()));
    let arguments = arguments
        .into_iter()
        .map(|argument| argument.as_ref().to_owned())
        .collect::<Vec<_>>();
    let started = Instant::now();
    let working_directory = log.parent().unwrap_or_else(|| {
        panic!(
            "qualification log {} has no parent directory",
            log.display()
        )
    });
    if !timed {
        let status = command(program, &arguments, environment, working_directory)
            .stdout(Stdio::from(output))
            .stderr(Stdio::from(error_output))
            .status()
            .unwrap_or_else(|error| panic!("run {}: {error}", program.display()));
        return CommandOutput {
            status,
            metrics: Metrics {
                wall_seconds: started.elapsed().as_secs_f64(),
                user_seconds: 0.0,
                system_seconds: 0.0,
                cpu_seconds: 0.0,
                peak_rss_kib: 0,
            },
        };
    }

    run_timed(
        program,
        &arguments,
        environment,
        working_directory,
        output,
        error_output,
        started,
    )
}

fn command(
    program: &Path,
    arguments: &[std::ffi::OsString],
    environment: &BTreeMap<String, PathBuf>,
    working_directory: &Path,
) -> Command {
    let mut command = Command::new(program);
    command.args(arguments);
    command.current_dir(working_directory);
    for key in tool_override_keys(std::env::vars_os().map(|(key, _)| key)) {
        command.env_remove(key);
    }
    for (key, value) in environment {
        command.env(key, value);
    }
    command
}

fn tool_override_keys(keys: impl Iterator<Item = std::ffi::OsString>) -> Vec<std::ffi::OsString> {
    keys.filter(|key| {
        key.to_str()
            .is_some_and(|key| key.starts_with(TOOL_OVERRIDE_PREFIX))
    })
    .collect()
}

#[cfg(unix)]
fn run_timed(
    program: &Path,
    arguments: &[std::ffi::OsString],
    environment: &BTreeMap<String, PathBuf>,
    working_directory: &Path,
    output: File,
    error_output: File,
    started: Instant,
) -> CommandOutput {
    use std::mem::MaybeUninit;
    use std::os::unix::process::ExitStatusExt;

    let child = command(program, arguments, environment, working_directory)
        .stdout(Stdio::from(output))
        .stderr(Stdio::from(error_output))
        .spawn()
        .unwrap_or_else(|error| panic!("run {}: {error}", program.display()));
    let child_id = libc::pid_t::try_from(child.id()).expect("child process id fits pid_t");
    let mut raw_status = 0;
    let mut usage = MaybeUninit::<libc::rusage>::zeroed();
    loop {
        // SAFETY: `child_id` names the live child created above, `raw_status` and
        // `usage` are valid writable storage, and no other code waits on this child.
        let waited = unsafe { libc::wait4(child_id, &raw mut raw_status, 0, usage.as_mut_ptr()) };
        if waited == child_id {
            break;
        }
        let error = std::io::Error::last_os_error();
        assert!(
            error.kind() == std::io::ErrorKind::Interrupted,
            "wait for {}: {error}",
            program.display()
        );
    }
    // SAFETY: a successful `wait4` initialized the complete `rusage` value.
    let usage = unsafe { usage.assume_init() };
    drop(child);

    let user_seconds = timeval_seconds(usage.ru_utime);
    let system_seconds = timeval_seconds(usage.ru_stime);
    let peak_rss = u64::try_from(usage.ru_maxrss).expect("peak RSS is non-negative");
    let peak_rss_kib = if cfg!(target_os = "macos") {
        peak_rss / 1024
    } else {
        peak_rss
    };
    CommandOutput {
        status: ExitStatus::from_raw(raw_status),
        metrics: Metrics {
            wall_seconds: started.elapsed().as_secs_f64(),
            user_seconds,
            system_seconds,
            cpu_seconds: user_seconds + system_seconds,
            peak_rss_kib,
        },
    }
}

#[cfg(unix)]
fn timeval_seconds(value: libc::timeval) -> f64 {
    let seconds = u64::try_from(value.tv_sec).expect("resource usage seconds are non-negative");
    let microseconds = u32::try_from(value.tv_usec)
        .expect("resource usage microseconds are non-negative and normalized");
    std::time::Duration::new(seconds, microseconds * 1_000).as_secs_f64()
}

#[cfg(not(unix))]
fn run_timed(
    _program: &Path,
    _arguments: &[std::ffi::OsString],
    _environment: &BTreeMap<String, PathBuf>,
    _working_directory: &Path,
    _output: File,
    _error_output: File,
    _started: Instant,
) -> CommandOutput {
    panic!("timed qualification requires Unix wait4 resource accounting")
}

#[cfg(test)]
mod tests {
    use super::tool_override_keys;
    use std::ffi::OsString;

    #[test]
    fn synthesis_overrides_are_stripped_from_inherited_environments() {
        let inherited = [
            "PATH",
            "OPTO_NO_REWRITE",
            "HOME",
            "OPTO_MUL_ARCH",
            "OPTOMISTIC",
            "OPTO_TIMING_EVAL_BUDGET",
        ]
        .into_iter()
        .map(OsString::from);

        assert_eq!(
            tool_override_keys(inherited),
            [
                "OPTO_NO_REWRITE",
                "OPTO_MUL_ARCH",
                "OPTO_TIMING_EVAL_BUDGET"
            ]
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_clean_environment_strips_nothing() {
        let inherited = ["PATH", "HOME"].into_iter().map(OsString::from);

        assert!(tool_override_keys(inherited).is_empty());
    }
}
