//! The axum integration renders every server page through `build_response`: the
//! app's HTML, then the `<script>` tags that carry the page's server data to the client.
//! These tests render through it as it does, including in situations that used to panic
//! (README, "Project policy": no panics, ever).

use crate::integration_utils::{
    build_response, BoxedFnOnce, PinnedFuture, PinnedStream,
};
use futures::{executor::block_on, stream::once, StreamExt};
use halyard::{nonce::provide_nonce, prelude::*, reactive::owner::Owner};

/// The app's view in these tests: a text node, so that what they check is only what
/// `build_response` adds around it.
const APP: &str = "the page";

/// The integrations' stream builders, reduced to what matters here: the app's HTML, then
/// the data scripts.
fn stream_builder<IV: IntoView + 'static>(
    app: IV,
    chunks: BoxedFnOnce<PinnedStream<String>>,
    _supports_ooo: bool,
) -> PinnedFuture<PinnedStream<String>> {
    Box::pin(async move {
        let html = app.to_html();
        Box::pin(once(async move { html }).chain(chunks()))
            as PinnedStream<String>
    })
}

/// Renders a page through `build_response`, as the integrations do, and returns the whole
/// response body.
fn render<IV: IntoView + 'static>(
    app_fn: impl FnOnce() -> IV + Send + 'static,
    additional_context: impl FnOnce() + Send + 'static,
) -> String {
    let (owner, stream) =
        build_response(app_fn, additional_context, stream_builder, false);
    let html = block_on(async move { stream.await.collect::<String>().await });
    owner.unset_with_forced_cleanup();
    html
}

/// Registers a resource's value with the current shared context, as a resource created
/// under the request's owner does. It resolves at once, so it is sent in the script that
/// follows the first one.
fn register_resource(value: &'static str) {
    let Some(shared_context) = Owner::current_shared_context() else {
        panic!("the app should start under the request's owner");
    };
    shared_context.write_async(
        shared_context.next_id(),
        Box::pin(async move { value.to_owned() }),
    );
}

/// The app's HTML, then its data: the resource registered by `register_resource("42")`
/// and the closing `__INCOMPLETE_CHUNKS` script.
fn assert_page_with_data(html: &str) {
    let app = html.find(APP);
    let scripts = html.find("<script");
    assert!(
        app.is_some() && scripts.is_some() && app < scripts,
        "expected the app's HTML followed by its data scripts, got: {html}"
    );
    assert!(
        html.contains("__PENDING_RESOURCES=[0,];")
            && html.contains("__RESOLVED_RESOURCES[0] = \"42\";"),
        "expected the resource's value in the data scripts, got: {html}"
    );
    assert!(
        html.ends_with("__INCOMPLETE_CHUNKS=[];</script>"),
        "expected the stream to end with the last data script, got: {html}"
    );
}

/// What the integrations stream for every page: the app's HTML, then its data in `<script>`
/// tags that carry the request's nonce.
#[test]
fn page_streams_its_html_then_its_data_scripts() {
    let html = render(
        || {
            register_resource("42");
            APP
        },
        provide_nonce,
    );

    assert_page_with_data(&html);
    assert!(
        html.contains("<script nonce=\""),
        "expected the data scripts to carry the nonce, got: {html}"
    );
}

/// The app can leave another reactive owner current when it returns, here a root owner
/// without a shared context. The data scripts looked the shared context up through the
/// current owner, found none and panicked, failing the request. The page's data was
/// registered with the request's own shared context, and is sent from there.
#[test]
fn page_streams_its_data_when_the_app_leaves_an_owner_without_shared_context_current(
) {
    let unrelated = Owner::new_root(None);
    let html = render(
        {
            let unrelated = unrelated.clone();
            move || {
                register_resource("42");
                unrelated.set();
                assert!(Owner::current_shared_context().is_none());
                APP
            }
        },
        || {},
    );

    assert_page_with_data(&html);
    drop(unrelated);
}

/// The same with an owner the app made current and then dropped: there is no current
/// owner left to ask, which panicked too.
#[test]
fn page_streams_its_data_when_the_app_leaves_a_dropped_owner_current() {
    let html = render(
        || {
            register_resource("42");
            let child = Owner::new();
            child.set();
            drop(child);
            assert!(Owner::current().is_none());
            APP
        },
        || {},
    );

    assert_page_with_data(&html);
}
