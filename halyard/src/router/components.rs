pub use super::{form::*, link::*};
#[cfg(feature = "ssr")]
use crate::router::location::RequestUrl;
pub use crate::router::nested_router::Outlet;
use crate::router::{
    error::{js_reason, report, report_once, RouterError},
    flat_router::FlatRoutesView,
    hooks::{use_navigate, Matched},
    location::{
        BrowserUrl, Location, LocationChange, LocationProvider, State, Url,
    },
    navigate::NavigateOptions,
    nested_router::NestedRoutesView,
    resolve_path::resolve_path,
    ChooseView, MatchNestedRoutes, NestedRoute, PossibleRouteMatch, RouteDefs,
    SsrMode,
};
use halyard::{children, prelude::*};
use halyard_reactive_graph::{
    owner::{provide_context, use_context, Owner},
    signal::ArcRwSignal,
    traits::{GetUntracked, ReadUntracked, Set},
    wrappers::write::SignalSetter,
};
use halyard_tachys::either::EitherOf3;
use std::{
    borrow::Cow,
    fmt::{Debug, Display},
    mem,
    sync::{atomic::AtomicBool, Arc},
    time::Duration,
};

/// A wrapper that allows passing route definitions as children to a component like [`Routes`],
/// [`FlatRoutes`], [`ParentRoute`], or [`ProtectedParentRoute`].
#[derive(Clone, Debug)]
pub struct RouteChildren<Children>(Children);

impl<Children> RouteChildren<Children> {
    /// Extracts the inner route definition.
    pub fn into_inner(self) -> Children {
        self.0
    }
}

impl<F, Children> ToChildren<F> for RouteChildren<Children>
where
    F: FnOnce() -> Children,
{
    fn to_children(f: F) -> Self {
        RouteChildren(f())
    }
}

#[component(transparent)]
pub fn Router<Chil>(
    /// The base URL for the router. Defaults to `""`.
    #[prop(optional, into)]
    base: Option<Cow<'static, str>>,
    /// A signal that will be set while the navigation process is underway.
    #[prop(optional, into)]
    set_is_routing: Option<SignalSetter<bool>>,
    // TODO trailing slashes
    ///// How trailing slashes should be handled in [`Route`] paths.
    //#[prop(optional)]
    //trailing_slash: TrailingSlash,
    /// The `<Router/>` should usually wrap your whole page. It can contain
    /// any elements, and should include a [`Routes`] component somewhere
    /// to define and display [`Route`]s.
    children: TypedChildren<Chil>,
) -> impl IntoView
where
    Chil: IntoView,
{
    #[cfg(feature = "ssr")]
    let (location_provider, current_url) = {
        let current_url =
            ArcRwSignal::new(request_url(use_context::<RequestUrl>()));

        (None, current_url)
    };

    #[cfg(not(feature = "ssr"))]
    let (location_provider, current_url) = {
        // TODO options here
        let location = match BrowserUrl::new() {
            Ok(location) => {
                location.init(base.clone());
                provide_context(location.clone());
                Some(location)
            }
            Err(error) => {
                report(&RouterError::Browser {
                    action: "reading the browser's location",
                    reason: js_reason(&error),
                    instead: "rendering the page for `/`, without client-side \
                              navigation (links load pages from the server)",
                });
                None
            }
        };
        let current_url = location.as_ref().map_or_else(
            || ArcRwSignal::new(Url::root()),
            |location| location.as_url().clone(),
        );

        (location, current_url)
    };
    // provide router context
    let state = ArcRwSignal::new(State::new(None));
    let location = Location::new(current_url.read_only(), state.read_only());

    provide_context(RouterContext {
        base,
        current_url,
        location,
        state,
        set_is_routing,
        query_mutations: Default::default(),
        location_provider,
    });

    let children = children.into_inner();
    children()
}

/// The URL of the request being rendered on the server, or `/` if there is none.
#[cfg(feature = "ssr")]
fn request_url(request_url: Option<RequestUrl>) -> Url {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    let Some(request_url) = request_url else {
        report_once(&REPORTED, &RouterError::NoRequestUrl);
        return Url::root();
    };
    request_url.parse().unwrap_or_else(|source| {
        report(&RouterError::UnparsableRequestUrl {
            url: request_url.as_ref().to_owned(),
            source,
        });
        Url::root()
    })
}

/// The router's context, and the owner it was found through: contexts are only found
/// through the current owner, so without an owner there is no router either.
fn router_and_owner() -> Option<(RouterContext, Owner)> {
    let owner = Owner::current()?;
    let router = use_context::<RouterContext>()?;
    Some((router, owner))
}

#[derive(Clone)]
pub(crate) struct RouterContext {
    pub base: Option<Cow<'static, str>>,
    pub current_url: ArcRwSignal<Url>,
    pub location: Location,
    pub state: ArcRwSignal<State>,
    pub set_is_routing: Option<SignalSetter<bool>>,
    pub query_mutations:
        ArcStoredValue<Vec<(Oco<'static, str>, Option<String>)>>,
    pub location_provider: Option<BrowserUrl>,
}

impl RouterContext {
    pub fn navigate(&self, path: &str, options: NavigateOptions) {
        // there is no browser to navigate during server rendering (as `use_navigate`
        // documents); parsing the path needs the browser's `window`
        if cfg!(feature = "ssr") {
            return;
        }
        let current = self.current_url.read_untracked();
        let resolved_to = if options.resolve {
            resolve_path(
                self.base.as_deref().unwrap_or_default(),
                path,
                // TODO this should be relative to the current *Route*, I think...
                Some(current.path()),
            )
        } else {
            resolve_path("", path, None)
        };

        let mut url = match BrowserUrl::parse(&resolved_to) {
            Ok(url) => url,
            Err(e) => {
                halyard::logging::error!("Error parsing URL: {e:?}");
                return;
            }
        };
        let query_mutations =
            mem::take(&mut *self.query_mutations.write_value());
        if !query_mutations.is_empty() {
            for (key, value) in query_mutations {
                if let Some(value) = value {
                    url.search_params_mut().replace(key, value);
                } else {
                    url.search_params_mut().remove(&key);
                }
            }
            *url.search_mut() = url
                .search_params()
                .to_query_string()
                .trim_start_matches('?')
                .into()
        }

        if url.origin() != current.origin() {
            if let Err(error) = window().location().set_href(path) {
                report(&RouterError::Browser {
                    action: "loading a page from another origin",
                    reason: js_reason(&error),
                    instead: "staying on this page",
                });
            }
            return;
        }

        // update state signal, if necessary
        if options.state != self.state.get_untracked() {
            self.state.set(options.state.clone());
        }

        // update URL signal, if necessary
        let value = url.to_full_path();
        if current != url {
            drop(current);
            self.current_url.set(url);
        }

        if let Some(location_provider) = &self.location_provider {
            location_provider.complete_navigation(&LocationChange {
                value,
                replace: options.replace,
                scroll: options.scroll,
                state: options.state,
            });
        }
    }

    pub fn resolve_path<'a>(
        &'a self,
        path: &'a str,
        from: Option<&'a str>,
    ) -> Cow<'a, str> {
        let base = self.base.as_deref().unwrap_or_default();
        resolve_path(base, path, from)
    }
}

impl Debug for RouterContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouterContext")
            .field("base", &self.base)
            .field("current_url", &self.current_url)
            .field("location", &self.location)
            .finish_non_exhaustive()
    }
}

#[component(transparent)]
pub fn Routes<Defs, FallbackFn, Fallback>(
    /// A function that returns the view that should be shown if no route is matched.
    fallback: FallbackFn,
    /// Whether to use the View Transition API during navigation.
    #[prop(optional)]
    transition: bool,
    /// The route definitions. This should consist of one or more [`ParentRoute`] or [`Route`]
    /// components.
    children: RouteChildren<Defs>,
) -> impl IntoView
where
    Defs: MatchNestedRoutes + Clone + Send + 'static,
    FallbackFn: FnOnce() -> Fallback + Clone + Send + 'static,
    Fallback: IntoView + 'static,
{
    static REPORTED: AtomicBool = AtomicBool::new(false);
    let location = use_context::<BrowserUrl>();
    let Some((
        RouterContext {
            current_url,
            base,
            set_is_routing,
            ..
        },
        outer_owner,
    )) = router_and_owner()
    else {
        report_once(
            &REPORTED,
            &RouterError::NoRouter {
                what: "<Routes/>",
                instead: "it renders nothing",
            },
        );
        return None;
    };
    let base = base.map(|base| {
        let mut base = Oco::from(base);
        base.upgrade_inplace();
        base
    });
    let routes = RouteDefs::new_with_base(
        children.into_inner(),
        base.clone().unwrap_or_default(),
    );
    Some(move || {
        current_url.track();
        NestedRoutesView {
            location: location.clone(),
            routes: routes.clone(),
            outer_owner: outer_owner.clone(),
            current_url: current_url.clone(),
            base: base.clone(),
            fallback: fallback.clone(),
            set_is_routing,
            transition,
        }
    })
}

#[component(transparent)]
pub fn FlatRoutes<Defs, FallbackFn, Fallback>(
    /// A function that returns the view that should be shown if no route is matched.
    fallback: FallbackFn,
    /// Whether to use the View Transition API during navigation.
    #[prop(optional)]
    transition: bool,
    /// The route definitions. This should consist of one or more [`ParentRoute`] or [`Route`]
    /// components.
    children: RouteChildren<Defs>,
) -> impl IntoView
where
    Defs: MatchNestedRoutes + Clone + Send + 'static,
    FallbackFn: FnOnce() -> Fallback + Clone + Send + 'static,
    Fallback: IntoView + 'static,
{
    static REPORTED: AtomicBool = AtomicBool::new(false);
    let location = use_context::<BrowserUrl>();
    let Some((
        RouterContext {
            current_url,
            base,
            set_is_routing,
            ..
        },
        outer_owner,
    )) = router_and_owner()
    else {
        report_once(
            &REPORTED,
            &RouterError::NoRouter {
                what: "<FlatRoutes/>",
                instead: "it renders nothing",
            },
        );
        return None;
    };

    // TODO base
    #[allow(unused)]
    let base = base.map(|base| {
        let mut base = Oco::from(base);
        base.upgrade_inplace();
        base
    });
    let routes = RouteDefs::new_with_base(
        children.into_inner(),
        base.clone().unwrap_or_default(),
    );

    Some(move || {
        current_url.track();
        FlatRoutesView {
            current_url: current_url.clone(),
            location: location.clone(),
            routes: routes.clone(),
            fallback: fallback.clone(),
            outer_owner: outer_owner.clone(),
            set_is_routing,
            transition,
        }
    })
}

/// Describes a portion of the nested layout of the app, specifying the route it should match
/// and the element it should display.
#[component(transparent)]
pub fn Route<Segments, View>(
    /// The path fragment that this route should match. This can be created using the
    /// [`path`](crate::router::path) macro, or path segments ([`StaticSegment`](crate::router::StaticSegment),
    /// [`ParamSegment`](crate::router::ParamSegment), [`WildcardSegment`](crate::router::WildcardSegment), and
    /// [`OptionalParamSegment`](crate::router::OptionalParamSegment)).
    path: Segments,
    /// The view for this route.
    view: View,
    /// The mode that this route prefers during server-side rendering.
    /// Defaults to out-of-order streaming.
    #[prop(optional)]
    ssr: SsrMode,
) -> <NestedRoute<Segments, (), (), View> as IntoMaybeErased>::Output
where
    View: ChooseView + Clone + 'static,
    Segments: PossibleRouteMatch + Clone + Send + 'static,
{
    NestedRoute::new(path, view)
        .ssr_mode(ssr)
        .into_maybe_erased()
}

/// Describes a portion of the nested layout of the app, specifying the route it should match
/// and the element it should display.
#[component(transparent)]
pub fn ParentRoute<Segments, View, Children>(
    /// The path fragment that this route should match. This can be created using the
    /// [`path`](crate::router::path) macro, or path segments ([`StaticSegment`](crate::router::StaticSegment),
    /// [`ParamSegment`](crate::router::ParamSegment), [`WildcardSegment`](crate::router::WildcardSegment), and
    /// [`OptionalParamSegment`](crate::router::OptionalParamSegment)).
    path: Segments,
    /// The view for this route.
    view: View,
    /// Nested child routes.
    children: RouteChildren<Children>,
    /// The mode that this route prefers during server-side rendering.
    /// Defaults to out-of-order streaming.
    #[prop(optional)]
    ssr: SsrMode,
) -> <NestedRoute<Segments, Children, (), View> as IntoMaybeErased>::Output
where
    View: ChooseView + Clone + 'static,
    Children: MatchNestedRoutes + Send + Clone + 'static,
    Segments: PossibleRouteMatch + Clone + Send + 'static,
{
    let children = children.into_inner();
    NestedRoute::new(path, view)
        .ssr_mode(ssr)
        .child(children)
        .into_maybe_erased()
}

/// With the `impl Fn` in the return signature, IntoMaybeErased::Output isn't accepted by the compiler, so changing return type depending on the erasure flag.
macro_rules! define_protected_route {
    ($ret:ty) => {
        /// Describes a route that is guarded by a certain condition. This works the same way as
        /// [`<Route/>`], except that if the `condition` function evaluates to `Some(false)`, it
        /// redirects to `redirect_path` instead of displaying its `view`.
        #[component(transparent)]
        pub fn ProtectedRoute<Segments, ViewFn, View, C, PathFn, P>(
            /// The path fragment that this route should match. This can be created using the
            /// [`path`](crate::router::path) macro, or path segments ([`StaticSegment`](crate::router::StaticSegment),
            /// [`ParamSegment`](crate::router::ParamSegment), [`WildcardSegment`](crate::router::WildcardSegment), and
            /// [`OptionalParamSegment`](crate::router::OptionalParamSegment)).
            path: Segments,
            /// The view for this route.
            view: ViewFn,
            /// A function that returns `Option<bool>`, where `Some(true)` means that the user can access
            /// the page, `Some(false)` means the user cannot access the page, and `None` means this
            /// information is still loading.
            condition: C,
            /// The path that will be redirected to if the condition is `Some(false)`.
            redirect_path: PathFn,
            /// Will be displayed while the condition is pending. By default this is the empty view.
            #[prop(optional, into)]
            fallback: children::ViewFn,
            /// The mode that this route prefers during server-side rendering.
            /// Defaults to out-of-order streaming.
            #[prop(optional)]
            ssr: SsrMode,
        ) -> $ret
        where
            Segments: PossibleRouteMatch + Clone + Send + 'static,
            ViewFn: Fn() -> View + Send + Clone + 'static,
            View: IntoView + 'static,
            C: Fn() -> Option<bool> + Send + Clone + 'static,
            PathFn: Fn() -> P + Send + Clone + 'static,
            P: Display + 'static,
        {
            let fallback = move || fallback.run();
            let view = move || {
                let condition = condition.clone();
                let redirect_path = redirect_path.clone();
                let view = view.clone();
                let fallback = fallback.clone();
                (view! {
                    <Transition fallback=fallback.clone()>
                        {move || {
                            let condition = condition();
                            let view = view.clone();
                            let redirect_path = redirect_path.clone();
                            let fallback = fallback.clone();
                            Unsuspend::new(move || match condition {
                                Some(true) => EitherOf3::A(view()),
                                #[allow(clippy::unit_arg)]
                                Some(false) => {
                                    EitherOf3::B(view! { <Redirect path=redirect_path()/> }.into_inner())
                                }
                                None => EitherOf3::C(fallback()),
                            })
                        }}

                    </Transition>
                })
                .into_any()
            };
            NestedRoute::new(path, view).ssr_mode(ssr).into_maybe_erased()
        }
    };
}

#[cfg(erase_components)]
define_protected_route!(crate::router::any_nested_route::AnyNestedRoute);
#[cfg(not(erase_components))]
define_protected_route!(NestedRoute<Segments, (), (), impl Fn() -> AnyView + Send + Clone>);

/// With the `impl Fn` in the return signature, IntoMaybeErased::Output isn't accepted by the compiler, so changing return type depending on the erasure flag.
macro_rules! define_protected_parent_route {
    ($ret:ty) => {
        #[component(transparent)]
        pub fn ProtectedParentRoute<
            Segments,
            ViewFn,
            View,
            C,
            PathFn,
            P,
            Children,
        >(
            /// The path fragment that this route should match. This can be created using the
            /// [`path`](crate::router::path) macro, or path segments ([`StaticSegment`](crate::router::StaticSegment),
            /// [`ParamSegment`](crate::router::ParamSegment), [`WildcardSegment`](crate::router::WildcardSegment), and
            /// [`OptionalParamSegment`](crate::router::OptionalParamSegment)).
            path: Segments,
            /// The view for this route.
            view: ViewFn,
            /// A function that returns `Option<bool>`, where `Some(true)` means that the user can access
            /// the page, `Some(false)` means the user cannot access the page, and `None` means this
            /// information is still loading.
            condition: C,
            /// Will be displayed while the condition is pending. By default this is the empty view.
            #[prop(optional, into)]
            fallback: children::ViewFn,
            /// The path that will be redirected to if the condition is `Some(false)`.
            redirect_path: PathFn,
            /// Nested child routes.
            children: RouteChildren<Children>,
            /// The mode that this route prefers during server-side rendering.
            /// Defaults to out-of-order streaming.
            #[prop(optional)]
            ssr: SsrMode,
        ) -> $ret
        where
            Segments: PossibleRouteMatch + Clone + Send + 'static,
            Children: MatchNestedRoutes + Send + Clone + 'static,
            ViewFn: Fn() -> View + Send + Clone + 'static,
            View: IntoView + 'static,
            C: Fn() -> Option<bool> + Send + Clone + 'static,
            PathFn: Fn() -> P + Send + Clone + 'static,
            P: Display + 'static,
        {
            let fallback = move || fallback.run();
            let children = children.into_inner();
            let view = move || {
                let condition = condition.clone();
                let redirect_path = redirect_path.clone();
                let fallback = fallback.clone();
                let view = view.clone();
                // routes run their views under an owner; without one there is none to
                // restore
                let owner = Owner::current();
                let view = {
                    let fallback = fallback.clone();
                    move || {
                        let condition = condition();
                        let view = view.clone();
                        let redirect_path = redirect_path.clone();
                        let fallback = fallback.clone();
                        let owner = owner.clone();
                        Unsuspend::new(move || match condition {
                            // reset the owner so that things like providing context work
                            // otherwise, this will be a child owner nested within the Transition, not
                            // the parent owner of the Outlet
                            //
                            // clippy: not redundant, a FnOnce vs FnMut issue
                            #[allow(clippy::redundant_closure)]
                            Some(true) => EitherOf3::A(match &owner {
                                Some(owner) => owner.with(|| view()),
                                None => view(),
                            }),
                            #[allow(clippy::unit_arg)]
                            Some(false) => EitherOf3::B(
                                view! { <Redirect path=redirect_path()/> }
                                    .into_inner(),
                            ),
                            None => EitherOf3::C(fallback()),
                        })
                    }
                };
                (view! { <Transition fallback>{view}</Transition> }).into_any()
            };
            NestedRoute::new(path, view)
                .ssr_mode(ssr)
                .child(children)
                .into_maybe_erased()
        }
    };
}

#[cfg(erase_components)]
define_protected_parent_route!(crate::router::any_nested_route::AnyNestedRoute);
#[cfg(not(erase_components))]
define_protected_parent_route!(NestedRoute<Segments, Children, (), impl Fn() -> AnyView + Send + Clone>);

/// Redirects the user to a new URL, whether on the client side or on the server
/// side. If rendered on the server, this sets a `302` status code and sets a `Location`
/// header. If rendered in the browser, it uses client-side navigation to redirect.
/// In either case, it resolves the route relative to the current route. (To use
/// an absolute path, prefix it with `/`).
///
/// **Note**: Support for server-side redirects is provided by the server framework
/// integration (`halyard::axum`, with the `axum` feature). If you’re not using it, you
/// should provide a way of redirecting on the server yourself, with
/// [`provide_server_redirect`].
///
#[component(transparent)]
pub fn Redirect<P>(
    /// The relative path to which the user should be redirected.
    path: P,
    /// Navigation options to be used on the client side.
    #[prop(optional)]
    #[allow(unused)]
    options: Option<NavigateOptions>,
) where
    P: core::fmt::Display + 'static,
{
    // TODO resolve relative path
    let path = path.to_string();

    // redirect on the server
    if let Some(redirect_fn) = use_context::<ServerRedirectFunction>() {
        // outside a matched route (e.g. in a layout), relative to the root
        let matched = use_context::<Matched>()
            .map(|Matched(matched)| matched.get_untracked())
            .unwrap_or_default();
        (redirect_fn.f)(&resolve_path("", &path, Some(&matched)));
    }
    // redirect on the client
    else {
        if cfg!(feature = "ssr") {
            #[cfg(feature = "tracing")]
            tracing::warn!(
                "Calling <Redirect/> without a ServerRedirectFunction \
                 provided, in SSR mode."
            );

            #[cfg(not(feature = "tracing"))]
            eprintln!(
                "Calling <Redirect/> without a ServerRedirectFunction \
                 provided, in SSR mode."
            );
            return;
        }
        let navigate = use_navigate();
        navigate(&path, options.unwrap_or_default());
    }
}

/// Wrapping type for a function provided as context to allow for
/// server-side redirects. See [`provide_server_redirect`]
/// and [`Redirect`].
#[derive(Clone)]
pub struct ServerRedirectFunction {
    f: Arc<dyn Fn(&str) + Send + Sync>,
}

impl core::fmt::Debug for ServerRedirectFunction {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ServerRedirectFunction").finish()
    }
}

/// Provides a function that can be used to redirect the user to another
/// absolute path, on the server. This should set a `302` status code and an
/// appropriate `Location` header.
pub fn provide_server_redirect(handler: impl Fn(&str) + Send + Sync + 'static) {
    provide_context(ServerRedirectFunction {
        f: Arc::new(handler),
    })
}

/// A visible indicator that the router is in the process of navigating
/// to another route.
///
/// This is used when `<Router set_is_routing>` has been provided, to
/// provide some visual indicator that the page is currently loading
/// async data, so that it is does not appear to have frozen. It can be
/// styled independently.
#[component]
pub fn RoutingProgress(
    /// Whether the router is currently loading the new page.
    #[prop(into)]
    is_routing: Signal<bool>,
    /// The maximum expected time for loading, which is used to
    /// calibrate the animation process.
    #[prop(optional, into)]
    max_time: std::time::Duration,
    /// The time to show the full progress bar after page has loaded, before hiding it. (Defaults to 100ms.)
    #[prop(default = std::time::Duration::from_millis(250))]
    before_hiding: std::time::Duration,
) -> impl IntoView {
    const INCREMENT_EVERY_MS: f32 = 5.0;
    let expected_increments =
        max_time.as_secs_f32() / (INCREMENT_EVERY_MS / 1000.0);
    let percent_per_increment = 100.0 / expected_increments;

    let (is_showing, set_is_showing) = signal(false);
    let (progress, set_progress) = signal(0.0);

    StoredValue::new(RenderEffect::new(
        move |prev: Option<Option<IntervalHandle>>| {
            if is_routing.get() && !is_showing.get() {
                set_is_showing.set(true);
                set_interval_with_handle(
                    move || {
                        set_progress.update(|n| *n += percent_per_increment);
                    },
                    Duration::from_millis(INCREMENT_EVERY_MS as u64),
                )
                .ok()
            } else if is_routing.get() && is_showing.get() {
                set_progress.set(0.0);
                prev?
            } else {
                set_progress.set(100.0);
                set_timeout(
                    move || {
                        set_progress.set(0.0);
                        set_is_showing.set(false);
                    },
                    before_hiding,
                );
                if let Some(Some(interval)) = prev {
                    interval.clear();
                }
                None
            }
        },
    ));

    view! {
        <Show when=move || is_showing.get() fallback=|| ()>
            <progress min="0" max="100" value=move || progress.get()></progress>
        </Show>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::StaticSegment;

    fn without_markers(html: &str) -> String {
        html.replace("<!>", "")
    }

    /// `<Routes/>` outside a `<Router/>` used to panic; it renders nothing.
    #[test]
    fn routes_outside_a_router_render_nothing() {
        let owner = Owner::new();
        owner.with(|| {
            let html = view! {
                <Routes fallback=|| "not found">
                    <Route path=StaticSegment("page") view=|| "page"/>
                </Routes>
            }
            .to_html();
            assert_eq!(without_markers(&html), "");
        });
    }

    #[test]
    fn flat_routes_outside_a_router_render_nothing() {
        let owner = Owner::new();
        owner.with(|| {
            let html = view! {
                <FlatRoutes fallback=|| "not found">
                    <Route path=StaticSegment("page") view=|| "page"/>
                </FlatRoutes>
            }
            .to_html();
            assert_eq!(without_markers(&html), "");
        });
    }

    #[test]
    fn routes_outside_any_owner_render_nothing() {
        assert!(Owner::current().is_none());
        let html = view! {
            <Routes fallback=|| "not found">
                <Route path=StaticSegment("page") view=|| "page"/>
            </Routes>
        }
        .to_html();
        assert_eq!(without_markers(&html), "");
    }

    #[cfg(feature = "ssr")]
    fn router_path_and_html() -> (String, String) {
        let view = view! {
            <Router>
                <Routes fallback=|| "not found">
                    <Route path=StaticSegment("page") view=|| "page"/>
                </Routes>
            </Router>
        };
        let path = use_context::<RouterContext>()
            .map(|router| router.current_url.get_untracked().path().to_string())
            .unwrap_or_default();
        (path, view.to_html())
    }

    /// On the server `<Router/>` reads the request's URL from context. Rendered without
    /// one (outside a server integration) it used to panic; it renders the page for `/`.
    #[cfg(feature = "ssr")]
    #[test]
    fn router_without_a_request_url_renders_the_page_for_root() {
        let owner = Owner::new();
        owner.with(|| {
            let (path, html) = router_path_and_html();
            assert_eq!(path, "/");
            assert_eq!(without_markers(&html), "not found");
        });
    }

    #[cfg(feature = "ssr")]
    #[test]
    fn router_with_an_unparsable_request_url_renders_the_page_for_root() {
        let owner = Owner::new();
        owner.with(|| {
            provide_context(RequestUrl::new("http://[::1"));
            let (path, html) = router_path_and_html();
            assert_eq!(path, "/");
            assert_eq!(without_markers(&html), "not found");
        });
    }

    #[cfg(feature = "ssr")]
    #[test]
    fn router_renders_the_request_url() {
        let owner = Owner::new();
        owner.with(|| {
            provide_context(RequestUrl::new("/nothing/here?q=1"));
            let (path, html) = router_path_and_html();
            assert_eq!(path, "/nothing/here");
            assert_eq!(without_markers(&html), "not found");
        });
    }

    /// There is no browser during server rendering: navigating parsed the path with the
    /// browser's `window`, which panics on the server. It does nothing, as `use_navigate`
    /// documents.
    #[cfg(feature = "ssr")]
    #[test]
    fn navigating_during_server_rendering_does_nothing() {
        let owner = Owner::new();
        owner.with(|| {
            provide_context(RequestUrl::new("/other"));
            let (path, _html) = router_path_and_html();
            assert_eq!(path, "/other");
            let navigate = use_navigate();
            navigate("/elsewhere", NavigateOptions::default());
            let path = use_context::<RouterContext>().map(|router| {
                router.current_url.get_untracked().path().to_string()
            });
            assert_eq!(path.as_deref(), Some("/other"));
        });
    }

    /// Attributes spread onto the router reach `<Routes/>`, whose `add_any_attr` was
    /// `todo!()`. They are ignored.
    #[cfg(feature = "ssr")]
    #[test]
    fn attributes_spread_onto_the_router_are_ignored() {
        use halyard::tachys::html::class::class;

        let owner = Owner::new();
        owner.with(|| {
            provide_context(RequestUrl::new("/other"));
            let html = view! {
                <Router>
                    <Routes fallback=|| "not found">
                        <Route path=StaticSegment("page") view=|| "page"/>
                    </Routes>
                </Router>
            }
            .add_any_attr(class("x"))
            .to_html();
            assert_eq!(without_markers(&html), "not found");
        });
    }
}
