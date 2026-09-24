//! Allows rendering user interfaces based on a statically-typed view tree.
//!
//! Views render to the DOM, render to HTML on the server, and hydrate server-rendered HTML.
//! Reactive values from `halyard_reactive_graph` (signals, memos, closures) are views and
//! attribute values that update themselves. The crate also holds the small building blocks
//! the view types are made of: [`either`], [`oco`] and [`next_tuple`].

#![deny(missing_docs)]

/// Commonly-used traits.
pub mod prelude {
    pub use crate::{
        html::{
            attribute::{
                any_attribute::IntoAnyAttribute,
                aria::AriaAttributes,
                custom::CustomAttribute,
                global::{
                    ClassAttribute, GlobalAttributes, GlobalOnAttributes,
                    OnAttribute, OnTargetAttribute, PropAttribute,
                    StyleAttribute,
                },
                IntoAttributeValue,
            },
            directive::DirectiveAttribute,
            element::{ElementChild, ElementExt, InnerHtmlAttribute},
            node_ref::NodeRefAttribute,
        },
        renderer::{dom::Dom, Renderer},
        view::{
            add_attr::AddAnyAttr,
            any_view::{AnyView, IntoAny, IntoMaybeErased},
            IntoRender, Mountable, Render, RenderHtml,
        },
    };
}

use wasm_bindgen::JsValue;
use web_sys::Node;

/// Helpers for interacting with the DOM.
pub mod dom;
mod dom_error;
/// Types for building a statically-typed HTML view tree.
pub mod html;
/// Supports adding interactivity to HTML.
pub mod hydration;
/// Types for MathML.
pub mod mathml;
/// Defines various backends that can render views.
pub mod renderer;
/// Rendering views to HTML.
pub mod ssr;
/// Types for SVG.
pub mod svg;
/// Core logic for manipulating views.
pub mod view;
mod view_error;

/// Enums of several possible types (`Either`, `EitherOf3`, ...), each of which renders the
/// variant it holds.
pub mod either;
#[cfg(feature = "islands")]
#[doc(hidden)]
pub use wasm_bindgen;
#[cfg(feature = "islands")]
#[doc(hidden)]
pub use web_sys;

/// [`Oco`](oco::Oco), a cheaply cloned string or slice ("owned or clone-on-write"), and
/// its views.
pub mod oco;
/// View implementations for `halyard_reactive_graph`'s signals, memos and other reactive
/// values.
pub mod reactive_graph;

/// A type-erased container.
pub mod erased;

/// Concatenates `&'static str` slices in `const` contexts (element templates).
#[doc(hidden)]
pub mod const_str_slice_concat;
/// Takes the next item onto a tuple (`(A, B)` becomes `(A, B, C)`), for building views and
/// attribute lists.
pub mod next_tuple;

pub(crate) trait UnwrapOrDebug {
    type Output;

    fn or_debug(self, el: &Node, label: &'static str);

    fn ok_or_debug(
        self,
        el: &Node,
        label: &'static str,
    ) -> Option<Self::Output>;
}

impl<T> UnwrapOrDebug for Result<T, JsValue> {
    type Output = T;

    #[track_caller]
    fn or_debug(self, el: &Node, name: &'static str) {
        #[cfg(any(debug_assertions, halyard_debuginfo))]
        {
            if let Err(err) = self {
                let location = std::panic::Location::caller();
                web_sys::console::warn_3(
                    &JsValue::from_str(&format!(
                        "[WARNING] Non-fatal error at {location}, while \
                         calling {name} on "
                    )),
                    el,
                    &err,
                );
            }
        }
        #[cfg(not(any(debug_assertions, halyard_debuginfo)))]
        {
            _ = self;
        }
    }

    #[track_caller]
    fn ok_or_debug(
        self,
        el: &Node,
        name: &'static str,
    ) -> Option<Self::Output> {
        #[cfg(any(debug_assertions, halyard_debuginfo))]
        {
            if let Err(err) = &self {
                let location = std::panic::Location::caller();
                web_sys::console::warn_3(
                    &JsValue::from_str(&format!(
                        "[WARNING] Non-fatal error at {location}, while \
                         calling {name} on "
                    )),
                    el,
                    err,
                );
            }
            self.ok()
        }
        #[cfg(not(any(debug_assertions, halyard_debuginfo)))]
        {
            self.ok()
        }
    }
}

#[doc(hidden)]
#[macro_export]
macro_rules! or_debug {
    ($action:expr, $el:expr, $label:literal) => {
        if cfg!(any(debug_assertions, halyard_debuginfo)) {
            $crate::UnwrapOrDebug::or_debug($action, $el, $label);
        } else {
            _ = $action;
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! ok_or_debug {
    ($action:expr, $el:expr, $label:literal) => {
        if cfg!(any(debug_assertions, halyard_debuginfo)) {
            $crate::UnwrapOrDebug::ok_or_debug($action, $el, $label)
        } else {
            $action.ok()
        }
    };
}
