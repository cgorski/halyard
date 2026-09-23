use super::{PartialPathMatch, PathSegment, PossibleRouteMatch};
use std::fmt::Debug;

impl PossibleRouteMatch for () {
    fn optional(&self) -> bool {
        false
    }

    fn test<'a>(&self, path: &'a str) -> Option<PartialPathMatch<'a>> {
        Some(PartialPathMatch::new(path, vec![], ""))
    }

    fn generate_path(&self, _path: &mut Vec<PathSegment>) {}
}

pub trait AsPath {
    fn as_path(&self) -> &'static str;
}

impl AsPath for &'static str {
    fn as_path(&self) -> &'static str {
        self
    }
}

/// A segment that is expected to be static. Not requiring mapping into params.
///
/// Should work exactly as you would expect.
///
/// # Examples
/// ```rust
/// # (|| -> Option<()> { // Option does not impl Terminate, so no main
/// use halyard::prelude::*;
/// use halyard_router::{path, PossibleRouteMatch, StaticSegment};
///
/// let path = &"/users";
///
/// // Manual definition
/// let manual = (StaticSegment("users"),);
/// let matched = manual.test(path)?;
/// assert_eq!(matched.matched(), "/users");
///
/// // Params are empty as we had no `ParamSegement`s or `WildcardSegment`s
/// // If you did have additional dynamic segments, this would not be empty.
/// assert_eq!(matched.params().len(), 0);
///
/// // Macro definition
/// let using_macro = path!("/users");
/// let matched = manual.test(path)?;
/// assert_eq!(matched.matched(), "/users");
///
/// assert_eq!(matched.params().len(), 0);
///
/// # Some(())
/// # })().unwrap();
/// ```
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct StaticSegment<T: AsPath>(pub T);

impl<T: AsPath> PossibleRouteMatch for StaticSegment<T> {
    fn optional(&self) -> bool {
        false
    }

    fn test<'a>(&self, path: &'a str) -> Option<PartialPathMatch<'a>> {
        let segment = self.0.as_path();

        // `""` and `"/"` are pass-through parents (e.g. nested wrapper routes): they
        // consume nothing and leave the rest of the path to their children. A non-empty
        // path must start with `/`, otherwise we are not certain about being at the
        // beginning of a segment in the path.
        if segment.is_empty() {
            return (path.is_empty() || path.starts_with('/'))
                .then(|| PartialPathMatch::new(path, vec![], ""));
        }
        if segment == "/" {
            // the `/` is reported as matched but not eaten, so that the next segment
            // can still tell that it is matching from the beginning of a segment
            return path
                .starts_with('/')
                .then(|| PartialPathMatch::new(path, vec![], "/"));
        }

        let rest = path.strip_prefix('/')?;
        let name = segment.strip_prefix('/').unwrap_or(segment);
        // a `/` inside the name can never match: the path's `/` ends the segment
        if name.contains('/') {
            return None;
        }
        let remaining = rest.strip_prefix(name)?;
        // the whole segment must match: `/foobar` does not match `fo`
        if !(remaining.is_empty() || remaining.starts_with('/')) {
            return None;
        }
        // `remaining` is the end of `path`, so this removes exactly its length
        let matched = path.strip_suffix(remaining)?;
        Some(PartialPathMatch::new(remaining, vec![], matched))
    }

    fn generate_path(&self, path: &mut Vec<PathSegment>) {
        path.push(PathSegment::Static(self.0.as_path().into()))
    }
}

#[cfg(test)]
mod tests {
    use super::{PossibleRouteMatch, StaticSegment};
    use crate::AsPath;

    #[derive(Debug, Clone)]
    enum Paths {
        Foo,
        Bar,
    }

    impl AsPath for Paths {
        fn as_path(&self) -> &'static str {
            match self {
                Foo => "foo",
                Bar => "bar",
            }
        }
    }

    use Paths::*;

    #[test]
    fn single_static_match() {
        let path = "/foo";
        let def = StaticSegment("foo");
        let matched = def.test(path).expect("couldn't match route");
        assert_eq!(matched.matched(), "/foo");
        assert_eq!(matched.remaining(), "");
        let params = matched.params();
        assert!(params.is_empty());
    }

    #[test]
    fn single_static_match_on_enum() {
        let path = "/foo";
        let def = StaticSegment(Foo);
        let matched = def.test(path).expect("couldn't match route");
        assert_eq!(matched.matched(), "/foo");
        assert_eq!(matched.remaining(), "");
        let params = matched.params();
        assert!(params.is_empty());
    }

    #[test]
    fn single_static_mismatch() {
        let path = "/foo";
        let def = StaticSegment("bar");
        assert!(def.test(path).is_none());
    }

    #[test]
    fn single_static_mismatch_on_enum() {
        let path = "/foo";
        let def = StaticSegment(Bar);
        assert!(def.test(path).is_none());
    }

    #[test]
    fn single_static_match_with_trailing_slash() {
        let path = "/foo/";
        let def = StaticSegment("foo");
        let matched = def.test(path).expect("couldn't match route");
        assert_eq!(matched.matched(), "/foo");
        assert_eq!(matched.remaining(), "/");
        let params = matched.params();
        assert!(params.is_empty());
    }

    #[test]
    fn single_static_match_with_trailing_slash_on_enum() {
        let path = "/foo/";
        let def = StaticSegment(Foo);
        let matched = def.test(path).expect("couldn't match route");
        assert_eq!(matched.matched(), "/foo");
        assert_eq!(matched.remaining(), "/");
        let params = matched.params();
        assert!(params.is_empty());
    }

    #[test]
    fn tuple_of_static_matches() {
        let path = "/foo/bar";
        let def = (StaticSegment("foo"), StaticSegment("bar"));
        let matched = def.test(path).expect("couldn't match route");
        assert_eq!(matched.matched(), "/foo/bar");
        assert_eq!(matched.remaining(), "");
        let params = matched.params();
        assert!(params.is_empty());
    }

    #[test]
    fn tuple_of_static_matches_on_enum() {
        let path = "/foo/bar";
        let def = (StaticSegment(Foo), StaticSegment(Bar));
        let matched = def.test(path).expect("couldn't match route");
        assert_eq!(matched.matched(), "/foo/bar");
        assert_eq!(matched.remaining(), "");
        let params = matched.params();
        assert!(params.is_empty());
    }

    #[test]
    fn allow_empty_match() {
        let path = "";
        let def = StaticSegment("");
        let matched = def.test(path).expect("couldn't match route");
        assert_eq!(matched.matched(), "");
        assert_eq!(matched.remaining(), "");
        let params = matched.params();
        assert!(params.is_empty());
    }

    #[test]
    fn tuple_static_mismatch() {
        let path = "/foo/baz";
        let def = (StaticSegment("foo"), StaticSegment("bar"));
        assert!(def.test(path).is_none());
    }

    #[test]
    fn tuple_static_mismatch_on_enum() {
        let path = "/foo/baz";
        let def = (StaticSegment(Foo), StaticSegment(Bar));
        assert!(def.test(path).is_none());
    }

    #[test]
    fn dont_match_smooshed_segments() {
        let path = "/foobar";
        let def = (StaticSegment(Foo), StaticSegment(Bar));
        assert!(def.test(path).is_none());
    }

    #[test]
    fn arbitrary_nesting_of_tuples_has_no_effect_on_matching() {
        let path = "/foo/bar";
        let def = (
            (),
            (StaticSegment("foo")),
            (),
            ((), ()),
            StaticSegment("bar"),
            (),
        );
        let matched = def.test(path).expect("couldn't match route");
        assert_eq!(matched.matched(), "/foo/bar");
        assert_eq!(matched.remaining(), "");
        let params = matched.params();
        assert!(params.is_empty());
    }

    #[test]
    fn arbitrary_nesting_of_tuples_has_no_effect_on_matching_on_enum() {
        let path = "/foo/bar";
        let def = (
            (),
            (StaticSegment(Foo)),
            (),
            ((), ()),
            StaticSegment(Bar),
            (),
        );
        let matched = def.test(path).expect("couldn't match route");
        assert_eq!(matched.matched(), "/foo/bar");
        assert_eq!(matched.remaining(), "");
        let params = matched.params();
        assert!(params.is_empty());
    }

    #[test]
    fn only_match_full_static_paths() {
        let def = (StaticSegment("tests"), StaticSegment("abc"));
        assert!(def.test("/tes/abc").is_none());
        assert!(def.test("/test/abc").is_none());
        assert!(def.test("/tes/abc/").is_none());
        assert!(def.test("/test/abc/").is_none());
        assert!(def.test("/tests/ab").is_none());
        assert!(def.test("/tests/ab/").is_none());
    }

    #[test]
    fn no_partial_match_on_overlong_path() {
        let def = StaticSegment("fo");
        assert!(def.test("/foobar").is_none());
    }

    #[test]
    fn static_segments_match_multibyte_names_whole() {
        let def = StaticSegment("café");
        let m = def.test("/café/menu").expect("should match");
        assert_eq!(m.matched(), "/café");
        assert_eq!(m.remaining(), "/menu");
        assert!(def.test("/cafés").is_none());
        assert!(def.test("/caf").is_none());
        assert!(def.test("/cafe").is_none());
        assert!(def.test("café").is_none());
        assert!(StaticSegment("é").test("/éa").is_none());
    }

    /// What `StaticSegment` matches, for a table of segments and paths (the behaviour the
    /// loop without arithmetic must keep).
    #[test]
    fn static_segment_table() {
        // (segment, path, Some((matched, remaining)) or None)
        let cases: &[(&'static str, &str, Option<(&str, &str)>)] = &[
            ("", "", Some(("", ""))),
            ("", "/", Some(("", "/"))),
            ("", "/a/b", Some(("", "/a/b"))),
            ("", "a", None),
            ("/", "", None),
            ("/", "/", Some(("/", "/"))),
            ("/", "/a", Some(("/", "/a"))),
            ("/", "a", None),
            ("a", "", None),
            ("a", "/", None),
            ("a", "/a", Some(("/a", ""))),
            ("a", "/a/", Some(("/a", "/"))),
            ("a", "/a//", Some(("/a", "//"))),
            ("a", "/ab", None),
            ("a", "a", None),
            ("a", "//a", None),
            ("/a", "/a/b", Some(("/a", "/b"))),
            ("ab", "/a", None),
            ("a/b", "/a/b", None),
            ("a/", "/a/", None),
        ];
        for &(segment, path, expected) in cases {
            let got = StaticSegment(segment)
                .test(path)
                .map(|m| (m.matched(), m.remaining()));
            assert_eq!(got, expected, "StaticSegment({segment:?}) on {path:?}");
        }
    }

    #[test]
    fn empty_segment_is_passthrough_parent() {
        let def = StaticSegment("");
        let m = def
            .test("/lang")
            .expect("empty segment should pass through");
        assert_eq!(m.matched(), "");
        assert_eq!(m.remaining(), "/lang");

        let def = StaticSegment("/");
        let m = def.test("/lang").expect("root segment should pass through");
        assert_eq!(m.matched(), "/");
        assert_eq!(m.remaining(), "/lang");
    }
}
