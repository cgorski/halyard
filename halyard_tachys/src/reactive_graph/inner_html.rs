use super::{
    detached_element, take_effect_value, ReactiveFunction,
    SharedReactiveFunction,
};
use crate::html::element::InnerHtmlValue;
use halyard_reactive_graph::effect::RenderEffect;

impl<F, V> InnerHtmlValue for F
where
    F: ReactiveFunction<Output = V>,
    V: InnerHtmlValue + 'static,
    V::State: 'static,
{
    type AsyncOutput = V::AsyncOutput;
    type State = RenderEffect<V::State>;
    type Cloneable = SharedReactiveFunction<V>;
    type CloneableOwned = SharedReactiveFunction<V>;

    fn html_len(&self) -> usize {
        0
    }

    fn to_html(mut self, buf: &mut String) {
        let value = self.invoke();
        value.to_html(buf);
    }

    fn to_template(_buf: &mut String) {}

    fn hydrate<const FROM_SERVER: bool>(
        mut self,
        el: &crate::renderer::types::Element,
    ) -> Self::State {
        let el = el.to_owned();
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
        let el = el.to_owned();
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
        const WHAT: &str = "a reactive inner_html";
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
}

macro_rules! inner_html_reactive {
    ($name:ident, <$($gen:ident),*>, $v:ty, $( $where_clause:tt )*) =>
    {
        #[allow(deprecated)]
        impl<$($gen),*> InnerHtmlValue for $name<$($gen),*>
        where
            $v: InnerHtmlValue + Clone + Send + Sync + 'static,
            Option<$v>: InnerHtmlValue,
            <Option<$v> as InnerHtmlValue>::State: 'static,
            $($where_clause)*
        {
            type AsyncOutput = Self;
            type State = RenderEffect<<Option<$v> as InnerHtmlValue>::State>;
            type Cloneable = Self;
            type CloneableOwned = Self;

            fn html_len(&self) -> usize {
                0
            }

            fn to_html(self, buf: &mut String) {
                let value = halyard_reactive_graph::gone::render_value(&self);
                value.to_html(buf);
            }

            fn to_template(_buf: &mut String) {}

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
        }
    };
}

mod stable {
    use crate::html::element::InnerHtmlValue;
    #[allow(deprecated)]
    use halyard_reactive_graph::wrappers::read::MaybeSignal;
    use halyard_reactive_graph::{
        computed::{ArcMemo, Memo},
        effect::RenderEffect,
        owner::Storage,
        signal::{ArcReadSignal, ArcRwSignal, ReadSignal, RwSignal},
        traits::{IsDisposed, TryGet},
        wrappers::read::{ArcSignal, Signal},
    };

    inner_html_reactive!(
        RwSignal,
        <V, S>,
        V,
        RwSignal<V, S>: TryGet<Value = V> + IsDisposed,
        S: Storage<V> + Storage<Option<V>>,
        S: Send + Sync + 'static,
    );
    inner_html_reactive!(
        ReadSignal,
        <V, S>,
        V,
        ReadSignal<V, S>: TryGet<Value = V> + IsDisposed,
        S: Storage<V> + Storage<Option<V>>,
        S: Send + Sync + 'static,
    );
    inner_html_reactive!(
        Memo,
        <V, S>,
        V,
        Memo<V, S>: TryGet<Value = V> + IsDisposed,
        S: Storage<V> + Storage<Option<V>>,
        S: Send + Sync + 'static,
    );
    inner_html_reactive!(
        Signal,
        <V, S>,
        V,
        Signal<V, S>: TryGet<Value = V> + IsDisposed,
        S: Storage<V> + Storage<Option<V>>,
        S: Send + Sync + 'static,
    );
    inner_html_reactive!(
        MaybeSignal,
        <V, S>,
        V,
        MaybeSignal<V, S>: TryGet<Value = V> + IsDisposed,
        S: Storage<V> + Storage<Option<V>>,
        S: Send + Sync + 'static,
    );
    inner_html_reactive!(ArcRwSignal, <V>, V, ArcRwSignal<V>: TryGet<Value = V> + IsDisposed);
    inner_html_reactive!(ArcReadSignal, <V>, V, ArcReadSignal<V>: TryGet<Value = V> + IsDisposed);
    inner_html_reactive!(ArcMemo, <V>, V, ArcMemo<V>: TryGet<Value = V> + IsDisposed);
    inner_html_reactive!(ArcSignal, <V>, V, ArcSignal<V>: TryGet<Value = V> + IsDisposed);
}
