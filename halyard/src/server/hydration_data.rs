//! A resource's value on its way from the server to the browser, in the page (the hydration
//! data). When that fails, the value is left out and the failure logged, and the browser
//! loads the resource itself, as it does for any resource the page has no data for.

use crate::server::error::ResourceError;
#[cfg(any(feature = "hydration", test))]
use crate::server::FromEncodedStr;
#[cfg(any(feature = "ssr", test))]
use crate::server::{error::warn, IntoEncodedString};
#[cfg(any(feature = "hydration", test))]
use codee::Decoder;
#[cfg(any(feature = "ssr", test))]
use codee::Encoder;
use halyard_reactive_graph::hydration_context::SerializedDataId;
use std::{fmt::Debug, panic::Location};

/// What the server sends in place of a resource's value when it has none that it can send
/// (the value could not be serialized, or the resource had none). The browser recognises it
/// and loads the resource itself.
///
/// None of the codecs halyard provides produces it: JSON cannot start with `!`, and base64
/// has neither `!` nor spaces. `FromToStringCodec` could, for a string that is exactly this;
/// the browser then loads that value itself, which is correct, only slower.
pub(crate) const NOT_SENT: &str =
    "!halyard: the server sent no value for this resource";

/// `value` encoded with `Ser` for the page, or why it cannot be sent.
#[cfg(any(feature = "ssr", test))]
pub(crate) fn encode<T, Ser>(
    value: Option<&T>,
    id: &SerializedDataId,
    created_at: &'static Location<'static>,
) -> Result<String, ResourceError>
where
    Ser: Encoder<T>,
    <Ser as Encoder<T>>::Error: Debug,
    <Ser as Encoder<T>>::Encoded: IntoEncodedString,
{
    let id = id.clone().into_inner();
    let value = value.ok_or(ResourceError::NoValue { id, created_at })?;
    Ser::encode(value)
        .map(IntoEncodedString::into_encoded_string)
        .map_err(|error| ResourceError::Encode {
            id,
            created_at,
            reason: format!("{error:?}"),
        })
}

/// What the page carries for a resource: its encoded value, or (logged) [`NOT_SENT`].
#[cfg(any(feature = "ssr", test))]
pub(crate) fn for_the_page(encoded: Result<String, ResourceError>) -> String {
    encoded.unwrap_or_else(|error| {
        warn(&error);
        NOT_SENT.to_owned()
    })
}

/// The value that the page's `data` for a resource carries, or why there is none to use.
#[cfg(any(feature = "hydration", test))]
pub(crate) fn decode<T, Ser>(
    data: &str,
    id: &SerializedDataId,
    created_at: &'static Location<'static>,
) -> Result<T, ResourceError>
where
    Ser: Decoder<T>,
    <Ser as Decoder<T>>::Error: Debug,
    <Ser as Decoder<T>>::Encoded: FromEncodedStr,
    <<Ser as Decoder<T>>::Encoded as FromEncodedStr>::DecodingError: Debug,
{
    use std::borrow::Borrow;

    let id = id.clone().into_inner();
    if data == NOT_SENT {
        return Err(ResourceError::NotSent { id, created_at });
    }
    let encoded = <Ser as Decoder<T>>::Encoded::from_encoded_str(data)
        .map_err(|error| ResourceError::Unreadable {
            id,
            created_at,
            reason: format!("{error:?}"),
        })?;
    Ser::decode(encoded.borrow()).map_err(|error| ResourceError::Decode {
        id,
        created_at,
        reason: format!("{error:?}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use codee::{
        binary::FromToBytesCodec,
        string::{FromToStringCodec, JsonSerdeCodec},
    };
    use std::collections::HashMap;

    fn id() -> SerializedDataId {
        SerializedDataId::new(3)
    }

    /// A value round-trips through the page unchanged.
    #[test]
    fn a_value_round_trips() {
        let here = Location::caller();
        let map = HashMap::from([("a".to_owned(), 1_u8)]);

        let sent = encode::<_, JsonSerdeCodec>(Some(&map), &id(), here);
        let Ok(sent) = sent else {
            panic!("expected the map to serialize, got {sent:?}");
        };

        assert_eq!(
            decode::<HashMap<String, u8>, JsonSerdeCodec>(&sent, &id(), here),
            Ok(map)
        );
        let bytes = encode::<u32, FromToBytesCodec>(Some(&7), &id(), here);
        let Ok(bytes) = bytes else {
            panic!("expected a number to encode, got {bytes:?}");
        };
        assert_eq!(decode::<u32, FromToBytesCodec>(&bytes, &id(), here), Ok(7));
    }

    /// JSON object keys must be strings: this used to be `unwrap()`ed on the server.
    #[test]
    fn a_value_the_codec_cannot_serialize_is_an_error() {
        let here = Location::caller();
        let map = HashMap::from([((1_u8, 2_u8), 3_u8)]);

        let sent = encode::<_, JsonSerdeCodec>(Some(&map), &id(), here);

        assert!(
            matches!(
                &sent,
                Err(ResourceError::Encode { id: 3, created_at, reason })
                    if *created_at == here && reason.contains("key must be a string")
            ),
            "{sent:?}"
        );
    }

    /// A resource without a value used to reach `unreachable!()` on the server.
    #[test]
    fn a_missing_value_is_an_error() {
        let here = Location::caller();

        assert_eq!(
            encode::<u32, JsonSerdeCodec>(None, &id(), here),
            Err(ResourceError::NoValue {
                id: 3,
                created_at: here
            })
        );
    }

    /// What the page carries when a value cannot be sent: the marker, which the browser
    /// recognises before trying to decode it.
    #[test]
    fn the_page_carries_the_marker_for_a_value_it_cannot_send() {
        let here = Location::caller();

        assert_eq!(
            for_the_page(Err(ResourceError::NoValue {
                id: 3,
                created_at: here
            })),
            NOT_SENT
        );
        assert_eq!(for_the_page(Ok("7".to_owned())), "7");
    }

    /// The browser recognises the marker whatever the codec. `FromToStringCodec` decodes
    /// any string into a `String`, so without the check the marker became the value.
    #[test]
    fn the_marker_means_no_value_was_sent() {
        let here = Location::caller();
        let not_sent = ResourceError::NotSent {
            id: 3,
            created_at: here,
        };

        assert_eq!(
            decode::<String, FromToStringCodec>(NOT_SENT, &id(), here),
            Err(not_sent.clone())
        );
        assert_eq!(
            decode::<u32, JsonSerdeCodec>(NOT_SENT, &id(), here),
            Err(not_sent.clone())
        );
        assert_eq!(
            decode::<u32, FromToBytesCodec>(NOT_SENT, &id(), here),
            Err(not_sent)
        );
    }

    /// The marker is not valid data for the JSON and binary (base64) codecs, so it cannot
    /// be mistaken for a value of theirs.
    #[test]
    fn the_marker_is_not_valid_json_or_base64() {
        assert!(serde_json::from_str::<serde_json::Value>(NOT_SENT).is_err());
        assert!(<[u8]>::from_encoded_str(NOT_SENT).is_err());
    }

    /// Data that is not base64 cannot even be read as the binary codec's input.
    #[test]
    fn data_in_another_encoding_is_an_error() {
        let here = Location::caller();

        let decoded =
            decode::<u32, FromToBytesCodec>("not base64!", &id(), here);

        assert!(
            matches!(&decoded, Err(ResourceError::Unreadable { id: 3, .. })),
            "{decoded:?}"
        );
    }

    #[test]
    fn data_that_does_not_deserialize_is_an_error() {
        let here = Location::caller();

        let decoded = decode::<u32, JsonSerdeCodec>("\"seven\"", &id(), here);

        assert!(
            matches!(&decoded, Err(ResourceError::Decode { id: 3, .. })),
            "{decoded:?}"
        );
    }
}
