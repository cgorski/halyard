//! The browser's `window` and `document`, and helpers to read an event's target.
//!
//! There is a window and a document only on a browser's main thread: not in a web worker,
//! and not in a native build (the server). There these functions return `None` instead of
//! panicking (README, "Project policy": no panics, ever), and each caller decides what to do
//! without them.

use wasm_bindgen::JsCast;
use web_sys::{Document, HtmlElement, Window};

thread_local! {
    static WINDOW: Option<Window> = find_window();

    static DOCUMENT: Option<Document> =
        WINDOW.with(|window| window.as_ref().and_then(Window::document));
}

/// The global object, if it is a `Window`.
#[cfg(all(
    target_arch = "wasm32",
    not(any(target_os = "emscripten", target_os = "wasi"))
))]
fn find_window() -> Option<Window> {
    web_sys::window()
}

/// A native build has no browser (and calling into JavaScript there panics).
#[cfg(not(all(
    target_arch = "wasm32",
    not(any(target_os = "emscripten", target_os = "wasi"))
)))]
fn find_window() -> Option<Window> {
    None
}

/// Returns the [`Window`](https://developer.mozilla.org/en-US/docs/Web/API/Window), or
/// `None` outside a browser's main thread (in a web worker, or in a native build such as
/// the server).
///
/// This is cached as a thread-local variable, so calling `window()` multiple times
/// requires only one call out to JavaScript.
pub fn window() -> Option<Window> {
    // `try_with`: while the thread's locals are being destroyed there is no window either
    WINDOW.try_with(Clone::clone).ok().flatten()
}

/// Returns the [`Document`](https://developer.mozilla.org/en-US/docs/Web/API/Document), or
/// `None` where there is no [`window`].
///
/// This is cached as a thread-local variable, so calling `document()` multiple times
/// requires only one call out to JavaScript.
pub fn document() -> Option<Document> {
    DOCUMENT.try_with(Clone::clone).ok().flatten()
}

/// The `<body>` element, or `None` if the document has none (yet: a script in the
/// `<head>` runs before the `<body>` is parsed), or if there is no [`document`].
pub fn body() -> Option<HtmlElement> {
    document()?.body()
}

/// Helper function to extract [`Event.target`](https://developer.mozilla.org/en-US/docs/Web/API/Event/target)
/// from any event, cast to `T` without a check (like
/// [`JsCast::unchecked_into`]).
///
/// `None` if the event has no target: it was created but never dispatched. (An event
/// handler always receives a dispatched event.)
pub fn event_target<T>(event: &web_sys::Event) -> Option<T>
where
    T: JsCast,
{
    event.target().map(JsCast::unchecked_into)
}

/// Helper function to extract `event.target.value` from an event.
///
/// This is useful in the `on:input` or `on:change` listeners for an `<input>` element.
///
/// `None` if the event has no target (see [`event_target`]); an empty input gives
/// `Some("")`.
pub fn event_target_value<T>(event: &T) -> Option<String>
where
    T: JsCast,
{
    event_target::<web_sys::HtmlInputElement>(event.unchecked_ref())
        .map(|input| input.value())
}

/// Helper function to extract `event.target.checked` from an event.
///
/// This is useful in the `on:change` listeners for an `<input type="checkbox">` element.
///
/// `None` if the event has no target (see [`event_target`]).
pub fn event_target_checked(ev: &web_sys::Event) -> Option<bool> {
    event_target::<web_sys::HtmlInputElement>(ev).map(|input| input.checked())
}

#[cfg(test)]
mod tests {
    use super::{body, document, window};

    /// A native build (the server, these tests) has no browser. Reading the window, the
    /// document or the body there panicked (in wasm-bindgen, which cannot call into
    /// JavaScript natively, and then in the `unwrap`s); each is absent.
    #[test]
    fn there_is_no_window_document_or_body_in_a_native_build() {
        assert!(window().is_none());
        assert!(document().is_none());
        assert!(body().is_none());
        // cached, and still absent
        assert!(window().is_none());
        assert!(document().is_none());
    }
}
