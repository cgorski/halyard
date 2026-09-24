use super::{
    add_attr::AddAnyAttr,
    list_diff::{reconcile, report_once, FxIndexSet, ListError, ListOps},
    MarkBranch, Mountable, Position, PositionState, Render, RenderHtml,
};
use crate::{
    html::attribute::{any_attribute::AnyAttribute, Attribute},
    hydration::Cursor,
    renderer::{CastFrom, Rndr},
    ssr::StreamBuilder,
};
use std::{hash::Hash, sync::atomic::AtomicBool};

/// Creates a keyed list of views.
pub fn keyed<T, I, K, KF, VF, VFS, V>(
    items: I,
    key_fn: KF,
    view_fn: VF,
) -> Keyed<T, I, K, KF, VF, VFS, V>
where
    I: IntoIterator<Item = T>,
    K: Eq + Hash + SerializableKey + 'static,
    KF: Fn(&T) -> K,
    V: Render,
    VF: Fn(usize, T) -> (VFS, V),
    VFS: Fn(usize),
{
    Keyed {
        #[cfg(not(feature = "ssr"))]
        items: Some(items),
        #[cfg(feature = "ssr")]
        items: None,
        #[cfg(feature = "ssr")]
        ssr_items: items
            .into_iter()
            .enumerate()
            .map(|(i, t)| {
                let key = if cfg!(feature = "islands") {
                    let key = (key_fn)(&t);
                    key.ser_key()
                } else {
                    String::new()
                };
                let (_, view) = (view_fn)(i, t);
                (key, view)
            })
            .collect::<Vec<_>>(),
        key_fn,
        view_fn,
    }
}

/// A keyed list of views.
pub struct Keyed<T, I, K, KF, VF, VFS, V>
where
    I: IntoIterator<Item = T>,
    K: Eq + Hash + 'static,
    KF: Fn(&T) -> K,
    VF: Fn(usize, T) -> (VFS, V),
    VFS: Fn(usize),
{
    items: Option<I>,
    #[cfg(feature = "ssr")]
    ssr_items: Vec<(String, V)>,
    key_fn: KF,
    view_fn: VF,
}

/// By default, keys used in for keyed iteration do not need to be serializable.
///
/// However, for some scenarios (like the “islands routing” mode that mixes server-side
/// rendering with client-side navigation) it is useful to have serializable keys.
///
/// When the `islands` feature is not enabled, this trait is implemented by all types.
///
/// When the `islands` features is enabled, this is automatically implemented for all types
/// that implement [`Serialize`](serde::Serialize), and can be manually implemented otherwise.
pub trait SerializableKey {
    /// Serializes the key to a unique string.
    ///
    /// The string can have any value, as long as it is idempotent (i.e., serializing the same key
    /// multiple times will give the same value).
    fn ser_key(&self) -> String;
}

#[cfg(not(feature = "islands"))]
impl<T> SerializableKey for T {
    /// Only `islands` uses serialized keys, and halyard calls this only with that feature.
    /// Without it, this logs once and returns an empty key.
    fn ser_key(&self) -> String {
        static REPORTED: AtomicBool = AtomicBool::new(false);
        report_once(&REPORTED, &ListError::SerializableKeyWithoutIslands);
        String::new()
    }
}
#[cfg(feature = "islands")]
impl<T: serde::Serialize> SerializableKey for T {
    /// A key that cannot be serialized (a map with non-string keys, a failing `Serialize`)
    /// is logged once and serialized as an empty key.
    fn ser_key(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|error| {
            static REPORTED: AtomicBool = AtomicBool::new(false);
            report_once(&REPORTED, &ListError::KeyNotSerializable(error));
            String::new()
        })
    }
}

/// Retained view state for a keyed list.
pub struct KeyedState<K, VFS, V>
where
    K: Eq + Hash + 'static,
    VFS: Fn(usize),
    V: Render,
{
    parent: Option<crate::renderer::types::Element>,
    marker: crate::renderer::types::Placeholder,
    hashed_items: FxIndexSet<K>,
    rendered_items: Vec<Option<(VFS, V::State)>>,
}

impl<T, I, K, KF, VF, VFS, V> Render for Keyed<T, I, K, KF, VF, VFS, V>
where
    I: IntoIterator<Item = T>,
    K: Eq + Hash + SerializableKey + 'static,
    KF: Fn(&T) -> K,
    V: Render,
    VF: Fn(usize, T) -> (VFS, V),
    VFS: Fn(usize),
{
    type State = KeyedState<K, VFS, V>;

    fn build(self) -> Self::State {
        let items = self.items.into_iter().flatten();
        let (capacity, _) = items.size_hint();
        let mut hashed_items =
            FxIndexSet::with_capacity_and_hasher(capacity, Default::default());
        let mut rendered_items = Vec::with_capacity(capacity);
        for (index, item) in items.enumerate() {
            hashed_items.insert((self.key_fn)(&item));
            let (set_index, view) = (self.view_fn)(index, item);
            rendered_items.push(Some((set_index, view.build())));
        }
        KeyedState {
            parent: None,
            marker: Rndr::create_placeholder(),
            hashed_items,
            rendered_items,
        }
    }

    fn rebuild(self, state: &mut Self::State) {
        let KeyedState {
            parent,
            marker,
            hashed_items,
            ref mut rendered_items,
        } = state;
        let new_items = self.items.into_iter().flatten();
        let (capacity, _) = new_items.size_hint();
        let mut new_hashed_items =
            FxIndexSet::with_capacity_and_hasher(capacity, Default::default());

        let mut items = Vec::new();
        for item in new_items {
            new_hashed_items.insert((self.key_fn)(&item));
            items.push(Some(item));
        }

        reconcile(
            hashed_items,
            &new_hashed_items,
            rendered_items,
            items,
            &mut DomRows {
                parent: parent.as_ref(),
                marker,
                view_fn: &self.view_fn,
            },
        );

        *hashed_items = new_hashed_items;
    }
}

impl<T, I, K, KF, VF, VFS, V> AddAnyAttr for Keyed<T, I, K, KF, VF, VFS, V>
where
    I: IntoIterator<Item = T> + Send + 'static,
    K: Eq + Hash + SerializableKey + 'static,
    KF: Fn(&T) -> K + Send + 'static,
    V: RenderHtml,
    V: 'static,
    VF: Fn(usize, T) -> (VFS, V) + Send + 'static,
    VFS: Fn(usize) + 'static,
    T: 'static,
{
    type Output<SomeNewAttr: Attribute> = Keyed<
        T,
        I,
        K,
        KF,
        Box<
            dyn Fn(
                    usize,
                    T,
                ) -> (
                    VFS,
                    <V as AddAnyAttr>::Output<SomeNewAttr::CloneableOwned>,
                ) + Send,
        >,
        VFS,
        V::Output<SomeNewAttr::CloneableOwned>,
    >;

    fn add_any_attr<NewAttr: Attribute>(
        self,
        attr: NewAttr,
    ) -> Self::Output<NewAttr>
    where
        Self::Output<NewAttr>: RenderHtml,
    {
        let Keyed {
            items,
            #[cfg(feature = "ssr")]
            ssr_items,
            key_fn,
            view_fn,
        } = self;
        let attr = attr.into_cloneable_owned();
        Keyed {
            items,
            key_fn,
            #[cfg(feature = "ssr")]
            ssr_items: ssr_items
                .into_iter()
                .map(|(k, v)| (k, v.add_any_attr(attr.clone())))
                .collect(),
            view_fn: Box::new(move |index, item| {
                let (index, view) = view_fn(index, item);
                (index, view.add_any_attr(attr.clone()))
            }),
        }
    }
}

impl<T, I, K, KF, VF, VFS, V> RenderHtml for Keyed<T, I, K, KF, VF, VFS, V>
where
    I: IntoIterator<Item = T> + Send + 'static,
    K: Eq + Hash + SerializableKey + 'static,
    KF: Fn(&T) -> K + Send + 'static,
    V: RenderHtml + 'static,
    VF: Fn(usize, T) -> (VFS, V) + Send + 'static,
    VFS: Fn(usize) + 'static,
    T: 'static,
{
    type AsyncOutput = Vec<V::AsyncOutput>; // TODO
    type Owned = Self;

    const MIN_LENGTH: usize = 0;

    fn dry_resolve(&mut self) {
        #[cfg(feature = "ssr")]
        for view in &mut self.ssr_items {
            view.dry_resolve();
        }
    }

    async fn resolve(self) -> Self::AsyncOutput {
        #[cfg(feature = "ssr")]
        {
            futures::future::join_all(
                self.ssr_items.into_iter().map(|(_, view)| view.resolve()),
            )
            .await
            .into_iter()
            .collect::<Vec<_>>()
        }
        #[cfg(not(feature = "ssr"))]
        {
            futures::future::join_all(
                self.items.into_iter().flatten().enumerate().map(
                    |(index, item)| {
                        let (_, view) = (self.view_fn)(index, item);
                        view.resolve()
                    },
                ),
            )
            .await
            .into_iter()
            .collect::<Vec<_>>()
        }
    }

    #[allow(unused)]
    fn to_html_with_buf(
        self,
        buf: &mut String,
        position: &mut Position,
        escape: bool,
        mark_branches: bool,
        extra_attrs: Vec<AnyAttribute>,
    ) {
        if mark_branches && escape {
            buf.open_branch("for");
        }

        #[cfg(feature = "ssr")]
        for (key, item) in self.ssr_items {
            let branch_name =
                (mark_branches && escape).then(|| format!("item-{key}"));
            if let Some(branch_name) = &branch_name {
                buf.open_branch(branch_name);
            }
            item.to_html_with_buf(
                buf,
                position,
                escape,
                mark_branches,
                extra_attrs.clone(),
            );
            if let Some(branch_name) = &branch_name {
                buf.close_branch(branch_name);
            }
            *position = Position::NextChild;
        }
        if mark_branches && escape {
            buf.close_branch("for");
        }
        buf.push_str("<!>");
    }

    #[allow(unused)]
    fn to_html_async_with_buf<const OUT_OF_ORDER: bool>(
        self,
        buf: &mut StreamBuilder,
        position: &mut Position,
        escape: bool,
        mark_branches: bool,
        extra_attrs: Vec<AnyAttribute>,
    ) {
        if mark_branches && escape {
            buf.open_branch("for");
        }

        #[cfg(feature = "ssr")]
        for (key, item) in self.ssr_items {
            let branch_name =
                (mark_branches && escape).then(|| format!("item-{key}"));
            if let Some(branch_name) = &branch_name {
                buf.open_branch(branch_name);
            }
            item.to_html_async_with_buf::<OUT_OF_ORDER>(
                buf,
                position,
                escape,
                mark_branches,
                extra_attrs.clone(),
            );
            if let Some(branch_name) = &branch_name {
                buf.close_branch(branch_name);
            }
            *position = Position::NextChild;
        }

        if mark_branches && escape {
            buf.close_branch("for");
        }
        buf.push_sync("<!>");
    }

    fn hydrate<const FROM_SERVER: bool>(
        self,
        cursor: &Cursor,
        position: &PositionState,
    ) -> Self::State {
        let parent = cursor_parent(cursor, position);

        // build list
        let items = self.items.into_iter().flatten();
        let (capacity, _) = items.size_hint();
        let mut hashed_items =
            FxIndexSet::with_capacity_and_hasher(capacity, Default::default());
        let mut rendered_items = Vec::with_capacity(capacity);
        for (index, item) in items.enumerate() {
            hashed_items.insert((self.key_fn)(&item));
            let (set_index, view) = (self.view_fn)(index, item);
            let item = view.hydrate::<FROM_SERVER>(cursor, position);
            rendered_items.push(Some((set_index, item)));
        }
        let marker = cursor.next_placeholder(position);
        position.set(Position::NextChild);

        KeyedState {
            parent: hydrated_parent(parent, &marker),
            marker,
            hashed_items,
            rendered_items,
        }
    }

    async fn hydrate_async(
        self,
        cursor: &Cursor,
        position: &PositionState,
    ) -> Self::State {
        let parent = cursor_parent(cursor, position);

        // build list
        let items = self.items.into_iter().flatten();
        let (capacity, _) = items.size_hint();
        let mut hashed_items =
            FxIndexSet::with_capacity_and_hasher(capacity, Default::default());
        let mut rendered_items = Vec::with_capacity(capacity);
        for (index, item) in items.enumerate() {
            hashed_items.insert((self.key_fn)(&item));
            let (set_index, view) = (self.view_fn)(index, item);
            let item = view.hydrate_async(cursor, position).await;
            rendered_items.push(Some((set_index, item)));
        }
        let marker = cursor.next_placeholder(position);
        position.set(Position::NextChild);

        KeyedState {
            parent: hydrated_parent(parent, &marker),
            marker,
            hashed_items,
            rendered_items,
        }
    }

    fn into_owned(self) -> Self::Owned {
        self
    }
}

impl<K, VFS, V> Mountable for KeyedState<K, VFS, V>
where
    K: Eq + Hash + 'static,
    VFS: Fn(usize),
    V: Render,
{
    fn mount(
        &mut self,
        parent: &crate::renderer::types::Element,
        marker: Option<&crate::renderer::types::Node>,
    ) {
        self.parent = Some(parent.clone());
        for (_, item) in self.rendered_items.iter_mut().flatten() {
            item.mount(parent, marker);
        }
        self.marker.mount(parent, marker);
    }

    fn unmount(&mut self) {
        for (_, item) in self.rendered_items.iter_mut().flatten() {
            item.unmount();
        }
        self.marker.unmount();
    }

    fn insert_before_this(&self, child: &mut dyn Mountable) -> bool {
        self.rendered_items
            .first()
            .map(|item| {
                if let Some((_, item)) = item {
                    item.insert_before_this(child)
                } else {
                    false
                }
            })
            .unwrap_or_else(|| self.marker.insert_before_this(child))
    }

    fn elements(&self) -> Vec<crate::renderer::types::Element> {
        self.rendered_items
            .iter()
            .flatten()
            .flat_map(|item| item.1.elements())
            .collect()
    }
}

/// The node that a list being hydrated at the cursor is in, if the cursor is on it (the list
/// is its first child) or on the list's previous sibling.
fn cursor_parent(
    cursor: &Cursor,
    position: &PositionState,
) -> Option<crate::renderer::types::Node> {
    let current = cursor.current();
    if position.get() == Position::FirstChild {
        Some(current)
    } else {
        Rndr::get_parent(&current)
    }
}

/// The element a hydrated list is in: the node found at the cursor or, if that is not an
/// element, the parent of the list's marker.
///
/// With neither, the list has no element around it, and rows it adds later are not mounted
/// (logged once). After a hydration mismatch that is expected and not logged: the cursor
/// gives detached nodes, the mismatch is logged already, and the hydrated tree is thrown
/// away for a client render.
fn hydrated_parent(
    found: Option<crate::renderer::types::Node>,
    marker: &crate::renderer::types::Placeholder,
) -> Option<crate::renderer::types::Element> {
    let parent = found
        .and_then(crate::renderer::types::Element::cast_from)
        .or_else(|| {
            Rndr::get_parent(marker.as_ref())
                .and_then(crate::renderer::types::Element::cast_from)
        });
    if parent.is_none() && !crate::hydration::hydration_failed() {
        static REPORTED: AtomicBool = AtomicBool::new(false);
        report_once(&REPORTED, &ListError::NoParent);
    }
    parent
}

/// The rows of a keyed list on the page, for the diff: `(set_index, state)` pairs, mounted
/// in `parent` before the list's `marker` (not at all while the list has no parent).
struct DomRows<'a, T, VFS, V> {
    parent: Option<&'a crate::renderer::types::Element>,
    marker: &'a crate::renderer::types::Placeholder,
    view_fn: &'a dyn Fn(usize, T) -> (VFS, V),
}

impl<T, VFS, V> ListOps for DomRows<'_, T, VFS, V>
where
    VFS: Fn(usize),
    V: Render,
{
    type Item = T;
    type Row = (VFS, V::State);

    fn build(&mut self, index: usize, item: T) -> Self::Row {
        let (set_index, view) = (self.view_fn)(index, item);
        (set_index, view.build())
    }

    fn unmount(&mut self, (_, state): &mut Self::Row) {
        state.unmount();
    }

    fn mount(
        &mut self,
        (_, state): &mut Self::Row,
        before: Option<&Self::Row>,
    ) {
        let Some(parent) = self.parent else {
            return;
        };
        let marker = Some(self.marker.as_ref());
        match before {
            Some((_, next)) => {
                next.insert_before_this_or_marker(parent, state, marker)
            }
            None => {
                state.try_mount(parent, marker);
            }
        }
    }

    fn set_index(&mut self, (set_index, _): &Self::Row, index: usize) {
        set_index(index);
    }
}

#[cfg(test)]
mod tests {
    #[cfg(not(feature = "islands"))]
    use super::SerializableKey;
    #[cfg(feature = "ssr")]
    use super::{keyed, Keyed};
    #[cfg(feature = "ssr")]
    use crate::{
        ssr::StreamBuilder,
        view::{Position, RenderHtml},
    };

    /// halyard never calls `ser_key` without `islands`, but it is public: a call returns
    /// an empty key instead of panicking.
    #[cfg(not(feature = "islands"))]
    #[test]
    fn ser_key_without_islands_is_empty() {
        assert_eq!(7u8.ser_key(), "");
    }

    #[cfg(feature = "ssr")]
    fn two_items() -> Keyed<
        u8,
        Vec<u8>,
        u8,
        impl Fn(&u8) -> u8,
        impl Fn(usize, u8) -> (fn(usize), String),
        fn(usize),
        String,
    > {
        fn set_index(_: usize) {}
        keyed(
            vec![1, 2],
            |key: &u8| *key,
            |_, item: u8| (set_index as fn(usize), item.to_string()),
        )
    }

    #[cfg(feature = "ssr")]
    const MARKED: &str =
        "<!--bo-for--><!--bo-item--->1<!--bc-item---><!--bo-item--->2\
                          <!--bc-item---><!--bc-for--><!>";

    #[cfg(feature = "ssr")]
    #[test]
    fn server_html_marks_each_item_when_asked() {
        let mut buf = String::new();
        two_items().to_html_with_buf(
            &mut buf,
            &mut Position::FirstChild,
            true,
            true,
            vec![],
        );
        assert_eq!(buf, MARKED);

        let mut buf = String::new();
        two_items().to_html_with_buf(
            &mut buf,
            &mut Position::FirstChild,
            true,
            false,
            vec![],
        );
        assert_eq!(buf, "12<!>");
    }

    #[cfg(feature = "ssr")]
    #[test]
    fn streamed_html_marks_each_item_when_asked() {
        let mut buf = StreamBuilder::new(None);
        two_items().to_html_async_with_buf::<false>(
            &mut buf,
            &mut Position::FirstChild,
            true,
            true,
            vec![],
        );
        assert_eq!(buf.sync_buf, MARKED);
    }
}
