use crate::{into_view::IntoView, show::ViewSource};
use halyard_macro::component;
use halyard_reactive_graph::{
    owner::Owner,
    signal::{ArcRwSignal, ReadSignal},
    traits::Set,
};
use halyard_tachys::{
    reactive_graph::OwnedView,
    view::keyed::{keyed, SerializableKey},
};
use std::hash::Hash;

/// The owner of a `<For/>`: the current owner or, if the list is created outside any owner,
/// a new root owner that lives as long as the list does (after a warning).
fn list_owner(component: &str) -> Owner {
    Owner::current().unwrap_or_else(|| {
        crate::logging::warn!(
            "[halyard] <{component}/> was created outside any reactive owner; its \
             rows are owned by a new root owner that lives as long as the list. \
             Create views inside `mount_to`, `hydrate_body` or `Owner::with` so \
             that they are disposed with the rest of the application."
        );
        Owner::new()
    })
}

/// Iterates over children and displays them, keyed by the `key` function given.
///
/// This is much more efficient than naively iterating over nodes with `.iter().map(|n| view! { ... })...`,
/// as it avoids re-creating DOM nodes that are not being changed.
///
/// ```
/// # use halyard::prelude::*;
///
/// #[derive(Copy, Clone, Debug, PartialEq, Eq)]
/// struct Counter {
///   id: usize,
///   count: RwSignal<i32>
/// }
///
/// #[component]
/// fn Counters() -> impl IntoView {
///   let (counters, set_counters) = create_signal::<Vec<Counter>>(vec![]);
///
///   view! {
///     <div>
///       <For
///         // a function that returns the items we're iterating over; a signal is fine
///         each=counters
///         // a unique key for each item
///         key=|counter| counter.id
///         // renders each item to a view
///         children=move |counter: Counter| {
///           view! {
///             <button>"Value: " {move || counter.count.try_get().unwrap()}</button>
///           }
///         }
///       />
///     </div>
///   }
/// }
/// ```
///
/// For convenience, you can also choose to write template code directly in the `<For>`
/// component, using the `let` syntax:
///
/// ```
/// # use halyard::prelude::*;
///
/// # #[derive(Copy, Clone, Debug, PartialEq, Eq)]
/// # struct Counter {
/// #   id: usize,
/// #   count: RwSignal<i32>
/// # }
/// #
/// # #[component]
/// # fn Counters() -> impl IntoView {
/// #   let (counters, set_counters) = create_signal::<Vec<Counter>>(vec![]);
/// #
///   view! {
///     <div>
///         <For
///           each=counters
///           key=|counter| counter.id
///           let(counter)
///         >
///             <button>"Value: " {move || counter.count.try_get().unwrap()}</button>
///         </For>
///     </div>
///   }
/// # }
/// ```
///
/// The `let` syntax also supports destructuring the pattern of your data.
/// `let((one, two))` in the case of tuples, and `let(Struct { field_one, field_two })`
/// in the case of structs.
///
/// ```
/// # use halyard::prelude::*;
///
/// # #[derive(Copy, Clone, Debug, PartialEq, Eq)]
/// # struct Counter {
/// #   id: usize,
/// #   count: RwSignal<i32>
/// # }
/// #
/// # #[component]
/// # fn Counters() -> impl IntoView {
/// #   let (counters, set_counters) = create_signal::<Vec<Counter>>(vec![]);
/// #
///   view! {
///     <div>
///         <For
///           each=counters
///           key=|counter| counter.id
///           let(Counter { id, count })
///         >
///             <button>"Value: " {count}</button>
///         </For>
///     </div>
///   }
/// # }
/// ```
#[cfg_attr(feature = "tracing", tracing::instrument(level = "trace", skip_all))]
#[component]
pub fn For<IF, I, T, EF, N, KF, K>(
    /// Items over which the component should iterate.
    /// The items: a closure that returns them, or a signal of them. A signal whose value
    /// is gone renders no rows.
    each: IF,
    /// A key function that will be applied to each item.
    key: KF,
    /// A function that takes the item, and returns the view that will be displayed for each item.
    children: EF,
) -> impl IntoView
where
    IF: ViewSource<Value = I> + Send + 'static,
    I: IntoIterator<Item = T> + Send + 'static,
    EF: Fn(T) -> N + Send + Clone + 'static,
    N: IntoView + 'static,
    KF: Fn(&T) -> K + Send + Clone + 'static,
    K: Eq + Hash + SerializableKey + 'static,
    T: Send + 'static,
{
    // this takes the owner of the For itself
    // this will end up with N + 1 children
    // 1) the effect for the `move || keyed(...)` updates
    // 2) an owner for each child
    //
    // this means
    // a) the reactive owner for each row will not be cleared when the whole list updates
    // b) context provided in each row will not wipe out the others
    let parent = list_owner("For");
    let children = move |_, child| {
        let owner = parent.with(Owner::new);
        let view = owner.with(|| children(child));
        (drop, OwnedView::new_with_owner(view, owner))
    };
    // a signal whose value is gone renders no rows
    move || {
        each.read_source()
            .map(|items| keyed(items, key.clone(), children.clone()))
    }
}

/// Iterates over children and displays them, keyed by the `key` function given.
///
/// Compared with For, it has an additional index parameter, which can be used to obtain the current index in real time.
///
/// This is much more efficient than naively iterating over nodes with `.iter().map(|n| view! { ... })...`,
/// as it avoids re-creating DOM nodes that are not being changed.
///
/// ```
/// # use halyard::prelude::*;
///
/// #[derive(Copy, Clone, Debug, PartialEq, Eq)]
/// struct Counter {
///   id: usize,
///   count: RwSignal<i32>
/// }
///
/// #[component]
/// fn Counters() -> impl IntoView {
///   let (counters, set_counters) = create_signal::<Vec<Counter>>(vec![]);
///
///   view! {
///     <div>
///       <ForEnumerate
///         // a function that returns the items we're iterating over; a signal is fine
///         each=counters
///         // a unique key for each item
///         key=|counter| counter.id
///         // renders each item to a view
///         children={move |index: ReadSignal<usize>, counter: Counter| {
///           view! {
///             <button>{index} ". Value: " {move || counter.count.try_get().unwrap()}</button>
///           }
///         }}
///       />
///     </div>
///   }
/// }
/// ```
#[cfg_attr(feature = "tracing", tracing::instrument(level = "trace", skip_all))]
#[component]
pub fn ForEnumerate<IF, I, T, EF, N, KF, K>(
    /// Items over which the component should iterate.
    /// The items: a closure that returns them, or a signal of them. A signal whose value
    /// is gone renders no rows.
    each: IF,
    /// A key function that will be applied to each item.
    key: KF,
    /// A function that takes the index and the item, and returns the view that will be displayed for each item.
    children: EF,
) -> impl IntoView
where
    IF: ViewSource<Value = I> + Send + 'static,
    I: IntoIterator<Item = T> + Send + 'static,
    EF: Fn(ReadSignal<usize>, T) -> N + Send + Clone + 'static,
    N: IntoView + 'static,
    KF: Fn(&T) -> K + Send + Clone + 'static,
    K: Eq + Hash + SerializableKey + 'static,
    T: Send + 'static,
{
    // this takes the owner of the For itself
    // this will end up with N + 1 children
    // 1) the effect for the `move || keyed(...)` updates
    // 2) an owner for each child
    //
    // this means
    // a) the reactive owner for each row will not be cleared when the whole list updates
    // b) context provided in each row will not wipe out the others
    let parent = list_owner("ForEnumerate");
    let children = move |index, child| {
        let owner = parent.with(Owner::new);
        let (index, set_index) = ArcRwSignal::new(index).split();
        let view = owner.with(|| children(index.into(), child));
        (
            move |index| set_index.set(index),
            OwnedView::new_with_owner(view, owner),
        )
    };
    // a signal whose value is gone renders no rows
    move || {
        each.read_source()
            .map(|items| keyed(items, key.clone(), children.clone()))
    }
}

/*
#[cfg(test)]
mod tests {
    use crate::prelude::*;
    use halyard_macro::view;
    use halyard_tachys::{html::element::HtmlElement, prelude::ElementChild};

    #[test]
    fn creates_list() {
        Owner::new().with(|| {
            let values = RwSignal::new(vec![1, 2, 3, 4, 5]);
            let list: View<HtmlElement<_, _, _>> = view! {
                <ol>
                    <For each=move || values.get() key=|i| *i let:i>
                        <li>{i}</li>
                    </For>
                </ol>
            };
            assert_eq!(
                list.to_html(),
                "<ol><li>1</li><li>2</li><li>3</li><li>4</li><li>5</li><!></\
                 ol>"
            );
        });
    }

    #[test]
    fn creates_list_enumerate() {
        Owner::new().with(|| {
            let values = RwSignal::new(vec![1, 2, 3, 4, 5]);
            let list: View<HtmlElement<_, _, _>> = view! {
                <ol>
                    <ForEnumerate each=move || values.get() key=|i| *i let(index, i)>
                        <li>{move || index.get()}"-"{i}</li>
                    </ForEnumerate>
                </ol>
            };
            assert_eq!(
                list.to_html(),
                "<ol><li>0<!>-<!>1</li><li>1<!>-<!>2</li><li>2<!>-<!>3</li><li>3\
                <!>-<!>4</li><li>4<!>-<!>5</li><!></ol>"
            );

            let list: View<HtmlElement<_, _, _>> = view! {
                <ol>
                    <ForEnumerate each=move || values.get() key=|i| *i let(index, i)>
                        <li>{move || index.get()}"-"{i}</li>
                    </ForEnumerate>
                </ol>
            };
            values.set(vec![5, 4, 1, 2, 3]);
            assert_eq!(
                list.to_html(),
                "<ol><li>0<!>-<!>5</li><li>1<!>-<!>4</li><li>2<!>-<!>1</li><li>3\
                <!>-<!>2</li><li>4<!>-<!>3</li><!></ol>"
            );
        });
    }
}
 */
