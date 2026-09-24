//! Utilities for simple isomorphic logging to the console or terminal.

use wasm_bindgen::JsValue;

#[doc(inline)]
pub use crate::__halyard_debug_error as debug_error;
#[doc(inline)]
pub use crate::__halyard_debug_log as debug_log;
#[doc(inline)]
pub use crate::__halyard_debug_warn as debug_warn;
#[doc(inline)]
pub use crate::__halyard_error as error;
#[doc(inline)]
pub use crate::__halyard_log as log;
#[doc(inline)]
pub use crate::__halyard_warn as warn;

/// Uses `println!()`-style formatting to log something to the console (in the browser)
/// or via `println!()` (if not in the browser).
#[doc(hidden)]
#[macro_export]
macro_rules! __halyard_log {
    ($($t:tt)*) => ($crate::logging::console_log(&format_args!($($t)*).to_string()))
}

/// Uses `println!()`-style formatting to log warnings to the console (in the browser)
/// or via `eprintln!()` (if not in the browser).
#[doc(hidden)]
#[macro_export]
macro_rules! __halyard_warn {
    ($($t:tt)*) => ($crate::logging::console_warn(&format_args!($($t)*).to_string()))
}

/// Uses `println!()`-style formatting to log errors to the console (in the browser)
/// or via `eprintln!()` (if not in the browser).
#[doc(hidden)]
#[macro_export]
macro_rules! __halyard_error {
    ($($t:tt)*) => ($crate::logging::console_error(&format_args!($($t)*).to_string()))
}

/// Uses `println!()`-style formatting to log something to the console (in the browser)
/// or via `println!()` (if not in the browser), but only if it's a debug build.
#[doc(hidden)]
#[macro_export]
macro_rules! __halyard_debug_log {
    ($($x:tt)*) => {
        {
            if cfg!(debug_assertions) {
                $crate::logging::log!($($x)*)
            }
        }
    }
}

/// Uses `println!()`-style formatting to log warnings to the console (in the browser)
/// or via `eprintln!()` (if not in the browser), but only if it's a debug build.
#[doc(hidden)]
#[macro_export]
macro_rules! __halyard_debug_warn {
    ($($x:tt)*) => {
        {
            if cfg!(debug_assertions) {
                $crate::logging::warn!($($x)*)
            }
        }
    }
}

/// Uses `println!()`-style formatting to log errors to the console (in the browser)
/// or via `eprintln!()` (if not in the browser), but only if it's a debug build.
#[doc(hidden)]
#[macro_export]
macro_rules! __halyard_debug_error {
    ($($x:tt)*) => {
        {
            if cfg!(debug_assertions) {
                $crate::logging::error!($($x)*)
            }
        }
    }
}

const fn log_to_stdout() -> bool {
    cfg!(not(all(
        target_arch = "wasm32",
        not(any(target_os = "emscripten", target_os = "wasi"))
    )))
}

/// Log a string to the console (in the browser)
/// or via `println!()` (if not in the browser).
pub fn console_log(s: &str) {
    #[allow(clippy::print_stdout)]
    if log_to_stdout() {
        println!("{s}");
    } else {
        web_sys::console::log_1(&JsValue::from_str(s));
    }
}

/// Log a warning to the console (in the browser)
/// or via `eprintln!()` (if not in the browser).
pub fn console_warn(s: &str) {
    if log_to_stdout() {
        eprintln!("{s}");
    } else {
        web_sys::console::warn_1(&JsValue::from_str(s));
    }
}

/// Log an error to the console (in the browser)
/// or via `eprintln!()` (if not in the browser).
#[inline(always)]
pub fn console_error(s: &str) {
    if log_to_stdout() {
        eprintln!("{s}");
    } else {
        web_sys::console::error_1(&JsValue::from_str(s));
    }
}

/// Log a string to the console (in the browser)
/// or via `println!()` (if not in the browser), but only in a debug build.
#[inline(always)]
pub fn console_debug_log(s: &str) {
    if cfg!(debug_assertions) {
        console_log(s)
    }
}

/// Log a warning to the console (in the browser)
/// or via `eprintln!()` (if not in the browser), but only in a debug build.
#[inline(always)]
pub fn console_debug_warn(s: &str) {
    if cfg!(debug_assertions) {
        console_warn(s)
    }
}

/// Log an error to the console (in the browser)
/// or via `eprintln!()` (if not in the browser), but only in a debug build.
#[inline(always)]
pub fn console_debug_error(s: &str) {
    if cfg!(debug_assertions) {
        console_error(s)
    }
}
