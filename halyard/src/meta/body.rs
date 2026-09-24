use crate::meta::{document_or_warn, error::MetaError, ServerMetaContext};
use halyard::{
    attr::{any_attribute::AnyAttribute, NextAttribute},
    component, html,
    reactive::owner::use_context,
    tachys::{
        dom::document,
        html::attribute::Attribute,
        hydration::Cursor,
        view::{
            add_attr::AddAnyAttr, Mountable, Position, PositionState, Render,
            RenderHtml,
        },
    },
    IntoView,
};

/// A component to set metadata on the document’s `<body>` element from
/// within the application.
///
/// This component takes no props, but can take any number of spread attributes
/// following the `{..}` operator.
///
/// ```
/// use halyard::prelude::*;
/// use halyard::meta::*;
///
/// #[component]
/// fn MyApp() -> impl IntoView {
///     provide_meta_context();
///     let (prefers_dark, set_prefers_dark) = signal(false);
///     // no class once the signal is gone
///     let body_class = move || {
///         prefers_dark
///             .try_get()
///             .map(|dark| if dark { "dark" } else { "light" })
///     };
///
///     view! {
///       <main>
///         <Body {..} class=body_class id="body"/>
///       </main>
///     }
/// }
/// ```
#[component]
pub fn Body() -> impl IntoView {
    BodyView { attributes: () }
}

struct BodyView<At> {
    attributes: At,
}

struct BodyViewState<At>
where
    At: Attribute,
{
    /// `None` if the document has no `<body>` element to set them on.
    attributes: Option<At::State>,
}

/// What happens when there is no `<body>` element to set the attributes on.
const NOT_APPLIED: &str = "The attributes of <Body/> are not applied.";

/// The document's `<body>` element; `None`, logged, if it has none (or there is no
/// document, logged once).
fn body_element() -> Option<web_sys::HtmlElement> {
    let el = document_or_warn(NOT_APPLIED)?.body();
    if el.is_none() {
        MetaError::NoElement("body").warn(NOT_APPLIED);
    }
    el
}

impl<At> Render for BodyView<At>
where
    At: Attribute,
{
    type State = BodyViewState<At>;

    fn build(self) -> Self::State {
        let attributes = body_element().map(|el| self.attributes.build(&el));

        BodyViewState { attributes }
    }

    fn rebuild(self, state: &mut Self::State) {
        if let Some(attributes) = &mut state.attributes {
            self.attributes.rebuild(attributes);
        }
    }
}

impl<At> AddAnyAttr for BodyView<At>
where
    At: Attribute,
{
    type Output<SomeNewAttr: Attribute> =
        BodyView<<At as NextAttribute>::Output<SomeNewAttr>>;

    fn add_any_attr<NewAttr: Attribute>(
        self,
        attr: NewAttr,
    ) -> Self::Output<NewAttr>
    where
        Self::Output<NewAttr>: RenderHtml,
    {
        BodyView {
            attributes: self.attributes.add_any_attr(attr),
        }
    }
}

impl<At> RenderHtml for BodyView<At>
where
    At: Attribute,
{
    type AsyncOutput = BodyView<At::AsyncOutput>;
    type Owned = BodyView<At::CloneableOwned>;

    const MIN_LENGTH: usize = At::MIN_LENGTH;

    fn dry_resolve(&mut self) {
        self.attributes.dry_resolve();
    }

    async fn resolve(self) -> Self::AsyncOutput {
        BodyView {
            attributes: self.attributes.resolve().await,
        }
    }

    fn to_html_with_buf(
        self,
        _buf: &mut String,
        _position: &mut Position,
        _escape: bool,
        _mark_branches: bool,
        extra_attrs: Vec<AnyAttribute>,
    ) {
        if let Some(meta) = use_context::<ServerMetaContext>() {
            let mut buf = String::new();
            _ = html::attributes_to_html(
                (self.attributes, extra_attrs),
                &mut buf,
            );
            if !buf.is_empty() {
                _ = meta.body.send(buf);
            }
        }
    }

    fn hydrate<const FROM_SERVER: bool>(
        self,
        _cursor: &Cursor,
        _position: &PositionState,
    ) -> Self::State {
        let attributes = body_element()
            .map(|el| self.attributes.hydrate::<FROM_SERVER>(&el));

        BodyViewState { attributes }
    }

    fn into_owned(self) -> Self::Owned {
        BodyView {
            attributes: self.attributes.into_cloneable_owned(),
        }
    }
}

impl<At> Mountable for BodyViewState<At>
where
    At: Attribute,
{
    fn unmount(&mut self) {}

    fn mount(
        &mut self,
        _parent: &halyard::tachys::renderer::types::Element,
        _marker: Option<&halyard::tachys::renderer::types::Node>,
    ) {
    }

    fn insert_before_this(&self, _child: &mut dyn Mountable) -> bool {
        false
    }

    fn elements(&self) -> Vec<halyard::tachys::renderer::types::Element> {
        document()
            .and_then(|document| document.body())
            .map(Into::into)
            .into_iter()
            .collect()
    }
}
