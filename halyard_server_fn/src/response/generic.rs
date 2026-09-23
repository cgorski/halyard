//! This module uses platform-agnostic abstractions
//! allowing users to run server functions on a wide range of
//! platforms.
//!
//! The crates in use in this crate are:
//!
//! * `bytes`: platform-agnostic manipulation of bytes.
//! * `http`: low-dependency HTTP abstractions' *front-end*.
//!
//! # Users
//!
//! * `wasm32-wasip*` integration crate `halyard_wasi` is using this
//!   crate under the hood.

use super::{Res, TryRes};
use crate::error::{
    FromServerFnError, IntoAppError, ServerFnErrorErr, ServerFnErrorWrapper,
    SERVER_FN_ERROR_HEADER_NAME,
};
use bytes::Bytes;
use futures::{Stream, TryStreamExt};
use halyard_throw_error::Error;
use http::{header, HeaderValue, Response, StatusCode};
use std::pin::Pin;

/// The Body of a Response whose *execution model* can be
/// customised using the variants.
pub enum Body {
    /// The response body will be written synchronously.
    Sync(Bytes),

    /// The response body will be written asynchronously,
    /// this execution model is also known as
    /// "streaming".
    Async(Pin<Box<dyn Stream<Item = Result<Bytes, Error>> + Send + 'static>>),
}

impl From<String> for Body {
    fn from(value: String) -> Self {
        Body::Sync(Bytes::from(value))
    }
}

impl From<Bytes> for Body {
    fn from(value: Bytes) -> Self {
        Body::Sync(value)
    }
}

impl<E> TryRes<E> for Response<Body>
where
    E: Send + Sync + FromServerFnError,
{
    fn try_from_string(content_type: &str, data: String) -> Result<Self, E> {
        let builder = http::Response::builder();
        builder
            .status(200)
            .header(http::header::CONTENT_TYPE, content_type)
            .body(data.into())
            .map_err(|e| {
                ServerFnErrorErr::Response(e.to_string()).into_app_error()
            })
    }

    fn try_from_bytes(content_type: &str, data: Bytes) -> Result<Self, E> {
        let builder = http::Response::builder();
        builder
            .status(200)
            .header(http::header::CONTENT_TYPE, content_type)
            .body(Body::Sync(data))
            .map_err(|e| {
                ServerFnErrorErr::Response(e.to_string()).into_app_error()
            })
    }

    fn try_from_stream(
        content_type: &str,
        data: impl Stream<Item = Result<Bytes, Bytes>> + Send + 'static,
    ) -> Result<Self, E> {
        let builder = http::Response::builder();
        builder
            .status(200)
            .header(http::header::CONTENT_TYPE, content_type)
            .body(Body::Async(Box::pin(
                data.map_err(|e| ServerFnErrorWrapper(E::de(e)))
                    .map_err(Error::from),
            )))
            .map_err(|e| {
                ServerFnErrorErr::Response(e.to_string()).into_app_error()
            })
    }
}

impl Res for Response<Body> {
    /// A `500` with `err` as its body. The server function's `path` goes in the
    /// [`SERVER_FN_ERROR_HEADER`](crate::error::SERVER_FN_ERROR_HEADER) header, unless
    /// it is not a valid header value.
    fn error_response(path: &str, err: Bytes) -> Self {
        let mut response = Response::new(Body::from(err));
        *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
        if let Ok(path) = HeaderValue::from_str(path) {
            response
                .headers_mut()
                .insert(SERVER_FN_ERROR_HEADER_NAME, path);
        }
        response
    }

    fn content_type(&mut self, content_type: &str) {
        if let Ok(content_type) = HeaderValue::from_str(content_type) {
            self.headers_mut()
                .insert(header::CONTENT_TYPE, content_type);
        }
    }

    fn redirect(&mut self, path: &str) {
        if let Ok(path) = HeaderValue::from_str(path) {
            self.headers_mut().insert(header::LOCATION, path);
            *self.status_mut() = StatusCode::FOUND;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::SERVER_FN_ERROR_HEADER;

    /// The error response used to `unwrap()` the builder, which fails on a path that
    /// is not a valid header value.
    #[test]
    fn error_response_for_a_path_that_is_not_a_header_value() {
        let response = Response::<Body>::error_response(
            "/api/bad\npath",
            Bytes::from_static(b"boom"),
        );

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(response.headers().get(SERVER_FN_ERROR_HEADER).is_none());
        match response.into_body() {
            Body::Sync(body) => assert_eq!(body, "boom"),
            Body::Async(_) => panic!("an error response has a complete body"),
        }
    }

    #[test]
    fn error_response_names_the_server_fn() {
        let response = Response::<Body>::error_response(
            "/api/f",
            Bytes::from_static(b"boom"),
        );

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            response.headers().get(SERVER_FN_ERROR_HEADER).unwrap(),
            "/api/f"
        );
    }
}
