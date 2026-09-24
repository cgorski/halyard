use super::{SerializedDataId, SharedContext};
use crate::hydration_context::{
    page_data::{self, JsValueLike, PageDataError, Parsed},
    PinnedFuture, PinnedStream,
};
use crate::throw_error::{Error, ErrorId};
use core::fmt::Debug;
use js_sys::{Array, Reflect};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    LazyLock,
};
use wasm_bindgen::{prelude::wasm_bindgen, JsCast, JsValue};

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(catch, js_namespace = console, js_name = warn)]
    fn console_warn(message: &str) -> Result<(), JsValue>;
}

/// Logs why some of the hydration data in the page is being left out.
fn warn(error: &PageDataError) {
    // if even `console.warn` throws, there is nowhere left to report it
    _ = console_warn(&format!(
        "[halyard] Ignoring hydration data from the server: {error}. The \
         affected data is loaded on the client instead."
    ));
}

impl JsValueLike for JsValue {
    fn is_missing(&self) -> bool {
        self.is_undefined()
    }

    fn number(&self) -> Option<f64> {
        self.as_f64()
    }

    fn string(&self) -> Option<String> {
        self.as_string()
    }

    fn items(&self) -> Option<Vec<Self>> {
        self.dyn_ref::<Array>().map(|array| array.iter().collect())
    }

    fn type_of(&self) -> String {
        if self.is_null() {
            "null".to_owned()
        } else {
            self.js_typeof().as_string().unwrap_or_default()
        }
    }
}

// Each global is read once per page load (and any problem with it logged once), as the
// `#[wasm_bindgen(thread_local)]` statics that these replace were. Unlike those, a
// missing global is not a `ReferenceError` thrown through the application.
thread_local! {
    static RESOLVED_RESOURCES: Option<Array> =
        global_array(page_data::RESOLVED_RESOURCES).inspect_err(warn).ok();
    static SERIALIZED_ERRORS: Vec<(SerializedDataId, ErrorId, Error)> =
        parse_global(page_data::SERIALIZED_ERRORS, page_data::serialized_errors);
    static INCOMPLETE_CHUNKS: Vec<SerializedDataId> =
        parse_global(page_data::INCOMPLETE_CHUNKS, page_data::incomplete_chunks);
}

/// Reads the global array `name` that the server's data script defines.
fn global_array(name: &'static str) -> Result<Array, PageDataError> {
    let value = Reflect::get(&js_sys::global(), &JsValue::from_str(name))
        .map_err(|error| PageDataError::Unreadable {
            global: name,
            reason: format!("{error:?}"),
        })?;
    value
        .dyn_into::<Array>()
        .map_err(|value| page_data::not_an_array(name, &value))
}

/// Reads and parses the global array `name`, and logs whatever had to be left out.
fn parse_global<T>(
    name: &'static str,
    parse: fn(&[JsValue]) -> Parsed<T>,
) -> Vec<T> {
    let entries: Vec<JsValue> = match global_array(name) {
        Ok(array) => array.iter().collect(),
        Err(error) => {
            warn(&error);
            return Vec::new();
        }
    };
    let (values, malformed) = parse(&entries);
    if let Some(error) = malformed {
        warn(&error);
    }
    values
}

fn serialized_errors() -> Vec<(SerializedDataId, ErrorId, Error)> {
    SERIALIZED_ERRORS.try_with(Clone::clone).unwrap_or_default()
}

fn incomplete_chunks() -> Vec<SerializedDataId> {
    INCOMPLETE_CHUNKS.try_with(Clone::clone).unwrap_or_default()
}

#[derive(Default)]
/// The shared context that should be used in the browser while hydrating.
pub struct HydrateSharedContext {
    id: AtomicUsize,
    is_hydrating: AtomicBool,
    during_hydration: AtomicBool,
    errors: LazyLock<Vec<(SerializedDataId, ErrorId, Error)>>,
    incomplete: LazyLock<Vec<SerializedDataId>>,
}

impl HydrateSharedContext {
    /// Creates a new shared context for hydration in the browser.
    pub fn new() -> Self {
        Self {
            id: AtomicUsize::new(0),
            is_hydrating: AtomicBool::new(true),
            during_hydration: AtomicBool::new(true),
            errors: LazyLock::new(serialized_errors),
            incomplete: LazyLock::new(incomplete_chunks),
        }
    }

    /// Creates a new shared context for hydration in the browser.
    ///
    /// This defaults to a mode in which the app is not hydrated, but allows you to opt into
    /// hydration for certain portions using [`SharedContext::set_is_hydrating`].
    pub fn new_islands() -> Self {
        Self {
            id: AtomicUsize::new(0),
            is_hydrating: AtomicBool::new(false),
            during_hydration: AtomicBool::new(true),
            errors: LazyLock::new(serialized_errors),
            incomplete: LazyLock::new(incomplete_chunks),
        }
    }
}

impl Debug for HydrateSharedContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HydrateSharedContext").finish()
    }
}

impl SharedContext for HydrateSharedContext {
    fn is_browser(&self) -> bool {
        true
    }

    fn next_id(&self) -> SerializedDataId {
        let id = self.id.fetch_add(1, Ordering::Relaxed);
        SerializedDataId(id)
    }

    fn write_async(&self, _id: SerializedDataId, _fut: PinnedFuture<String>) {}

    fn read_data(&self, id: &SerializedDataId) -> Option<String> {
        RESOLVED_RESOURCES
            .try_with(|resources| {
                let resources = resources.as_ref()?;
                let index =
                    page_data::resource_index(id).inspect_err(warn).ok()?;
                page_data::resolved_resource(id, &resources.get(index))
                    .inspect_err(warn)
                    .ok()
                    .flatten()
            })
            .ok()
            .flatten()
    }

    fn await_data(&self, id: &SerializedDataId) -> Option<String> {
        // halyard's bootstrap script hydrates once the document has been parsed, so every
        // data script that the server streamed has run: what is not here now never will be
        self.read_data(id)
    }

    fn pending_data(&self) -> Option<PinnedStream<String>> {
        None
    }

    fn during_hydration(&self) -> bool {
        self.during_hydration.load(Ordering::Relaxed)
    }

    fn hydration_complete(&self) {
        self.during_hydration.store(false, Ordering::Relaxed)
    }

    fn get_is_hydrating(&self) -> bool {
        self.is_hydrating.load(Ordering::Relaxed)
    }

    fn set_is_hydrating(&self, is_hydrating: bool) {
        self.is_hydrating.store(is_hydrating, Ordering::Relaxed)
    }

    fn errors(&self, boundary_id: &SerializedDataId) -> Vec<(ErrorId, Error)> {
        self.errors
            .iter()
            .filter_map(|(boundary, id, error)| {
                if boundary == boundary_id {
                    Some((id.clone(), error.clone()))
                } else {
                    None
                }
            })
            .collect()
    }

    #[inline(always)]
    fn register_error(
        &self,
        _error_boundary: SerializedDataId,
        _error_id: ErrorId,
        _error: Error,
    ) {
    }

    #[inline(always)]
    fn seal_errors(&self, _boundary_id: &SerializedDataId) {}

    fn take_errors(&self) -> Vec<(SerializedDataId, ErrorId, Error)> {
        self.errors.clone()
    }

    #[inline(always)]
    fn defer_stream(&self, _wait_for: PinnedFuture<()>) {}

    #[inline(always)]
    fn await_deferred(&self) -> Option<PinnedFuture<()>> {
        None
    }

    #[inline(always)]
    fn set_incomplete_chunk(&self, _id: SerializedDataId) {}

    fn get_incomplete_chunk(&self, id: &SerializedDataId) -> bool {
        self.incomplete.iter().any(|entry| entry == id)
    }
}
