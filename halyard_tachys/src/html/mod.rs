use self::attribute::Attribute;
use crate::{
    hydration::{failed_to_cast_element, Cursor},
    no_attrs,
    prelude::{AddAnyAttr, Mountable},
    renderer::{
        dom::{Element, Node},
        CastFrom, Rndr,
    },
    view::{Position, PositionState, Render, RenderHtml},
    view_error::{report_once, ViewError},
};
use attribute::any_attribute::AnyAttribute;
use std::{borrow::Cow, sync::atomic::AtomicBool};

/// Diagnostic for a client-side value (event handler, directive, property) that was not
/// created.
///
/// When the `ssr` feature is active, tachys skips creating client-side values
/// (event handlers, directives, properties) to avoid `SendWrapper` cross-thread
/// panics on multithreaded servers. If one is missing in the browser, the `ssr`
/// feature was activated unintentionally via Cargo feature unification in a
/// client-side (hydrate) build.
pub(crate) const FEATURE_CONFLICT_DIAGNOSTIC: &str =
    "Value is None because the `ssr` feature is active. When `ssr` is \
     enabled, tachys skips creating client-side values (event handlers, \
     directives, properties) to avoid cross-thread panics on multithreaded \
     servers. If you are building the client-side (hydrate) target, this \
     means the `ssr` feature is being activated unintentionally via Cargo \
     feature unification; another dependency in your workspace is enabling \
     it. Run `cargo tree -e features -i tachys` to identify the source.";

/// Types for HTML attributes.
pub mod attribute;
/// Types for manipulating the `class` attribute and `classList`.
pub mod class;
/// Types for creating user-defined attributes with custom behavior (directives).
pub mod directive;
/// Types for HTML elements.
pub mod element;
/// Types for DOM events.
pub mod event;
/// Types for adding interactive islands to inert HTML pages.
pub mod islands;
/// Types for accessing a reference to an HTML element.
pub mod node_ref;
/// Types for DOM properties.
pub mod property;
/// Types for the `style` attribute and individual style manipulation.
pub mod style;

/// A `<!DOCTYPE>` declaration.
pub struct Doctype {
    value: &'static str,
}

/// Creates a `<!DOCTYPE>`.
pub fn doctype(value: &'static str) -> Doctype {
    Doctype { value }
}

impl Render for Doctype {
    type State = ();

    fn build(self) -> Self::State {}

    fn rebuild(self, _state: &mut Self::State) {}
}

no_attrs!(Doctype);

impl RenderHtml for Doctype {
    type AsyncOutput = Self;
    type Owned = Self;

    const MIN_LENGTH: usize = "<!DOCTYPE html>".len();

    fn dry_resolve(&mut self) {}

    async fn resolve(self) -> Self::AsyncOutput {
        self
    }

    fn to_html_with_buf(
        self,
        buf: &mut String,
        _position: &mut Position,
        _escape: bool,
        _mark_branches: bool,
        _extra_attrs: Vec<AnyAttribute>,
    ) {
        buf.push_str("<!DOCTYPE ");
        buf.push_str(self.value);
        buf.push('>');
    }

    fn hydrate<const FROM_SERVER: bool>(
        self,
        _cursor: &Cursor,
        _position: &PositionState,
    ) -> Self::State {
    }

    fn into_owned(self) -> Self::Owned {
        self
    }
}

/// An element that contains no interactivity, and whose contents can be known at compile time.
pub struct InertElement {
    html: Cow<'static, str>,
}

impl InertElement {
    /// Creates a new inert element.
    pub fn new(html: impl Into<Cow<'static, str>>) -> Self {
        Self { html: html.into() }
    }
}

/// Retained view state for [`InertElement`].
pub struct InertElementState(Cow<'static, str>, Element);

impl Mountable for InertElementState {
    fn unmount(&mut self) {
        self.1.unmount();
    }

    fn mount(&mut self, parent: &Element, marker: Option<&Node>) {
        self.1.mount(parent, marker)
    }

    fn insert_before_this(&self, child: &mut dyn Mountable) -> bool {
        self.1.insert_before_this(child)
    }

    fn elements(&self) -> Vec<crate::renderer::types::Element> {
        vec![self.1.clone()]
    }
}

impl Render for InertElement {
    type State = InertElementState;

    fn build(self) -> Self::State {
        let el = Rndr::create_element_from_html(self.html.clone());
        InertElementState(self.html, el)
    }

    fn rebuild(self, state: &mut Self::State) {
        let InertElementState(prev, el) = state;
        if &self.html != prev {
            let mut new_el = Rndr::create_element_from_html(self.html.clone());
            el.insert_before_this(&mut new_el);
            el.unmount();
            *el = new_el;
            *prev = self.html;
        }
    }
}

impl AddAnyAttr for InertElement {
    type Output<SomeNewAttr: Attribute> = Self;

    // an inert element should only be used as a child, not returned at the top level of a
    // component that attributes can be spread onto
    fn add_any_attr<NewAttr: Attribute>(
        self,
        _attr: NewAttr,
    ) -> Self::Output<NewAttr>
    where
        Self::Output<NewAttr>: RenderHtml,
    {
        static REPORTED: AtomicBool = AtomicBool::new(false);
        report_once(
            &REPORTED,
            &ViewError::AttributesIgnored {
                what: "an InertElement",
            },
        );
        self
    }
}

impl RenderHtml for InertElement {
    type AsyncOutput = Self;
    type Owned = Self;

    const MIN_LENGTH: usize = 0;

    fn html_len(&self) -> usize {
        self.html.len()
    }

    fn dry_resolve(&mut self) {}

    async fn resolve(self) -> Self {
        self
    }

    fn to_html_with_buf(
        self,
        buf: &mut String,
        position: &mut Position,
        _escape: bool,
        _mark_branches: bool,
        _extra_attrs: Vec<AnyAttribute>,
    ) {
        buf.push_str(&self.html);
        *position = Position::NextChild;
    }

    fn hydrate<const FROM_SERVER: bool>(
        self,
        cursor: &Cursor,
        position: &PositionState,
    ) -> Self::State {
        let curr_position = position.get();
        if curr_position == Position::FirstChild {
            cursor.child();
        } else if curr_position != Position::Current {
            cursor.sibling();
        }
        let el = crate::renderer::types::Element::cast_from(cursor.current())
            .unwrap_or_else(|| {
                failed_to_cast_element(
                    first_tag_name(&self.html),
                    cursor.current(),
                )
            });
        position.set(Position::NextChild);
        InertElementState(self.html, el)
    }

    fn into_owned(self) -> Self::Owned {
        self
    }
}

/// The tag name of the first element in an inert element's `html`, for the hydration
/// mismatch report and the element created in its place; `template` if `html` does not
/// start with a tag.
pub(crate) fn first_tag_name(html: &str) -> &str {
    let after_bracket = html.trim_start().strip_prefix('<').unwrap_or_default();
    let name = after_bracket
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .next()
        .unwrap_or_default();
    if name.starts_with(|c: char| c.is_ascii_alphabetic()) {
        name
    } else {
        "template"
    }
}

#[cfg(test)]
mod tests {
    use super::{first_tag_name, InertElement};
    use crate::{
        html::attribute::id,
        view::{add_attr::AddAnyAttr, RenderHtml},
    };

    /// Spreading attributes onto an `InertElement` (as onto a component that returns one)
    /// panicked, on the server too. They are ignored.
    #[test]
    fn inert_element_ignores_spread_attributes() {
        let el = InertElement::new("<p>inert</p>").add_any_attr(id("main"));
        assert_eq!(el.to_html(), "<p>inert</p>");
    }

    #[test]
    fn first_tag_name_is_the_name_of_the_first_tag() {
        assert_eq!(first_tag_name("<p class=\"x\">inert</p>"), "p");
        assert_eq!(first_tag_name("\n  <my-element/>"), "my-element");
        assert_eq!(first_tag_name("<circle r=\"1\"/>"), "circle");
        assert_eq!(first_tag_name("text"), "template");
        assert_eq!(first_tag_name("<>"), "template");
        assert_eq!(first_tag_name("<!-- comment -->"), "template");
        assert_eq!(first_tag_name(""), "template");
    }
}
