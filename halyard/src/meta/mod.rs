#![deny(missing_docs)]
#![forbid(unsafe_code)]

//! # The document's head
//!
//! `halyard::meta` allows you to modify content in a document’s `<head>` from within
//! components.
//!
//! Document metadata is updated automatically when running in the browser. For server-side
//! rendering, after the component tree is rendered to HTML, [`ServerMetaContextOutput::inject_meta_context`] will inject meta tags into a stream of HTML inside the `<head>`.
//!
//! ```
//! use halyard::prelude::*;
//! use halyard::meta::*;
//!
//! #[component]
//! fn MyApp() -> impl IntoView {
//!     // Provides a [`MetaContext`], if there is not already one provided.
//!     provide_meta_context();
//!
//!     let (name, set_name) = create_signal("Alice".to_string());
//!
//!     view! {
//!       <Title
//!         // reactively sets document.title when `name` changes
//!         text=name
//!         // applies the `formatter` function to the `text` value
//!         formatter=|text| format!("“{text}” is your name")
//!       />
//!       <main>
//!         <input
//!           prop:value=name
//!           on:input=move |ev| set_name.set(event_target_value(&ev))
//!         />
//!       </main>
//!     }
//! }
//! ```
//!
//! Its server side (collecting the tags and injecting them into the `<head>`) is compiled
//! with `halyard`'s `ssr` feature.

use error::MetaError;
use futures::{Stream, StreamExt};
use halyard::{
    attr::{any_attribute::AnyAttribute, NextAttribute},
    component,
    logging::debug_warn,
    nonce::use_nonce,
    oco::Oco,
    reactive::owner::{provide_context, use_context},
    tachys::{
        dom::document,
        html::{
            attribute::Attribute,
            element::{ElementType, HtmlElement},
        },
        hydration::Cursor,
        view::{
            add_attr::AddAnyAttr, Mountable, Position, PositionState, Render,
            RenderHtml,
        },
    },
    IntoView,
};
use inject::HeadPlacement;
use send_wrapper::SendWrapper;
use std::{
    fmt::Debug,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{channel, Receiver, Sender},
        Arc, LazyLock,
    },
};
use wasm_bindgen::JsCast;
use web_sys::HtmlHeadElement;

mod body;
mod error;
mod html;
mod inject;
mod link;
mod meta_tags;
mod script;
mod style;
mod stylesheet;
mod title;
pub use body::*;
pub use html::*;
pub use link::*;
pub use meta_tags::*;
pub use script::*;
pub use style::*;
pub use stylesheet::*;
pub use title::*;

/// Contains the current state of meta tags. To access it, you can use [`use_head`].
///
/// This should generally by provided somewhere in the root of your application using
/// [`provide_meta_context`].
#[derive(Clone, Debug)]
pub struct MetaContext {
    /// Metadata associated with the `<title>` element.
    pub(crate) title: TitleContext,
    /// The hydration cursor for the location in the `<head>` for arbitrary tags will be rendered.
    ///
    /// `None` if the document has no `<head>`, or no `<!--HEAD-->` marker in it (logged
    /// when first needed): the tags are then not hydrated (see [`RegisteredMetaTag`]).
    pub(crate) cursor: Arc<LazyLock<Option<SendWrapper<Cursor>>>>,
}

impl MetaContext {
    /// Creates an empty [`MetaContext`].
    pub fn new() -> Self {
        Default::default()
    }
}

pub(crate) const HEAD_MARKER_COMMENT: &str = "HEAD";
/// What happens to a server-rendered tag that cannot be hydrated (see
/// [`RegisteredMetaTag`]'s `hydrate`).
const NOT_HYDRATED: &str =
    "The tags that halyard::meta rendered on the server stay as they are, but are \
     not updated.";
/// Return value of [`Node::node_type`] for a comment.
/// https://developer.mozilla.org/en-US/docs/Web/API/Node/nodeType#node.comment_node
const COMMENT_NODE: u16 = 8;

/// The document; `None` outside a browser's main thread, logged once with what is done
/// instead (`recovery`).
pub(crate) fn document_or_warn(recovery: &str) -> Option<web_sys::Document> {
    static WARNED: AtomicBool = AtomicBool::new(false);
    let document = document();
    if document.is_none() && !WARNED.swap(true, Ordering::Relaxed) {
        MetaError::NoDocument.warn(recovery);
    }
    document
}

impl Default for MetaContext {
    fn default() -> Self {
        let build_cursor: fn() -> Option<SendWrapper<Cursor>> = || {
            let Some(head) = document_or_warn(NOT_HYDRATED)?.head() else {
                MetaError::NoElement("head").warn(NOT_HYDRATED);
                return None;
            };
            let mut child = head.first_child();
            while let Some(this_child) = child {
                if this_child.node_type() == COMMENT_NODE
                    && this_child.text_content().as_deref()
                        == Some(HEAD_MARKER_COMMENT)
                {
                    return Some(SendWrapper::new(Cursor::new(
                        this_child.unchecked_into(),
                    )));
                }
                child = this_child.next_sibling();
            }
            MetaError::NoHeadMarker.warn(NOT_HYDRATED);
            None
        };

        let cursor = Arc::new(LazyLock::new(build_cursor));
        Self {
            title: Default::default(),
            cursor,
        }
    }
}

/// Allows you to add `<head>` content from components located in the `<body>` of the application,
/// which can be accessed during server rendering via [`ServerMetaContextOutput`].
///
/// This should be provided as context during server rendering.
///
/// No content added after the first chunk of the stream has been sent will be included in the
/// initial `<head>`. Data that needs to be included in the `<head>` during SSR should be
/// synchronous or loaded as a blocking resource.
#[derive(Clone, Debug)]
pub struct ServerMetaContext {
    /// Metadata associated with the `<title>` element.
    pub(crate) title: TitleContext,
    /// Attributes for the `<html>` element.
    pub(crate) html: Sender<String>,
    /// Attributes for the `<body>` element.
    pub(crate) body: Sender<String>,
    /// Arbitrary elements to be added to the `<head>` as HTML.
    #[allow(unused)] // used in SSR
    pub(crate) elements: Sender<String>,
}

/// Allows you to access `<head>` content that was inserted via [`ServerMetaContext`].
#[must_use = "If you do not use the output, adding meta tags will have no \
              effect."]
#[derive(Debug)]
pub struct ServerMetaContextOutput {
    pub(crate) title: TitleContext,
    html: Receiver<String>,
    body: Receiver<String>,
    elements: Receiver<String>,
}

impl ServerMetaContext {
    /// Creates an empty [`ServerMetaContext`].
    pub fn new() -> (ServerMetaContext, ServerMetaContextOutput) {
        let title = TitleContext::default();
        let (html_tx, html_rx) = channel();
        let (body_tx, body_rx) = channel();
        let (elements_tx, elements_rx) = channel();
        let tx = ServerMetaContext {
            title: title.clone(),
            html: html_tx,
            body: body_tx,
            elements: elements_tx,
        };
        let rx = ServerMetaContextOutput {
            title,
            html: html_rx,
            body: body_rx,
            elements: elements_rx,
        };
        (tx, rx)
    }
}

impl ServerMetaContextOutput {
    /// Consumes the metadata, injecting it into the the first chunk of an HTML stream in the
    /// appropriate place.
    ///
    /// This means that only meta tags rendered during the first chunk of the stream will be
    /// included.
    ///
    /// The title and meta tags go after the `<!--HEAD-->` marker that [`MetaTags`]
    /// renders, or else before `</head>`. A first chunk with neither (a shell without a
    /// `<head>`, or a first chunk that ends inside it) gets them before `<body>`, or else at
    /// the start of the document, after its doctype; the browser puts them in the head from
    /// there too. That is logged, as are `<Html>`/`<Body>` attributes without an
    /// `<html>`/`<body>` tag in the first chunk, which are left out.
    pub async fn inject_meta_context(
        self,
        mut stream: impl Stream<Item = String> + Send + Unpin,
    ) -> impl Stream<Item = String> + Send {
        // if the first chunk consists of a synchronously-available Suspend,
        // inject_meta_context can accidentally run a tick before it, but the Suspend
        // when both are available. waiting a tick before awaiting the first chunk
        // in the Stream ensures that this always runs after that first chunk
        // see https://github.com/leptos-rs/leptos/issues/3976 for the original issue
        halyard::task::tick().await;

        // wait for the first chunk of the stream, to ensure our components hve run
        let first_chunk = stream.next().await.unwrap_or_default();
        let modified_chunk = self.inject_into_first_chunk(first_chunk);

        futures::stream::once(async move { modified_chunk }).chain(stream)
    }

    /// Puts the meta tags, title and `<html>`/`<body>` attributes registered so far into
    /// `first_chunk`, the start of the page (see [`Self::inject_meta_context`]).
    fn inject_into_first_chunk(self, first_chunk: String) -> String {
        // all registered meta tags, then the <title>
        let mut head = self.elements.try_iter().collect::<String>();
        if let Some(title) = self.title.as_string() {
            // The title is text: escaped like any other text node, so a title
            // holding `</title><script>` (a committee or contact name, say) stays
            // text and cannot end the element early.
            head.push_str("<title>");
            head.push_str(&html_escape::encode_text(&title));
            head.push_str("</title>");
        }

        let mut page = first_chunk;
        if !head.is_empty() {
            let (with_head, placement) =
                inject::insert_head_content(&page, &head);
            let recovery = match placement {
                HeadPlacement::AfterMarker | HeadPlacement::BeforeHeadEnd => None,
                HeadPlacement::BeforeBody => Some(
                    "The title and meta tags were put before <body>, where the \
                     browser puts them in the document's head.",
                ),
                HeadPlacement::DocumentStart => Some(
                    "The title and meta tags were put at the start of the \
                     document, where the browser puts them in its head.",
                ),
            };
            if let Some(recovery) = recovery {
                MetaError::NoHeadInFirstChunk.warn(recovery);
            }
            page = with_head;
        }

        let html_attrs = self.html.try_iter().collect::<String>();
        let page = with_attributes(page, "html", "Html", &html_attrs);
        let body_attrs = self.body.try_iter().collect::<String>();
        with_attributes(page, "body", "Body", &body_attrs)
    }
}

/// `page` with `attributes` (from `<component/>`) on its first `<tag>`; unchanged, and
/// logged, if the page has no such tag.
fn with_attributes(
    page: String,
    tag: &'static str,
    component: &'static str,
    attributes: &str,
) -> String {
    if attributes.is_empty() {
        return page;
    }
    match inject::insert_attributes(&page, tag, attributes) {
        Some(page) => page,
        None => {
            MetaError::NoTagInFirstChunk { tag, component }
                .warn("Its attributes are left out of the page.");
            page
        }
    }
}

/// Provides a [`MetaContext`], if there is not already one provided. This ensures that you can provide it
/// at the highest possible level, without overwriting a [`MetaContext`] that has already been provided
/// (for example, by a server-rendering integration.)
pub fn provide_meta_context() {
    if use_context::<MetaContext>().is_none() {
        provide_context(MetaContext::new());
    }
}

/// Returns the current [`MetaContext`].
///
/// If there is no [`MetaContext`] in this or any parent scope, this will
/// create a new [`MetaContext`] and provide it to the current scope.
///
/// Note that this may cause confusing behavior, e.g., if multiple nested routes independently
/// call `use_head()` but a single [`MetaContext`] has not been provided at the application root.
/// The best practice is always to call [`provide_meta_context`] early in the application.
pub fn use_head() -> MetaContext {
    match use_context::<MetaContext>() {
        None => {
            debug_warn!(
                "use_head() is being called without a MetaContext being \
                 provided. We'll automatically create and provide one, but if \
                 this is being called in a child route it may cause bugs. To \
                 be safe, you should provide_meta_context() somewhere in the \
                 root of the app."
            );
            let meta = MetaContext::new();
            provide_context(meta.clone());
            meta
        }
        Some(ctx) => ctx,
    }
}

fn register<E, At, Ch>(
    el: HtmlElement<E, At, Ch>,
) -> RegisteredMetaTag<E, At, Ch>
where
    HtmlElement<E, At, Ch>: RenderHtml,
{
    RegisteredMetaTag { el }
}

struct RegisteredMetaTag<E, At, Ch> {
    // this is `None` if we've already taken it out to render to HTML on the server
    // we don't render it in place in RenderHtml, so it's fine
    el: HtmlElement<E, At, Ch>,
}

struct RegisteredMetaTagState<E, At, Ch>
where
    HtmlElement<E, At, Ch>: Render,
{
    state: <HtmlElement<E, At, Ch> as Render>::State,
}

impl<E, At, Ch> Drop for RegisteredMetaTagState<E, At, Ch>
where
    HtmlElement<E, At, Ch>: Render,
{
    fn drop(&mut self) {
        self.state.unmount();
    }
}

/// The document's `<head>`, created (at the end of `<html>`) if the document has none.
fn document_head(
    document: &web_sys::Document,
) -> Result<HtmlHeadElement, MetaError> {
    if let Some(head) = document.head() {
        return Ok(head);
    }
    let html = document
        .document_element()
        .ok_or(MetaError::NoElement("html"))?;
    let head = document.create_element("head").map_err(|thrown| {
        MetaError::thrown("document.createElement", &thrown)
    })?;
    html.append_child(&head)
        .map_err(|thrown| MetaError::thrown("html.appendChild", &thrown))?;
    Ok(head.unchecked_into())
}

/// The hydration cursor in the `<head>` of the current [`MetaContext`], which the server
/// put this page's tags after. `None` if there is no context (logged once), or no cursor
/// (logged when the context looked for it).
fn head_cursor() -> Option<Cursor> {
    static WARNED_NO_META_CONTEXT: AtomicBool = AtomicBool::new(false);

    let Some(meta) = use_context::<MetaContext>() else {
        if !WARNED_NO_META_CONTEXT.swap(true, Ordering::Relaxed) {
            MetaError::NoMetaContext.warn(NOT_HYDRATED);
        }
        return None;
    };
    let cursor = LazyLock::force(&meta.cursor).as_ref()?;
    Some(Cursor::clone(cursor))
}

impl<E, At, Ch> Render for RegisteredMetaTag<E, At, Ch>
where
    E: ElementType,
    At: Attribute,
    Ch: Render,
{
    type State = RegisteredMetaTagState<E, At, Ch>;

    fn build(self) -> Self::State {
        let state = self.el.build();
        RegisteredMetaTagState { state }
    }

    fn rebuild(self, state: &mut Self::State) {
        self.el.rebuild(&mut state.state);
    }
}

impl<E, At, Ch> AddAnyAttr for RegisteredMetaTag<E, At, Ch>
where
    E: ElementType + Send,
    At: Attribute + Send,
    Ch: RenderHtml + Send,
{
    type Output<SomeNewAttr: Attribute> =
        RegisteredMetaTag<E, <At as NextAttribute>::Output<SomeNewAttr>, Ch>;

    fn add_any_attr<NewAttr: Attribute>(
        self,
        attr: NewAttr,
    ) -> Self::Output<NewAttr>
    where
        Self::Output<NewAttr>: RenderHtml,
    {
        RegisteredMetaTag {
            el: self.el.add_any_attr(attr),
        }
    }
}

impl<E, At, Ch> RenderHtml for RegisteredMetaTag<E, At, Ch>
where
    E: ElementType,
    At: Attribute,
    Ch: RenderHtml + Send,
{
    type AsyncOutput = Self;
    type Owned = RegisteredMetaTag<E, At::CloneableOwned, Ch::Owned>;

    const MIN_LENGTH: usize = 0;
    const EXISTS: bool = false;

    fn dry_resolve(&mut self) {
        self.el.dry_resolve()
    }

    async fn resolve(self) -> Self::AsyncOutput {
        self // TODO?
    }

    fn to_html_with_buf(
        self,
        _buf: &mut String,
        _position: &mut Position,
        _escape: bool,
        _mark_branches: bool,
        _extra_attrs: Vec<AnyAttribute>,
    ) {
        // meta tags are rendered into the buffer stored into the context
        // the value has already been taken out, when we're on the server
        #[cfg(feature = "ssr")]
        if let Some(cx) = use_context::<ServerMetaContext>() {
            let mut buf = String::new();
            self.el.to_html_with_buf(
                &mut buf,
                &mut Position::NextChild,
                false,
                false,
                vec![],
            );
            _ = cx.elements.send(buf); // fails only if the receiver is already dropped
        } else {
            let msg = "tried to use a halyard::meta component without \
                       `ServerMetaContext` provided";

            #[cfg(feature = "tracing")]
            tracing::warn!("{}", msg);

            #[cfg(not(feature = "tracing"))]
            eprintln!("{msg}");
        }
    }

    fn hydrate<const FROM_SERVER: bool>(
        self,
        _cursor: &Cursor,
        _position: &PositionState,
    ) -> Self::State {
        let Some(cursor) = head_cursor() else {
            // Nowhere to hydrate from: the tag is created on the client but not added, and
            // the server's copy of it stays in the page as rendered. Adding this one would
            // duplicate that copy (and run a `<Script>` twice). Should it be moved later,
            // `mount` adds it to the `<head>`.
            let state = self.el.build();
            return RegisteredMetaTagState { state };
        };
        let state = self.el.hydrate::<FROM_SERVER>(
            &cursor,
            &PositionState::new(Position::NextChild),
        );
        RegisteredMetaTagState { state }
    }

    fn into_owned(self) -> Self::Owned {
        RegisteredMetaTag {
            el: self.el.into_owned(),
        }
    }
}

impl<E, At, Ch> Mountable for RegisteredMetaTagState<E, At, Ch>
where
    E: ElementType,
    At: Attribute,
    Ch: Render,
{
    fn unmount(&mut self) {
        self.state.unmount();
    }

    fn mount(
        &mut self,
        _parent: &halyard::tachys::renderer::types::Element,
        _marker: Option<&halyard::tachys::renderer::types::Node>,
    ) {
        // we always mount this to the <head>, which is the whole point
        // but this shouldn't warn about the parent being a regular element or being unused
        // because it will call "mount" with the parent where it is located in the component tree,
        // but actually be mounted to the <head>
        const NOT_ADDED: &str = "The tag is not added to the page.";
        let Some(document) = document_or_warn(NOT_ADDED) else {
            return;
        };
        match document_head(&document) {
            Ok(head) => self.state.mount(&head, None),
            Err(error) => error.warn(NOT_ADDED),
        }
    }

    fn insert_before_this(&self, _child: &mut dyn Mountable) -> bool {
        // Registered meta tags will be mounted in the <head>, but *seem* to be mounted somewhere
        // else in the DOM. We should never tell the renderer that we have successfully mounted
        // something before this, because if e.g., a <Meta/> is the first item in an Either, then
        // the alternate view will end up being mounted in the <head> -- which is not at all what
        // we intended!
        false
    }

    fn elements(&self) -> Vec<halyard::tachys::renderer::types::Element> {
        self.state.elements()
    }
}

/// During server rendering, inserts the meta tags that have been generated by the other components
/// in this module into the DOM. This should be placed somewhere inside the `<head>` element that is
/// being used during server rendering.
#[component]
pub fn MetaTags() -> impl IntoView {
    MetaTagsView
}

#[derive(Debug)]
struct MetaTagsView;

// this implementation doesn't do anything during client-side rendering, it's just for server-side
// rendering HTML for all the tags that will be injected into the `<head>`
//
// client-side rendering is handled by the individual components
impl Render for MetaTagsView {
    type State = ();

    fn build(self) -> Self::State {}

    fn rebuild(self, _state: &mut Self::State) {}
}

impl AddAnyAttr for MetaTagsView {
    type Output<SomeNewAttr: Attribute> = MetaTagsView;

    fn add_any_attr<NewAttr: Attribute>(
        self,
        _attr: NewAttr,
    ) -> Self::Output<NewAttr>
    where
        Self::Output<NewAttr>: RenderHtml,
    {
        self
    }
}

impl RenderHtml for MetaTagsView {
    type AsyncOutput = Self;
    type Owned = Self;

    const MIN_LENGTH: usize = 0;

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
        buf.push_str("<!--HEAD-->");
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

pub(crate) trait OrDefaultNonce {
    fn or_default_nonce(self) -> Option<Oco<'static, str>>;
}

impl OrDefaultNonce for Option<Oco<'static, str>> {
    fn or_default_nonce(self) -> Option<Oco<'static, str>> {
        match self {
            Some(nonce) => Some(nonce),
            None => use_nonce().map(|n| Arc::clone(n.as_inner()).into()),
        }
    }
}

/// Server rendering: the `<head>` content that the page's components register goes into
/// the first chunk of the page. The integrations call `inject_meta_context` for every page;
/// these tests call the step after its `.await`s, which needs no async executor.
///
/// The title comes from rendering `<Title>`. Rendering elements to HTML needs the `ssr`
/// feature, and these tests run in every build of the crate, so the first chunks are
/// written out, and `<Meta>`, `<Html>` and `<Body>` content is sent to the context as those
/// components send it.
#[cfg(test)]
mod tests {
    use super::*;
    use halyard::{prelude::*, reactive::owner::Owner};

    /// Renders `<Title text=text/>` with a [`ServerMetaContext`], as the integrations do,
    /// and returns the context (to register more `<head>` content with) and the output
    /// that injects it.
    fn render_title(
        text: &'static str,
    ) -> (ServerMetaContext, ServerMetaContextOutput) {
        let (meta_context, output) = ServerMetaContext::new();
        let html = Owner::new().with(|| {
            provide_context(meta_context.clone());
            provide_meta_context();
            view! { <Title text=text /> }.to_html()
        });
        assert_eq!(html, "", "<Title> renders into the context, not in place");
        (meta_context, output)
    }

    fn inject_title(text: &'static str, first_chunk: &str) -> String {
        render_title(text)
            .1
            .inject_into_first_chunk(first_chunk.to_owned())
    }

    /// A shell without `<head>`, which is valid HTML: `.expect("you are using halyard::meta
    /// without a </head> tag")` failed the request. The title goes before `<body>`, where
    /// the browser's parser puts it in the document's head.
    #[test]
    fn a_shell_without_head_gets_its_title_before_body() {
        let page = inject_title(
            "Reports",
            "<!DOCTYPE html><html lang=\"en\"><body><main>content</main>\
             </body></html>",
        );

        assert_eq!(
            page,
            "<!DOCTYPE html><html lang=\"en\"><title>Reports</title><body>\
             <main>content</main></body></html>"
        );
    }

    /// `<Meta>` (like `<Link>`, `<Script>`, `<Style>` and `<Stylesheet>`) goes where the
    /// title goes, just before it.
    #[test]
    fn a_shell_without_head_gets_its_meta_tags_before_body() {
        let (meta_context, output) = render_title("Reports");
        _ = meta_context
            .elements
            .send("<meta name=\"description\" content=\"FEC\">".to_owned());

        let page = output.inject_into_first_chunk(
            "<html><body><main>content</main></body></html>".to_owned(),
        );

        assert_eq!(
            page,
            "<html><meta name=\"description\" content=\"FEC\"><title>Reports\
             </title><body><main>content</main></body></html>"
        );
    }

    /// The first chunk ends inside the `<head>` (something async in the head), after the
    /// `<!--HEAD-->` marker of `<MetaTags/>` but before `</head>`. The marker is where the
    /// content goes, but a `</head>` was required as well, and failed the request.
    #[test]
    fn a_first_chunk_that_ends_inside_the_head_gets_its_title_after_the_marker()
    {
        let page = inject_title(
            "Reports",
            "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><!--HEAD-->\
             <link rel=\"modulepreload\" href=\"/pkg/app.js\">",
        );

        assert_eq!(
            page,
            "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><!--HEAD-->\
             <title>Reports</title><link rel=\"modulepreload\" \
             href=\"/pkg/app.js\">"
        );
    }

    /// A page that is not a whole document (no `<head>`, no `<body>`): the title goes at
    /// the start, after the doctype if there is one (before it, the browser would render
    /// the page in quirks mode). The parser puts a `<title>` at the start of a document in
    /// its head.
    #[test]
    fn a_page_without_head_or_body_gets_its_title_at_the_start() {
        assert_eq!(
            inject_title("Reports", "<!DOCTYPE html><main>content</main>"),
            "<!DOCTYPE html><title>Reports</title><main>content</main>"
        );
        assert_eq!(
            inject_title("Reports", "<!doctype html>\n<main>content</main>"),
            "<!doctype html><title>Reports</title>\n<main>content</main>"
        );
        assert_eq!(
            inject_title("Reports", "<main>content</main>"),
            "<title>Reports</title><main>content</main>"
        );
    }

    /// An empty first chunk (the stream ended at once) gets the title and nothing else.
    #[test]
    fn an_empty_first_chunk_gets_just_the_title() {
        assert_eq!(inject_title("Reports", ""), "<title>Reports</title>");
    }

    /// A title, or a tag, containing non-ASCII text is inserted whole.
    #[test]
    fn non_ascii_head_content_is_inserted_whole() {
        assert_eq!(
            inject_title("Rapports – été", "<html><body>é</body></html>"),
            "<html><title>Rapports – été</title><body>é</body></html>"
        );
    }

    /// What every page of the application gets: the title after the `<!--HEAD-->` marker,
    /// and the `<Html>` and `<Body>` attributes on their tags. Pinned exactly.
    #[test]
    fn a_shell_with_the_marker_gets_its_title_after_it_and_its_attributes() {
        let (meta_context, output) = render_title("Reports");
        _ = meta_context.html.send(" data-theme=\"dark\"".to_owned());
        _ = meta_context.body.send(" class=\"app\"".to_owned());

        let page = output.inject_into_first_chunk(
            "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\">\
             <!--HEAD--></head><body><main>content</main></body></html>"
                .to_owned(),
        );

        assert_eq!(
            page,
            "<!DOCTYPE html><html data-theme=\"dark\" lang=\"en\"><head>\
             <meta charset=\"utf-8\"><!--HEAD--><title>Reports</title></head>\
             <body class=\"app\"><main>content</main></body></html>"
        );
    }

    /// A title is text: markup in it is escaped, so it cannot end `<title>` and inject
    /// a script (the browser sets `document.title` as text, which is already safe).
    #[test]
    fn a_title_with_markup_stays_text() {
        let page = inject_title(
            "A & B </title><script>alert(1)</script>",
            "<html><head><!--HEAD--></head><body></body></html>",
        );

        assert_eq!(
            page,
            "<html><head><!--HEAD--><title>A &amp; B &lt;/title&gt;&lt;script&gt;\
             alert(1)&lt;/script&gt;</title></head><body></body></html>"
        );
    }

    /// Without `<MetaTags/>`, the title goes before `</head>`.
    #[test]
    fn a_shell_without_the_marker_gets_its_title_before_head_end() {
        let page = inject_title(
            "Reports",
            "<html><head><meta charset=\"utf-8\"></head><body></body></html>",
        );

        assert_eq!(
            page,
            "<html><head><meta charset=\"utf-8\"><title>Reports</title></head>\
             <body></body></html>"
        );
    }

    /// With nothing to inject, the first chunk is passed through unchanged, whatever it is.
    #[test]
    fn a_page_without_head_content_is_unchanged() {
        let (_, output) = ServerMetaContext::new();

        let page =
            output.inject_into_first_chunk("<main>content</main>".to_owned());

        assert_eq!(page, "<main>content</main>");
    }

    /// `<Html>` and `<Body>` attributes with no `<html` or `<body` tag in the first chunk
    /// are left out (and logged); the rest of the page is unchanged.
    #[test]
    fn attributes_without_their_tag_are_left_out() {
        let (meta_context, output) = ServerMetaContext::new();
        _ = meta_context.html.send(" lang=\"fr\"".to_owned());
        _ = meta_context.body.send(" class=\"app\"".to_owned());

        let page =
            output.inject_into_first_chunk("<main>content</main>".to_owned());

        assert_eq!(page, "<main>content</main>");
    }
}
