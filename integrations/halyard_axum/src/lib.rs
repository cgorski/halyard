#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![allow(clippy::type_complexity)]

//! Provides functions to easily integrate Halyard with Axum.
//!
//! ## JS Fetch Integration
//! The `halyard_axum` integration supports running in JavaScript-hosted WebAssembly
//! runtimes, e.g., running inside Deno, Cloudflare Workers, or other JS environments.
//! To run in this environment, you need to disable the default feature set and enable
//! the `wasm` feature on `halyard_axum` in your `Cargo.toml`.
//! ```toml
//! halyard_axum = { version = "0.6.0", default-features = false, features = ["wasm"] }
//! ```
//!
//! ## Features
//! - `default`: supports running in a typical native Tokio/Axum environment
//! - `wasm`: with `default-features = false`, supports running in a JS Fetch-based
//!   environment
//!
//! ### Important Note
//! Prior to 0.5, using `default-features = false` on `halyard_axum` simply did nothing. Now, it actively
//! disables features necessary to support the normal native/Tokio runtime environment we create. This can
//! generate errors like the following, which don’t point to an obvious culprit:
//! `
//! `spawn_local` called from outside of a `task::LocalSet`
//! `
//! If you are not using the `wasm` feature, do not set `default-features = false` on this package.
//!
//!
//! ## More information
//!
//! For more details on how to use the integrations, see the
//! [`examples`](https://github.com/leptos-rs/leptos/tree/main/examples)
//! directory in the Halyard repository.

#[cfg(feature = "default")]
use axum::http::Uri;
use axum::{
    body::{Body, Bytes},
    extract::{FromRef, MatchedPath, State},
    http::{
        header::{self, HeaderName, HeaderValue, ACCEPT},
        request::Parts,
        HeaderMap, Method, Request, Response, StatusCode,
    },
    response::IntoResponse,
    routing::{on, MethodFilter, MethodRouter},
};
#[cfg(not(feature = "default"))]
use error::RouteError;
use error::{error_response, report, request_id, RequestError};
use futures::{stream::once, Future, Stream, StreamExt};
#[cfg(feature = "default")]
use halyard::reactive::computed::ScopedFuture;
use halyard::{
    config::HalyardOptions,
    context::{provide_context, use_context},
    prelude::*,
    reactive::owner::Owner,
    IntoView,
};
use halyard_hydration_context::SsrSharedContext;
use halyard_integration_utils::{
    BoxedFnOnce, ExtendResponse, PinnedFuture, PinnedStream,
};
use halyard_meta::ServerMetaContext;
use halyard_or_poisoned::OrPoisoned;
#[cfg(feature = "default")]
use halyard_router::static_routes::ResolvedStaticPath;
use halyard_router::{
    components::provide_server_redirect, location::RequestUrl,
    static_routes::RegenerationFn, ExpandOptionals, PathSegment, RouteList,
    RouteListing, SsrMode,
};

use route_path::RouteRegistry;
#[cfg(feature = "default")]
use std::sync::LazyLock;
#[cfg(feature = "default")]
use std::{collections::HashMap, path::Path};
use std::{
    fmt::Debug,
    future::ready,
    io,
    pin::Pin,
    sync::{Arc, RwLock},
};
#[cfg(feature = "default")]
use tower::util::ServiceExt;
#[cfg(feature = "default")]
use tower_http::services::ServeDir;
// use tracing::Instrument; // TODO check tracing span -- was this used in 0.6 for a missing link?

mod error;
mod route_path;
#[cfg(feature = "default")]
mod service;
#[cfg(feature = "default")]
pub use service::ErrorHandler;
#[cfg(test)]
mod tests;

/// This struct lets you define headers and override the status of the Response from a page
/// rendered on the server. Typically contained inside of a ResponseOptions. Setting this is useful for cookies and custom responses.
#[derive(Debug, Clone, Default)]
pub struct ResponseParts {
    /// If provided, this will overwrite any other status code for this response.
    pub status: Option<StatusCode>,
    /// The map of headers that should be added to the response.
    pub headers: HeaderMap,
}

impl ResponseParts {
    /// Insert a header, overwriting any previous value with the same key
    pub fn insert_header(&mut self, key: HeaderName, value: HeaderValue) {
        self.headers.insert(key, value);
    }
    /// Append a header, leaving any header with the same key intact
    pub fn append_header(&mut self, key: HeaderName, value: HeaderValue) {
        self.headers.append(key, value);
    }
}

/// Allows you to override details of the HTTP response like the status code and add Headers/Cookies.
///
/// `ResponseOptions` is provided via context when you use most of the handlers provided in this
/// crate, including [`.halyard_routes`](HalyardRoutes::halyard_routes),
/// [`.halyard_routes_with_context`](HalyardRoutes::halyard_routes_with_context), etc.
/// You can find the full set of provided context types in each handler function.
///
/// If you provide your own handler, you will need to provide `ResponseOptions` via context
/// yourself if you want to access it via context.
/// ```
/// use axum::http::StatusCode;
/// use halyard::prelude::*;
/// use halyard_axum::ResponseOptions;
///
/// #[component]
/// pub fn NotFound() -> impl IntoView {
///     // provided while the page is rendered on the server
///     if let Some(response) = use_context::<ResponseOptions>() {
///         response.set_status(StatusCode::NOT_FOUND);
///     }
///     view! { <h1>"Not found"</h1> }
/// }
/// ```
#[derive(Debug, Clone, Default)]
pub struct ResponseOptions(pub Arc<RwLock<ResponseParts>>);

impl ResponseOptions {
    /// A simpler way to overwrite the contents of `ResponseOptions` with a new `ResponseParts`.
    pub fn overwrite(&self, parts: ResponseParts) {
        let mut writable = self.0.write().or_poisoned();
        *writable = parts
    }
    /// Set the status of the returned Response.
    pub fn set_status(&self, status: StatusCode) {
        let mut writeable = self.0.write().or_poisoned();
        let res_parts = &mut *writeable;
        res_parts.status = Some(status);
    }
    /// Insert a header, overwriting any previous value with the same key.
    pub fn insert_header(&self, key: HeaderName, value: HeaderValue) {
        let mut writeable = self.0.write().or_poisoned();
        let res_parts = &mut *writeable;
        res_parts.headers.insert(key, value);
    }
    /// Append a header, leaving any header with the same key intact.
    pub fn append_header(&self, key: HeaderName, value: HeaderValue) {
        let mut writeable = self.0.write().or_poisoned();
        let res_parts = &mut *writeable;
        res_parts.headers.append(key, value);
    }
}

struct AxumResponse(Response<Body>);

impl ExtendResponse for AxumResponse {
    type ResponseOptions = ResponseOptions;

    fn from_stream(
        stream: impl Stream<Item = String> + Send + 'static,
    ) -> Self {
        AxumResponse(
            Body::from_stream(
                stream.map(|chunk| Ok(chunk) as Result<String, std::io::Error>),
            )
            .into_response(),
        )
    }

    fn extend_response(&mut self, res_options: &Self::ResponseOptions) {
        let mut res_options = res_options.0.write().or_poisoned();
        if let Some(status) = res_options.status {
            *self.0.status_mut() = status;
        }
        self.0
            .headers_mut()
            .extend(std::mem::take(&mut res_options.headers));
    }

    fn set_default_content_type(&mut self, content_type: &str) {
        let headers = self.0.headers_mut();
        if !headers.contains_key(header::CONTENT_TYPE) {
            // Set the Content Type headers on all responses. This makes Firefox show the page source
            // without complaining
            match HeaderValue::from_str(content_type) {
                Ok(value) => {
                    headers.insert(header::CONTENT_TYPE, value);
                }
                Err(_) => report(
                    &RequestError::InvalidContentType {
                        content_type: content_type.to_owned(),
                    },
                    None,
                ),
            }
        }
    }
}

/// Redirects the browser away from a page that is being rendered on the server.
///
/// The route handlers of this crate provide it to `halyard_router`, whose `<Redirect/>` calls
/// it during server rendering; application code can call it while a page renders too, for
/// example from a [blocking resource](halyard::server::Resource::new_blocking).
///
/// Using it with a non-blocking [`Resource`] will not work if you are using streaming rendering,
/// as the response's headers will already have been sent by the time it is called.
///
/// ### Implementation
///
/// This sets the `Location` header to the URL given.
///
/// If the page is being requested by an ordinary `GET` request or an HTML `<form>` without
/// any enhancement, it also sets a status code of `302` for a temporary redirect. (This is
/// determined by whether the `Accept` header contains `text/html` as it does for an ordinary
/// navigation.) Otherwise the status is left as it is.
///
/// A `path` that cannot be a header value (it contains a control character such as CR or
/// LF, which would start another header) is refused: the response gets no `Location` and
/// the status 500, and the refusal is logged with the request's `x-request-id`.
pub fn redirect(path: &str) {
    if let (Some(req), Some(res)) =
        (use_context::<Parts>(), use_context::<ResponseOptions>())
    {
        let Ok(location) = HeaderValue::from_str(path) else {
            report(
                &RequestError::InvalidRedirect {
                    location: path.to_owned(),
                },
                request_id(&req.headers),
            );
            res.set_status(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        };
        // insert the Location header in any case
        res.insert_header(header::LOCATION, location);

        let accepts_html = req
            .headers
            .get(ACCEPT)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.contains("text/html"))
            .unwrap_or(false);
        if accepts_html {
            // if the request accepts text/html, it's a navigation or a plain form
            // request and needs to have the 302 code set
            res.set_status(StatusCode::FOUND);
        }
    } else {
        #[cfg(feature = "tracing")]
        {
            tracing::warn!(
                "Couldn't retrieve either Parts or ResponseOptions while \
                 trying to redirect()."
            );
        }
        #[cfg(not(feature = "tracing"))]
        {
            eprintln!(
                "Couldn't retrieve either Parts or ResponseOptions while \
                 trying to redirect()."
            );
        }
    }
}

/// Decomposes an HTTP request into its parts, allowing you to read its headers
/// and other data without consuming the body. Creates a new Request from the
/// original parts for further processing
pub fn generate_request_and_parts(
    req: Request<Body>,
) -> (Request<Body>, Parts) {
    let (parts, body) = req.into_parts();
    let parts2 = parts.clone();
    (Request::from_parts(parts, body), parts2)
}

fn init_executor() {
    #[cfg(feature = "wasm")]
    let _ = halyard_any_spawner::Executor::init_wasm_bindgen();
    #[cfg(all(not(feature = "wasm"), feature = "default"))]
    let _ = halyard_any_spawner::Executor::init_tokio();
    #[cfg(all(not(feature = "wasm"), not(feature = "default")))]
    {
        eprintln!(
            "It appears you have set 'default-features = false' on \
             'halyard_axum', but are not using the 'wasm' feature. Either \
             remove 'default-features = false' or, if you are running in a \
             JS-hosted WASM server environment, add the 'wasm' feature."
        );
    }
}

/// A stream of bytes of HTML.
pub type PinnedHtmlStream =
    Pin<Box<dyn Stream<Item = io::Result<Bytes>> + Send>>;

/// Returns an Axum [Handler](axum::handler::Handler) that listens for a `GET` request and tries
/// to route it using [halyard_router], serving an HTML stream of your application.
///
/// This can then be set up at an appropriate route in your application:
/// ```no_run
/// use axum::{handler::Handler, Router};
/// use halyard::{config::get_configuration, prelude::*};
/// use std::{env, net::SocketAddr};
///
/// #[component]
/// fn MyApp() -> impl IntoView {
///     view! { <main>"Hello, world!"</main> }
/// }
///
/// #[cfg(feature = "default")]
/// #[tokio::main]
/// async fn main() {
///     let conf = get_configuration(Some("Cargo.toml")).unwrap();
///     let halyard_options = conf.halyard_options;
///     let addr = halyard_options.site_addr.clone();
///
///     // build our application with a route
///     let app = Router::new().fallback(halyard_axum::render_app_to_stream(
///         || { /* your application here */ },
///     ));
///
///     // run our app with hyper
///     let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
///     axum::serve(listener, app.into_make_service())
///         .await
///         .unwrap();
/// }
///
/// # #[cfg(not(feature = "default"))]
/// # fn main() { }
/// ```
///
/// ## Provided Context Types
/// This function always provides context values including the following types:
/// - [`Parts`]
/// - [`ResponseOptions`]
/// - [`ServerMetaContext`]
#[cfg_attr(
    feature = "tracing",
    tracing::instrument(level = "trace", fields(error), skip_all)
)]
pub fn render_app_to_stream<IV>(
    app_fn: impl Fn() -> IV + Clone + Send + Sync + 'static,
) -> impl Fn(
    Request<Body>,
) -> Pin<Box<dyn Future<Output = Response<Body>> + Send + 'static>>
       + Clone
       + Send
       + 'static
where
    IV: IntoView + 'static,
{
    render_app_to_stream_with_context(|| {}, app_fn)
}

/// Returns an Axum [Handler](axum::handler::Handler) that listens for a `GET` request and tries
/// to route it using [halyard_router], serving an HTML stream of your application.
/// The difference between calling this and `render_app_to_stream_with_context()` is that this
/// one respects the `SsrMode` on each Route and thus requires `Vec<AxumRouteListing>` for route checking.
/// This is useful if you are using `.halyard_routes_with_handler()`
#[cfg_attr(
    feature = "tracing",
    tracing::instrument(level = "trace", fields(error), skip_all)
)]
pub fn render_route<S, IV>(
    paths: Vec<AxumRouteListing>,
    app_fn: impl Fn() -> IV + Clone + Send + Sync + 'static,
) -> impl Fn(
    State<S>,
    Request<Body>,
) -> Pin<Box<dyn Future<Output = Response<Body>> + Send + 'static>>
       + Clone
       + Send
       + 'static
where
    IV: IntoView + 'static,
    HalyardOptions: FromRef<S>,
    S: Send + 'static,
{
    render_route_with_context(paths, || {}, app_fn)
}

/// Returns an Axum [Handler](axum::handler::Handler) that listens for a `GET` request and tries
/// to route it using [halyard_router], serving an in-order HTML stream of your application.
/// This stream will pause at each `<Suspense/>` node and wait for it to resolve before
/// sending down its HTML. The app will become interactive once it has fully loaded.
///
/// This can then be set up at an appropriate route in your application:
/// ```no_run
/// use axum::{handler::Handler, Router};
/// use halyard::{config::get_configuration, prelude::*};
/// use std::{env, net::SocketAddr};
///
/// #[component]
/// fn MyApp() -> impl IntoView {
///     view! { <main>"Hello, world!"</main> }
/// }
///
/// #[cfg(feature = "default")]
/// #[tokio::main]
/// async fn main() {
///     let conf = get_configuration(Some("Cargo.toml")).unwrap();
///     let halyard_options = conf.halyard_options;
///     let addr = halyard_options.site_addr.clone();
///
///     // build our application with a route
///     let app = Router::new().fallback(
///         halyard_axum::render_app_to_stream_in_order(|| view! { <MyApp/> }),
///     );
///
///     // run our app with hyper
///     let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
///     axum::serve(listener, app.into_make_service())
///         .await
///         .unwrap();
/// }
///
/// # #[cfg(not(feature = "default"))]
/// # fn main() { }
/// ```
///
/// ## Provided Context Types
/// This function always provides context values including the following types:
/// - [`Parts`]
/// - [`ResponseOptions`]
/// - [`ServerMetaContext`]
#[cfg_attr(
    feature = "tracing",
    tracing::instrument(level = "trace", fields(error), skip_all)
)]
pub fn render_app_to_stream_in_order<IV>(
    app_fn: impl Fn() -> IV + Clone + Send + Sync + 'static,
) -> impl Fn(
    Request<Body>,
) -> Pin<Box<dyn Future<Output = Response<Body>> + Send + 'static>>
       + Clone
       + Send
       + 'static
where
    IV: IntoView + 'static,
{
    render_app_to_stream_in_order_with_context(|| {}, app_fn)
}

/// Returns an Axum [Handler](axum::handler::Handler) that listens for a `GET` request and tries
/// to route it using [halyard_router], serving an HTML stream of your application.
///
/// This version allows us to pass Axum State/Extension/Extractor or other info from Axum or network
/// layers above Halyard itself. To use it, you'll need to write your own handler function that provides
/// the data to halyard in a closure. An example is below
/// ```
/// use axum::{
///     body::Body,
///     extract::Path,
///     http::Request,
///     response::{IntoResponse, Response},
/// };
/// use halyard::{context::provide_context, prelude::*};
///
/// async fn custom_handler(
///     Path(id): Path<String>,
///     req: Request<Body>,
/// ) -> Response {
///     let handler = halyard_axum::render_app_to_stream_with_context(
///         move || {
///             provide_context(id.clone());
///         },
///         || { /* your app here */ },
///     );
///     handler(req).await.into_response()
/// }
/// ```
/// Otherwise, this function is identical to [render_app_to_stream].
///
/// ## Provided Context Types
/// This function always provides context values including the following types:
/// - [`Parts`]
/// - [`ResponseOptions`]
/// - [`ServerMetaContext`]
#[cfg_attr(
    feature = "tracing",
    tracing::instrument(level = "trace", fields(error), skip_all)
)]
pub fn render_app_to_stream_with_context<IV>(
    additional_context: impl Fn() + 'static + Clone + Send + Sync,
    app_fn: impl Fn() -> IV + Clone + Send + Sync + 'static,
) -> impl Fn(
    Request<Body>,
) -> Pin<Box<dyn Future<Output = Response<Body>> + Send + 'static>>
       + Clone
       + Send
       + Sync
       + 'static
where
    IV: IntoView + 'static,
{
    render_app_to_stream_with_context_and_replace_blocks(
        additional_context,
        app_fn,
        false,
    )
}
/// Returns an Axum [Handler](axum::handler::Handler) that listens for a `GET` request and tries
/// to route it using [halyard_router], serving an HTML stream of your application. It allows you
/// to pass in a context function with additional info to be made available to the app
/// The difference between calling this and `render_app_to_stream_with_context()` is that this
/// one respects the `SsrMode` on each Route, and thus requires `Vec<AxumRouteListing>` for route checking.
/// This is useful if you are using `.halyard_routes_with_handler()`.
#[cfg_attr(
    feature = "tracing",
    tracing::instrument(level = "trace", fields(error), skip_all)
)]
pub fn render_route_with_context<S, IV>(
    paths: Vec<AxumRouteListing>,
    additional_context: impl Fn() + 'static + Clone + Send + Sync,
    app_fn: impl Fn() -> IV + Clone + Send + Sync + 'static,
) -> impl Fn(
    State<S>,
    Request<Body>,
) -> Pin<Box<dyn Future<Output = Response<Body>> + Send + 'static>>
       + Clone
       + Send
       + 'static
where
    IV: IntoView + 'static,
    HalyardOptions: FromRef<S>,
    S: Send + 'static,
{
    let ooo = render_app_to_stream_with_context(
        additional_context.clone(),
        app_fn.clone(),
    );
    let pb = render_app_to_stream_with_context_and_replace_blocks(
        additional_context.clone(),
        app_fn.clone(),
        true,
    );
    let io = render_app_to_stream_in_order_with_context(
        additional_context.clone(),
        app_fn.clone(),
    );
    let asyn = render_app_async_stream_with_context(
        additional_context.clone(),
        app_fn.clone(),
    );

    move |state, req| {
        // 1. Find the RouteListing of the route axum matched
        let listing = match matched_listing(&paths, &req) {
            Ok(listing) => listing,
            Err(error) => {
                let response: PinnedFuture<Response<Body>> =
                    Box::pin(ready(error_response(&error, req.headers())));
                return response;
            }
        };
        // 2. Match listing mode against known, and choose function
        match listing.mode() {
            SsrMode::OutOfOrder => ooo(req),
            SsrMode::PartiallyBlocked => pb(req),
            SsrMode::InOrder => io(req),
            SsrMode::Async => asyn(req),
            SsrMode::Static(_) => {
                #[cfg(feature = "default")]
                {
                    let regenerate = listing.regenerate.clone();
                    handle_static_route(
                        additional_context.clone(),
                        app_fn.clone(),
                        regenerate,
                    )(state, req)
                }
                #[cfg(not(feature = "default"))]
                {
                    _ = state;
                    Box::pin(ready(error_response(
                        &RequestError::StaticRoutesUnsupported,
                        req.headers(),
                    )))
                }
            }
        }
    }
}

/// The listing of the route that axum matched for `req`. This should probably be optimized,
/// we probably don't want to search for this every time.
fn matched_listing<'a>(
    paths: &'a [AxumRouteListing],
    req: &Request<Body>,
) -> Result<&'a AxumRouteListing, RequestError> {
    let path = req
        .extensions()
        .get::<MatchedPath>()
        .ok_or(RequestError::NoMatchedPath)?
        .as_str();
    paths.iter().find(|r| r.path() == path).ok_or_else(|| {
        RequestError::UnknownRoute {
            path: path.to_owned(),
        }
    })
}

/// Returns an Axum [Handler](axum::handler::Handler) that listens for a `GET` request and tries
/// to route it using [halyard_router], serving an HTML stream of your application.
///
/// This version allows us to pass Axum State/Extension/Extractor or other info from Axum or network
/// layers above Halyard itself. To use it, you'll need to write your own handler function that provides
/// the data to halyard in a closure.
///
/// `replace_blocks` additionally lets you specify whether `<Suspense/>` fragments that read
/// from blocking resources should be retrojected into the HTML that's initially served, rather
/// than dynamically inserting them with JavaScript on the client. This means you will have
/// better support if JavaScript is not enabled, in exchange for a marginally slower response time.
///
/// Otherwise, this function is identical to [render_app_to_stream_with_context].
///
/// ## Provided Context Types
/// This function always provides context values including the following types:
/// - [`Parts`]
/// - [`ResponseOptions`]
/// - [`ServerMetaContext`]
#[cfg_attr(
    feature = "tracing",
    tracing::instrument(level = "trace", fields(error), skip_all)
)]
pub fn render_app_to_stream_with_context_and_replace_blocks<IV>(
    additional_context: impl Fn() + 'static + Clone + Send + Sync,
    app_fn: impl Fn() -> IV + Clone + Send + Sync + 'static,
    replace_blocks: bool,
) -> impl Fn(
    Request<Body>,
) -> Pin<Box<dyn Future<Output = Response<Body>> + Send + 'static>>
       + Clone
       + Send
       + Sync
       + 'static
where
    IV: IntoView + 'static,
{
    _ = replace_blocks; // TODO
    handle_response(additional_context, app_fn, |app, chunks, supports_ooo| {
        Box::pin(async move {
            let app = if cfg!(feature = "islands-router") {
                if supports_ooo {
                    app.to_html_stream_out_of_order_branching()
                } else {
                    app.to_html_stream_in_order_branching()
                }
            } else if supports_ooo {
                app.to_html_stream_out_of_order()
            } else {
                app.to_html_stream_in_order()
            };
            Box::pin(app.chain(chunks())) as PinnedStream<String>
        })
    })
}

/// Returns an Axum [Handler](axum::handler::Handler) that listens for a `GET` request and tries
/// to route it using [halyard_router], serving an in-order HTML stream of your application.
/// This stream will pause at each `<Suspense/>` node and wait for it to resolve before
/// sending down its HTML. The app will become interactive once it has fully loaded.
///
/// This version allows us to pass Axum State/Extension/Extractor or other info from Axum or network
/// layers above Halyard itself. To use it, you'll need to write your own handler function that provides
/// the data to halyard in a closure. An example is below
/// ```
/// use axum::{
///     body::Body,
///     extract::Path,
///     http::Request,
///     response::{IntoResponse, Response},
/// };
/// use halyard::context::provide_context;
///
/// async fn custom_handler(
///     Path(id): Path<String>,
///     req: Request<Body>,
/// ) -> Response {
///     let handler = halyard_axum::render_app_to_stream_in_order_with_context(
///         move || {
///             provide_context(id.clone());
///         },
///         || { /* your application here */ },
///     );
///     handler(req).await.into_response()
/// }
/// ```
/// Otherwise, this function is identical to [render_app_to_stream].
///
/// ## Provided Context Types
/// This function always provides context values including the following types:
/// - [`Parts`]
/// - [`ResponseOptions`]
/// - [`ServerMetaContext`]
#[cfg_attr(
    feature = "tracing",
    tracing::instrument(level = "trace", fields(error), skip_all)
)]
pub fn render_app_to_stream_in_order_with_context<IV>(
    additional_context: impl Fn() + 'static + Clone + Send + Sync,
    app_fn: impl Fn() -> IV + Clone + Send + Sync + 'static,
) -> impl Fn(
    Request<Body>,
) -> Pin<Box<dyn Future<Output = Response<Body>> + Send + 'static>>
       + Clone
       + Send
       + 'static
where
    IV: IntoView + 'static,
{
    handle_response(additional_context, app_fn, |app, chunks, _supports_ooo| {
        let app = if cfg!(feature = "islands-router") {
            app.to_html_stream_in_order_branching()
        } else {
            app.to_html_stream_in_order()
        };
        Box::pin(async move {
            Box::pin(app.chain(chunks())) as PinnedStream<String>
        })
    })
}

fn handle_response<IV>(
    additional_context: impl Fn() + 'static + Clone + Send + Sync,
    app_fn: impl Fn() -> IV + Clone + Send + Sync + 'static,
    stream_builder: fn(
        IV,
        BoxedFnOnce<PinnedStream<String>>,
        bool,
    ) -> PinnedFuture<PinnedStream<String>>,
) -> impl Fn(Request<Body>) -> PinnedFuture<Response<Body>>
       + Clone
       + Send
       + Sync
       + 'static
where
    IV: IntoView + 'static,
{
    move |req: Request<Body>| {
        let app_fn = app_fn.clone();
        let additional_context = additional_context.clone();
        handle_response_inner(additional_context, app_fn, req, stream_builder)
    }
}

/// Can be used in conjunction with a custom [file_and_error_handler_with_context] to process an Axum [Request](axum::extract::Request) into an Axum [Response](axum::response::Response)
///
/// A request whose target has no path (the authority form of `CONNECT host:port`) is
/// answered with 400 Bad Request, as there is no page to render.
pub fn handle_response_inner<IV>(
    additional_context: impl Fn() + 'static + Clone + Send,
    app_fn: impl FnOnce() -> IV + Send + 'static,
    req: Request<Body>,
    stream_builder: fn(
        IV,
        BoxedFnOnce<PinnedStream<String>>,
        bool,
    ) -> PinnedFuture<PinnedStream<String>>,
) -> PinnedFuture<Response<Body>>
where
    IV: IntoView + 'static,
{
    Box::pin(async move {
        // Need to get the path and query string of the Request
        // For reasons that escape me, if the incoming URI protocol is https, it provides the absolute URI
        let Some(path) = req.uri().path_and_query().cloned() else {
            let error = RequestError::NoPath {
                target: req.uri().to_string(),
            };
            return error_response(&error, req.headers());
        };

        let is_island_router_navigation = cfg!(feature = "islands-router")
            && req.headers().get("Islands-Router").is_some();

        let add_context = additional_context.clone();
        let res_options = ResponseOptions::default();
        let (meta_context, meta_output) = ServerMetaContext::new();

        let additional_context = {
            let meta_context = meta_context.clone();
            let res_options = res_options.clone();
            move || {
                let full_path = format!("https://leptos.dev{path}");
                let (_, req_parts) = generate_request_and_parts(req);
                provide_contexts(
                    &full_path,
                    &meta_context,
                    req_parts,
                    res_options.clone(),
                );
                add_context();

                if is_island_router_navigation {
                    provide_context(IslandsRouterNavigation);
                }
            }
        };

        let res = AxumResponse::from_app(
            app_fn,
            meta_output,
            additional_context,
            res_options,
            stream_builder,
            !is_island_router_navigation,
        )
        .await;

        res.0
    })
}

#[cfg_attr(
    feature = "tracing",
    tracing::instrument(level = "trace", fields(error), skip_all)
)]
fn provide_contexts(
    path: &str,
    meta_context: &ServerMetaContext,
    parts: Parts,
    default_res_options: ResponseOptions,
) {
    provide_context(RequestUrl::new(path));
    provide_context(meta_context.clone());
    provide_context(parts);
    provide_context(default_res_options);
    provide_server_redirect(redirect);
    halyard::nonce::provide_nonce();
}

/// Returns an Axum [Handler](axum::handler::Handler) that listens for a `GET` request and tries
/// to route it using [halyard_router], asynchronously rendering an HTML page after all
/// `async` resources have loaded.
///
/// This can then be set up at an appropriate route in your application:
/// ```no_run
/// use axum::{handler::Handler, Router};
/// use halyard::{config::get_configuration, prelude::*};
/// use std::{env, net::SocketAddr};
///
/// #[component]
/// fn MyApp() -> impl IntoView {
///     view! { <main>"Hello, world!"</main> }
/// }
///
/// #[cfg(feature = "default")]
/// #[tokio::main]
/// async fn main() {
///     let conf = get_configuration(Some("Cargo.toml")).unwrap();
///     let halyard_options = conf.halyard_options;
///     let addr = halyard_options.site_addr.clone();
///
///     // build our application with a route
///     let app = Router::new()
///         .fallback(halyard_axum::render_app_async(|| view! { <MyApp/> }));
///
///     // run our app with hyper
///     // `axum::Server` is a re-export of `hyper::Server`
///     let listener =
///         tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
///     axum::serve(listener, app.into_make_service())
///         .await
///         .unwrap();
/// }
///
/// # #[cfg(not(feature = "default"))]
/// # fn main() { }
/// ```
///
/// ## Provided Context Types
/// This function always provides context values including the following types:
/// - [`Parts`]
/// - [`ResponseOptions`]
/// - [`ServerMetaContext`]
#[cfg_attr(
    feature = "tracing",
    tracing::instrument(level = "trace", fields(error), skip_all)
)]
pub fn render_app_async<IV>(
    app_fn: impl Fn() -> IV + Clone + Send + Sync + 'static,
) -> impl Fn(
    Request<Body>,
) -> Pin<Box<dyn Future<Output = Response<Body>> + Send + 'static>>
       + Clone
       + Send
       + 'static
where
    IV: IntoView + 'static,
{
    render_app_async_with_context(|| {}, app_fn)
}

/// Returns an Axum [Handler](axum::handler::Handler) that listens for a `GET` request and tries
/// to route it using [halyard_router], asynchronously rendering an HTML page after all
/// `async` resources have loaded.
///
/// This version allows us to pass Axum State/Extension/Extractor or other info from Axum or network
/// layers above Halyard itself. To use it, you'll need to write your own handler function that provides
/// the data to halyard in a closure. An example is below
/// ```
/// use axum::{
///     body::Body,
///     extract::Path,
///     http::Request,
///     response::{IntoResponse, Response},
/// };
/// use halyard::context::provide_context;
///
/// async fn custom_handler(
///     Path(id): Path<String>,
///     req: Request<Body>,
/// ) -> Response {
///     let handler = halyard_axum::render_app_async_with_context(
///         move || {
///             provide_context(id.clone());
///         },
///         || { /* your application here */ },
///     );
///     handler(req).await.into_response()
/// }
/// ```
/// Otherwise, this function is identical to [render_app_to_stream].
///
/// ## Provided Context Types
/// This function always provides context values including the following types:
/// - [`Parts`]
/// - [`ResponseOptions`]
/// - [`ServerMetaContext`]
#[cfg_attr(
    feature = "tracing",
    tracing::instrument(level = "trace", fields(error), skip_all)
)]
pub fn render_app_async_stream_with_context<IV>(
    additional_context: impl Fn() + 'static + Clone + Send + Sync,
    app_fn: impl Fn() -> IV + Clone + Send + Sync + 'static,
) -> impl Fn(
    Request<Body>,
) -> Pin<Box<dyn Future<Output = Response<Body>> + Send + 'static>>
       + Clone
       + Send
       + 'static
where
    IV: IntoView + 'static,
{
    handle_response(additional_context, app_fn, |app, chunks, _supports_ooo| {
        Box::pin(async move {
            let app = if cfg!(feature = "islands-router") {
                app.to_html_stream_in_order_branching()
            } else {
                app.to_html_stream_in_order()
            };
            let app = app.collect::<String>().await;
            let chunks = chunks();
            Box::pin(once(async move { app }).chain(chunks))
                as PinnedStream<String>
        })
    })
}

/// Returns an Axum [Handler](axum::handler::Handler) that listens for a `GET` request and tries
/// to route it using [halyard_router], asynchronously rendering an HTML page after all
/// `async` resources have loaded.
///
/// This version allows us to pass Axum State/Extension/Extractor or other info from Axum or network
/// layers above Halyard itself. To use it, you'll need to write your own handler function that provides
/// the data to halyard in a closure. An example is below
/// ```
/// use axum::{
///     body::Body,
///     extract::Path,
///     http::Request,
///     response::{IntoResponse, Response},
/// };
/// use halyard::context::provide_context;
///
/// async fn custom_handler(
///     Path(id): Path<String>,
///     req: Request<Body>,
/// ) -> Response {
///     let handler = halyard_axum::render_app_async_with_context(
///         move || {
///             provide_context(id.clone());
///         },
///         || { /* your application here */ },
///     );
///     handler(req).await.into_response()
/// }
/// ```
/// Otherwise, this function is identical to [render_app_to_stream].
///
/// ## Provided Context Types
/// This function always provides context values including the following types:
/// - [`Parts`]
/// - [`ResponseOptions`]
/// - [`ServerMetaContext`]
#[cfg_attr(
    feature = "tracing",
    tracing::instrument(level = "trace", fields(error), skip_all)
)]
pub fn render_app_async_with_context<IV>(
    additional_context: impl Fn() + 'static + Clone + Send + Sync,
    app_fn: impl Fn() -> IV + Clone + Send + Sync + 'static,
) -> impl Fn(
    Request<Body>,
) -> Pin<Box<dyn Future<Output = Response<Body>> + Send + 'static>>
       + Clone
       + Send
       + 'static
where
    IV: IntoView + 'static,
{
    handle_response(additional_context, app_fn, async_stream_builder)
}

fn async_stream_builder<IV>(
    app: IV,
    chunks: BoxedFnOnce<PinnedStream<String>>,
    _supports_ooo: bool,
) -> PinnedFuture<PinnedStream<String>>
where
    IV: IntoView + 'static,
{
    Box::pin(async move {
        let app = if cfg!(feature = "islands-router") {
            app.to_html_stream_in_order_branching()
        } else {
            app.to_html_stream_in_order()
        };
        let app = app.collect::<String>().await;
        let chunks = chunks();
        Box::pin(once(async move { app }).chain(chunks)) as PinnedStream<String>
    })
}

/// Generates a list of all routes defined in Halyard's Router in your app. We can then use this to automatically
/// create routes in Axum's Router without having to use wildcard matching or fallbacks. Takes in your root app Element
/// as an argument so it can walk you app tree. This version is tailored to generate Axum compatible paths.
#[cfg_attr(
    feature = "tracing",
    tracing::instrument(level = "trace", fields(error), skip_all)
)]
pub fn generate_route_list<IV>(
    app_fn: impl Fn() -> IV + 'static + Clone + Send,
) -> Vec<AxumRouteListing>
where
    IV: IntoView + 'static,
{
    generate_route_list_with_exclusions_and_ssg(app_fn, None).0
}

/// Generates a list of all routes defined in Halyard's Router in your app. We can then use this to automatically
/// create routes in Axum's Router without having to use wildcard matching or fallbacks. Takes in your root app Element
/// as an argument so it can walk you app tree. This version is tailored to generate Axum compatible paths.
#[cfg_attr(
    feature = "tracing",
    tracing::instrument(level = "trace", fields(error), skip_all)
)]
pub fn generate_route_list_with_ssg<IV>(
    app_fn: impl Fn() -> IV + 'static + Clone + Send,
) -> (Vec<AxumRouteListing>, StaticRouteGenerator)
where
    IV: IntoView + 'static,
{
    generate_route_list_with_exclusions_and_ssg(app_fn, None)
}

/// Generates a list of all routes defined in Halyard's Router in your app. We can then use this to automatically
/// create routes in Axum's Router without having to use wildcard matching or fallbacks. Takes in your root app Element
/// as an argument so it can walk you app tree. This version is tailored to generate Axum compatible paths. Adding excluded_routes
/// to this function will stop `.halyard_routes()` from generating a route for it, allowing a custom handler. These need to be in Axum path format
#[cfg_attr(
    feature = "tracing",
    tracing::instrument(level = "trace", fields(error), skip_all)
)]
pub fn generate_route_list_with_exclusions<IV>(
    app_fn: impl Fn() -> IV + 'static + Clone + Send,
    excluded_routes: Option<Vec<String>>,
) -> Vec<AxumRouteListing>
where
    IV: IntoView + 'static,
{
    generate_route_list_with_exclusions_and_ssg(app_fn, excluded_routes).0
}

/// Generates a list of all routes defined in Halyard's Router in your app. We can then use this to automatically
/// create routes in Axum's Router without having to use wildcard matching or fallbacks. Takes in your root app Element
/// as an argument so it can walk you app tree. This version is tailored to generate Axum compatible paths. Adding excluded_routes
/// to this function will stop `.halyard_routes()` from generating a route for it, allowing a custom handler. These need to be in Axum path format
#[cfg_attr(
    feature = "tracing",
    tracing::instrument(level = "trace", fields(error), skip_all)
)]
pub fn generate_route_list_with_exclusions_and_ssg<IV>(
    app_fn: impl Fn() -> IV + 'static + Clone + Send,
    excluded_routes: Option<Vec<String>>,
) -> (Vec<AxumRouteListing>, StaticRouteGenerator)
where
    IV: IntoView + 'static,
{
    generate_route_list_with_exclusions_and_ssg_and_context(
        app_fn,
        excluded_routes,
        || {},
    )
}

#[derive(Clone, Debug, Default)]
/// A route that this application can serve.
pub struct AxumRouteListing {
    path: String,
    mode: SsrMode,
    methods: Vec<halyard_router::Method>,
    #[allow(unused)]
    regenerate: Vec<RegenerationFn>,
    exclude: bool,
}

trait IntoRouteListing: Sized {
    fn into_route_listing(self) -> Vec<AxumRouteListing>;
}

impl IntoRouteListing for RouteListing {
    fn into_route_listing(self) -> Vec<AxumRouteListing> {
        self.path()
            .to_vec()
            .expand_optionals()
            .into_iter()
            .map(|path| {
                let path = path.to_axum_path();
                let path = if path.is_empty() {
                    "/".to_string()
                } else {
                    path
                };
                let mode = self.mode();
                let methods = self.methods().collect();
                let regenerate = self.regenerate().into();
                AxumRouteListing {
                    path,
                    mode: mode.clone(),
                    methods,
                    regenerate,
                    exclude: false,
                }
            })
            .collect()
    }
}

impl AxumRouteListing {
    /// Create a route listing from its parts.
    pub fn new(
        path: String,
        mode: SsrMode,
        methods: impl IntoIterator<Item = halyard_router::Method>,
        regenerate: impl Into<Vec<RegenerationFn>>,
    ) -> Self {
        Self {
            path,
            mode,
            methods: methods.into_iter().collect(),
            regenerate: regenerate.into(),
            exclude: false,
        }
    }

    /// The path this route handles.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The rendering mode for this path.
    pub fn mode(&self) -> &SsrMode {
        &self.mode
    }

    /// The HTTP request methods this path can handle.
    pub fn methods(&self) -> impl Iterator<Item = halyard_router::Method> + '_ {
        self.methods.iter().copied()
    }
}

/// Generates a list of all routes defined in Halyard's Router in your app. We can then use this to automatically
/// create routes in Axum's Router without having to use wildcard matching or fallbacks. Takes in your root app Element
/// as an argument so it can walk you app tree. This version is tailored to generate Axum compatible paths. Adding excluded_routes
/// to this function will stop `.halyard_routes()` from generating a route for it, allowing a custom handler. These need to be in Axum path format
/// Additional context will be provided to the app Element.
#[cfg_attr(
    feature = "tracing",
    tracing::instrument(level = "trace", fields(error), skip_all)
)]
pub fn generate_route_list_with_exclusions_and_ssg_and_context<IV>(
    app_fn: impl Fn() -> IV + Clone + Send + 'static,
    excluded_routes: Option<Vec<String>>,
    additional_context: impl Fn() + Clone + Send + 'static,
) -> (Vec<AxumRouteListing>, StaticRouteGenerator)
where
    IV: IntoView + 'static,
{
    // do some basic reactive setup
    init_executor();
    let owner = Owner::new_root(Some(Arc::new(SsrSharedContext::new())));

    let routes = owner
        .with(|| {
            // stub out a path for now
            provide_context(RequestUrl::new(""));
            let (mock_parts, _) = Request::new(Body::from("")).into_parts();
            let (mock_meta, _) = ServerMetaContext::new();
            provide_contexts("", &mock_meta, mock_parts, Default::default());
            additional_context();
            RouteList::generate(&app_fn)
        })
        .unwrap_or_default();

    let generator = StaticRouteGenerator::new(
        &routes,
        app_fn.clone(),
        additional_context.clone(),
    );

    // Axum's Router defines Root routes as "/" not ""
    let mut routes = routes
        .into_inner()
        .into_iter()
        .flat_map(IntoRouteListing::into_route_listing)
        .collect::<Vec<_>>();

    let routes = if routes.is_empty() {
        vec![AxumRouteListing::new(
            "/".to_string(),
            Default::default(),
            [halyard_router::Method::Get],
            vec![],
        )]
    } else {
        // Routes to exclude from auto generation
        if let Some(excluded_routes) = &excluded_routes {
            routes.retain(|p| !excluded_routes.iter().any(|e| e == p.path()))
        }
        routes
    };
    let excluded =
        excluded_routes
            .into_iter()
            .flatten()
            .map(|path| AxumRouteListing {
                path,
                mode: Default::default(),
                methods: Vec::new(),
                regenerate: Vec::new(),
                exclude: true,
            });

    (routes.into_iter().chain(excluded).collect(), generator)
}

/// Allows generating any prerendered routes.
#[allow(clippy::type_complexity)]
pub struct StaticRouteGenerator(
    // this is here to keep the root owner alive for the duration
    // of the route generation, so that base context provided continues
    // to exist until it is dropped
    #[allow(dead_code)] Owner,
    Box<dyn FnOnce(&HalyardOptions) -> PinnedFuture<()> + Send>,
);

impl StaticRouteGenerator {
    #[cfg(feature = "default")]
    fn render_route<IV: IntoView + 'static>(
        path: String,
        app_fn: impl Fn() -> IV + Clone + Send + 'static,
        additional_context: impl Fn() + Clone + Send + 'static,
    ) -> impl Future<Output = (Owner, String)> {
        let (meta_context, meta_output) = ServerMetaContext::new();
        let additional_context = {
            let add_context = additional_context.clone();
            move || {
                let full_path = format!("https://leptos.dev{path}");
                let mut mock_req = Request::new(Body::empty());
                *mock_req.method_mut() = Method::GET;
                mock_req.headers_mut().insert(
                    ACCEPT,
                    const { HeaderValue::from_static("text/html") },
                );
                let (mock_parts, _) = mock_req.into_parts();
                let res_options = ResponseOptions::default();
                provide_contexts(
                    &full_path,
                    &meta_context,
                    mock_parts,
                    res_options,
                );
                add_context();
            }
        };

        let (owner, stream) = halyard_integration_utils::build_response(
            app_fn.clone(),
            additional_context,
            async_stream_builder,
            false,
        );

        async move {
            let stream = stream.await;
            await_deferred(&owner).await;

            let html = meta_output
                .inject_meta_context(stream)
                .await
                .collect::<String>()
                .await;
            (owner, html)
        }
    }

    /// Creates a new static route generator from the given list of route definitions.
    pub fn new<IV>(
        routes: &RouteList,
        app_fn: impl Fn() -> IV + Clone + Send + 'static,
        additional_context: impl Fn() + Clone + Send + 'static,
    ) -> Self
    where
        IV: IntoView + 'static,
    {
        #[cfg(feature = "default")]
        {
            let owner = Owner::new();
            Self(owner.clone(), {
                let routes = routes.clone();
                Box::new(move |options| {
                    let options = options.clone();
                    let app_fn = app_fn.clone();
                    let additional_context = additional_context.clone();
                    owner.with(|| {
                        additional_context();
                        Box::pin(ScopedFuture::new(routes.generate_static_files(
                        move |path: &ResolvedStaticPath| {
                            Self::render_route(
                                path.to_string(),
                                app_fn.clone(),
                                additional_context.clone(),
                            )
                        },
                        move |path: &ResolvedStaticPath,
                              owner: &Owner,
                              html: String| {
                            let options = options.clone();
                            let path = path.to_owned();
                            let response_options = owner.with(use_context);
                            async move {
                                write_static_route(
                                    &options,
                                    response_options,
                                    path.as_ref(),
                                    &html,
                                )
                                .await
                            }
                        },
                        was_404,
                    )))
                    })
                })
            })
        }

        #[cfg(not(feature = "default"))]
        {
            _ = routes;
            _ = app_fn;
            _ = additional_context;
            Self(
                Owner::new(),
                Box::new(|_| {
                    report(&RouteError::StaticGenerationUnsupported, None);
                    Box::pin(ready(())) as PinnedFuture<()>
                }),
            )
        }
    }

    /// Generates the routes.
    pub async fn generate(self, options: &HalyardOptions) {
        (self.1)(options).await
    }
}

#[cfg(feature = "default")]
static STATIC_HEADERS: LazyLock<
    std::sync::RwLock<HashMap<String, ResponseOptions>>,
> = LazyLock::new(Default::default);

/// Waits for the data that the render under `owner` deferred. Without a shared context
/// nothing can have been deferred.
#[cfg(feature = "default")]
async fn await_deferred(owner: &Owner) {
    if let Some(sc) = owner.shared_context() {
        while let Some(pending) = sc.await_deferred() {
            pending.await;
        }
    }
}

#[cfg(feature = "default")]
fn was_404(owner: &Owner) -> bool {
    // without `ResponseOptions`, nothing can have set the status
    let Some(resp) = owner.with(use_context::<ResponseOptions>) else {
        return false;
    };
    let status = resp.0.read().or_poisoned().status;

    status == Some(StatusCode::NOT_FOUND)
}

#[cfg(feature = "default")]
fn static_path(options: &HalyardOptions, path: &str) -> String {
    use halyard_integration_utils::static_file_path;

    // If the path ends with a trailing slash, we generate the path
    // as a directory with a index.html file inside.
    if path != "/" && path.ends_with("/") {
        static_file_path(options, &format!("{path}index"))
    } else {
        static_file_path(options, path)
    }
}

#[cfg(feature = "default")]
async fn write_static_route(
    options: &HalyardOptions,
    response_options: Option<ResponseOptions>,
    path: &str,
    html: &str,
) -> Result<(), std::io::Error> {
    if let Some(options) = response_options {
        STATIC_HEADERS
            .write()
            .or_poisoned()
            .insert(path.to_string(), options);
    }

    let path = static_path(options, path);
    let path = Path::new(&path);
    if let Some(path) = path.parent() {
        tokio::fs::create_dir_all(path).await?;
    }
    tokio::fs::write(path, &html).await?;

    Ok(())
}

#[cfg(feature = "default")]
fn handle_static_route<S, IV>(
    additional_context: impl Fn() + 'static + Clone + Send,
    app_fn: impl Fn() -> IV + Clone + Send + 'static,
    regenerate: Vec<RegenerationFn>,
) -> impl Fn(
    State<S>,
    Request<Body>,
) -> Pin<Box<dyn Future<Output = Response<Body>> + Send + 'static>>
       + Clone
       + Send
       + 'static
where
    HalyardOptions: FromRef<S>,
    S: Send + 'static,
    IV: IntoView + 'static,
{
    use tower_http::services::ServeFile;

    move |state, req| {
        let app_fn = app_fn.clone();
        let additional_context = additional_context.clone();
        let regenerate = regenerate.clone();
        Box::pin(async move {
            let options = HalyardOptions::from_ref(&state);
            let orig_path = req.uri().path();
            let path = static_path(&options, orig_path);
            let path = Path::new(&path);
            let exists = tokio::fs::try_exists(path).await.unwrap_or(false);

            let (response_options, html) = if !exists {
                let path = ResolvedStaticPath::new(orig_path);

                let (owner, html) = path
                    .build(
                        move |path: &ResolvedStaticPath| {
                            StaticRouteGenerator::render_route(
                                path.to_string(),
                                app_fn.clone(),
                                additional_context.clone(),
                            )
                        },
                        move |path: &ResolvedStaticPath,
                              owner: &Owner,
                              html: String| {
                            let options = options.clone();
                            let path = path.to_owned();
                            let response_options = owner.with(use_context);
                            async move {
                                write_static_route(
                                    &options,
                                    response_options,
                                    path.as_ref(),
                                    &html,
                                )
                                .await
                            }
                        },
                        was_404,
                        regenerate,
                    )
                    .await;
                (owner.with(use_context::<ResponseOptions>), html)
            } else {
                let headers =
                    STATIC_HEADERS.read().or_poisoned().get(orig_path).cloned();
                (headers, None)
            };

            // if html is Some(_), it means that `was_error_response` is true and we're not
            // actually going to cache this route, just return it as HTML
            //
            // this if for thing like 404s, where we do not want to cache an endless series of
            // typos (or malicious requests)
            let mut res = AxumResponse(match html {
                Some(html) => axum::response::Html(html).into_response(),
                None => match ServeFile::new(path).oneshot(req).await {
                    Ok(res) => res.into_response(),
                    Err(err) => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Something went wrong: {err}"),
                    )
                        .into_response(),
                },
            });

            if let Some(options) = response_options {
                res.extend_response(&options);
            }

            res.0
        })
    }
}

/// This trait allows one to pass a list of routes and a render function to Axum's router, letting us avoid
/// having to use wildcards or manually define all routes in multiple places.
///
/// A route that axum cannot route is logged and left out, where `axum::Router::route` would
/// panic: a path that is not valid axum syntax, a method that axum cannot filter, a second
/// handler for the same path and method (the first is kept), or a path that axum cannot tell
/// apart from one added before it (`/users/{id}` and `/users/{name}`). Only the routes these
/// methods add are checked; a route already on the router can still conflict.
pub trait HalyardRoutes<S>
where
    S: Clone + Send + Sync + 'static,
    HalyardOptions: FromRef<S>,
{
    /// Adds the routes generated by `halyard_router` to the Axum router.
    fn halyard_routes<IV>(
        self,
        options: &S,
        paths: Vec<AxumRouteListing>,
        app_fn: impl Fn() -> IV + Clone + Send + Sync + 'static,
    ) -> Self
    where
        IV: IntoView + 'static;

    /// Adds the routes generated by `halyard_router` to the Axum router.
    ///
    /// Runs `additional_context` to provide additional data to the reactive system via context,
    /// when handling a route.
    fn halyard_routes_with_context<IV>(
        self,
        options: &S,
        paths: Vec<AxumRouteListing>,
        additional_context: impl Fn() + 'static + Clone + Send + Sync,
        app_fn: impl Fn() -> IV + Clone + Send + Sync + 'static,
    ) -> Self
    where
        IV: IntoView + 'static;

    /// Extends the Axum router with the given paths, and handles the requests with the given
    /// handler.
    fn halyard_routes_with_handler<H, T>(
        self,
        paths: Vec<AxumRouteListing>,
        handler: H,
    ) -> Self
    where
        H: axum::handler::Handler<T, S>,
        T: 'static;
}

trait AxumPath {
    fn to_axum_path(&self) -> String;
}

impl AxumPath for Vec<PathSegment> {
    fn to_axum_path(&self) -> String {
        let mut path = String::new();
        for segment in self.iter() {
            // TODO trailing slash handling
            let raw = segment.as_raw_str();
            if !raw.is_empty() && !raw.starts_with('/') {
                path.push('/');
            }
            match segment {
                // A static segment is literal. axum reads `{` and `}` as the braces of a
                // parameter (and panics on a lone one), and a doubled brace as a literal one.
                PathSegment::Static(s) => {
                    path.push_str(&s.replace('{', "{{").replace('}', "}}"))
                }
                PathSegment::Param(s) => {
                    path.push('{');
                    path.push_str(s);
                    path.push('}');
                }
                PathSegment::Splat(s) => {
                    path.push('{');
                    path.push('*');
                    path.push_str(s);
                    path.push('}');
                }
                PathSegment::Unit => {}
                PathSegment::OptionalParam(_) => {
                    #[cfg(feature = "tracing")]
                    tracing::error!(
                        "to_axum_path should only be called on expanded \
                         paths, which do not have OptionalParam any longer"
                    );
                    Default::default()
                }
            }
        }
        path
    }
}

/// The default implementation of `HalyardRoutes` which takes in a list of paths, and dispatches GET requests
/// to those paths to Halyard's renderer.
impl<S> HalyardRoutes<S> for axum::Router<S>
where
    S: Clone + Send + Sync + 'static,
    HalyardOptions: FromRef<S>,
{
    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(level = "trace", fields(error), skip_all)
    )]
    fn halyard_routes<IV>(
        self,
        state: &S,
        paths: Vec<AxumRouteListing>,
        app_fn: impl Fn() -> IV + Clone + Send + Sync + 'static,
    ) -> Self
    where
        IV: IntoView + 'static,
    {
        self.halyard_routes_with_context(state, paths, || {}, app_fn)
    }

    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(level = "trace", fields(error), skip_all)
    )]
    fn halyard_routes_with_context<IV>(
        self,
        state: &S,
        paths: Vec<AxumRouteListing>,
        additional_context: impl Fn() + 'static + Clone + Send + Sync,
        app_fn: impl Fn() -> IV + Clone + Send + Sync + 'static,
    ) -> Self
    where
        IV: IntoView + 'static,
    {
        init_executor();

        // S represents the router's finished state, provided via context to
        // every route it renders.
        let state = state.clone();
        let cx_with_state = move || {
            provide_context::<S>(state.clone());
            additional_context();
        };

        let mut router = self;
        let mut routes = RouteRegistry::default();

        // register router paths
        for listing in paths.iter().filter(|p| !p.exclude) {
            let path = listing.path();

            for method in listing.methods() {
                let cx_with_state = cx_with_state.clone();
                let cx = move || {
                    provide_context(method);
                    cx_with_state();
                };
                let app_fn = app_fn.clone();
                let http_method = http_method(method);
                router = match listing.mode() {
                    SsrMode::OutOfOrder => add_route(
                        router,
                        &mut routes,
                        path,
                        &http_method,
                        |f| {
                            on(f, render_app_to_stream_with_context(cx, app_fn))
                        },
                    ),
                    SsrMode::PartiallyBlocked => add_route(
                        router,
                        &mut routes,
                        path,
                        &http_method,
                        |f| {
                            on(
                                f,
                                render_app_to_stream_with_context_and_replace_blocks(
                                    cx, app_fn, true,
                                ),
                            )
                        },
                    ),
                    SsrMode::InOrder => add_route(
                        router,
                        &mut routes,
                        path,
                        &http_method,
                        |f| {
                            on(
                                f,
                                render_app_to_stream_in_order_with_context(
                                    cx, app_fn,
                                ),
                            )
                        },
                    ),
                    SsrMode::Async => add_route(
                        router,
                        &mut routes,
                        path,
                        &http_method,
                        |f| on(f, render_app_async_with_context(cx, app_fn)),
                    ),
                    // a static page is always served for GET
                    SsrMode::Static(_) => {
                        #[cfg(feature = "default")]
                        {
                            let regenerate = listing.regenerate.clone();
                            add_route(
                                router,
                                &mut routes,
                                path,
                                &Method::GET,
                                |f| {
                                    on(
                                        f,
                                        handle_static_route(
                                            cx, app_fn, regenerate,
                                        ),
                                    )
                                },
                            )
                        }
                        #[cfg(not(feature = "default"))]
                        {
                            _ = (cx, app_fn);
                            report(
                                &RouteError::StaticRoutesUnsupported {
                                    path: path.to_owned(),
                                },
                                None,
                            );
                            router
                        }
                    }
                };
            }
        }

        router
    }

    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(level = "trace", fields(error), skip_all)
    )]
    fn halyard_routes_with_handler<H, T>(
        self,
        paths: Vec<AxumRouteListing>,
        handler: H,
    ) -> Self
    where
        H: axum::handler::Handler<T, S>,
        T: 'static,
    {
        let mut router = self;
        let mut routes = RouteRegistry::default();
        for listing in paths.iter().filter(|p| !p.exclude) {
            for method in listing.methods() {
                router = add_route(
                    router,
                    &mut routes,
                    listing.path(),
                    &http_method(method),
                    |filter| on(filter, handler.clone()),
                );
            }
        }
        router
    }
}

/// Routes `method` at `path` with the method router that `method_router` builds, unless
/// axum would panic on it (see [`RouteRegistry::admit`]): then logs why and leaves the
/// router as it is.
fn add_route<S>(
    router: axum::Router<S>,
    routes: &mut RouteRegistry,
    path: &str,
    method: &Method,
    method_router: impl FnOnce(MethodFilter) -> MethodRouter<S>,
) -> axum::Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    match routes.admit(path, method) {
        Ok(filter) => router.route(path, method_router(filter)),
        Err(error) => {
            report(&error, None);
            router
        }
    }
}

fn http_method(method: halyard_router::Method) -> Method {
    match method {
        halyard_router::Method::Get => Method::GET,
        halyard_router::Method::Post => Method::POST,
        halyard_router::Method::Put => Method::PUT,
        halyard_router::Method::Delete => Method::DELETE,
        halyard_router::Method::Patch => Method::PATCH,
    }
}

/// A reasonable handler for serving static files (like JS/WASM/CSS) and 404 errors.
///
/// This is provided as a convenience, but is a fairly simple function. If you need to adapt it,
/// simply reuse the source code of this function in your own application.  A more compositional
/// implementation is offered by [`ErrorHandler`] as it implements a tower [`Service`] which
/// may be composed with other tower services.
///
/// [`Service`]: tower::Service
#[cfg(feature = "default")]
pub fn file_and_error_handler_with_context<S, IV>(
    additional_context: impl Fn() + 'static + Clone + Send,
    shell: impl Fn(HalyardOptions) -> IV + 'static + Clone + Send,
) -> impl Fn(
    Uri,
    State<S>,
    Request<Body>,
) -> Pin<Box<dyn Future<Output = Response<Body>> + Send + 'static>>
       + Clone
       + Send
       + 'static
where
    IV: IntoView + 'static,
    S: Send + Sync + Clone + 'static,
    HalyardOptions: FromRef<S>,
{
    move |uri: Uri, State(state): State<S>, req: Request<Body>| {
        Box::pin({
            let additional_context = additional_context.clone();
            let shell = shell.clone();
            async move {
                let options = HalyardOptions::from_ref(&state);
                let res =
                    get_static_file(uri, &options.site_root, req.headers())
                        .await;

                if res.status() == StatusCode::OK {
                    let owner = Owner::new();
                    owner.with(|| {
                        additional_context();
                        let res = res.into_response();
                        if let Some(response_options) =
                            use_context::<ResponseOptions>()
                        {
                            let mut res = AxumResponse(res);
                            res.extend_response(&response_options);
                            res.0
                        } else {
                            res
                        }
                    })
                } else {
                    let mut res = handle_response_inner(
                        move || {
                            provide_context(state.clone());
                            additional_context();
                        },
                        move || shell(options),
                        req,
                        |app, chunks, _supports_ooo| {
                            Box::pin(async move {
                                let app = if cfg!(feature = "islands-router") {
                                    app.to_html_stream_in_order_branching()
                                } else {
                                    app.to_html_stream_in_order()
                                };
                                let app = app.collect::<String>().await;
                                let chunks = chunks();
                                Box::pin(once(async move { app }).chain(chunks))
                                    as PinnedStream<String>
                            })
                        },
                    )
                    .await;

                    // set the status to 404
                    // but if the status was already set (for example, to a 302 redirect) don't
                    // overwrite it
                    let status = res.status_mut();
                    if *status == StatusCode::OK {
                        *res.status_mut() = StatusCode::NOT_FOUND;
                    }

                    res
                }
            }
        })
    }
}

/// A reasonable handler for serving static files (like JS/WASM/CSS) and 404 errors.
///
/// This is provided as a convenience, but is a fairly simple function. If you need to adapt it,
/// simply reuse the source code of this function in your own application.  A more compositional
/// implementation is offered by [`ErrorHandler`] as it implements a tower [`Service`] which
/// may be composed with other tower services.
///
/// [`Service`]: tower::Service
#[cfg(feature = "default")]
pub fn file_and_error_handler<S, IV>(
    shell: impl Fn(HalyardOptions) -> IV + 'static + Clone + Send,
) -> impl Fn(
    Uri,
    State<S>,
    Request<Body>,
) -> Pin<Box<dyn Future<Output = Response<Body>> + Send + 'static>>
       + Clone
       + Send
       + 'static
where
    IV: IntoView + 'static,
    S: Send + Sync + Clone + 'static,
    HalyardOptions: FromRef<S>,
{
    file_and_error_handler_with_context(move || (), shell)
}

#[cfg(feature = "default")]
async fn get_static_file(
    uri: Uri,
    root: &str,
    headers: &HeaderMap<HeaderValue>,
) -> Response<Body> {
    use axum::http::header::ACCEPT_ENCODING;

    let mut req = Request::new(Body::empty());
    *req.uri_mut() = uri;
    if let Some(value) = headers.get(ACCEPT_ENCODING) {
        req.headers_mut().insert(ACCEPT_ENCODING, value.clone());
    }

    // `ServeDir` implements `tower::Service` so we can call it with `tower::ServiceExt::oneshot`
    // This path is relative to the cargo root
    match ServeDir::new(root)
        .precompressed_gzip()
        .precompressed_br()
        .oneshot(req)
        .await
    {
        Ok(res) => res.into_response(),
        // `ServeDir` answers every request, with an error status if need be
        Err(never) => match never {},
    }
}

/// A helper to create a [`ServeDir`] service for the static files under
/// `HALYARD_SITE_ROOT`.  This may be further configured before being assigned
/// as the fallback service, or be attached as a service route on the router,
/// typically with the path derived from [`site_pkg_dir_service_route_path`].
///
/// [`ServeDir`]: tower_http::services::ServeDir
#[cfg(feature = "default")]
pub fn site_pkg_dir_service(options: &HalyardOptions) -> ServeDir {
    ServeDir::new(&*options.site_root)
        .precompressed_gzip()
        .precompressed_br()
}

/// A helper for constructing the axum route path from the `HalyardOptions`, can be used
/// in conjunction with the [`ServeDir`] service produced by [`site_pkg_dir_service`]
/// for setting up a routed site pkg service with [`Router::route_service`].
///
/// [`ServeDir`]: tower_http::services::ServeDir
pub fn site_pkg_dir_service_route_path(options: &HalyardOptions) -> String {
    // The path of the route being built will be constained to serve only the
    // contents of `site_pkg_dir` to avoid conflicts with the root routes.
    let mut path = String::new();
    // While it shouldn't start with a '/', but check anyway.
    if !options.site_pkg_dir.starts_with('/') {
        path.push('/');
    }
    path.push_str(&options.site_pkg_dir);
    if !path.ends_with('/') {
        path.push('/');
    }
    path.push_str("{*path}");
    path
}
