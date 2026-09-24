use super::{PartialPathMatch, PathSegment, PossibleRouteMatch};
use std::borrow::Cow;

/// Splits off the segment that `path` starts with: `(matched, value, remaining)`, where
/// `matched` is the leading `/` and `value`, `value` runs up to (not including) the next
/// `/`, and `remaining` is the rest of `path`.
///
/// A path that does not start with `/` is not at the start of a segment, as for
/// [`StaticSegment`](super::StaticSegment): `None`.
fn first_segment(path: &str) -> Option<(&str, &str, &str)> {
    let rest = path.strip_prefix('/')?;
    let (value, remaining) = match rest.find('/') {
        Some(end) => rest.split_at_checked(end)?,
        None => (rest, ""),
    };
    // `remaining` is the end of `path`, so this removes exactly its length
    let matched = path.strip_suffix(remaining)?;
    Some((matched, value, remaining))
}

/// A segment that captures a value from the url and maps it to a key.
///
/// # Examples
/// ```rust
/// # (|| -> Option<()> { // Option does not impl Terminate, so no main
/// use halyard::prelude::*;
/// use halyard::router::{path, ParamSegment, PossibleRouteMatch};
///
/// let path = &"/hello";
///
/// // Manual definition
/// let manual = (ParamSegment("message"),);
/// let params = manual.test(path)?.params();
/// let (key, value) = params.last()?;
///
/// assert_eq!(key, "message");
/// assert_eq!(value, "hello");
///
/// // Macro definition
/// let using_macro = path!("/:message");
/// let params = using_macro.test(path)?.params();
/// let (key, value) = params.last()?;
///
/// assert_eq!(key, "message");
/// assert_eq!(value, "hello");
///
/// # Some(())
/// # })().unwrap();
/// ```
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct ParamSegment(pub &'static str);

impl PossibleRouteMatch for ParamSegment {
    fn optional(&self) -> bool {
        false
    }

    fn test<'a>(&self, path: &'a str) -> Option<PartialPathMatch<'a>> {
        let (matched, value, remaining) = first_segment(path)?;
        // a param needs a value: `/` or `//x` do not match
        if value.is_empty() {
            return None;
        }
        let param_value = vec![(Cow::Borrowed(self.0), value.to_string())];
        Some(PartialPathMatch::new(remaining, param_value, matched))
    }

    fn generate_path(&self, path: &mut Vec<PathSegment>) {
        path.push(PathSegment::Param(self.0.into()));
    }
}

/// A segment that captures all remaining values from the url and maps it to a key.
///
/// A [`WildcardSegment`] __must__ be the last segment of your path definition.
///
/// ```rust
/// # (|| -> Option<()> { // Option does not impl Terminate, so no main
/// use halyard::prelude::*;
/// use halyard::router::{
///     path, ParamSegment, PossibleRouteMatch, StaticSegment, WildcardSegment,
/// };
///
/// let path = &"/echo/send/sync/and/static";
///
/// // Manual definition
/// let manual = (StaticSegment("echo"), WildcardSegment("kitchen_sink"));
/// let params = manual.test(path)?.params();
/// let (key, value) = params.last()?;
///
/// assert_eq!(key, "kitchen_sink");
/// assert_eq!(value, "send/sync/and/static");
///
/// // Macro definition
/// let using_macro = path!("/echo/*else");
/// let params = using_macro.test(path)?.params();
/// let (key, value) = params.last()?;
///
/// assert_eq!(key, "else");
/// assert_eq!(value, "send/sync/and/static");
///
/// // This fails to compile because the macro will catch the bad ordering
/// // let bad = path!("/echo/*foo/bar/:baz");
///
/// // This compiles but may not work as you expect at runtime.
/// (
///     StaticSegment("echo"),
///     WildcardSegment("foo"),
///     ParamSegment("baz"),
/// );
///
/// # Some(())
/// # })().unwrap();
/// ```
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct WildcardSegment(pub &'static str);

impl PossibleRouteMatch for WildcardSegment {
    fn optional(&self) -> bool {
        false
    }

    fn test<'a>(&self, path: &'a str) -> Option<PartialPathMatch<'a>> {
        // the rest of the path, which may be empty; a non-empty rest must start a segment
        let value = if path.is_empty() {
            path
        } else {
            path.strip_prefix('/')?
        };
        let param_value = vec![(Cow::Borrowed(self.0), value.to_string())];
        Some(PartialPathMatch::new("", param_value, path))
    }

    fn generate_path(&self, path: &mut Vec<PathSegment>) {
        path.push(PathSegment::Splat(self.0.into()));
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct OptionalParamSegment(pub &'static str);

impl PossibleRouteMatch for OptionalParamSegment {
    fn optional(&self) -> bool {
        true
    }

    fn test<'a>(&self, path: &'a str) -> Option<PartialPathMatch<'a>> {
        match first_segment(path) {
            Some((matched, value, remaining)) if !value.is_empty() => {
                let param_value =
                    vec![(Cow::Borrowed(self.0), value.to_string())];
                Some(PartialPathMatch::new(remaining, param_value, matched))
            }
            // no value here: match nothing and leave the path to the next segment
            _ => Some(PartialPathMatch::new(path, Vec::new(), "")),
        }
    }

    fn generate_path(&self, path: &mut Vec<PathSegment>) {
        path.push(PathSegment::OptionalParam(self.0.into()));
    }
}

#[cfg(test)]
mod tests {
    use super::PossibleRouteMatch;
    use crate::router::{
        OptionalParamSegment, ParamSegment, StaticSegment, WildcardSegment,
    };

    #[test]
    fn single_param_match() {
        let path = "/foo";
        let def = ParamSegment("a");
        let matched = def.test(path).expect("couldn't match route");
        assert_eq!(matched.matched(), "/foo");
        assert_eq!(matched.remaining(), "");
        let params = matched.params();
        assert_eq!(params[0], ("a".into(), "foo".into()));
    }

    #[test]
    fn single_param_match_with_trailing_slash() {
        let path = "/foo/";
        let def = ParamSegment("a");
        let matched = def.test(path).expect("couldn't match route");
        assert_eq!(matched.matched(), "/foo");
        assert_eq!(matched.remaining(), "/");
        let params = matched.params();
        assert_eq!(params[0], ("a".into(), "foo".into()));
    }

    #[test]
    fn tuple_of_param_matches() {
        let path = "/foo/bar";
        let def = (ParamSegment("a"), ParamSegment("b"));
        let matched = def.test(path).expect("couldn't match route");
        assert_eq!(matched.matched(), "/foo/bar");
        assert_eq!(matched.remaining(), "");
        let params = matched.params();
        assert_eq!(params[0], ("a".into(), "foo".into()));
        assert_eq!(params[1], ("b".into(), "bar".into()));
    }

    #[test]
    fn splat_should_match_all() {
        let path = "/foo/bar/////";
        let def = (
            StaticSegment("foo"),
            StaticSegment("bar"),
            WildcardSegment("rest"),
        );
        let matched = def.test(path).expect("couldn't match route");
        assert_eq!(matched.matched(), "/foo/bar/////");
        assert_eq!(matched.remaining(), "");
        let params = matched.params();
        assert_eq!(params[0], ("rest".into(), "////".into()));
    }

    #[test]
    fn optional_param_can_match() {
        let path = "/foo";
        let def = OptionalParamSegment("a");
        let matched = def.test(path).expect("couldn't match route");
        assert_eq!(matched.matched(), "/foo");
        assert_eq!(matched.remaining(), "");
        let params = matched.params();
        assert_eq!(params[0], ("a".into(), "foo".into()));
    }

    #[test]
    fn optional_param_can_not_match() {
        let path = "/";
        let def = OptionalParamSegment("a");
        let matched = def.test(path).expect("couldn't match route");
        assert_eq!(matched.matched(), "");
        assert_eq!(matched.remaining(), "/");
        let params = matched.params();
        assert_eq!(params.first(), None);
    }

    #[test]
    fn optional_params_match_first() {
        let path = "/foo";
        let def = (OptionalParamSegment("a"), OptionalParamSegment("b"));
        let matched = def.test(path).expect("couldn't match route");
        assert_eq!(matched.matched(), "/foo");
        assert_eq!(matched.remaining(), "");
        let params = matched.params();
        assert_eq!(params[0], ("a".into(), "foo".into()));
    }

    #[test]
    fn optional_params_can_match_both() {
        let path = "/foo/bar";
        let def = (OptionalParamSegment("a"), OptionalParamSegment("b"));
        let matched = def.test(path).expect("couldn't match route");
        assert_eq!(matched.matched(), "/foo/bar");
        assert_eq!(matched.remaining(), "");
        let params = matched.params();
        assert_eq!(params[0], ("a".into(), "foo".into()));
        assert_eq!(params[1], ("b".into(), "bar".into()));
    }

    #[test]
    fn matching_after_optional_param() {
        let path = "/bar";
        let def = (OptionalParamSegment("a"), StaticSegment("bar"));
        let matched = def.test(path).expect("couldn't match route");
        assert_eq!(matched.matched(), "/bar");
        assert_eq!(matched.remaining(), "");
        let params = matched.params();
        assert!(params.is_empty());
    }

    #[test]
    fn static_before_param() {
        let path = "/foo/bar";
        let def = (StaticSegment("foo"), ParamSegment("b"));
        let matched = def.test(path).expect("couldn't match route");
        assert_eq!(matched.matched(), "/foo/bar");
        assert_eq!(matched.remaining(), "");
        let params = matched.params();
        assert_eq!(params[0], ("b".into(), "bar".into()));
    }

    #[test]
    fn static_before_optional_param() {
        let path = "/foo/bar";
        let def = (StaticSegment("foo"), OptionalParamSegment("b"));
        let matched = def.test(path).expect("couldn't match route");
        assert_eq!(matched.matched(), "/foo/bar");
        assert_eq!(matched.remaining(), "");
        let params = matched.params();
        assert_eq!(params[0], ("b".into(), "bar".into()));
    }

    #[test]
    fn multiple_optional_params_match_first() {
        let path = "/foo/bar";
        let def = (
            OptionalParamSegment("a"),
            OptionalParamSegment("b"),
            StaticSegment("bar"),
        );
        let matched = def.test(path).expect("couldn't match route");
        assert_eq!(matched.matched(), "/foo/bar");
        assert_eq!(matched.remaining(), "");
        let params = matched.params();
        assert_eq!(params[0], ("a".into(), "foo".into()));
    }

    #[test]
    fn multiple_optionals_can_match_both() {
        let path = "/foo/qux/bar";
        let def = (
            OptionalParamSegment("a"),
            OptionalParamSegment("b"),
            StaticSegment("bar"),
        );
        let matched = def.test(path).expect("couldn't match route");
        assert_eq!(matched.matched(), "/foo/qux/bar");
        assert_eq!(matched.remaining(), "");
        let params = matched.params();
        assert_eq!(params[0], ("a".into(), "foo".into()));
        assert_eq!(params[1], ("b".into(), "qux".into()));
    }

    // A path that does not start with `/` is not at the start of a segment (the rule
    // `StaticSegment` already had). These segments used to drop the path's first character
    // uncounted and then split the path at a byte count that was off by that character: a
    // wrong match for ASCII, and a panic ("not a char boundary") when the first character
    // is multibyte. `RouteDefs::match_route` hands them such paths when a base is a partial
    // prefix of the path (see `matching::tests::base_that_is_a_partial_prefix_is_no_match`).

    #[test]
    fn param_without_leading_slash_is_no_match() {
        assert!(ParamSegment("a").test("éa").is_none());
        assert!(ParamSegment("a").test("foo").is_none());
        assert!(ParamSegment("a").test("foo/bar").is_none());
    }

    #[test]
    fn wildcard_without_leading_slash_is_no_match() {
        assert!(WildcardSegment("a").test("éa").is_none());
        assert!(WildcardSegment("a").test("foo/bar").is_none());
    }

    #[test]
    fn optional_param_without_leading_slash_matches_nothing() {
        for path in ["éa", "foo", "foo/bar"] {
            let matched = OptionalParamSegment("a")
                .test(path)
                .expect("an optional param always matches");
            assert_eq!(matched.matched(), "");
            assert_eq!(matched.remaining(), path);
            assert!(matched.params().is_empty());
        }
    }

    #[test]
    fn params_capture_multibyte_values() {
        let matched = ParamSegment("a").test("/é🦀/x").expect("param");
        assert_eq!(matched.matched(), "/é🦀");
        assert_eq!(matched.remaining(), "/x");
        assert_eq!(matched.params(), vec![("a".into(), "é🦀".into())]);

        let matched = OptionalParamSegment("a").test("/ñ").expect("optional");
        assert_eq!(matched.matched(), "/ñ");
        assert_eq!(matched.params(), vec![("a".into(), "ñ".into())]);

        let matched = WildcardSegment("a").test("/é/ü/").expect("wildcard");
        assert_eq!(matched.matched(), "/é/ü/");
        assert_eq!(matched.remaining(), "");
        assert_eq!(matched.params(), vec![("a".into(), "é/ü/".into())]);
    }

    #[test]
    fn empty_and_slash_only_paths() {
        assert!(ParamSegment("a").test("").is_none());
        assert!(ParamSegment("a").test("/").is_none());
        assert!(ParamSegment("a").test("//x").is_none());

        let matched = WildcardSegment("a").test("").expect("wildcard");
        assert_eq!((matched.matched(), matched.remaining()), ("", ""));
        assert_eq!(matched.params(), vec![("a".into(), "".into())]);
        let matched = WildcardSegment("a").test("/").expect("wildcard");
        assert_eq!((matched.matched(), matched.remaining()), ("/", ""));
        assert_eq!(matched.params(), vec![("a".into(), "".into())]);

        for path in ["", "/", "//x"] {
            let matched = OptionalParamSegment("a").test(path).expect("opt");
            assert_eq!((matched.matched(), matched.remaining()), ("", path));
            assert!(matched.params().is_empty());
        }
    }
}
