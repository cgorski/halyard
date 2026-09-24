//! How a signal is written (docs/no-panics.md, "Re-entrant access").
//!
//! Every write takes the signal's *writer turn* (so that writes from different threads
//! serialize), and holds the value's lock only to swap a new value in: no user code runs
//! under it. An `update` runs its closure on a copy of the committed value, so reads of the
//! signal inside it see the committed value. A write made while this thread is already using
//! the signal (reading it, updating it, holding a guard of it) is deferred in the thread's
//! record ([`crate::reentry`]) and applied, in order, when the outermost use ends.

use super::{guards::UntrackedWriteGuard, ArcWriteSignal};
use crate::{
    error::{Access, GraphError, ReportOnce},
    graph::ReactiveNode,
    or_poisoned::OrPoisoned,
    reentry::{self, Begin, Held, Pending, Writing, SINGLE_THREADED},
    signal::guards::{report_reentered, Plain},
    traits::{DefinedAt, UntrackableGuard},
};
use guardian::ArcMutexGuardian;
use std::{
    any::Any,
    collections::VecDeque,
    fmt::{self, Debug},
    mem,
    ops::{Deref, DerefMut},
    panic::Location,
    sync::{Arc, TryLockError},
};

/// A write deferred until this thread's outermost use of the signal ends.
enum Deferred<T> {
    /// Replace the value, and notify subscribers if `notify`.
    Set { value: T, notify: bool },
    /// Notify subscribers.
    Notify,
}

/// The writes deferred for one signal on this thread.
struct PendingWrites<T: 'static> {
    signal: ArcWriteSignal<T>,
    writes: VecDeque<Deferred<T>>,
}

impl<T: 'static> Pending for PendingWrites<T> {
    fn flush(self: Box<Self>, turn: Option<ArcMutexGuardian<()>>) {
        let signal = self.signal.clone();
        // this thread holds nothing of the signal now: taking the turn waits only for other
        // threads
        let Some(turn) = turn.or_else(|| reentry::take_turn(&signal.turn))
        else {
            report_dropped(
                "its turn to write was busy on a single thread",
                signal.defined_at(),
            );
            return;
        };
        match reentry::resume_write(signal.id(), turn, self) {
            Some(writing) => signal.drain(&writing),
            None => report_dropped(
                "this thread is shutting down",
                signal.defined_at(),
            ),
        }
    }

    fn append(&mut self, later: Box<dyn Pending>) {
        // one value has one type: `later` is always this type
        if let Ok(later) = later.into_any().downcast::<Self>() {
            self.writes.extend(later.writes);
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }

    fn is_empty(&self) -> bool {
        self.writes.is_empty()
    }
}

static UPDATED_INSIDE_UPDATE: ReportOnce = ReportOnce::new();
static WRITE_DROPPED: ReportOnce = ReportOnce::new();
static RUNAWAY: ReportOnce = ReportOnce::new();

/// The most deferred writes one flush applies (see [`ArcWriteSignal::drain`]).
const MAX_APPLIED: usize = 100_000;

fn report_dropped(
    why: &'static str,
    defined_at: Option<&'static Location<'static>>,
) {
    WRITE_DROPPED.report(|| GraphError::WriteDropped { why, defined_at });
}

impl<T: 'static> ArcWriteSignal<T> {
    /// The signal's identity in this thread's record: the address of its value's lock.
    pub(crate) fn id(&self) -> usize {
        reentry::lock_id(&*self.value)
    }

    /// Whether this thread is using the signal (then writes are deferred).
    fn in_use_here(&self) -> bool {
        reentry::state(self.id()).is_some_and(|state| state.in_use())
    }

    /// Notifies the subscribers now.
    fn mark_subscribers(&self) {
        let subscribers = self.inner.read().or_poisoned().clone();
        for subscriber in subscribers {
            subscriber.mark_dirty();
        }
    }

    /// A copy of the committed value.
    fn read_committed(&self) -> Option<T>
    where
        T: Clone,
    {
        Plain::try_new_at(Arc::clone(&self.value), self.defined_at())
            .map(|guard| (*guard).clone())
    }

    /// A copy of the value that a write deferred now will be applied over: the last value
    /// deferred on this thread, or the committed value.
    fn pending_or_committed(&self) -> Option<T>
    where
        T: Clone,
    {
        let id = self.id();
        // taken out of the record, so that `clone` (user code) runs without it
        if let Some(mut pending) = reentry::take_pending(id) {
            let last = pending
                .as_any_mut()
                .downcast_mut::<PendingWrites<T>>()
                .and_then(|pending| {
                    pending.writes.iter().rev().find_map(|write| match write {
                        Deferred::Set { value, .. } => Some(value.clone()),
                        Deferred::Notify => None,
                    })
                });
            if let Err(pending) = reentry::put_pending(id, pending) {
                drop(pending);
                report_dropped(
                    "this thread stopped using the signal while it was deferred",
                    self.defined_at(),
                );
            }
            if last.is_some() {
                return last;
            }
        }
        self.read_committed()
    }

    /// Defers a write until this thread's outermost use of the signal ends; `first` puts it
    /// ahead of the writes deferred so far.
    fn defer(&self, write: Deferred<T>, first: bool) {
        let id = self.id();
        let mut pending = reentry::take_pending(id).unwrap_or_else(|| {
            Box::new(PendingWrites {
                signal: self.clone(),
                writes: VecDeque::new(),
            })
        });
        if let Some(writes) = pending
            .as_any_mut()
            .downcast_mut::<PendingWrites<T>>()
            .map(|pending| &mut pending.writes)
        {
            if first {
                writes.push_front(write);
            } else {
                writes.push_back(write);
            }
        }
        if let Err(pending) = reentry::put_pending(id, pending) {
            drop(pending);
            report_dropped(
                "this thread's use of the signal could not be recorded",
                self.defined_at(),
            );
        }
    }

    /// Swaps `value` in as the committed value (`value` then holds the previous one, to drop
    /// once the lock is released). This thread holds nothing else of the value.
    fn swap_in(&self, value: &mut T) -> bool {
        let mut committed = if SINGLE_THREADED {
            match self.value.try_write() {
                Ok(guard) => guard,
                Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
                Err(TryLockError::WouldBlock) => {
                    report_reentered(Access::Write, self.defined_at());
                    return false;
                }
            }
        } else {
            self.value.write().or_poisoned()
        };
        mem::swap(&mut *committed, value);
        true
    }

    /// Commits `value` for the write `writing`, now if nothing else of this thread uses the
    /// signal (then `value` holds the previous value, to drop). Returns `false` if another
    /// use alive on this thread holds the value (a guard that a closure kept): then the caller
    /// defers it, ahead of the writes deferred meanwhile.
    fn commit_now(
        &self,
        writing: &Writing,
        value: &mut T,
        notify: bool,
    ) -> bool {
        let alone =
            reentry::state(writing.id()).is_some_and(|state| state.depth <= 1);
        if !alone || !self.swap_in(value) {
            return false;
        }
        if notify {
            self.mark_subscribers();
        }
        true
    }

    /// Commits `value` for the write `writing`, or defers it (see [`Self::commit_now`]).
    fn commit(&self, writing: &Writing, mut value: T, notify: bool) {
        if !self.commit_now(writing, &mut value, notify) {
            self.defer(Deferred::Set { value, notify }, true);
        }
    }

    /// Applies the writes deferred on this thread, in order, while `writing` holds the turn.
    ///
    /// Subscribers notified here may write the signal again (deferred, and applied by this
    /// loop). One that does so on every change would never let it end: after
    /// [`MAX_APPLIED`] writes the rest are dropped, and that is logged.
    fn drain(&self, writing: &Writing) {
        let id = writing.id();
        let mut applied = 0_usize;
        loop {
            if applied >= MAX_APPLIED {
                RUNAWAY.report(|| GraphError::RunawayWrites {
                    applied: MAX_APPLIED,
                    defined_at: self.defined_at(),
                });
                drop(reentry::take_pending(id));
                break;
            }
            applied = applied.saturating_add(1);
            // another use alive on this thread holds the value (a guard that a subscriber
            // kept): the rest is applied when it ends
            if !reentry::state(id).is_some_and(|state| state.depth <= 1) {
                break;
            }
            let next = reentry::take_pending(id).and_then(|mut pending| {
                let next = pending
                    .as_any_mut()
                    .downcast_mut::<PendingWrites<T>>()
                    .and_then(|pending| pending.writes.pop_front());
                if !pending.is_empty() {
                    if let Err(pending) = reentry::put_pending(id, pending) {
                        drop(pending);
                    }
                }
                next
            });
            match next {
                Some(Deferred::Set { mut value, notify }) => {
                    let swapped = self.swap_in(&mut value);
                    // the previous value, dropped after the lock is released
                    drop(value);
                    if swapped && notify {
                        self.mark_subscribers();
                    }
                }
                Some(Deferred::Notify) => self.mark_subscribers(),
                None => break,
            }
        }
    }

    /// `set`: replaces the value and notifies subscribers, or defers that while this thread
    /// uses the signal.
    pub(crate) fn set_value(&self, value: T) {
        match reentry::begin_write(self.id(), &self.turn, false) {
            Begin::Started(writing) => {
                self.commit(&writing, value, true);
                // the writes deferred by subscribers are applied here
                drop(writing);
            }
            Begin::InUse => self.defer(
                Deferred::Set {
                    value,
                    notify: true,
                },
                false,
            ),
            Begin::Unavailable => self.set_unrecorded(value),
        }
    }

    /// Writes without this thread's record, which is gone (the thread is shutting down, and
    /// its thread-locals with it): only if neither the turn nor the value is busy, never
    /// waiting, since this thread's own use can no longer be told apart.
    fn set_unrecorded(&self, mut value: T) {
        let turn = match self.turn.try_lock() {
            Ok(turn) => turn,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                report_dropped(
                    "the thread is shutting down and the signal is busy",
                    self.defined_at(),
                );
                return;
            }
        };
        let swapped = match self.value.try_write() {
            Ok(mut committed) => {
                mem::swap(&mut *committed, &mut value);
                true
            }
            Err(TryLockError::Poisoned(poisoned)) => {
                mem::swap(&mut *poisoned.into_inner(), &mut value);
                true
            }
            Err(TryLockError::WouldBlock) => false,
        };
        drop(turn);
        // the previous value, or the one that could not be written
        drop(value);
        if swapped {
            self.mark_subscribers();
        } else {
            report_dropped(
                "the thread is shutting down and the signal is busy",
                self.defined_at(),
            );
        }
    }

    /// `notify`: notifies subscribers, or defers that while this thread uses the signal.
    pub(crate) fn notify_or_defer(&self) {
        if self.in_use_here() {
            self.defer(Deferred::Notify, false);
        } else {
            self.mark_subscribers();
        }
    }

    /// `update`/`maybe_update`: runs `fun` on a copy of the committed value, outside every
    /// lock, and commits the result (notifying if `fun` returns `(true, _)`).
    ///
    /// While this thread uses the signal, `fun` runs at once on the value the write will be
    /// applied over, and the result is deferred.
    pub(crate) fn update_snapshot<U>(
        &self,
        fun: impl FnOnce(&mut T) -> (bool, U),
    ) -> Option<U>
    where
        T: Clone,
    {
        match reentry::begin_write(self.id(), &self.turn, true) {
            Begin::Started(writing) => {
                let mut value = self.read_committed()?;
                let (changed, out) = fun(&mut value);
                self.commit(&writing, value, changed);
                drop(writing);
                Some(out)
            }
            Begin::InUse => {
                if reentry::state(self.id())
                    .is_some_and(|state| state.snapshots > 0)
                {
                    UPDATED_INSIDE_UPDATE.report(|| {
                        GraphError::UpdatedInsideUpdate {
                            defined_at: self.defined_at(),
                        }
                    });
                }
                let mut value = self.pending_or_committed()?;
                let (changed, out) = fun(&mut value);
                self.defer(
                    Deferred::Set {
                        value,
                        notify: changed,
                    },
                    false,
                );
                Some(out)
            }
            Begin::Unavailable => {
                let mut value = match self.value.try_read() {
                    Ok(committed) => (*committed).clone(),
                    Err(TryLockError::Poisoned(poisoned)) => {
                        (*poisoned.into_inner()).clone()
                    }
                    Err(TryLockError::WouldBlock) => {
                        report_dropped(
                            "the thread is shutting down and the signal is busy",
                            self.defined_at(),
                        );
                        return None;
                    }
                };
                let (changed, out) = fun(&mut value);
                if changed {
                    self.set_unrecorded(value);
                }
                Some(out)
            }
        }
    }

    /// `try_update`: runs `fun` on the value in place, holding its lock; `None` if this
    /// thread uses the signal already.
    pub(crate) fn update_in_place<U>(
        &self,
        fun: impl FnOnce(&mut T) -> (bool, U),
    ) -> Option<U> {
        let writing = self.begin_in_place()?;
        // keeps this thread's use of the signal, and its turn, until the notification below
        // is done; the writes deferred meanwhile are applied when it is dropped
        let _using = Held::new(writing.id());
        let mut guard = UntrackedWriteGuard::for_write(
            Arc::clone(&self.value),
            writing,
            self.defined_at(),
        )?;
        let (changed, out) = fun(&mut guard);
        drop(guard);
        if changed {
            self.mark_subscribers();
        }
        Some(out)
    }

    fn begin_in_place(&self) -> Option<Writing> {
        match reentry::begin_write(self.id(), &self.turn, false) {
            Begin::Started(writing) => Some(writing),
            Begin::InUse => {
                report_reentered(Access::Write, self.defined_at());
                None
            }
            Begin::Unavailable => None,
        }
    }

    /// `try_write_in_place`: a guard that changes the value in place; `None` if this thread
    /// uses the signal already.
    pub(crate) fn in_place_guard(&self) -> Option<UntrackedWriteGuard<T>> {
        let writing = self.begin_in_place()?;
        UntrackedWriteGuard::for_write(
            Arc::clone(&self.value),
            writing,
            self.defined_at(),
        )
    }

    /// `try_write`: a guard holding a copy of the value, committed when it is dropped.
    pub(crate) fn snapshot_guard(&self) -> Option<SignalWriteGuard<T>>
    where
        T: Clone,
    {
        let (value, access) =
            match reentry::begin_write(self.id(), &self.turn, true) {
                Begin::Started(writing) => {
                    (self.read_committed()?, GuardAccess::Writing(writing))
                }
                Begin::InUse => {
                    let held = Held::snapshot(self.id());
                    (
                        self.pending_or_committed()?,
                        GuardAccess::Nested { _held: held },
                    )
                }
                Begin::Unavailable => return None,
            };
        Some(SignalWriteGuard {
            value,
            signal: self.clone(),
            notify: true,
            access,
        })
    }
}

/// How a [`SignalWriteGuard`] uses its signal.
#[derive(Debug)]
enum GuardAccess {
    /// It holds the writer turn: it commits when dropped.
    Writing(Writing),
    /// This thread was using the signal already: its value is deferred when dropped.
    Nested { _held: Held },
}

/// The guard returned by a signal's [`Write::try_write`](crate::traits::Write::try_write):
/// it holds a copy of the value, which it commits (notifying subscribers, unless
/// [untracked](UntrackableGuard::untrack)) when it is dropped. No lock is held while it is
/// alive: the signal can be read (giving the committed value) and written (the write is
/// deferred until the guard has committed).
pub struct SignalWriteGuard<T: Clone + 'static> {
    // dropped first: after the commit, the previous value
    value: T,
    signal: ArcWriteSignal<T>,
    notify: bool,
    // dropped last: ends the access, applying the writes deferred meanwhile
    access: GuardAccess,
}

impl<T: Clone + 'static> Debug for SignalWriteGuard<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignalWriteGuard")
            .field("signal", &self.signal)
            .finish_non_exhaustive()
    }
}

impl<T: Clone + 'static> Deref for SignalWriteGuard<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl<T: Clone + 'static> DerefMut for SignalWriteGuard<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.value
    }
}

impl<T: Clone + 'static> UntrackableGuard for SignalWriteGuard<T> {
    fn untrack(&mut self) {
        self.notify = false;
    }
}

impl<T: Clone + 'static> Drop for SignalWriteGuard<T> {
    fn drop(&mut self) {
        // `value` cannot be moved out of `&mut self`: committed now, it is swapped in;
        // deferred (only when the write is re-entrant), a copy of it is kept
        match &self.access {
            GuardAccess::Writing(writing) => {
                if !self.signal.commit_now(
                    writing,
                    &mut self.value,
                    self.notify,
                ) {
                    self.signal.defer(
                        Deferred::Set {
                            value: self.value.clone(),
                            notify: self.notify,
                        },
                        true,
                    );
                }
            }
            GuardAccess::Nested { .. } => self.signal.defer(
                Deferred::Set {
                    value: self.value.clone(),
                    notify: self.notify,
                },
                false,
            ),
        }
    }
}
