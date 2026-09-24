use thiserror::Error;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{
    Event, FormData, HtmlButtonElement, HtmlFormElement, HtmlInputElement,
    SubmitEvent,
};

/// Tries to deserialize a type from form data. This can be used for client-side
/// validation during form submission.
pub trait FromFormData
where
    Self: Sized + serde::de::DeserializeOwned,
{
    /// Tries to deserialize the data, given only the `submit` event.
    fn from_event(ev: &web_sys::Event) -> Result<Self, FromFormDataError>;

    /// Tries to deserialize the data, given the actual form data.
    fn from_form_data(
        form_data: &web_sys::FormData,
    ) -> Result<Self, serde_qs::Error>;
}

/// Errors that can arise when converting from an HTML event or form into a Rust data type.
#[derive(Error, Debug)]
pub enum FromFormDataError {
    /// Could not find a `<form>` connected to the event.
    #[error("Could not find <form> connected to event.")]
    MissingForm(Event),
    /// Could not create `FormData` from the form.
    #[error("Could not create FormData from <form>: {0:?}")]
    FormData(JsValue),
    /// Failed to deserialize this Rust type from the form data.
    #[error("Deserialization error: {0:?}")]
    Deserialization(serde_qs::Error),
}

impl<T> FromFormData for T
where
    T: serde::de::DeserializeOwned,
{
    fn from_event(ev: &Event) -> Result<Self, FromFormDataError> {
        let submit_ev = ev.unchecked_ref();
        let form_data = form_data_from_event(submit_ev)?;
        Self::from_form_data(&form_data)
            .map_err(FromFormDataError::Deserialization)
    }

    fn from_form_data(
        form_data: &web_sys::FormData,
    ) -> Result<Self, serde_qs::Error> {
        let data =
            web_sys::UrlSearchParams::new_with_str_sequence_sequence(form_data)
                .map_err(|err| {
                    <serde_qs::Error as serde::de::Error>::custom(format!(
                        "the form data has a value that is not text (a file \
                         input?): {err:?}"
                    ))
                })?;
        let data = data.to_string().as_string().unwrap_or_default();
        serde_qs::Config::new(5, false).deserialize_str::<Self>(&data)
    }
}

fn form_data_from_event(
    ev: &SubmitEvent,
) -> Result<FormData, FromFormDataError> {
    let submitter = ev.submitter();
    let mut submitter_name_value = None;
    let opt_form = match &submitter {
        Some(el) => {
            if let Some(form) = el.dyn_ref::<HtmlFormElement>() {
                Some(form.clone())
            } else if let Some(input) = el.dyn_ref::<HtmlInputElement>() {
                submitter_name_value = Some((input.name(), input.value()));
                // no target: `MissingForm` below
                ev.target().map(JsCast::unchecked_into)
            } else if let Some(button) = el.dyn_ref::<HtmlButtonElement>() {
                submitter_name_value = Some((button.name(), button.value()));
                ev.target().map(JsCast::unchecked_into)
            } else {
                None
            }
        }
        None => ev.target().map(|form| form.unchecked_into()),
    };
    match opt_form.as_ref().map(FormData::new_with_form) {
        None => Err(FromFormDataError::MissingForm(ev.clone().into())),
        Some(Err(e)) => Err(FromFormDataError::FormData(e)),
        Some(Ok(form_data)) => {
            if let Some((name, value)) = submitter_name_value {
                form_data
                    .append_with_str(&name, &value)
                    .map_err(FromFormDataError::FormData)?;
            }
            Ok(form_data)
        }
    }
}
