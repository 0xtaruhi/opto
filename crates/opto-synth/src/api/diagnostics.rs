// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Low-overhead synthesis tracing enabled by [`crate::SynthesisDiagnostics`].
//!
//! [`SynthTrace`] is the single sink for synthesis diagnostics. It owns the
//! enable gate and the record format, so passes never test a diagnostics flag
//! or write to `stderr` themselves. Every record is one `event=<name> ...` line,
//! which keeps profiling spans and pass events readable by the same consumer.

/// A gated diagnostic sink for one synthesis pass.
///
/// Copying is free; pass it by value into workers that need to trace.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SynthTrace {
    enabled: bool,
}

impl SynthTrace {
    /// Creates a sink that emits only when `enabled`.
    pub(crate) const fn new(enabled: bool) -> Self {
        Self { enabled }
    }

    /// Creates a sink gated on the timing-diagnostics control.
    pub(crate) const fn timing(diagnostics: crate::SynthesisDiagnostics) -> Self {
        Self::new(diagnostics.timing)
    }

    /// Returns whether records are emitted, for callers that must avoid
    /// building an expensive measurement at all.
    pub(crate) const fn is_enabled(self) -> bool {
        self.enabled
    }

    /// Returns a sink that emits only when this one does and `condition` holds.
    pub(crate) const fn and(self, condition: bool) -> Self {
        Self::new(self.enabled && condition)
    }

    /// Emits one record. Prefer [`trace!`], which skips formatting when the
    /// sink is disabled.
    pub(crate) fn emit(self, event: &str, fields: std::fmt::Arguments<'_>) {
        if self.enabled {
            eprintln!("event={event} {fields}");
        }
    }

    /// Starts a span that reports its wall time on every exit path.
    pub(crate) fn span(self, label: impl FnOnce() -> String) -> ProfileSpan {
        ProfileSpan::new(self.enabled, label)
    }
}

/// Emits one [`SynthTrace`] record, formatting the fields only when the sink is
/// enabled.
macro_rules! trace {
    ($trace:expr, $event:literal, $($fields:tt)*) => {{
        let trace = $trace;
        if $crate::api::diagnostics::SynthTrace::is_enabled(trace) {
            $crate::api::diagnostics::SynthTrace::emit(trace, $event, format_args!($($fields)*));
        }
    }};
}

pub(crate) use trace;

/// A wall-clock profiling span that reports on every exit path.
pub(crate) struct ProfileSpan {
    active: Option<ActiveProfileSpan>,
}

struct ActiveProfileSpan {
    label: String,
    started: std::time::Instant,
}

impl ProfileSpan {
    /// Starts a named span only when profiling is enabled.
    pub(crate) fn new(enabled: bool, label: impl FnOnce() -> String) -> Self {
        Self {
            active: enabled.then(|| ActiveProfileSpan {
                label: label(),
                started: std::time::Instant::now(),
            }),
        }
    }
}

impl Drop for ProfileSpan {
    fn drop(&mut self) {
        let Some(active) = &self.active else {
            return;
        };
        eprintln!(
            "event=span label={} wall_ms={:.3}",
            active.label,
            active.started.elapsed().as_secs_f64() * 1_000.0,
        );
    }
}
