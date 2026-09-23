#![allow(missing_docs)]

mod any_choose_view;
mod choose_view;
mod path_segment;
pub(crate) mod resolve_path;
pub use choose_view::*;
pub use path_segment::*;
mod horizontal;
mod nested;
mod vertical;
use crate::{static_routes::RegenerationFn, Method, SsrMode};
pub use horizontal::*;
pub use nested::*;
use std::{borrow::Cow, collections::HashSet, sync::atomic::Ordering};
pub use vertical::*;

#[derive(Debug)]
pub struct RouteDefs<Children> {
    base: Option<Cow<'static, str>>,
    children: Children,
}

impl<Children> Clone for RouteDefs<Children>
where
    Children: Clone,
{
    fn clone(&self) -> Self {
        Self {
            base: self.base.clone(),
            children: self.children.clone(),
        }
    }
}

impl<Children> RouteDefs<Children> {
    pub fn new(children: Children) -> Self {
        Self {
            base: None,
            children,
        }
    }

    pub fn new_with_base(
        children: Children,
        base: impl Into<Cow<'static, str>>,
    ) -> Self {
        Self {
            base: Some(base.into()),
            children,
        }
    }
}

impl<Children> RouteDefs<Children>
where
    Children: MatchNestedRoutes,
{
    pub fn match_route(&self, path: &str) -> Option<Children::Match> {
        let path = match &self.base {
            None => path,
            Some(base) => {
                let (base, path) = if base.starts_with('/') {
                    (base.trim_start_matches('/'), path.trim_start_matches('/'))
                } else {
                    (base.as_ref(), path)
                };
                path.strip_prefix(base)?
            }
        };

        let (matched, remaining) = self.children.match_nested(path);
        let matched = matched?;

        if !(remaining.is_empty() || remaining == "/") {
            None
        } else {
            Some(matched.1)
        }
    }

    pub fn generate_routes(
        &self,
    ) -> (
        Option<&str>,
        impl IntoIterator<Item = GeneratedRouteData> + '_,
    ) {
        (self.base.as_deref(), self.children.generate_routes())
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct RouteMatchId(pub(crate) u16);

impl RouteMatchId {
    /// Creates a new match ID based on the current route ID used in nested route generation.
    ///
    /// In general, you do not need this; it should only be used for custom route matching behavior
    /// in a library that creates its own route types.
    pub fn new_from_route_id() -> RouteMatchId {
        RouteMatchId(ROUTE_ID.fetch_add(1, Ordering::Relaxed))
    }
}

pub trait MatchInterface {
    type Child: MatchInterface + MatchParams + 'static;

    fn as_id(&self) -> RouteMatchId;

    fn as_matched(&self) -> &str;

    fn into_view_and_child(self) -> (impl ChooseView, Option<Self::Child>);
}

pub trait MatchParams {
    fn to_params(&self) -> Vec<(Cow<'static, str>, String)>;
}

pub trait MatchNestedRoutes {
    type Data;
    type Match: MatchInterface + MatchParams;

    /// Matches nested routes
    ///
    /// # Arguments
    ///
    /// * path - A path which is being navigated to
    ///
    /// # Returns
    ///
    /// Tuple where
    ///
    /// * 0 - If match has been found `Some` containing tuple where
    ///     * 0 - [RouteMatchId] identifying the matching route
    ///     * 1 - [Self::Match] matching route
    /// * 1 - Remaining path
    fn match_nested<'a>(
        &'a self,
        path: &'a str,
    ) -> (Option<(RouteMatchId, Self::Match)>, &'a str);

    fn generate_routes(
        &self,
    ) -> impl IntoIterator<Item = GeneratedRouteData> + '_;

    fn optional(&self) -> bool;
}

#[derive(Default, Debug, PartialEq)]
pub struct GeneratedRouteData {
    pub segments: Vec<PathSegment>,
    pub ssr_mode: SsrMode,
    pub methods: HashSet<Method>,
    pub regenerate: Vec<RegenerationFn>,
}

#[cfg(test)]
mod tests {
    use super::{NestedRoute, ParamSegment, RouteDefs};
    use crate::{
        matching::MatchParams, MatchInterface, OptionalParamSegment,
        PathSegment, StaticSegment, WildcardSegment,
    };
    use halyard_either_of::{Either, EitherOf4};

    #[test]
    pub fn matches_single_root_route() {
        let routes =
            RouteDefs::<_>::new(NestedRoute::new(StaticSegment("/"), || ()));
        let matched = routes.match_route("/");
        assert!(matched.is_some());
        // this case seems like it should match, but implementing it interferes with
        // handling trailing slash requirements accurately -- paths for the root are "/",
        // not "", in any case
        let matched = routes.match_route("");
        assert!(matched.is_none());
        let (base, paths) = routes.generate_routes();
        assert_eq!(base, None);
        let paths = paths.into_iter().map(|g| g.segments).collect::<Vec<_>>();
        assert_eq!(paths, vec![vec![PathSegment::Static("/".into())]]);
    }

    #[test]
    pub fn matches_nested_route() {
        let routes: RouteDefs<_> = RouteDefs::new(
            NestedRoute::new(StaticSegment(""), || "Home").child(
                NestedRoute::new(
                    (StaticSegment("author"), StaticSegment("contact")),
                    || "Contact Me",
                ),
            ),
        );

        // route generation
        let (base, paths) = routes.generate_routes();
        assert_eq!(base, None);
        let paths = paths.into_iter().map(|g| g.segments).collect::<Vec<_>>();
        assert_eq!(
            paths,
            vec![vec![
                PathSegment::Static("".into()),
                PathSegment::Static("author".into()),
                PathSegment::Static("contact".into())
            ]]
        );

        let matched = routes.match_route("/author/contact").unwrap();
        assert_eq!(MatchInterface::as_matched(&matched), "");
        let (_, child) = MatchInterface::into_view_and_child(matched);
        assert_eq!(
            MatchInterface::as_matched(&child.unwrap()),
            "/author/contact"
        );
    }

    #[test]
    pub fn does_not_match_route_unless_full_param_matches() {
        let routes = RouteDefs::<_>::new((
            NestedRoute::new(StaticSegment("/property-api"), || ()),
            NestedRoute::new(StaticSegment("/property"), || ()),
        ));
        let matched = routes.match_route("/property").unwrap();
        assert!(matches!(matched, Either::Right(_)));
    }

    #[test]
    pub fn does_not_match_incomplete_route() {
        let routes: RouteDefs<_> = RouteDefs::new(
            NestedRoute::new(StaticSegment(""), || "Home").child(
                NestedRoute::new(
                    (StaticSegment("author"), StaticSegment("contact")),
                    || "Contact Me",
                ),
            ),
        );
        let matched = routes.match_route("/");
        assert!(matched.is_none());
    }

    #[test]
    pub fn chooses_between_nested_routes() {
        let routes: RouteDefs<_> = RouteDefs::new((
            NestedRoute::new(StaticSegment("/"), || ()).child((
                NestedRoute::new(StaticSegment(""), || ()),
                NestedRoute::new(StaticSegment("about"), || ()),
            )),
            NestedRoute::new(StaticSegment("/blog"), || ()).child((
                NestedRoute::new(StaticSegment(""), || ()),
                NestedRoute::new(
                    (StaticSegment("post"), ParamSegment("id")),
                    || (),
                ),
            )),
        ));

        // generates routes correctly
        let (base, paths) = routes.generate_routes();
        assert_eq!(base, None);
        let paths = paths.into_iter().map(|g| g.segments).collect::<Vec<_>>();
        assert_eq!(
            paths,
            vec![
                vec![
                    PathSegment::Static("/".into()),
                    PathSegment::Static("".into()),
                ],
                vec![
                    PathSegment::Static("/".into()),
                    PathSegment::Static("about".into())
                ],
                vec![
                    PathSegment::Static("/blog".into()),
                    PathSegment::Static("".into()),
                ],
                vec![
                    PathSegment::Static("/blog".into()),
                    PathSegment::Static("post".into()),
                    PathSegment::Param("id".into())
                ]
            ]
        );

        let matched = routes.match_route("/about").unwrap();
        let params = matched.to_params();
        assert!(params.is_empty());
        let matched = routes.match_route("/blog").unwrap();
        let params = matched.to_params();
        assert!(params.is_empty());
        let matched = routes.match_route("/blog/post/42").unwrap();
        let params = matched.to_params();
        assert_eq!(params, vec![("id".into(), "42".into())]);
    }

    #[test]
    pub fn arbitrary_nested_routes() {
        let routes: RouteDefs<_> = RouteDefs::new_with_base(
            (
                NestedRoute::new(StaticSegment("/"), || ()).child((
                    NestedRoute::new(StaticSegment("/"), || ()),
                    NestedRoute::new(StaticSegment("about"), || ()),
                )),
                NestedRoute::new(StaticSegment("/blog"), || ()).child((
                    NestedRoute::new(StaticSegment(""), || ()),
                    NestedRoute::new(StaticSegment("category"), || ()),
                    NestedRoute::new(
                        (StaticSegment("post"), ParamSegment("id")),
                        || (),
                    ),
                )),
                NestedRoute::new(
                    (StaticSegment("/contact"), WildcardSegment("any")),
                    || (),
                ),
            ),
            "/portfolio",
        );

        // generates routes correctly
        let (base, _paths) = routes.generate_routes();
        assert_eq!(base, Some("/portfolio"));

        let matched = routes.match_route("/about");
        assert!(matched.is_none());

        let matched = routes.match_route("/portfolio/about").unwrap();
        let params = matched.to_params();
        assert!(params.is_empty());

        let matched = routes.match_route("/portfolio/blog/post/42").unwrap();
        let params = matched.to_params();
        assert_eq!(params, vec![("id".into(), "42".into())]);

        let matched = routes.match_route("/portfolio/contact").unwrap();
        let params = matched.to_params();
        assert_eq!(params, vec![("any".into(), "".into())]);

        let matched = routes.match_route("/portfolio/contact/foobar").unwrap();
        let params = matched.to_params();
        assert_eq!(params, vec![("any".into(), "foobar".into())]);
    }

    #[test]
    pub fn dont_match_smooshed_static_segments() {
        let routes = RouteDefs::<_>::new((
            NestedRoute::new(StaticSegment(""), || ()),
            NestedRoute::new(StaticSegment("users"), || ()),
            NestedRoute::new(
                (StaticSegment("users"), StaticSegment("id")),
                || (),
            ),
            NestedRoute::new(WildcardSegment("any"), || ()),
        ));

        let matched = routes.match_route("/users");
        assert!(matches!(matched, Some(EitherOf4::B(..))));

        let matched = routes.match_route("/users/id");
        assert!(matches!(matched, Some(EitherOf4::C(..))));

        let matched = routes.match_route("/usersid");
        assert!(matches!(matched, Some(EitherOf4::D(..))));
    }

    /// An optional parent whose child matches only once the parent has given its part of
    /// the path back: the parent is re-tested on what is left before the child, and a
    /// parent that cannot match that (its fixed part is in the child's match) used to be
    /// unwrapped, a panic. That combination is no match.
    #[test]
    pub fn optional_parent_that_cannot_rematch_is_no_match() {
        let routes = RouteDefs::<_>::new(
            NestedRoute::new(
                (StaticSegment("a"), OptionalParamSegment("b")),
                || (),
            )
            .child(NestedRoute::new(StaticSegment("a"), || ())),
        );
        assert!(routes.match_route("/a").is_none());
        assert!(routes.match_route("/a/").is_none());
        let matched = routes.match_route("/a/x/a").expect("parent and child");
        assert_eq!(matched.to_params(), vec![("b".into(), "x".into())]);
    }

    /// The optional-parent fallback that does work: the child takes the whole path and the
    /// parent's optional param is left empty.
    #[test]
    pub fn optional_parent_falls_back_to_child() {
        let routes = RouteDefs::<_>::new(
            NestedRoute::new(OptionalParamSegment("lang"), || ())
                .child(NestedRoute::new(StaticSegment("about"), || ())),
        );
        let matched = routes.match_route("/about").expect("child only");
        assert!(matched.to_params().is_empty());
        let matched = routes.match_route("/fr/about").expect("both");
        assert_eq!(matched.to_params(), vec![("lang".into(), "fr".into())]);
    }

    /// A base that is a prefix of the path but ends inside a segment (`/app` of
    /// `/appé...`) leaves the routes a path without a leading `/`. A param route used to
    /// panic on it when the next character was multibyte.
    #[test]
    pub fn base_that_is_a_partial_prefix_is_no_match() {
        let routes = RouteDefs::<_>::new_with_base(
            (
                NestedRoute::new((ParamSegment("id"),), || ()),
                NestedRoute::new((WildcardSegment("rest"),), || ()),
            ),
            "/app",
        );
        assert!(routes.match_route("/appéa").is_none());
        assert!(routes.match_route("/applesauce").is_none());
        let matched = routes.match_route("/app/éa").expect("param");
        assert_eq!(matched.to_params(), vec![("id".into(), "éa".into())]);
        let matched = routes.match_route("/app/é/a").expect("wildcard");
        assert_eq!(matched.to_params(), vec![("rest".into(), "é/a".into())]);
    }

    /// Odd paths against a realistic route table: never a panic, and only full matches.
    #[test]
    pub fn odd_paths_match_or_fall_back() {
        let routes = RouteDefs::<_>::new_with_base(
            (
                NestedRoute::new(StaticSegment("/"), || ()),
                NestedRoute::new(
                    (StaticSegment("reports"), ParamSegment("id")),
                    || (),
                ),
                NestedRoute::new(
                    (StaticSegment("c"), OptionalParamSegment("tab")),
                    || (),
                ),
                NestedRoute::new(
                    (StaticSegment("files"), WildcardSegment("path")),
                    || (),
                ),
            ),
            "",
        );
        let paths = [
            "",
            "/",
            "//",
            "///",
            "/reports",
            "/reports/",
            "/reports//",
            "/reports/1",
            "/reports/1/",
            "/reports/1/2",
            "reports/1",
            "/c",
            "/c/",
            "/c/x",
            "/c/x/y",
            "/files",
            "/files/",
            "/files/a//b/",
            "/é",
            "é",
            "éa",
            "/reports/é",
            "/🦀/🦀",
            "/reports/\u{301}",
            "?q=1",
            "#x",
            "/%E9",
            "/reports/%2F",
        ];
        for path in paths {
            // must not panic; a match must consume the whole path
            _ = routes.match_route(path);
        }
        let id = |path: &str| routes.match_route(path).map(|m| m.to_params());
        assert_eq!(id("/reports/é"), Some(vec![("id".into(), "é".into())]));
        assert_eq!(id("/reports/1/"), Some(vec![("id".into(), "1".into())]));
        assert_eq!(id("/reports/1/2"), None);
        assert_eq!(id("/reports"), None);
        assert_eq!(id("/c"), Some(vec![]));
        assert_eq!(
            id("/files/a//b/"),
            Some(vec![("path".into(), "a//b/".into())])
        );
    }
}

/// Successful result of [testing](PossibleRouteMatch::test) a single segment in the route path
#[derive(Debug)]
pub struct PartialPathMatch<'a> {
    /// unmatched yet part of the path
    pub(crate) remaining: &'a str,
    /// value of parameters encoded inside of the path
    pub(crate) params: Vec<(Cow<'static, str>, String)>,
    /// part of the original path that was matched by segment
    pub(crate) matched: &'a str,
}

impl<'a> PartialPathMatch<'a> {
    pub fn new(
        remaining: &'a str,
        params: Vec<(Cow<'static, str>, String)>,
        matched: &'a str,
    ) -> Self {
        Self {
            remaining,
            params,
            matched,
        }
    }

    pub fn is_complete(&self) -> bool {
        self.remaining.is_empty() || self.remaining == "/"
    }

    pub fn remaining(&self) -> &'a str {
        self.remaining
    }

    pub fn params(self) -> Vec<(Cow<'static, str>, String)> {
        self.params
    }

    pub fn matched(&self) -> &'a str {
        self.matched
    }
}
