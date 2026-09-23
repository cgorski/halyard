//! Validates the hydration data that the server embeds in the page.
//!
//! The server's data scripts (see `ssr.rs`) define three globals. Before the browser
//! reads them, the page may have lost the script that defines them (stripped, or blocked),
//! been edited by an extension, or been cached from an older build. None of that may crash
//! the application: every problem becomes a [`PageDataError`], and the browser context
//! treats the affected data as absent (a resource then loads on the client, as it would in
//! client-side rendering) and logs why.
//!
//! The parsers take a [`JsValueLike`] rather than a `JsValue`, so they can be tested
//! natively.

use crate::SerializedDataId;
use halyard_throw_error::{Error, ErrorId};
use std::fmt::{self, Display};

/// Serialized resource values, indexed by resource id.
pub(crate) const RESOLVED_RESOURCES: &str = "__RESOLVED_RESOURCES";
/// Errors registered on the server: `[boundary id, error id, message]` entries.
pub(crate) const SERIALIZED_ERRORS: &str = "__SERIALIZED_ERRORS";
/// Ids of the chunks that the server sent before all their data had loaded.
pub(crate) const INCOMPLETE_CHUNKS: &str = "__INCOMPLETE_CHUNKS";

/// Why hydration data that the server embedded in the page cannot be used.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub(crate) enum PageDataError {
    /// The global is not defined.
    #[error(
        "`{global}` is not defined (the server's data script is missing from \
         the page)"
    )]
    Missing { global: &'static str },
    /// Reading the global threw.
    #[error("`{global}` could not be read: {reason}")]
    Unreadable {
        global: &'static str,
        reason: String,
    },
    /// The global is defined, but it is not an array.
    #[error("`{global}` is {found}, not an array")]
    NotAnArray { global: &'static str, found: String },
    /// Entries of `__SERIALIZED_ERRORS` or `__INCOMPLETE_CHUNKS` that were left out.
    #[error("left out {count} malformed entry/entries of `{global}`; the first: {first}")]
    MalformedEntries {
        global: &'static str,
        count: usize,
        first: MalformedEntry,
    },
    /// An entry of `__RESOLVED_RESOURCES` that is not a string.
    #[error("{0}")]
    MalformedResource(MalformedEntry),
    /// A resource id that cannot index a JavaScript array.
    #[error("resource id {id} is too large to index `__RESOLVED_RESOURCES`")]
    IdOutOfRange { id: usize },
}

/// The entries of a global that could be used, and why any others could not.
pub(crate) type Parsed<T> = (Vec<T>, Option<PageDataError>);

/// A value in the page's hydration data that does not have the shape the server writes.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("expected {expected} at `{location}`, found {found}")]
pub(crate) struct MalformedEntry {
    location: String,
    expected: &'static str,
    found: String,
}

/// What the parsers need to know about a JavaScript value. Implemented for `JsValue` in
/// the browser, and by a plain enum in the tests.
pub(crate) trait JsValueLike: Sized {
    /// Whether the value is `undefined`, as an absent global or array element is.
    fn is_missing(&self) -> bool;
    /// The value, if it is a number.
    fn number(&self) -> Option<f64>;
    /// The value, if it is a string.
    fn string(&self) -> Option<String>;
    /// The elements, if the value is an array.
    fn items(&self) -> Option<Vec<Self>>;
    /// The JavaScript `typeof` of the value, but `"null"` for `null`.
    fn type_of(&self) -> String;
}

/// Describes a value that has the wrong shape, for the log.
fn describe(value: &impl JsValueLike) -> String {
    if let Some(number) = value.number() {
        format!("the number {number}")
    } else if value.items().is_some() {
        "an array".to_owned()
    } else {
        format!("a value of type `{}`", value.type_of())
    }
}

/// Reads an id that the server wrote as a number: a non-negative integer.
///
/// An integer beyond `usize::MAX` saturates, as the `as` conversion always has here.
/// Such ids come from the server's countdown for the parts of an islands page that are
/// not hydrated, and no id that the browser allocates comes near them.
fn id(value: &impl JsValueLike) -> Option<usize> {
    let number = value.number()?;
    let is_id = number.is_finite() && number >= 0.0 && number.fract() == 0.0;
    is_id.then_some(number as usize)
}

/// Why the global `global`, which the server defines as an array, is not one.
pub(crate) fn not_an_array(
    global: &'static str,
    value: &impl JsValueLike,
) -> PageDataError {
    if value.is_missing() {
        PageDataError::Missing { global }
    } else {
        PageDataError::NotAnArray {
            global,
            found: describe(value),
        }
    }
}

/// The index of resource `id` in `__RESOLVED_RESOURCES`.
pub(crate) fn resource_index(
    id: &SerializedDataId,
) -> Result<u32, PageDataError> {
    u32::try_from(id.0).map_err(|_| PageDataError::IdOutOfRange { id: id.0 })
}

/// The serialized value of resource `id`, given its entry in `__RESOLVED_RESOURCES`.
///
/// `Ok(None)` if the server sent no value for it (the entry is `undefined`).
pub(crate) fn resolved_resource(
    id: &SerializedDataId,
    entry: &impl JsValueLike,
) -> Result<Option<String>, PageDataError> {
    if entry.is_missing() {
        return Ok(None);
    }
    entry.string().map(Some).ok_or_else(|| {
        PageDataError::MalformedResource(MalformedEntry {
            location: format!("{RESOLVED_RESOURCES}[{}]", id.0),
            expected: "a serialized value (a string)",
            found: describe(entry),
        })
    })
}

/// Parses the entries of `__SERIALIZED_ERRORS`, leaving out any that are malformed.
pub(crate) fn serialized_errors<V: JsValueLike>(
    entries: &[V],
) -> Parsed<(SerializedDataId, ErrorId, Error)> {
    collect(
        SERIALIZED_ERRORS,
        entries
            .iter()
            .enumerate()
            .map(|(index, entry)| serialized_error(index, entry)),
    )
}

fn serialized_error<V: JsValueLike>(
    index: usize,
    entry: &V,
) -> Result<(SerializedDataId, ErrorId, Error), MalformedEntry> {
    const SHAPE: &str = "an array [boundary id, error id, message]";
    let malformed = |found: String| MalformedEntry {
        location: format!("{SERIALIZED_ERRORS}[{index}]"),
        expected: SHAPE,
        found,
    };
    let field =
        |field: usize, expected: &'static str, value: &V| MalformedEntry {
            location: format!("{SERIALIZED_ERRORS}[{index}][{field}]"),
            expected,
            found: describe(value),
        };

    let fields = entry.items().ok_or_else(|| malformed(describe(entry)))?;
    let [boundary_id, error_id, message, ..] = fields.as_slice() else {
        return Err(malformed(format!("an array of length {}", fields.len())));
    };
    let boundary_id = id(boundary_id).ok_or_else(|| {
        field(0, "a boundary id (a non-negative integer)", boundary_id)
    })?;
    let error_id = id(error_id).ok_or_else(|| {
        field(1, "an error id (a non-negative integer)", error_id)
    })?;
    let message = message
        .string()
        .ok_or_else(|| field(2, "an error message (a string)", message))?;
    Ok((
        SerializedDataId(boundary_id),
        ErrorId::from(error_id),
        Error::from(SerializedError(message)),
    ))
}

/// Parses the entries of `__INCOMPLETE_CHUNKS`, leaving out any that are malformed.
pub(crate) fn incomplete_chunks<V: JsValueLike>(
    entries: &[V],
) -> Parsed<SerializedDataId> {
    collect(
        INCOMPLETE_CHUNKS,
        entries.iter().enumerate().map(|(index, entry)| {
            id(entry)
                .map(SerializedDataId)
                .ok_or_else(|| MalformedEntry {
                    location: format!("{INCOMPLETE_CHUNKS}[{index}]"),
                    expected: "a chunk id (a non-negative integer)",
                    found: describe(entry),
                })
        }),
    )
}

/// Keeps the entries that parsed, and sums up the ones that did not in one error.
fn collect<T>(
    global: &'static str,
    entries: impl Iterator<Item = Result<T, MalformedEntry>>,
) -> Parsed<T> {
    let mut values = Vec::new();
    let mut malformed = Vec::new();
    for entry in entries {
        match entry {
            Ok(value) => values.push(value),
            Err(error) => malformed.push(error),
        }
    }
    let count = malformed.len();
    let error = malformed.into_iter().next().map(|first| {
        PageDataError::MalformedEntries {
            global,
            count,
            first,
        }
    });
    (values, error)
}

/// An error that has been serialized across the network boundary.
#[derive(Debug, Clone)]
struct SerializedError(String);

impl Display for SerializedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, f)
    }
}

impl std::error::Error for SerializedError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A JavaScript value, as the page's data scripts can leave it.
    #[derive(Clone, Debug)]
    enum Js {
        Undefined,
        Null,
        Num(f64),
        Str(&'static str),
        Arr(Vec<Js>),
        Obj,
    }

    impl JsValueLike for Js {
        fn is_missing(&self) -> bool {
            matches!(self, Js::Undefined)
        }

        fn number(&self) -> Option<f64> {
            match self {
                Js::Num(number) => Some(*number),
                _ => None,
            }
        }

        fn string(&self) -> Option<String> {
            match self {
                Js::Str(string) => Some((*string).to_owned()),
                _ => None,
            }
        }

        fn items(&self) -> Option<Vec<Self>> {
            match self {
                Js::Arr(items) => Some(items.clone()),
                _ => None,
            }
        }

        fn type_of(&self) -> String {
            match self {
                Js::Undefined => "undefined",
                Js::Null => "null",
                Js::Num(_) => "number",
                Js::Str(_) => "string",
                Js::Arr(_) | Js::Obj => "object",
            }
            .to_owned()
        }
    }

    fn error_entry(boundary: Js, error: Js, message: Js) -> Js {
        Js::Arr(vec![boundary, error, message])
    }

    fn summary(errors: &[(SerializedDataId, ErrorId, Error)]) -> Vec<String> {
        errors
            .iter()
            .map(|(boundary, id, error)| {
                format!("{}/{id}: {error}", boundary.0)
            })
            .collect()
    }

    fn malformed_count(error: Option<PageDataError>) -> Option<usize> {
        match error {
            Some(PageDataError::MalformedEntries { count, .. }) => Some(count),
            _ => None,
        }
    }

    /// A field of the wrong type used to panic (`unwrap`/`expect`); now the entry is
    /// left out, the others are kept, and the first problem is reported.
    #[test]
    fn serialized_errors_leaves_out_entries_with_a_mistyped_field() {
        let entries = [
            error_entry(Js::Num(0.0), Js::Num(1.0), Js::Str("kept")),
            error_entry(Js::Str("0"), Js::Num(1.0), Js::Str("bad boundary")),
            error_entry(Js::Num(0.0), Js::Null, Js::Str("bad error id")),
            error_entry(Js::Num(0.0), Js::Num(2.0), Js::Num(3.0)),
            Js::Arr(vec![Js::Num(0.0), Js::Num(4.0)]),
            error_entry(Js::Num(5.0), Js::Num(6.0), Js::Str("also kept")),
        ];

        let (errors, rejected) = serialized_errors(&entries);

        assert_eq!(summary(&errors), ["0/1: kept", "5/6: also kept"]);
        let Some(PageDataError::MalformedEntries {
            global,
            count,
            first,
        }) = rejected
        else {
            panic!("expected MalformedEntries, got {rejected:?}");
        };
        assert_eq!((global, count), (SERIALIZED_ERRORS, 4));
        assert_eq!(first.location, "__SERIALIZED_ERRORS[1][0]");
        assert_eq!(first.found, "a value of type `string`");
    }

    /// Entries that are not arrays used to be dropped without a word; they are still
    /// left out, but now reported.
    #[test]
    fn serialized_errors_reports_entries_that_are_not_arrays() {
        let entries = [
            Js::Num(7.0),
            Js::Obj,
            error_entry(Js::Num(0.0), Js::Num(0.0), Js::Str("kept")),
        ];

        let (errors, rejected) = serialized_errors(&entries);

        assert_eq!(summary(&errors), ["0/0: kept"]);
        assert_eq!(malformed_count(rejected), Some(2));
    }

    /// A number that is not an id used to be converted anyway (`-1` and `NaN` to `0`,
    /// `1.5` to `1`), which files the data under another id; now it is left out.
    #[test]
    fn ids_must_be_non_negative_integers() {
        let entries = [
            Js::Num(-1.0),
            Js::Num(1.5),
            Js::Num(f64::NAN),
            Js::Num(f64::INFINITY),
            Js::Num(3.0),
        ];

        let (chunks, rejected) = incomplete_chunks(&entries);

        assert_eq!(chunks, [SerializedDataId(3)]);
        assert_eq!(malformed_count(rejected), Some(4));
    }

    /// A chunk id that is not a number used to panic (`unwrap`).
    #[test]
    fn incomplete_chunks_leaves_out_values_that_are_not_numbers() {
        let entries = [Js::Str("1"), Js::Null, Js::Undefined, Js::Num(2.0)];

        let (chunks, rejected) = incomplete_chunks(&entries);

        assert_eq!(chunks, [SerializedDataId(2)]);
        let Some(PageDataError::MalformedEntries { count, first, .. }) =
            rejected
        else {
            panic!("expected MalformedEntries, got {rejected:?}");
        };
        assert_eq!(count, 3);
        assert_eq!(first.location, "__INCOMPLETE_CHUNKS[0]");
    }

    /// Guard, not a fix: well-formed data parses exactly as before, including the
    /// saturating conversion of ids from the server's non-hydrated countdown, so an
    /// islands page does not log a false warning.
    #[test]
    fn well_formed_ids_convert_as_before() {
        let entries = [Js::Num(0.0), Js::Num(18_446_744_073_709_551_615.0)];

        let (chunks, rejected) = incomplete_chunks(&entries);

        assert_eq!(chunks, [SerializedDataId(0), SerializedDataId(usize::MAX)]);
        assert_eq!(rejected, None);
    }

    /// An entry that is not a string used to read as "no data" without a word; it still
    /// does, but it is reported. An absent entry is the normal "not sent" case.
    #[test]
    fn resolved_resource_reports_entries_that_are_not_strings() {
        let id = SerializedDataId(4);

        assert_eq!(resolved_resource(&id, &Js::Undefined), Ok(None));
        assert_eq!(
            resolved_resource(&id, &Js::Str("\"value\"")),
            Ok(Some("\"value\"".to_owned()))
        );
        for entry in [Js::Null, Js::Num(1.0), Js::Obj] {
            let Err(PageDataError::MalformedResource(malformed)) =
                resolved_resource(&id, &entry)
            else {
                panic!("expected MalformedResource for {entry:?}");
            };
            assert_eq!(malformed.location, "__RESOLVED_RESOURCES[4]");
        }
    }

    /// `id as u32` used to wrap a large id around to another resource's index.
    #[cfg(target_pointer_width = "64")]
    #[test]
    fn resource_index_does_not_wrap_large_ids() {
        assert_eq!(resource_index(&SerializedDataId(7)), Ok(7));
        assert_eq!(
            resource_index(&SerializedDataId(4_294_967_296)),
            Err(PageDataError::IdOutOfRange { id: 4_294_967_296 })
        );
    }

    /// A global that is absent (the script that defines it is missing) is reported as
    /// such, and one that is something else says what it is.
    #[test]
    fn not_an_array_tells_missing_from_mistyped() {
        assert_eq!(
            not_an_array(RESOLVED_RESOURCES, &Js::Undefined),
            PageDataError::Missing {
                global: RESOLVED_RESOURCES
            }
        );
        assert_eq!(
            not_an_array(SERIALIZED_ERRORS, &Js::Obj),
            PageDataError::NotAnArray {
                global: SERIALIZED_ERRORS,
                found: "a value of type `object`".to_owned()
            }
        );
        assert_eq!(
            not_an_array(INCOMPLETE_CHUNKS, &Js::Num(1.0)).to_string(),
            "`__INCOMPLETE_CHUNKS` is the number 1, not an array"
        );
    }

    /// The warning is all a developer gets: it names the global and what is wrong.
    #[test]
    fn warnings_name_the_global_and_the_problem() {
        let (_, rejected) = serialized_errors(&[Js::Arr(vec![Js::Num(0.0)])]);
        let messages = [
            not_an_array(RESOLVED_RESOURCES, &Js::Undefined).to_string(),
            PageDataError::Unreadable {
                global: SERIALIZED_ERRORS,
                reason: "TypeError: denied".to_owned(),
            }
            .to_string(),
            rejected.map(|error| error.to_string()).unwrap_or_default(),
        ];

        assert_eq!(
            messages,
            [
                "`__RESOLVED_RESOURCES` is not defined (the server's data \
                 script is missing from the page)",
                "`__SERIALIZED_ERRORS` could not be read: TypeError: denied",
                "left out 1 malformed entry/entries of `__SERIALIZED_ERRORS`; \
                 the first: expected an array [boundary id, error id, \
                 message] at `__SERIALIZED_ERRORS[0]`, found an array of \
                 length 1",
            ]
        );
    }
}
