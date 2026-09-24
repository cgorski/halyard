//! What happens when the value behind a handle cannot be reached: a weak (arena) handle whose
//! value is gone is reported once per call site, and a strong read that finds its value in
//! use waits for it (see [`Strong`](crate::traits::Strong)).

use std::{cell::RefCell, collections::HashSet, panic::Location};

thread_local! {
    static REPORTED: RefCell<HashSet<(usize, u8)>> = RefCell::new(HashSet::new());
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
/// A strong handle keeps its value alive, so `attempt` fails only while the value is in use
/// in a way that leaves nothing to read: changed in place on this thread (a read inside its
/// own in-place change, reached through another handle to the same value), a memo read
/// inside its own computation (a cycle), or, on a server, a memo being recomputed by another
/// thread. The first two are cycles in the application's code; the read is reported once and
/// waits.
#[doc(hidden)]
#[track_caller]
pub fn wait_for<V>(
    defined_at: Option<&'static Location<'static>>,
    mut attempt: impl FnMut() -> Option<V>,
) -> V {
    let at = Location::caller();
    loop {
        if let Some(value) = attempt() {
            return value;
        }
        if first_time(at, Attempt::Wait) {
            let defined = defined_at
                .map(|defined_at| format!(" (defined at {defined_at})"))
                .unwrap_or_default();
            crate::log_warning(format_args!(
                "At {at}, a strong handle{defined} was read while its value \
                 was in use: changed in place, or being computed, by the code \
                 that reads it (a cycle), or recomputed by another thread. The \
                 read waits for the value."
            ));
        }
        std::thread::yield_now();
    }
}
