//! Telling re-entry from contention (docs/no-panics.md, "Structural changes" 2).
//!
//! A reactive value's lock can be busy for two reasons. Another thread holds it (on the
//! server): waiting is right, it will be released. Or this thread holds it, because user code
//! running inside `with`/`update`/`with_value`/`update_value` (or holding a guard from `read`
//! or `write`) reached the same value again: waiting would never end (a deadlock natively; in
//! the browser, std's single-threaded lock aborts the whole app). Guards over signal values
//! record here, per thread, which locks they hold, so that a busy lock can be refused, not
//! waited for, when it is this thread's own.

use std::{cell::RefCell, marker::PhantomData, sync::MutexGuard};

thread_local! {
    /// The addresses of the locks that guards alive on this thread hold (a lock appears once
    /// per guard).
    static HELD: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

/// The identity of a lock: its address.
pub(crate) fn lock_id<T: ?Sized>(lock: &T) -> usize {
    (lock as *const T).cast::<()>() as usize
}

/// Records, while it lives, that this thread holds a lock. It lives inside a std lock guard's
/// wrapper, and like that guard it is not `Send`: it is dropped on the thread that took the
/// lock.
#[derive(Debug)]
pub(crate) struct Held {
    lock: usize,
    not_send: PhantomData<MutexGuard<'static, ()>>,
}

impl Held {
    pub(crate) fn new(lock: usize) -> Self {
        // while the thread is shutting down there is nothing left to record into, and no user
        // code left to re-enter
        _ = HELD.try_with(|held| {
            if let Ok(mut held) = held.try_borrow_mut() {
                held.push(lock);
            }
        });
        Self {
            lock,
            not_send: PhantomData,
        }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        _ = HELD.try_with(|held| {
            if let Ok(mut held) = held.try_borrow_mut() {
                if let Some(index) = held.iter().rposition(|&l| l == self.lock)
                {
                    held.swap_remove(index);
                }
            }
        });
    }
}

/// Whether a guard alive on this thread holds the lock.
pub(crate) fn held_by_this_thread(lock: usize) -> bool {
    HELD.try_with(|held| {
        held.try_borrow()
            .map(|held| held.contains(&lock))
            .unwrap_or(false)
    })
    .unwrap_or(false)
}

/// Whether this target has only one thread (the browser): a busy lock can only be this
/// thread's own, and waiting for it aborts.
pub(crate) const SINGLE_THREADED: bool =
    cfg!(all(target_arch = "wasm32", not(target_feature = "atomics")));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lock_is_held_while_its_marker_lives() {
        let lock = 0_u8;
        let id = lock_id(&lock);
        assert!(!held_by_this_thread(id));

        let outer = Held::new(id);
        let inner = Held::new(id);
        assert!(held_by_this_thread(id));
        drop(inner);
        assert!(held_by_this_thread(id), "the outer guard still holds it");
        drop(outer);
        assert!(!held_by_this_thread(id));
    }

    #[test]
    fn another_threads_lock_is_not_this_threads() {
        let lock = 0_u8;
        let id = lock_id(&lock);
        let _held = Held::new(id);

        let elsewhere =
            std::thread::spawn(move || held_by_this_thread(id)).join();

        assert!(matches!(elsewhere, Ok(false)));
    }
}
