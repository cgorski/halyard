use std::borrow::Cow;

pub fn resolve_path<'a>(
    base: &'a str,
    path: &'a str,
    from: Option<&'a str>,
) -> Cow<'a, str> {
    if has_scheme(path) {
        path.into()
    } else {
        let base_path = normalize(base, false);
        let from_path = from.map(|from| normalize(from, false));
        let result = if let Some(from_path) = from_path {
            if path.starts_with('/') {
                base_path
            } else if from_path.find(base_path.as_ref()) != Some(0) {
                concat(base_path, from_path)
            } else {
                from_path
            }
        } else {
            base_path
        };

        let result_empty = result.is_empty();
        let prefix = if result_empty { "/".into() } else { result };

        concat(prefix, normalize(path, result_empty))
    }
}

/// `left` followed by `right`, borrowing when one of them is empty (as `Cow`'s `+` does).
fn concat<'a>(left: Cow<'a, str>, right: Cow<'a, str>) -> Cow<'a, str> {
    if left.is_empty() {
        right
    } else if right.is_empty() {
        left
    } else {
        let mut joined = left.into_owned();
        joined.push_str(&right);
        Cow::Owned(joined)
    }
}

fn has_scheme(path: &str) -> bool {
    path.starts_with("//")
        || path.starts_with("tel:")
        || path.starts_with("mailto:")
        || path
            .split_once("://")
            .map(|(prefix, _)| {
                prefix.chars().all(
                    |c: char| matches!(c, 'a'..='z' | 'A'..='Z' | '0'..='9'),
                )
            })
            .unwrap_or(false)
}

#[doc(hidden)]
fn normalize(path: &str, omit_slash: bool) -> Cow<'_, str> {
    let s = path.trim_start_matches('/');
    // keep one of any trailing slashes: `s` up to and including the first of them, or all
    // of `s` if it has none (then that range ends past `s`)
    let s = s.get(..=s.trim_end_matches('/').len()).unwrap_or(s);
    if s.is_empty() || omit_slash || begins_with_query_or_hash(s) {
        s.into()
    } else {
        format!("/{s}").into()
    }
}

fn begins_with_query_or_hash(text: &str) -> bool {
    matches!(text.chars().next(), Some('#') | Some('?'))
}

/* TODO can remove?
#[doc(hidden)]
pub fn join_paths<'a>(from: &'a str, to: &'a str) -> String {
    let from = remove_wildcard(&normalize(from, false));
    from + normalize(to, false).as_ref()
}

fn remove_wildcard(text: &str) -> String {
    text.rsplit_once('*')
        .map(|(prefix, _)| prefix)
        .unwrap_or(text)
        .trim_end_matches('/')
        .to_string()
}
*/

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalize_query_string_with_opening_slash() {
        assert_eq!(normalize("/?foo=bar", false), "?foo=bar");
    }

    #[test]
    fn normalize_retain_trailing_slash() {
        assert_eq!(normalize("foo/bar/", false), "/foo/bar/");
    }

    #[test]
    fn normalize_dedup_trailing_slashes() {
        assert_eq!(normalize("foo/bar/////", false), "/foo/bar/");
    }

    #[test]
    fn normalize_edge_cases() {
        assert_eq!(normalize("", false), "");
        assert_eq!(normalize("/", false), "");
        assert_eq!(normalize("////", false), "");
        assert_eq!(normalize("a", true), "a");
        assert_eq!(normalize("//a//", true), "a/");
        assert_eq!(normalize("café///", false), "/café/");
        assert_eq!(normalize("#top", false), "#top");
    }

    /// The joins that `resolve_path` makes (`base + from`, `prefix + path`).
    #[test]
    fn resolve_path_joins() {
        assert_eq!(resolve_path("", "", None), "/");
        assert_eq!(resolve_path("", "/", None), "/");
        assert_eq!(resolve_path("", "foo", None), "/foo");
        assert_eq!(resolve_path("/base", "foo", None), "/base/foo");
        assert_eq!(resolve_path("/base", "/foo", Some("/x")), "/base/foo");
        assert_eq!(resolve_path("/base", "foo", Some("/x")), "/base/x/foo");
        assert_eq!(
            resolve_path("/base", "foo", Some("/base/x")),
            "/base/x/foo"
        );
        assert_eq!(resolve_path("", "foo", Some("/x/")), "/x//foo");
        assert_eq!(resolve_path("", "?q=1", Some("/x")), "/x?q=1");
        assert_eq!(resolve_path("/é", "ü", Some("/é/ñ")), "/é/ñ/ü");
        assert_eq!(
            resolve_path("", "https://a.b/c", Some("/x")),
            "https://a.b/c"
        );
    }
}
