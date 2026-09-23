/// Response types for Actix.
#[cfg(feature = "actix-no-default")]
pub mod actix;
/// Response types for the browser.
#[cfg(feature = "browser")]
pub mod browser;
#[cfg(feature = "generic")]
pub mod generic;
/// Response types for Axum.
#[cfg(feature = "axum-no-default")]
pub mod http;
/// Response types for [`reqwest`].
#[cfg(feature = "reqwest")]
pub mod reqwest;

use crate::{error::FromServerFnError, mock::NoServer};
use bytes::Bytes;
use futures::Stream;
use std::future::Future;

/// Represents the response as created by the server;
pub trait TryRes<E>
where
    Self: Sized,
{
    /// Attempts to convert a UTF-8 string into an HTTP response.
    fn try_from_string(content_type: &str, data: String) -> Result<Self, E>;

    /// Attempts to convert a binary blob represented as bytes into an HTTP response.
    fn try_from_bytes(content_type: &str, data: Bytes) -> Result<Self, E>;

    /// Attempts to convert a stream of bytes into an HTTP response.
    fn try_from_stream(
        content_type: &str,
        data: impl Stream<Item = Result<Bytes, Bytes>> + Send + 'static,
    ) -> Result<Self, E>;
}

/// Represents the response as created by the server;
pub trait Res {
    /// Converts an error into a response, with a `500` status code and the error as its body.
    fn error_response(path: &str, err: Bytes) -> Self;
    /// Set the `Content-Type` header for the response.
    fn content_type(&mut self, #[allow(unused_variables)] content_type: &str) {
        // TODO 0.9: remove this method and default implementation. It is only included here
        //  to allow setting the `Content-Type` header for error responses without requiring a
        //  semver-incompatible change.
    }
    /// Redirect the response by setting a 302 code and Location header.
    fn redirect(&mut self, path: &str);
}

/// Represents the response as received by the client.
pub trait ClientRes<E> {
    /// Attempts to extract a UTF-8 string from an HTTP response.
    fn try_into_string(self) -> impl Future<Output = Result<String, E>> + Send;

    /// Attempts to extract a binary blob from an HTTP response.
    fn try_into_bytes(self) -> impl Future<Output = Result<Bytes, E>> + Send;

    /// Attempts to extract a binary stream from an HTTP response.
    fn try_into_stream(
        self,
    ) -> Result<
        impl Stream<Item = Result<Bytes, Bytes>> + Send + Sync + 'static,
        E,
    >;

    /// HTTP status code of the response.
    fn status(&self) -> u16;

    /// Status text for the status code.
    fn status_text(&self) -> String;

    /// The `Location` header or (if none is set), the URL of the response.
    fn location(&self) -> String;

    /// Whether the response has the [`REDIRECT_HEADER`](crate::redirect::REDIRECT_HEADER) set.
    fn has_redirect(&self) -> bool;
}

/// A mocked response type that can be used in place of the actual server response,
/// when compiling for the browser.
///
/// It carries nothing. Building one from a server function's output returns an error
/// (this build has no server to send it); an error response is an empty
/// `BrowserMockRes`, on which setting headers does nothing.
pub struct BrowserMockRes;

impl<E: FromServerFnError> TryRes<E> for BrowserMockRes {
    fn try_from_string(_content_type: &str, _data: String) -> Result<Self, E> {
        Err(NoServer::Response.into_app_error())
    }

    fn try_from_bytes(_content_type: &str, _data: Bytes) -> Result<Self, E> {
        Err(NoServer::Response.into_app_error())
    }

    fn try_from_stream(
        _content_type: &str,
        _data: impl Stream<Item = Result<Bytes, Bytes>>,
    ) -> Result<Self, E> {
        Err(NoServer::Response.into_app_error())
    }
}

impl Res for BrowserMockRes {
    fn error_response(_path: &str, _err: Bytes) -> Self {
        BrowserMockRes
    }

    fn content_type(&mut self, _content_type: &str) {}

    fn redirect(&mut self, _path: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ServerFnError;

    fn is_response_error<T>(result: Result<T, ServerFnError>) -> bool {
        matches!(result, Err(ServerFnError::Response(_)))
    }

    /// These used to be `unreachable!()`: a panic in a build without a server.
    #[test]
    fn browser_mock_response_cannot_carry_a_server_function_response() {
        assert!(is_response_error(<BrowserMockRes as TryRes<
            ServerFnError,
        >>::try_from_string(
            "text/plain", "output".into()
        )));
        assert!(is_response_error(<BrowserMockRes as TryRes<
            ServerFnError,
        >>::try_from_bytes(
            "application/octet-stream",
            Bytes::from_static(b"output"),
        )));
        assert!(is_response_error(<BrowserMockRes as TryRes<
            ServerFnError,
        >>::try_from_stream(
            "application/octet-stream",
            futures::stream::empty(),
        )));
    }

    #[test]
    fn browser_mock_error_response_has_nothing_to_set() {
        let mut response = BrowserMockRes::error_response(
            "/api/f",
            Bytes::from_static(b"err"),
        );
        response.content_type("text/plain");
        response.redirect("/");
    }
}
