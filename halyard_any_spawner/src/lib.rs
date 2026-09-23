//! This crate makes it easier to write asynchronous code that is executor-agnostic, by providing a
//! utility that can be used to spawn tasks in a variety of executors.
//!
//! It only supports single executor per program, but that executor can be set at runtime, anywhere
//! in your crate (or an application that depends on it).
//!
//! This can be extended to support any executor or runtime that supports spawning [`Future`]s.
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
    #[cfg(feature = "glib")]
    Glib,
    /// `spawn` uses the pool; `spawn_local` uses this thread's `LocalPool`.
    #[cfg(feature = "futures-executor")]
    FuturesExecutor(futures::executor::ThreadPool),
    /// `spawn` uses the executor; `spawn_local` uses this thread's `LocalExecutor`.
    #[cfg(feature = "async-executor")]
    AsyncExecutor(async_executor::Executor<'static>),
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
            #[cfg(feature = "glib")]
            GlobalExecutor::Glib => {
                let main_context = glib::MainContext::default();
                main_context.spawn(fut);
            }
            #[cfg(feature = "futures-executor")]
            GlobalExecutor::FuturesExecutor(pool) => pool.spawn_ok(fut),
            #[cfg(feature = "async-executor")]
            GlobalExecutor::AsyncExecutor(executor) => {
                executor.spawn(fut).detach();
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
            #[cfg(feature = "glib")]
            GlobalExecutor::Glib => {
                let main_context = glib::MainContext::default();
                // glib panics if another thread owns the context; acquiring it first (which
                // the owning thread can do again) tells the two apart
                match main_context.acquire() {
                    Ok(_owned) => {
                        main_context.spawn_local(fut);
                    }
                    Err(_) => {
                        static REPORTED: error::ReportOnce =
                            error::ReportOnce::new();
                        let caller = Location::caller();
                        REPORTED.report(|| {
                            Unspawned::GlibContextOwnedElsewhere { caller }
                        });
                        drop(fut);
                    }
                };
            }
            #[cfg(feature = "futures-executor")]
            GlobalExecutor::FuturesExecutor(_) => {
                futures_local::spawn_local(fut)
            }
            #[cfg(feature = "async-executor")]
            GlobalExecutor::AsyncExecutor(_) => async_local::spawn_local(fut),
            GlobalExecutor::Custom(executor) => executor.spawn_local(fut),
            GlobalExecutor::PerThread => per_thread::spawn_local(fut),
        }
    }

    fn poll_local(&self) {
        match self {
            // Tokio, the browser's event loop and glib's main loop drive their own tasks
            #[cfg(feature = "tokio")]
            GlobalExecutor::Tokio => {}
            #[cfg(feature = "wasm-bindgen")]
            GlobalExecutor::WasmBindgen => {}
            #[cfg(feature = "glib")]
            GlobalExecutor::Glib => {}
            #[cfg(feature = "futures-executor")]
            GlobalExecutor::FuturesExecutor(_) => futures_local::poll(),
            #[cfg(feature = "async-executor")]
            GlobalExecutor::AsyncExecutor(_) => async_local::poll(),
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
    /// The `futures` executor's thread pool could not be started (the system could not
    /// create its threads, or the target has none). No executor was set, so another one can
    /// be.
    #[error("the futures executor's thread pool could not be started: {0}")]
    ThreadPool(#[source] std::io::Error),
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

    /// Globally sets the [`glib`] runtime as the executor used to spawn tasks.
    ///
    /// `spawn_local` on a thread other than the one that owns glib's default main context
    /// drops the task (logged once).
    ///
    /// Returns `Err(ExecutorError::AlreadySet)` if a global executor has already been set.
    ///
    /// Requires the `glib` feature to be activated on this crate.
    #[cfg(feature = "glib")]
    #[cfg_attr(docsrs, doc(cfg(feature = "glib")))]
    pub fn init_glib() -> Result<(), ExecutorError> {
        set(GlobalExecutor::Glib)
    }

    /// Globally sets the [`futures`] executor as the executor used to spawn tasks: `spawn`
    /// uses a thread pool (one thread per CPU), started here, and `spawn_local` uses a
    /// `LocalPool` for each thread, run by [`Executor::poll_local`].
    ///
    /// Returns `Err(ExecutorError::AlreadySet)` if a global executor has already been set,
    /// and `Err(ExecutorError::ThreadPool)` if the thread pool could not be started (then no
    /// executor is set).
    ///
    /// Requires the `futures-executor` feature to be activated on this crate.
    #[cfg(feature = "futures-executor")]
    #[cfg_attr(docsrs, doc(cfg(feature = "futures-executor")))]
    pub fn init_futures_executor() -> Result<(), ExecutorError> {
        // a pool started only to be refused would leave its threads idle
        if EXECUTOR.get().is_some() {
            return Err(ExecutorError::AlreadySet);
        }
        set_futures_executor(futures::executor::ThreadPool::new())
    }

    /// Globally sets the [`async_executor`] executor as the executor used to spawn tasks:
    /// `spawn` uses a global `Executor`, and `spawn_local` uses a `LocalExecutor` for each
    /// thread, ticked by [`Executor::poll_local`].
    ///
    /// Returns `Err(ExecutorError::AlreadySet)` if a global executor has already been set.
    ///
    /// Requires the `async-executor` feature to be activated on this crate.
    #[cfg(feature = "async-executor")]
    #[cfg_attr(docsrs, doc(cfg(feature = "async-executor")))]
    pub fn init_async_executor() -> Result<(), ExecutorError> {
        set(GlobalExecutor::AsyncExecutor(
            async_executor::Executor::new(),
        ))
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

/// Sets the `futures` executor, if its thread pool started.
#[cfg(feature = "futures-executor")]
fn set_futures_executor(
    pool: std::io::Result<futures::executor::ThreadPool>,
) -> Result<(), ExecutorError> {
    let pool = pool.map_err(ExecutorError::ThreadPool)?;
    set(GlobalExecutor::FuturesExecutor(pool))
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

/// A task was spawned while this thread's local executor is already destroyed.
#[cfg(any(feature = "futures-executor", feature = "async-executor"))]
#[cold]
#[inline(never)]
#[track_caller]
fn thread_exiting(method: Method) {
    static REPORTED: ReportOncePerMethod = ReportOncePerMethod::new();
    let caller = Location::caller();
    REPORTED
        .get(method)
        .report(|| Unspawned::ThreadExiting { method, caller });
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

/// The `futures` executor's `LocalPool` for each thread.
#[cfg(feature = "futures-executor")]
mod futures_local {
    use crate::{error::Method, thread_exiting, PinnedLocalFuture};
    use futures::{
        executor::{LocalPool, LocalSpawner},
        task::LocalSpawnExt,
    };
    use std::cell::RefCell;

    /// This thread's pool and a spawner into it, made together so that spawning never
    /// borrows the pool (which is borrowed while it runs its tasks).
    struct Local {
        pool: RefCell<LocalPool>,
        spawner: LocalSpawner,
    }

    thread_local! {
        static LOCAL: Local = {
            let pool = LocalPool::new();
            let spawner = pool.spawner();
            Local {
                pool: RefCell::new(pool),
                spawner,
            }
        };
    }

    #[track_caller]
    pub(crate) fn spawn_local(fut: PinnedLocalFuture<()>) {
        // either error means the pool is gone: the thread is exiting, and a thread-local
        // value's destructor spawned this task
        let spawned = LOCAL.try_with(|local| local.spawner.spawn_local(fut));
        if !matches!(spawned, Ok(Ok(()))) {
            thread_exiting(Method::SpawnLocal);
        }
    }

    pub(crate) fn poll() {
        _ = LOCAL.try_with(|local| {
            // already borrowed: a task the pool is running polled it; nothing to do
            if let Ok(mut pool) = local.pool.try_borrow_mut() {
                pool.run_until_stalled();
            }
        });
    }
}

/// The `async-executor` executor's `LocalExecutor` for each thread.
#[cfg(feature = "async-executor")]
mod async_local {
    use crate::{error::Method, thread_exiting, PinnedLocalFuture};
    use async_executor::LocalExecutor;

    thread_local! {
        static LOCAL: LocalExecutor<'static> = const { LocalExecutor::new() };
    }

    #[track_caller]
    pub(crate) fn spawn_local(fut: PinnedLocalFuture<()>) {
        if LOCAL.try_with(|local| local.spawn(fut).detach()).is_err() {
            thread_exiting(Method::SpawnLocal);
        }
    }

    pub(crate) fn poll() {
        // `try_tick` runs one task without blocking, so a nested poll cannot deadlock
        _ = LOCAL.try_with(|local| local.try_tick());
    }
}

#[cfg(all(test, feature = "futures-executor"))]
mod tests {
    use super::*;

    /// A thread pool that cannot start is a typed error, and sets no executor, so the
    /// caller can set another one. (The only lib unit test that touches the global
    /// executor, so the lib test binary's executor is unset before it.)
    #[test]
    fn a_thread_pool_that_cannot_start_is_an_error_and_sets_nothing() {
        let result = set_futures_executor(Err(std::io::Error::other(
            "no threads on this target",
        )));
        match result {
            Err(ExecutorError::ThreadPool(source)) => {
                assert_eq!(source.to_string(), "no threads on this target");
            }
            other => {
                panic!("expected ExecutorError::ThreadPool, got {other:?}")
            }
        }
        assert!(EXECUTOR.get().is_none());
    }
}
