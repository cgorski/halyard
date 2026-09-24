use crate::router::{
    components::RouterContext,
    error::{report, report_once, RouterError},
    location::{Location, State, Url},
    navigate::NavigateOptions,
    params::{Params, ParamsError, ParamsMap},
    resolve_path::resolve_path,
};
use halyard::{dom::helpers::request_animation_frame, oco::Oco};
use halyard_reactive_graph::traits::StrongWriteValue;
use halyard_reactive_graph::traits::TryWith;
use halyard_reactive_graph::{
    computed::{ArcMemo, Memo},
    owner::use_context,
    signal::{ArcRwSignal, ReadSignal},
    traits::{Get, TryGetUntracked, TryWithUntracked, With},
    wrappers::write::SignalSetter,
};
use std::{
    str::FromStr,
    sync::atomic::{AtomicBool, Ordering},
};

/// See [`query_signal`].
#[track_caller]
#[deprecated = "This has been renamed to `query_signal` to match Rust naming \
                conventions."]
pub fn create_query_signal<T>(
    key: impl Into<Oco<'static, str>>,
) -> (Memo<Option<T>>, SignalSetter<Option<T>>)
where
    T: FromStr + ToString + PartialEq + Send + Sync,
{
    query_signal(key)
}

/// See [`query_signal_with_options`].
#[track_caller]
#[deprecated = "This has been renamed to `query_signal_with_options` to mtch \
                Rust naming conventions."]
pub fn create_query_signal_with_options<T>(
    key: impl Into<Oco<'static, str>>,
    nav_options: NavigateOptions,
) -> (Memo<Option<T>>, SignalSetter<Option<T>>)
where
    T: FromStr + ToString + PartialEq + Send + Sync,
{
    query_signal_with_options(key, nav_options)
}

/// Constructs a signal synchronized with a specific URL query parameter.
///
/// The function creates a bidirectional sync mechanism between the state encapsulated in a signal and a URL query parameter.
/// This means that any change to the state will update the URL, and vice versa, making the function especially useful
/// for maintaining state consistency across page reloads.
///
/// The `key` argument is the unique identifier for the query parameter to be synced with the state.
/// It is important to note that only one state can be tied to a specific key at any given time.
///
/// The function operates with types that can be parsed from and formatted into strings, denoted by `T`.
/// If the parsing fails for any reason, the function treats the value as `None`.
/// The URL parameter can be cleared by setting the signal to `None`.
///
/// ```rust
/// use halyard::prelude::*;
/// use halyard::router::hooks::query_signal;
///
/// #[component]
/// pub fn SimpleQueryCounter() -> impl IntoView {
///     let (count, set_count) = query_signal::<i32>("count");
///     let clear = move |_| set_count.set(None);
///     let decrement =
///         move |_| set_count.set(Some(count.try_get().unwrap().unwrap_or(0) - 1));
///     let increment =
///         move |_| set_count.set(Some(count.try_get().unwrap().unwrap_or(0) + 1));
///
///     view! {
///         <div>
///             <button on:click=clear>"Clear"</button>
///             <button on:click=decrement>"-1"</button>
///             <span>"Value: " {move || count.try_get().unwrap().unwrap_or(0)} "!"</span>
///             <button on:click=increment>"+1"</button>
///         </div>
///     }
/// }
/// ```
#[track_caller]
pub fn query_signal<T>(
    key: impl Into<Oco<'static, str>>,
) -> (Memo<Option<T>>, SignalSetter<Option<T>>)
where
    T: FromStr + ToString + PartialEq + Send + Sync,
{
    query_signal_with_options::<T>(key, NavigateOptions::default())
}

/// Constructs a signal synchronized with a specific URL query parameter.
///
/// This is the same as [`query_signal`], but allows you to specify additional navigation options.
#[track_caller]
pub fn query_signal_with_options<T>(
    key: impl Into<Oco<'static, str>>,
    nav_options: NavigateOptions,
) -> (Memo<Option<T>>, SignalSetter<Option<T>>)
where
    T: FromStr + ToString + PartialEq + Send + Sync,
{
    static IS_NAVIGATING: AtomicBool = AtomicBool::new(false);

    let mut key: Oco<'static, str> = key.into();
    let query_map = use_query_map();
    let navigate = use_navigate();
    let location = use_location();
    // outside a router the hooks above have logged it, and there is no URL to update
    let query_mutations =
        use_context::<RouterContext>().map(|router| router.query_mutations);

    let get = Memo::new_try({
        let key = key.clone_inplace();
        move |_| {
            query_map.try_with(|map| {
                map.get_str(&key).and_then(|value| value.parse().ok())
            })
        }
    });

    let set = SignalSetter::map(move |value: Option<T>| {
        let Some(query_mutations) = &query_mutations else {
            return;
        };
        // once the location's owner is gone (the page was left), there is nothing to set
        let (Some(path), Some(hash), Some(qs)) = (
            location.pathname.try_get_untracked(),
            location.hash.try_get_untracked(),
            location
                .query
                .try_with_untracked(ParamsMap::to_query_string),
        ) else {
            return;
        };
        let new_url = format!("{path}{qs}{hash}");
        query_mutations
            .write_value()
            .push((key.clone(), value.as_ref().map(ToString::to_string)));

        if !IS_NAVIGATING.load(Ordering::Relaxed) {
            IS_NAVIGATING.store(true, Ordering::Relaxed);
            request_animation_frame({
                let navigate = navigate.clone();
                let nav_options = nav_options.clone();
                move || {
                    navigate(&new_url, nav_options.clone());
                    IS_NAVIGATING.store(false, Ordering::Relaxed)
                }
            })
        }
    });

    (get, set)
}

#[track_caller]
pub(crate) fn has_router() -> bool {
    use_context::<RouterContext>().is_some()
}

/*
/// Returns the current [`RouterContext`], containing information about the router's state.
#[track_caller]
pub(crate) fn use_router() -> RouterContext {
    if let Some(router) = use_context::<RouterContext>() {
        router
    } else {
        halyard::logging::debug_warn!(
            "You must call use_router() within a <Router/> component {:?}",
            std::panic::Location::caller()
        );
        panic!("You must call use_router() within a <Router/> component");
    }
}
*/

/// Returns the current [`Location`], which contains reactive variables
///
/// Outside a `<Router>` this is the location `/`, and that is logged once.
#[track_caller]
pub fn use_location() -> Location {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    match use_context::<RouterContext>() {
        Some(RouterContext { location, .. }) => location,
        None => {
            report_once(
                &REPORTED,
                &RouterError::NoRouter {
                    what: "use_location()",
                    instead: "it returns the location `/`",
                },
            );
            Location::new(
                ArcRwSignal::new(Url::root()).read_only(),
                ArcRwSignal::new(State::default()).read_only(),
            )
        }
    }
}

pub(crate) type RawParamsMap = ArcMemo<ParamsMap>;

#[track_caller]
fn use_params_raw() -> RawParamsMap {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    use_context().unwrap_or_else(|| {
        report_once(
            &REPORTED,
            &RouterError::NoMatchedRoute {
                what: "use_params() or use_params_map()",
                instead: "there are no params",
            },
        );
        ArcMemo::new(|_| ParamsMap::new())
    })
}

/// Returns a raw key-value map of route params.
///
/// Outside a matched route the map is empty, and that is logged once.
#[track_caller]
pub fn use_params_map() -> Memo<ParamsMap> {
    use_params_raw().into()
}

/// Returns the current route params, parsed into the given type, or an error.
#[track_caller]
pub fn use_params<T>() -> Memo<Result<T, ParamsError>>
where
    T: Params + PartialEq + Send + Sync + 'static,
{
    // TODO this can be optimized in future to map over the signal, rather than cloning
    let params = use_params_raw();
    Memo::new(move |_| params.with(T::from_map))
}

#[track_caller]
fn use_url_raw() -> ArcRwSignal<Url> {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    use_context().unwrap_or_else(|| match use_context::<RouterContext>() {
        Some(RouterContext { current_url, .. }) => current_url,
        None => {
            report_once(
                &REPORTED,
                &RouterError::NoRouter {
                    what: "use_url(), use_query() or use_query_map()",
                    instead: "the URL is `/`, with no query",
                },
            );
            ArcRwSignal::new(Url::root())
        }
    })
}

/// Gives reactive access to the current URL.
///
/// Outside a `<Router>` this is the URL `/`, and that is logged once.
#[track_caller]
pub fn use_url() -> ReadSignal<Url> {
    use_url_raw().read_only().into()
}

/// Returns a raw key-value map of the URL search query.
#[track_caller]
pub fn use_query_map() -> Memo<ParamsMap> {
    let url = use_url_raw();
    Memo::new(move |_| url.with(|url| url.search_params().clone()))
}

/// Returns the current URL search query, parsed into the given type, or an error.
#[track_caller]
pub fn use_query<T>() -> Memo<Result<T, ParamsError>>
where
    T: Params + PartialEq + Send + Sync + 'static,
{
    let url = use_url_raw();
    Memo::new(move |_| url.with(|url| T::from_map(url.search_params())))
}

#[derive(Debug, Clone)]
pub(crate) struct Matched(pub ArcMemo<String>);

/// Resolves the given path relative to the current route (outside a router, relative to
/// `/`).
#[track_caller]
pub(crate) fn use_resolved_path(
    path: impl Fn() -> String + Send + Sync + 'static,
) -> ArcMemo<String> {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    let router = use_context::<RouterContext>();
    if router.is_none() {
        report_once(
            &REPORTED,
            &RouterError::NoRouter {
                what: "a link (<A/>)",
                instead:
                    "a relative href is resolved from `/`, and the browser \
                          loads the page from the server",
            },
        );
    }
    // TODO make this work with flat routes too?
    let matched = use_context::<Matched>().map(|n| n.0);
    ArcMemo::new(move |_| {
        let path = path();
        if path.starts_with('/') {
            return path;
        }
        match &router {
            Some(router) => router
                .resolve_path(
                    &path,
                    matched.as_ref().map(|n| n.get()).as_deref(),
                )
                .to_string(),
            None => resolve_path("", &path, None).to_string(),
        }
    })
}

/// Returns a function that can be used to navigate to a new route.
///
/// This should only be called on the client; it does nothing during
/// server rendering. Outside a `<Router>` the function it returns does nothing, and logs
/// each call.
///
/// ```rust
/// # if false { // can't actually navigate, no <Router/>
/// let navigate = halyard::router::hooks::use_navigate();
/// navigate("/", Default::default());
/// # }
/// ```
#[track_caller]
pub fn use_navigate() -> impl Fn(&str, NavigateOptions) + Clone {
    let cx = use_context::<RouterContext>();
    move |path: &str, options: NavigateOptions| match &cx {
        Some(cx) => cx.navigate(path, options),
        None => report(&RouterError::NavigateWithoutRouter {
            path: path.to_owned(),
        }),
    }
}

/// Returns a reactive string that contains the route that was matched for
/// this [`Route`](crate::router::components::Route).
///
/// Outside a matched route the string is empty, and that is logged once.
#[track_caller]
pub fn use_matched() -> Memo<String> {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    match use_context::<Matched>() {
        Some(Matched(matched)) => matched.into(),
        None => {
            report_once(
                &REPORTED,
                &RouterError::NoMatchedRoute {
                    what: "use_matched()",
                    instead: "the matched path is empty",
                },
            );
            ArcMemo::new(|_| String::new()).into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use halyard_reactive_graph::owner::Owner;

    // Each hook used to `expect` the router's context (or a matched route's) and panic
    // without it. Outside a router they now return inert values: the URL `/`, empty maps,
    // and a navigate function that does nothing.

    #[test]
    fn use_navigate_outside_a_router_does_nothing() {
        let owner = Owner::new();
        owner.with(|| {
            let navigate = use_navigate();
            navigate("/elsewhere", NavigateOptions::default());
        });
    }

    #[test]
    fn use_location_outside_a_router_is_root() {
        let owner = Owner::new();
        owner.with(|| {
            let location = use_location();
            assert_eq!(
                location.pathname.try_get_untracked(),
                Some("/".to_string())
            );
            assert_eq!(
                location.search.try_get_untracked(),
                Some("".to_string())
            );
            assert_eq!(
                location.query.try_get_untracked(),
                Some(ParamsMap::new())
            );
        });
    }

    #[test]
    fn use_url_outside_a_router_is_root() {
        let owner = Owner::new();
        owner.with(|| {
            assert_eq!(use_url().try_get_untracked().unwrap().path(), "/");
        });
    }

    #[test]
    fn use_query_outside_a_router_is_empty() {
        let owner = Owner::new();
        owner.with(|| {
            assert_eq!(
                use_query_map().try_get_untracked(),
                Some(ParamsMap::new())
            );
            assert_eq!(use_query::<()>().try_get_untracked(), Some(Ok(())));
        });
    }

    #[test]
    fn use_params_outside_a_route_is_empty() {
        let owner = Owner::new();
        owner.with(|| {
            assert_eq!(
                use_params_map().try_get_untracked(),
                Some(ParamsMap::new())
            );
            assert_eq!(use_params::<()>().try_get_untracked(), Some(Ok(())));
        });
    }

    #[test]
    fn use_matched_outside_a_route_is_empty() {
        let owner = Owner::new();
        owner.with(|| {
            assert_eq!(use_matched().try_get_untracked(), Some("".to_string()));
        });
    }

    #[test]
    fn use_resolved_path_outside_a_router_resolves_from_root() {
        let owner = Owner::new();
        owner.with(|| {
            let path = use_resolved_path(|| "reports/1".to_string());
            assert_eq!(
                path.try_get_untracked(),
                Some("/reports/1".to_string())
            );
            let path = use_resolved_path(|| "/c".to_string());
            assert_eq!(path.try_get_untracked(), Some("/c".to_string()));
        });
    }

    /// `query_signal` used `expect_context`, the same panic behind another name.
    #[test]
    fn query_signal_outside_a_router_reads_nothing() {
        let owner = Owner::new();
        owner.with(|| {
            let (page, _set_page) = query_signal::<u32>("page");
            assert_eq!(page.try_get_untracked(), Some(None));
        });
    }
}
