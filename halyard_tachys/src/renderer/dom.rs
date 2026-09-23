#![allow(missing_docs)]

//! See [`Renderer`](crate::renderer::Renderer) and [`Rndr`](crate::renderer::Rndr) for additional information.

use super::{CastFrom, RemoveEventHandler};
use crate::{
    dom::{document, window},
    dom_error::DomError,
    ok_or_debug, or_debug,
    view::{Mountable, ToTemplate},
};
use rustc_hash::FxHashSet;
use std::{
    any::TypeId,
    borrow::Cow,
    cell::{LazyCell, RefCell},
};
use wasm_bindgen::{intern, prelude::Closure, JsCast, JsValue};
use web_sys::{AddEventListenerOptions, Comment, HtmlTemplateElement};

/// A [`Renderer`](crate::renderer::Renderer) that uses `web-sys` to manipulate DOM elements in the browser.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Dom;

thread_local! {
    pub(crate) static GLOBAL_EVENTS: RefCell<FxHashSet<Cow<'static, str>>> = Default::default();
    pub static TEMPLATE_CACHE: RefCell<Vec<(Cow<'static, str>, web_sys::Element)>> = Default::default();
}

pub type Node = web_sys::Node;
pub type Text = web_sys::Text;
pub type Element = web_sys::Element;
pub type Placeholder = web_sys::Comment;
pub type Event = wasm_bindgen::JsValue;
pub type ClassList = web_sys::DomTokenList;
pub type CssStyleDeclaration = web_sys::CssStyleDeclaration;
pub type TemplateElement = web_sys::HtmlTemplateElement;

/// The tag of the placeholder that [`Dom::create_element`] returns when the browser refuses
/// to create an element. A valid custom-element name that halyard never defines, so the
/// browser accepts it, and the inspector shows what happened.
const INVALID_ELEMENT_TAG: &str = "halyard-invalid-element";

const SVG_NAMESPACE: &str = "http://www.w3.org/2000/svg";

/// A microtask is a short function which will run after the current task has
/// completed its work and when there is no other code waiting to be run before
/// control of the execution context is returned to the browser's event loop.
///
/// Microtasks are especially useful for libraries and frameworks that need
/// to perform final cleanup or other just-before-rendering tasks.
///
/// [MDN queueMicrotask](https://developer.mozilla.org/en-US/docs/Web/API/queueMicrotask)
///
/// Where `queueMicrotask` is missing, this uses `Promise.resolve().then(task)`, which
/// also runs `task` as a microtask. If neither works, it logs a warning and `task` is
/// dropped without running.
pub fn queue_microtask(task: impl FnOnce() + 'static) {
    if let Err(err) = try_queue_microtask(task) {
        err.warn("the task was dropped without running", None);
    }
}

fn try_queue_microtask(task: impl FnOnce() + 'static) -> Result<(), DomError> {
    let task = Closure::once_into_js(task);
    call_method(&window(), "queueMicrotask", "window.queueMicrotask", &task)
        .or_else(|_| {
            let resolved = js_sys::Promise::resolve(&JsValue::UNDEFINED);
            call_method(&resolved, "then", "Promise.prototype.then", &task)
        })
}

/// Calls `target[name](arg)`, checking that `target[name]` is a function.
fn call_method(
    target: &JsValue,
    name: &'static str,
    op: &'static str,
    arg: &JsValue,
) -> Result<(), DomError> {
    let method = js_sys::Reflect::get(target, &JsValue::from_str(name))
        .map_err(|err| DomError::thrown(op, &err))?
        .dyn_into::<js_sys::Function>()
        .map_err(|_| DomError::new(op, "not a function"))?;
    method
        .call1(target, arg)
        .map(drop)
        .map_err(|err| DomError::thrown(op, &err))
}

fn queue(fun: Box<dyn FnOnce()>) {
    use std::cell::{Cell, RefCell};

    thread_local! {
        static PENDING: Cell<bool> = const { Cell::new(false) };
        static QUEUE: RefCell<Vec<Box<dyn FnOnce()>>> = RefCell::new(Vec::new());
    }

    fn flush() {
        let tasks = QUEUE.take();
        for task in tasks {
            task();
        }
        PENDING.set(false);
    }

    QUEUE.with_borrow_mut(|q| q.push(fun));
    if !PENDING.replace(true) {
        if let Err(err) = try_queue_microtask(flush) {
            // otherwise `PENDING` would stay set and every later update would be lost
            err.warn("running the queued DOM updates now instead", None);
            flush();
        }
    }
}

impl Dom {
    pub fn intern(text: &str) -> &str {
        intern(text)
    }

    /// Creates an element, in `namespace` if given.
    ///
    /// If the browser refuses (an invalid tag name, or one the namespace does not allow),
    /// this logs a warning and returns a `<halyard-invalid-element>` placeholder instead.
    pub fn create_element(tag: &str, namespace: Option<&str>) -> Element {
        Self::try_create_element(tag, namespace).unwrap_or_else(|err| {
            err.warn(
                &format!(
                    "rendering <{INVALID_ELEMENT_TAG}> in place of <{tag}>"
                ),
                None,
            );
            Self::invalid_element_placeholder(namespace)
        })
    }

    fn try_create_element(
        tag: &str,
        namespace: Option<&str>,
    ) -> Result<Element, DomError> {
        if let Some(namespace) = namespace {
            document()
                .create_element_ns(
                    Some(Self::intern(namespace)),
                    Self::intern(tag),
                )
                .map_err(|err| {
                    DomError::thrown("document.createElementNS", &err)
                })
        } else {
            document()
                .create_element(Self::intern(tag))
                .map_err(|err| DomError::thrown("document.createElement", &err))
        }
    }

    /// Stands in for an element the browser refused to create: in the same namespace if
    /// the namespace allows the name, as an HTML element otherwise.
    fn invalid_element_placeholder(namespace: Option<&str>) -> Element {
        let document = document();
        namespace
            .and_then(|namespace| {
                document
                    .create_element_ns(Some(namespace), INVALID_ELEMENT_TAG)
                    .ok()
            })
            .or_else(|| document.create_element(INVALID_ELEMENT_TAG).ok())
            .unwrap_or_else(|| {
                // The DOM standard rules this out: the name is valid and halyard never
                // defines it as a custom element. If a browser does it anyway, a comment
                // still keeps the tree consistent (it can be inserted and removed);
                // element-only operations on it fail.
                DomError::new(
                    "document.createElement",
                    format!("refused the placeholder <{INVALID_ELEMENT_TAG}>"),
                )
                .warn("rendering an empty comment in its place", None);
                document.create_comment("").unchecked_into()
            })
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(level = "trace"))]
    pub fn create_text_node(text: &str) -> Text {
        document().create_text_node(text)
    }

    pub fn create_placeholder() -> Placeholder {
        thread_local! {
            static COMMENT: LazyCell<Comment> = LazyCell::new(|| {
                document().create_comment("")
            });
        }
        COMMENT.with(|n| match n.clone_node() {
            Ok(comment) => comment.unchecked_into(),
            Err(err) => {
                DomError::thrown("Node.cloneNode", &err)
                    .warn("creating a new comment instead", None);
                document().create_comment("")
            }
        })
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(level = "trace"))]
    pub fn set_text(node: &Text, text: &str) {
        node.set_node_value(Some(text));
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(level = "trace"))]
    pub fn set_attribute(node: &Element, name: &str, value: &str) {
        or_debug!(node.set_attribute(name, value), node, "setAttribute");
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(level = "trace"))]
    pub fn remove_attribute(node: &Element, name: &str) {
        or_debug!(node.remove_attribute(name), node, "removeAttribute");
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(level = "trace"))]
    pub fn insert_node(
        parent: &Element,
        new_child: &Node,
        anchor: Option<&Node>,
    ) {
        ok_or_debug!(
            parent.insert_before(new_child, anchor),
            parent,
            "insertNode"
        );
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(level = "trace"))]
    pub fn try_insert_node(
        parent: &Element,
        new_child: &Node,
        anchor: Option<&Node>,
    ) -> bool {
        parent.insert_before(new_child, anchor).is_ok()
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(level = "trace"))]
    pub fn remove_node(parent: &Element, child: &Node) -> Option<Node> {
        ok_or_debug!(parent.remove_child(child), parent, "removeNode")
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(level = "trace"))]
    pub fn remove(node: &Node) {
        node.unchecked_ref::<Element>().remove();
    }

    pub fn get_parent(node: &Node) -> Option<Node> {
        node.parent_node()
    }

    pub fn first_child(node: &Node) -> Option<Node> {
        #[cfg(debug_assertions)]
        {
            let node = node.first_child();
            // if it's a comment node that starts with hot-reload, it's a marker that should be
            // ignored
            if let Some(node) = node.as_ref() {
                if node.node_type() == 8
                    && node
                        .text_content()
                        .unwrap_or_default()
                        .starts_with("hot-reload")
                {
                    return Self::next_sibling(node);
                }
            }

            node
        }
        #[cfg(not(debug_assertions))]
        {
            node.first_child()
        }
    }

    pub fn next_sibling(node: &Node) -> Option<Node> {
        #[cfg(debug_assertions)]
        {
            let node = node.next_sibling();
            // if it's a comment node that starts with hot-reload, it's a marker that should be
            // ignored
            if let Some(node) = node.as_ref() {
                if node.node_type() == 8
                    && node
                        .text_content()
                        .unwrap_or_default()
                        .starts_with("hot-reload")
                {
                    return Self::next_sibling(node);
                }
            }

            node
        }
        #[cfg(not(debug_assertions))]
        {
            node.next_sibling()
        }
    }

    pub fn log_node(node: &Node) {
        web_sys::console::log_1(node);
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(level = "trace"))]
    pub fn clear_children(parent: &Element) {
        parent.set_text_content(Some(""));
    }

    /// Mounts the new child before the marker as its sibling.
    ///
    /// If `before` does not have a parent [`crate::renderer::types::Element`] (it was
    /// detached, or its parent is a document fragment or shadow root), this logs a
    /// warning and leaves the child unmounted.
    pub fn mount_before<M>(new_child: &mut M, before: &Node)
    where
        M: Mountable,
    {
        if !Self::try_mount_before(new_child, before) {
            DomError::new(
                "Dom::mount_before",
                "the marker has no parent element",
            )
            .warn("the new content was not mounted", Some(before.as_ref()));
        }
    }

    /// Tries to mount the new child before the marker as its sibling.
    ///
    /// Returns `false` if the child did not have a valid parent.
    #[track_caller]
    pub fn try_mount_before<M>(new_child: &mut M, before: &Node) -> bool
    where
        M: Mountable,
    {
        if let Some(parent) =
            Self::get_parent(before).and_then(Element::cast_from)
        {
            new_child.mount(&parent, Some(before));
            true
        } else {
            false
        }
    }

    pub fn set_property_or_value(el: &Element, key: &str, value: &JsValue) {
        if key == "value" {
            queue(Box::new({
                let el = el.clone();
                let value = value.clone();
                move || {
                    Self::set_property(&el, "value", &value);
                }
            }))
        } else {
            Self::set_property(el, key, value);
        }
    }

    pub fn set_property(el: &Element, key: &str, value: &JsValue) {
        or_debug!(
            js_sys::Reflect::set(
                el,
                &wasm_bindgen::JsValue::from_str(key),
                value,
            ),
            el,
            "setProperty"
        );
    }

    pub fn add_event_listener(
        el: &Element,
        name: &str,
        cb: Box<dyn FnMut(Event)>,
    ) -> RemoveEventHandler<Element> {
        let cb = wasm_bindgen::closure::Closure::wrap(cb);
        let name = intern(name);
        or_debug!(
            el.add_event_listener_with_callback(
                name,
                cb.as_ref().unchecked_ref()
            ),
            el,
            "addEventListener"
        );

        // return the remover
        RemoveEventHandler::new({
            let name = name.to_owned();
            let el = el.clone();
            // safe to construct this here, because it will only run in the browser
            // so it will always be accessed or dropped from the main thread
            let cb = send_wrapper::SendWrapper::new(move || {
                or_debug!(
                    el.remove_event_listener_with_callback(
                        intern(&name),
                        cb.as_ref().unchecked_ref()
                    ),
                    &el,
                    "removeEventListener"
                )
            });
            move || cb()
        })
    }

    pub fn add_event_listener_use_capture(
        el: &Element,
        name: &str,
        cb: Box<dyn FnMut(Event)>,
    ) -> RemoveEventHandler<Element> {
        let cb = wasm_bindgen::closure::Closure::wrap(cb);
        let name = intern(name);
        let options = AddEventListenerOptions::new();
        options.set_capture(true);
        or_debug!(
            el.add_event_listener_with_callback_and_add_event_listener_options(
                name,
                cb.as_ref().unchecked_ref(),
                &options
            ),
            el,
            "addEventListenerUseCapture"
        );

        // return the remover
        RemoveEventHandler::new({
            let name = name.to_owned();
            let el = el.clone();
            // safe to construct this here, because it will only run in the browser
            // so it will always be accessed or dropped from the main thread
            let cb = send_wrapper::SendWrapper::new(move || {
                or_debug!(
                    el.remove_event_listener_with_callback_and_bool(
                        intern(&name),
                        cb.as_ref().unchecked_ref(),
                        true
                    ),
                    &el,
                    "removeEventListener"
                )
            });
            move || cb()
        })
    }

    /// Returns `event.target` cast to `T`.
    ///
    /// If the target is not a `T` (the event happened on a child of the element that has
    /// the listener), this returns the listener's element (`event.currentTarget`) if that
    /// is a `T`, or else the nearest ancestor of the target that is a `T` (for delegated
    /// listeners, whose current target is the window).
    ///
    /// ## Panics
    /// If none of those is a `T`: the signature has no way to say so until the renderer
    /// returns typed errors (`docs/no-panics.md`, change 3). For a listener that halyard
    /// attached to an element of type `T`, one of them is that element, unless this is
    /// called after the event, once the target has been moved out of that element.
    pub fn event_target<T>(ev: &Event) -> T
    where
        T: CastFrom<Element>,
    {
        Self::find_event_target(ev.unchecked_ref()).expect(
            "event_target: neither the event's target, its current target, nor \
             any ancestor of the target has the requested element type",
        )
    }

    fn find_event_target<T>(ev: &web_sys::Event) -> Option<T>
    where
        T: CastFrom<Element>,
    {
        let cast = |target: &JsValue| {
            T::cast_from(target.clone().unchecked_into::<Element>())
        };
        let target = ev.target();
        if let Some(found) = target.as_ref().and_then(|target| cast(target)) {
            return Some(found);
        }
        if let Some(found) = ev
            .current_target()
            .as_ref()
            .and_then(|current| cast(current))
        {
            return Some(found);
        }
        let mut node = target.and_then(|target| target.dyn_into::<Node>().ok());
        while let Some(current) = node {
            if let Some(found) = cast(&current) {
                return Some(found);
            }
            node = current.parent_node().or_else(|| {
                current
                    .dyn_ref::<web_sys::ShadowRoot>()
                    .map(|root| root.host().into())
            });
        }
        None
    }

    pub fn add_event_listener_delegated(
        el: &Element,
        name: Cow<'static, str>,
        delegation_key: Cow<'static, str>,
        cb: Box<dyn FnMut(Event)>,
    ) -> RemoveEventHandler<Element> {
        let cb = Closure::wrap(cb);
        let key = intern(&delegation_key);
        or_debug!(
            js_sys::Reflect::set(el, &JsValue::from_str(key), cb.as_ref()),
            el,
            "set property"
        );

        GLOBAL_EVENTS.with_borrow_mut(|events| {
            if !events.contains(&name) {
                // create global handler
                let key = JsValue::from_str(key);
                let handler = move |ev: web_sys::Event| {
                    let target = ev.target();
                    let node = ev.composed_path().get(0);
                    let mut node = if node.is_undefined() || node.is_null() {
                        JsValue::from(target)
                    } else {
                        node
                    };

                    // TODO reverse Shadow DOM retargetting
                    // TODO simulate currentTarget

                    // not `!is_null()`: with no target at all, `node` is `undefined`
                    while node.is_object() {
                        let node_is_disabled = js_sys::Reflect::get(
                            &node,
                            &JsValue::from_str("disabled"),
                        )
                        .map(|disabled| disabled.is_truthy())
                        .unwrap_or_else(|err| {
                            DomError::thrown(
                                "reading `disabled` for event delegation",
                                &err,
                            )
                            .warn("treating the node as enabled", Some(&node));
                            false
                        });
                        if !node_is_disabled {
                            let maybe_handler =
                                js_sys::Reflect::get(&node, &key)
                                    .unwrap_or_else(|err| {
                                        DomError::thrown(
                                            "reading a delegated event handler",
                                            &err,
                                        )
                                        .warn(
                                            "skipping this node's handler",
                                            Some(&node),
                                        );
                                        JsValue::UNDEFINED
                                    });
                            if !maybe_handler.is_undefined() {
                                let f = maybe_handler
                                    .unchecked_ref::<js_sys::Function>();
                                let _ = f.call1(&node, &ev);

                                if ev.cancel_bubble() {
                                    return;
                                }
                            }
                        }

                        // navigate up tree
                        if let Some(parent) =
                            node.unchecked_ref::<web_sys::Node>().parent_node()
                        {
                            node = parent.into()
                        } else if let Some(root) =
                            node.dyn_ref::<web_sys::ShadowRoot>()
                        {
                            node = root.host().unchecked_into();
                        } else {
                            node = JsValue::null()
                        }
                    }
                };

                let handler =
                    Box::new(handler) as Box<dyn FnMut(web_sys::Event)>;
                let handler = Closure::wrap(handler).into_js_value();
                match window().add_event_listener_with_callback(
                    &name,
                    handler.unchecked_ref(),
                ) {
                    // register that we've created handler
                    Ok(()) => {
                        events.insert(name);
                    }
                    Err(err) => {
                        DomError::thrown("window.addEventListener", &err).warn(
                            &format!(
                                "delegated `{name}` handlers will not run; \
                                 adding the next `{name}` handler retries"
                            ),
                            None,
                        )
                    }
                }
            }
        });

        // return the remover
        RemoveEventHandler::new({
            let key = key.to_owned();
            let el = el.clone();
            // safe to construct this here, because it will only run in the browser
            // so it will always be accessed or dropped from the main thread
            let el_cb = send_wrapper::SendWrapper::new((el, cb));
            move || {
                let (el, cb) = el_cb.take();
                drop(cb);
                or_debug!(
                    js_sys::Reflect::delete_property(
                        &el,
                        &JsValue::from_str(&key)
                    ),
                    &el,
                    "delete property"
                );
            }
        })
    }

    pub fn class_list(el: &Element) -> ClassList {
        el.class_list()
    }

    pub fn add_class(list: &ClassList, name: &str) {
        or_debug!(list.add_1(name), list.unchecked_ref(), "add()");
    }

    pub fn remove_class(list: &ClassList, name: &str) {
        or_debug!(list.remove_1(name), list.unchecked_ref(), "remove()");
    }

    pub fn style(el: &Element) -> CssStyleDeclaration {
        el.unchecked_ref::<web_sys::HtmlElement>().style()
    }

    pub fn set_css_property(
        style: &CssStyleDeclaration,
        name: &str,
        value: &str,
    ) {
        or_debug!(
            style.set_property(name, value),
            style.unchecked_ref(),
            "setProperty"
        );
    }

    pub fn remove_css_property(style: &CssStyleDeclaration, name: &str) {
        or_debug!(
            style.remove_property(name),
            style.unchecked_ref(),
            "removeProperty"
        );
    }

    pub fn set_inner_html(el: &Element, html: &str) {
        el.set_inner_html(html);
    }

    pub fn get_template<V>() -> TemplateElement
    where
        V: ToTemplate + 'static,
    {
        thread_local! {
            static TEMPLATE_ELEMENT: LazyCell<HtmlTemplateElement> =
                LazyCell::new(Dom::create_template_element);
            static TEMPLATES: RefCell<Vec<(TypeId, HtmlTemplateElement)>> = Default::default();
        }

        TEMPLATES.with_borrow_mut(|t| {
            let id = TypeId::of::<V>();
            t.iter()
                .find_map(|entry| (entry.0 == id).then(|| entry.1.clone()))
                .unwrap_or_else(|| {
                    let tpl = TEMPLATE_ELEMENT.with(|t| match t.clone_node() {
                        Ok(tpl) => tpl.unchecked_into::<HtmlTemplateElement>(),
                        Err(err) => {
                            DomError::thrown("Node.cloneNode", &err).warn(
                                "creating a new <template> instead",
                                None,
                            );
                            Self::create_template_element()
                        }
                    });
                    let mut buf = String::new();
                    V::to_template(
                        &mut buf,
                        &mut String::new(),
                        &mut String::new(),
                        &mut String::new(),
                        &mut Default::default(),
                    );
                    tpl.set_inner_html(&buf);
                    t.push((id, tpl.clone()));
                    tpl
                })
        })
    }

    /// A new `<template>`.
    ///
    /// Outside an HTML document `createElement("template")` gives an element with no
    /// `content` (and if the browser refuses, [`Dom::create_element`] gives a
    /// placeholder); [`Dom::clone_template`] then logs and returns an empty fragment.
    fn create_template_element() -> HtmlTemplateElement {
        Self::create_element("template", None).unchecked_into()
    }

    /// Deeply clones the template's content.
    ///
    /// If the browser refuses (`tpl` is not a real `<template>`), this logs a warning and
    /// returns an empty fragment, so the view built from it renders nothing.
    pub fn clone_template(tpl: &TemplateElement) -> Element {
        match tpl.content().clone_node_with_deep(true) {
            Ok(content) => content.unchecked_into(),
            Err(err) => {
                DomError::thrown("cloning <template> content", &err)
                    .warn("using an empty fragment instead", None);
                document().create_document_fragment().unchecked_into()
            }
        }
    }

    pub fn create_element_from_html(html: Cow<'static, str>) -> Element {
        let tpl = TEMPLATE_CACHE.with_borrow_mut(|cache| {
            if let Some(tpl_content) = cache.iter().find_map(|(key, tpl)| {
                (html == *key)
                    .then_some(Self::clone_template(tpl.unchecked_ref()))
            }) {
                tpl_content
            } else {
                let tpl = Self::create_element("template", None);
                tpl.set_inner_html(&html);
                let tpl_content = Self::clone_template(tpl.unchecked_ref());
                cache.push((html, tpl));
                tpl_content
            }
        });
        tpl.first_element_child().unwrap_or(tpl)
    }

    pub fn create_svg_element_from_html(html: Cow<'static, str>) -> Element {
        let tpl = TEMPLATE_CACHE.with_borrow_mut(|cache| {
            if let Some(tpl_content) = cache.iter().find_map(|(key, tpl)| {
                (html == *key)
                    .then_some(Self::clone_template(tpl.unchecked_ref()))
            }) {
                tpl_content
            } else {
                let tpl = Self::create_element("template", None);
                let svg = Self::create_element("svg", Some(SVG_NAMESPACE));
                let g = Self::create_element("g", Some(SVG_NAMESPACE));
                g.set_inner_html(&html);
                if let Err(err) = svg.append_child(&g) {
                    DomError::thrown("Node.appendChild", &err)
                        .warn("<g> not added to <svg>", None);
                }
                if let Err(err) = tpl
                    .unchecked_ref::<TemplateElement>()
                    .content()
                    .append_child(&svg)
                {
                    DomError::thrown("Node.appendChild", &err)
                        .warn("<svg> not added to <template>", None);
                }
                let tpl_content = Self::clone_template(tpl.unchecked_ref());
                cache.push((html, tpl));
                tpl_content
            }
        });

        match tpl.first_element_child() {
            Some(svg) => svg.first_element_child().unwrap_or(svg),
            None => {
                DomError::new(
                    "Dom::create_svg_element_from_html",
                    "the template has no <svg> element",
                )
                .warn("rendering an empty <g> instead", None);
                Self::create_element("g", Some(SVG_NAMESPACE))
            }
        }
    }
}

impl Mountable for Node {
    fn unmount(&mut self) {
        // What `ChildNode.remove()` does for the other node types (a plain `Node` has no
        // `remove()`): detach it from its parent, if it has one.
        if let Some(parent) = self.parent_node() {
            if let Err(err) = parent.remove_child(self) {
                DomError::thrown("Node.removeChild", &err)
                    .warn("the node stays in the page", Some(self));
            }
        }
    }

    fn mount(&mut self, parent: &Element, marker: Option<&Node>) {
        Dom::insert_node(parent, self, marker);
    }

    fn try_mount(&mut self, parent: &Element, marker: Option<&Node>) -> bool {
        Dom::try_insert_node(parent, self, marker)
    }

    fn insert_before_this(&self, child: &mut dyn Mountable) -> bool {
        let parent = Dom::get_parent(self).and_then(Element::cast_from);
        if let Some(parent) = parent {
            child.mount(&parent, Some(self));
            return true;
        }
        false
    }

    fn elements(&self) -> Vec<crate::renderer::types::Element> {
        vec![]
    }
}

impl Mountable for Text {
    fn unmount(&mut self) {
        self.remove();
    }

    fn mount(&mut self, parent: &Element, marker: Option<&Node>) {
        Dom::insert_node(parent, self, marker);
    }

    fn try_mount(&mut self, parent: &Element, marker: Option<&Node>) -> bool {
        Dom::try_insert_node(parent, self, marker)
    }

    fn insert_before_this(&self, child: &mut dyn Mountable) -> bool {
        let parent =
            Dom::get_parent(self.as_ref()).and_then(Element::cast_from);
        if let Some(parent) = parent {
            child.mount(&parent, Some(self));
            return true;
        }
        false
    }

    fn elements(&self) -> Vec<crate::renderer::types::Element> {
        vec![]
    }
}

impl Mountable for Comment {
    fn unmount(&mut self) {
        self.remove();
    }

    fn mount(&mut self, parent: &Element, marker: Option<&Node>) {
        Dom::insert_node(parent, self, marker);
    }

    fn try_mount(&mut self, parent: &Element, marker: Option<&Node>) -> bool {
        Dom::try_insert_node(parent, self, marker)
    }

    fn insert_before_this(&self, child: &mut dyn Mountable) -> bool {
        let parent =
            Dom::get_parent(self.as_ref()).and_then(Element::cast_from);
        if let Some(parent) = parent {
            child.mount(&parent, Some(self));
            return true;
        }
        false
    }

    fn elements(&self) -> Vec<crate::renderer::types::Element> {
        vec![]
    }
}

impl Mountable for Element {
    fn unmount(&mut self) {
        self.remove();
    }

    fn mount(&mut self, parent: &Element, marker: Option<&Node>) {
        Dom::insert_node(parent, self, marker);
    }

    fn insert_before_this(&self, child: &mut dyn Mountable) -> bool {
        let parent =
            Dom::get_parent(self.as_ref()).and_then(Element::cast_from);
        if let Some(parent) = parent {
            child.mount(&parent, Some(self));
            return true;
        }
        false
    }

    fn elements(&self) -> Vec<crate::renderer::types::Element> {
        vec![self.clone()]
    }
}

impl CastFrom<Node> for Text {
    fn cast_from(node: Node) -> Option<Text> {
        node.clone().dyn_into().ok()
    }
}

impl CastFrom<Node> for Comment {
    fn cast_from(node: Node) -> Option<Comment> {
        node.clone().dyn_into().ok()
    }
}

impl CastFrom<Node> for Element {
    fn cast_from(node: Node) -> Option<Element> {
        node.clone().dyn_into().ok()
    }
}

impl<T> CastFrom<JsValue> for T
where
    T: JsCast,
{
    fn cast_from(source: JsValue) -> Option<Self> {
        source.dyn_into::<T>().ok()
    }
}

impl<T> CastFrom<Element> for T
where
    T: JsCast,
{
    fn cast_from(source: Element) -> Option<Self> {
        source.dyn_into::<T>().ok()
    }
}
