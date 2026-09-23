use super::attribute::{
    maybe_next_attr_erasure_macros::next_attr_output_type, Attribute,
    NextAttribute,
};
use crate::{
    html::attribute::{
        maybe_next_attr_erasure_macros::next_attr_combine, NamedAttributeKey,
    },
    renderer::Rndr,
    view::{Position, ToTemplate},
    view_error::{report_once, ViewError},
};
use send_wrapper::SendWrapper;
use std::{
    borrow::Cow,
    sync::{atomic::AtomicBool, Arc},
};
use wasm_bindgen::JsValue;

/// Creates an [`Attribute`] that will set a DOM property on an element.
#[inline(always)]
pub fn prop<K, P>(key: K, value: P) -> Property<K, P>
where
    K: AsRef<str>,
    P: IntoProperty,
{
    Property {
        key,
        value: (!cfg!(feature = "ssr")).then(|| SendWrapper::new(value)),
    }
}

/// An [`Attribute`] that will set a DOM property on an element.
#[derive(Debug)]
pub struct Property<K, P> {
    key: K,
    // property values will only be accessed in the browser
    value: Option<SendWrapper<P>>,
}

impl<K, P> Clone for Property<K, P>
where
    K: Clone,
    P: Clone,
{
    fn clone(&self) -> Self {
        Self {
            key: self.key.clone(),
            value: self.value.clone(),
        }
    }
}

/// A property's value is not created when `ssr` is active (see
/// [`FEATURE_CONFLICT_DIAGNOSTIC`](super::FEATURE_CONFLICT_DIAGNOSTIC)). Without one, the
/// property is not set or updated (logged once), and its state is `None`.
fn value_missing(instead: &'static str) {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    report_once(
        &REPORTED,
        &ViewError::ClientValueMissing {
            what: "a property's value",
            instead,
        },
    );
}

impl<K, P> Attribute for Property<K, P>
where
    K: AsRef<str> + Send,
    P: IntoProperty,
{
    const MIN_LENGTH: usize = 0;

    type AsyncOutput = Self;
    /// `None` if the property had no value to set (it is not created with `ssr`, see
    /// [`prop`]).
    type State = Option<P::State>;
    type Cloneable = Property<Arc<str>, P::Cloneable>;
    type CloneableOwned = Property<Arc<str>, P::CloneableOwned>;

    #[inline(always)]
    fn html_len(&self) -> usize {
        0
    }

    fn to_html(
        self,
        _buf: &mut String,
        _class: &mut String,
        _style: &mut String,
        _inner_html: &mut String,
    ) {
    }

    fn hydrate<const FROM_SERVER: bool>(
        self,
        el: &crate::renderer::types::Element,
    ) -> Self::State {
        let Some(value) = self.value else {
            value_missing("is not set");
            return None;
        };
        Some(value.take().hydrate::<FROM_SERVER>(el, self.key.as_ref()))
    }

    fn build(self, el: &crate::renderer::types::Element) -> Self::State {
        let Some(value) = self.value else {
            value_missing("is not set");
            return None;
        };
        Some(value.take().build(el, self.key.as_ref()))
    }

    fn rebuild(self, state: &mut Self::State) {
        match (self.value, state) {
            (Some(value), Some(state)) => {
                value.take().rebuild(state, self.key.as_ref())
            }
            // without a value now, or when the element was built (then there is no
            // state to update), the property keeps what it has
            _ => value_missing("is not updated"),
        }
    }

    fn into_cloneable(self) -> Self::Cloneable {
        Property {
            key: self.key.as_ref().into(),
            value: self
                .value
                .map(|value| SendWrapper::new(value.take().into_cloneable())),
        }
    }

    fn into_cloneable_owned(self) -> Self::CloneableOwned {
        Property {
            key: self.key.as_ref().into(),
            value: self.value.map(|value| {
                SendWrapper::new(value.take().into_cloneable_owned())
            }),
        }
    }

    fn dry_resolve(&mut self) {}

    async fn resolve(self) -> Self::AsyncOutput {
        self
    }

    fn keys(&self) -> Vec<NamedAttributeKey> {
        vec![NamedAttributeKey::Property(
            self.key.as_ref().to_string().into(),
        )]
    }
}

impl<K, P> NextAttribute for Property<K, P>
where
    K: AsRef<str> + Send,
    P: IntoProperty,
{
    next_attr_output_type!(Self, NewAttr);

    fn add_any_attr<NewAttr: Attribute>(
        self,
        new_attr: NewAttr,
    ) -> Self::Output<NewAttr> {
        next_attr_combine!(self, new_attr)
    }
}

impl<K, P> ToTemplate for Property<K, P>
where
    K: AsRef<str>,
    P: IntoProperty,
{
    fn to_template(
        _buf: &mut String,
        _class: &mut String,
        _style: &mut String,
        _inner_html: &mut String,
        _position: &mut Position,
    ) {
    }
}

/// A possible value for a DOM property.
pub trait IntoProperty {
    /// The view state retained between building and rebuilding.
    type State;
    /// An equivalent value that can be cloned.
    type Cloneable: IntoProperty + Clone;
    /// An equivalent value that can be cloned and is `'static`.
    type CloneableOwned: IntoProperty + Clone + 'static;

    /// Adds the property on an element created from HTML.
    fn hydrate<const FROM_SERVER: bool>(
        self,
        el: &crate::renderer::types::Element,
        key: &str,
    ) -> Self::State;

    /// Adds the property during client-side rendering.
    fn build(
        self,
        el: &crate::renderer::types::Element,
        key: &str,
    ) -> Self::State;

    /// Updates the property with a new value.
    fn rebuild(self, state: &mut Self::State, key: &str);

    /// Converts this to a cloneable type.
    fn into_cloneable(self) -> Self::Cloneable;

    /// Converts this to a cloneable, owned type.
    fn into_cloneable_owned(self) -> Self::CloneableOwned;
}

macro_rules! prop_type {
    ($prop_type:ty) => {
        impl IntoProperty for $prop_type {
            type State = (crate::renderer::types::Element, JsValue);
            type Cloneable = Self;
            type CloneableOwned = Self;

            fn hydrate<const FROM_SERVER: bool>(
                self,
                el: &crate::renderer::types::Element,
                key: &str,
            ) -> Self::State {
                let value = self.into();
                Rndr::set_property_or_value(el, key, &value);
                (el.clone(), value)
            }

            fn build(
                self,
                el: &crate::renderer::types::Element,
                key: &str,
            ) -> Self::State {
                let value = self.into();
                Rndr::set_property_or_value(el, key, &value);
                (el.clone(), value)
            }

            fn rebuild(self, state: &mut Self::State, key: &str) {
                let (el, prev) = state;
                let value = self.into();
                Rndr::set_property_or_value(el, key, &value);
                *prev = value;
            }

            fn into_cloneable(self) -> Self::Cloneable {
                self
            }

            fn into_cloneable_owned(self) -> Self::CloneableOwned {
                self
            }
        }

        impl IntoProperty for Option<$prop_type> {
            type State = (crate::renderer::types::Element, JsValue);
            type Cloneable = Self;
            type CloneableOwned = Self;

            fn hydrate<const FROM_SERVER: bool>(
                self,
                el: &crate::renderer::types::Element,
                key: &str,
            ) -> Self::State {
                let was_some = self.is_some();
                let value = self.into();
                if was_some {
                    Rndr::set_property_or_value(el, key, &value);
                }
                (el.clone(), value)
            }

            fn build(
                self,
                el: &crate::renderer::types::Element,
                key: &str,
            ) -> Self::State {
                let was_some = self.is_some();
                let value = self.into();
                if was_some {
                    Rndr::set_property_or_value(el, key, &value);
                }
                (el.clone(), value)
            }

            fn rebuild(self, state: &mut Self::State, key: &str) {
                let (el, prev) = state;
                let value = self.into();
                Rndr::set_property_or_value(el, key, &value);
                *prev = value;
            }

            fn into_cloneable(self) -> Self::Cloneable {
                self
            }

            fn into_cloneable_owned(self) -> Self::CloneableOwned {
                self
            }
        }
    };
}

macro_rules! prop_type_str {
    ($prop_type:ty) => {
        impl IntoProperty for $prop_type {
            type State = (crate::renderer::types::Element, JsValue);
            type Cloneable = Arc<str>;
            type CloneableOwned = Arc<str>;

            fn hydrate<const FROM_SERVER: bool>(
                self,
                el: &crate::renderer::types::Element,
                key: &str,
            ) -> Self::State {
                let value = JsValue::from(&*self);
                Rndr::set_property_or_value(el, key, &value);
                (el.clone(), value)
            }

            fn build(
                self,
                el: &crate::renderer::types::Element,
                key: &str,
            ) -> Self::State {
                let value = JsValue::from(&*self);
                Rndr::set_property_or_value(el, key, &value);
                (el.clone(), value)
            }

            fn rebuild(self, state: &mut Self::State, key: &str) {
                let (el, prev) = state;
                let value = JsValue::from(&*self);
                Rndr::set_property_or_value(el, key, &value);
                *prev = value;
            }

            fn into_cloneable(self) -> Self::Cloneable {
                let this: &str = &*self;
                this.into()
            }

            fn into_cloneable_owned(self) -> Self::CloneableOwned {
                let this: &str = &*self;
                this.into()
            }
        }

        impl IntoProperty for Option<$prop_type> {
            type State = (crate::renderer::types::Element, JsValue);
            type Cloneable = Option<Arc<str>>;
            type CloneableOwned = Option<Arc<str>>;

            fn hydrate<const FROM_SERVER: bool>(
                self,
                el: &crate::renderer::types::Element,
                key: &str,
            ) -> Self::State {
                let was_some = self.is_some();
                let value = JsValue::from(self.map(|n| JsValue::from_str(&n)));
                if was_some {
                    Rndr::set_property_or_value(el, key, &value);
                }
                (el.clone(), value)
            }

            fn build(
                self,
                el: &crate::renderer::types::Element,
                key: &str,
            ) -> Self::State {
                let was_some = self.is_some();
                let value = JsValue::from(self.map(|n| JsValue::from_str(&n)));
                if was_some {
                    Rndr::set_property_or_value(el, key, &value);
                }
                (el.clone(), value)
            }

            fn rebuild(self, state: &mut Self::State, key: &str) {
                let (el, prev) = state;
                let value = JsValue::from(self.map(|n| JsValue::from_str(&n)));
                Rndr::set_property_or_value(el, key, &value);
                *prev = value;
            }

            fn into_cloneable(self) -> Self::Cloneable {
                self.map(|n| {
                    let this: &str = &*n;
                    this.into()
                })
            }

            fn into_cloneable_owned(self) -> Self::CloneableOwned {
                self.map(|n| {
                    let this: &str = &*n;
                    this.into()
                })
            }
        }
    };
}

impl IntoProperty for Arc<str> {
    type State = (crate::renderer::types::Element, JsValue);
    type Cloneable = Self;
    type CloneableOwned = Self;

    fn hydrate<const FROM_SERVER: bool>(
        self,
        el: &crate::renderer::types::Element,
        key: &str,
    ) -> Self::State {
        let value = JsValue::from_str(self.as_ref());
        Rndr::set_property_or_value(el, key, &value);
        (el.clone(), value)
    }

    fn build(
        self,
        el: &crate::renderer::types::Element,
        key: &str,
    ) -> Self::State {
        let value = JsValue::from_str(self.as_ref());
        Rndr::set_property_or_value(el, key, &value);
        (el.clone(), value)
    }

    fn rebuild(self, state: &mut Self::State, key: &str) {
        let (el, prev) = state;
        let value = JsValue::from_str(self.as_ref());
        Rndr::set_property_or_value(el, key, &value);
        *prev = value;
    }

    fn into_cloneable(self) -> Self::Cloneable {
        self
    }

    fn into_cloneable_owned(self) -> Self::CloneableOwned {
        self
    }
}

impl IntoProperty for Option<Arc<str>> {
    type State = (crate::renderer::types::Element, JsValue);
    type Cloneable = Self;
    type CloneableOwned = Self;

    fn hydrate<const FROM_SERVER: bool>(
        self,
        el: &crate::renderer::types::Element,
        key: &str,
    ) -> Self::State {
        let was_some = self.is_some();
        let value = JsValue::from(self.map(|n| JsValue::from_str(&n)));
        if was_some {
            Rndr::set_property_or_value(el, key, &value);
        }
        (el.clone(), value)
    }

    fn build(
        self,
        el: &crate::renderer::types::Element,
        key: &str,
    ) -> Self::State {
        let was_some = self.is_some();
        let value = JsValue::from(self.map(|n| JsValue::from_str(&n)));
        if was_some {
            Rndr::set_property_or_value(el, key, &value);
        }
        (el.clone(), value)
    }

    fn rebuild(self, state: &mut Self::State, key: &str) {
        let (el, prev) = state;
        let value = JsValue::from(self.map(|n| JsValue::from_str(&n)));
        Rndr::set_property_or_value(el, key, &value);
        *prev = value;
    }

    fn into_cloneable(self) -> Self::Cloneable {
        self
    }

    fn into_cloneable_owned(self) -> Self::CloneableOwned {
        self
    }
}

prop_type!(JsValue);
prop_type!(usize);
prop_type!(u8);
prop_type!(u16);
prop_type!(u32);
prop_type!(u64);
prop_type!(u128);
prop_type!(isize);
prop_type!(i8);
prop_type!(i16);
prop_type!(i32);
prop_type!(i64);
prop_type!(i128);
prop_type!(f32);
prop_type!(f64);
prop_type!(bool);

prop_type_str!(String);
prop_type_str!(&String);
prop_type_str!(&str);
prop_type_str!(Cow<'_, str>);

#[cfg(test)]
mod tests {
    use super::Property;
    use crate::{html::attribute::Attribute, renderer::types::Element};
    use wasm_bindgen::{JsCast, JsValue};

    /// A property whose value was not created (the `ssr` feature switched on in a browser
    /// build by feature unification).
    fn without_value() -> Property<&'static str, i32> {
        Property {
            key: "value",
            value: None,
        }
    }

    /// Building or hydrating an element with such a property panicked
    /// (`FEATURE_CONFLICT_DIAGNOSTIC`). The property is not set (logged once), and
    /// updating it later does nothing.
    #[test]
    fn a_property_without_its_value_is_not_set_or_updated() {
        // never touched: a native test cannot call into JavaScript
        let el: Element = JsValue::UNDEFINED.unchecked_into();

        let mut built = without_value().build(&el);
        assert!(built.is_none());
        assert!(without_value().hydrate::<true>(&el).is_none());
        assert!(without_value().hydrate::<false>(&el).is_none());

        without_value().rebuild(&mut built);
        assert!(built.is_none());
    }
}
