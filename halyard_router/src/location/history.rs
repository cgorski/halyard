use super::{handle_anchor_click, LocationChange, LocationProvider, Url};
use crate::{
    error::{js_reason, report, RouterError},
    hooks::use_navigate,
    params::ParamsMap,
};
use core::fmt;
use futures::channel::oneshot;
use halyard::{ev, prelude::*};
use halyard_or_poisoned::OrPoisoned;
use halyard_reactive_graph::{
    signal::ArcRwSignal,
    traits::{ReadUntracked, Set},
};
use halyard_tachys::dom::{document, window};
use js_sys::{try_iter, Array, JsString};
use std::{
    borrow::Cow,
    string::String,
    sync::{Arc, Mutex},
};
use wasm_bindgen::{JsCast, JsValue};
use web_sys::UrlSearchParams;

#[derive(Clone)]
pub struct BrowserUrl {
    url: ArcRwSignal<Url>,
    pub(crate) pending_navigation: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    pub(crate) path_stack: ArcStoredValue<Vec<Url>>,
    pub(crate) is_back: ArcRwSignal<bool>,
}

impl fmt::Debug for BrowserUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrowserUrl").finish_non_exhaustive()
    }
}

impl BrowserUrl {
    fn scroll_to_el(loc_scroll: bool) {
        if let Ok(hash) = window().location().hash() {
            if !hash.is_empty() {
                let hash =
                    js_sys::decode_uri(hash.strip_prefix('#').unwrap_or(&hash))
                        .ok()
                        .and_then(|decoded| decoded.as_string())
                        .unwrap_or(hash);
                let el = document().get_element_by_id(&hash);
                if let Some(el) = el {
                    el.scroll_into_view();
                    return;
                }
            }
        }

        // scroll to top
        if loc_scroll {
            window().scroll_to_with_x_and_y(0.0, 0.0);
        }
    }
}

impl LocationProvider for BrowserUrl {
    type Error = JsValue;

    fn new() -> Result<Self, JsValue> {
        let url = ArcRwSignal::new(Self::current()?);
        let path_stack = ArcStoredValue::new(
            Self::current().map(|n| vec![n]).unwrap_or_default(),
        );
        Ok(Self {
            url,
            pending_navigation: Default::default(),
            path_stack,
            is_back: Default::default(),
        })
    }

    fn as_url(&self) -> &ArcRwSignal<Url> {
        &self.url
    }

    fn current() -> Result<Url, Self::Error> {
        let location = window().location();
        Ok(Url {
            origin: location.origin()?,
            path: location.pathname()?,
            search: location
                .search()?
                .strip_prefix('?')
                .map(String::from)
                .unwrap_or_default(),
            search_params: search_params_from_web_url(
                &UrlSearchParams::new_with_str(&location.search()?)?,
            )?,
            hash: location.hash()?,
        })
    }

    fn parse(url: &str) -> Result<Url, Self::Error> {
        let base = window().location().origin()?;
        Self::parse_with_base(url, &base)
    }

    fn parse_with_base(url: &str, base: &str) -> Result<Url, Self::Error> {
        let location = web_sys::Url::new_with_base(url, base)?;
        Ok(Url {
            origin: location.origin(),
            path: location.pathname(),
            search: location
                .search()
                .strip_prefix('?')
                .map(String::from)
                .unwrap_or_default(),
            search_params: search_params_from_web_url(
                &location.search_params(),
            )?,
            hash: location.hash(),
        })
    }

    fn init(&self, base: Option<Cow<'static, str>>) {
        let navigate = {
            let url = self.url.clone();
            let pending = Arc::clone(&self.pending_navigation);
            let this = self.clone();
            move |new_url: Url, loc| {
                let same_path = {
                    let curr = url.read_untracked();
                    curr.origin() == new_url.origin()
                        && curr.path() == new_url.path()
                };

                url.set(new_url.clone());
                if same_path {
                    this.complete_navigation(&loc);
                }
                let pending = Arc::clone(&pending);
                let (tx, rx) = oneshot::channel::<()>();
                if !same_path {
                    *pending.lock().or_poisoned() = Some(tx);
                }
                let url = url.clone();
                let this = this.clone();
                async move {
                    if !same_path {
                        // if it has been canceled, ignore
                        // otherwise, complete navigation -- i.e., set URL in address bar
                        if rx.await.is_ok() {
                            // only update the URL in the browser if this is still the current URL
                            // if we've navigated to another page in the meantime, don't update the
                            // browser URL
                            let curr = url.read_untracked();
                            if curr == new_url {
                                this.complete_navigation(&loc);
                            }
                        }
                    }
                }
            }
        };

        let handle_anchor_click =
            handle_anchor_click(base, Self::parse_with_base, navigate);

        let click_handle = window_event_listener(ev::click, move |ev| {
            if let Err(e) = handle_anchor_click(ev) {
                #[cfg(feature = "tracing")]
                tracing::error!("{e:?}");
                #[cfg(not(feature = "tracing"))]
                web_sys::console::error_1(&e);
            }
        });

        // handle popstate event (forward/back navigation)
        let popstate_cb = {
            let url = self.url.clone();
            let path_stack = self.path_stack.clone();
            let is_back = self.is_back.clone();
            move || match Self::current() {
                Ok(new_url) => {
                    let mut stack = path_stack.write_value();
                    // back to the first page, or to the one before the current page
                    let is_navigating_back = stack.len() == 1
                        || stack
                            .len()
                            .checked_sub(2)
                            .and_then(|previous| stack.get(previous))
                            == Some(&new_url);

                    if is_navigating_back {
                        stack.pop();
                    }

                    is_back.set(is_navigating_back);

                    url.set(new_url);
                }
                Err(e) => {
                    #[cfg(feature = "tracing")]
                    tracing::error!("{e:?}");
                    #[cfg(not(feature = "tracing"))]
                    web_sys::console::error_1(&e);
                }
            }
        };

        let popstate_handle =
            window_event_listener(ev::popstate, move |_| popstate_cb());

        on_cleanup(|| {
            click_handle.remove();
            popstate_handle.remove();
        });
    }

    fn ready_to_complete(&self) {
        if let Some(tx) = self.pending_navigation.lock().or_poisoned().take() {
            _ = tx.send(());
        }
    }

    fn complete_navigation(&self, loc: &LocationChange) {
        let window = window();

        let current_path = self
            .path_stack
            .read_value()
            .last()
            .map(|url| url.to_full_path());
        let add_to_stack = current_path.as_ref() != Some(&loc.value);

        let updated = window.history().and_then(|history| {
            if loc.replace {
                history.replace_state_with_url(
                    &loc.state.to_js_value(),
                    "",
                    Some(&loc.value),
                )
            } else if add_to_stack {
                // push the "forward direction" marker
                let state = &loc.state.to_js_value();
                history.push_state_with_url(state, "", Some(&loc.value))
            } else {
                Ok(())
            }
        });
        // the page is showing the new route but the address bar is not: load the page
        // from the server, so that the two agree (and a reload shows this page)
        if let Err(error) = updated {
            report(&RouterError::Browser {
                action: "updating the browser history",
                reason: js_reason(&error),
                instead: "loading the page from the server",
            });
            let location = window.location();
            let loaded = if loc.replace {
                location.replace(&loc.value)
            } else {
                location.assign(&loc.value)
            };
            if let Err(error) = loaded {
                report(&RouterError::Browser {
                    action: "loading the page from the server",
                    reason: js_reason(&error),
                    instead: "the address bar keeps the previous URL",
                });
            }
            return;
        }

        // add this URL to the "path stack" for detecting back navigations, and
        // unset "navigating back" state
        if let Ok(url) = Self::current() {
            if add_to_stack {
                self.path_stack.write_value().push(url);
            }
            self.is_back.set(false);
        }

        // scroll to el
        Self::scroll_to_el(loc.scroll);
    }

    fn redirect(loc: &str) {
        let navigate = use_navigate();
        let Some(url) = resolve_redirect_url(loc) else {
            return; // resolve_redirect_url() already logs an error
        };
        let same_origin = match location().origin() {
            Ok(current_origin) => url.origin() == current_origin,
            Err(error) => {
                report(&RouterError::Browser {
                    action: "reading this page's origin for a redirect",
                    reason: js_reason(&error),
                    instead: "loading the redirect target from the server",
                });
                false
            }
        };
        if same_origin {
            let navigate = navigate.clone();
            // delay by a tick here, so that the Action updates *before* the redirect
            request_animation_frame(move || {
                navigate(&url.href(), Default::default());
            });
            // Use set_href() if the conditions for client-side navigation were not satisfied
        } else if let Err(e) = location().set_href(&url.href()) {
            halyard::logging::error!("Failed to redirect: {e:#?}");
        }
    }

    fn is_back(&self) -> ReadSignal<bool> {
        self.is_back.read_only().into()
    }
}

fn search_params_from_web_url(
    params: &web_sys::UrlSearchParams,
) -> Result<ParamsMap, JsValue> {
    try_iter(params)?
        .into_iter()
        .flatten()
        .map(|pair| {
            pair.and_then(|pair| {
                let row = pair.dyn_into::<Array>()?;
                Ok((
                    String::from(row.get(0).dyn_into::<JsString>()?),
                    String::from(row.get(1).dyn_into::<JsString>()?),
                ))
            })
        })
        .collect()
}

/// Resolves a redirect location to an (absolute) URL.
pub(crate) fn resolve_redirect_url(loc: &str) -> Option<web_sys::Url> {
    let origin = match window().location().origin() {
        Ok(origin) => origin,
        Err(e) => {
            halyard::logging::error!("Failed to get origin: {:#?}", e);
            return None;
        }
    };

    // TODO: Use server function's URL as base instead.
    let base = origin;

    match web_sys::Url::new_with_base(loc, &base) {
        Ok(url) => Some(url),
        Err(e) => {
            halyard::logging::error!(
                "Invalid redirect location: {}",
                e.as_string().unwrap_or_default(),
            );
            None
        }
    }
}
