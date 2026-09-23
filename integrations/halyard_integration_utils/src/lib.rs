#![allow(clippy::type_complexity)]

use futures::{stream::once, Stream, StreamExt};
use halyard::{
    context::provide_context,
    nonce::use_nonce,
    prelude::ReadValue,
    reactive::owner::{Owner, Sandboxed},
    IntoView, PrefetchLazyFn, WasmSplitManifest,
};
use halyard_config::HalyardOptions;
use halyard_hydration_context::{SharedContext, SsrSharedContext};
use halyard_meta::{Link, ServerMetaContextOutput};
use std::{future::Future, pin::Pin, sync::Arc};

pub type PinnedStream<T> = Pin<Box<dyn Stream<Item = T> + Send>>;
pub type PinnedFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;
pub type BoxedFnOnce<T> = Box<dyn FnOnce() -> T + Send>;

pub trait ExtendResponse: Sized {
    type ResponseOptions: Send;

    fn from_stream(stream: impl Stream<Item = String> + Send + 'static)
        -> Self;

    fn extend_response(&mut self, opt: &Self::ResponseOptions);

    fn set_default_content_type(&mut self, content_type: &str);

    fn from_app<IV>(
        app_fn: impl FnOnce() -> IV + Send + 'static,
        meta_context: ServerMetaContextOutput,
        additional_context: impl FnOnce() + Send + 'static,
        res_options: Self::ResponseOptions,
        stream_builder: fn(
            IV,
            BoxedFnOnce<PinnedStream<String>>,
            bool,
        ) -> PinnedFuture<PinnedStream<String>>,
        supports_ooo: bool,
    ) -> impl Future<Output = Self> + Send
    where
        IV: IntoView + 'static,
    {
        async move {
            let prefetches = PrefetchLazyFn::default();

            let (owner, sc, stream) = build_response_with_shared_context(
                app_fn,
                additional_context,
                stream_builder,
                supports_ooo,
            );

            owner.with(|| provide_context(prefetches.clone()));

            let stream = stream.await.ready_chunks(32).map(|n| n.join(""));

            while let Some(pending) = sc.await_deferred() {
                pending.await;
            }

            if !prefetches.0.read_value().is_empty() {
                use halyard::prelude::*;

                let nonce =
                    use_nonce().map(|n| n.to_string()).unwrap_or_default();
                if let Some(manifest) = use_context::<WasmSplitManifest>() {
                    let (pkg_path, manifest, wasm_split_file) =
                        &*manifest.0.read_value();
                    let prefetches = prefetches.0.read_value();

                    let all_prefetches = prefetches.iter().flat_map(|key| {
                        manifest.get(*key).into_iter().flatten()
                    });

                    for module in all_prefetches {
                        // to_html() on halyard_meta components registers them with the meta context,
                        // rather than returning HTML directly
                        _ = view! {
                            <Link
                                rel="preload"
                                href=format!("{pkg_path}/{module}.wasm")
                                as_="fetch"
                                type_="application/wasm"
                                crossorigin=nonce.clone()
                            />
                        }
                        .to_html();
                    }
                    _ = view! {
                        <Link rel="modulepreload" href=format!("{pkg_path}/{wasm_split_file}") crossorigin=nonce/>
                    }
                    .to_html();
                }
            }

            let mut stream = Box::pin(
                meta_context.inject_meta_context(stream).await.then({
                    let sc = Arc::clone(&sc);
                    move |chunk| {
                        let sc = Arc::clone(&sc);
                        async move {
                            while let Some(pending) = sc.await_deferred() {
                                pending.await;
                            }
                            chunk
                        }
                    }
                }),
            );

            // wait for the first chunk of the stream, then set the status and headers
            let first_chunk = stream.next().await.unwrap_or_default();

            let mut res = Self::from_stream(Sandboxed::new(
                once(async move { first_chunk })
                    .chain(stream)
                    // drop the owner, cleaning up the reactive runtime,
                    // once the stream is over
                    .chain(once(async move {
                        owner.unset_with_forced_cleanup();
                        Default::default()
                    })),
            ));

            res.extend_response(&res_options);

            // Set the Content Type headers on all responses. This makes Firefox show the page source
            // without complaining
            res.set_default_content_type("text/html; charset=utf-8");

            res
        }
    }
}

pub fn build_response<IV>(
    app_fn: impl FnOnce() -> IV + Send + 'static,
    additional_context: impl FnOnce() + Send + 'static,
    stream_builder: fn(
        IV,
        BoxedFnOnce<PinnedStream<String>>,
        // this argument indicates whether a request wants to support out-of-order streaming
        // responses
        bool,
    ) -> PinnedFuture<PinnedStream<String>>,
    is_islands_router_navigation: bool,
) -> (Owner, PinnedFuture<PinnedStream<String>>)
where
    IV: IntoView + 'static,
{
    let (owner, _, stream) = build_response_with_shared_context(
        app_fn,
        additional_context,
        stream_builder,
        is_islands_router_navigation,
    );
    (owner, stream)
}

/// [`build_response`], also returning the request's shared context.
///
/// The root owner is created with this shared context and every owner beneath it shares
/// it, so the page's resources, errors and deferred futures are registered with it. Holding
/// it, rather than asking an owner for it, means it cannot be missing.
fn build_response_with_shared_context<IV>(
    app_fn: impl FnOnce() -> IV + Send + 'static,
    additional_context: impl FnOnce() + Send + 'static,
    stream_builder: fn(
        IV,
        BoxedFnOnce<PinnedStream<String>>,
        bool,
    ) -> PinnedFuture<PinnedStream<String>>,
    is_islands_router_navigation: bool,
) -> (
    Owner,
    Arc<dyn SharedContext + Send + Sync>,
    PinnedFuture<PinnedStream<String>>,
)
where
    IV: IntoView + 'static,
{
    let shared_context = Arc::new(SsrSharedContext::new())
        as Arc<dyn SharedContext + Send + Sync>;
    let owner = Owner::new_root(Some(Arc::clone(&shared_context)));
    let stream = Box::pin(Sandboxed::new({
        let owner = owner.clone();
        let shared_context = Arc::clone(&shared_context);
        async move {
            let stream = owner.with(|| {
                additional_context();

                // run app
                let app = app_fn();

                let nonce = use_nonce()
                    .as_ref()
                    .map(|nonce| format!(" nonce=\"{nonce}\""))
                    .unwrap_or_default();

                // the request's own shared context, not the current owner's: the app can
                // leave another owner current (one without a shared context, or one it has
                // dropped), but the page's data was registered with this one
                let chunks: BoxedFnOnce<PinnedStream<String>> =
                    Box::new(move || {
                        data_scripts(shared_context.pending_data(), nonce)
                    });

                // convert app to appropriate response type
                // and chain the app stream, followed by chunks
                // in theory, we could select here, and intersperse them
                // the problem is that during the DOM walk, that would be mean random <script> tags
                // interspersed where we expect other children
                //
                // we also don't actually start hydrating until after the whole stream is complete,
                // so it's not useful to send those scripts down earlier.
                stream_builder(app, chunks, is_islands_router_navigation)
            });

            stream.await
        }
    }));
    (owner, shared_context, stream)
}

/// The page's server data (resolved resources, errors, pending resources and incomplete
/// chunks) as `<script>` tags, streamed after the app's HTML.
///
/// Without pending data there is nothing to serialise: the page streams without these
/// scripts, and the client, finding no server data, loads the page's resources itself, as
/// in client-side rendering. The server's shared context always has pending data; only
/// another implementation of [`SharedContext`] can have none.
fn data_scripts(
    pending_data: Option<halyard_hydration_context::PinnedStream<String>>,
    nonce: String,
) -> PinnedStream<String> {
    let Some(pending_data) = pending_data else {
        halyard::logging::warn!(
            "[halyard] The shared context has no data to send with this page: it is \
             streamed without its data scripts, and the browser loads its resources \
             itself."
        );
        return Box::pin(futures::stream::empty());
    };
    Box::pin(
        pending_data
            .map(move |chunk| format!("<script{nonce}>{chunk}</script>")),
    )
}

pub fn static_file_path(options: &HalyardOptions, path: &str) -> String {
    let trimmed_path = path.trim_start_matches('/');
    let path = if trimmed_path.is_empty() {
        "index"
    } else {
        trimmed_path
    };
    format!("{}/{}.html", options.site_root, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;

    /// A [`SharedContext`] can answer `pending_data()` with `None`. That was
    /// `pending_data().unwrap()`: a panic that failed the request. Now there is nothing to
    /// serialise, and the page streams without data scripts.
    #[test]
    fn no_pending_data_streams_no_data_scripts() {
        let scripts =
            block_on(data_scripts(None, String::new()).collect::<Vec<_>>());

        assert!(scripts.is_empty(), "expected no scripts, got: {scripts:?}");
    }

    /// Each chunk of pending data becomes one `<script>` tag with the request's nonce.
    #[test]
    fn each_chunk_of_pending_data_becomes_a_script_with_the_nonce() {
        let pending_data = SsrSharedContext::new().pending_data();
        assert!(pending_data.is_some());

        let scripts = block_on(
            data_scripts(pending_data, " nonce=\"abc\"".to_owned())
                .collect::<Vec<_>>(),
        );

        assert_eq!(
            scripts,
            [
                "<script nonce=\"abc\">__RESOLVED_RESOURCES=[];\
                 __SERIALIZED_ERRORS=[];__PENDING_RESOURCES=[];\
                 __RESOURCE_RESOLVERS=[];</script>",
                "<script nonce=\"abc\">__INCOMPLETE_CHUNKS=[];</script>",
            ]
        );
    }
}
