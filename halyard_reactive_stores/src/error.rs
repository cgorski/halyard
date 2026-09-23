//! What the stores do instead of panicking (README, "Project policy": no panics, ever): a
//! typed error, logged (once, where it would otherwise repeat on every update), and a
//! recovery that keeps the store usable. Each variant says what happens instead.
//!
//! The outcomes a caller can act on are typed in the API itself: a field whose value is not
//! there (an `Option` that is `None`, an index past the end, a key that is not in the
//! collection, a disposed store) gives no guard (`reader`/`writer` return `None`), so its
//! `try_*` accessors return `None`.

use crate::path::StorePath;
use std::sync::atomic::{AtomicBool, Ordering};
use thiserror::Error;

/// A failure that a store recovered from.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub(crate) enum StoreError {
    /// A patch notified an entry of a keyed field by its index, and the field's key map has
    /// no key recorded for that index.
    #[error(
        "a patch changed entry {index} of the keyed field at {path:?}, but that field has no \
         key recorded for the index; every entry of the field is notified instead"
    )]
    NoKeyForIndex { path: StorePath, index: usize },
    /// A keyed patch matches old and new entries by key, which needs each key once.
    #[error(
        "a keyed field was patched with {entries} entries but {keys} distinct keys; keys \
         must be unique. Every entry is kept in its place; an entry whose key repeats is \
         added as a new entry, and the field is notified as changed"
    )]
    RepeatedKeys { entries: usize, keys: usize },
    /// Every key slot (`usize::MAX` of them) of a keyed field has been handed out.
    #[error(
        "a keyed field has handed out every key slot; new keys share the last one, so \
         their subscribers are notified together"
    )]
    KeySlotsExhausted,
}

/// Logs a failure that a store recovered from: in the browser's console, or on standard
/// error.
pub(crate) fn report(error: &StoreError) {
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    halyard_reactive_graph::log_warning(format_args!("[halyard] {error}"));
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    {
        use std::io::Write;
        // `eprintln!` panics if standard error is closed; with nowhere left to report the
        // error, it is dropped
        _ = writeln!(std::io::stderr(), "[halyard] {error}");
    }
}

/// Reports one kind of failure the first time it happens, and never again: for failures
/// that would otherwise be logged on every update.
pub(crate) struct ReportOnce(AtomicBool);

impl ReportOnce {
    pub(crate) const fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    /// Reports the error that `error` builds, if this kind has not been reported yet.
    pub(crate) fn report(&self, error: impl FnOnce() -> StoreError) {
        if !self.0.swap(true, Ordering::Relaxed) {
            report(&error());
        }
    }
}

/// The entry that a guard over an `Option`'s value or over an entry of a keyed collection was
/// made for.
///
/// Such a guard is only made after its entry was found (`reader` and `writer` return `None`
/// otherwise), and the lock it holds keeps the store's value as it was while the guard lives,
/// so looking the entry up again always finds it. (Only a key whose `Hash`, `Eq` or `Ord`
/// changes its answer could miss it: a logic error for which the standard library's maps
/// also allow a panic.)
///
/// `Deref` must return a reference, and safe Rust has none to give for an entry that is not
/// there. This is the one place the crate states that invariant instead of proving it by its
/// types, which needs a guard that keeps the projected reference itself (`unsafe`, as the
/// `guardian` crate is for the lock guards).
pub(crate) fn held<R>(entry: Option<R>) -> R {
    match entry {
        Some(entry) => entry,
        None => unreachable!(
            "a store guard lost the entry it was made for, which its lock keeps in place"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_say_what_happens_instead() {
        let no_key = StoreError::NoKeyForIndex {
            path: vec![0.into()].into(),
            index: 3,
        }
        .to_string();
        assert!(no_key.contains("entry 3 of the keyed field"), "{no_key}");
        assert!(
            no_key.ends_with("every entry of the field is notified instead")
        );

        let repeated = StoreError::RepeatedKeys {
            entries: 3,
            keys: 2,
        }
        .to_string();
        assert!(
            repeated.contains("3 entries but 2 distinct keys"),
            "{repeated}"
        );
        assert!(repeated.contains("Every entry is kept"), "{repeated}");
    }

    #[test]
    fn report_once_reports_only_the_first_time() {
        let once = ReportOnce::new();
        let mut built = 0;
        for _ in 0..3 {
            once.report(|| {
                built += 1;
                StoreError::KeySlotsExhausted
            });
        }
        assert_eq!(built, 1);
    }

    #[test]
    fn held_gives_the_entry() {
        let value = 7;
        assert_eq!(*held(Some(&value)), 7);
        let mut value = 7;
        *held(Some(&mut value)) = 8;
        assert_eq!(value, 8);
    }
}
