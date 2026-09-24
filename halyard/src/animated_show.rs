use crate::dom::helpers::TimeoutHandle;
use crate::{children::ChildrenFn, component, control_flow::Show, IntoView};
use core::time::Duration;
use halyard_macro::view;
use halyard_reactive_graph::traits::TryGet;
use halyard_reactive_graph::{
    effect::RenderEffect,
    owner::{on_cleanup, StoredValue},
    signal::RwSignal,
    traits::{Set, SetValue, TryGetUntracked, TryGetValue},
    wrappers::read::Signal,
};
use halyard_tachys::prelude::*;
use wasm_bindgen::JsValue;

/// `<AnimatedShow/>` could not schedule hiding its children.
#[derive(Debug, thiserror::Error)]
#[error(
    "[halyard] <AnimatedShow/> could not schedule hiding its children after \
     {delay:?} (setTimeout failed: {js:?}); hiding them now"
)]
struct SetTimeoutError {
    delay: Duration,
    js: JsValue,
}

/// A component that will show its children when the `when` condition is `true`.
/// Additionally, you need to specify a `hide_delay`. If the `when` condition changes to `false`,
/// the unmounting of the children will be delayed by the specified Duration.
/// If you provide the optional `show_class` and `hide_class`, you can create very easy mount /
/// unmount animations.
///
/// ```rust
/// # use core::time::Duration;
/// # use halyard::prelude::*;
/// # #[component]
/// # pub fn App() -> impl IntoView {
/// let show = RwSignal::new(false);
///
/// view! {
///     <div
///         class="hover-me"
///         on:mouseenter=move |_| show.set(true)
///         on:mouseleave=move |_| show.set(false)
///     >
///         "Hover Me"
///     </div>
///
///     <AnimatedShow
///        when=show
///        show_class="fade-in-1000"
///        hide_class="fade-out-1000"
///        hide_delay=Duration::from_millis(1000)
///     >
///        <div class="here-i-am">
///            "Here I Am!"
///        </div>
///     </AnimatedShow>
/// }
/// # }
/// ```
#[cfg_attr(feature = "tracing", tracing::instrument(level = "trace", skip_all))]
#[component]
pub fn AnimatedShow(
    /// The components Show wraps
    children: ChildrenFn,
    /// If the component should show or not
    #[prop(into)]
    when: Signal<bool>,
    /// Optional CSS class to apply if `when == true`
    #[prop(optional)]
    show_class: &'static str,
    /// Optional CSS class to apply if `when == false`
    #[prop(optional)]
    hide_class: &'static str,
    /// The timeout after which the component will be unmounted if `when == false`
    hide_delay: Duration,
) -> impl IntoView {
    let handle: StoredValue<Option<TimeoutHandle>> = StoredValue::new(None);
    // a `when` that is gone already shows nothing
    let shown = when.try_get_untracked() == Some(true);
    let cls = RwSignal::new(if shown { show_class } else { hide_class });
    let show = RwSignal::new(shown);

    let eff = RenderEffect::new(move |_| {
        // `when` is gone once its owner is disposed: then there is nothing left to animate
        let Some(when) = when.try_get() else {
            return;
        };
        if when {
            // clear any possibly active timer
            if let Some(h) = handle.try_get_value().flatten() {
                h.clear();
            }

            cls.set(show_class);
            show.set(true);
        } else {
            cls.set(hide_class);

            match crate::dom::helpers::set_timeout_with_handle(
                move || show.set(false),
                hide_delay,
            ) {
                Ok(h) => handle.set_value(Some(h)),
                Err(js) => {
                    crate::logging::warn!(
                        "{}",
                        SetTimeoutError {
                            delay: hide_delay,
                            js
                        }
                    );
                    show.set(false);
                }
            }
        }
    });

    on_cleanup(move || {
        if let Some(Some(h)) = handle.try_get_value() {
            h.clear();
        }
        drop(eff);
    });

    view! {
        <Show when=show fallback=|| ()>
            <div class=cls>{children()}</div>
        </Show>
    }
}
