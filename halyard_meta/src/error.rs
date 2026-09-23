//! What `halyard_meta` could not do, logged in place of the panics these replace.

use std::{io, path::PathBuf};
use wasm_bindgen::{JsCast, JsValue};

/// Something `halyard_meta` could not do.
///
/// None of these stops the page: each is logged with [`MetaError::warn`], with what was
/// done instead, and the page renders without the affected metadata if need be (README,
/// "Project policy": no panics, ever).
#[derive(Debug, thiserror::Error)]
pub(crate) enum MetaError {
    /// Server: the first chunk of the page has neither the `<!--HEAD-->` marker of
    /// `<MetaTags/>` nor a `</head>` to put the title and meta tags before.
    #[error(
        "the start of the page has no `<!--HEAD-->` marker (rendered by \
         <MetaTags/>) and no `</head>`"
    )]
    NoHeadInFirstChunk,
    /// Server: `<Html>` or `<Body>` attributes, but no `<html` or `<body` tag in the first
    /// chunk of the page to put them on.
    #[error(
        "the start of the page has no <{tag}> tag for the attributes of \
         <{component}/>"
    )]
    NoTagInFirstChunk {
        /// `html` or `body`.
        tag: &'static str,
        /// `Html` or `Body`.
        component: &'static str,
    },
    /// Server: the file with the hashed asset names exists, but could not be read.
    #[error("could not read the hash file {}: {source}", path.display())]
    HashFile {
        /// The hash file.
        path: PathBuf,
        /// Why it could not be read.
        source: io::Error,
    },
    /// Browser: the document has no `<html>`, `<head>` or `<body>` element.
    #[error("the document has no <{0}> element")]
    NoElement(&'static str),
    /// Client-side code ran where there is no document: not on a browser's main thread
    /// (in a web worker, or in a native build).
    #[error(
        "there is no document (there is one only on a browser's main thread, not in a \
         web worker or a native build)"
    )]
    NoDocument,
    /// Browser: the document's `<head>` has no `<!--HEAD-->` marker, where the server put
    /// the tags that are hydrated.
    #[error(
        "the document's <head> has no `<!--HEAD-->` marker; is <MetaTags/> in \
         the <head> of the server-rendered shell?"
    )]
    NoHeadMarker,
    /// Browser: a tag was hydrated outside any `MetaContext`.
    #[error(
        "a halyard_meta tag was hydrated without a MetaContext; call \
         provide_meta_context() at the root of the app"
    )]
    NoMetaContext,
    /// Browser: a DOM operation threw.
    #[error("{op} failed: {thrown}")]
    Dom {
        /// The operation, e.g. `document.createElement`.
        op: &'static str,
        /// The exception the browser threw.
        thrown: String,
    },
}

impl MetaError {
    /// A DOM operation `op` that threw `thrown`.
    pub(crate) fn thrown(op: &'static str, thrown: &JsValue) -> Self {
        let thrown = match thrown.dyn_ref::<web_sys::js_sys::Error>() {
            // `DOMException` is an `Error` too
            Some(error) => format!("{}: {}", error.name(), error.message()),
            None => thrown.as_string().unwrap_or_else(|| format!("{thrown:?}")),
        };
        Self::Dom { op, thrown }
    }

    /// Logs this error as a warning (`console.warn` in the browser, standard error on the
    /// server), with what was done instead (`recovery`).
    pub(crate) fn warn(&self, recovery: &str) {
        halyard::logging::warn!("[halyard] halyard_meta: {self}. {recovery}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The messages say what was missing and point at the fix.
    #[test]
    fn messages_name_what_is_missing() {
        assert_eq!(
            MetaError::NoTagInFirstChunk {
                tag: "body",
                component: "Body"
            }
            .to_string(),
            "the start of the page has no <body> tag for the attributes of \
             <Body/>"
        );
        assert!(MetaError::NoHeadMarker.to_string().contains("<MetaTags/>"));
        assert!(MetaError::NoMetaContext
            .to_string()
            .contains("provide_meta_context()"));
    }
}
