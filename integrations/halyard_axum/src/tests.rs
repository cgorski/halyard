//! Requests and route lists that used to panic, driven through the public handlers.

use crate::*;
use axum::{
    body::to_bytes,
    extract::State,
    http::header::{CONTENT_TYPE, LOCATION},
    Router,
};
use halyard_integration_utils::ExtendResponse;
use halyard_router::{
    components::{Route, Router as HalyardRouter, Routes},
    static_routes::StaticRoute,
    ParamSegment, StaticSegment,
};
use tempfile::TempDir;
use tower::ServiceExt;

fn options() -> HalyardOptions {
    HalyardOptions::builder()
        .output_name("halyard_axum_test")
        .build()
}

fn options_with_site_root(site_root: &TempDir) -> HalyardOptions {
    HalyardOptions::builder()
        .output_name("halyard_axum_test")
        .site_root(site_root.path().to_str().expect("a UTF-8 temporary path"))
        .build()
}

fn page() -> &'static str {
    "the page"
}

fn listing(path: &str, mode: SsrMode) -> AxumRouteListing {
    AxumRouteListing::new(
        path.to_owned(),
        mode,
        [halyard_router::Method::Get],
        vec![],
    )
}

fn request(method: Method, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("x-request-id", "req-42")
        .body(Body::empty())
        .expect("a valid test request")
}

async fn send(router: Router, method: Method, uri: &str) -> Response<Body> {
    match router.oneshot(request(method, uri)).await {
        Ok(response) => response,
        Err(never) => match never {},
    }
}

async fn text(response: Response<Body>) -> String {
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("a readable body");
    String::from_utf8(bytes.to_vec()).expect("a UTF-8 body")
}

/// An authority-form target (`CONNECT host:port`) has no path; rendering it unwrapped the
/// missing path and panicked.
#[tokio::test]
async fn a_request_without_a_path_gets_400() {
    let request = request(Method::CONNECT, "example.com:443");
    assert!(request.uri().path_and_query().is_none());

    let response = render_app_to_stream(page)(request).await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(text(response).await, "Bad Request (request id req-42)\n");
}

/// The same request, through the fallback handler the application mounts.
#[tokio::test]
async fn the_file_and_error_handler_answers_a_request_without_a_path_with_400()
{
    let site_root = TempDir::new().expect("a temporary directory");
    let app = Router::new()
        .fallback(file_and_error_handler_with_context::<HalyardOptions, _>(
            || {},
            |_| page(),
        ))
        .with_state(options_with_site_root(&site_root));

    let response = send(app, Method::CONNECT, "example.com:443").await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(text(response).await, "Bad Request (request id req-42)\n");
}

/// `render_route` needs axum's `MatchedPath`, which a handler called outside a routed path
/// does not have; it used to `expect` it.
#[tokio::test]
async fn render_route_without_a_matched_path_gets_500() {
    let handler = render_route::<HalyardOptions, _>(
        vec![listing("/a", SsrMode::OutOfOrder)],
        page,
    );

    let response = handler(State(options()), request(Method::GET, "/a")).await;

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        text(response).await,
        "Internal Server Error (request id req-42)\n"
    );
}

/// A route mounted with `render_route` but missing from its route list panicked.
#[tokio::test]
async fn render_route_for_a_route_missing_from_its_list_gets_500() {
    let app = Router::new()
        .route(
            "/b",
            axum::routing::get(render_route::<HalyardOptions, _>(
                vec![listing("/a", SsrMode::OutOfOrder)],
                page,
            )),
        )
        .with_state(options());

    let response = send(app, Method::GET, "/b").await;

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        text(response).await,
        "Internal Server Error (request id req-42)\n"
    );
}

/// Runs `f` with the contexts that a request handler provides to `redirect`.
fn with_request_contexts(accept: &str, f: impl FnOnce(&ResponseOptions)) {
    let owner = Owner::new();
    owner.with(|| {
        let (parts, ()) = Request::builder()
            .header(ACCEPT, accept)
            .header("x-request-id", "req-42")
            .body(())
            .expect("a valid test request")
            .into_parts();
        provide_context(parts);
        let response_options = ResponseOptions::default();
        provide_context(response_options.clone());
        f(&response_options);
    });
}

/// A redirect to a location with a line break (which would start another header) used to
/// panic building the `Location` header. It is refused, with a 500.
#[test]
fn redirect_refuses_a_location_that_is_not_a_header_value() {
    with_request_contexts("text/html", |response_options| {
        redirect("/next\r\nSet-Cookie: session=stolen");

        let parts = response_options.0.read().or_poisoned();
        assert_eq!(parts.status, Some(StatusCode::INTERNAL_SERVER_ERROR));
        assert!(parts.headers.is_empty(), "{:?}", parts.headers);
    });
}

#[test]
fn redirect_sets_the_location_and_a_302_for_a_page_request() {
    with_request_contexts("text/html", |response_options| {
        redirect("/next?a=b");

        let parts = response_options.0.read().or_poisoned();
        assert_eq!(parts.status, Some(StatusCode::FOUND));
        assert_eq!(
            parts.headers.get(LOCATION).map(|v| v.as_bytes()),
            Some(&b"/next?a=b"[..])
        );
    });
}

/// A request that does not accept HTML gets the `Location` header only: no status, and no
/// other header.
#[test]
fn redirect_sets_only_the_location_for_a_request_that_is_not_a_page() {
    with_request_contexts("application/json", |response_options| {
        redirect("/next");

        let parts = response_options.0.read().or_poisoned();
        assert_eq!(parts.status, None);
        assert_eq!(
            parts.headers.get(LOCATION).map(|v| v.as_bytes()),
            Some(&b"/next"[..])
        );
        assert_eq!(parts.headers.len(), 1, "{:?}", parts.headers);
    });
}

/// An invalid default content type used to panic; the response goes out without one.
#[test]
fn an_invalid_default_content_type_is_left_out() {
    let mut response = AxumResponse(Response::new(Body::empty()));

    response.set_default_content_type("text/html\n");
    assert_eq!(response.0.headers().get(CONTENT_TYPE), None);

    response.set_default_content_type("text/html; charset=utf-8");
    assert_eq!(
        response.0.headers().get(CONTENT_TYPE).map(|v| v.as_bytes()),
        Some(&b"text/html; charset=utf-8"[..])
    );
}

/// `was_404` used `expect_context`, a panic when the render provided no `ResponseOptions`.
#[test]
fn a_render_without_response_options_was_not_a_404() {
    assert!(!was_404(&Owner::new()));
}

/// The static renderer unwrapped the render's shared context. Without one, nothing was
/// deferred and there is nothing to wait for.
#[test]
fn awaiting_deferred_data_without_a_shared_context_returns() {
    futures::executor::block_on(await_deferred(&Owner::new()));
}

/// Two listings with one path and method made axum panic ("Overlapping method route"),
/// e.g. `/posts/:id?` next to `/posts`. The first one is kept.
#[tokio::test]
async fn halyard_routes_keeps_the_first_of_two_routes_with_one_path_and_method()
{
    let options = options();
    let app = Router::new()
        .halyard_routes(
            &options,
            vec![
                listing("/a", SsrMode::OutOfOrder),
                listing("/a", SsrMode::Async),
            ],
            page,
        )
        .with_state(options);

    let response = send(app, Method::GET, "/a").await;

    assert_eq!(response.status(), StatusCode::OK);
    assert!(text(response).await.contains("the page"));
}

/// Paths axum cannot route made it panic while the router was built; they are left out.
#[tokio::test]
async fn halyard_routes_leaves_out_paths_axum_cannot_route() {
    let options = options();
    let app = Router::new()
        .halyard_routes(
            &options,
            ["", "no-slash", "/{}", "/a{b", "/:id", "/{*rest}/x", "/ok"]
                .into_iter()
                .map(|path| listing(path, SsrMode::OutOfOrder))
                .collect(),
            page,
        )
        .with_state(options);

    let response = send(app, Method::GET, "/ok").await;

    assert_eq!(response.status(), StatusCode::OK);
    assert!(text(response).await.contains("the page"));
}

/// axum cannot hold two routes that differ only in a parameter's name.
#[tokio::test]
async fn halyard_routes_leaves_out_a_route_that_differs_only_in_parameter_names(
) {
    let options = options();
    let app = Router::new()
        .halyard_routes(
            &options,
            vec![
                listing("/u/{id}", SsrMode::OutOfOrder),
                listing("/u/{name}", SsrMode::OutOfOrder),
            ],
            page,
        )
        .with_state(options);

    let response = send(app, Method::GET, "/u/7").await;

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn halyard_routes_with_handler_keeps_the_first_of_two_identical_routes() {
    let app = Router::new()
        .halyard_routes_with_handler(
            vec![
                listing("/a", SsrMode::OutOfOrder),
                listing("/a", SsrMode::OutOfOrder),
                listing("/a{b", SsrMode::OutOfOrder),
            ],
            || async { "handled" },
        )
        .with_state(options());

    let response = send(app, Method::GET, "/a").await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(text(response).await, "handled");
}

#[component]
fn OddPaths() -> impl IntoView {
    view! {
        <HalyardRouter>
            <Routes fallback=|| "not found">
                <Route path=StaticSegment("a{b") view=|| "brace"/>
                <Route path=(StaticSegment("x"), ParamSegment("")) view=|| "unnamed"/>
                <Route path=StaticSegment("ok") view=|| "ok"/>
            </Routes>
        </HalyardRouter>
    }
}

/// A brace in a static segment reached axum as the start of a parameter, and a lone one
/// made it panic. It is escaped, as axum writes a literal brace; a parameter without a name
/// (which axum cannot route) is left out.
#[tokio::test]
async fn a_route_list_with_odd_paths_builds_a_router() {
    let routes = generate_route_list(OddPaths);
    let paths = routes.iter().map(|r| r.path()).collect::<Vec<_>>();
    assert!(paths.contains(&"/a{{b"), "{paths:?}");
    assert!(paths.contains(&"/x{}"), "{paths:?}");
    assert!(paths.contains(&"/ok"), "{paths:?}");

    let options = options();
    let app = Router::new()
        .halyard_routes(&options, routes, OddPaths)
        .with_state(options);

    let response = send(app, Method::GET, "/ok").await;

    assert_eq!(response.status(), StatusCode::OK);
    assert!(text(response).await.contains("ok"));
}

/// The registration of every rendering mode, rewritten without `unreachable!`, still routes
/// each mode and method to its renderer.
#[tokio::test]
async fn every_ssr_mode_is_routed() {
    let options = options();
    let post = AxumRouteListing::new(
        "/post".to_owned(),
        SsrMode::OutOfOrder,
        [halyard_router::Method::Post],
        vec![],
    );
    let app = Router::new()
        .halyard_routes(
            &options,
            vec![
                listing("/out-of-order", SsrMode::OutOfOrder),
                listing("/partially-blocked", SsrMode::PartiallyBlocked),
                listing("/in-order", SsrMode::InOrder),
                listing("/async", SsrMode::Async),
                post,
            ],
            page,
        )
        .with_state(options);

    for path in ["/out-of-order", "/partially-blocked", "/in-order", "/async"] {
        let response = send(app.clone(), Method::GET, path).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert!(text(response).await.contains("the page"), "{path}");
    }
    let response = send(app.clone(), Method::POST, "/post").await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = send(app, Method::GET, "/post").await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
}

/// A static route is rendered on the first request, written under the site root, and
/// served from the file after that.
#[tokio::test]
async fn a_static_route_is_rendered_written_and_served() {
    let site_root = TempDir::new().expect("a temporary directory");
    let options = options_with_site_root(&site_root);
    let app = Router::new()
        .halyard_routes(
            &options,
            vec![listing("/static", SsrMode::Static(StaticRoute::new()))],
            page,
        )
        .with_state(options);

    let first = send(app.clone(), Method::GET, "/static").await;
    assert_eq!(first.status(), StatusCode::OK);
    assert!(text(first).await.contains("the page"));
    let written = std::fs::read_to_string(site_root.path().join("static.html"))
        .expect("the rendered page is written");
    assert!(written.contains("the page"));

    let second = send(app, Method::GET, "/static").await;
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(text(second).await, written);
}
