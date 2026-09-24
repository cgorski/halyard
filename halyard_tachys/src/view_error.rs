//! What the view layer does instead of panicking (README, "Project policy": no panics,
//! ever): a typed error, logged, and a recovery that keeps the page usable. Each variant
//! says what happens instead.

use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, thiserror::Error)]
pub(crate) enum ViewError {
    /// A type-erased view or attribute keeps its HTML renderer only with `ssr`.
    #[cfg(not(feature = "ssr"))]
    #[error(
        "{what} was rendered to HTML, but halyard_tachys was built without the `ssr` \
         feature, which it needs to render HTML; nothing is rendered for it"
    )]
    RenderedWithoutSsr { what: &'static str },
    /// A type-erased view or attribute keeps its hydration code only with `hydrate`.
    #[cfg(not(feature = "hydrate"))]
    #[error(
        "{what} was hydrated, but halyard_tachys was built without the `hydrate` \
         feature, which it needs to hydrate; it is created on the client instead"
    )]
    HydratedWithoutHydrate { what: &'static str },
    /// `ViewTemplate` clones a `<template>` rendered from types known at compile time.
    #[error(
        "{what} cannot be hydrated from a <template> (ViewTemplate, `template!`), \
         whose markup is fixed at compile time; it is created on the client instead"
    )]
    NotInTemplate { what: &'static str },
    #[error(
        "attributes cannot be added to {what}, which has no element of its own to \
         add them to; they are ignored"
    )]
    AttributesIgnored { what: &'static str },
    /// `EitherKeepAlive`'s fields are public: it can be told to show a side it has no view
    /// for.
    #[error(
        "EitherKeepAlive shows its `{side}` side, which was never given a view; it \
         renders nothing"
    )]
    KeepAliveSideMissing { side: &'static str },
    #[error(
        "EitherKeepAlive cannot switch from its `{from}` side to its `{to}` side: \
         one of them was never given a view; it keeps showing `{from}`"
    )]
    KeepAliveCannotSwitch {
        from: &'static str,
        to: &'static str,
    },
    /// Client-side values are not created when `ssr` is active (`FEATURE_CONFLICT_DIAGNOSTIC`).
    #[error(
        "{what} is missing: {} It {instead}.",
        crate::html::FEATURE_CONFLICT_DIAGNOSTIC
    )]
    ClientValueMissing {
        what: &'static str,
        instead: &'static str,
    },
    /// The render effect behind a reactive attribute or view normally always holds its
    /// last value between runs.
    #[error(
        "the effect behind {what} held no value when it was updated; {instead}"
    )]
    EffectWithoutValue {
        what: &'static str,
        instead: &'static str,
    },
    #[error(
        "OwnedView::new was called outside any reactive owner; the view gets a new \
         root owner"
    )]
    NoOwner,
    #[error(
        "more than 65536 out-of-order chunks at one level of the stream: chunk ids \
         wrap around to 0, so a chunk still pending under a reused id may be streamed \
         into the wrong placeholder"
    )]
    ChunkIdWrapped,
    #[error(
        "the out-of-order chunk {id:?} has an opening marker in the stream but no \
         closing marker after it (raw HTML with a comment that looks like a chunk \
         marker?); it is streamed in a <template> instead of in place"
    )]
    UnclosedChunkMarker { id: String },
    /// There is a document only on a browser's main thread (`crate::dom::document`). The
    /// mount functions check for one first, so this is a view built by hand elsewhere.
    #[error(
        "the DOM renderer has no document to create nodes in: there is one only on a \
         browser's main thread, not in a web worker or a native build such as the \
         server; the nodes it creates are stand-ins that are not in any document, so \
         nothing is shown"
    )]
    NoDocument,
    /// `AnyView`, `AnyAttribute` and their states keep a type-erased value next to
    /// functions for its type, both made by the one constructor, so the types match.
    #[error(
        "{what} holds a value of another type than the one its functions were made \
         for; {instead}"
    )]
    ErasedTypeMismatch {
        what: &'static str,
        instead: &'static str,
    },
    /// Only an event that was created but never dispatched has no target, and listeners
    /// run only for dispatched events.
    #[error(
        "{what} received an event without a target (created, but never \
         dispatched); {instead}"
    )]
    NoEventTarget {
        what: &'static str,
        instead: &'static str,
    },
}

/// Logs an error that the view layer recovered from: with `tracing` when that feature is
/// on, otherwise in the browser console or on standard error.
pub(crate) fn report(error: &ViewError) {
    #[cfg(feature = "tracing")]
    tracing::error!("{error}");
    #[cfg(not(feature = "tracing"))]
    log_error(&format!("[halyard] {error}"));
}

/// Logs `error` the first time `reported` is seen unset: for misuse that would otherwise be
/// logged on every render.
pub(crate) fn report_once(reported: &AtomicBool, error: &ViewError) {
    if !reported.swap(true, Ordering::Relaxed) {
        report(error);
    }
}

#[cfg(all(
    not(feature = "tracing"),
    target_arch = "wasm32",
    not(any(target_os = "emscripten", target_os = "wasi"))
))]
fn log_error(message: &str) {
    web_sys::console::error_1(&wasm_bindgen::JsValue::from_str(message));
}

#[cfg(all(
    not(feature = "tracing"),
    not(all(
        target_arch = "wasm32",
        not(any(target_os = "emscripten", target_os = "wasi"))
    ))
))]
fn log_error(message: &str) {
    use std::io::Write;
    // `eprintln!` panics if standard error is closed; with nowhere left to report the
    // error, it is dropped
    _ = writeln!(std::io::stderr(), "{message}");
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Values whose HTML length estimate is `usize::MAX`. A custom view, attribute value or
    //! class may return any estimate, and adding the length of the markup around one used to
    //! overflow.

    use crate::{
        html::{
            attribute::{
                any_attribute::AnyAttribute, Attribute, AttributeValue,
            },
            class::IntoClass,
        },
        hydration::Cursor,
        renderer::types::Element,
        view::{
            add_attr::AddAnyAttr, Position, PositionState, Render, RenderHtml,
        },
    };

    /// A view that renders `huge` and estimates its length as `usize::MAX`.
    pub(crate) struct HugeView;

    impl Render for HugeView {
        type State = ();

        fn build(self) -> Self::State {}

        fn rebuild(self, _state: &mut Self::State) {}
    }

    impl AddAnyAttr for HugeView {
        type Output<SomeNewAttr: Attribute> = Self;

        fn add_any_attr<NewAttr: Attribute>(
            self,
            _attr: NewAttr,
        ) -> Self::Output<NewAttr> {
            self
        }
    }

    impl RenderHtml for HugeView {
        type AsyncOutput = Self;
        type Owned = Self;

        const MIN_LENGTH: usize = 0;

        fn dry_resolve(&mut self) {}

        async fn resolve(self) -> Self::AsyncOutput {
            self
        }

        fn html_len(&self) -> usize {
            usize::MAX
        }

        fn to_html_with_buf(
            self,
            buf: &mut String,
            position: &mut Position,
            _escape: bool,
            _mark_branches: bool,
            _extra_attrs: Vec<AnyAttribute>,
        ) {
            buf.push_str("huge");
            *position = Position::NextChild;
        }

        fn hydrate<const FROM_SERVER: bool>(
            self,
            _cursor: &Cursor,
            _position: &PositionState,
        ) -> Self::State {
        }

        fn into_owned(self) -> Self::Owned {
            self
        }
    }

    /// An attribute value that renders `"huge"` and estimates its length as `usize::MAX`.
    #[derive(Clone)]
    pub(crate) struct HugeValue;

    impl AttributeValue for HugeValue {
        type State = ();
        type AsyncOutput = Self;
        type Cloneable = Self;
        type CloneableOwned = Self;

        fn html_len(&self) -> usize {
            usize::MAX
        }

        fn to_html(self, key: &str, buf: &mut String) {
            buf.push(' ');
            buf.push_str(key);
            buf.push_str("=\"huge\"");
        }

        fn to_template(_key: &str, _buf: &mut String) {}

        fn hydrate<const FROM_SERVER: bool>(
            self,
            _key: &str,
            _el: &Element,
        ) -> Self::State {
        }

        fn build(self, _el: &Element, _key: &str) -> Self::State {}

        fn rebuild(self, _key: &str, _state: &mut Self::State) {}

        fn into_cloneable(self) -> Self::Cloneable {
            self
        }

        fn into_cloneable_owned(self) -> Self::CloneableOwned {
            self
        }

        fn dry_resolve(&mut self) {}

        async fn resolve(self) -> Self::AsyncOutput {
            self
        }
    }

    /// A class that renders `huge` and estimates its length as `usize::MAX`.
    #[derive(Clone)]
    pub(crate) struct HugeClass;

    impl IntoClass for HugeClass {
        type AsyncOutput = Self;
        type State = ();
        type Cloneable = Self;
        type CloneableOwned = Self;

        fn html_len(&self) -> usize {
            usize::MAX
        }

        fn to_html(self, class: &mut String) {
            class.push_str("huge");
        }

        fn hydrate<const FROM_SERVER: bool>(
            self,
            _el: &Element,
        ) -> Self::State {
        }

        fn build(self, _el: &Element) -> Self::State {}

        fn rebuild(self, _state: &mut Self::State) {}

        fn into_cloneable(self) -> Self::Cloneable {
            self
        }

        fn into_cloneable_owned(self) -> Self::CloneableOwned {
            self
        }

        fn dry_resolve(&mut self) {}

        async fn resolve(self) -> Self::AsyncOutput {
            self
        }

        fn reset(_state: &mut Self::State) {}
    }
}
