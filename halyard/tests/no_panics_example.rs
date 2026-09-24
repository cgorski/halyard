//! The worked example of docs/no-panics.md ("What an application writes", "Under B"), as
//! written there: it must compile, and the server renders it. Only `Contact`, `ApiError` and
//! `api::save` are supplied here.
#![cfg(feature = "ssr")]

use halyard::prelude::*;

#[derive(Clone, Debug, PartialEq)]
pub struct Contact {
    name: String,
    email: String,
}

#[derive(Clone, Debug)]
pub struct ApiError;

mod api {
    use super::{ApiError, Contact};

    pub async fn save(contact: Contact) -> Result<Contact, ApiError> {
        Ok(contact)
    }
}

// --- the example, unchanged ---

#[component]
pub fn ContactForm(contacts: RwSignal<Vec<Contact>>) -> impl IntoView {
    let name = RwSignal::new(String::new());
    let email = RwSignal::new(String::new());
    let valid = (name, email)
        .memo(|(name, email)| !name.trim().is_empty() && email.contains('@'));
    let save = Action::new(move |contact: &Contact| {
        let contact = contact.clone();
        async move {
            let saved = api::save(contact).await?;
            // the form may be gone by now: then there is nothing to clear
            if email.try_with(|e| *e == saved.email) == Some(true) {
                name.set(String::new());
                email.set(String::new());
            }
            contacts.update(|list| list.push(saved)); // a logged no-op if gone
            Ok::<_, ApiError>(())
        }
    });
    view! {
        <form on:submit=move |ev| {
            ev.prevent_default();
            let Some((name, email)) = (name, email).try_get() else { return };
            save.dispatch(Contact { name, email });
        }>
            <input prop:value=name
                on:input=move |ev| name.set(event_target_value(&ev)) />
            <input prop:value=email
                on:input=move |ev| email.set(event_target_value(&ev)) />
            <button disabled=(valid, save.pending()).map(|(v, p)| !v || *p)>"Save"</button>
        </form>
        <ul>
            <For each=contacts key=|c| c.email.clone() let:contact>
                <li>{contact.name}" <"{contact.email}">"</li>
            </For>
        </ul>
    }
}

// --- end of the example ---

#[test]
fn renders_on_the_server() {
    let owner = Owner::new();
    owner.set();
    let contacts = RwSignal::new(vec![Contact {
        name: String::from("Ada"),
        email: String::from("ada@example.com"),
    }]);
    let html = view! { <ContactForm contacts=contacts /> }.to_html();
    // both fields are empty, so the form is not valid and Save is disabled
    assert_eq!(
        html,
        "<form><input><input><button disabled>Save</button></form><ul><li>Ada<!> \
         &lt;<!>ada@example.com<!>&gt;</li><!></ul>"
    );
}
