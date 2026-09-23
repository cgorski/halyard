//! What `axum::Router::route` accepts, checked before calling it.
//!
//! `Router::route` panics on a path it cannot route: axum 0.8's own checks (`validate_path`),
//! then matchit 0.8's route syntax and its conflicts between routes, and on a second
//! handler for a method it already routes at a path. [`RouteRegistry::admit`] gives the same
//! answers as a `Result`, so that `HalyardRoutes` logs and skips a bad route at startup
//! instead of panicking. The tests check these answers against axum itself.
//!
//! Only routes added through one `RouteRegistry` are known to it: a route the application
//! put on the router before calling `halyard_routes*` can still conflict inside axum.

use crate::error::RouteError;
use axum::{http::Method, routing::MethodFilter};

/// Why axum cannot route a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum PathError {
    #[error("it does not start with `/` (the root is \"/\")")]
    NoLeadingSlash,
    #[error("a segment starts with `:` (a parameter is written `{{name}}`)")]
    ColonSegment,
    #[error("a segment starts with `*` (a catch-all is written `{{*name}}`)")]
    StarSegment,
    #[error(
        "a `{{` or `}}` does not form a `{{name}}` parameter (a literal brace is written \
         twice)"
    )]
    InvalidParam,
    #[error("a parameter is followed by more text in its segment")]
    InvalidParamSegment,
    #[error("a catch-all is not at the end of the path")]
    CatchAllNotLast,
    #[error("it has more than 25 parameters")]
    TooManyParams,
}

/// The most parameters matchit 0.8 can hold in one route: it renames them `a` to `y` and
/// panics on the 26th.
const MAX_PARAMS: usize = 25;

/// One element of a route path as matchit reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    /// A literal byte (a `{` or `}` here was written doubled).
    Literal(u8),
    /// A `{name}` parameter. matchit renames parameters by position, so two parameters in
    /// the same place are the same whatever their names.
    Param,
    /// A `{*name}` catch-all. matchit keeps these names, so they must agree.
    CatchAll(Vec<u8>),
}

impl Token {
    fn is_wildcard(&self) -> bool {
        !matches!(self, Token::Literal(_))
    }
}

/// A path that `Router::route` accepts, as matchit reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RoutePath(Vec<Token>);

impl RoutePath {
    /// Reads `path` as axum 0.8.8 and matchit 0.8.4 do, and fails where they do.
    pub(crate) fn parse(path: &str) -> Result<Self, PathError> {
        // axum's `validate_path`
        if !path.starts_with('/') {
            return Err(PathError::NoLeadingSlash);
        }
        // axum's checks for the 0.7 syntax, on unless the router calls `without_v07_checks`
        for segment in path.split('/') {
            if segment.starts_with(':') {
                return Err(PathError::ColonSegment);
            }
            if segment.starts_with('*') {
                return Err(PathError::StarSegment);
            }
        }

        let bytes = unescape(path.as_bytes());
        let mut tokens = Vec::with_capacity(bytes.len());
        let mut params = 0_usize;
        let mut rest = bytes.as_slice();
        while let Some((&(byte, escaped), after)) = rest.split_first() {
            if escaped || !matches!(byte, b'{' | b'}') {
                tokens.push(Token::Literal(byte));
                rest = after;
                continue;
            }
            if byte == b'}' {
                return Err(PathError::InvalidParam);
            }
            let (token, after) = wildcard(after)?;
            match token {
                Token::CatchAll(_) if !after.is_empty() => {
                    return Err(PathError::CatchAllNotLast);
                }
                Token::Param => {
                    params = params.saturating_add(1);
                    if params > MAX_PARAMS {
                        return Err(PathError::TooManyParams);
                    }
                }
                _ => {}
            }
            tokens.push(token);
            rest = after;
        }
        Ok(RoutePath(tokens))
    }

    /// Whether matchit refuses to hold both paths in one router. Past their common
    /// beginning, matchit allows a literal next to a wildcard, but two different
    /// wildcards in one place, or two paths that differ only in parameter names, conflict.
    fn conflicts_with(&self, other: &RoutePath) -> bool {
        let mut ours = self.0.iter();
        let mut theirs = other.0.iter();
        loop {
            match (ours.next(), theirs.next()) {
                (None, None) => return true,
                (Some(a), Some(b)) if a == b => {}
                (Some(a), Some(b)) => {
                    return a.is_wildcard() && b.is_wildcard()
                }
                // one path is the beginning of the other
                _ => return false,
            }
        }
    }
}

/// matchit's unescaping: a doubled `{` or `}` is one literal brace (`true`: escaped).
fn unescape(path: &[u8]) -> Vec<(u8, bool)> {
    let mut out = Vec::with_capacity(path.len());
    let mut bytes = path.iter().copied().peekable();
    while let Some(byte) = bytes.next() {
        let doubled =
            matches!(byte, b'{' | b'}') && bytes.peek() == Some(&byte);
        if doubled {
            bytes.next();
        }
        out.push((byte, doubled));
    }
    out
}

/// Reads the wildcard after an opening `{`, as matchit's `find_wildcard` does: returns it
/// and what follows its closing `}`.
fn wildcard(
    after_brace: &[(u8, bool)],
) -> Result<(Token, &[(u8, bool)]), PathError> {
    // the byte after `{` is part of the name whatever it is, unless it closes the
    // wildcard at once (`{}`)
    match after_brace.first() {
        None | Some((b'}', _)) => return Err(PathError::InvalidParam),
        Some(_) => {}
    }
    let close = after_brace
        .iter()
        .enumerate()
        .skip(1)
        .find_map(|(i, &(byte, escaped))| match byte {
            b'}' if !escaped => Some(Ok(i)),
            b'*' | b'/' => Some(Err(PathError::InvalidParam)),
            _ => None,
        })
        .unwrap_or(Err(PathError::InvalidParam))?;
    let (name, closing_and_rest) = after_brace
        .split_at_checked(close)
        .ok_or(PathError::InvalidParam)?;
    let rest = closing_and_rest.get(1..).unwrap_or_default();
    // `{*}`: a catch-all needs a name
    if name.last().map(|&(byte, _)| byte) == Some(b'*') {
        return Err(PathError::InvalidParam);
    }
    if let Some(&(next, _)) = rest.first() {
        if next != b'/' {
            return Err(PathError::InvalidParamSegment);
        }
    }
    let token = match name.split_first() {
        Some(((b'*', _), catch_all)) => {
            Token::CatchAll(catch_all.iter().map(|&(byte, _)| byte).collect())
        }
        _ => Token::Param,
    };
    Ok((token, rest))
}

/// The routes added to one router so far, to refuse what `Router::route` would panic on.
#[derive(Debug, Default)]
pub(crate) struct RouteRegistry {
    routes: Vec<RegisteredPath>,
}

#[derive(Debug)]
struct RegisteredPath {
    path: String,
    parsed: RoutePath,
    methods: Vec<Method>,
}

impl RouteRegistry {
    /// Checks that axum can route `method` at `path` next to the routes admitted so far and,
    /// if it can, records it and returns the method filter to route it with.
    pub(crate) fn admit(
        &mut self,
        path: &str,
        method: &Method,
    ) -> Result<MethodFilter, RouteError> {
        let filter = MethodFilter::try_from(method.clone()).map_err(|_| {
            RouteError::UnsupportedMethod {
                path: path.to_owned(),
                method: method.clone(),
            }
        })?;
        let parsed = RoutePath::parse(path).map_err(|reason| {
            RouteError::InvalidPath {
                path: path.to_owned(),
                reason,
            }
        })?;

        // axum merges the method routers of one path, but not two handlers for one method
        if let Some(existing) = self.routes.iter_mut().find(|r| r.path == path)
        {
            if existing.methods.contains(method) {
                return Err(RouteError::Duplicate {
                    path: path.to_owned(),
                    method: method.clone(),
                });
            }
            existing.methods.push(method.clone());
            return Ok(filter);
        }
        if let Some(existing) = self
            .routes
            .iter()
            .find(|r| r.parsed.conflicts_with(&parsed))
        {
            return Err(RouteError::Conflict {
                path: path.to_owned(),
                existing: existing.path.clone(),
            });
        }
        self.routes.push(RegisteredPath {
            path: path.to_owned(),
            parsed,
            methods: vec![method.clone()],
        });
        Ok(filter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::on;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    /// Whether axum 0.8 routes `path`, or panics on it.
    fn axum_accepts(path: &str) -> bool {
        catch_unwind(|| {
            axum::Router::<()>::new()
                .route(path, axum::routing::get(|| async {}))
        })
        .is_ok()
    }

    /// Paths on both sides of every rule in axum's and matchit's path checks.
    const PATHS: &[&str] = &[
        "/",
        "",
        "a",
        "a/b",
        "/a",
        "/a/",
        "/a/b/c",
        "/{id}",
        "/a/{id}",
        "/a/{id}/b",
        "/a/{*rest}",
        "/{*rest}",
        "/a{id}",
        "/a{b",
        "/a}b",
        "/a{{b",
        "/a}}b",
        "/a{{b}}",
        "/{{id}}",
        "/{{{id}",
        "/{}",
        "/{*}",
        "/{**}",
        "/{a*b}",
        "/{a/b}",
        "/{/a}",
        "/{a{b}",
        "/{a}}",
        "/{a}}b}",
        "/{a}b",
        "/{a}{b}",
        "/{a}/{b}",
        "/{*rest}/x",
        "/{*rest}x",
        "/:id",
        "/a/:id",
        "/*rest",
        "/a/*",
        "/a:b",
        "/a*b",
        "/{:id}",
        "/café/{id}",
        "/a//b",
    ];

    #[test]
    fn route_path_accepts_what_axum_accepts() {
        // anchors, so that the comparison below is not vacuous
        assert!(axum_accepts("/a{{b") && axum_accepts("/a/{id}"));
        assert!(!axum_accepts("/a{b") && !axum_accepts(""));
        for path in PATHS {
            assert_eq!(
                RoutePath::parse(path).is_ok(),
                axum_accepts(path),
                "path {path:?}: {:?}",
                RoutePath::parse(path)
            );
        }
    }

    #[test]
    fn route_path_allows_at_most_25_parameters() {
        let path =
            |n: usize| (0..n).map(|i| format!("/{{p{i}}}")).collect::<String>();
        assert!(RoutePath::parse(&path(25)).is_ok());
        assert!(axum_accepts(&path(25)));
        assert_eq!(RoutePath::parse(&path(26)), Err(PathError::TooManyParams));
        assert!(!axum_accepts(&path(26)));
    }

    /// Routes each `(path, method)` in turn on one axum router, and on a registry, and
    /// returns for each whether axum panicked (the route left out) and whether the registry
    /// refused it.
    fn axum_and_registry(routes: &[(&str, Method)]) -> (Vec<bool>, Vec<bool>) {
        let mut router = axum::Router::<()>::new();
        let mut registry = RouteRegistry::default();
        let mut axum_refused = Vec::new();
        let mut registry_refused = Vec::new();
        for (path, method) in routes {
            let filter = MethodFilter::try_from(method.clone())
                .expect("the tests use methods that axum can filter");
            let attempt = catch_unwind(AssertUnwindSafe(|| {
                router.clone().route(path, on(filter, || async {}))
            }));
            axum_refused.push(attempt.is_err());
            if let Ok(next) = attempt {
                router = next;
            }
            registry_refused.push(registry.admit(path, method).is_err());
        }
        (axum_refused, registry_refused)
    }

    #[test]
    fn registry_refuses_what_axum_refuses() {
        let get = || Method::GET;
        let cases: &[&[(&str, Method)]] = &[
            &[("/a", get()), ("/a", get())],
            &[("/a", get()), ("/a", Method::POST), ("/a", Method::HEAD)],
            &[("/a", Method::POST), ("/a", get()), ("/a", Method::POST)],
            &[("/u/{id}", get()), ("/u/{name}", get())],
            &[("/u/{id}", get()), ("/u/{name}", Method::POST)],
            &[("/u/{id}", get()), ("/u/{id}", Method::POST)],
            &[("/u/{id}/a", get()), ("/u/{name}/b", get())],
            &[("/u/{id}/a", get()), ("/u/{name}/a", get())],
            &[("/f/{*a}", get()), ("/f/{*b}", get())],
            &[("/f/{a}", get()), ("/f/{*b}", get())],
            &[("/f/{*b}", get()), ("/f/{a}", get())],
            &[("/f/{*b}", get()), ("/f/{a}/c", get())],
            &[("/f/{a}", get()), ("/f/x", get())],
            &[("/f/x", get()), ("/f/{a}", get())],
            &[("/f/{*a}", get()), ("/f/x", get()), ("/f/x/y", get())],
            &[("/f/{*a}", get()), ("/f/", get()), ("/f", get())],
            &[("/", get()), ("/{*rest}", get()), ("/{id}", get())],
            &[("/a{{b", get()), ("/a{b}", get()), ("/a{c}", get())],
            &[("/x{a}", get()), ("/x/{a}", get()), ("/x", get())],
            &[("/x/{a}/{b}", get()), ("/x/{c}", get()), ("/x/{c}/", get())],
            &[("/p/{a}", get()), ("/p/{b}/q", get()), ("/p/{c}/r", get())],
        ];
        for routes in cases {
            let (axum_refused, registry_refused) = axum_and_registry(routes);
            assert_eq!(registry_refused, axum_refused, "routes {routes:?}");
        }
        // anchors, so that the comparison above is not vacuous
        let (refused, _) =
            axum_and_registry(&[("/u/{id}", get()), ("/u/{name}", get())]);
        assert_eq!(refused, [false, true]);
        let (refused, _) =
            axum_and_registry(&[("/f/{a}", get()), ("/f/x", get())]);
        assert_eq!(refused, [false, false]);
    }

    #[test]
    fn registry_refuses_methods_axum_cannot_filter() {
        let mut registry = RouteRegistry::default();
        let purge = Method::from_bytes(b"PURGE").expect("a valid method");

        assert_eq!(
            registry.admit("/api/f", &purge),
            Err(RouteError::UnsupportedMethod {
                path: "/api/f".to_owned(),
                method: purge,
            })
        );
    }

    /// Server functions used to be routed only for GET, POST, PUT, DELETE and PATCH, and
    /// any other method panicked. Every method axum can filter is routed now.
    #[test]
    fn registry_routes_every_method_axum_can_filter() {
        let mut registry = RouteRegistry::default();
        for method in [
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::PATCH,
            Method::HEAD,
            Method::OPTIONS,
            Method::TRACE,
            Method::CONNECT,
        ] {
            assert!(
                registry.admit("/api/f", &method).is_ok(),
                "{method} should be routed"
            );
        }
    }
}
