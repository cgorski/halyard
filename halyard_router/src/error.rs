//! What the router does instead of panicking (README, "Project policy": no panics, ever):
//! a typed error, logged, and a recovery that keeps the page usable. Each variant says what
//! happens instead.

use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, thiserror::Error)]
pub(crate) enum RouterError {
    /// A hook or component that needs the router's context was used outside `<Router>`.
    #[error("{what} is used outside a <Router>; {instead}")]
    NoRouter {
        what: &'static str,
        instead: &'static str,
    },
    /// A hook or component that needs a matched route was used outside one.
    #[error("{what} is used outside a matched <Route>; {instead}")]
    NoMatchedRoute {
        what: &'static str,
        instead: &'static str,
    },
    /// A navigate function from `use_navigate()` outside a router was called.
    #[error(
        "not navigating to {path:?}: the navigate function comes from use_navigate() \
         called outside a <Router>"
    )]
    NavigateWithoutRouter { path: String },

    #[cfg(feature = "ssr")]
    #[error(
        "<Router> is rendered on the server without a `RequestUrl` in context (the server \
         integration provides one per request); rendering the page for `/`"
    )]
    NoRequestUrl,
    #[cfg(feature = "ssr")]
    #[error(
        "the request URL {url:?} cannot be parsed ({source}); rendering the page for `/`"
    )]
    UnparsableRequestUrl {
        url: String,
        source: url::ParseError,
    },
    /// Server rendering and `hydrate_body()` need every matched route synchronously.
    #[error(
        "a matched route was not ready during {during} (a lazy route, or a route whose \
         data loads asynchronously); {instead}"
    )]
    RouteNotReady {
        during: &'static str,
        instead: &'static str,
    },
    #[error(
        "<FlatRoutes> does not render nested routes; it renders the parent route and \
         ignores its children (use <Routes> for nested routes)"
    )]
    NestedRoutesInFlatRoutes,
    #[error(
        "attributes cannot be added to {component}, which has no element of its own; \
         they are ignored"
    )]
    AttributesIgnored { component: &'static str },
    #[error(
        "the static route {path} has regeneration triggers but its page did not provide \
         route params; it will not be regenerated"
    )]
    StaticRegenerationWithoutParams { path: String },
    #[error(
        "rendering the static route {path} stopped before it finished (its task was \
         dropped or panicked); nothing was written for it"
    )]
    StaticRenderAborted { path: String },
    /// A browser API that the router uses threw, or is missing.
    #[error("{action} failed ({reason}); {instead}")]
    Browser {
        action: &'static str,
        reason: String,
        instead: &'static str,
    },
    /// `AnyChooseView`, `AnyNestedMatch` and `AnyNestedRoute` keep a type-erased value
    /// next to functions for its type, both made by the one constructor, so the types match.
    #[error(
        "{what} holds a value of another type than the one its functions were made \
         for; {instead}"
    )]
    ErasedTypeMismatch {
        what: &'static str,
        instead: &'static str,
    },
}

/// Logs an error that the router recovered from: with `tracing` when that feature is on,
/// otherwise in the browser console or on standard error.
pub(crate) fn report(error: &RouterError) {
    #[cfg(feature = "tracing")]
    tracing::error!("{error}");
    #[cfg(not(feature = "tracing"))]
    log_error(&format!("[halyard_router] {error}"));
}

/// Logs `error` the first time `reported` is seen unset: for misuse (a hook outside a
/// router) that would otherwise be logged on every render.
pub(crate) fn report_once(reported: &AtomicBool, error: &RouterError) {
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

/// A thrown JavaScript value, for an error message.
pub(crate) fn js_reason(value: &wasm_bindgen::JsValue) -> String {
    value.as_string().unwrap_or_else(|| format!("{value:?}"))
}

/// Why the browser location cannot be read or changed outside a browser's main thread
/// (`halyard_tachys::dom::window` is `None`): the error of `BrowserUrl`'s methods, which
/// their callers log.
pub(crate) fn no_window() -> wasm_bindgen::JsValue {
    wasm_bindgen::JsValue::from_str(
        "there is no window: the browser location is available only on a browser's \
         main thread",
    )
}
