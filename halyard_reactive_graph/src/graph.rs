//! Types that define the reactive graph itself. These are mostly internal, but can be used to
//! create custom reactive primitives.

mod node;
mod sets;
mod source;
mod subscriber;

pub use node::*;
pub(crate) use sets::*;
pub use source::*;
pub use subscriber::*;

/// The node behind inert sources and subscribers ([`AnySource::inert`],
/// [`AnySubscriber::inert`]). It has no values: they hold a weak reference that can never be
/// upgraded, so every operation on them does nothing.
pub(crate) enum Inert {}

impl ReactiveNode for Inert {
    fn mark_dirty(&self) {
        match *self {}
    }

    fn mark_check(&self) {
        match *self {}
    }

    fn mark_subscribers_check(&self) {
        match *self {}
    }

    fn update_if_necessary(&self) -> bool {
        match *self {}
    }
}

impl Source for Inert {
    fn add_subscriber(&self, _subscriber: AnySubscriber) {
        match *self {}
    }

    fn remove_subscriber(&self, _subscriber: &AnySubscriber) {
        match *self {}
    }

    fn clear_subscribers(&self) {
        match *self {}
    }
}

impl Subscriber for Inert {
    fn add_source(&self, _source: AnySource) {
        match *self {}
    }

    fn clear_sources(&self, _subscriber: &AnySubscriber) {
        match *self {}
    }
}
