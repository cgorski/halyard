//! The error type for DOM operations in the browser renderer.

use wasm_bindgen::{JsCast, JsValue};

/// A DOM operation that the browser refused, or that gave back something unusable.
///
/// The renderer cannot return these yet: the `Renderer` and `Mountable` traits are
/// infallible until the DOM layer returns typed errors (`docs/no-panics.md`, change 3).
/// Until then each operation logs its `DomError` with [`DomError::warn`] and recovers
/// (a placeholder node, or skipping the operation) instead of panicking.
#[derive(Debug, thiserror::Error)]
#[error("{op}: {detail}")]
pub(crate) struct DomError {
    /// The operation, e.g. `document.createElement`.
    op: &'static str,
    /// What went wrong: the exception the browser threw, or what was unexpected.
    detail: String,
}

impl DomError {
    /// An operation that gave back something unusable; `detail` says what.
    pub(crate) fn new(op: &'static str, detail: impl Into<String>) -> Self {
        Self {
            op,
            detail: detail.into(),
        }
    }

    /// An operation that threw `thrown`.
    pub(crate) fn thrown(op: &'static str, thrown: &JsValue) -> Self {
        let detail = match thrown.dyn_ref::<js_sys::Error>() {
            // `DOMException` is an `Error` too
            Some(err) => format!("{}: {}", err.name(), err.message()),
            None => thrown.as_string().unwrap_or_else(|| format!("{thrown:?}")),
        };
        Self::new(op, detail)
    }

    /// Logs this error as a console warning, with what the renderer did instead
    /// (`recovery`) and, if given, the node involved (clickable in the browser's console).
    ///
    /// Logged in every build: each of these used to be a panic that ended the app, and a
    /// release build has no other trace of it.
    pub(crate) fn warn(&self, recovery: &str, node: Option<&JsValue>) {
        let message = JsValue::from_str(&format!(
            "[halyard] DOM operation failed: {self}\n  recovery: {recovery}"
        ));
        match node {
            Some(node) => web_sys::console::warn_2(&message, node),
            None => web_sys::console::warn_1(&message),
        }
    }
}
