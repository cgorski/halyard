//! What `halyard_any_spawner` does instead of panicking (README, "Project policy": no panics,
//! ever) when a task cannot be spawned: the task is dropped without running, and the first
//! task dropped for each reason is logged. There is no caller to tell (spawning returns
//! nothing), so the log is the only trace. Each variant says why the task was dropped.

use std::{
    fmt,
    panic::Location,
    sync::atomic::{AtomicBool, Ordering},
};
use thiserror::Error;

/// Why a task was dropped instead of spawned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub(crate) enum Unspawned {
    /// No `Executor::init_*` function has been called yet.
    #[error(
        "`{method}` was called at {caller} before an executor was set (call one of the \
         `Executor::init_*` functions first, as halyard's `mount` functions and server \
         integrations do); the task is dropped without running"
    )]
    NotSet {
        method: Method,
        caller: &'static Location<'static>,
    },
    /// The executor was set per thread (`init_local_custom_executor`), and this thread has
    /// none.
    #[error(
        "`{method}` was called at {caller} on a thread that has no executor: executors were \
         set per thread with `Executor::init_local_custom_executor`, and this thread did not \
         set one (or it is exiting); the task is dropped without running"
    )]
    NotSetOnThisThread {
        method: Method,
        caller: &'static Location<'static>,
    },
    /// The executor is Tokio, and `spawn` was called outside a Tokio runtime.
    #[cfg(feature = "tokio")]
    #[error(
        "`Executor::spawn` was called at {caller} outside a Tokio runtime (the executor is \
         Tokio: spawn from inside the runtime, or enter it with `Handle::enter`); the task is \
         dropped without running"
    )]
    OutsideTokioRuntime { caller: &'static Location<'static> },
    /// The executor is glib, and `spawn_local` was called on a thread that does not own
    /// glib's default main context.
    #[cfg(feature = "glib")]
    #[error(
        "`Executor::spawn_local` was called at {caller} on a thread that does not own glib's \
         default main context (another thread owns it: spawn local tasks from that thread); \
         the task is dropped without running"
    )]
    GlibContextOwnedElsewhere { caller: &'static Location<'static> },
    /// This thread's local executor is gone: the thread is exiting, and a thread-local
    /// value's destructor spawned the task.
    #[cfg(any(feature = "futures-executor", feature = "async-executor"))]
    #[error(
        "`{method}` was called at {caller} while this thread is exiting, after its local \
         executor was destroyed; the task is dropped without running"
    )]
    ThreadExiting {
        method: Method,
        caller: &'static Location<'static>,
    },
}

/// Which spawning function a dropped task was given to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Method {
    Spawn,
    SpawnLocal,
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Method::Spawn => "Executor::spawn",
            Method::SpawnLocal => "Executor::spawn_local",
        })
    }
}

/// Said after every report: only the first task dropped for a reason is logged.
const ONCE: &str =
    "(logged once: later tasks dropped for the same reason are not logged)";

/// Logs a dropped task: with `tracing` when that feature is on, otherwise in the browser's
/// console, or on standard error.
fn report(error: &Unspawned) {
    #[cfg(feature = "tracing")]
    tracing::error!("{error} {ONCE}");
    #[cfg(all(
        not(feature = "tracing"),
        target_arch = "wasm32",
        target_os = "unknown"
    ))]
    web_sys::console::error_1(
        &format!("[halyard_any_spawner] {error} {ONCE}").into(),
    );
    #[cfg(all(
        not(feature = "tracing"),
        not(all(target_arch = "wasm32", target_os = "unknown"))
    ))]
    {
        use std::io::Write;
        // `eprintln!` panics if standard error is closed; with nowhere left to report the
        // error, it is dropped
        _ = writeln!(std::io::stderr(), "[halyard_any_spawner] {error} {ONCE}");
    }
}

/// Reports one kind of dropped task the first time it happens, and never again: a task
/// that cannot be spawned usually means every later one cannot either.
pub(crate) struct ReportOnce(AtomicBool);

impl ReportOnce {
    pub(crate) const fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    /// Reports the error that `error` builds, if this kind has not been reported yet.
    pub(crate) fn report(&self, error: impl FnOnce() -> Unspawned) {
        if !self.0.swap(true, Ordering::Relaxed) {
            report(&error());
        }
    }
}

/// One [`ReportOnce`] for each spawning function.
pub(crate) struct ReportOncePerMethod {
    spawn: ReportOnce,
    spawn_local: ReportOnce,
}

impl ReportOncePerMethod {
    pub(crate) const fn new() -> Self {
        Self {
            spawn: ReportOnce::new(),
            spawn_local: ReportOnce::new(),
        }
    }

    pub(crate) fn get(&self, method: Method) -> &ReportOnce {
        match method {
            Method::Spawn => &self.spawn,
            Method::SpawnLocal => &self.spawn_local,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The log says which function was called, where, why the task was dropped, and how
    /// to fix it.
    #[test]
    fn messages_say_where_why_and_what_to_do() {
        let here = Location::caller();

        let none = Unspawned::NotSet {
            method: Method::SpawnLocal,
            caller: here,
        }
        .to_string();
        assert!(
            none.starts_with(&format!(
                "`Executor::spawn_local` was called at {here} before an executor was set"
            )),
            "{none}"
        );
        assert!(none.contains("`Executor::init_*`"), "{none}");
        assert!(
            none.ends_with("the task is dropped without running"),
            "{none}"
        );

        let thread = Unspawned::NotSetOnThisThread {
            method: Method::Spawn,
            caller: here,
        }
        .to_string();
        assert!(
            thread.starts_with(&format!(
                "`Executor::spawn` was called at {here}"
            )),
            "{thread}"
        );
        assert!(thread.contains("init_local_custom_executor"), "{thread}");

        #[cfg(feature = "tokio")]
        {
            let tokio =
                Unspawned::OutsideTokioRuntime { caller: here }.to_string();
            assert!(tokio.contains("outside a Tokio runtime"), "{tokio}");
        }

        #[cfg(feature = "glib")]
        {
            let glib = Unspawned::GlibContextOwnedElsewhere { caller: here }
                .to_string();
            assert!(
                glib.contains("does not own glib's default main context"),
                "{glib}"
            );
        }

        #[cfg(any(feature = "futures-executor", feature = "async-executor"))]
        {
            let exiting = Unspawned::ThreadExiting {
                method: Method::SpawnLocal,
                caller: here,
            }
            .to_string();
            assert!(
                exiting.contains("while this thread is exiting"),
                "{exiting}"
            );
        }
    }

    #[test]
    fn report_once_reports_only_the_first_time() {
        let once = ReportOnce::new();
        let mut built = 0;
        for _ in 0..3 {
            once.report(|| {
                built += 1;
                Unspawned::NotSet {
                    method: Method::Spawn,
                    caller: Location::caller(),
                }
            });
        }
        assert_eq!(built, 1);
    }

    #[test]
    fn each_method_is_reported_once_on_its_own() {
        let per_method = ReportOncePerMethod::new();
        let mut built = Vec::new();
        for method in [
            Method::Spawn,
            Method::SpawnLocal,
            Method::Spawn,
            Method::SpawnLocal,
        ] {
            per_method.get(method).report(|| {
                built.push(method);
                Unspawned::NotSet {
                    method,
                    caller: Location::caller(),
                }
            });
        }
        assert_eq!(built, [Method::Spawn, Method::SpawnLocal]);
    }
}
