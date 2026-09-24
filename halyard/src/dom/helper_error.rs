//! What the DOM helpers could not do, logged in place of the panics these replace.

use wasm_bindgen::{JsCast, JsValue};

/// Something a DOM helper could not do.
///
/// None of these stops the app: each is logged with [`HelperError::warn`], with what the
/// helper did instead (README, "Project policy": no panics, ever).
#[derive(Debug, thiserror::Error)]
pub(crate) enum HelperError {
    /// The event has no target: it was created, but never dispatched.
    #[error("the event has no target")]
    NoEventTarget,
    /// The delay does not fit the browser's timers, which count milliseconds in a signed
    /// 32-bit integer (about 24.8 days); a longer delay would overflow and fire at once.
    #[error(
        "a delay of {millis} ms is longer than the longest the browser can wait \
         ({} ms)",
        i32::MAX
    )]
    DelayTooLong {
        /// The delay asked for.
        millis: u128,
    },
    /// A debounced callback was due while it was still running.
    #[error("the debounced callback was called while it was still running")]
    DebounceReentered,
    /// A browser API threw.
    #[error("{op} failed: {thrown}")]
    Thrown {
        /// The operation, e.g. `setTimeout`.
        op: &'static str,
        /// The exception the browser threw.
        thrown: String,
    },
}

impl HelperError {
    /// A browser API `op` that threw `thrown`.
    pub(crate) fn thrown(op: &'static str, thrown: &JsValue) -> Self {
        let thrown = match thrown.dyn_ref::<js_sys::Error>() {
            // `DOMException` is an `Error` too
            Some(error) => format!("{}: {}", error.name(), error.message()),
            None => thrown.as_string().unwrap_or_else(|| format!("{thrown:?}")),
        };
        Self::Thrown { op, thrown }
    }

    /// Logs this error as a warning (`console.warn` in the browser, standard error
    /// elsewhere), with what the helper did instead (`recovery`).
    pub(crate) fn warn(&self, recovery: &str) {
        crate::logging::console_warn(&format!("[halyard] {self}. {recovery}"));
    }
}

/// For the helpers that return the browser's exception as a `JsValue`: the error as a
/// JavaScript `Error`.
impl From<HelperError> for JsValue {
    fn from(error: HelperError) -> Self {
        js_sys::Error::new(&error.to_string()).into()
    }
}
