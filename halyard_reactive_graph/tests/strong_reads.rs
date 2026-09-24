//! A strong read never waits for its own thread (docs/no-panics.md, "As implemented").
//!
//! A strong (reference-counted) read is total: it used to wait, spinning on
//! `std::thread::yield_now`, whenever its value was unavailable. That ends when another
//! thread is computing the value, but not when this thread is: a read of a value while it
//! was changed in place through a weak handle, or a memo read inside its own computation.
//! In the browser, where `yield_now` does nothing, the tab froze. Now no value is lent out
//! for a change in place (writes replace the value or change a copy), and a memo read inside
//! its own recomputation gives its previous value.
//!
//! Each scenario runs on a thread of its own with a deadline: on the old code it spun
//! forever, so the test fails instead of hanging.

use halyard_reactive_graph::{
    computed::{ArcAsyncDerived, ArcMemo, Memo},
    executor::Executor,
    owner::{ArcStoredValue, Owner},
    prelude::*,
    signal::{ArcMappedSignal, ArcRwSignal, RwSignal},
};
use std::{
    process::Command,
    sync::{mpsc, Arc, Mutex, OnceLock},
    thread,
    time::Duration,
};
use tokio::runtime::{Builder, Runtime};

/// The Tokio runtime whose worker threads run the tasks the scenarios spawn.
fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        Builder::new_multi_thread()
            .build()
            .expect("a Tokio runtime for the tests")
    })
}

/// Runs `scenario` under a fresh reactive owner on a thread of its own, and returns what it
/// returns; fails the test if it has not finished after five seconds (it waited for itself)
/// or if it panicked.
fn finishes<T: Send + 'static>(
    what: &str,
    scenario: impl FnOnce() -> T + Send + 'static,
) -> T {
    _ = Executor::init_tokio();
    let runtime = runtime().handle().clone();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _runtime = runtime.enter();
        let owner = Owner::new();
        owner.set();
        let out = scenario();
        _ = tx.send(out);
        drop(owner);
    });
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(out) => out,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("{what}: did not finish within 5 s (it waited for itself)")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("{what}: panicked")
        }
    }
}

/// A strong read inside an update made through a weak handle to the same signal: the update
/// works on a copy, so the strong read gives the committed value (every write form).
#[test]
fn a_strong_read_inside_a_weak_handles_update_gets_the_committed_value() {
    let seen = finishes("a strong read inside a weak update", || {
        let strong = ArcRwSignal::new(1);
        let weak = strong.downgrade();
        let mut seen = Vec::new();

        weak.update(|n| {
            seen.push(strong.get());
            *n += 1;
        });
        let returned = weak.try_update(|n| {
            seen.push(strong.get_untracked());
            *n += 1;
            *n
        });
        seen.push(returned.unwrap_or_default());
        weak.update_untracked(|n| {
            seen.push(strong.with(|n| *n));
            *n += 1;
        });
        weak.maybe_update(|n| {
            seen.push(*strong.read());
            *n += 1;
            true
        });
        if let Some(mut guard) = weak.try_write() {
            *guard += 1;
            seen.push(strong.get());
        }
        seen.push(strong.get());
        seen
    });
    assert_eq!(seen, vec![1, 2, 3, 3, 4, 5, 6]);
}

/// The same through a mapped signal: its write changes a copy of the mapped part and swaps
/// it in, so a strong read of the whole signal inside it gives the committed value.
#[test]
fn a_strong_read_inside_a_mapped_signals_update_gets_the_committed_value() {
    let (seen, after) =
        finishes("a strong read inside a mapped update", || {
            let whole = ArcRwSignal::new((1, 10));
            let second = ArcMappedSignal::new(
                whole.clone(),
                |pair| &pair.1,
                |pair| &mut pair.1,
            );
            let mut seen = Vec::new();
            second.update(|n| {
                seen.push(whole.get());
                *n += 1;
            });
            if let Some(mut guard) = second.try_write() {
                *guard += 1;
                seen.push(whole.get());
            }
            second.set(20);
            (seen, whole.get())
        });
    assert_eq!(seen, vec![(1, 10), (1, 11)]);
    assert_eq!(after, (1, 20));
}

/// A stored value is written the same way: a strong read inside an update made through its
/// weak handle, or while a write guard is alive, gives the value as it was.
#[test]
fn a_strong_read_inside_a_stored_values_update_gets_the_value_as_it_was() {
    let (seen, after) =
        finishes("a strong read inside a stored update", || {
            let strong = ArcStoredValue::new(1);
            let weak = strong.downgrade();
            let mut seen = Vec::new();
            weak.update_value(|n| {
                seen.push(strong.get_value());
                *n += 1;
            });
            if let Some(mut guard) = weak.try_write_value() {
                *guard += 1;
                seen.push(strong.get_value());
            }
            let mut guard = strong.write_value();
            *guard += 1;
            seen.push(strong.with_value(|n| *n));
            drop(guard);
            (seen, strong.get_value())
        });
    assert_eq!(seen, vec![1, 2, 3]);
    assert_eq!(after, 4);
}

/// An async derived value too: a strong read inside an update made through its weak handle
/// gives the committed value.
#[test]
fn a_strong_read_inside_an_async_derived_update_gets_the_committed_value() {
    let (seen, after) =
        finishes("a strong read inside an async update", || {
            let strong = ArcAsyncDerived::new(|| async { 1 });
            let weak = strong.downgrade();
            let mut seen = Vec::new();
            weak.update(|value| {
                seen.push(strong.get_untracked());
                *value = Some(2);
            });
            (seen, strong.get_untracked())
        });
    assert_eq!(seen, vec![Some(1)]);
    assert_eq!(after, Some(2));
}

/// A memo that reads itself (a cycle) through a strong handle when it recomputes gets its
/// previous value, which is also the value its function receives.
#[test]
fn a_memo_reading_itself_on_recompute_gets_its_previous_value() {
    let (seen, values) = finishes("a memo reading itself", || {
        let source = ArcRwSignal::new(1);
        let this: Arc<OnceLock<ArcMemo<i32>>> = Arc::new(OnceLock::new());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let memo = ArcMemo::new({
            let (source, this, seen) =
                (source.clone(), Arc::clone(&this), Arc::clone(&seen));
            move |previous: Option<&i32>| {
                let n = source.get();
                // not in its first computation: it has no previous value to give there
                if let (Some(previous), Some(this)) = (previous, this.get()) {
                    let itself = this.get();
                    seen.lock().unwrap().push((*previous, itself));
                }
                n * 10
            }
        });
        _ = this.set(memo.clone());

        let first = memo.get();
        source.set(2);
        let second = memo.get();
        source.set(3);
        let third = memo.with(|n| *n);
        let seen = seen.lock().unwrap().clone();
        (seen, vec![first, second, third])
    });
    assert_eq!(seen, vec![(10, 10), (20, 20)]);
    assert_eq!(values, vec![10, 20, 30]);
}

/// The same through a weak handle: `try_get` inside the memo's own function gives its
/// previous value when it recomputes, and `None` in its first computation (no abort).
#[test]
fn a_weak_memo_reading_itself_gets_its_previous_value_or_none() {
    let (seen, value) = finishes("a weak memo reading itself", || {
        let source = RwSignal::new(1);
        let this: Arc<OnceLock<Memo<i32>>> = Arc::new(OnceLock::new());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let memo = Memo::new_try({
            let (this, seen) = (Arc::clone(&this), Arc::clone(&seen));
            move |_| {
                let n = source.try_get()?;
                let itself = this.get().and_then(|this| this.try_get());
                seen.lock().unwrap().push(itself);
                Some(n * 10)
            }
        });
        _ = this.set(memo);

        _ = memo.try_get();
        source.set(2);
        let value = memo.try_get();
        let seen = seen.lock().unwrap().clone();
        (seen, value)
    });
    assert_eq!(seen, vec![None, Some(10)]);
    assert_eq!(value, Some(20));
}

/// A memo recomputed on one thread while another thread reads it strongly: the other thread
/// is not mistaken for this one (no abort); its read waits for the recomputation where it
/// must (to store its own result), and both finish with the new value.
#[test]
fn a_memo_recomputed_by_another_thread_is_waited_for() {
    thread_local! {
        static SLOW: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    let values = finishes("a memo read by two threads", || {
        let (started_tx, started_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (started_tx, release_rx) =
            (Mutex::new(started_tx), Mutex::new(release_rx));
        let source = ArcRwSignal::new(1);
        let memo = ArcMemo::new({
            let source = source.clone();
            move |_| {
                let n = source.get();
                if SLOW.with(std::cell::Cell::get) {
                    _ = started_tx.lock().unwrap().send(());
                    _ = release_rx.lock().unwrap().recv();
                }
                n * 10
            }
        });
        let first = memo.get();
        source.set(2);

        let slow = thread::spawn({
            let memo = memo.clone();
            move || {
                SLOW.with(|slow| slow.set(true));
                memo.get()
            }
        });
        started_rx.recv().unwrap();
        let reader = thread::spawn({
            let memo = memo.clone();
            move || memo.get()
        });
        thread::sleep(Duration::from_millis(50));
        release_tx.send(()).unwrap();
        vec![
            first,
            slow.join().unwrap(),
            reader.join().unwrap(),
            memo.get(),
        ]
    });
    assert_eq!(values, vec![10, 20, 20, 20]);
}

/// Run in a child process by the next test: a strong read of a memo inside its own first
/// computation, which has no possible value.
#[test]
#[ignore = "aborts the process: run in a child process by the next test"]
fn child_process_first_computation_self_read() {
    if std::env::var_os("HALYARD_STRONG_READS_CHILD").is_none() {
        return;
    }
    let owner = Owner::new();
    owner.set();
    let this: Arc<OnceLock<ArcMemo<i32>>> = Arc::new(OnceLock::new());
    let memo = ArcMemo::new({
        let this = Arc::clone(&this);
        move |_| this.get().map(|this| this.get()).unwrap_or_default() + 1
    });
    _ = this.set(memo.clone());
    _ = memo.get();
}

/// The one read with no possible value (a strong read of a memo inside its own first
/// computation) is reported, naming where the memo was created and where it is read, and
/// aborts: it neither spins nor gives a made-up value.
#[test]
fn a_strong_memo_read_inside_its_first_computation_aborts() {
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "child_process_first_computation_self_read",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("HALYARD_STRONG_READS_CHILD", "1")
        .output()
        .unwrap();
    assert!(!output.status.success(), "the child process aborts");
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(output.status.signal(), Some(6), "SIGABRT");
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("can never be given"), "{stderr}");
    assert!(stderr.contains("its own computation"), "{stderr}");
    assert_eq!(
        stderr.matches("tests/strong_reads.rs:").count() >= 2,
        true,
        "names where the memo was created and where it is read: {stderr}"
    );
}
