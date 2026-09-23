use super::{Encoding, FromReq};
use crate::{
    error::{
        FromServerFnError, IntoAppError, ServerFnErrorErr, ServerFnErrorWrapper,
    },
    request::{browser::BrowserFormData, ClientReq, Req},
    ContentType, IntoReq,
};
use futures::StreamExt;
use http::Method;
use multer::Multipart;
use web_sys::FormData;

/// Encodes multipart form data.
///
/// You should primarily use this if you are trying to handle file uploads.
pub struct MultipartFormData;

impl ContentType for MultipartFormData {
    const CONTENT_TYPE: &'static str = "multipart/form-data";
}

impl Encoding for MultipartFormData {
    const METHOD: Method = Method::POST;
}

/// Describes whether the multipart data is on the client side or the server side.
#[derive(Debug)]
pub enum MultipartData {
    /// `FormData` from the browser.
    Client(BrowserFormData),
    /// Generic multipart form using [`multer`]. This implements [`Stream`](futures::Stream).
    Server(multer::Multipart<'static>),
}

impl MultipartData {
    /// Extracts the inner data to handle as a stream.
    ///
    /// On the server side, this always returns `Some(_)`. On the client side, always returns `None`.
    pub fn into_inner(self) -> Option<Multipart<'static>> {
        match self {
            MultipartData::Client(_) => None,
            MultipartData::Server(data) => Some(data),
        }
    }

    /// Extracts the inner form data on the client side.
    ///
    /// On the server side, this always returns `None`. On the client side, always returns `Some(_)`.
    pub fn into_client_data(self) -> Option<BrowserFormData> {
        match self {
            MultipartData::Client(data) => Some(data),
            MultipartData::Server(_) => None,
        }
    }
}

impl From<FormData> for MultipartData {
    fn from(value: FormData) -> Self {
        MultipartData::Client(value.into())
    }
}

/// Why a multipart request could not be sent or read.
#[derive(Debug, thiserror::Error)]
enum MultipartError {
    /// The client was given the server's multipart stream to send.
    #[error(
        "only browser `FormData` can be sent as multipart data, not a \
         received multipart stream"
    )]
    NotClientData,
    /// The request has no `Content-Type` header.
    #[error("the multipart request has no Content-Type header")]
    NoContentType,
    /// The `Content-Type` header has no usable boundary.
    #[error(
        "the multipart request's Content-Type {content_type:?} has no \
         boundary ({reason})"
    )]
    NoBoundary {
        content_type: String,
        reason: multer::Error,
    },
}

impl<E: FromServerFnError, T, Request> IntoReq<MultipartFormData, Request, E>
    for T
where
    Request: ClientReq<E, FormData = BrowserFormData>,
    T: Into<MultipartData>,
{
    fn into_req(self, path: &str, accepts: &str) -> Result<Request, E> {
        let form_data = self.into().into_client_data().ok_or_else(|| {
            ServerFnErrorErr::Serialization(
                MultipartError::NotClientData.to_string(),
            )
            .into_app_error()
        })?;
        Request::try_new_post_multipart(path, accepts, form_data)
    }
}

/// The multipart boundary named in a request's `Content-Type`.
fn boundary(content_type: Option<&str>) -> Result<String, MultipartError> {
    let content_type = content_type.ok_or(MultipartError::NoContentType)?;
    multer::parse_boundary(content_type).map_err(|reason| {
        MultipartError::NoBoundary {
            content_type: content_type.to_owned(),
            reason,
        }
    })
}

impl<E, T, Request> FromReq<MultipartFormData, Request, E> for T
where
    Request: Req<E> + Send + 'static,
    T: From<MultipartData>,
    E: FromServerFnError + Send + Sync,
{
    async fn from_req(req: Request) -> Result<Self, E> {
        let boundary =
            boundary(req.to_content_type().as_deref()).map_err(|error| {
                ServerFnErrorErr::Args(error.to_string()).into_app_error()
            })?;
        let stream = req.try_into_stream()?;
        let data = multer::Multipart::new(
            stream.map(|data| data.map_err(|e| ServerFnErrorWrapper(E::de(e)))),
            boundary,
        );
        Ok(MultipartData::Server(data).into())
    }
}

#[cfg(all(test, feature = "generic"))]
mod tests {
    use super::*;
    use crate::ServerFnError;
    use bytes::Bytes;

    fn from_req(
        content_type: Option<&str>,
    ) -> Result<MultipartData, ServerFnError> {
        let mut req = http::Request::builder().method(Method::POST);
        if let Some(content_type) = content_type {
            req = req.header(http::header::CONTENT_TYPE, content_type);
        }
        let req = req.body(Bytes::from_static(b"--x--")).unwrap();
        futures::executor::block_on(<MultipartData as FromReq<
            MultipartFormData,
            _,
            ServerFnError,
        >>::from_req(req))
    }

    fn args_error(result: Result<MultipartData, ServerFnError>) -> String {
        match result {
            Err(ServerFnError::Args(message)) => message,
            other => panic!("expected an argument error, got {other:?}"),
        }
    }

    /// Any client can send these; they used to panic the request handler.
    #[test]
    fn multipart_request_without_a_boundary_is_an_argument_error() {
        let message = args_error(from_req(Some("multipart/form-data")));
        assert!(message.contains("multipart/form-data"), "{message}");

        let message = args_error(from_req(None));
        assert!(message.contains("Content-Type"), "{message}");
    }

    #[test]
    fn multipart_request_with_a_boundary_is_read() {
        let data = from_req(Some("multipart/form-data; boundary=x")).unwrap();

        assert!(data.into_inner().is_some());
    }

    struct NoRequest;

    impl ClientReq<ServerFnError> for NoRequest {
        type FormData = BrowserFormData;

        fn try_new_req_query(
            _: &str,
            _: &str,
            _: &str,
            _: &str,
            _: Method,
        ) -> Result<Self, ServerFnError> {
            Ok(NoRequest)
        }

        fn try_new_req_text(
            _: &str,
            _: &str,
            _: &str,
            _: String,
            _: Method,
        ) -> Result<Self, ServerFnError> {
            Ok(NoRequest)
        }

        fn try_new_req_bytes(
            _: &str,
            _: &str,
            _: &str,
            _: Bytes,
            _: Method,
        ) -> Result<Self, ServerFnError> {
            Ok(NoRequest)
        }

        fn try_new_req_form_data(
            _: &str,
            _: &str,
            _: &str,
            _: Self::FormData,
            _: Method,
        ) -> Result<Self, ServerFnError> {
            Ok(NoRequest)
        }

        fn try_new_req_multipart(
            _: &str,
            _: &str,
            _: Self::FormData,
            _: Method,
        ) -> Result<Self, ServerFnError> {
            Ok(NoRequest)
        }

        fn try_new_req_streaming(
            _: &str,
            _: &str,
            _: &str,
            _: impl futures::Stream<Item = Bytes> + Send + 'static,
            _: Method,
        ) -> Result<Self, ServerFnError> {
            Ok(NoRequest)
        }
    }

    /// Only browser `FormData` can be sent; the server's multipart stream used to
    /// panic here.
    #[test]
    fn multipart_data_from_the_server_cannot_be_sent() {
        let data = MultipartData::Server(multer::Multipart::new(
            futures::stream::empty::<Result<Bytes, std::io::Error>>(),
            "x",
        ));

        let sent = <MultipartData as IntoReq<
            MultipartFormData,
            NoRequest,
            ServerFnError,
        >>::into_req(data, "/api/upload", "application/json");

        assert!(matches!(sent, Err(ServerFnError::Serialization(_))));
    }
}
