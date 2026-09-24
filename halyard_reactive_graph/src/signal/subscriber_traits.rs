//! Traits to reduce the boilerplate when implementing the [`ReactiveNode`], [`Source`], and
//! [`ToAnySource`] traits for signal types.
//!
//! These traits can be automatically derived for any type that
//! 1) is a root node in the reactive graph, with no sources (i.e., a signal, not a memo)
//! 2) contains an `Arc<RwLock<SubscriberSet>>`
//!
//! This makes it easy to implement a variety of different signal primitives, as long as they share
//! these characteristics.

use crate::or_poisoned::OrPoisoned;
use crate::{
    graph::{
        AnySource, AnySubscriber, ReactiveNode, Source, SubscriberSet,
        ToAnySource,
    },
    traits::{DefinedAt, IsDisposed},
};
use std::{
    borrow::Borrow,
    sync::{Arc, RwLock, Weak},
};

pub(crate) trait AsSubscriberSet {
    type Output: Borrow<RwLock<SubscriberSet>>;

    fn as_subscriber_set(&self) -> Option<Self::Output>;
}

impl<'a> AsSubscriberSet for &'a RwLock<SubscriberSet> {
    type Output = &'a RwLock<SubscriberSet>;

    #[inline(always)]
    fn as_subscriber_set(&self) -> Option<Self::Output> {
        Some(self)
    }
}

impl DefinedAt for RwLock<SubscriberSet> {
    fn defined_at(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
}

// Implement reactive types for RwLock<SubscriberSet>
// This is used so that Weak<RwLock<SubscriberSet>> is a Weak<dyn ReactiveNode> and Weak<dyn
// Source>
impl<T: AsSubscriberSet + DefinedAt> ReactiveNode for T {
    fn mark_dirty(&self) {
        self.mark_subscribers_check();
    }

    fn mark_check(&self) {}

    fn mark_subscribers_check(&self) {
        if let Some(inner) = self.as_subscriber_set() {
            let subs = inner.borrow().read().or_poisoned().clone();
            for sub in subs {
                sub.mark_dirty();
            }
        }
    }

    fn update_if_necessary(&self) -> bool {
        // a signal will always mark its dependents Dirty when it runs, so they know
        // that they may have changed and need to check themselves at least
        //
        // however, it's always possible that *another* signal or memo has triggered any
        // given effect/memo, and so this signal should *not* say that it is dirty, as it
        // may also be checked but has not changed
        false
    }
}

impl<T: AsSubscriberSet + DefinedAt> Source for T {
    fn clear_subscribers(&self) {
        if let Some(inner) = self.as_subscriber_set() {
            // the subscribers are dropped after the lock is released
            let subscribers = inner.borrow().write().or_poisoned().take();
            drop(subscribers);
        }
    }

    fn add_subscriber(&self, subscriber: AnySubscriber) {
        if let Some(inner) = self.as_subscriber_set() {
            inner.borrow().write().or_poisoned().subscribe(subscriber)
        }
    }

    fn remove_subscriber(&self, subscriber: &AnySubscriber) {
        if let Some(inner) = self.as_subscriber_set() {
            inner.borrow().write().or_poisoned().unsubscribe(subscriber)
        }
    }
}

impl<T: AsSubscriberSet + DefinedAt + IsDisposed> ToAnySource for T
where
    T::Output: Borrow<Arc<RwLock<SubscriberSet>>>,
{
    /// Once the signal's owner is gone, a source that never changes.
    #[track_caller]
    fn to_any_source(&self) -> AnySource {
        self.as_subscriber_set()
            .map(|subs| {
                let subs = subs.borrow();
                AnySource(
                    Arc::as_ptr(subs) as usize,
                    Arc::downgrade(subs) as Weak<dyn Source + Send + Sync>,
                    #[cfg(any(debug_assertions, halyard_debuginfo))]
                    self.defined_at().unwrap_or(std::panic::Location::caller()),
                )
            })
            .unwrap_or_else(|| AnySource::inert(self.defined_at()))
    }
}

impl ReactiveNode for RwLock<SubscriberSet> {
    fn mark_dirty(&self) {
        self.mark_subscribers_check();
    }

    fn mark_check(&self) {}

    fn mark_subscribers_check(&self) {
        let subs = self.write().or_poisoned().take();
        for sub in subs {
            sub.mark_dirty();
        }
    }

    fn update_if_necessary(&self) -> bool {
        // a signal will always mark its dependents Dirty when it runs, so they know
        // that they may have changed and need to check themselves at least
        //
        // however, it's always possible that *another* signal or memo has triggered any
        // given effect/memo, and so this signal should *not* say that it is dirty, as it
        // may also be checked but has not changed
        false
    }
}

impl Source for RwLock<SubscriberSet> {
    fn clear_subscribers(&self) {
        self.write().or_poisoned().take();
    }

    fn add_subscriber(&self, subscriber: AnySubscriber) {
        self.write().or_poisoned().subscribe(subscriber)
    }

    fn remove_subscriber(&self, subscriber: &AnySubscriber) {
        self.write().or_poisoned().unsubscribe(subscriber)
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        graph::{ReactiveNode, ToAnySource},
        owner::Owner,
        signal::RwSignal,
        traits::Dispose,
    };

    /// A disposed signal has no subscriber set to point at: it used to panic ("you tried to
    /// access a reactive value ... but it has already been disposed"). It is a source that
    /// never changes.
    #[test]
    fn a_disposed_signal_is_a_source_that_never_changes() {
        let owner = Owner::new();
        owner.set();
        let signal = RwSignal::new(0);
        signal.dispose();

        let source = signal.to_any_source();

        assert!(!source.update_if_necessary());
        source.mark_dirty();
    }
}
