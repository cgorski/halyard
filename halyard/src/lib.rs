#![deny(missing_docs)]

//! # About Halyard
//!
//! Halyard is a maintained fork of the [Leptos](https://github.com/leptos-rs/leptos) web
//! framework (MIT, © 2022 Greg Johnston; see the `NOTICE` file). It is a full-stack framework
//! for building web applications in Rust, in one way: pages are rendered on the server and
//! then hydrated in the browser with WebAssembly, which enhances `<a>` and `<form>`
//! navigations and mutations once the WASM has loaded. With islands, only the components
//! marked `#[island]` are hydrated.
//!
//! The API is Leptos 0.8's, gathered into one crate: `leptos` is `halyard`, and
//! `leptos_router`, `leptos_meta` and `leptos_axum` are the modules [`router`], [`meta`] and
//! `axum` (with the `axum` feature); `LeptosOptions` is
//! [`HalyardOptions`](halyard::config::HalyardOptions). So the upstream
//! [Leptos Book](https://book.leptos.dev/) and
//! [examples](https://github.com/leptos-rs/leptos/tree/main/examples) apply, with those paths.
//! What the fork changes is documented in the repository `README.md`; the user-visible parts
//! are:
//!
//! - the WASM/JS file names are resolved at **runtime** from
//!   [`HalyardOptions`](halyard::config::HalyardOptions) (see
//!   [`HydrationScripts`](hydration::HydrationScripts) and the `wasm_file_name` option);
//! - every runtime setting is read from `HALYARD_*` with the legacy `LEPTOS_*` as a fallback;
//! - a hydration mismatch logs one detailed error and falls back to client-side rendering
//!   instead of panicking (see `mount::hydrate_body`), and a server/client
//!   `--cfg erase_components` mismatch is detected up front (see [`hydration::RENDER_MODE`]).
//!
//! # Quick Links
//!
//! Here are links to the most important sections of the docs:
//! - **Reactivity**: the [`reactive`] overview, and more details in
//!   + signals: [`signal`](halyard::prelude::signal), [`ReadSignal`](halyard::prelude::ReadSignal),
//!     [`WriteSignal`](halyard::prelude::WriteSignal) and [`RwSignal`](halyard::prelude::RwSignal).
//!   + computations: [`Memo`](halyard::prelude::Memo).
//!   + `async` interop: [`Resource`](halyard::prelude::Resource) for loading data using `async` functions
//!     and [`Action`](halyard::prelude::Action) to mutate data or imperatively call `async` functions.
//!   + reactions: [`Effect`](halyard::prelude::Effect) and [`RenderEffect`](halyard::prelude::RenderEffect).
//! - **Templating/Views**: the [`view`] macro and [`IntoView`] trait.
//! - **Routing**: the [`router`] module (`<Router/>`, `<Routes/>`, `<Route/>`, `<A/>`, hooks).
//! - **The document's `<head>`**: the [`meta`] module (`<Title/>`, `<Meta/>`, `<Link/>`, ...).
//! - **Serving**: the `axum` module, with the `axum` feature (`HalyardRoutes`,
//!   `generate_route_list`, `file_and_error_handler`).
//!
//! [`prelude`] brings in what most components use, including the router's and the head's
//! everyday components and hooks.
//!
//! # Feature Flags
//!
//! - **`ssr`** Server-side rendering: Generate an HTML string (typically on the server). Also
//!   turns on the server side of the router and of the head's tags.
//! - **`axum`** The axum integration (`halyard::axum`): serves an application's pages on a
//!   native server running on Tokio. Implies `ssr` and `nonce`.
//! - **`islands`** Activates “islands mode,” in which components are not made interactive on the
//!   client unless they use the `#[island]` macro.
//! - **`islands-router`** Client-side navigation between pages in islands mode.
//! - **`hydrate`** Hydration: use this to add interactivity to an SSRed Halyard app.
//! - **`nonce`** Adds support for nonces to be added as part of a Content Security Policy. The
//!   server generates them; the browser build only reads the one on the page.
//! - **`tracing`** Adds support for [`tracing`](https://docs.rs/tracing/latest/tracing/).
//! - **`trace-component-props`** Adds `tracing` support for component props.
//! - **`delegation`** Uses event delegation rather than the browser’s native event handling
//!   system. (This improves the performance of creating large numbers of elements simultaneously,
//!   in exchange for occasional edge cases in which events behave differently from native browser
//!   events.)
//!
//! Resources send their data with the page as serde JSON (or, with `Resource::new_str`,
//! through `ToString` and `FromStr`). A resource whose source may have no value (for example
//! one that reads a weak handle with `?`) is made with `Resource::new_try` (and
//! `LocalResource::new_try`): while the source gives `None`, nothing is fetched and the
//! resource stays as it is, pending if it never loaded.
//!
//! **Important Note:** You must enable either `hydrate` or `ssr` (or `axum`) to tell Halyard
//! which build you are compiling. You should only enable one of these per build target,
//! i.e., you should not have both `hydrate` and `ssr` enabled for your server binary, only `ssr`.
//!
//! # A Simple Counter
//!
//! ```rust
//! use halyard::prelude::*;
//!
//! #[component]
//! pub fn SimpleCounter(initial_value: i32) -> impl IntoView {
//!     // create a reactive signal with the initial value
//!     let (value, set_value) = signal(initial_value);
//!
//!     // create event handlers for our buttons
//!     // `value` and `set_value` are `Copy` (weak) handles, so they move into closures for
//!     // free; writes through them always succeed (or do nothing once the component is gone),
//!     // and the view reads `value` directly
//!     let clear = move |_| set_value.set(0);
//!     let decrement = move |_| set_value.update(|value| *value -= 1);
//!     let increment = move |_| set_value.update(|value| *value += 1);
//!
//!     view! {
//!         <div>
//!             <button on:click=clear>"Clear"</button>
//!             <button on:click=decrement>"-1"</button>
//!             <span>"Value: " {value} "!"</span>
//!             <button on:click=increment>"+1"</button>
//!         </div>
//!     }
//! }
//! ```
//!
//! The server build (`ssr`) renders the page, usually through the axum integration
//! (`halyard::axum`, with the `axum` feature); the browser build (`hydrate`) hydrates it with
//! `halyard::mount::hydrate_body` (or `hydrate_lazy`, for lazy routes and components, or
//! `hydrate_islands` in islands mode).

extern crate self as halyard;

/// Exports all the core types of the library.
pub mod prelude {
    // Traits
    // These should always be exported from the prelude
    pub use halyard_reactive_graph::prelude::*;
    pub use halyard_tachys::prelude::*;

    // Structs
    // In the future, maybe we should remove this blanket export
    // However, it is definitely useful relative to looking up every struct etc.
    mod export_types {
        pub use crate::config::*;
        pub use crate::dom::helpers::*;
        pub use crate::server::*;
        pub use crate::{
            callback::*, children::*, component::*, control_flow::*, error::*,
            form::*, hydration::*, into_view::*, mount::*, nonce::*,
            suspense::*, text_prop::*,
        };
        pub use halyard_macro::*;
        pub use halyard_reactive_graph::{
            actions::*,
            computed::*,
            effect::*,
            graph::untrack,
            owner::*,
            signal::*,
            wrappers::{read::*, write::*},
        };
        pub use halyard_tachys::oco::*;
        pub use halyard_tachys::{
            reactive_graph::{bind::BindAttribute, node_ref::*, Suspend},
            view::{fragment::Fragment, template::ViewTemplate},
        };
        // the router's and the head's everyday items
        pub use crate::meta::{
            provide_meta_context, Body, HashedStylesheet, Html, Link, Meta,
            MetaTags, Script, Style, Stylesheet, Title,
        };
        pub use crate::router::{
            components::{
                FlatRoutes, Form, Outlet, ParentRoute, ProtectedParentRoute,
                ProtectedRoute, Redirect, Route, Router, Routes, A,
            },
            hooks::{
                use_location, use_navigate, use_params, use_params_map,
                use_query, use_query_map,
            },
            path, LazyRoute, NavigateOptions, OptionalParamSegment,
            ParamSegment, SsrMode, StaticSegment, WildcardSegment,
        };
    }
    pub use export_types::*;
}

/// Tools for working with HTML forms, like reading a submitted form into a Rust type.
pub mod form;

/// A standard way to wrap functions and closures to pass them to components.
pub use halyard_reactive_graph::callback;

/// Types that can be passed as the `children` prop of a component.
pub mod children;

/// Wrapper for intercepting component attributes.
pub mod attribute_interceptor;

#[doc(hidden)]
/// Traits used to implement component constructors.
pub mod component;
mod error_boundary;

/// Tools for handling errors.
pub mod error {
    pub use crate::error_boundary::*;
    pub use halyard_reactive_graph::throw_error::*;
}

/// Control-flow components like `<Show>`, `<For>`, and `<Await>`.
pub mod control_flow {
    pub use crate::{
        animated_show::*, await_::*, for_loop::*, show::*, show_let::*,
    };
}
mod animated_show;
mod await_;
mod for_loop;
mod show;
mod show_let;

/// A component that allows rendering a component somewhere else.
pub mod portal;

/// Components to enable server-side rendering and client-side hydration.
pub mod hydration;

/// Utilities for exporting nonces to be used for a Content Security Policy.
pub mod nonce;

/// Components to load asynchronous data.
pub mod suspense {
    pub use crate::{suspense_component::*, transition::*};
}

#[macro_use]
mod suspense_component;

/// Types for reactive string properties for components.
pub mod text_prop;
mod transition;
pub use halyard_macro::*;

/// Type-erase a reactive closure into a [`halyard_tachys::reactive_graph::SharedReactiveFunction`].
///
/// Helper used by the `view!` macro when compiled with `RUSTFLAGS="--cfg erase_components"`.
/// Wrapping `move || expr` in this helper makes every reactive child closure resolve
/// to the same type (`Arc<Mutex<dyn FnMut() -> T + Send>>`) instead of a unique
/// anonymous closure type per call site, so downstream `Render`/`Effect` machinery
/// monomorphizes per output type `T` rather than per closure `F`.
///
/// Trade-off: one `Arc<Mutex<_>>` allocation per closure + a vtable dispatch per render,
/// in exchange for substantially less monomorphization (smaller WASM, faster compile).
#[doc(hidden)]
#[cfg(erase_components)]
#[inline]
pub fn __as_shared_reactive_fn<T, F>(
    f: F,
) -> ::std::sync::Arc<::std::sync::Mutex<dyn FnMut() -> T + Send>>
where
    F: FnMut() -> T + Send + 'static,
    T: 'static,
{
    ::std::sync::Arc::new(::std::sync::Mutex::new(f))
}
#[doc(hidden)]
pub use typed_builder;
#[doc(hidden)]
pub use typed_builder_macro;
mod into_view;
pub use into_view::IntoView;
mod provider;
#[doc(inline)]
pub use halyard_tachys as tachys;
/// The axum integration: serves the routes of an application, renders its pages and
/// serves its static files.
#[cfg(feature = "axum")]
pub mod axum;
/// Runtime configuration: [`HalyardOptions`](config::HalyardOptions), read from
/// `HALYARD_*` variables and `Cargo.toml` metadata.
pub mod config;
/// Browser helpers: `window()`, `document()`, event listeners, timers.
pub mod dom;
#[cfg(feature = "axum")]
mod integration_utils;
/// Tags that belong in the document's `<head>` (`<Title/>`, `<Meta/>`, `<Link/>`,
/// `<Stylesheet/>`, ...), set from any component.
pub mod meta;
/// Tools to mount an application to the DOM, or to hydrate it from server-rendered HTML.
pub mod mount;
/// The router: `<Router/>`, `<Routes/>`, `<Route/>`, `<A/>`, `<Form/>`, route matching,
/// hooks such as `use_navigate` and `use_params`, and the `path!` macro.
pub mod router;
#[doc(inline)]
pub use halyard_tachys::oco;

#[doc(inline)]
pub use halyard_reactive_graph as reactive;
#[doc(inline)]
pub use halyard_tachys::either;

/// Provide and access data along the reactive graph, sharing data without directly passing arguments.
pub mod context {
    pub use crate::provider::*;
    pub use halyard_reactive_graph::owner::{provide_context, use_context};
}

/// Resources (`Resource`, `OnceResource`, `LocalResource`) and `SharedValue`: data loaded
/// on the server and sent to the browser with the page.
pub mod server;
/// HTML attribute types.
#[doc(inline)]
pub use halyard_tachys::html::attribute as attr;
/// HTML element types.
#[doc(inline)]
pub use halyard_tachys::html::element as html;
/// HTML event types.
#[doc(no_inline)]
pub use halyard_tachys::html::event as ev;
/// MathML element types.
#[doc(inline)]
pub use halyard_tachys::mathml as math;
/// SVG element types.
#[doc(inline)]
pub use halyard_tachys::svg;

pub mod logging;

/// Utilities for working with asynchronous tasks.
pub mod task {
    use halyard_reactive_graph::computed::ScopedFuture;
    use halyard_reactive_graph::executor::Executor;
    use std::future::Future;

    /// Spawns a thread-safe [`Future`].
    ///
    /// This will be run with the current reactive owner and observer using a [`ScopedFuture`].
    #[track_caller]
    #[inline(always)]
    pub fn spawn(fut: impl Future<Output = ()> + Send + 'static) {
        let fut = ScopedFuture::new(fut);

        #[cfg(not(target_family = "wasm"))]
        Executor::spawn(fut);

        #[cfg(target_family = "wasm")]
        Executor::spawn_local(fut);
    }

    /// Spawns a [`Future`] that cannot be sent across threads.
    #[track_caller]
    #[inline(always)]
    pub fn spawn_local(fut: impl Future<Output = ()> + 'static) {
        Executor::spawn_local(fut)
    }

    /// Waits until the next "tick" of the current async executor.
    pub async fn tick() {
        Executor::tick().await
    }

    pub use halyard_reactive_graph::{
        spawn_local_scoped, spawn_local_scoped_with_cancellation,
    };
}

// these reexports are used in islands
#[cfg(feature = "islands")]
#[doc(hidden)]
pub use serde;
#[doc(hidden)]
pub use serde_json;
#[cfg(feature = "tracing")]
#[doc(hidden)]
pub use tracing;
#[doc(hidden)]
pub use wasm_bindgen;
#[doc(hidden)]
pub use wasm_split_helpers as wasm_split;
#[doc(hidden)]
pub use web_sys;

#[doc(hidden)]
pub mod __reexports {
    pub use send_wrapper;
    pub use wasm_bindgen_futures;
}

#[doc(hidden)]
#[derive(Clone, Debug, Default)]
pub struct PrefetchLazyFn(
    pub  halyard_reactive_graph::owner::ArcStoredValue<
        std::collections::HashSet<&'static str>,
    >,
);

#[doc(hidden)]
pub fn prefetch_lazy_fn_on_server(id: &'static str) {
    use crate::context::use_context;
    use halyard_reactive_graph::traits::StrongWriteValue;

    if let Some(prefetches) = use_context::<PrefetchLazyFn>() {
        prefetches.0.write_value().insert(id);
    }
}

#[doc(hidden)]
#[derive(Clone, Debug, Default)]
pub struct WasmSplitManifest(
    pub  halyard_reactive_graph::owner::ArcStoredValue<(
        String,                                         // the pkg root
        std::collections::HashMap<String, Vec<String>>, // preloads
        String, // the name of the __wasm_split.js file
    )>,
);
