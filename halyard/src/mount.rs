#[cfg(debug_assertions)]
use crate::logging;
use crate::IntoView;
use halyard_any_spawner::Executor;
use halyard_reactive_graph::owner::Owner;
use halyard_tachys::{
    dom::body,
    view::{Mountable, Render},
};
#[cfg(feature = "hydrate")]
use halyard_tachys::{
    hydration::Cursor,
    view::{PositionState, RenderHtml},
};
#[cfg(debug_assertions)]
use std::cell::Cell;
#[cfg(feature = "hydrate")]
use wasm_bindgen::JsCast;
use web_sys::HtmlElement;

#[cfg(feature = "hydrate")]
/// Hydrates the app described by the provided function, starting at `<body>`.
///
/// If the server-rendered DOM does not match the view (a *hydration mismatch*), one detailed
/// error is logged to the console, the partially hydrated state is discarded, and the app is
/// rendered on the client instead (which is why `f` must be `Fn`, not `FnOnce`). Enable the
/// `panic-on-hydration-mismatch` feature to panic instead, e.g. in tests.
///
/// If the page carries a `<meta name="halyard-render-mode">` tag (emitted by
/// [`HydrationScripts`](crate::hydration::HydrationScripts)) whose value differs from this
/// build's [`RENDER_MODE`](crate::hydration::RENDER_MODE), hydration is not attempted at all:
/// the server and client were compiled with different `--cfg erase_components` settings and
/// would disagree about every hydration marker. One console error explains how to fix it.
pub fn hydrate_body<F, N>(f: F)
where
    F: Fn() -> N + 'static,
    N: IntoView,
{
    if let Some(owner) = hydrate_from(body(), f) {
        owner.forget();
    }
}

#[cfg(feature = "hydrate")]
/// Hydrates the app described by the provided function, starting at `<body>`, with support
/// for lazy-loaded routes and components.
///
/// See [`hydrate_body`] for how hydration mismatches and render-mode mismatches are handled.
pub fn hydrate_lazy<F, N>(f: F)
where
    F: Fn() -> N + 'static,
    N: IntoView,
{
    // use wasm-bindgen-futures to drive the reactive system
    // we ignore the return value because an Err here just means the wasm-bindgen executor is
    // already initialized, which is not an issue
    _ = Executor::init_wasm_bindgen();

    crate::task::spawn_local(async move {
        if let Some(owner) = hydrate_from_async(body(), f).await {
            owner.forget();
        }
    })
}

/// Checks the `<meta name="halyard-render-mode">` tag written by `HydrationScripts` against
/// this build's mode. Returns `false` (after logging one error) on a mismatch.
///
/// The two modes (`--cfg erase_components` or not) produce different hydration marker
/// comments, so hydrating would fail at the first marker with a confusing message; this
/// names the actual problem instead.
#[cfg(feature = "hydrate")]
pub fn check_render_mode() -> bool {
    use crate::hydration::{RENDER_MODE, RENDER_MODE_META_NAME};
    use halyard_tachys::dom::document;

    let server_mode = document()
        .query_selector(&format!("meta[name=\"{RENDER_MODE_META_NAME}\"]"))
        .ok()
        .flatten()
        .and_then(|meta| meta.get_attribute("content"));
    match server_mode {
        // no tag: the page was not rendered with `HydrationScripts`, so there is nothing
        // to compare against
        None => true,
        Some(server_mode) if server_mode == RENDER_MODE => true,
        Some(server_mode) => {
            let flag = |mode: &str| {
                if mode == "erased" {
                    "with `RUSTFLAGS=\"--cfg erase_components\"`"
                } else {
                    "without `--cfg erase_components`"
                }
            };
            web_sys::console::error_1(&wasm_bindgen::JsValue::from_str(
                &format!(
                    "[halyard] Render-mode mismatch: the server that produced \
                     this page was built {} (mode `{server_mode}`), but this \
                     WASM bundle was built {} (mode `{RENDER_MODE}`). The two \
                     modes emit different hydration markers, so the \
                     server-rendered HTML cannot be hydrated by this bundle.\n\
                     \nFix: build the server and the client the same way. \
                     `cargo-halyard`/`cargo-leptos` add `--cfg \
                     erase_components` to *debug* builds it runs itself; a \
                     server binary built with plain `cargo build` does not \
                     get it. Either run both builds through the build tool, \
                     or set `RUSTFLAGS=\"--cfg erase_components\"` for the \
                     plain build too, or disable erasure in the build tool \
                     (`disable-erase-components = true`).",
                    flag(&server_mode),
                    flag(RENDER_MODE),
                ),
            ));
            false
        }
    }
}

/// Used by `#[island]` when hydrating a single island failed: replaces the island element's
/// server-rendered children with a client-side render of `view`, under the current owner.
#[doc(hidden)]
#[cfg(feature = "hydrate")]
pub fn __island_client_render_fallback<N>(el: &HtmlElement, view: N)
where
    N: IntoView,
{
    crate::logging::warn!(
        "[halyard] Rendering island <{}> on the client because hydration \
         failed (see the error above).",
        el.get_attribute("data-component").unwrap_or_default()
    );
    el.set_inner_html("");
    let mut state = view.into_view().build();
    state.mount(el, None);
    // islands leak their state on purpose (see `#[island]`)
    std::mem::forget(state);
}

/// Replaces the children of `parent` with a client-side render of `f`, after hydration of
/// the server-rendered DOM failed. Server-serialized resource data is still used, so
/// resources do not refetch.
#[cfg(feature = "hydrate")]
fn client_render_fallback<F, N>(
    parent: HtmlElement,
    f: F,
) -> UnmountHandle<N::State>
where
    F: FnOnce() -> N + 'static,
    N: IntoView,
{
    use halyard_hydration_context::{HydrateSharedContext, SharedContext};
    use std::sync::Arc;

    crate::logging::warn!(
        "[halyard] Rendering the application on the client because hydration \
         failed (see the error above)."
    );
    parent.set_inner_html("");

    let sc = HydrateSharedContext::new();
    sc.set_is_hydrating(false);
    let owner = Owner::new_root(Some(Arc::new(sc)));
    let mountable = owner.with(move || {
        let view = f().into_view();
        let mut mountable = view.build();
        mountable.mount(&parent, None);
        mountable
    });
    if let Some(sc) = Owner::current_shared_context() {
        sc.hydration_complete();
    }
    UnmountHandle { owner, mountable }
}

#[cfg(debug_assertions)]
thread_local! {
    static FIRST_CALL: Cell<bool> = const { Cell::new(true) };
}

#[cfg(feature = "hydrate")]
/// Runs the provided closure and hydrates the result against the provided element.
///
/// Returns `None` if the page's render mode does not match this build (see
/// [`check_render_mode`]); in that case nothing is hydrated or mounted. On a hydration
/// mismatch the app is client-rendered into `parent` instead (see [`hydrate_body`]).
pub fn hydrate_from<F, N>(
    parent: HtmlElement,
    f: F,
) -> Option<UnmountHandle<N::State>>
where
    F: Fn() -> N + 'static,
    N: IntoView,
{
    use halyard_hydration_context::HydrateSharedContext;
    use halyard_tachys::hydration::take_hydration_failure;
    use std::sync::Arc;

    // use wasm-bindgen-futures to drive the reactive system
    // we ignore the return value because an Err here just means the wasm-bindgen executor is
    // already initialized, which is not an issue
    _ = Executor::init_wasm_bindgen();

    if !check_render_mode() {
        return None;
    }
    // start from a clean slate, in case an earlier hydration pass failed
    take_hydration_failure();

    #[cfg(debug_assertions)]
    {
        if !cfg!(feature = "hydrate") && FIRST_CALL.get() {
            logging::warn!(
                "It seems like you're trying to use Halyard in hydration mode, \
                 but the `hydrate` feature is not enabled on the `halyard` \
                 crate. Add `features = [\"hydrate\"]` to your Cargo.toml for \
                 the crate to work properly.\n\nNote that hydration and \
                 client-side rendering now use separate functions from \
                 halyard::mount: you are calling a hydration function."
            );
        }
        FIRST_CALL.set(false);
    }

    // create a new reactive owner and use it as the root node to run the app
    let owner = Owner::new_root(Some(Arc::new(HydrateSharedContext::new())));
    let mountable = owner.with(|| {
        let view = f().into_view();
        view.hydrate::<true>(
            &Cursor::new(parent.clone().unchecked_into()),
            &PositionState::default(),
        )
    });

    if take_hydration_failure() {
        // drop the half-hydrated state and its reactive owner, then render on the client
        drop(UnmountHandle { owner, mountable });
        return Some(client_render_fallback(parent, f));
    }

    if let Some(sc) = Owner::current_shared_context() {
        sc.hydration_complete();
    }

    // returns a handle that owns the owner
    // when this is dropped, it will clean up the reactive system and unmount the view
    Some(UnmountHandle { owner, mountable })
}

#[cfg(feature = "hydrate")]
/// Runs the provided closure and hydrates the result against the provided element, with
/// support for lazy-loaded routes and components.
///
/// See [`hydrate_from`] for the meaning of `None` and for mismatch handling.
pub async fn hydrate_from_async<F, N>(
    parent: HtmlElement,
    f: F,
) -> Option<UnmountHandle<N::State>>
where
    F: Fn() -> N + 'static,
    N: IntoView,
{
    use halyard_hydration_context::HydrateSharedContext;
    use halyard_tachys::hydration::take_hydration_failure;
    use std::sync::Arc;

    // use wasm-bindgen-futures to drive the reactive system
    // we ignore the return value because an Err here just means the wasm-bindgen executor is
    // already initialized, which is not an issue
    _ = Executor::init_wasm_bindgen();

    if !check_render_mode() {
        return None;
    }
    take_hydration_failure();

    #[cfg(debug_assertions)]
    {
        if !cfg!(feature = "hydrate") && FIRST_CALL.get() {
            logging::warn!(
                "It seems like you're trying to use Halyard in hydration mode, \
                 but the `hydrate` feature is not enabled on the `halyard` \
                 crate. Add `features = [\"hydrate\"]` to your Cargo.toml for \
                 the crate to work properly.\n\nNote that hydration and \
                 client-side rendering now use separate functions from \
                 halyard::mount: you are calling a hydration function."
            );
        }
        FIRST_CALL.set(false);
    }

    // create a new reactive owner and use it as the root node to run the app
    let owner = Owner::new_root(Some(Arc::new(HydrateSharedContext::new())));
    let mountable = owner
        .with(|| {
            use halyard_reactive_graph::computed::ScopedFuture;

            let f = &f;
            let cursor = Cursor::new(parent.clone().unchecked_into());
            ScopedFuture::new(async move {
                let view = f().into_view();
                view.hydrate_async(&cursor, &PositionState::default()).await
            })
        })
        .await;

    if take_hydration_failure() {
        drop(UnmountHandle { owner, mountable });
        return Some(client_render_fallback(parent, f));
    }

    if let Some(sc) = Owner::current_shared_context() {
        sc.hydration_complete();
    }

    // returns a handle that owns the owner
    // when this is dropped, it will clean up the reactive system and unmount the view
    Some(UnmountHandle { owner, mountable })
}

/// Runs the provided closure and mounts the result to the `<body>`.
pub fn mount_to_body<F, N>(f: F)
where
    F: FnOnce() -> N + 'static,
    N: IntoView,
{
    let owner = mount_to(body(), f);
    owner.forget();
}

/// Runs the provided closure and mounts the result to the provided element.
pub fn mount_to<F, N>(parent: HtmlElement, f: F) -> UnmountHandle<N::State>
where
    F: FnOnce() -> N + 'static,
    N: IntoView,
{
    // use wasm-bindgen-futures to drive the reactive system
    // we ignore the return value because an Err here just means the wasm-bindgen executor is
    // already initialized, which is not an issue
    _ = Executor::init_wasm_bindgen();

    #[cfg(debug_assertions)]
    {
        if !cfg!(feature = "csr") && FIRST_CALL.get() {
            logging::warn!(
                "It seems like you're trying to use Halyard in client-side \
                 rendering mode, but the `csr` feature is not enabled on the \
                 `halyard` crate. Add `features = [\"csr\"]` to your \
                 Cargo.toml for the crate to work properly.\n\nNote that \
                 hydration and client-side rendering now use different \
                 functions from halyard::mount. You are using a client-side \
                 rendering mount function."
            );
        }
        FIRST_CALL.set(false);
    }

    // create a new reactive owner and use it as the root node to run the app
    let owner = Owner::new();
    let mountable = owner.with(move || {
        let view = f().into_view();
        let mut mountable = view.build();
        mountable.mount(&parent, None);
        mountable
    });

    // returns a handle that owns the owner
    // when this is dropped, it will clean up the reactive system and unmount the view
    UnmountHandle { owner, mountable }
}

/// Runs the provided closure and mounts the result to the provided element.
pub fn mount_to_renderer<F, N>(
    parent: &halyard_tachys::renderer::types::Element,
    f: F,
) -> UnmountHandle<N::State>
where
    F: FnOnce() -> N + 'static,
    N: Render,
{
    // use wasm-bindgen-futures to drive the reactive system
    // we ignore the return value because an Err here just means the wasm-bindgen executor is
    // already initialized, which is not an issue
    _ = Executor::init_wasm_bindgen();

    // create a new reactive owner and use it as the root node to run the app
    let owner = Owner::new();
    let mountable = owner.with(move || {
        let view = f();
        let mut mountable = view.build();
        mountable.mount(parent, None);
        mountable
    });

    // returns a handle that owns the owner
    // when this is dropped, it will clean up the reactive system and unmount the view
    UnmountHandle { owner, mountable }
}

/// Hydrates any islands that are currently present on the page.
#[cfg(feature = "hydrate")]
pub fn hydrate_islands() {
    use halyard_hydration_context::{HydrateSharedContext, SharedContext};
    use std::sync::Arc;

    // use wasm-bindgen-futures to drive the reactive system
    // we ignore the return value because an Err here just means the wasm-bindgen executor is
    // already initialized, which is not an issue
    _ = Executor::init_wasm_bindgen();

    #[cfg(debug_assertions)]
    FIRST_CALL.set(false);

    // logs the render-mode mismatch (if any) once, up front; each island then hits the
    // regular hydration-mismatch path and is rendered on the client instead
    _ = check_render_mode();

    // create a new reactive owner and use it as the root node to run the app
    let sc = HydrateSharedContext::new();
    sc.set_is_hydrating(false); // islands mode starts in "not hydrating"
    let owner = Owner::new_root(Some(Arc::new(sc)));
    owner.set();
    std::mem::forget(owner);
}

/// On drop, this will clean up the reactive [`Owner`] and unmount the view created by
/// [`mount_to`].
///
/// If you are using it to create the root of an application, you should use
/// [`UnmountHandle::forget`] to leak it.
#[must_use = "Dropping an `UnmountHandle` will unmount the view and cancel the \
              reactive system. You should either call `.forget()` to keep the \
              view permanently mounted, or store the `UnmountHandle` somewhere \
              and drop it when you'd like to unmount the view."]
pub struct UnmountHandle<M>
where
    M: Mountable,
{
    #[allow(dead_code)]
    owner: Owner,
    mountable: M,
}

impl<M> UnmountHandle<M>
where
    M: Mountable,
{
    /// Leaks the handle, preventing the reactive system from being cleaned up and the view from
    /// being unmounted. This should always be called when [`mount_to`] is used for the root of an
    /// application that should live for the long term.
    pub fn forget(self) {
        std::mem::forget(self);
    }
}

impl<M> Drop for UnmountHandle<M>
where
    M: Mountable,
{
    fn drop(&mut self) {
        self.mountable.unmount();
    }
}
