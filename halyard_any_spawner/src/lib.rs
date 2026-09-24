//! This crate makes it easier to write asynchronous code that is executor-agnostic, by providing a
//! utility that can be used to spawn tasks in a variety of executors.
//!
//! It only supports single executor per program, but that executor can be set at runtime, anywhere
//! in your crate (or an application that depends on it).
//!
//! Two executors are built in: Tokio (the `tokio` feature, for the server) and
//! `wasm-bindgen-futures` (the `wasm-bindgen` feature, for the browser). Any other executor or
//! runtime that supports spawning [`Future`]s can be plugged in with a [`CustomExecutor`].
//!
//! This is a least common denominator implementation in many ways. Limitations include:
//! - setting an executor is a one-time, global action
//! - no "join handle" or other result is returned from the spawn
//! - the `Future` must output `()`
//!
//! ```no_run
//! use halyard_any_spawner::Executor;
//!
//! // make sure an Executor has been initialized with one of the init_ functions
//!
//! // spawn a thread-safe Future
//! Executor::spawn(async { /* ... */ });
//!
//! // spawn a Future that is !Send
//! Executor::spawn_local(async { /* ... */ });
//! ```
//!
//! # When a task cannot be spawned
//!
//! Spawning never panics. A task that cannot be spawned (no executor has been set yet; the
//! executor is Tokio and `spawn` is called outside a Tokio runtime; executors are set per
//! thread and this thread has none; the thread is exiting) is dropped without running, and
//! the first task dropped for each reason is logged: with `tracing` when that feature is on,
//! otherwise in the browser's console or on standard error. Setting an executor twice is an
//! [`ExecutorError`] the caller can ignore: the first executor stays.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]

mod error;

use error::{Method, ReportOncePerMethod, Unspawned};
use std::{future::Future, panic::Location, pin::Pin, sync::OnceLock};
use thiserror::Error;

/// A future that has been pinned.
pub type PinnedFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;
/// A future that has been pinned.
pub type PinnedLocalFuture<T> = Pin<Box<dyn Future<Output = T>>>;

/// The executor that one of the `init_*` functions set. Each variant holds what it spawns
/// into, so spawning never looks up state that might be missing.
enum GlobalExecutor {
    #[cfg(feature = "tokio")]
    Tokio,
    #[cfg(feature = "wasm-bindgen")]
    WasmBindgen,
    Custom(Box<dyn CustomExecutor + Send + Sync>),
    /// `init_local_custom_executor`: each thread uses the executor it set.
    PerThread,
}

static EXECUTOR: OnceLock<GlobalExecutor> = OnceLock::new();

impl GlobalExecutor {
    #[track_caller]
    fn spawn(&self, fut: PinnedFuture<()>) {
        match self {
            #[cfg(feature = "tokio")]
            GlobalExecutor::Tokio => {
                match tokio::runtime::Handle::try_current() {
                    Ok(runtime) => {
                        runtime.spawn(fut);
                    }
                    // `tokio::spawn` would panic here
                    Err(_) => {
                        static REPORTED: error::ReportOnce =
                            error::ReportOnce::new();
                        let caller = Location::caller();
                        REPORTED.report(|| Unspawned::OutsideTokioRuntime {
                            caller,
                        });
                        drop(fut);
                    }
                }
            }
            // wasm-bindgen-futures runs every task on the current thread (in the browser,
            // the only one), so a thread-safe task runs as a local one
            #[cfg(feature = "wasm-bindgen")]
            GlobalExecutor::WasmBindgen => {
                wasm_bindgen_futures::spawn_local(fut)
            }
            GlobalExecutor::Custom(executor) => executor.spawn(fut),
            GlobalExecutor::PerThread => per_thread::spawn(fut),
        }
    }

    #[track_caller]
    fn spawn_local(&self, fut: PinnedLocalFuture<()>) {
        match self {
            #[cfg(feature = "tokio")]
            GlobalExecutor::Tokio => {
                tokio::task::spawn_local(fut);
            }
            #[cfg(feature = "wasm-bindgen")]
            GlobalExecutor::WasmBindgen => {
                wasm_bindgen_futures::spawn_local(fut)
            }
            GlobalExecutor::Custom(executor) => executor.spawn_local(fut),
            GlobalExecutor::PerThread => per_thread::spawn_local(fut),
        }
    }

    fn poll_local(&self) {
        match self {
            // Tokio and the browser's event loop drive their own tasks
            #[cfg(feature = "tokio")]
            GlobalExecutor::Tokio => {}
            #[cfg(feature = "wasm-bindgen")]
            GlobalExecutor::WasmBindgen => {}
            GlobalExecutor::Custom(executor) => executor.poll_local(),
            GlobalExecutor::PerThread => per_thread::poll_local(),
        }
    }
}

/// Sets the global executor, if none is set yet.
fn set(executor: GlobalExecutor) -> Result<(), ExecutorError> {
    EXECUTOR
        .set(executor)
        .map_err(|_| ExecutorError::AlreadySet)
}

/// Why an executor could not be set.
#[derive(Error, Debug)]
pub enum ExecutorError {
    /// An executor has already been set, and it stays. This is usually safe to ignore:
    /// halyard's `mount` functions and server integrations set one every time they start.
    #[error("Global executor has already been set.")]
    AlreadySet,
    /// This thread is exiting (its thread-local values are being destroyed), so it cannot
    /// hold an executor any more.
    #[error("this thread is exiting, so no executor can be set for it")]
    ThreadExiting,
}

/// A global async executor that can spawn tasks.
pub struct Executor;

impl Executor {
    /// Spawns a thread-safe [`Future`].
    ///
    /// Uses the globally configured executor. If no executor has been set, the task is
    /// dropped without running, and the first such task is logged (see the crate's
    /// documentation).
    #[inline(always)]
    #[track_caller]
    pub fn spawn(fut: impl Future<Output = ()> + Send + 'static) {
        let pinned_fut = Box::pin(fut);

        match EXECUTOR.get() {
            Some(executor) => executor.spawn(pinned_fut),
            None => {
                no_executor(Method::Spawn);
                drop(pinned_fut);
            }
        }
    }

    /// Spawns a [`Future`] that cannot be sent across threads.
    ///
    /// Uses the globally configured executor. If no executor has been set, the task is
    /// dropped without running, and the first such task is logged (see the crate's
    /// documentation).
    #[inline(always)]
    #[track_caller]
    pub fn spawn_local(fut: impl Future<Output = ()> + 'static) {
        let pinned_fut = Box::pin(fut);

        match EXECUTOR.get() {
            Some(executor) => executor.spawn_local(pinned_fut),
            None => {
                no_executor(Method::SpawnLocal);
                drop(pinned_fut);
            }
        }
    }

    /// Waits until the next "tick" of the current async executor.
    /// Respects the global executor.
    ///
    /// Returns at once if the task it spawns to wait for the tick is dropped (for example,
    /// because no executor has been set).
    #[inline(always)]
    pub async fn tick() {
        let (tx, rx) = futures::channel::oneshot::channel();
        #[cfg(not(all(feature = "wasm-bindgen", target_family = "wasm")))]
        Executor::spawn(async move {
            _ = tx.send(());
        });
        #[cfg(all(feature = "wasm-bindgen", target_family = "wasm"))]
        Executor::spawn_local(async move {
            _ = tx.send(());
        });

        _ = rx.await;
    }

    /// Polls the global async executor.
    ///
    /// Uses the globally configured executor.
    /// Does nothing if the global executor does not support polling, or if none is set.
    #[inline(always)]
    pub fn poll_local() {
        if let Some(executor) = EXECUTOR.get() {
            executor.poll_local();
        }
    }
}

impl Executor {
    /// Globally sets the [`tokio`] runtime as the executor used to spawn tasks.
    ///
    /// `spawn` outside a Tokio runtime drops the task (logged once). `spawn_local` must
    /// be called inside a `tokio::task::LocalSet`: Tokio panics otherwise, and offers no
    /// way to ask whether the current thread is inside one.
    ///
    /// Returns `Err(ExecutorError::AlreadySet)` if a global executor has already been set.
    ///
    /// Requires the `tokio` feature to be activated on this crate.
    #[cfg(feature = "tokio")]
    #[cfg_attr(docsrs, doc(cfg(feature = "tokio")))]
    pub fn init_tokio() -> Result<(), ExecutorError> {
        set(GlobalExecutor::Tokio)
    }

    /// Globally sets the [`wasm-bindgen-futures`] runtime as the executor used to spawn tasks.
    ///
    /// wasm-bindgen-futures runs every task on the current thread, so `spawn` and
    /// `spawn_local` both spawn there.
    ///
    /// Returns `Err(ExecutorError::AlreadySet)` if a global executor has already been set.
    ///
    /// Requires the `wasm-bindgen` feature to be activated on this crate.
    #[cfg(feature = "wasm-bindgen")]
    #[cfg_attr(docsrs, doc(cfg(feature = "wasm-bindgen")))]
    pub fn init_wasm_bindgen() -> Result<(), ExecutorError> {
        set(GlobalExecutor::WasmBindgen)
    }

    /// Globally sets a custom executor as the executor used to spawn tasks.
    ///
    /// Requires the custom executor to be `Send + Sync` as it will be stored statically.
    ///
    /// Returns `Err(ExecutorError::AlreadySet)` if a global executor has already been set.
    pub fn init_custom_executor(
        custom_executor: impl CustomExecutor + Send + Sync + 'static,
    ) -> Result<(), ExecutorError> {
        set(GlobalExecutor::Custom(Box::new(custom_executor)))
    }

    /// Sets a custom executor *for the current thread*.
    ///
    /// The first successful call makes executors per thread for the whole program: from
    /// then on, `spawn`, `spawn_local` and `poll_local` use the executor that the calling
    /// thread set. Every thread that spawns tasks must set its own; on a thread that has
    /// none, a spawned task is dropped without running (logged once) and `poll_local` does
    /// nothing.
    ///
    /// The provided `custom_executor` must implement [`CustomExecutor`] and `'static`, but does
    /// **not** need to be `Send` or `Sync`.
    ///
    /// Returns `Err(ExecutorError::AlreadySet)` if this thread has already set one, or if a
    /// shared executor was set with another `init_*` function (it is the one every thread
    /// uses), and `Err(ExecutorError::ThreadExiting)` if called while this thread is exiting.
    pub fn init_local_custom_executor(
        custom_executor: impl CustomExecutor + 'static,
    ) -> Result<(), ExecutorError> {
        if EXECUTOR.get().is_some_and(|executor| {
            !matches!(executor, GlobalExecutor::PerThread)
        }) {
            return Err(ExecutorError::AlreadySet);
        }
        per_thread::set(Box::new(custom_executor))?;
        match EXECUTOR.get_or_init(|| GlobalExecutor::PerThread) {
            GlobalExecutor::PerThread => Ok(()),
            // another thread set a shared executor in the meantime; it wins
            _ => Err(ExecutorError::AlreadySet),
        }
    }
}

/// A trait for custom executors.
/// Custom executors can be used to integrate with any executor that supports spawning futures.
///
/// If used with `init_custom_executor`, the implementation must be `Send + Sync + 'static`.
///
/// All methods can be called recursively. Implementors should be mindful of potential
/// deadlocks or excessive resource consumption if recursive calls are not handled carefully
/// (e.g., using `try_borrow_mut` or non-blocking polls within implementations).
pub trait CustomExecutor {
    /// Spawns a future, usually on a thread pool.
    fn spawn(&self, fut: PinnedFuture<()>);
    /// Spawns a local future. May require calling `poll_local` to make progress.
    fn spawn_local(&self, fut: PinnedLocalFuture<()>);
    /// Polls the executor, if it supports polling. Implementations should ideally be
    /// non-blocking or use mechanisms like `try_tick` or `try_borrow_mut` to handle
    /// re-entrant calls safely.
    fn poll_local(&self);
}

/// A task was spawned before any executor was set: it is dropped (by the caller) and the
/// first one for each method is logged.
#[cold]
#[inline(never)]
#[track_caller]
fn no_executor(method: Method) {
    static REPORTED: ReportOncePerMethod = ReportOncePerMethod::new();
    let caller = Location::caller();
    REPORTED
        .get(method)
        .report(|| Unspawned::NotSet { method, caller });
}

/// The executors set with [`Executor::init_local_custom_executor`], one per thread.
mod per_thread {
    use crate::{
        error::{Method, ReportOncePerMethod, Unspawned},
        CustomExecutor, ExecutorError, PinnedFuture, PinnedLocalFuture,
    };
    use std::{cell::OnceCell, panic::Location};

    thread_local! {
        static EXECUTOR: OnceCell<Box<dyn CustomExecutor>> = const { OnceCell::new() };
    }

    pub(crate) fn set(
        executor: Box<dyn CustomExecutor>,
    ) -> Result<(), ExecutorError> {
        EXECUTOR
            .try_with(|cell| {
                cell.set(executor).map_err(|_| ExecutorError::AlreadySet)
            })
            .unwrap_or(Err(ExecutorError::ThreadExiting))
    }

    /// Calls `f` with this thread's executor; `false` if it has none (or is exiting), and
    /// then `f` is dropped without being called.
    fn with(f: impl FnOnce(&dyn CustomExecutor)) -> bool {
        EXECUTOR
            .try_with(|cell| cell.get().map(|executor| f(executor.as_ref())))
            .ok()
            .flatten()
            .is_some()
    }

    #[track_caller]
    pub(crate) fn spawn(fut: PinnedFuture<()>) {
        if !with(|executor| executor.spawn(fut)) {
            none_on_this_thread(Method::Spawn);
        }
    }

    #[track_caller]
    pub(crate) fn spawn_local(fut: PinnedLocalFuture<()>) {
        if !with(|executor| executor.spawn_local(fut)) {
            none_on_this_thread(Method::SpawnLocal);
        }
    }

    pub(crate) fn poll_local() {
        // nothing to poll on a thread without an executor
        _ = with(|executor| executor.poll_local());
    }

    #[cold]
    #[inline(never)]
    #[track_caller]
    fn none_on_this_thread(method: Method) {
        static REPORTED: ReportOncePerMethod = ReportOncePerMethod::new();
        let caller = Location::caller();
        REPORTED
            .get(method)
            .report(|| Unspawned::NotSetOnThisThread { method, caller });
    }
}
