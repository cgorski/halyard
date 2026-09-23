use super::{
    add_attr::AddAnyAttr, Mountable, Position, PositionState, Render,
    RenderHtml, ToTemplate,
};
use crate::{
    html::attribute::{any_attribute::AnyAttribute, Attribute},
    hydration::Cursor,
    renderer::Rndr,
    view_error::{report_once, ViewError},
};
use std::sync::atomic::AtomicBool;

/// A view wrapper that uses a `<template>` node to optimize DOM node creation.
///
/// Rather than creating all of the DOM nodes each time it is built, this template will create a
/// single `<template>` node once, then use `.cloneNode(true)` to clone that entire tree, and
/// hydrate it to add event listeners and interactivity for this instance.
pub struct ViewTemplate<V> {
    view: V,
}

impl<V> ViewTemplate<V>
where
    V: Render + ToTemplate + 'static,
{
    /// Creates a new view template.
    pub fn new(view: V) -> Self {
        Self { view }
    }

    fn to_template() -> crate::renderer::types::TemplateElement {
        Rndr::get_template::<V>()
    }
}

impl<V> Render for ViewTemplate<V>
where
    V: Render + RenderHtml + ToTemplate + 'static,
    V::State: Mountable,
{
    type State = V::State;

    // TODO try_build/try_rebuild()

    fn build(self) -> Self::State {
        let tpl = Self::to_template();
        let contents = Rndr::clone_template(&tpl);
        self.view
            .hydrate::<false>(&Cursor::new(contents), &Default::default())
    }

    fn rebuild(self, state: &mut Self::State) {
        self.view.rebuild(state)
    }
}

impl<V> AddAnyAttr for ViewTemplate<V>
where
    V: RenderHtml + ToTemplate + 'static,
    V::State: Mountable,
{
    type Output<SomeNewAttr: Attribute> = ViewTemplate<V>;

    // the template's markup is fixed at compile time, and `Output` is this type
    fn add_any_attr<NewAttr: Attribute>(
        self,
        _attr: NewAttr,
    ) -> Self::Output<NewAttr> {
        static REPORTED: AtomicBool = AtomicBool::new(false);
        report_once(
            &REPORTED,
            &ViewError::AttributesIgnored {
                what: "a ViewTemplate",
            },
        );
        self
    }
}

impl<V> RenderHtml for ViewTemplate<V>
where
    V: RenderHtml + ToTemplate + 'static,
    V::State: Mountable,
{
    type AsyncOutput = V::AsyncOutput;
    type Owned = V::Owned;

    const MIN_LENGTH: usize = V::MIN_LENGTH;

    fn to_html_with_buf(
        self,
        buf: &mut String,
        position: &mut Position,
        escape: bool,
        mark_branches: bool,
        extra_attrs: Vec<AnyAttribute>,
    ) {
        self.view.to_html_with_buf(
            buf,
            position,
            escape,
            mark_branches,
            extra_attrs,
        )
    }

    fn hydrate<const FROM_SERVER: bool>(
        self,
        cursor: &Cursor,
        position: &PositionState,
    ) -> Self::State {
        self.view.hydrate::<FROM_SERVER>(cursor, position)
    }

    fn dry_resolve(&mut self) {
        self.view.dry_resolve();
    }

    async fn resolve(self) -> Self::AsyncOutput {
        self.view.resolve().await
    }

    fn into_owned(self) -> Self::Owned {
        self.view.into_owned()
    }
}

impl<V> ToTemplate for ViewTemplate<V>
where
    V: RenderHtml + ToTemplate + 'static,
    V::State: Mountable,
{
    const TEMPLATE: &'static str = V::TEMPLATE;

    fn to_template(
        buf: &mut String,
        class: &mut String,
        style: &mut String,
        inner_html: &mut String,
        position: &mut Position,
    ) {
        V::to_template(buf, class, style, inner_html, position);
    }
}

#[cfg(test)]
mod tests {
    use super::ViewTemplate;
    use crate::{
        html::attribute::id,
        view::{add_attr::AddAnyAttr, RenderHtml},
    };

    /// Spreading attributes onto a `ViewTemplate` (as onto a component that returns
    /// `template! { ... }`) panicked, on the server too. They are ignored.
    #[test]
    fn view_template_ignores_spread_attributes() {
        let view = ViewTemplate::new("hello").add_any_attr(id("main"));
        assert_eq!(view.to_html(), "hello");
    }
}
