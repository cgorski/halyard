//! What `halyard_axum` does instead of panicking: typed errors, logged, and on the request
//! path an error response.

use crate::route_path::PathError;
use axum::{
    body::Body,
    http::{HeaderMap, Method, Response, StatusCode},
};
use std::fmt::Display;

/// Why a request was answered with an error instead of the application.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum RequestError {
    /// An authority-form target (`CONNECT host:port`) names no page to render.
    #[error("the request target {target} has no path, so there is no page to render")]
    NoPath { target: String },
    #[error(
        "a `render_route` handler got a request that axum did not route to it by path \
         (no `MatchedPath`); mount it with `Router::route` or \
         `halyard_routes_with_handler`, not as a fallback"
    )]
    NoMatchedPath,
    #[error(
        "a `render_route` handler got a request for the route {path}, which is not in \
         the route list it was given"
    )]
    UnknownRoute { path: String },
    #[cfg(not(feature = "default"))]
    #[error("static routes are not supported on WASM32 server targets")]
    StaticRoutesUnsupported,
    /// A CR or LF in a `Location` would start another header, so the redirect is refused.
    #[error(
        "`redirect` was given the location {location:?}, which is not a valid \
         `Location` header value (it contains a control character); not redirecting"
    )]
    InvalidRedirect { location: String },
    #[error(
        "the content type {content_type:?} is not a valid header value; the response \
         is sent without one"
    )]
    InvalidContentType { content_type: String },
}

impl RequestError {
    fn status(&self) -> StatusCode {
        match self {
            RequestError::NoPath { .. } => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// A route that `HalyardRoutes` leaves out of the router, rather than let
/// `axum::Router::route` panic on it at startup.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum RouteError {
    #[error(
        "skipping the route {path:?}: axum cannot route it, because {reason}"
    )]
    InvalidPath { path: String, reason: PathError },
    #[error(
        "skipping {method} {path}: axum's router has no method filter for {method}"
    )]
    UnsupportedMethod { path: String, method: Method },
    #[error(
        "skipping the second {method} {path}: it is already routed (the route list or \
         the server functions have this path and method twice)"
    )]
    Duplicate { path: String, method: Method },
    #[error(
        "skipping the route {path}: axum cannot tell it apart from the route {existing} \
         (the same path up to parameter names, or a parameter and a catch-all in the \
         same place)"
    )]
    Conflict { path: String, existing: String },
    #[cfg(not(feature = "default"))]
    #[error(
        "skipping the static route {path}: static routes are not supported on WASM32 \
         server targets"
    )]
    StaticRoutesUnsupported { path: String },
    #[cfg(not(feature = "default"))]
    #[error(
        "static routes are not supported on WASM32 server targets; no static pages \
         were generated"
    )]
    StaticGenerationUnsupported,
}

/// The request's `x-request-id`, if it has a readable one.
pub(crate) fn request_id(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
}

/// Logs an error that `halyard_axum` recovered from: with `tracing` when that feature is
/// on, otherwise on standard error, as the crate's other diagnostics are.
pub(crate) fn report(error: &dyn Display, request_id: Option<&str>) {
    #[cfg(feature = "tracing")]
    tracing::error!(request_id, "{error}");
    #[cfg(not(feature = "tracing"))]
    {
        use std::io::Write;
        // `eprintln!` panics if standard error is closed; with nowhere left to report the
        // error, it is dropped
        _ = match request_id {
            Some(id) => writeln!(
                std::io::stderr(),
                "[halyard_axum] {error} (request id {id})"
            ),
            None => writeln!(std::io::stderr(), "[halyard_axum] {error}"),
        };
    }
}

/// Logs `error` and answers the request with its status and a short plain-text body that
/// names the request id, if there is one. The error's details stay in the log.
pub(crate) fn error_response(
    error: &RequestError,
    headers: &HeaderMap,
) -> Response<Body> {
    let request_id = request_id(headers);
    report(error, request_id);
    let status = error.status();
    let reason = status.canonical_reason().unwrap_or("Error");
    let body = match request_id {
        Some(id) => format!("{reason} (request id {id})\n"),
        None => format!("{reason}\n"),
    };
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        const {
            axum::http::HeaderValue::from_static("text/plain; charset=utf-8")
        },
    );
    response
}
