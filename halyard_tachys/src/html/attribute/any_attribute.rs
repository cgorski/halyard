use super::{Attribute, NextAttribute};
use crate::{
    erased::{Erased, ErasedLocal},
    html::attribute::NamedAttributeKey,
    renderer::{dom::Element, Rndr},
    view_error::{report_once, ViewError},
};
use std::{any::TypeId, fmt::Debug, mem, sync::atomic::AtomicBool};
#[cfg(feature = "ssr")]
use std::{future::Future, pin::Pin};

/// A type-erased container for any [`Attribute`].
pub struct AnyAttribute {
    type_id: TypeId,
    html_len: usize,
    value: Erased,
    clone: fn(&Erased) -> AnyAttribute,
    #[cfg(feature = "ssr")]
    to_html: fn(Erased, &mut String, &mut String, &mut String, &mut String),
    build: fn(Erased, el: crate::renderer::types::Element) -> AnyAttributeState,
    rebuild: fn(Erased, &mut AnyAttributeState),
    #[cfg(feature = "hydrate")]
    hydrate_from_server:
        fn(Erased, crate::renderer::types::Element) -> AnyAttributeState,
    #[cfg(feature = "hydrate")]
    hydrate_from_template:
        fn(Erased, crate::renderer::types::Element) -> AnyAttributeState,
    #[cfg(feature = "ssr")]
    #[allow(clippy::type_complexity)]
    resolve: fn(Erased) -> Pin<Box<dyn Future<Output = AnyAttribute> + Send>>,
    #[cfg(feature = "ssr")]
    dry_resolve: fn(&mut Erased),
    keys: fn(&Erased) -> Vec<NamedAttributeKey>,
}

impl Clone for AnyAttribute {
    fn clone(&self) -> Self {
        (self.clone)(&self.value)
    }
}

impl Debug for AnyAttribute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnyAttribute").finish_non_exhaustive()
    }
}

/// View state for [`AnyAttribute`].
pub struct AnyAttributeState {
    type_id: TypeId,
    state: ErasedLocal,
    el: crate::renderer::types::Element,
    keys: Vec<NamedAttributeKey>,
}

/// Converts an [`Attribute`] into [`AnyAttribute`].
pub trait IntoAnyAttribute {
    /// Wraps the given attribute.
    fn into_any_attr(self) -> AnyAttribute;
}

impl<T> IntoAnyAttribute for T
where
    Self: Send,
    T: Attribute,
    crate::renderer::types::Element: Clone,
{
    fn into_any_attr(self) -> AnyAttribute {
        fn clone<T: Attribute + Clone + 'static>(
            value: &Erased,
        ) -> AnyAttribute {
            value.get_ref::<T>().clone().into_any_attr()
        }

        #[cfg(feature = "ssr")]
        fn to_html<T: Attribute + 'static>(
            value: Erased,
            buf: &mut String,
            class: &mut String,
            style: &mut String,
            inner_html: &mut String,
        ) {
            value
                .into_inner::<T>()
                .to_html(buf, class, style, inner_html);
        }

        fn build<T: Attribute + 'static>(
            value: Erased,
            el: crate::renderer::types::Element,
        ) -> AnyAttributeState {
            AnyAttributeState {
                type_id: TypeId::of::<T>(),
                keys: value.get_ref::<T>().keys(),
                state: ErasedLocal::new(value.into_inner::<T>().build(&el)),
                el,
            }
        }

        #[cfg(feature = "hydrate")]
        fn hydrate_from_server<T: Attribute + 'static>(
            value: Erased,
            el: crate::renderer::types::Element,
        ) -> AnyAttributeState {
            AnyAttributeState {
                type_id: TypeId::of::<T>(),
                keys: value.get_ref::<T>().keys(),
                state: ErasedLocal::new(
                    value.into_inner::<T>().hydrate::<true>(&el),
                ),
                el,
            }
        }

        #[cfg(feature = "hydrate")]
        fn hydrate_from_template<T: Attribute + 'static>(
            value: Erased,
            el: crate::renderer::types::Element,
        ) -> AnyAttributeState {
            AnyAttributeState {
                type_id: TypeId::of::<T>(),
                keys: value.get_ref::<T>().keys(),
                state: ErasedLocal::new(
                    value.into_inner::<T>().hydrate::<false>(&el),
                ),
                el,
            }
        }

        fn rebuild<T: Attribute + 'static>(
            value: Erased,
            state: &mut AnyAttributeState,
        ) {
            let value = value.into_inner::<T>();
            let state = state.state.get_mut::<T::State>();
            value.rebuild(state);
        }

        #[cfg(feature = "ssr")]
        fn dry_resolve<T: Attribute + 'static>(value: &mut Erased) {
            value.get_mut::<T>().dry_resolve();
        }

        #[cfg(feature = "ssr")]
        fn resolve<T: Attribute + 'static>(
            value: Erased,
        ) -> Pin<Box<dyn Future<Output = AnyAttribute> + Send>> {
            use futures::FutureExt;

            async move {value.into_inner::<T>().resolve().await.into_any_attr()}.boxed()
        }

        fn keys<T: Attribute + 'static>(
            value: &Erased,
        ) -> Vec<NamedAttributeKey> {
            value.get_ref::<T>().keys()
        }

        let value = self.into_cloneable_owned();
        AnyAttribute {
            type_id: TypeId::of::<T::CloneableOwned>(),
            html_len: value.html_len(),
            value: Erased::new(value),
            clone: clone::<T::CloneableOwned>,
            #[cfg(feature = "ssr")]
            to_html: to_html::<T::CloneableOwned>,
            build: build::<T::CloneableOwned>,
            rebuild: rebuild::<T::CloneableOwned>,
            #[cfg(feature = "hydrate")]
            hydrate_from_server: hydrate_from_server::<T::CloneableOwned>,
            #[cfg(feature = "hydrate")]
            hydrate_from_template: hydrate_from_template::<T::CloneableOwned>,
            #[cfg(feature = "ssr")]
            resolve: resolve::<T::CloneableOwned>,
            #[cfg(feature = "ssr")]
            dry_resolve: dry_resolve::<T::CloneableOwned>,
            keys: keys::<T::CloneableOwned>,
        }
    }
}

impl NextAttribute for AnyAttribute {
    type Output<NewAttr: Attribute> = Vec<AnyAttribute>;

    fn add_any_attr<NewAttr: Attribute>(
        self,
        new_attr: NewAttr,
    ) -> Self::Output<NewAttr> {
        vec![self, new_attr.into_any_attr()]
    }
}

impl Attribute for AnyAttribute {
    const MIN_LENGTH: usize = 0;

    type AsyncOutput = AnyAttribute;
    type State = AnyAttributeState;
    type Cloneable = AnyAttribute;
    type CloneableOwned = AnyAttribute;

    fn html_len(&self) -> usize {
        self.html_len
    }

    #[allow(unused)] // they are used in SSR
    fn to_html(
        self,
        buf: &mut String,
        class: &mut String,
        style: &mut String,
        inner_html: &mut String,
    ) {
        #[cfg(feature = "ssr")]
        {
            (self.to_html)(self.value, buf, class, style, inner_html);
        }
        // without `ssr` an `AnyAttribute` keeps no HTML renderer (so that the browser bundle
        // does not carry one): it renders nothing
        #[cfg(not(feature = "ssr"))]
        {
            static REPORTED: AtomicBool = AtomicBool::new(false);
            report_once(
                &REPORTED,
                &ViewError::RenderedWithoutSsr {
                    what: "an AnyAttribute",
                },
            );
        }
    }

    fn hydrate<const FROM_SERVER: bool>(
        self,
        el: &crate::renderer::types::Element,
    ) -> Self::State {
        #[cfg(feature = "hydrate")]
        if FROM_SERVER {
            (self.hydrate_from_server)(self.value, el.clone())
        } else {
            (self.hydrate_from_template)(self.value, el.clone())
        }
        // without `hydrate` it is added to the element as on the client, which sets the
        // value the server rendered again and attaches what it needs
        #[cfg(not(feature = "hydrate"))]
        {
            static REPORTED: AtomicBool = AtomicBool::new(false);
            report_once(
                &REPORTED,
                &ViewError::HydratedWithoutHydrate {
                    what: "an AnyAttribute",
                },
            );
            self.build(el)
        }
    }

    fn build(self, el: &crate::renderer::types::Element) -> Self::State {
        (self.build)(self.value, el.clone())
    }

    fn rebuild(self, state: &mut Self::State) {
        if self.type_id == state.type_id {
            (self.rebuild)(self.value, state)
        } else {
            let new = self.build(&state.el);
            *state = new;
        }
    }

    fn into_cloneable(self) -> Self::Cloneable {
        self
    }

    fn into_cloneable_owned(self) -> Self::CloneableOwned {
        self
    }

    fn dry_resolve(&mut self) {
        // without `ssr` there is nothing to resolve; rendering logs
        #[cfg(feature = "ssr")]
        {
            (self.dry_resolve)(&mut self.value)
        }
    }

    async fn resolve(self) -> Self::AsyncOutput {
        // without `ssr` the attribute resolves to itself; rendering logs
        #[cfg(feature = "ssr")]
        {
            (self.resolve)(self.value).await
        }
        #[cfg(not(feature = "ssr"))]
        {
            self
        }
    }

    fn keys(&self) -> Vec<NamedAttributeKey> {
        (self.keys)(&self.value)
    }
}

impl NextAttribute for Vec<AnyAttribute> {
    type Output<NewAttr: Attribute> = Self;

    fn add_any_attr<NewAttr: Attribute>(
        mut self,
        new_attr: NewAttr,
    ) -> Self::Output<NewAttr> {
        self.push(new_attr.into_any_attr());
        self
    }
}

impl Attribute for Vec<AnyAttribute> {
    const MIN_LENGTH: usize = 0;

    type AsyncOutput = Vec<AnyAttribute>;
    type State = (Element, Vec<AnyAttributeState>);
    type Cloneable = Vec<AnyAttribute>;
    type CloneableOwned = Vec<AnyAttribute>;

    // each method hands every attribute to `AnyAttribute`, which handles a build without
    // `ssr` or `hydrate`

    fn html_len(&self) -> usize {
        self.iter()
            .map(|attr| attr.html_len())
            .fold(0, usize::saturating_add)
    }

    fn to_html(
        self,
        buf: &mut String,
        class: &mut String,
        style: &mut String,
        inner_html: &mut String,
    ) {
        for attr in self {
            attr.to_html(buf, class, style, inner_html)
        }
    }

    fn hydrate<const FROM_SERVER: bool>(
        self,
        el: &crate::renderer::types::Element,
    ) -> Self::State {
        (
            el.clone(),
            self.into_iter()
                .map(|attr| attr.hydrate::<FROM_SERVER>(el))
                .collect(),
        )
    }

    fn build(self, el: &crate::renderer::types::Element) -> Self::State {
        (
            el.clone(),
            self.into_iter().map(|attr| attr.build(el)).collect(),
        )
    }

    fn rebuild(self, state: &mut Self::State) {
        let (el, state) = state;
        for old in mem::take(state) {
            for key in old.keys {
                match key {
                    NamedAttributeKey::InnerHtml => {
                        Rndr::set_inner_html(&old.el, "");
                    }
                    NamedAttributeKey::Property(prop_name) => {
                        Rndr::set_property(
                            &old.el,
                            &prop_name,
                            &wasm_bindgen::JsValue::UNDEFINED,
                        );
                    }
                    NamedAttributeKey::Attribute(key) => {
                        Rndr::remove_attribute(&old.el, &key);
                    }
                }
            }
        }
        *state = self.into_iter().map(|s| s.build(el)).collect();
    }

    fn into_cloneable(self) -> Self::Cloneable {
        self
    }

    fn into_cloneable_owned(self) -> Self::CloneableOwned {
        self
    }

    fn dry_resolve(&mut self) {
        for attr in self.iter_mut() {
            attr.dry_resolve()
        }
    }

    async fn resolve(self) -> Self::AsyncOutput {
        futures::future::join_all(self.into_iter().map(|attr| attr.resolve()))
            .await
    }

    fn keys(&self) -> Vec<NamedAttributeKey> {
        self.iter().flat_map(|s| s.keys()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{AnyAttribute, IntoAnyAttribute};
    use crate::html::attribute::{custom::custom_attribute, id, Attribute};
    use futures::executor::block_on;

    fn render(attr: impl Attribute) -> String {
        let (mut buf, mut class, mut style, mut inner_html) =
            (String::new(), String::new(), String::new(), String::new());
        attr.to_html(&mut buf, &mut class, &mut style, &mut inner_html);
        buf
    }

    fn attrs() -> Vec<AnyAttribute> {
        vec![
            id("main").into_any_attr(),
            custom_attribute("data-x", "1").into_any_attr(),
        ]
    }

    /// Without the `ssr` feature an `AnyAttribute` has no HTML renderer. Rendering one to
    /// HTML panicked; it renders nothing.
    #[cfg(not(feature = "ssr"))]
    #[test]
    fn any_attribute_renders_nothing_without_the_ssr_feature() {
        assert_eq!(render(id("main").into_any_attr()), "");
        assert_eq!(render(attrs()), "");
    }

    /// Resolving (waiting for async data before rendering) panicked without `ssr`; it
    /// returns the attributes as they are.
    #[cfg(not(feature = "ssr"))]
    #[test]
    fn any_attribute_resolves_to_itself_without_the_ssr_feature() {
        let mut attr = id("main").into_any_attr();
        attr.dry_resolve();
        let resolved = block_on(attr.resolve());
        assert_eq!(resolved.keys().len(), 1);

        let mut list = attrs();
        list.dry_resolve();
        assert_eq!(block_on(list.resolve()).len(), 2);
    }

    #[cfg(feature = "ssr")]
    #[test]
    fn any_attribute_renders_its_attribute() {
        assert_eq!(render(id("main").into_any_attr()), " id=\"main\"");
        let mut list = attrs();
        list.dry_resolve();
        assert_eq!(
            render(block_on(list.resolve())),
            " id=\"main\" data-x=\"1\""
        );
    }

    /// A list's length estimate summed its attributes' and overflowed (in debug builds)
    /// for an attribute that estimates `usize::MAX`.
    #[test]
    fn any_attribute_list_length_estimate_saturates() {
        use crate::view_error::test_support::HugeValue;

        let list = vec![
            id(HugeValue).into_any_attr(),
            custom_attribute("data-x", "1").into_any_attr(),
        ];
        assert_eq!(list.html_len(), usize::MAX);
    }
}
