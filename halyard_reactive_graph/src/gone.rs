//! What happens when the value behind a handle cannot be reached: a weak (arena) handle whose
//! value is gone is reported once per call site, and a strong read that finds its value in
//! use by another thread waits for it (see [`Strong`](crate::traits::Strong)); one that
//! finds it in use by its own thread, where no value can ever come, aborts ([`wait_for`]).

use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    panic::Location,
};

thread_local! {
    static REPORTED: RefCell<HashSet<(usize, u8)>> = RefCell::new(HashSet::new());

    /// Where the strong read that [`wait_for`] is attempting on this thread was made.
    static STRONG_READ_AT: Cell<Option<&'static Location<'static>>> =
        const { Cell::new(None) };
}

/// Where the strong read being attempted on this thread was made, if one is (a read through
/// [`wait_for`] passes through closures, which lose the caller's location). Taken, so that
/// the reads made further in (by a memo's function) do not see it.
pub(crate) fn take_strong_read_site() -> Option<&'static Location<'static>> {
    STRONG_READ_AT.try_with(|site| site.take()).ok().flatten()
}

/// What was attempted through a handle whose value could not be reached.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attempt {
    /// A write (`set`, `update`, `notify`, `dispatch`, ...): it does nothing.
    Write,
    /// Rendering (a `view!` child, an attribute value, a `For`/`Show` source): nothing is
    /// rendered or updated.
    Render,
    /// A `try_*` read: it gives `None`.
    Read,
    /// Awaiting an async value: the future stays pending.
    Await,
    /// A strong read that found the value in use: it waits.
    Wait,
}

/// `true` the first time `attempt` is reported at `at` on this thread.
fn first_time(at: &'static Location<'static>, attempt: Attempt) -> bool {
    let key = (at as *const Location<'static> as usize, attempt as u8);
    REPORTED
        .try_with(|set| {
            set.try_borrow_mut()
                .map(|mut set| set.insert(key))
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

/// Reports, once per call site, that a weak handle's value was gone when `attempt` was made
/// through it.
#[doc(hidden)]
pub fn report_gone(
    attempt: Attempt,
    what: &str,
    defined_at: Option<&'static Location<'static>>,
    at: &'static Location<'static>,
) {
    if !first_time(at, attempt) {
        return;
    }
    let defined = defined_at
        .map(|defined_at| format!(" (defined at {defined_at})"))
        .unwrap_or_default();
    let outcome = match attempt {
        Attempt::Write => "the write does nothing",
        Attempt::Render => "nothing is rendered or updated",
        Attempt::Read => "the read gives `None`",
        Attempt::Await => "the future stays pending",
        Attempt::Wait => "the read waits",
    };
    crate::log_warning(format_args!(
        "At {at}, a {what}{defined} was used after its value was gone (its \
         owner was disposed): {outcome}."
    ));
}

/// Reports, once per handle (keyed by where it was defined, or else by the call site), that
/// a weak handle rendered in a view has no value any more: nothing is rendered or updated.
#[doc(hidden)]
#[track_caller]
pub fn report_gone_render(
    what: &str,
    defined_at: Option<&'static Location<'static>>,
) {
    let at = defined_at.unwrap_or_else(Location::caller);
    if !first_time(at, Attempt::Render) {
        return;
    }
    let defined = defined_at
        .map(|defined_at| format!(" (defined at {defined_at})"))
        .unwrap_or_default();
    crate::log_warning(format_args!(
        "A {what}{defined} is rendered in a view, but its value is gone (its \
         owner was disposed): nothing is rendered or updated."
    ));
}

/// Reads a weak handle for rendering: its value, or `None` if it is gone (reported once).
#[doc(hidden)]
#[track_caller]
pub fn render_value<H>(
    handle: &H,
) -> Option<<H as crate::traits::TryGet>::Value>
where
    H: crate::traits::TryGet + crate::traits::IsDisposed,
{
    let value = handle.try_get();
    if value.is_none() && handle.is_disposed() {
        report_gone_render(std::any::type_name::<H>(), handle.defined_at());
    }
    value
}

/// [`render_value`] for a handle whose value is an `Option` (a `MaybeProp`): `None` when it
/// is unset or gone.
#[doc(hidden)]
#[track_caller]
pub fn render_optional<H, T>(handle: &H) -> Option<T>
where
    H: crate::traits::TryGet<Value = Option<T>> + crate::traits::IsDisposed,
{
    render_value(handle).flatten()
}

/// Reports a gone value only in debug builds (for `try_*` reads, which say so in their
/// result already).
#[doc(hidden)]
#[inline]
pub fn report_gone_read(
    what: &str,
    defined_at: Option<&'static Location<'static>>,
    at: &'static Location<'static>,
) {
    #[cfg(any(debug_assertions, halyard_debuginfo))]
    report_gone(Attempt::Read, what, defined_at, at);
    #[cfg(not(any(debug_assertions, halyard_debuginfo)))]
    {
        _ = (what, defined_at, at);
    }
}

/// The read of a strong handle: `attempt` until it gives the value.
///
/// A strong handle keeps its value alive, and no value is ever lent out for a change in
/// place, so `attempt` fails only while the value is being computed:
/// - by another thread (a memo recomputed on a server): the read is reported once and waits
///   for it, which ends;
/// - by this thread, with no value to give: a memo read inside its own first computation
///   (the same thread's per-thread record says so, see [`crate::reentry::refuse_here`]).
///   That is a cycle in the program's logic with no possible value, like unbounded
///   recursion: it is reported, naming where the value was created and where it is read,
///   and the process aborts (`std::process::abort`). This is the only abort in halyard.
///
/// In the browser (`wasm32` without threads) there is no other thread to wait for: every
/// read that gets here is the same-thread case.
#[doc(hidden)]
#[track_caller]
pub fn wait_for<V>(
    defined_at: Option<&'static Location<'static>>,
    mut attempt: impl FnMut() -> Option<V>,
) -> V {
    let at = Location::caller();
    loop {
        // a note left by an earlier read that did not come through here
        _ = crate::reentry::take_refusal();
        let outer = STRONG_READ_AT.try_with(|site| site.replace(Some(at)));
        let value = attempt();
        if let Ok(outer) = outer {
            _ = STRONG_READ_AT.try_with(|site| site.set(outer));
        }
        if let Some(value) = value {
            return value;
        }
        let refusal = crate::reentry::take_refusal();
        if refusal.is_some() || crate::reentry::SINGLE_THREADED {
            abort_on_cycle(at, defined_at, refusal);
        }
        if first_time(at, Attempt::Wait) {
            let defined = defined_at
                .map(|defined_at| format!(" (defined at {defined_at})"))
                .unwrap_or_default();
            crate::log_warning(format_args!(
                "At {at}, a strong handle{defined} was read while another \
                 thread was computing its value. The read waits for it."
            ));
        }
        std::thread::yield_now();
    }
}

/// The end of a strong read that can never get a value (see [`wait_for`]): reports it, then
/// aborts the process.
#[cold]
fn abort_on_cycle(
    at: &'static Location<'static>,
    defined_at: Option<&'static Location<'static>>,
    refusal: Option<crate::reentry::Refusal>,
) -> ! {
    let defined_at =
        defined_at.or_else(|| refusal.and_then(|refusal| refusal.defined_at));
    let created = defined_at
        .map(|defined_at| format!("created at {defined_at}"))
        .unwrap_or_else(|| {
            "its creation site is known in debug builds only".to_owned()
        });
    let why = refusal.map_or(
        "it is in use by the code that is running now, and there is no other thread \
         that could release it",
        |refusal| refusal.why,
    );
    let message = format!(
        "[halyard] At {at}, a strong handle to a reactive value ({created}) was read, but \
         the value can never be given: {why}. This is a cycle in the program's logic with no \
         possible value (like unbounded recursion); the process aborts. A memo that reads \
         itself gets its previous value on later computations, but has none during its \
         first: read it through its weak handle (`try_get`), or restructure the computation."
    );
    #[cfg(feature = "tracing")]
    tracing::error!("{message}");
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    web_sys::console::error_1(&message.as_str().into());
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    {
        use std::io::Write;
        // `eprintln!` panics if standard error is closed; with nowhere left to report, the
        // process still aborts
        _ = writeln!(std::io::stderr(), "{message}");
    }
    std::process::abort()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        thread,
        time::Duration,
    };

    /// A strong read whose value another thread is computing (nothing on this thread says
    /// otherwise) waits for it, and gets it.
    #[test]
    fn a_strong_read_waits_for_another_thread() {
        let ready = Arc::new(AtomicBool::new(false));
        let computing = thread::spawn({
            let ready = Arc::clone(&ready);
            move || {
                thread::sleep(Duration::from_millis(20));
                ready.store(true, Ordering::SeqCst);
            }
        });
        let mut attempts = 0_u32;
        let value = wait_for(None, || {
            attempts = attempts.saturating_add(1);
            ready.load(Ordering::SeqCst).then_some(7)
        });
        assert_eq!(value, 7);
        assert!(attempts > 1, "it waited");
        assert!(computing.join().is_ok());
    }

    /// A note left by a read that did not go through `wait_for` is not taken for this
    /// read's own.
    #[test]
    fn a_stale_note_is_cleared_before_the_read() {
        crate::reentry::refuse_here(crate::reentry::Refusal {
            defined_at: None,
            why: "an earlier read",
        });
        assert_eq!(wait_for(None, || Some(1)), 1);
        let mut first = true;
        let value = wait_for(None, || {
            let value = (!first).then_some(2);
            first = false;
            value
        });
        assert_eq!(value, 2);
        assert_eq!(crate::reentry::take_refusal(), None);
    }
}
