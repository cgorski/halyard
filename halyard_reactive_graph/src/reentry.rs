//! What this thread is doing with each reactive value (docs/no-panics.md, "Re-entrant
//! access").
//!
//! A reactive value's lock can be busy for two reasons. Another thread holds it (on the
//! server): waiting is right, it will be released. Or this thread holds it, because user code
//! running inside `with`/`update`/`with_value`/`update_value` (or holding a guard from `read`
//! or `write`) reached the same value again: waiting would never end (a deadlock natively; in
//! the browser, std's single-threaded lock aborts the whole app). So every access records
//! here, per thread, which value it uses, and a conflict is detected before any lock is tried.
//!
//! For signals the record also carries what makes re-entrant writes total:
//! - the *writer turn*: a signal's writes (from `set`, `update`, a write guard) serialize
//!   across threads on a mutex of their own, which is not the value's lock (reads never wait
//!   for it). This thread takes it for its first write and keeps it until its outermost
//!   access to the signal ends;
//! - the *deferred writes*: a write made while this thread is using the signal (inside its
//!   `with`, while a guard of its is alive, inside its `update`) cannot take the value's lock
//!   now. It is kept here and applied, in order, when the outermost access ends.

use guardian::ArcMutexGuardian;
use std::{
    any::Any,
    cell::RefCell,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    rc::{Rc, Weak},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

thread_local! {
    /// The values this thread is using: one entry per value, while it is in use.
    static ACCESSES: RefCell<Vec<Entry>> = const { RefCell::new(Vec::new()) };
}

/// This thread's use of one value.
struct Entry {
    id: usize,
    /// The accesses alive on this thread: read guards, write locks, writes in progress,
    /// write guards.
    depth: usize,
    /// How many of them hold the value's write lock (an in-place change): the value cannot be
    /// read on this thread until they end.
    write_locks: usize,
    /// How many of them are working on a copy of the value that they will commit (an
    /// `update` closure that is running, a write guard that is alive).
    snapshots: usize,
    /// This thread's turn to write the value, from its first write until `depth` is 0.
    turn: Option<ArcMutexGuardian<()>>,
    /// Writes made while the value was in use here, to apply when `depth` returns to 0.
    pending: Option<Box<dyn Pending>>,
    /// The read guard this thread holds on the value, if any: nested reads share it rather
    /// than take the lock again (a recursive read of a std lock may wait forever for a
    /// writer that is itself waiting for the outer read).
    shared_read: Option<Weak<dyn Any>>,
}

impl Entry {
    fn new(id: usize) -> Self {
        Self {
            id,
            depth: 0,
            write_locks: 0,
            snapshots: 0,
            turn: None,
            pending: None,
            shared_read: None,
        }
    }
}

/// What kind of access starts or ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A read guard, or any access that holds no write lock and no copy.
    Read,
    /// An access holding the value's write lock.
    WriteLock,
    /// A write working on a copy of the value.
    Snapshot,
}

/// Writes deferred until the outermost access to a value ends (signals implement it).
pub(crate) trait Pending: Any {
    /// Applies the writes in order, holding `turn` if this thread already has it.
    fn flush(self: Box<Self>, turn: Option<ArcMutexGuardian<()>>);

    /// Appends the writes of `later`, which were deferred after these.
    fn append(&mut self, later: Box<dyn Pending>);

    /// For reaching the typed writes.
    fn as_any_mut(&mut self) -> &mut dyn Any;

    /// For taking the typed writes.
    fn into_any(self: Box<Self>) -> Box<dyn Any>;

    /// Whether there is nothing to apply.
    fn is_empty(&self) -> bool;
}

/// What this thread is doing with a value, as far as the record shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct State {
    pub(crate) depth: usize,
    pub(crate) write_locks: usize,
    pub(crate) snapshots: usize,
    pub(crate) has_turn: bool,
}

impl State {
    /// Whether this thread is using the value at all.
    pub(crate) fn in_use(&self) -> bool {
        self.depth > 0
    }
}

/// Runs `f` on the record; `None` while the thread is shutting down (there is nothing left to
/// record into, and no user code left to re-enter), or if the record is busy (it never is:
/// no code runs while it is borrowed but the short functions of this module).
fn with_entries<R>(f: impl FnOnce(&mut Vec<Entry>) -> R) -> Option<R> {
    ACCESSES
        .try_with(|entries| {
            entries
                .try_borrow_mut()
                .ok()
                .map(|mut entries| f(&mut entries))
        })
        .ok()
        .flatten()
}

/// The identity of a lock (and of the value it guards): its address.
pub(crate) fn lock_id<T: ?Sized>(lock: &T) -> usize {
    (lock as *const T).cast::<()>() as usize
}

/// What this thread is doing with the value; `None` if the record cannot be reached (see
/// [`with_entries`]).
pub(crate) fn state(id: usize) -> Option<State> {
    with_entries(|entries| {
        entries
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| State {
                depth: entry.depth,
                write_locks: entry.write_locks,
                snapshots: entry.snapshots,
                has_turn: entry.turn.is_some(),
            })
            .unwrap_or_default()
    })
}

/// Whether an access alive on this thread uses the value.
pub(crate) fn held_by_this_thread(id: usize) -> bool {
    state(id).is_some_and(|state| state.in_use())
}

fn count(entry: &mut Entry, kind: Kind, up: bool) {
    let step = |n: usize| {
        if up {
            n.saturating_add(1)
        } else {
            n.saturating_sub(1)
        }
    };
    entry.depth = step(entry.depth);
    match kind {
        Kind::Read => {}
        Kind::WriteLock => entry.write_locks = step(entry.write_locks),
        Kind::Snapshot => entry.snapshots = step(entry.snapshots),
    }
}

/// Records the start of an access. Returns whether it was recorded.
fn enter(id: usize, kind: Kind) -> bool {
    with_entries(|entries| {
        match entries.iter_mut().find(|entry| entry.id == id) {
            Some(entry) => count(entry, kind, true),
            None => {
                let mut entry = Entry::new(id);
                count(&mut entry, kind, true);
                entries.push(entry);
            }
        }
    })
    .is_some()
}

/// Records the end of an access. When it was the outermost one, releases the writer turn,
/// after applying the writes deferred meanwhile.
fn exit(id: usize, kind: Kind) {
    let finished = with_entries(|entries| {
        let index = entries.iter().position(|entry| entry.id == id)?;
        let entry = entries.get_mut(index)?;
        count(entry, kind, false);
        if entry.depth > 0 {
            return None;
        }
        // `index` was just found, and nothing has changed since
        let entry = entries.swap_remove(index);
        Some((entry.pending, entry.turn))
    })
    .flatten();
    // the record is released: flushing runs user code (subscribers, `Drop`s)
    if let Some((pending, turn)) = finished {
        match pending {
            Some(pending)
                if !pending.is_empty() && !std::thread::panicking() =>
            {
                pending.flush(turn)
            }
            pending => {
                drop(turn);
                drop(pending);
            }
        }
    }
}

/// The read guard alive on this thread for the value, to share with a nested read.
pub(crate) fn shared_read(id: usize) -> Option<Rc<dyn Any>> {
    with_entries(|entries| {
        entries
            .iter()
            .find(|entry| entry.id == id)
            .and_then(|entry| entry.shared_read.as_ref())
            .and_then(Weak::upgrade)
    })
    .flatten()
}

/// Records the read guard that nested reads of the value on this thread share. The access
/// must be recorded already (see [`Held`]).
pub(crate) fn share_read(id: usize, guard: &Rc<dyn Any>) {
    with_entries(|entries| {
        if let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) {
            entry.shared_read = Some(Rc::downgrade(guard));
        }
    });
}

/// A lock guard whose access is recorded while it lives.
pub(crate) struct Recorded<G> {
    guard: G,
    // after `guard`: the lock is released before it stops being recorded
    _held: Held,
}

impl<G> Recorded<G> {
    /// Records `guard`, of the lock `id`, which it holds for writing if `write`.
    pub(crate) fn new(guard: G, id: usize, write: bool) -> Self {
        Self {
            guard,
            _held: if write {
                Held::write_lock(id)
            } else {
                Held::new(id)
            },
        }
    }
}

impl<G: Deref> Deref for Recorded<G> {
    type Target = G::Target;

    fn deref(&self) -> &Self::Target {
        self.guard.deref()
    }
}

impl<G: DerefMut> DerefMut for Recorded<G> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.guard.deref_mut()
    }
}

impl<G> std::fmt::Debug for Recorded<G> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recorded").finish_non_exhaustive()
    }
}

/// Takes a writer turn when this thread holds nothing of its value: waits for other threads,
/// except where there is only one thread (a busy turn could only be this thread's own).
pub(crate) fn take_turn(turn: &Arc<Mutex<()>>) -> Option<ArcMutexGuardian<()>> {
    if SINGLE_THREADED {
        ArcMutexGuardian::try_take(Arc::clone(turn))
            .map(|taken| taken.unwrap_or_else(PoisonError::into_inner))
    } else {
        Some(
            ArcMutexGuardian::take(Arc::clone(turn))
                .unwrap_or_else(PoisonError::into_inner),
        )
    }
}

/// Records, while it lives, an access to a value on this thread. Like a std lock guard it is
/// not `Send`: it ends on the thread where it started.
#[derive(Debug)]
pub(crate) struct Held {
    id: usize,
    kind: Kind,
    not_send: PhantomData<MutexGuard<'static, ()>>,
}

impl Held {
    /// A read (or an access that holds neither a write lock nor a copy).
    pub(crate) fn new(id: usize) -> Self {
        Self::of(id, Kind::Read)
    }

    /// An access that holds the value's write lock.
    pub(crate) fn write_lock(id: usize) -> Self {
        Self::of(id, Kind::WriteLock)
    }

    /// An access working on a copy of the value, to commit later.
    pub(crate) fn snapshot(id: usize) -> Self {
        Self::of(id, Kind::Snapshot)
    }

    fn of(id: usize, kind: Kind) -> Self {
        enter(id, kind);
        Self {
            id,
            kind,
            not_send: PhantomData,
        }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        exit(self.id, self.kind);
    }
}

/// How a write to a signal can proceed.
#[derive(Debug)]
pub(crate) enum Begin {
    /// This thread now has the writer turn, and the access is recorded until the returned
    /// [`Writing`] is dropped.
    Started(Writing),
    /// This thread is using the signal already: the write must be deferred.
    InUse,
    /// The record cannot be reached (the thread is shutting down), or the turn is busy where
    /// only this thread could hold it: the write cannot be made safely.
    Unavailable,
}

/// A write in progress on this thread (it holds the writer turn, through the record).
#[derive(Debug)]
pub(crate) struct Writing {
    held: Held,
}

impl Writing {
    /// The value being written.
    pub(crate) fn id(&self) -> usize {
        self.held.id
    }
}

/// Starts a write to the value `id`, whose writer turn is `turn`: takes the turn, waiting
/// while another thread writes the value. `snapshot` says whether the write works on a copy.
pub(crate) fn begin_write(
    id: usize,
    turn: &Arc<Mutex<()>>,
    snapshot: bool,
) -> Begin {
    match state(id) {
        None => return Begin::Unavailable,
        Some(state) if state.in_use() => return Begin::InUse,
        Some(_) => {}
    }
    // This thread holds nothing of the value, so waiting for the turn waits only for other
    // threads.
    let Some(guard) = take_turn(turn) else {
        return Begin::Unavailable;
    };
    let kind = if snapshot { Kind::Snapshot } else { Kind::Read };
    let mut guard = Some(guard);
    let recorded = with_entries(|entries| {
        let entry = match entries.iter_mut().position(|entry| entry.id == id) {
            Some(index) => entries.get_mut(index),
            None => {
                entries.push(Entry::new(id));
                entries.last_mut()
            }
        };
        if let Some(entry) = entry {
            count(entry, kind, true);
            entry.turn = guard.take();
        }
    });
    if recorded.is_none() || guard.is_some() {
        // not recorded: the turn is released here
        return Begin::Unavailable;
    }
    Begin::Started(Writing {
        held: Held {
            id,
            kind,
            not_send: PhantomData,
        },
    })
}

/// Starts applying deferred writes with the writer turn already taken: records the access and
/// puts `pending` back, ahead of any writes deferred since.
pub(crate) fn resume_write(
    id: usize,
    turn: ArcMutexGuardian<()>,
    pending: Box<dyn Pending>,
) -> Option<Writing> {
    let mut parts = Some((turn, pending));
    with_entries(|entries| {
        let entry = match entries.iter_mut().position(|entry| entry.id == id) {
            Some(index) => entries.get_mut(index),
            None => {
                entries.push(Entry::new(id));
                entries.last_mut()
            }
        };
        if let (Some(entry), Some((turn, mut pending))) = (entry, parts.take())
        {
            count(entry, Kind::Read, true);
            if entry.turn.is_none() {
                entry.turn = Some(turn);
            }
            if let Some(later) = entry.pending.take() {
                pending.append(later);
            }
            entry.pending = Some(pending);
        }
    })?;
    if parts.is_some() {
        return None;
    }
    Some(Writing {
        held: Held {
            id,
            kind: Kind::Read,
            not_send: PhantomData,
        },
    })
}

/// Takes the deferred writes of the value out of the record (to read or change them without
/// holding the record).
pub(crate) fn take_pending(id: usize) -> Option<Box<dyn Pending>> {
    with_entries(|entries| {
        entries
            .iter_mut()
            .find(|entry| entry.id == id)
            .and_then(|entry| entry.pending.take())
    })
    .flatten()
}

/// Puts deferred writes (back) into the record, ahead of any deferred since they were taken.
/// Gives them back if this thread is not using the value (nothing would apply them).
pub(crate) fn put_pending(
    id: usize,
    pending: Box<dyn Pending>,
) -> Result<(), Box<dyn Pending>> {
    let mut slot = Some(pending);
    with_entries(|entries| {
        if let Some(entry) = entries
            .iter_mut()
            .find(|entry| entry.id == id && entry.depth > 0)
        {
            if let Some(mut pending) = slot.take() {
                if let Some(later) = entry.pending.take() {
                    pending.append(later);
                }
                entry.pending = Some(pending);
            }
        }
    });
    match slot {
        Some(pending) => Err(pending),
        None => Ok(()),
    }
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

    #[test]
    fn a_write_in_use_on_this_thread_is_not_started() {
        let lock = 0_u8;
        let id = lock_id(&lock);
        let turn = Arc::new(Mutex::new(()));

        let reading = Held::new(id);
        assert!(matches!(begin_write(id, &turn, false), Begin::InUse));
        drop(reading);

        let writing = begin_write(id, &turn, true);
        assert!(matches!(writing, Begin::Started(_)));
        let state = state(id);
        assert_eq!(
            state,
            Some(State {
                depth: 1,
                write_locks: 0,
                snapshots: 1,
                has_turn: true
            })
        );
        assert!(turn.try_lock().is_err(), "this thread has the turn");
        drop(writing);
        assert!(turn.try_lock().is_ok(), "the turn is released");
        assert!(!held_by_this_thread(id));
    }
}
