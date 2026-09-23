//! What the reactive graph does instead of panicking, deadlocking or aborting (README,
//! "Project policy": no panics, ever): a typed error, logged (once, where it would otherwise
//! repeat on every access), and a recovery that leaves the graph usable. Each variant says
//! what happens instead.

use std::{
    fmt,
    panic::Location,
    sync::atomic::{AtomicBool, Ordering},
};
use thiserror::Error;

/// A failure that the reactive graph recovered from.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub(crate) enum GraphError {
    /// A reactive value was accessed while this thread already held its lock: from inside
    /// its own `with`/`update`/`with_value`/`update_value` closure, or while a guard from its
    /// `read`/`write` was alive. Waiting would never end (a deadlock natively, an abort in
    /// the browser).
    #[error(
        "a reactive value{} was {access} while this thread already holds its lock (inside its \
         own `with`, `update`, `with_value` or `update_value`, or while a guard from its \
         `read` or `write` is alive); {}",
        created(.defined_at),
        access.instead()
    )]
    Reentered {
        access: Access,
        defined_at: Option<&'static Location<'static>>,
    },
    /// A memo was read, after its sources changed, while this thread held a guard on its
    /// value: it cannot store a new value until that guard is dropped.
    #[error(
        "a memo{} was read after its sources changed, while this thread holds a guard on \
         its value; it gives its previous value, and recomputes once the guard is dropped",
        created(.defined_at)
    )]
    MemoBorrowed {
        defined_at: Option<&'static Location<'static>>,
    },
    /// An `ImmediateEffect::new_mut` effect was triggered while its function was running.
    #[error(
        "an ImmediateEffect{} made with `new_mut` was triggered again while its function was \
         still running (it wrote a signal it reads, or another thread triggered it at the \
         same time); that run is skipped",
        created(.defined_at)
    )]
    EffectRetriggered {
        defined_at: Option<&'static Location<'static>>,
    },
    /// The subscriber of an effect that is not running was asked for.
    #[error(
        "{what} has no running effect (it was stopped, or effects do not run without the \
         `effects` feature); it is used as a subscriber that tracks nothing"
    )]
    NotRunning { what: &'static str },
    /// With `sandboxed-arenas`, the arena was used on a thread where none is active.
    #[error(
        "the `sandboxed-arenas` feature is on, but no arena is active on this thread (at \
         {at}; set an owner first); {instead}"
    )]
    NoArena {
        at: &'static Location<'static>,
        instead: &'static str,
    },
    /// A value stored with `LocalStorage` was reached from another thread.
    #[error(
        "a thread-local reactive value (`LocalStorage`) was used on a thread other than the \
         one that created it; {instead}"
    )]
    WrongThread { instead: &'static str },
    /// A handle was derived from an arena handle whose value is gone.
    #[error(
        "{what} of a reactive value{} whose owner was disposed; {instead}",
        created(.defined_at)
    )]
    Disposed {
        what: &'static str,
        instead: &'static str,
        defined_at: Option<&'static Location<'static>>,
    },
}

/// How a reactive value was accessed when it was found locked by the same thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    Read,
    Write,
}

impl Access {
    fn instead(self) -> &'static str {
        match self {
            Access::Read => "the read gives nothing (its `try_*` form returns `None`)",
            Access::Write => {
                "the write is refused (its `try_*` form returns `None`, `try_set` returns \
                 the value), instead of waiting forever; write after the closure returns or \
                 the guard is dropped"
            }
        }
    }
}

impl fmt::Display for Access {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Access::Read => "read",
            Access::Write => "written",
        })
    }
}

/// ` (created at file:line:column)`, where that is known (in debug builds).
fn created(defined_at: &Option<&'static Location<'static>>) -> String {
    defined_at
        .map(|at| format!(" (created at {at})"))
        .unwrap_or_default()
}

/// Logs a failure that the graph recovered from: with `tracing` when that feature is on,
/// otherwise in the browser's console, or on standard error.
pub(crate) fn report(error: &GraphError) {
    #[cfg(feature = "tracing")]
    tracing::warn!("{error}");
    #[cfg(all(
        not(feature = "tracing"),
        target_arch = "wasm32",
        target_os = "unknown"
    ))]
    web_sys::console::warn_1(&format!("[halyard] {error}").into());
    #[cfg(all(
        not(feature = "tracing"),
        not(all(target_arch = "wasm32", target_os = "unknown"))
    ))]
    {
        use std::io::Write;
        // `eprintln!` panics if standard error is closed; with nowhere left to report the
        // error, it is dropped
        _ = writeln!(std::io::stderr(), "[halyard] {error}");
    }
}

/// Reports one kind of failure the first time it happens, and never again: for failures
/// that would otherwise be logged on every access.
pub(crate) struct ReportOnce(AtomicBool);

impl ReportOnce {
    pub(crate) const fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    /// Reports the error that `error` builds, if this kind has not been reported yet.
    pub(crate) fn report(&self, error: impl FnOnce() -> GraphError) {
        if !self.0.swap(true, Ordering::Relaxed) {
            report(&error());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The log says what was accessed, where it was created, and what happens instead; and
    /// says nothing about a creation site it does not know (release builds).
    #[test]
    fn messages_say_where_and_what_happens_instead() {
        let here = Location::caller();

        let write = GraphError::Reentered {
            access: Access::Write,
            defined_at: Some(here),
        }
        .to_string();
        assert!(
            write.starts_with(&format!(
                "a reactive value (created at {here}) was written while this thread \
                 already holds its lock"
            )),
            "{write}"
        );
        assert!(write.contains("the write is refused"), "{write}");

        let read = GraphError::Reentered {
            access: Access::Read,
            defined_at: None,
        }
        .to_string();
        assert!(
            read.starts_with("a reactive value was read while"),
            "{read}"
        );
        assert!(read.ends_with("returns `None`)"), "{read}");
    }

    #[test]
    fn report_once_reports_only_the_first_time() {
        let once = ReportOnce::new();
        let mut built = 0;
        for _ in 0..3 {
            once.report(|| {
                built += 1;
                GraphError::NotRunning { what: "a test" }
            });
        }
        assert_eq!(built, 1);
    }
}
