//! Minimal diagnostics for the halyard proc-macro crates.
//!
//! This replaces the unmaintained `proc-macro-error2` crate with a few dozen
//! lines built on `syn::Error` and `proc_macro2::Span`:
//!
//! * [`emit_error!`] records a `syn::Error` and lets the macro keep running,
//!   so several errors can be reported at once;
//! * [`abort!`] / [`abort_call_site!`] record an error and unwind to the
//!   nearest [`entry_point`];
//! * [`entry_point`] runs a macro body, catches the unwind, and renders every
//!   recorded error with [`syn::Error::to_compile_error`], followed by the
//!   optional dummy output set with [`set_dummy`] (so downstream code still
//!   sees the item and reports fewer cascading errors).
//!
//! `rustc` hides panic output from proc-macro expansions, so the unwind used
//! by `abort!` is invisible to users; only the `compile_error!` invocations
//! are shown. This is the same mechanism `proc-macro-error` used.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate proc_macro;

#[doc(hidden)]
pub use proc_macro2;
use proc_macro2::{Span, TokenStream};
use quote::ToTokens;
use std::{
    cell::{Cell, RefCell},
    panic::{catch_unwind, resume_unwind, AssertUnwindSafe},
};

thread_local! {
    static ERRORS: RefCell<Vec<syn::Error>> = const { RefCell::new(Vec::new()) };
    static DUMMY: RefCell<Option<TokenStream>> = const { RefCell::new(None) };
    static DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// Payload used to unwind from `abort!` to `entry_point`.
struct AbortNow;

/// Runs a proc-macro body and converts recorded diagnostics into
/// `compile_error!` invocations.
///
/// If any error was emitted (or `abort!` was called), the macro's own output
/// is replaced by the errors followed by the dummy tokens (if any).
pub fn entry_point<F>(f: F) -> proc_macro::TokenStream
where
    F: FnOnce() -> proc_macro::TokenStream,
{
    DEPTH.with(|d| d.set(d.get() + 1));
    let result = catch_unwind(AssertUnwindSafe(f));
    DEPTH.with(|d| d.set(d.get() - 1));

    let errors = ERRORS.take();
    let dummy = DUMMY.take();

    match result {
        Ok(tokens) if errors.is_empty() => tokens,
        Ok(_) => render(errors, dummy).into(),
        Err(payload) if payload.is::<AbortNow>() => {
            render(errors, dummy).into()
        }
        Err(payload) => resume_unwind(payload),
    }
}

fn render(errors: Vec<syn::Error>, dummy: Option<TokenStream>) -> TokenStream {
    let mut out = TokenStream::new();
    for err in errors {
        out.extend(err.to_compile_error());
    }
    if let Some(dummy) = dummy {
        out.extend(dummy);
    }
    out
}

/// Records an error without stopping macro expansion.
pub fn emit_error(err: syn::Error) {
    check_in_entry_point();
    ERRORS.with(|errors| errors.borrow_mut().push(err));
}

/// Records an error and unwinds to the enclosing [`entry_point`].
pub fn abort(err: syn::Error) -> ! {
    emit_error(err);
    std::panic::panic_any(AbortNow)
}

/// Sets the tokens to be emitted (after the errors) if the macro aborts.
///
/// Returns the previous dummy, if any.
pub fn set_dummy(tokens: TokenStream) -> Option<TokenStream> {
    check_in_entry_point();
    DUMMY.with(|dummy| dummy.replace(Some(tokens)))
}

fn check_in_entry_point() {
    if DEPTH.with(Cell::get) == 0 {
        panic!(
            "halyard_macro_diagnostics: emit_error!/abort!/set_dummy used \
             outside of `entry_point`"
        );
    }
}

/// Formats a message plus optional `help`/`note` suggestions the way
/// `proc-macro-error` did, so error text is unchanged.
#[doc(hidden)]
pub fn format_message(
    msg: String,
    suggestions: &[(&'static str, String)],
) -> String {
    if suggestions.is_empty() {
        return msg;
    }
    let mut message = msg;
    if !message.ends_with('\n') {
        message.push('\n');
    }
    message.push('\n');
    for (kind, note) in suggestions {
        message.push_str("  = ");
        message.push_str(kind);
        message.push_str(": ");
        message.push_str(note);
        if !note.ends_with('\n') {
            message.push('\n');
        }
    }
    message.push('\n');
    message
}

/// Build a `syn::Error` from either a `Span` or anything `ToTokens`.
///
/// Two traits with the same method name are used so that method resolution
/// picks the right one by receiver type (`Span` does not implement
/// `ToTokens`); this is the same autoref trick `proc-macro-error` used.
#[doc(hidden)]
pub mod span {
    use super::*;

    /// `Span` receivers.
    pub trait SpanDiagnostic {
        /// Creates an error at this span.
        fn __halyard_diag_error(&self, msg: String) -> syn::Error;
    }

    impl SpanDiagnostic for Span {
        fn __halyard_diag_error(&self, msg: String) -> syn::Error {
            syn::Error::new(*self, msg)
        }
    }

    /// `ToTokens` receivers (spans the whole token range).
    pub trait TokensDiagnostic {
        /// Creates an error spanning these tokens.
        fn __halyard_diag_error(&self, msg: String) -> syn::Error;
    }

    impl<T: ToTokens> TokensDiagnostic for T {
        fn __halyard_diag_error(&self, msg: String) -> syn::Error {
            syn::Error::new_spanned(self, msg)
        }
    }
}

/// Builds a `syn::Error` from `(span_or_tokens, fmt, args...; help = ..)`.
#[doc(hidden)]
#[macro_export]
macro_rules! __diag_error {
    ($span:expr, $($rest:tt)*) => {{
        #[allow(unused_imports)]
        use $crate::span::{SpanDiagnostic as _, TokensDiagnostic as _};
        let (msg, suggestions) = $crate::__diag_message!($($rest)*);
        let msg = $crate::format_message(msg, &suggestions);
        ($span).__halyard_diag_error(msg)
    }};
}

/// Splits `fmt, args...; help = "..."; note = "..."` into a message string
/// and a list of suggestions.
#[doc(hidden)]
#[macro_export]
macro_rules! __diag_message {
    // single expression, no format args (e.g. a `syn::Error` or `String`)
    ($msg:expr $(;)?) => {
        (::std::string::ToString::to_string(&$msg), ::std::vec::Vec::<(&'static str, ::std::string::String)>::new())
    };
    ($msg:expr; $($kind:ident = $note:expr),+ $(,)?) => {
        (
            ::std::string::ToString::to_string(&$msg),
            ::std::vec![$((stringify!($kind), ::std::string::ToString::to_string(&$note))),+],
        )
    };
    ($fmt:expr, $($arg:expr),+ $(,)?) => {
        (::std::format!($fmt, $($arg),+), ::std::vec::Vec::<(&'static str, ::std::string::String)>::new())
    };
    ($fmt:expr, $($arg:expr),+; $($kind:ident = $note:expr),+ $(,)?) => {
        (
            ::std::format!($fmt, $($arg),+),
            ::std::vec![$((stringify!($kind), ::std::string::ToString::to_string(&$note))),+],
        )
    };
}

/// Records an error at the given span or tokens and unwinds to the enclosing
/// [`entry_point`].
///
/// ```ignore
/// abort!(span, "message");
/// abort!(tokens, "expected {}, found {}", a, b);
/// abort!(span, "message"; help = "try this instead");
/// ```
#[macro_export]
macro_rules! abort {
    ($span:expr, $($rest:tt)*) => {
        $crate::abort($crate::__diag_error!($span, $($rest)*))
    };
}

/// Like [`abort!`] but at `Span::call_site()`.
#[macro_export]
macro_rules! abort_call_site {
    ($($rest:tt)*) => {
        $crate::abort!($crate::proc_macro2::Span::call_site(), $($rest)*)
    };
}

/// Records an error at the given span or tokens and continues.
#[macro_export]
macro_rules! emit_error {
    ($span:expr, $($rest:tt)*) => {
        $crate::emit_error($crate::__diag_error!($span, $($rest)*))
    };
}

/// Extension trait mirroring `proc_macro_error2::OptionExt`.
pub trait OptionExt {
    /// The wrapped type.
    type Some;
    /// Unwraps the option or aborts at the call site with `message`.
    fn expect_or_abort(self, message: &str) -> Self::Some;
}

impl<T> OptionExt for Option<T> {
    type Some = T;
    fn expect_or_abort(self, message: &str) -> T {
        match self {
            Some(res) => res,
            None => abort!(Span::call_site(), message),
        }
    }
}

/// Extension trait mirroring `proc_macro_error2::ResultExt`.
pub trait ResultExt {
    /// The `Ok` type.
    type Ok;
    /// Unwraps the result or aborts with the error's span and message.
    fn unwrap_or_abort(self) -> Self::Ok;
}

impl<T> ResultExt for Result<T, syn::Error> {
    type Ok = T;
    fn unwrap_or_abort(self) -> T {
        match self {
            Ok(res) => res,
            Err(e) => abort(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;

    fn run(f: impl FnOnce() -> TokenStream) -> String {
        // `proc_macro::TokenStream` can't be constructed outside of a macro
        // invocation, so test the pieces `entry_point` is made of.
        DEPTH.with(|d| d.set(d.get() + 1));
        let result = catch_unwind(AssertUnwindSafe(f));
        DEPTH.with(|d| d.set(d.get() - 1));
        let errors = ERRORS.take();
        let dummy = DUMMY.take();
        match result {
            Ok(tokens) if errors.is_empty() => tokens.to_string(),
            Ok(_) => render(errors, dummy).to_string(),
            Err(payload) if payload.is::<AbortNow>() => {
                render(errors, dummy).to_string()
            }
            Err(payload) => resume_unwind(payload),
        }
    }

    #[test]
    fn success_passes_through() {
        assert_eq!(run(|| quote! { ok }), "ok");
    }

    #[test]
    fn abort_renders_compile_error_and_dummy() {
        let out = run(|| {
            set_dummy(quote! { struct Dummy; });
            abort!(Span::call_site(), "boom {}", 1);
        });
        assert!(out.contains("compile_error !"), "{out}");
        assert!(out.contains("\"boom 1\""), "{out}");
        assert!(out.contains("struct Dummy ;"), "{out}");
    }

    #[test]
    fn emit_error_collects_multiple() {
        let out = run(|| {
            emit_error!(Span::call_site(), "first");
            emit_error!(quote! { some tokens }, "second");
            quote! { dropped }
        });
        assert_eq!(out.matches("compile_error !").count(), 2, "{out}");
        assert!(!out.contains("dropped"), "{out}");
    }

    #[test]
    fn help_is_appended_like_proc_macro_error() {
        let out = run(|| {
            abort!(Span::call_site(), "msg"; help = "try this");
        });
        assert!(out.contains("msg\\n\\n  = help: try this\\n\\n"), "{out}");
    }

    #[test]
    fn non_abort_panics_propagate() {
        let res = catch_unwind(|| run(|| panic!("real bug")));
        assert!(res.is_err());
    }
}
