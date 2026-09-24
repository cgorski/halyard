use crate::{
    renderer::{CastFrom, Rndr},
    view::{Position, PositionState},
};
#[cfg(any(debug_assertions, halyard_debuginfo))]
use std::cell::Cell;
use std::{cell::RefCell, panic::Location, rc::Rc};
use wasm_bindgen::JsCast;
use web_sys::{Comment, Element, Node, Text};

#[cfg(feature = "mark_branches")]
const COMMENT_NODE: u16 = 8;

/// Hydration works by walking over the DOM, adding interactivity as needed.
///
/// This cursor tracks the location in the DOM that is currently being hydrated. Each that type
/// implements [`RenderHtml`](crate::view::RenderHtml) knows how to advance the cursor to access
/// the nodes it needs.
///
/// # Mismatch recovery
///
/// If the DOM does not match what the view expects (see [`hydration_failed`]), the cursor
/// switches into a *detached* mode: it stops walking the server-rendered DOM, every
/// subsequent lookup fails to cast and is replaced by a freshly created, detached node, and
/// the caller (see `halyard::mount::hydrate_from`) discards the resulting state and
/// client-renders instead. This keeps the Rust side consistent (no unwinding, which is not
/// available on `wasm32`) while never touching the server DOM with a bad cursor position.
#[derive(Debug)]
pub struct Cursor(Rc<RefCell<crate::renderer::types::Node>>);

impl Clone for Cursor {
    fn clone(&self) -> Self {
        Self(Rc::clone(&self.0))
    }
}

impl Cursor
where
    crate::renderer::types::Element: AsRef<crate::renderer::types::Node>,
{
    /// Creates a new cursor starting at the root element.
    pub fn new(root: crate::renderer::types::Element) -> Self {
        let root = <crate::renderer::types::Element as AsRef<
            crate::renderer::types::Node,
        >>::as_ref(&root)
        .clone();
        Self(Rc::new(RefCell::new(root)))
    }

    /// Returns the node at which the cursor is currently located.
    ///
    /// After a hydration mismatch this returns a detached node that no view can cast into
    /// its expected type, so that the rest of the tree is synthesized rather than read from
    /// the (mismatched) server DOM.
    pub fn current(&self) -> crate::renderer::types::Node {
        if hydration_failed() {
            return detached_sentinel();
        }
        self.0.borrow().clone()
    }

    /// Advances to the next child of the node at which the cursor is located.
    ///
    /// Does nothing if there is no child.
    pub fn child(&self) {
        if hydration_failed() {
            return;
        }
        let mut inner = self.0.borrow_mut();
        if let Some(node) = Rndr::first_child(&inner) {
            *inner = node;
        }

        #[cfg(feature = "mark_branches")]
        {
            while inner.node_type() == COMMENT_NODE {
                if let Some(content) = inner.text_content() {
                    if content.starts_with("bo") || content.starts_with("bc") {
                        if let Some(sibling) = Rndr::next_sibling(&inner) {
                            *inner = sibling;
                            continue;
                        }
                    }
                }

                break;
            }
        }
    }

    /// Advances to the next sibling of the node at which the cursor is located.
    ///
    /// Does nothing if there is no sibling.
    pub fn sibling(&self) {
        if hydration_failed() {
            return;
        }
        let mut inner = self.0.borrow_mut();
        if let Some(node) = Rndr::next_sibling(&inner) {
            *inner = node;
        }

        #[cfg(feature = "mark_branches")]
        {
            while inner.node_type() == COMMENT_NODE {
                if let Some(content) = inner.text_content() {
                    if content.starts_with("bo") || content.starts_with("bc") {
                        if let Some(sibling) = Rndr::next_sibling(&inner) {
                            *inner = sibling;
                            continue;
                        }
                    }
                }
                break;
            }
        }
    }

    /// Moves to the parent of the node at which the cursor is located.
    ///
    /// Does nothing if there is no parent.
    pub fn parent(&self) {
        if hydration_failed() {
            return;
        }
        let mut inner = self.0.borrow_mut();
        if let Some(node) = Rndr::get_parent(&inner) {
            *inner = node;
        }
    }

    /// Sets the cursor to some node.
    pub fn set(&self, node: crate::renderer::types::Node) {
        if hydration_failed() {
            return;
        }
        *self.0.borrow_mut() = node;
    }

    /// Advances to the next placeholder node and returns it
    pub fn next_placeholder(
        &self,
        position: &PositionState,
    ) -> crate::renderer::types::Placeholder {
        self.advance_to_placeholder(position);
        let marker = self.current();
        crate::renderer::types::Placeholder::cast_from(marker.clone())
            .unwrap_or_else(|| failed_to_cast_marker_node(marker))
    }

    /// Advances to the next placeholder node.
    pub fn advance_to_placeholder(&self, position: &PositionState) {
        if position.get() == Position::FirstChild {
            self.child();
        } else {
            self.sibling();
        }
        position.set(Position::NextChild);
    }
}

#[cfg(any(debug_assertions, halyard_debuginfo))]
thread_local! {
    static CURRENTLY_HYDRATING: Cell<Option<&'static Location<'static>>> = const { Cell::new(None) };
}

thread_local! {
    /// Set on the first hydration mismatch; cleared by [`take_hydration_failure`].
    static HYDRATION_FAILED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// A detached node returned by `Cursor::current` once hydration has failed. A
    /// `DocumentFragment` is used because it can never be cast to an `Element`, `Text`,
    /// or `Comment`, so every view synthesizes its own node instead. (Without a document,
    /// the renderer's stand-in, which cannot be cast to any of them either.)
    static DETACHED_SENTINEL: RefCell<Option<Node>> = const { RefCell::new(None) };
}

fn detached_sentinel() -> Node {
    DETACHED_SENTINEL.with(|s| {
        s.borrow_mut()
            .get_or_insert_with(|| {
                crate::renderer::dom::render_document()
                    .map_or_else(crate::renderer::dom::stand_in, |document| {
                        document.create_document_fragment().into()
                    })
            })
            .clone()
    })
}

/// Returns `true` if a hydration mismatch has been detected since the last call to
/// [`take_hydration_failure`].
///
/// While this is `true`, [`Cursor`] no longer reads the server-rendered DOM (see the type
/// docs), and the hydrated state must be discarded.
pub fn hydration_failed() -> bool {
    HYDRATION_FAILED.with(std::cell::Cell::get)
}

/// Returns whether a hydration mismatch occurred and resets the flag, so that the next
/// hydration attempt starts clean.
pub fn take_hydration_failure() -> bool {
    let failed = HYDRATION_FAILED.with(|f| f.replace(false));
    DETACHED_SENTINEL.with(|s| s.borrow_mut().take());
    failed
}

pub(crate) fn set_currently_hydrating(
    location: Option<&'static Location<'static>>,
) {
    #[cfg(any(debug_assertions, halyard_debuginfo))]
    {
        CURRENTLY_HYDRATING.set(location);
    }
    #[cfg(not(any(debug_assertions, halyard_debuginfo)))]
    {
        _ = location;
    }
}

fn currently_hydrating() -> String {
    #[cfg(any(debug_assertions, halyard_debuginfo))]
    {
        CURRENTLY_HYDRATING
            .take()
            .map(|n| n.to_string())
            .unwrap_or_else(|| "{unknown}".to_string())
    }
    #[cfg(not(any(debug_assertions, halyard_debuginfo)))]
    {
        "{unknown: build with debug assertions or `--cfg halyard_debuginfo` \
         to record view locations}"
            .to_string()
    }
}

/// A short, human-readable description of a DOM node, e.g. `<div id="app" class="x">`,
/// `#text "Hello, wo…"`, or `<!--<() />-->`.
pub fn describe_node(node: &Node) -> String {
    const TEXT_NODE: u16 = 3;
    const COMMENT: u16 = 8;
    const DOCUMENT: u16 = 9;
    const DOCUMENT_FRAGMENT: u16 = 11;

    fn snippet(text: &str) -> String {
        let text = text.trim();
        let mut out: String = text.chars().take(40).collect();
        if text.chars().count() > 40 {
            out.push('…');
        }
        out
    }

    match node.node_type() {
        TEXT_NODE => {
            format!(
                "#text {:?}",
                snippet(&node.text_content().unwrap_or_default())
            )
        }
        COMMENT => {
            format!(
                "<!--{}-->",
                snippet(&node.text_content().unwrap_or_default())
            )
        }
        DOCUMENT => "#document".to_string(),
        DOCUMENT_FRAGMENT => "#document-fragment (detached)".to_string(),
        _ => match node.dyn_ref::<Element>() {
            Some(el) => {
                let mut out = format!("<{}", el.tag_name().to_lowercase());
                if let Some(id) = el.get_attribute("id") {
                    out.push_str(&format!(" id={id:?}"));
                }
                if let Some(class) = el.get_attribute("class") {
                    out.push_str(&format!(" class={:?}", snippet(&class)));
                }
                out.push('>');
                out
            }
            None => node.node_name().to_lowercase(),
        },
    }
}

/// The path from the document root to `node`, in CSS-like notation, e.g.
/// `html > body > main > div#app > p:nth-child(2) > #text`.
pub fn dom_path(node: &Node) -> String {
    const ELEMENT_NODE: u16 = 1;
    const DOCUMENT: u16 = 9;
    let mut segments = Vec::new();
    let mut current = Some(node.clone());
    while let Some(node) = current {
        if node.node_type() == DOCUMENT {
            break;
        }
        let mut segment = if node.node_type() == ELEMENT_NODE {
            let el = node.unchecked_ref::<Element>();
            let mut seg = el.tag_name().to_lowercase();
            if let Some(id) = el.get_attribute("id") {
                seg.push('#');
                seg.push_str(&id);
            }
            seg
        } else {
            node.node_name().to_lowercase()
        };
        if let Some(parent) = node.parent_node() {
            // index among *all* child nodes (comments and text included),
            // because that is what the hydration cursor walks
            let children = parent.child_nodes();
            let count = children.length();
            let index = (0..count).find(|&i| {
                children
                    .item(i)
                    .is_some_and(|child| child.is_same_node(Some(&node)))
            });
            if let Some(suffix) = nth_child_suffix(index, count) {
                segment.push_str(&suffix);
            }
            current = Some(parent);
        } else {
            current = None;
        }
        segments.push(segment);
    }
    segments.reverse();
    segments.join(" > ")
}

/// The `:nth-child(n)` suffix (1-based) for the child node at 0-based `index` among its
/// parent's `count` child nodes. `None` for an only child, for a node not found among
/// its parent's children, and for an index whose position does not fit in a `u32`.
fn nth_child_suffix(index: Option<u32>, count: u32) -> Option<String> {
    if count <= 1 {
        return None;
    }
    let position = index?.checked_add(1)?;
    Some(format!(":nth-child({position})"))
}

/// Reports a hydration mismatch. The first mismatch of a hydration pass is logged in full
/// (with the view's source location if available, what was expected, what was found, and
/// where in the DOM), later ones are suppressed since they are consequences of the first.
///
/// It never panics: it marks hydration as failed (see [`hydration_failed`]) and returns so the
/// caller can synthesize a detached node.
fn report_mismatch(expected: &str, found: &Node) {
    let first = !hydration_failed();
    let hydrating = currently_hydrating();
    let is_detached = found.node_type() == 11;
    if first {
        let found_desc = if is_detached {
            "nothing (the server-rendered DOM ended here)".to_string()
        } else {
            describe_node(found)
        };
        let msg = format!(
            "[halyard] Hydration mismatch while hydrating the view defined at \
             {hydrating}.\n  expected: {expected}\n  found:    \
             {found_desc}\n  at:       {}\n\nThe server-rendered HTML does \
             not match what the client expected at this position. Common \
             causes: the server and client were built from different code \
             or with different `--cfg`/feature flags, a component renders \
             differently on the server and in the browser (e.g. reads \
             browser-only state during render), or a browser extension \
             modified the page. The mismatch may have started slightly \
             earlier; this is the first node of an unexpected type.",
            dom_path(found)
        );
        web_sys::console::error_3(
            &wasm_bindgen::JsValue::from_str(&msg),
            found,
            &wasm_bindgen::JsValue::from_str(
                "\n\nHydration of this tree is being abandoned; the \
                 application will be rendered on the client instead.",
            ),
        );
    }
    HYDRATION_FAILED.with(|f| f.set(true));
}

pub(crate) fn failed_to_cast_element(tag_name: &str, node: Node) -> Element {
    report_mismatch(&format!("an HTML <{tag_name}> element"), &node);
    Rndr::create_element(tag_name, None)
}

pub(crate) fn failed_to_cast_marker_node(node: Node) -> Comment {
    report_mismatch("a marker (comment) node", &node);
    Rndr::create_placeholder()
}

pub(crate) fn failed_to_cast_text_node(node: Node) -> Text {
    report_mismatch("a text node", &node);
    Rndr::create_text_node("")
}

#[cfg(test)]
mod tests {
    use super::nth_child_suffix;

    #[test]
    fn nth_child_suffix_does_not_overflow_at_the_largest_index() {
        assert_eq!(nth_child_suffix(Some(u32::MAX), u32::MAX), None);
        assert_eq!(
            nth_child_suffix(Some(u32::MAX - 1), u32::MAX).as_deref(),
            Some(":nth-child(4294967295)")
        );
    }

    #[test]
    fn nth_child_suffix_is_one_based_and_only_among_siblings() {
        assert_eq!(
            nth_child_suffix(Some(0), 3).as_deref(),
            Some(":nth-child(1)")
        );
        assert_eq!(
            nth_child_suffix(Some(2), 3).as_deref(),
            Some(":nth-child(3)")
        );
        assert_eq!(nth_child_suffix(Some(0), 1), None);
        assert_eq!(nth_child_suffix(None, 3), None);
    }
}
