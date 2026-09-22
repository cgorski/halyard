#![deny(missing_docs)]

//! # About Halyard
//!
//! Halyard is a maintained fork of the [Leptos](https://github.com/leptos-rs/leptos) web
//! framework (MIT, © 2022 Greg Johnston; see the `NOTICE` file). It is a full-stack framework
//! for building web applications in Rust. You can use it to build
//! - single-page apps (SPAs) rendered entirely in the browser, using client-side routing and loading
//!   or mutating data via async requests to the server.
//! - multi-page apps (MPAs) rendered on the server, managing navigation, data, and mutations via
//!   web-standard `<a>` and `<form>` tags.
//! - progressively-enhanced single-page apps that are rendered on the server and then hydrated on the client,
//!   enhancing your `<a>` and `<form>` navigations and mutations seamlessly when WASM is available.
//!
//! And you can do all three of these **using the same Halyard code**.
//!
//! The API is Leptos 0.8's with the crates renamed (`leptos` → `halyard`, `leptos_router` →
//! `halyard_router`, ...; `LeptosOptions` → [`HalyardOptions`](halyard_config::HalyardOptions)),
//! so the upstream [Leptos Book](https://book.leptos.dev/) and
//! [examples](https://github.com/leptos-rs/leptos/tree/main/examples) apply. What the fork
//! changes is documented in the repository `README.md`; the user-visible parts are:
//!
//! - the WASM/JS file names are resolved at **runtime** from
//!   [`HalyardOptions`](halyard_config::HalyardOptions) (see
//!   [`HydrationScripts`](hydration::HydrationScripts) and the `wasm_file_name` option);
//! - every runtime setting is read from `HALYARD_*` with the legacy `LEPTOS_*` as a fallback;
//! - a hydration mismatch logs one detailed error and falls back to client-side rendering
//!   instead of panicking (see `mount::hydrate_body` and the `panic-on-hydration-mismatch`
//!   feature), and a server/client `--cfg erase_components` mismatch is detected up front
//!   (see [`hydration::RENDER_MODE`]).
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
//! - **Routing**: the [`halyard_router`](https://docs.rs/leptos_router/latest/leptos_router/) crate
//! - **Server Functions**: the [`server`](macro@halyard::prelude::server) macro and [`ServerAction`](halyard::prelude::ServerAction).
//!
//! # Feature Flags
//!
//! - **`nightly`**: On `nightly` Rust, enables the function-call syntax for signal getters and setters.
//!   Also enables some experimental optimizations that improve the handling of static strings and
//!   the performance of the `template! {}` macro.
//! - **`csr`** Client-side rendering: Generate DOM nodes in the browser.
//! - **`ssr`** Server-side rendering: Generate an HTML string (typically on the server).
//! - **`islands`** Activates “islands mode,” in which components are not made interactive on the
//!   client unless they use the `#[island]` macro.
//! - **`hydrate`** Hydration: use this to add interactivity to an SSRed Halyard app.
//! - **`nonce`** Adds support for nonces to be added as part of a Content Security Policy.
//! - **`rkyv`** In SSR/hydrate mode, enables using [`rkyv`](https://docs.rs/rkyv/latest/rkyv/) to serialize resources.
//! - **`tracing`** Adds support for [`tracing`](https://docs.rs/tracing/latest/tracing/).
//! - **`trace-component-props`** Adds `tracing` support for component props.
//! - **`delegation`** Uses event delegation rather than the browser’s native event handling
//!   system. (This improves the performance of creating large numbers of elements simultaneously,
//!   in exchange for occasional edge cases in which events behave differently from native browser
//!   events.)
//! - **`rustls`** Use `rustls` for server functions.
//!
//! **Important Note:** You must enable one of `csr`, `hydrate`, or `ssr` to tell Halyard
//! which mode your app is operating in. You should only enable one of these per build target,
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
//!     // note that `value` and `set_value` are `Copy`, so it's super easy to move them into closures
//!     let clear = move |_| set_value.set(0);
//!     let decrement = move |_| *set_value.write() -= 1;
//!     let increment = move |_| *set_value.write() += 1;
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
//! Halyard is easy to use with [Trunk](https://trunk-rs.github.io/trunk/) (or with a simple wasm-bindgen setup):
//!
//! ```rust
//! use halyard::{mount::mount_to_body, prelude::*};
//!
//! #[component]
//! fn SimpleCounter(initial_value: i32) -> impl IntoView {
//!     // ...
//!     # _ = initial_value;
//! }
//!
//! pub fn main() {
//! # if false { // can't run in doctest
//!     mount_to_body(|| view! { <SimpleCounter initial_value=3 /> })
//! # }
//! }
//! ```

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
        pub use crate::{
            callback::*, children::*, component::*, control_flow::*, error::*,
            form::*, hydration::*, into_view::*, mount::*, nonce::*,
            suspense::*, text_prop::*,
        };
        pub use halyard_config::*;
        pub use halyard_dom::helpers::*;
        pub use halyard_macro::*;
        pub use halyard_oco::*;
        pub use halyard_reactive_graph::{
            actions::*,
            computed::*,
            effect::*,
            graph::untrack,
            owner::*,
            signal::*,
            wrappers::{read::*, write::*},
        };
        pub use halyard_server::*;
        pub use halyard_server_fn::{
            self as server_fn,
            error::{FromServerFnError, ServerFnError, ServerFnErrorErr},
        };
        pub use halyard_tachys::{
            reactive_graph::{bind::BindAttribute, node_ref::*, Suspend},
            view::{fragment::Fragment, template::ViewTemplate},
        };
    }
    pub use export_types::*;
}

/// Components used for working with HTML forms, like `<ActionForm>`.
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
    pub use halyard_throw_error::*;
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
#[doc(inline)]
pub use halyard_server_fn as server_fn;

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
#[doc(inline)]
pub use halyard_dom;
pub use into_view::IntoView;
mod provider;
#[doc(inline)]
pub use halyard_tachys as tachys;
/// Tools to mount an application to the DOM, or to hydrate it from server-rendered HTML.
pub mod mount;
#[doc(inline)]
pub use halyard_config as config;
#[doc(inline)]
pub use halyard_oco as oco;
mod from_form_data;
#[doc(inline)]
pub use halyard_either_of as either;
#[doc(inline)]
pub use halyard_reactive_graph as reactive;

/// Provide and access data along the reactive graph, sharing data without directly passing arguments.
pub mod context {
    pub use crate::provider::*;
    pub use halyard_reactive_graph::owner::{provide_context, use_context};
}

#[doc(inline)]
pub use halyard_server as server;
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

#[cfg(feature = "subsecond")]
/// Utilities for using binary hot-patching with [`subsecond`].
pub mod subsecond;

/// Utilities for simple isomorphic logging to the console or terminal.
pub mod logging {
    pub use halyard_dom::{
        debug_error, debug_log, debug_warn, error, log, warn,
    };
}

/// Utilities for working with asynchronous tasks.
pub mod task {
    use halyard_any_spawner::Executor;
    use halyard_reactive_graph::computed::ScopedFuture;
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
    use halyard_reactive_graph::traits::WriteValue;

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
