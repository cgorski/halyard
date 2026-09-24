//! Takes the guard out of a [`std::sync`] lock result without ever panicking.
//!
//! A lock is *poisoned* when a thread panicked while holding it. The standard
//! library then returns `Err(PoisonError)` from every later `lock()`/`read()`/
//! `write()`, and the usual `.unwrap()` turns one panic into a cascade: every
//! other user of the lock panics too. halyard never panics (README, "Project
//! policy"), so [`OrPoisoned::or_poisoned`] recovers the guard instead, as
//! `parking_lot` and tokio's locks do by never poisoning at all. The data may
//! be in whatever state the panicking thread left it; halyard's locks guard
//! bookkeeping (subscriber lists, arenas, caches) that stays usable, and a
//! half-updated entry is far better than a dead reactive system or a failed
//! request for every later caller.
//!
//! ```rust
//! use halyard_reactive_graph::or_poisoned::OrPoisoned;
//! use std::sync::RwLock;
//!
//! let lock = RwLock::new(String::from("Hello!"));
//! let read = lock.read().or_poisoned();
//! assert_eq!(*read, "Hello!");
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    clippy::indexing_slicing
)]

use std::sync::{
    LockResult, MutexGuard, PoisonError, RwLockReadGuard, RwLockWriteGuard,
};

/// Takes the guard out of a lock result, poisoned or not.
pub trait OrPoisoned {
    /// The inner guard type.
    type Inner;

    /// Returns the guard. If the lock is poisoned, the guard is recovered
    /// from the [`PoisonError`] rather than panicking (see the module docs).
    fn or_poisoned(self) -> Self::Inner;
}

impl<'a, T: ?Sized> OrPoisoned
    for Result<RwLockReadGuard<'a, T>, PoisonError<RwLockReadGuard<'a, T>>>
{
    type Inner = RwLockReadGuard<'a, T>;

    fn or_poisoned(self) -> Self::Inner {
        self.unwrap_or_else(PoisonError::into_inner)
    }
}

impl<'a, T: ?Sized> OrPoisoned
    for Result<RwLockWriteGuard<'a, T>, PoisonError<RwLockWriteGuard<'a, T>>>
{
    type Inner = RwLockWriteGuard<'a, T>;

    fn or_poisoned(self) -> Self::Inner {
        self.unwrap_or_else(PoisonError::into_inner)
    }
}

impl<'a, T: ?Sized> OrPoisoned for LockResult<MutexGuard<'a, T>> {
    type Inner = MutexGuard<'a, T>;

    fn or_poisoned(self) -> Self::Inner {
        self.unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
// Tests poison locks on purpose, which takes a panicking thread.
#[allow(clippy::panic)]
mod tests {
    use super::OrPoisoned;
    use std::sync::{Arc, Mutex, RwLock};

    /// Runs `hold` (which takes a lock and panics) on its own thread, which
    /// poisons that lock.
    fn poison(hold: impl FnOnce() + Send + 'static) {
        let joined = std::thread::spawn(hold).join();
        assert!(joined.is_err(), "the holder thread must have panicked");
    }

    #[test]
    fn a_poisoned_mutex_still_yields_its_guard() {
        let lock = Arc::new(Mutex::new(1));
        let held = Arc::clone(&lock);
        poison(move || {
            let mut guard = held.lock().or_poisoned();
            *guard = 2;
            panic!("poison the lock");
        });
        assert!(lock.is_poisoned());
        assert_eq!(
            *lock.lock().or_poisoned(),
            2,
            "the write before the panic is kept"
        );
    }

    #[test]
    fn a_poisoned_rwlock_still_yields_read_and_write_guards() {
        let lock = Arc::new(RwLock::new(String::from("a")));
        let held = Arc::clone(&lock);
        poison(move || {
            let _guard = held.write().or_poisoned();
            panic!("poison the lock");
        });
        assert!(lock.is_poisoned());
        assert_eq!(*lock.read().or_poisoned(), "a");
        lock.write().or_poisoned().push('b');
        assert_eq!(*lock.read().or_poisoned(), "ab");
    }
}
