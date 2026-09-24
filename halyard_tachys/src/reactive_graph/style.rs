use super::{
    detached_element, take_effect_value, update_effect_value, ReactiveFunction,
    SharedReactiveFunction,
};
use crate::{
    html::style::{IntoStyle, IntoStyleValue},
    renderer::Rndr,
};
use halyard_reactive_graph::effect::RenderEffect;
use std::sync::Arc;

impl<F, S> IntoStyleValue for F
where
    F: ReactiveFunction<Output = S>,
    S: IntoStyleValue + 'static,
{
    type AsyncOutput = Self;
    type State = (Arc<str>, RenderEffect<S::State>);
    type Cloneable = SharedReactiveFunction<S>;
    type CloneableOwned = SharedReactiveFunction<S>;

    fn to_html(self, name: &str, style: &mut String) {
        let mut f = self;
        let value = f.invoke();
        value.to_html(name, style);
    }

    fn build(
        mut self,
        style: &crate::renderer::dom::CssStyleDeclaration,
        name: &str,
    ) -> Self::State {
        let name: Arc<str> = Rndr::intern(name).into();
        let style = style.to_owned();
        (
            Arc::clone(&name),
            RenderEffect::new(move |prev| {
                let value = self.invoke();
                if let Some(mut state) = prev {
                    value.rebuild(&style, &name, &mut state);
                    state
                } else {
                    value.build(&style, &name)
                }
            }),
        )
    }

    fn rebuild(
        mut self,
        style: &crate::renderer::dom::CssStyleDeclaration,
        name: &str,
        state: &mut Self::State,
    ) {
        let (prev_name, prev_effect) = state;
        let mut prev_value = prev_effect.take_value();
        if name != prev_name.as_ref() {
            Rndr::remove_css_property(style, prev_name.as_ref());
            prev_value = None;
        }
        let name: Arc<str> = name.into();
        let style = style.to_owned();

        *state = (
            Arc::clone(&name),
            RenderEffect::new_with_value(
                move |prev| {
                    let value = self.invoke();
                    if let Some(mut state) = prev {
                        value.rebuild(&style, &name, &mut state);
                        state
                    } else {
                        value.build(&style, &name)
                    }
                },
                prev_value,
            ),
        );
    }

    fn hydrate(
        mut self,
        style: &crate::renderer::dom::CssStyleDeclaration,
        name: &str,
    ) -> Self::State {
        let name: Arc<str> = Rndr::intern(name).into();
        let style = style.to_owned();
        (
            Arc::clone(&name),
            RenderEffect::new(move |prev| {
                let value = self.invoke();
                if let Some(mut state) = prev {
                    value.rebuild(&style, &name, &mut state);
                    state
                } else {
                    value.hydrate(&style, &name)
                }
            }),
        )
    }

    fn into_cloneable(self) -> Self::Cloneable {
        self.into_shared()
    }

    fn into_cloneable_owned(self) -> Self::CloneableOwned {
        self.into_shared()
    }

    fn dry_resolve(&mut self) {
        self.invoke();
    }

    async fn resolve(self) -> Self::AsyncOutput {
        self
    }
}

impl<F, C> IntoStyle for F
where
    F: ReactiveFunction<Output = C>,
    C: IntoStyle + 'static,
    C::State: 'static,
{
    type AsyncOutput = C::AsyncOutput;
    type State = RenderEffect<C::State>;
    type Cloneable = SharedReactiveFunction<C>;
    type CloneableOwned = SharedReactiveFunction<C>;

    fn to_html(mut self, style: &mut String) {
        let value = self.invoke();
        value.to_html(style);
    }

    fn hydrate<const FROM_SERVER: bool>(
        mut self,
        el: &crate::renderer::types::Element,
    ) -> Self::State {
        // TODO FROM_SERVER vs template
        let el = el.clone();
        RenderEffect::new(move |prev| {
            let value = self.invoke();
            if let Some(mut state) = prev {
                value.rebuild(&mut state);
                state
            } else {
                value.hydrate::<FROM_SERVER>(&el)
            }
        })
    }

    fn build(mut self, el: &crate::renderer::types::Element) -> Self::State {
        let el = el.clone();
        RenderEffect::new(move |prev| {
            let value = self.invoke();
            if let Some(mut state) = prev {
                value.rebuild(&mut state);
                state
            } else {
                value.build(&el)
            }
        })
    }

    fn rebuild(mut self, state: &mut Self::State) {
        const WHAT: &str = "a reactive style";
        let Some(prev_value) = take_effect_value(state, WHAT) else {
            return;
        };
        *state = RenderEffect::new_with_value(
            move |prev| {
                let value = self.invoke();
                if let Some(mut state) = prev {
                    value.rebuild(&mut state);
                    state
                } else {
                    value.build(&detached_element(WHAT))
                }
            },
            Some(prev_value),
        );
    }

    fn into_cloneable(self) -> Self::Cloneable {
        self.into_shared()
    }

    fn into_cloneable_owned(self) -> Self::CloneableOwned {
        self.into_shared()
    }

    fn dry_resolve(&mut self) {
        self.invoke();
    }

    async fn resolve(mut self) -> Self::AsyncOutput {
        self.invoke().resolve().await
    }

    // in place: a reset state is dropped right after (`Option::rebuild` to `None`), so its
    // function does not run again
    fn reset(state: &mut Self::State) {
        update_effect_value(state, "a reactive style", C::reset);
    }
}

macro_rules! style_reactive {
    ($name:ident, <$($gen:ident),*>, $v:ty, $( $where_clause:tt )*) =>
    {
        #[allow(deprecated)]
        impl<$($gen),*> IntoStyle for $name<$($gen),*>
        where
            $v: IntoStyle + Clone + Send + Sync + 'static,
            Option<$v>: IntoStyle,
            <Option<$v> as IntoStyle>::State: 'static,
            $($where_clause)*
        {
            type AsyncOutput = Self;
            type State = RenderEffect<<Option<$v> as IntoStyle>::State>;
            type Cloneable = Self;
            type CloneableOwned = Self;

            fn to_html(self, style: &mut String) {
                let value = halyard_reactive_graph::gone::render_value(&self);
                value.to_html(style);
            }

            fn hydrate<const FROM_SERVER: bool>(
                self,
                el: &crate::renderer::types::Element,
            ) -> Self::State {
                (move || halyard_reactive_graph::gone::render_value(&self)).hydrate::<FROM_SERVER>(el)
            }

            fn build(
                self,
                el: &crate::renderer::types::Element,
            ) -> Self::State {
                (move || halyard_reactive_graph::gone::render_value(&self)).build(el)
            }

            fn rebuild(self, state: &mut Self::State) {
                (move || halyard_reactive_graph::gone::render_value(&self)).rebuild(state)
            }

            fn into_cloneable(self) -> Self::Cloneable {
                self
            }

            fn into_cloneable_owned(self) -> Self::CloneableOwned {
                self
            }

            fn dry_resolve(&mut self) {}

            async fn resolve(self) -> Self::AsyncOutput {
                self
            }

            fn reset(state: &mut Self::State) {
                *state = RenderEffect::new_with_value(
                    move |prev| {
                        if let Some(mut state) = prev {
                            <Option<$v>>::reset(&mut state);
                            state
                        } else {
                            unreachable!()
                        }
                    },
                    state.take_value(),
                );
            }
        }

        #[allow(deprecated)]
        impl<$($gen),*> IntoStyleValue for $name<$($gen),*>
        where
            $v: IntoStyleValue + Send + Sync + Clone + 'static,
            Option<$v>: IntoStyleValue,
            $($where_clause)*
        {
            type AsyncOutput = Self;
            type State = (Arc<str>, RenderEffect<<Option<$v> as IntoStyleValue>::State>);
            type Cloneable = $name<$($gen),*>;
            type CloneableOwned = $name<$($gen),*>;

            fn to_html(self, name: &str, style: &mut String) {
                IntoStyleValue::to_html(move || halyard_reactive_graph::gone::render_value(&self), name, style)
            }

            fn build(
                self,
                style: &crate::renderer::dom::CssStyleDeclaration,
                name: &str,
            ) -> Self::State {
                IntoStyleValue::build(move || halyard_reactive_graph::gone::render_value(&self), style, name)
            }

            fn rebuild(
                self,
                style: &crate::renderer::dom::CssStyleDeclaration,
                name: &str,
                state: &mut Self::State,
            ) {
                IntoStyleValue::rebuild(
                    move || halyard_reactive_graph::gone::render_value(&self),
                    style,
                    name,
                    state,
                )
            }

            fn hydrate(
                self,
                style: &crate::renderer::dom::CssStyleDeclaration,
                name: &str,
            ) -> Self::State {
                IntoStyleValue::hydrate(move || halyard_reactive_graph::gone::render_value(&self), style, name)
            }

            fn into_cloneable(self) -> Self::Cloneable {
                self
            }

            fn into_cloneable_owned(self) -> Self::CloneableOwned {
                self
            }

            fn dry_resolve(&mut self) {}

            async fn resolve(self) -> Self::AsyncOutput {
                self
            }
        }
    };
}

mod stable {
    use super::RenderEffect;
    use crate::html::style::{IntoStyle, IntoStyleValue};
    #[allow(deprecated)]
    use halyard_reactive_graph::wrappers::read::MaybeSignal;
    use halyard_reactive_graph::{
        computed::{ArcMemo, Memo},
        owner::Storage,
        signal::{ArcReadSignal, ArcRwSignal, ReadSignal, RwSignal},
        traits::{IsDisposed, TryGet},
        wrappers::read::{ArcSignal, Signal},
    };
    use std::sync::Arc;

    style_reactive!(
        RwSignal,
        <V, S>,
        V,
        RwSignal<V, S>: TryGet<Value = V> + IsDisposed,
        S: Storage<V> + Storage<Option<V>>,
        S: Send + Sync + 'static,
    );
    style_reactive!(
        ReadSignal,
        <V, S>,
        V,
        ReadSignal<V, S>: TryGet<Value = V> + IsDisposed,
        S: Storage<V> + Storage<Option<V>>,
        S: Send + Sync + 'static,
    );
    style_reactive!(
        Memo,
        <V, S>,
        V,
        Memo<V, S>: TryGet<Value = V> + IsDisposed,
        S: Storage<V> + Storage<Option<V>>,
        S: Send + Sync + 'static,
    );
    style_reactive!(
        Signal,
        <V, S>,
        V,
        Signal<V, S>: TryGet<Value = V> + IsDisposed,
        S: Storage<V> + Storage<Option<V>>,
        S: Send + Sync + 'static,
    );
    style_reactive!(
        MaybeSignal,
        <V, S>,
        V,
        MaybeSignal<V, S>: TryGet<Value = V> + IsDisposed,
        S: Storage<V> + Storage<Option<V>>,
        S: Send + Sync + 'static,
    );
    style_reactive!(ArcRwSignal, <V>, V, ArcRwSignal<V>: TryGet<Value = V> + IsDisposed);
    style_reactive!(ArcReadSignal, <V>, V, ArcReadSignal<V>: TryGet<Value = V> + IsDisposed);
    style_reactive!(ArcMemo, <V>, V, ArcMemo<V>: TryGet<Value = V> + IsDisposed);
    style_reactive!(ArcSignal, <V>, V, ArcSignal<V>: TryGet<Value = V> + IsDisposed);
}

/*
impl<Fut> IntoStyle for Suspend<Fut>
where
    Fut: Clone + Future + Send + 'static,
    Fut::Output: IntoStyle,
{
    type AsyncOutput = Fut::Output;
    type State = Rc<RefCell<Option<<Fut::Output as IntoStyle>::State>>>;
    type Cloneable = Self;
    type CloneableOwned = Self;

    fn to_html(self, style: &mut String) {
        if let Some(inner) = self.inner.now_or_never() {
            inner.to_html(style);
        } else {
            panic!("You cannot use Suspend on an attribute outside Suspense");
        }
    }

    fn hydrate<const FROM_SERVER: bool>(
        self,
        el: &crate::renderer::types::Element,
    ) -> Self::State {
        let el = el.to_owned();
        let state = Rc::new(RefCell::new(None));
        halyard_reactive_graph::spawn_local_scoped({
            let state = Rc::clone(&state);
            async move {
                *state.borrow_mut() =
                    Some(self.inner.await.hydrate::<FROM_SERVER>(&el));
                self.subscriber.forward();
            }
        });
        state
    }

    fn build(self, el: &crate::renderer::types::Element) -> Self::State {
        let el = el.to_owned();
        let state = Rc::new(RefCell::new(None));
        halyard_reactive_graph::spawn_local_scoped({
            let state = Rc::clone(&state);
            async move {
                *state.borrow_mut() = Some(self.inner.await.build(&el));
                self.subscriber.forward();
            }
        });
        state
    }

    fn rebuild(self, state: &mut Self::State) {
        halyard_reactive_graph::spawn_local_scoped({
            let state = Rc::clone(state);
            async move {
                let value = self.inner.await;
                let mut state = state.borrow_mut();
                if let Some(state) = state.as_mut() {
                    value.rebuild(state);
                }
                self.subscriber.forward();
            }
        });
    }

    fn into_cloneable(self) -> Self::Cloneable {
        self
    }

    fn into_cloneable_owned(self) -> Self::CloneableOwned {
        self
    }

    fn dry_resolve(&mut self) {}

    async fn resolve(self) -> Self::AsyncOutput {
        self.inner.await
    }
}
*/
