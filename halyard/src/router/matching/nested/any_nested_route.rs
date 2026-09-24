#![allow(clippy::type_complexity)]
use crate::router::{
    error::{report_once, RouterError},
    matching::nested::any_nested_match::{AnyNestedMatch, IntoAnyNestedMatch},
    GeneratedRouteData, MatchNestedRoutes, RouteMatchId,
};
use halyard_tachys::{erased::Erased, prelude::IntoMaybeErased};
use std::{fmt::Debug, sync::atomic::AtomicBool};

/// A type-erased container for any [`MatchNestedRoutes`].
pub struct AnyNestedRoute {
    value: Erased,
    clone: fn(&Erased) -> AnyNestedRoute,
    match_nested:
        for<'a> fn(
            &'a Erased,
            &'a str,
        )
            -> (Option<(RouteMatchId, AnyNestedMatch)>, &'a str),
    generate_routes: fn(&Erased) -> Vec<GeneratedRouteData>,
    optional: fn(&Erased) -> bool,
}

impl Clone for AnyNestedRoute {
    fn clone(&self) -> Self {
        (self.clone)(&self.value)
    }
}

impl Debug for AnyNestedRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnyNestedRoute").finish_non_exhaustive()
    }
}

impl IntoMaybeErased for AnyNestedRoute {
    type Output = Self;

    fn into_maybe_erased(self) -> Self::Output {
        self
    }
}

/// An `AnyNestedRoute` keeps its type-erased value next to functions made for the value's
/// type, both by `into_any_nested_route`, so the value always has that type. If it had
/// not, it acts as [`NoRoute`] (logged once) instead of reading the value as the wrong type.
fn type_mismatch() {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    report_once(
        &REPORTED,
        &RouterError::ErasedTypeMismatch {
            what: "an AnyNestedRoute",
            instead: "it matches no path and generates no routes",
        },
    );
}

/// A route that matches no path, generates no routes and is not optional.
#[derive(Clone, Copy)]
struct NoRoute;

impl MatchNestedRoutes for NoRoute {
    type Data = ();
    type Match = ();

    fn match_nested<'a>(
        &'a self,
        path: &'a str,
    ) -> (Option<(RouteMatchId, Self::Match)>, &'a str) {
        (None, path)
    }

    fn generate_routes(
        &self,
    ) -> impl IntoIterator<Item = GeneratedRouteData> + '_ {
        std::iter::empty()
    }

    fn optional(&self) -> bool {
        false
    }
}

/// Converts anything implementing [`MatchNestedRoutes`] into [`AnyNestedRoute`].
pub trait IntoAnyNestedRoute {
    /// Wraps the nested route.
    fn into_any_nested_route(self) -> AnyNestedRoute;
}

impl<T> IntoAnyNestedRoute for T
where
    T: MatchNestedRoutes + Send + Clone + 'static,
{
    fn into_any_nested_route(self) -> AnyNestedRoute {
        fn clone<T: MatchNestedRoutes + Send + Clone + 'static>(
            value: &Erased,
        ) -> AnyNestedRoute {
            match value.get_ref::<T>() {
                Some(value) => value.clone().into_any_nested_route(),
                None => {
                    type_mismatch();
                    NoRoute.into_any_nested_route()
                }
            }
        }

        fn match_nested<'a, T: MatchNestedRoutes + Send + Clone + 'static>(
            value: &'a Erased,
            path: &'a str,
        ) -> (Option<(RouteMatchId, AnyNestedMatch)>, &'a str) {
            let Some(value) = value.get_ref::<T>() else {
                type_mismatch();
                return (None, path);
            };
            let (maybe_match, path) = value.match_nested(path);
            (
                maybe_match
                    .map(|(id, matched)| (id, matched.into_any_nested_match())),
                path,
            )
        }

        fn generate_routes<T: MatchNestedRoutes + Send + Clone + 'static>(
            value: &Erased,
        ) -> Vec<GeneratedRouteData> {
            match value.get_ref::<T>() {
                Some(value) => value.generate_routes().into_iter().collect(),
                None => {
                    type_mismatch();
                    Vec::new()
                }
            }
        }

        fn optional<T: MatchNestedRoutes + Send + Clone + 'static>(
            value: &Erased,
        ) -> bool {
            match value.get_ref::<T>() {
                Some(value) => value.optional(),
                None => {
                    type_mismatch();
                    false
                }
            }
        }

        AnyNestedRoute {
            value: Erased::new(self),
            clone: clone::<T>,
            match_nested: match_nested::<T>,
            generate_routes: generate_routes::<T>,
            optional: optional::<T>,
        }
    }
}

impl MatchNestedRoutes for AnyNestedRoute {
    type Data = AnyNestedMatch;
    type Match = AnyNestedMatch;

    fn match_nested<'a>(
        &'a self,
        path: &'a str,
    ) -> (Option<(RouteMatchId, Self::Match)>, &'a str) {
        (self.match_nested)(&self.value, path)
    }

    fn generate_routes(&self) -> impl IntoIterator<Item = GeneratedRouteData> {
        (self.generate_routes)(&self.value)
    }

    fn optional(&self) -> bool {
        (self.optional)(&self.value)
    }
}

#[cfg(test)]
mod tests {
    use super::{AnyNestedRoute, IntoAnyNestedRoute};
    use crate::router::MatchNestedRoutes;
    use halyard_tachys::erased::Erased;

    /// An `AnyNestedRoute` whose value has another type than its functions
    /// (`into_any_nested_route` makes the two together, so only code in this module can get
    /// here). Reading the value panicked ("Erased: type mismatch"), and with
    /// `--cfg erase_components` read a `u8` as the route. It matches no path and generates
    /// no routes.
    #[test]
    fn an_any_nested_route_holding_another_type_matches_nothing() {
        // `()` matches every path and generates one route
        let route = ().into_any_nested_route();
        assert!(route.match_nested("/a").0.is_some());
        assert_eq!(route.generate_routes().into_iter().count(), 1);

        let mismatched = || -> AnyNestedRoute {
            let mut route = ().into_any_nested_route();
            route.value = Erased::new(5u8);
            route
        };
        let route = mismatched();
        let (matched, rest) = route.match_nested("/a");
        assert!(matched.is_none());
        assert_eq!(rest, "/a");
        assert_eq!(mismatched().generate_routes().into_iter().count(), 0);
        assert!(!mismatched().optional());

        let clone = mismatched().clone();
        assert!(clone.match_nested("/a").0.is_none());
        assert_eq!(clone.generate_routes().into_iter().count(), 0);
    }
}
