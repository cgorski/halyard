use super::{PartialPathMatch, PathSegment, PossibleRouteMatch};

/// The part of `path` that the segments of a tuple have matched so far.
///
/// A segment's `matched` is usually what it consumed, but `StaticSegment("/")` reports its
/// `/` as matched and leaves it in the remaining path for the next segment. So the matched
/// prefix ends where the last consumed part ends, or after a reported `/` that nothing
/// consumed after it (the trailing slash of `path!("/foo/")`). Adding up the matched
/// lengths instead counts such a `/` twice when a later segment consumes it.
struct MatchedPrefix<'a> {
    path: &'a str,
    end: usize,
}

impl<'a> MatchedPrefix<'a> {
    fn new(path: &'a str) -> Self {
        Self { path, end: 0 }
    }

    /// Records that a segment tested on `input` (the end of `path`) matched `matched` and
    /// left `remaining`. `None` if these are not parts of `path`, which only a custom
    /// [`PossibleRouteMatch`] could return: then the tuple does not match.
    fn record(
        &mut self,
        input: &str,
        matched: &str,
        remaining: &str,
    ) -> Option<()> {
        let start = self.path.len().checked_sub(input.len())?;
        let reported_end = start.checked_add(matched.len())?;
        let consumed_end = self.path.len().checked_sub(remaining.len())?;
        self.end = self.end.max(reported_end).max(consumed_end);
        Some(())
    }

    fn matched(&self) -> Option<&'a str> {
        self.path.get(..self.end)
    }
}

macro_rules! tuples {
    ($first:ident => $($ty:ident),*) => {
        impl<$first, $($ty),*> PossibleRouteMatch for ($first, $($ty,)*)
        where
            $first: PossibleRouteMatch,
			$($ty: PossibleRouteMatch),*,
        {
            fn optional(&self) -> bool {
                #[allow(non_snake_case)]
                let ($first, $($ty,)*) = &self;
                [$first.optional(), $($ty.optional()),*].into_iter().any(|n| n)
            }

            fn test<'a>(&self, path: &'a str) -> Option<PartialPathMatch<'a>> {
                #[allow(non_snake_case)]
                let ($first, $($ty,)*) = &self;

                // on the first run, include all optionals
                let mut include_optionals = {
                    [$first.optional(), $($ty.optional()),*].into_iter().filter(|n| *n).count()
                };

                loop {
                    let mut nth_field = 0usize;
                    let mut prefix = MatchedPrefix::new(path);
                    let mut r = path;

                    let mut p = Vec::new();

                    if $first.optional() {
                        nth_field = nth_field.saturating_add(1);
                    }
                    if !$first.optional() || nth_field <= include_optionals {
                        let PartialPathMatch { remaining, matched, params } =
                            $first.test(r)?;
                        prefix.record(r, matched, remaining)?;
                        p.extend(params);
                        r = remaining;
                    }

                    $(
                        if $ty.optional() {
                            nth_field = nth_field.saturating_add(1);
                        }
                        if !$ty.optional() || nth_field <= include_optionals {
                            let PartialPathMatch {
                                remaining,
                                matched,
                                params
                            } = match $ty.test(r) {
                                None => if $ty.optional() {
                                    return None;
                                } else {
                                    // retry with one optional fewer, if any is left
                                    include_optionals = include_optionals.checked_sub(1)?;
                                    continue;
                                },
                                Some(v) => v,
                            };
                            prefix.record(r, matched, remaining)?;
                            r = remaining;
                            p.extend(params);
                        }
                    )*
                    return Some(PartialPathMatch {
                        remaining: r,
                        matched: prefix.matched()?,
                        params: p
                    });
                }
            }

            fn generate_path(&self, path: &mut Vec<PathSegment>) {
                #[allow(non_snake_case)]
                let ($first, $($ty,)*) = &self;
                $first.generate_path(path);
                $(
                    $ty.generate_path(path);
                )*
            }
        }
	};
}

impl<A> PossibleRouteMatch for (A,)
where
    Self: core::fmt::Debug,
    A: PossibleRouteMatch,
{
    fn optional(&self) -> bool {
        self.0.optional()
    }

    fn test<'a>(&self, path: &'a str) -> Option<PartialPathMatch<'a>> {
        let PartialPathMatch {
            remaining,
            matched,
            params,
        } = self.0.test(path)?;
        let mut prefix = MatchedPrefix::new(path);
        prefix.record(path, matched, remaining)?;
        Some(PartialPathMatch {
            remaining,
            matched: prefix.matched()?,
            params,
        })
    }

    fn generate_path(&self, path: &mut Vec<PathSegment>) {
        self.0.generate_path(path);
    }
}

tuples!(A => B);
tuples!(A => B, C);
tuples!(A => B, C, D);
tuples!(A => B, C, D, E);
tuples!(A => B, C, D, E, F);
tuples!(A => B, C, D, E, F, G);
tuples!(A => B, C, D, E, F, G, H);
tuples!(A => B, C, D, E, F, G, H, I);
tuples!(A => B, C, D, E, F, G, H, I, J);
tuples!(A => B, C, D, E, F, G, H, I, J, K);
tuples!(A => B, C, D, E, F, G, H, I, J, K, L);
tuples!(A => B, C, D, E, F, G, H, I, J, K, L, M);
tuples!(A => B, C, D, E, F, G, H, I, J, K, L, M, N);
tuples!(A => B, C, D, E, F, G, H, I, J, K, L, M, N, O);
tuples!(A => B, C, D, E, F, G, H, I, J, K, L, M, N, O, P);
tuples!(A => B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q);
tuples!(A => B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R);
tuples!(A => B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S);
tuples!(A => B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T);
tuples!(A => B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U);
tuples!(A => B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V);
tuples!(A => B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W);
tuples!(A => B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W, X);
/*tuples!(
    A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W, X, Y
);
tuples!(
    A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W, X, Y,
    Z
);*/

#[cfg(test)]
mod tests {
    use crate::{
        OptionalParamSegment, ParamSegment, PossibleRouteMatch, StaticSegment,
        WildcardSegment,
    };

    /// `StaticSegment("/")` reports its `/` as matched but leaves it in the remaining path
    /// (so the next segment knows it starts a segment). The tuple used to add up the
    /// matched lengths, counting that `/` twice when a later segment consumed it: the
    /// matched prefix ran one byte past the end of the path, a panic.
    #[test]
    fn slash_segment_before_another_segment_is_counted_once() {
        let def = (StaticSegment("/"), StaticSegment("foo"));
        let matched = def.test("/foo").expect("should match");
        assert_eq!(matched.matched(), "/foo");
        assert_eq!(matched.remaining(), "");

        let def = (
            StaticSegment("foo"),
            StaticSegment("/"),
            StaticSegment("bar"),
        );
        let matched = def.test("/foo/bar").expect("should match");
        assert_eq!(matched.matched(), "/foo/bar");
        assert_eq!(matched.remaining(), "");
    }

    /// The same double count without running past the end: the matched prefix took one
    /// byte of the remaining path.
    #[test]
    fn slash_segment_before_a_param_reports_only_what_matched() {
        let def = (StaticSegment("/"), ParamSegment("id"));
        let matched = def.test("/42/x").expect("should match");
        assert_eq!(matched.matched(), "/42");
        assert_eq!(matched.remaining(), "/x");
        assert_eq!(matched.params(), vec![("id".into(), "42".into())]);
    }

    /// A trailing `/` (what `path!("/foo/")` generates) is still reported as matched.
    #[test]
    fn trailing_slash_segment_is_reported_as_matched() {
        let def = (StaticSegment("foo"), StaticSegment("/"));
        let matched = def.test("/foo/").expect("should match");
        assert_eq!(matched.matched(), "/foo/");
        assert_eq!(matched.remaining(), "/");
        assert!(def.test("/foo").is_none());
    }

    #[test]
    fn single_element_tuples() {
        let matched = (StaticSegment("/"),).test("/a").expect("match");
        assert_eq!((matched.matched(), matched.remaining()), ("/", "/a"));
        let matched = (ParamSegment("a"),).test("/é/b").expect("match");
        assert_eq!((matched.matched(), matched.remaining()), ("/é", "/b"));
        assert!((ParamSegment("a"),).test("éa").is_none());
    }

    /// The largest tuple (24 segments) on paths of 22 to 25 segments.
    #[test]
    fn twenty_four_segments() {
        let p = ParamSegment;
        let def = (
            p("a"),
            p("b"),
            p("c"),
            p("d"),
            p("e"),
            p("f"),
            p("g"),
            p("h"),
            p("i"),
            p("j"),
            p("k"),
            p("l"),
            p("m"),
            p("n"),
            p("o"),
            p("p"),
            p("q"),
            p("r"),
            p("s"),
            p("t"),
            p("u"),
            p("v"),
            p("w"),
            WildcardSegment("x"),
        );
        let path = |n: usize| -> String {
            (0..n).map(|i| format!("/{i}é")).collect()
        };

        let full = path(24);
        let matched = def.test(&full).expect("24 segments");
        assert_eq!(matched.matched(), full);
        let params = matched.params();
        assert_eq!(params.len(), 24);
        assert_eq!(params[23], ("x".into(), "23é".into()));

        let longer = path(25);
        let matched = def.test(&longer).expect("the wildcard takes the rest");
        assert_eq!(matched.params()[23], ("x".into(), "23é/24é".into()));

        // the wildcard matches an empty rest
        let shorter = path(23);
        let matched = def.test(&shorter).expect("empty wildcard");
        assert_eq!(matched.params()[23], ("x".into(), "".into()));

        assert!(def.test(&path(22)).is_none());
    }

    #[test]
    fn optionals_retry_without_running_out() {
        let def = (
            OptionalParamSegment("a"),
            OptionalParamSegment("b"),
            StaticSegment("end"),
        );
        assert!(def.test("/x/y/z").is_none());
        assert!(def.test("").is_none());
        let matched = def.test("/end").expect("no optionals");
        assert_eq!(matched.matched(), "/end");
        assert!(matched.params().is_empty());
    }
}
