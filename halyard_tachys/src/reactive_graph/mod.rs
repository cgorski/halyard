use crate::{
    html::attribute::{any_attribute::AnyAttribute, Attribute, AttributeValue},
    hydration::Cursor,
    renderer::Rndr,
    ssr::StreamBuilder,
    view::{
        add_attr::AddAnyAttr, Mountable, Position, PositionState, Render,
        RenderHtml, ToTemplate,
    },
    view_error::{report_once, ViewError},
};
use halyard_reactive_graph::effect::RenderEffect;
use halyard_reactive_graph::or_poisoned::OrPoisoned;
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{atomic::AtomicBool, Arc, Mutex},
};

/// Types for two way data binding.
pub mod bind;
mod class;
mod inner_html;
/// Provides a reactive [`NodeRef`](node_ref::NodeRef) type.
pub mod node_ref;
mod owned;
mod property;
mod style;
mod suspense;

pub use owned::*;
pub use suspense::*;

// A render effect holds its value between runs, and calls its function with `None` only on
// its first run. A reactive attribute or view that is rebuilt replaces its effect with one
// for the new function, seeded with the old effect's value, so that function never sees
// `None`. The helpers below cover an effect that holds no value all the same (a re-entrant
// update while it runs): logged once, and the page keeps working.

/// Takes the value of the effect behind `what`, which is about to be replaced by an effect
/// for a new function. `None` (logged once) if it holds none: the caller keeps the effect it
/// has, which puts its value back when its run ends.
pub(crate) fn take_effect_value<T>(
    effect: &RenderEffect<T>,
    what: &'static str,
) -> Option<T> {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    let value = effect.take_value();
    if value.is_none() {
        report_once(
            &REPORTED,
            &ViewError::EffectWithoutValue {
                what,
                instead: "it keeps its previous effect",
            },
        );
    }
    value
}

/// Updates the value of the effect behind `what` in place (a reset). Does nothing (logged
/// once) if it holds none.
pub(crate) fn update_effect_value<T>(
    effect: &RenderEffect<T>,
    what: &'static str,
    update: impl FnOnce(&mut T),
) {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    if effect.with_value_mut(update).is_none() {
        report_once(
            &REPORTED,
            &ViewError::EffectWithoutValue {
                what,
                instead: "it is not reset",
            },
        );
    }
}

/// An element, not in the page, for the effect behind `what` to create its value on when its
/// function is called without one (logged once): the effect keeps a consistent state, and
/// its updates no longer reach the page.
pub(crate) fn detached_element(
    what: &'static str,
) -> crate::renderer::types::Element {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    report_once(
        &REPORTED,
        &ViewError::EffectWithoutValue {
            what,
            instead: "it is created again outside the page",
        },
    );
    Rndr::create_element("div", None)
}

impl<F, V> ToTemplate for F
where
    F: ReactiveFunction<Output = V>,
    V: ToTemplate,
{
    const TEMPLATE: &'static str = V::TEMPLATE;

    fn to_template(
        buf: &mut String,
        class: &mut String,
        style: &mut String,
        inner_html: &mut String,
        position: &mut Position,
    ) {
        // FIXME this seems wrong
        V::to_template(buf, class, style, inner_html, position)
    }
}

impl<F, V> Render for F
where
    F: ReactiveFunction<Output = V>,
    V: Render,
    V::State: 'static,
{
    type State = RenderEffectState<V::State>;

    #[track_caller]
    fn build(mut self) -> Self::State {
        let hook = halyard_reactive_graph::throw_error::get_error_hook();
        RenderEffect::new(move |prev| {
            let _guard = hook.as_ref().map(|h| {
                halyard_reactive_graph::throw_error::set_error_hook(Arc::clone(
                    h,
                ))
            });
            let value = self.invoke();
            if let Some(mut state) = prev {
                value.rebuild(&mut state);
                state
            } else {
                value.build()
            }
        })
        .into()
    }

    #[track_caller]
    fn rebuild(self, state: &mut Self::State) {
        let new = self.build();
        let mut old = std::mem::replace(state, new);
        old.insert_before_this(state);
        old.unmount();
    }
}

/// Retained view state for a [`RenderEffect`].
pub struct RenderEffectState<T: 'static>(Option<RenderEffect<T>>);

impl<T> From<RenderEffect<T>> for RenderEffectState<T> {
    fn from(value: RenderEffect<T>) -> Self {
        Self(Some(value))
    }
}

impl<T> Mountable for RenderEffectState<T>
where
    T: Mountable,
{
    fn unmount(&mut self) {
        if let Some(ref mut inner) = self.0 {
            inner.unmount();
        }
    }

    fn mount(
        &mut self,
        parent: &crate::renderer::types::Element,
        marker: Option<&crate::renderer::types::Node>,
    ) {
        if let Some(ref mut inner) = self.0 {
            inner.mount(parent, marker);
        }
    }

    fn insert_before_this(&self, child: &mut dyn Mountable) -> bool {
        if let Some(inner) = &self.0 {
            inner.insert_before_this(child)
        } else {
            false
        }
    }

    fn elements(&self) -> Vec<crate::renderer::types::Element> {
        self.0
            .as_ref()
            .map(|inner| inner.elements())
            .unwrap_or_default()
    }
}

impl<F, V> RenderHtml for F
where
    F: ReactiveFunction<Output = V>,
    V: RenderHtml + 'static,
    V::State: 'static,
{
    type AsyncOutput = V::AsyncOutput;
    type Owned = Self;

    const MIN_LENGTH: usize = 0;

    fn dry_resolve(&mut self) {
        self.invoke().dry_resolve();
    }

    async fn resolve(mut self) -> Self::AsyncOutput {
        self.invoke().resolve().await
    }

    fn html_len(&self) -> usize {
        V::MIN_LENGTH
    }

    fn to_html_with_buf(
        mut self,
        buf: &mut String,
        position: &mut Position,
        escape: bool,
        mark_branches: bool,
        extra_attrs: Vec<AnyAttribute>,
    ) {
        let value = self.invoke();
        value.to_html_with_buf(
            buf,
            position,
            escape,
            mark_branches,
            extra_attrs,
        )
    }

    fn to_html_async_with_buf<const OUT_OF_ORDER: bool>(
        mut self,
        buf: &mut StreamBuilder,
        position: &mut Position,
        escape: bool,
        mark_branches: bool,
        extra_attrs: Vec<AnyAttribute>,
    ) where
        Self: Sized,
    {
        let value = self.invoke();
        value.to_html_async_with_buf::<OUT_OF_ORDER>(
            buf,
            position,
            escape,
            mark_branches,
            extra_attrs,
        );
    }

    fn hydrate<const FROM_SERVER: bool>(
        mut self,
        cursor: &Cursor,
        position: &PositionState,
    ) -> Self::State {
        /// codegen optimisation:
        fn prep(
            cursor: &Cursor,
            position: &PositionState,
        ) -> (
            Cursor,
            PositionState,
            Option<Arc<dyn halyard_reactive_graph::throw_error::ErrorHook>>,
        ) {
            let cursor = cursor.clone();
            let position = position.clone();
            let hook = halyard_reactive_graph::throw_error::get_error_hook();
            (cursor, position, hook)
        }
        let (cursor, position, hook) = prep(cursor, position);

        RenderEffect::new(move |prev| {
            /// codegen optimisation:
            fn get_guard(
                hook: &Option<Arc<dyn halyard_reactive_graph::throw_error::ErrorHook>>,
            ) -> Option<halyard_reactive_graph::throw_error::ResetErrorHookOnDrop> {
                hook.as_ref()
                    .map(|h| halyard_reactive_graph::throw_error::set_error_hook(Arc::clone(h)))
            }
            let _guard = get_guard(&hook);

            let value = self.invoke();
            if let Some(mut state) = prev {
                value.rebuild(&mut state);
                state
            } else {
                value.hydrate::<FROM_SERVER>(&cursor, &position)
            }
        })
        .into()
    }

    async fn hydrate_async(
        self,
        cursor: &Cursor,
        position: &PositionState,
    ) -> Self::State {
        /// codegen optimisation:
        fn prep(
            cursor: &Cursor,
            position: &PositionState,
        ) -> (
            Cursor,
            PositionState,
            Option<Arc<dyn halyard_reactive_graph::throw_error::ErrorHook>>,
        ) {
            let cursor = cursor.clone();
            let position = position.clone();
            let hook = halyard_reactive_graph::throw_error::get_error_hook();
            (cursor, position, hook)
        }
        let (cursor, position, hook) = prep(cursor, position);

        let mut fun = self.into_shared();

        RenderEffect::new_with_async_value(
            {
                let mut fun = fun.clone();
                move |prev| {
                    /// codegen optimisation:
                    fn get_guard(
                        hook: &Option<Arc<dyn halyard_reactive_graph::throw_error::ErrorHook>>,
                    ) -> Option<halyard_reactive_graph::throw_error::ResetErrorHookOnDrop>
                    {
                        hook.as_ref().map(|h| {
                            halyard_reactive_graph::throw_error::set_error_hook(Arc::clone(h))
                        })
                    }
                    let _guard = get_guard(&hook);

                    let value = fun.invoke();
                    if let Some(mut state) = prev {
                        value.rebuild(&mut state);
                        state
                    } else {
                        // seeded by the hydrated value, so never without one; if it is,
                        // the view is created again, outside the page
                        static REPORTED: AtomicBool = AtomicBool::new(false);
                        report_once(
                            &REPORTED,
                            &ViewError::EffectWithoutValue {
                                what: "a reactive view",
                                instead:
                                    "it is created again, outside the page",
                            },
                        );
                        value.build()
                    }
                }
            },
            async move { fun.invoke().hydrate_async(&cursor, &position).await },
        )
        .await
        .into()
    }

    fn into_owned(self) -> Self::Owned {
        self
    }
}

impl<F, V> AddAnyAttr for F
where
    F: ReactiveFunction<Output = V>,
    V: RenderHtml + 'static,
{
    type Output<SomeNewAttr: Attribute> =
        Box<dyn FnMut() -> V::Output<SomeNewAttr::CloneableOwned> + Send>;

    fn add_any_attr<NewAttr: Attribute>(
        mut self,
        attr: NewAttr,
    ) -> Self::Output<NewAttr>
    where
        Self::Output<NewAttr>: RenderHtml,
    {
        let attr = attr.into_cloneable_owned();
        Box::new(move || self.invoke().add_any_attr(attr.clone()))
    }
}

impl<M> Mountable for RenderEffect<M>
where
    M: Mountable + 'static,
{
    fn unmount(&mut self) {
        self.with_value_mut(|state| state.unmount());
    }

    fn mount(
        &mut self,
        parent: &crate::renderer::types::Element,
        marker: Option<&crate::renderer::types::Node>,
    ) {
        self.with_value_mut(|state| {
            state.mount(parent, marker);
        });
    }

    fn insert_before_this(&self, child: &mut dyn Mountable) -> bool {
        self.with_value_mut(|value| value.insert_before_this(child))
            .unwrap_or(false)
    }

    fn elements(&self) -> Vec<crate::renderer::types::Element> {
        self.with_value_mut(|inner| inner.elements())
            .unwrap_or_default()
    }
}

impl<T> Drop for RenderEffectState<T> {
    fn drop(&mut self) {
        if let Some(effect) = self.0.take() {
            drop(effect.take_value());
            drop(effect);
        }
    }
}

impl<M, E> Mountable for Result<M, E>
where
    M: Mountable,
{
    fn unmount(&mut self) {
        if let Ok(ref mut inner) = self {
            inner.unmount();
        }
    }

    fn mount(
        &mut self,
        parent: &crate::renderer::types::Element,
        marker: Option<&crate::renderer::types::Node>,
    ) {
        if let Ok(ref mut inner) = self {
            inner.mount(parent, marker);
        }
    }

    fn insert_before_this(&self, child: &mut dyn Mountable) -> bool {
        if let Ok(inner) = &self {
            inner.insert_before_this(child)
        } else {
            false
        }
    }

    fn elements(&self) -> Vec<crate::renderer::types::Element> {
        self.as_ref()
            .map(|inner| inner.elements())
            .unwrap_or_default()
    }
}

// Dynamic attributes
impl<F, V> AttributeValue for F
where
    F: ReactiveFunction<Output = V>,
    V: AttributeValue + 'static,
    V::State: 'static,
{
    type AsyncOutput = V::AsyncOutput;
    type State = RenderEffect<V::State>;
    type Cloneable = SharedReactiveFunction<V>;
    type CloneableOwned = SharedReactiveFunction<V>;

    fn html_len(&self) -> usize {
        0
    }

    fn to_html(mut self, key: &str, buf: &mut String) {
        let value = self.invoke();
        value.to_html(key, buf);
    }

    fn to_template(_key: &str, _buf: &mut String) {}

    fn hydrate<const FROM_SERVER: bool>(
        mut self,
        key: &str,
        el: &crate::renderer::types::Element,
    ) -> Self::State {
        let key = Rndr::intern(key);
        let key = key.to_owned();
        let el = el.to_owned();

        RenderEffect::new(move |prev| {
            let value = self.invoke();
            if let Some(mut state) = prev {
                value.rebuild(&key, &mut state);
                state
            } else {
                value.hydrate::<FROM_SERVER>(&key, &el)
            }
        })
    }

    fn build(
        mut self,
        el: &crate::renderer::types::Element,
        key: &str,
    ) -> Self::State {
        let key = Rndr::intern(key);
        let key = key.to_owned();
        let el = el.to_owned();

        RenderEffect::new(move |prev| {
            let value = self.invoke();
            if let Some(mut state) = prev {
                value.rebuild(&key, &mut state);
                state
            } else {
                value.build(&el, &key)
            }
        })
    }

    fn rebuild(mut self, key: &str, state: &mut Self::State) {
        const WHAT: &str = "a reactive attribute";
        let key = Rndr::intern(key);
        let key = key.to_owned();
        let Some(prev_value) = take_effect_value(state, WHAT) else {
            return;
        };

        *state = RenderEffect::new_with_value(
            move |prev| {
                let value = self.invoke();
                if let Some(mut state) = prev {
                    value.rebuild(&key, &mut state);
                    state
                } else {
                    value.build(&detached_element(WHAT), &key)
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

impl<V> AttributeValue for Suspend<V>
where
    V: AttributeValue + 'static,
    V::State: 'static,
{
    type State = Rc<RefCell<Option<V::State>>>;
    type AsyncOutput = V;
    type Cloneable = ();
    type CloneableOwned = ();

    fn html_len(&self) -> usize {
        0
    }

    fn to_html(self, _key: &str, _buf: &mut String) {
        #[cfg(feature = "tracing")]
        tracing::error!(
            "Suspended attributes cannot be used outside Suspense."
        );
    }

    fn to_template(_key: &str, _buf: &mut String) {}

    fn hydrate<const FROM_SERVER: bool>(
        self,
        key: &str,
        el: &crate::renderer::types::Element,
    ) -> Self::State {
        let key = key.to_owned();
        let el = el.to_owned();
        let state = Rc::new(RefCell::new(None));
        halyard_reactive_graph::spawn_local_scoped({
            let state = Rc::clone(&state);
            async move {
                *state.borrow_mut() =
                    Some(self.inner.await.hydrate::<FROM_SERVER>(&key, &el));
                self.subscriber.forward();
            }
        });
        state
    }

    fn build(
        self,
        el: &crate::renderer::types::Element,
        key: &str,
    ) -> Self::State {
        let key = key.to_owned();
        let el = el.to_owned();
        let state = Rc::new(RefCell::new(None));
        halyard_reactive_graph::spawn_local_scoped({
            let state = Rc::clone(&state);
            async move {
                *state.borrow_mut() = Some(self.inner.await.build(&el, &key));
                self.subscriber.forward();
            }
        });
        state
    }

    fn rebuild(self, key: &str, state: &mut Self::State) {
        let key = key.to_owned();
        halyard_reactive_graph::spawn_local_scoped({
            let state = Rc::clone(state);
            async move {
                let value = self.inner.await;
                let mut state = state.borrow_mut();
                if let Some(state) = state.as_mut() {
                    value.rebuild(&key, state);
                }
                self.subscriber.forward();
            }
        });
    }

    fn into_cloneable(self) -> Self::Cloneable {
        #[cfg(feature = "tracing")]
        tracing::error!("Suspended attributes cannot be spread");
    }

    fn into_cloneable_owned(self) -> Self::CloneableOwned {
        #[cfg(feature = "tracing")]
        tracing::error!("Suspended attributes cannot be spread");
    }

    fn dry_resolve(&mut self) {}

    async fn resolve(self) -> Self::AsyncOutput {
        self.inner.await
    }
}

/// A reactive function that can be shared across multiple locations and across threads.
pub type SharedReactiveFunction<T> = Arc<Mutex<dyn FnMut() -> T + Send>>;

/// A reactive view function.
pub trait ReactiveFunction: Send + 'static {
    /// The return type of the function.
    type Output;

    /// Call the function.
    fn invoke(&mut self) -> Self::Output;

    /// Converts the function into a cloneable, shared type.
    fn into_shared(self) -> Arc<Mutex<dyn FnMut() -> Self::Output + Send>>;
}

impl<T: 'static> ReactiveFunction for Arc<Mutex<dyn FnMut() -> T + Send>> {
    type Output = T;

    fn invoke(&mut self) -> Self::Output {
        // a panic in an earlier call poisons the lock; the function is still callable
        let mut fun = self.lock().or_poisoned();
        fun()
    }

    fn into_shared(self) -> Arc<Mutex<dyn FnMut() -> Self::Output + Send>> {
        self
    }
}

impl<T: Send + Sync + 'static> ReactiveFunction
    for Arc<dyn Fn() -> T + Send + Sync>
{
    type Output = T;

    fn invoke(&mut self) -> Self::Output {
        self()
    }

    fn into_shared(self) -> Arc<Mutex<dyn FnMut() -> Self::Output + Send>> {
        Arc::new(Mutex::new(move || self()))
    }
}

impl<F, T> ReactiveFunction for F
where
    F: FnMut() -> T + Send + 'static,
{
    type Output = T;

    fn invoke(&mut self) -> Self::Output {
        self()
    }

    fn into_shared(self) -> Arc<Mutex<dyn FnMut() -> Self::Output + Send>> {
        Arc::new(Mutex::new(self))
    }
}

macro_rules! reactive_impl {
    ($name:ident, <$($gen:ident),*>, $v:ty, $dry_resolve:literal, $( $where_clause:tt )*) =>
    {
        #[allow(deprecated)]
        impl<$($gen),*> Render for $name<$($gen),*>
        where
            $v: Render + Clone + Send + Sync + 'static,
            Option<$v>: Render,
            <Option<$v> as Render>::State: 'static,
            $($where_clause)*
        {
            type State = RenderEffectState<<Option<$v> as Render>::State>;

            #[track_caller]
            fn build(self) -> Self::State {
                (move || halyard_reactive_graph::gone::render_value(&self)).build()
            }

            #[track_caller]
            fn rebuild(self, state: &mut Self::State) {
                let new = self.build();
                let mut old = std::mem::replace(state, new);
                old.insert_before_this(state);
                old.unmount();
            }
        }

        #[allow(deprecated)]
        impl<$($gen),*> AddAnyAttr for $name<$($gen),*>
        where
            $v: RenderHtml + Clone + Send + Sync + 'static,
            Option<$v>: RenderHtml,
            <Option<$v> as Render>::State: 'static,
            $($where_clause)*
        {
            type Output<SomeNewAttr: Attribute> = Self;

            fn add_any_attr<NewAttr: Attribute>(
                self,
                _attr: NewAttr,
            ) -> Self::Output<NewAttr> {
                todo!()
            }
        }

        #[allow(deprecated)]
        impl<$($gen),*> RenderHtml for $name<$($gen),*>
        where
            $v: RenderHtml + Clone + Send + Sync + 'static,
            Option<$v>: RenderHtml,
            <Option<$v> as Render>::State: 'static,
            $($where_clause)*
        {
            type AsyncOutput = Self;
            type Owned = Self;

            const MIN_LENGTH: usize = 0;

            fn dry_resolve(&mut self) {
                if $dry_resolve {
                    _ = halyard_reactive_graph::gone::render_value(&*self);
                }
            }

            async fn resolve(self) -> Self::AsyncOutput {
                self
            }

            fn html_len(&self) -> usize {
                <Option<$v>>::MIN_LENGTH
            }

            fn to_html_with_buf(
                self,
                buf: &mut String,
                position: &mut Position,
                escape: bool,
                mark_branches: bool,
                extra_attrs: Vec<AnyAttribute>,
            ) {
                let value = halyard_reactive_graph::gone::render_value(&self);
                value.to_html_with_buf(
                    buf,
                    position,
                    escape,
                    mark_branches,
                    extra_attrs,
                )
            }

            fn to_html_async_with_buf<const OUT_OF_ORDER: bool>(
                self,
                buf: &mut StreamBuilder,
                position: &mut Position,
                escape: bool,
                mark_branches: bool,
                extra_attrs: Vec<AnyAttribute>,
            ) where
                Self: Sized,
            {
                let value = halyard_reactive_graph::gone::render_value(&self);
                value.to_html_async_with_buf::<OUT_OF_ORDER>(
                    buf,
                    position,
                    escape,
                    mark_branches,
                    extra_attrs,
                );
            }

            fn hydrate<const FROM_SERVER: bool>(
                self,
                cursor: &Cursor,
                position: &PositionState,
            ) -> Self::State {
                (move || halyard_reactive_graph::gone::render_value(&self))
                    .hydrate::<FROM_SERVER>(cursor, position)
            }

            fn into_owned(self) -> Self::Owned {
                self
            }
        }

        #[allow(deprecated)]
        impl<$($gen),*> AttributeValue for $name<$($gen),*>
        where
            $v: AttributeValue + Send + Sync + Clone + 'static,
            Option<$v>: AttributeValue,
            <Option<$v> as AttributeValue>::State: 'static,
            $($where_clause)*
        {
            type AsyncOutput = Self;
            type State = RenderEffect<<Option<$v> as AttributeValue>::State>;
            type Cloneable = Self;
            type CloneableOwned = Self;

            fn html_len(&self) -> usize {
                0
            }

            fn to_html(self, key: &str, buf: &mut String) {
                let value = halyard_reactive_graph::gone::render_value(&self);
                value.to_html(key, buf);
            }

            fn to_template(_key: &str, _buf: &mut String) {}

            fn hydrate<const FROM_SERVER: bool>(
                self,
                key: &str,
                el: &crate::renderer::types::Element,
            ) -> Self::State {
                (move || halyard_reactive_graph::gone::render_value(&self)).hydrate::<FROM_SERVER>(key, el)
            }

            fn build(
                self,
                el: &crate::renderer::types::Element,
                key: &str,
            ) -> Self::State {
                (move || halyard_reactive_graph::gone::render_value(&self)).build(el, key)
            }

            fn rebuild(self, key: &str, state: &mut Self::State) {
                (move || halyard_reactive_graph::gone::render_value(&self)).rebuild(key, state)
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
    use super::RenderEffectState;
    use crate::{
        html::attribute::{
            any_attribute::AnyAttribute, Attribute, AttributeValue,
        },
        hydration::Cursor,
        ssr::StreamBuilder,
        view::{
            add_attr::AddAnyAttr, Mountable, Position, PositionState, Render,
            RenderHtml,
        },
    };
    #[allow(deprecated)]
    use halyard_reactive_graph::wrappers::read::MaybeSignal;
    use halyard_reactive_graph::{
        computed::{ArcMemo, Memo},
        effect::RenderEffect,
        owner::Storage,
        signal::{
            ArcMappedSignal, ArcReadSignal, ArcRwSignal, MappedSignal,
            ReadSignal, RwSignal,
        },
        traits::{IsDisposed, TryGet},
        wrappers::read::{ArcSignal, MaybeProp, Signal, SignalTypes},
    };

    reactive_impl!(
        RwSignal,
        <V, S>,
        V,
        false,
        RwSignal<V, S>: TryGet<Value = V> + IsDisposed,
        S: Storage<V> + Storage<Option<V>>,
        S: Send + Sync + 'static,
    );
    reactive_impl!(
        ReadSignal,
        <V, S>,
        V,
        false,
        ReadSignal<V, S>: TryGet<Value = V> + IsDisposed,
        S: Storage<V> + Storage<Option<V>>,
        S: Send + Sync + 'static,
    );
    reactive_impl!(
        Memo,
        <V, S>,
        V,
        true,
        Memo<V, S>: TryGet<Value = V> + IsDisposed,
        S: Storage<V> + Storage<Option<V>>,
        S: Send + Sync + 'static,
    );
    reactive_impl!(
        Signal,
        <V, S>,
        V,
        true,
        Signal<V, S>: TryGet<Value = V> + IsDisposed,
        S: Storage<V> + Storage<Option<V>>,
        S: Send + Sync + 'static,
    );
    reactive_impl!(
        MaybeSignal,
        <V, S>,
        V,
        true,
        MaybeSignal<V, S>: TryGet<Value = V> + IsDisposed,
        S: Storage<V> + Storage<Option<V>>,
        S: Send + Sync + 'static,
    );
    reactive_impl!(
        MaybeProp,
        <V, S>,
        Option<V>,
        true,
        MaybeProp<V, S>: TryGet<Value = Option<V>> + IsDisposed,
        S: Storage<Option<V>> + Storage<SignalTypes<Option<V>, S>>,
        S: Send + Sync + 'static,
    );
    reactive_impl!(ArcRwSignal, <V>, V, false, ArcRwSignal<V>: TryGet<Value = V> + IsDisposed);
    reactive_impl!(ArcReadSignal, <V>, V, false, ArcReadSignal<V>: TryGet<Value = V> + IsDisposed);
    reactive_impl!(ArcMemo, <V>, V, false, ArcMemo<V>: TryGet<Value = V> + IsDisposed);
    reactive_impl!(ArcSignal, <V>, V, true, ArcSignal<V>: TryGet<Value = V> + IsDisposed);
    reactive_impl!(MappedSignal, <V>, V, false, MappedSignal<V>: TryGet<Value = V> + IsDisposed);
    reactive_impl!(ArcMappedSignal, <V>, V, false, ArcMappedSignal<V>: TryGet<Value = V> + IsDisposed);
}

#[cfg(test)]
mod tests {
    use super::{ReactiveFunction, SharedReactiveFunction};
    use std::{
        panic,
        sync::{Arc, Mutex},
        thread,
    };

    /// A shared reactive function whose lock was poisoned (a panic while it ran, on another
    /// thread of the server) panicked on every later call ("lock poisoned"). It runs.
    #[test]
    fn shared_reactive_function_runs_after_its_lock_was_poisoned() {
        let shared: SharedReactiveFunction<i32> = Arc::new(Mutex::new(|| 42));
        let poisoner = Arc::clone(&shared);
        let poisoned = thread::spawn(move || {
            let _guard = poisoner.lock();
            panic::panic_any("poisoning the lock");
        })
        .join();
        assert!(poisoned.is_err());
        assert!(shared.is_poisoned());

        let mut fun = shared;
        assert_eq!(fun.invoke(), 42);
    }
}
