use super::ChooseView;
use crate::error::{report_once, RouterError};
use futures::FutureExt;
use halyard_tachys::{
    erased::Erased,
    view::any_view::{AnyView, IntoAny},
};
use std::{future::Future, pin::Pin, sync::atomic::AtomicBool};

/// A type-erased [`ChooseView`].
pub struct AnyChooseView {
    value: Erased,
    clone: fn(&Erased) -> AnyChooseView,
    #[allow(clippy::type_complexity)]
    choose: fn(Erased) -> Pin<Box<dyn Future<Output = AnyView>>>,
    preload: for<'a> fn(&'a Erased) -> Pin<Box<dyn Future<Output = ()> + 'a>>,
}

impl Clone for AnyChooseView {
    fn clone(&self) -> Self {
        (self.clone)(&self.value)
    }
}

/// An `AnyChooseView` keeps its type-erased value next to functions made for the value's
/// type, both by `new`, so the value always has that type. If it had not, it chooses an
/// empty view (logged once) instead of reading the value as the wrong type.
fn type_mismatch(instead: &'static str) {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    report_once(
        &REPORTED,
        &RouterError::ErasedTypeMismatch {
            what: "an AnyChooseView",
            instead,
        },
    );
}

impl AnyChooseView {
    pub(crate) fn new<T: ChooseView>(value: T) -> Self {
        fn clone<T: ChooseView>(value: &Erased) -> AnyChooseView {
            match value.get_ref::<T>() {
                Some(value) => AnyChooseView::new(value.clone()),
                None => {
                    type_mismatch("its clone chooses an empty view");
                    AnyChooseView::new(())
                }
            }
        }

        fn choose<T: ChooseView>(
            value: Erased,
        ) -> Pin<Box<dyn Future<Output = AnyView>>> {
            match value.into_inner::<T>() {
                Some(value) => value.choose().boxed_local(),
                None => {
                    type_mismatch("it chooses an empty view");
                    Box::pin(async { ().into_any() })
                }
            }
        }

        fn preload<'a, T: ChooseView>(
            value: &'a Erased,
        ) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
            match value.get_ref::<T>() {
                Some(value) => value.preload().boxed_local(),
                None => {
                    type_mismatch("nothing is preloaded");
                    Box::pin(async {})
                }
            }
        }

        Self {
            value: Erased::new(value),
            clone: clone::<T>,
            choose: choose::<T>,
            preload: preload::<T>,
        }
    }
}

impl ChooseView for AnyChooseView {
    async fn choose(self) -> AnyView {
        (self.choose)(self.value).await
    }

    async fn preload(&self) {
        (self.preload)(&self.value).await;
    }
}

#[cfg(test)]
mod tests {
    use super::AnyChooseView;
    use crate::ChooseView;
    use futures::executor::block_on;
    use halyard_tachys::{erased::Erased, view::RenderHtml};

    /// An `AnyChooseView` whose value has another type than its functions (`new` makes
    /// the two together, so only code in this module can get here). Reading the value
    /// panicked ("Erased: type mismatch"), and with `--cfg erase_components` read a `u8`
    /// as the view. It chooses an empty view.
    #[test]
    fn an_any_choose_view_holding_another_type_chooses_an_empty_view() {
        let mismatched = || {
            let mut view = AnyChooseView::new(|| "hello");
            view.value = Erased::new(5u8);
            view
        };
        let empty = block_on(AnyChooseView::new(()).choose()).to_html();

        block_on(mismatched().preload());
        assert_eq!(block_on(mismatched().choose()).to_html(), empty);
        assert_eq!(block_on(mismatched().clone().choose()).to_html(), empty);
    }
}
