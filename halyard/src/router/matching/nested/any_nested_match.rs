#![allow(clippy::type_complexity)]
use crate::router::{
    error::{report_once, RouterError},
    matching::any_choose_view::AnyChooseView,
    ChooseView, MatchInterface, MatchParams, RouteMatchId,
};
use halyard_tachys::erased::ErasedLocal;
use std::{borrow::Cow, fmt::Debug, sync::atomic::AtomicBool};

/// A type-erased container for any [`MatchParams'] + [`MatchInterface`].
pub struct AnyNestedMatch {
    value: ErasedLocal,
    to_params: fn(&ErasedLocal) -> Vec<(Cow<'static, str>, String)>,
    as_id: fn(&ErasedLocal) -> RouteMatchId,
    as_matched: for<'a> fn(&'a ErasedLocal) -> &'a str,
    into_view_and_child:
        fn(ErasedLocal) -> (AnyChooseView, Option<AnyNestedMatch>),
}

impl Debug for AnyNestedMatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnyNestedMatch").finish_non_exhaustive()
    }
}

/// An `AnyNestedMatch` keeps its type-erased value next to functions made for the value's
/// type, both by `into_any_nested_match`, so the value always has that type. If it had
/// not, it acts as an empty match (`()`: no params, an empty view, no child), logged once,
/// instead of reading the value as the wrong type.
fn type_mismatch() {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    report_once(
        &REPORTED,
        &RouterError::ErasedTypeMismatch {
            what: "an AnyNestedMatch",
            instead: "it acts as an empty match",
        },
    );
}

/// Converts anything implementing [`MatchParams'] + [`MatchInterface`] into an erased type.
pub trait IntoAnyNestedMatch {
    /// Wraps the nested route.
    fn into_any_nested_match(self) -> AnyNestedMatch;
}

impl<T> IntoAnyNestedMatch for T
where
    T: MatchParams + MatchInterface + 'static,
{
    fn into_any_nested_match(self) -> AnyNestedMatch {
        let value = ErasedLocal::new(self);

        fn to_params<T: MatchParams + 'static>(
            value: &ErasedLocal,
        ) -> Vec<(Cow<'static, str>, String)> {
            match value.get_ref::<T>() {
                Some(value) => value.to_params(),
                None => {
                    type_mismatch();
                    ().to_params()
                }
            }
        }

        fn as_id<T: MatchInterface + 'static>(
            value: &ErasedLocal,
        ) -> RouteMatchId {
            match value.get_ref::<T>() {
                Some(value) => value.as_id(),
                None => {
                    type_mismatch();
                    ().as_id()
                }
            }
        }

        fn as_matched<T: MatchInterface + 'static>(
            value: &ErasedLocal,
        ) -> &str {
            match value.get_ref::<T>() {
                Some(value) => value.as_matched(),
                None => {
                    type_mismatch();
                    ""
                }
            }
        }

        fn into_view_and_child<T: MatchInterface + 'static>(
            value: ErasedLocal,
        ) -> (AnyChooseView, Option<AnyNestedMatch>) {
            let Some(value) = value.into_inner::<T>() else {
                type_mismatch();
                return (AnyChooseView::new(()), None);
            };
            let (view, child) = value.into_view_and_child();
            (
                AnyChooseView::new(view),
                child.map(|child| child.into_any_nested_match()),
            )
        }

        AnyNestedMatch {
            value,
            to_params: to_params::<T>,
            as_id: as_id::<T>,
            as_matched: as_matched::<T>,
            into_view_and_child: into_view_and_child::<T>,
        }
    }
}

impl MatchParams for AnyNestedMatch {
    fn to_params(&self) -> Vec<(Cow<'static, str>, String)> {
        (self.to_params)(&self.value)
    }
}

impl MatchInterface for AnyNestedMatch {
    type Child = AnyNestedMatch;

    fn as_id(&self) -> RouteMatchId {
        (self.as_id)(&self.value)
    }

    fn as_matched(&self) -> &str {
        (self.as_matched)(&self.value)
    }

    fn into_view_and_child(self) -> (impl ChooseView, Option<Self::Child>) {
        (self.into_view_and_child)(self.value)
    }
}

#[cfg(test)]
mod tests {
    use super::{AnyNestedMatch, IntoAnyNestedMatch};
    use crate::router::{
        ChooseView, MatchInterface, MatchParams, RouteMatchId,
    };
    use halyard_tachys::erased::ErasedLocal;
    use std::borrow::Cow;

    /// A match with a param, an id, a matched path and a child.
    struct Matched;

    impl MatchParams for Matched {
        fn to_params(&self) -> Vec<(Cow<'static, str>, String)> {
            vec![("id".into(), "1".into())]
        }
    }

    impl MatchInterface for Matched {
        type Child = ();

        fn as_id(&self) -> RouteMatchId {
            RouteMatchId(7)
        }

        fn as_matched(&self) -> &str {
            "/a"
        }

        fn into_view_and_child(self) -> (impl ChooseView, Option<Self::Child>) {
            (|| "a", Some(()))
        }
    }

    /// An `AnyNestedMatch` whose value has another type than its functions
    /// (`into_any_nested_match` makes the two together, so only code in this module can
    /// get here). Reading the value panicked ("Erased: type mismatch"), and with
    /// `--cfg erase_components` read a `u8` as the match. It acts as an empty match.
    #[test]
    fn an_any_nested_match_holding_another_type_is_an_empty_match() {
        let matched = Matched.into_any_nested_match();
        assert_eq!(matched.to_params().len(), 1);
        assert_eq!(matched.as_matched(), "/a");

        let mismatched = || -> AnyNestedMatch {
            let mut matched = Matched.into_any_nested_match();
            matched.value = ErasedLocal::new(5u8);
            matched
        };
        assert!(mismatched().to_params().is_empty());
        assert_eq!(mismatched().as_id(), ().as_id());
        assert_eq!(mismatched().as_matched(), "");
        let (_view, child) = mismatched().into_view_and_child();
        assert!(child.is_none());
    }
}
