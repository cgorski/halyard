use crate::dom::helpers::document;
use crate::{children::TypedChildrenFn, mount, IntoView};
use halyard_macro::component;
use halyard_reactive_graph::{effect::Effect, graph::untrack, owner::Owner};
use std::sync::Arc;
use wasm_bindgen::JsValue;

/// Why a `<Portal/>` rendered nothing.
#[derive(Debug, thiserror::Error)]
enum PortalError {
    /// No `mount` was given and the document has no `<body>`.
    #[error("no `mount` element was given and the document has no <body>")]
    NoBody,
    /// The DOM refused to create the portal's container element.
    #[error("could not create the <{tag}> container: {js:?}")]
    CreateContainer { tag: &'static str, js: JsValue },
}

fn warn_portal(err: PortalError) {
    crate::logging::warn!("[halyard] <Portal/> renders nothing: {err}");
}

/// Renders components somewhere else in the DOM.
///
/// Useful for inserting modals and tooltips outside of a cropping layout.
/// If no mount point is given, the portal is inserted in `document.body`;
/// it is wrapped in a `<div>` unless  `is_svg` is `true` in which case it's wrapped in a `<g>`.
/// Setting `use_shadow` to `true` places the element in a shadow root to isolate styles.
#[cfg_attr(feature = "tracing", tracing::instrument(level = "trace", skip_all))]
#[component]
pub fn Portal<V>(
    /// Target element where the children will be appended
    #[prop(into, optional)]
    mount: Option<web_sys::Element>,
    /// Whether to use a shadow DOM inside `mount`. Defaults to `false`.
    #[prop(optional)]
    use_shadow: bool,
    /// When using SVG this has to be set to `true`. Defaults to `false`.
    #[prop(optional)]
    is_svg: bool,
    /// The children to teleport into the `mount` element
    children: TypedChildrenFn<V>,
) -> impl IntoView
where
    V: IntoView + 'static,
{
    if cfg!(target_arch = "wasm32")
        && Owner::current_shared_context()
            .map(|sc| sc.is_browser())
            .unwrap_or(true)
    {
        use send_wrapper::SendWrapper;
        use wasm_bindgen::JsCast;

        let Some(mount) =
            mount.or_else(|| document().body().map(JsCast::unchecked_into))
        else {
            warn_portal(PortalError::NoBody);
            return;
        };
        let children = children.into_inner();

        Effect::new(move |_| {
            let container = if is_svg {
                document()
                    .create_element_ns(Some("http://www.w3.org/2000/svg"), "g")
                    .map_err(|js| PortalError::CreateContainer { tag: "g", js })
            } else {
                document().create_element("div").map_err(|js| {
                    PortalError::CreateContainer { tag: "div", js }
                })
            };
            let container = match container {
                Ok(container) => container,
                Err(err) => {
                    warn_portal(err);
                    return;
                }
            };

            let render_root = if use_shadow {
                container
                    .attach_shadow(&web_sys::ShadowRootInit::new(
                        web_sys::ShadowRootMode::Open,
                    ))
                    .map(|root| root.unchecked_into())
                    .unwrap_or(container.clone())
            } else {
                container.clone()
            };

            let _ = mount.append_child(&container);
            let handle = SendWrapper::new((
                mount::mount_to(render_root.unchecked_into(), {
                    let children = Arc::clone(&children);
                    move || untrack(|| children())
                }),
                mount.clone(),
                container,
            ));

            Owner::on_cleanup({
                move || {
                    let (handle, mount, container) = handle.take();
                    drop(handle);
                    let _ = mount.remove_child(&container);
                }
            })
        });
    }
}
