//! Guards the `--cfg erase_components` render-mode fingerprint.
//!
//! Compiling with `RUSTFLAGS="--cfg erase_components"` (the default for debug builds run
//! through `cargo-halyard`/`cargo-leptos`) changes the *hydration marker comments* that
//! server-side rendering emits: every element's children become a `StaticVec<AnyView>`
//! (trailing `<!>`), every component's output becomes an `AnyView` (`<!--<() />-->` after
//! DOM-less views such as `<Title>`), and so on. A client compiled in the other mode
//! expects different markers and hydration fails at the first one.
//!
//! Halyard does not try to make the two layouts identical (that would either bloat the
//! HTML of release builds with debug-mode markers or change the DOM-anchoring semantics
//! erased mode relies on). Instead:
//!
//! * `HydrationScripts` emits `<meta name="halyard-render-mode" content="typed|erased">`
//!   describing the *server's* mode ([`halyard::hydration::RENDER_MODE`]);
//! * `hydrate_body` & friends compare it with the *client's* mode before touching the DOM
//!   and, on a mismatch, log one clear error naming both modes and how to fix it, then skip
//!   hydration instead of panicking (`halyard::mount::check_render_mode`).
//!
//! Run this test twice to see the fingerprint flip:
//!
//! ```text
//! cargo test -p halyard --features ssr --test render_mode
//! RUSTFLAGS="--cfg erase_components" cargo test -p halyard --features ssr --test render_mode
//! ```
//!
//! The second half of the file pins down the exact marker layout of a fragment containing
//! `<Title>`, an `Option` view and a `Suspense` fallback in *both* modes, so that any future
//! change to either layout (e.g. after merging upstream) is noticed.

#![cfg(feature = "ssr")]

use halyard::meta::{ServerMetaContext, Title};
use halyard::{
    hydration::{HydrationScripts, RENDER_MODE, RENDER_MODE_META_NAME},
    prelude::*,
};

const ERASED: bool = cfg!(erase_components);

#[test]
fn render_mode_constant_reflects_cfg() {
    assert_eq!(RENDER_MODE, if ERASED { "erased" } else { "typed" });
}

#[test]
fn hydration_scripts_emit_render_mode_fingerprint() {
    let options = HalyardOptions::builder().output_name("app").build();
    let html = Owner::new()
        .with(|| view! { <HydrationScripts options=options/> }.to_html());

    // the fingerprint the client checks before hydrating
    let meta = format!(
        "<meta name=\"{RENDER_MODE_META_NAME}\" content=\"{RENDER_MODE}\">"
    );
    assert!(
        html.contains(&meta),
        "expected the render-mode fingerprint {meta:?} in {html:?}"
    );

    // defect 1: the WASM file name comes from `HalyardOptions` at runtime, never from a
    // compile-time environment variable, so it is `<output_name>.wasm`...
    assert!(html.contains("\"app\", \"app\");"), "{html}");
    assert!(!html.contains("app_bg"), "{html}");

    // defect 5: no `<link rel="preload" as="fetch">` for the WASM (WebKit does not match
    // it, so it caused a second download); the bootstrap fetches the binary itself
    let (tags, script) = html.split_once("<script").expect("a <script> tag");
    assert!(!tags.contains("rel=\"preload\""), "{tags}");
    assert!(tags.contains("rel=\"modulepreload\""), "{tags}");
    assert!(script.contains("fetch(wasm_url)"), "{script}");

    // defect 4: rejections (e.g. navigation cancelling the fetch) are caught
    assert!(html.contains(".catch("), "{html}");
}

#[test]
fn hydration_scripts_honour_explicit_wasm_file_name() {
    let options = HalyardOptions::builder()
        .output_name("app")
        .wasm_file_name("app_bg")
        .build();
    let html = Owner::new()
        .with(|| view! { <HydrationScripts options=options/> }.to_html());
    assert!(html.contains("\"app\", \"app_bg\");"), "{html}");
}

/// Renders a fragment containing `<Title>`, an `Option` view and a `Suspense` fallback and
/// returns the body HTML plus the `<head>` elements the meta context collected.
fn render_fixture(show_optional: bool) -> String {
    #[component]
    fn Fixture(show_optional: bool) -> impl IntoView {
        let optional = show_optional.then(|| view! { <em>"maybe"</em> });
        view! {
            <Title text="fixture"/>
            <p>"static"</p>
            {optional}
            <Suspense fallback=|| "loading…">
                <span>"never resolves in this test"</span>
            </Suspense>
        }
    }

    let (meta_context, _meta_output) = ServerMetaContext::new();
    Owner::new().with(|| {
        provide_context(meta_context);
        view! { <Fixture show_optional=show_optional/> }.to_html()
    })
}

#[test]
fn fixture_marker_layout_matches_render_mode() {
    // NB: these two layouts differ in exactly the ways the module docs describe; that
    // difference is the reason the render-mode fingerprint exists.
    let typed_some = "<p>static</p><em>maybe</em>loading…";
    let typed_none = "<p>static</p><!>loading…";
    // erased: `<Fixture>` is an `AnyView` around a `StaticVec<AnyView>`; `<Title>` is
    // DOM-less so its `AnyView` leaves a `<!--<() />-->` anchor, the vector ends with a
    // `<!>`, and *every element's children* are a `StaticVec<AnyView>` too, hence the
    // `<!>` before each closing tag
    let erased_some =
        "<!--<() />--><p>static<!></p><em>maybe<!></em>loading…<!>";
    let erased_none = "<!--<() />--><p>static<!></p><!>loading…<!>";

    let (expected_some, expected_none) = if ERASED {
        (erased_some, erased_none)
    } else {
        (typed_some, typed_none)
    };
    let (other_some, other_none) = if ERASED {
        (typed_some, typed_none)
    } else {
        (erased_some, erased_none)
    };

    let some = render_fixture(true);
    let none = render_fixture(false);
    assert_eq!(some, expected_some, "Option = Some, mode = {RENDER_MODE}");
    assert_eq!(none, expected_none, "Option = None, mode = {RENDER_MODE}");

    // ...and the other mode's layout is *not* what we produce, i.e. the two modes really
    // are incompatible and a client in the other mode would mis-hydrate this fragment
    assert_ne!(some, other_some);
    assert_ne!(none, other_none);
}
