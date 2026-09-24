#![allow(clippy::type_complexity)]
#[cfg(feature = "ssr")]
use super::MarkBranch;
use super::{
    add_attr::AddAnyAttr, Mountable, Position, PositionState, Render,
    RenderHtml,
};
use crate::{
    erased::{Erased, ErasedLocal},
    html::attribute::{
        any_attribute::{AnyAttribute, AnyAttributeState, IntoAnyAttribute},
        Attribute,
    },
    hydration::Cursor,
    renderer::Rndr,
    ssr::StreamBuilder,
    view_error::{report_once, ViewError},
};
use futures::future::{join, join_all};
use std::{any::TypeId, fmt::Debug, sync::atomic::AtomicBool};
#[cfg(any(feature = "ssr", feature = "hydrate"))]
use std::{future::Future, pin::Pin};

/// A type-erased view. This can be used if control flow requires that multiple different types of
/// view must be received, and it is either impossible or too cumbersome to use the `EitherOf___`
/// enums.
///
/// It can also be used to create recursive components, which otherwise cannot return themselves
/// due to the static typing of the view tree.
///
/// Generally speaking, using `AnyView` restricts the amount of information available to the
/// compiler and should be limited to situations in which it is necessary to preserve the maximum
/// amount of type information possible.
pub struct AnyView {
    type_id: TypeId,
    value: Erased,
    build: fn(Erased) -> AnyViewState,
    rebuild: fn(Erased, &mut AnyViewState),
    // The fields below are cfg-gated so they will not be included in WASM bundles if not needed.
    // Ordinarily, the compiler can simply omit this dead code because the methods are not called.
    // With this type-erased wrapper, however, the compiler is not *always* able to correctly
    // eliminate that code.
    #[cfg(feature = "ssr")]
    html_len: usize,
    #[cfg(feature = "ssr")]
    to_html:
        fn(Erased, &mut String, &mut Position, bool, bool, Vec<AnyAttribute>),
    #[cfg(feature = "ssr")]
    to_html_async: fn(
        Erased,
        &mut StreamBuilder,
        &mut Position,
        bool,
        bool,
        Vec<AnyAttribute>,
    ),
    #[cfg(feature = "ssr")]
    to_html_async_ooo: fn(
        Erased,
        &mut StreamBuilder,
        &mut Position,
        bool,
        bool,
        Vec<AnyAttribute>,
    ),
    #[cfg(feature = "ssr")]
    #[allow(clippy::type_complexity)]
    resolve: fn(Erased) -> Pin<Box<dyn Future<Output = AnyView> + Send>>,
    #[cfg(feature = "ssr")]
    dry_resolve: fn(&mut Erased),
    #[cfg(feature = "hydrate")]
    #[allow(clippy::type_complexity)]
    hydrate_from_server: fn(Erased, &Cursor, &PositionState) -> AnyViewState,
    #[cfg(feature = "hydrate")]
    #[allow(clippy::type_complexity)]
    hydrate_async: fn(
        Erased,
        &Cursor,
        &PositionState,
    ) -> Pin<Box<dyn Future<Output = AnyViewState>>>,
}

impl AnyView {
    #[doc(hidden)]
    pub fn as_type_id(&self) -> TypeId {
        self.type_id
    }
}

impl Debug for AnyView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnyView")
            .field("type_id", &self.type_id)
            .finish_non_exhaustive()
    }
}
/// Retained view state for [`AnyView`].
pub struct AnyViewState {
    type_id: TypeId,
    state: ErasedLocal,
    unmount: fn(&mut ErasedLocal),
    mount: fn(
        &mut ErasedLocal,
        parent: &crate::renderer::types::Element,
        marker: Option<&crate::renderer::types::Node>,
    ),
    insert_before_this: fn(&ErasedLocal, child: &mut dyn Mountable) -> bool,
    elements: fn(&ErasedLocal) -> Vec<crate::renderer::types::Element>,
    placeholder: Option<crate::renderer::types::Placeholder>,
}

impl Debug for AnyViewState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnyViewState")
            .field("type_id", &self.type_id)
            .field("state", &"")
            .field("unmount", &self.unmount)
            .field("mount", &self.mount)
            .field("insert_before_this", &self.insert_before_this)
            .finish()
    }
}

/// Allows converting some view into [`AnyView`].
pub trait IntoAny {
    /// Converts the view into a type-erased [`AnyView`].
    fn into_any(self) -> AnyView;
}

/// A more general version of [`IntoAny`] that allows into [`AnyView`],
/// but also erasing other types that don't implement [`RenderHtml`] like routing.
pub trait IntoMaybeErased {
    /// The type of the output.
    type Output: IntoMaybeErased;

    /// Converts the view into a type-erased view if in erased mode.
    fn into_maybe_erased(self) -> Self::Output;
}

impl<T> IntoMaybeErased for T
where
    T: RenderHtml,
{
    #[cfg(not(erase_components))]
    type Output = Self;

    #[cfg(erase_components)]
    type Output = AnyView;

    fn into_maybe_erased(self) -> Self::Output {
        #[cfg(not(erase_components))]
        {
            self
        }
        #[cfg(erase_components)]
        {
            self.into_owned().into_any()
        }
    }
}

/// An `AnyView` (or its state) keeps its type-erased value next to functions made for the
/// value's type, both by `into_any`, so the value always has that type. If it had not, the
/// functions do nothing (logged once) instead of reading it as the wrong type.
fn type_mismatch(instead: &'static str) {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    report_once(
        &REPORTED,
        &ViewError::ErasedTypeMismatch {
            what: "an AnyView",
            instead,
        },
    );
}

/// The state of an empty view, for an `AnyView` whose value has another type than its
/// functions (see [`type_mismatch`]): it shows nothing, and a later update replaces it.
fn mismatched_state() -> AnyViewState {
    type_mismatch("it renders nothing");
    ().into_any().build()
}

fn mount_any<T>(
    state: &mut ErasedLocal,
    parent: &crate::renderer::types::Element,
    marker: Option<&crate::renderer::types::Node>,
) where
    T: Render,
    T::State: 'static,
{
    match state.get_mut::<T::State>() {
        Some(state) => state.mount(parent, marker),
        None => type_mismatch("it is not mounted"),
    }
}

fn unmount_any<T>(state: &mut ErasedLocal)
where
    T: Render,
    T::State: 'static,
{
    match state.get_mut::<T::State>() {
        Some(state) => state.unmount(),
        None => type_mismatch("it is not unmounted"),
    }
}

fn insert_before_this<T>(state: &ErasedLocal, child: &mut dyn Mountable) -> bool
where
    T: Render,
    T::State: 'static,
{
    match state.get_ref::<T::State>() {
        Some(state) => state.insert_before_this(child),
        None => {
            type_mismatch("nothing is inserted before it");
            false
        }
    }
}

fn elements<T>(state: &ErasedLocal) -> Vec<crate::renderer::types::Element>
where
    T: Render,
    T::State: 'static,
{
    match state.get_ref::<T::State>() {
        Some(state) => state.elements(),
        None => {
            type_mismatch("it has no elements");
            Vec::new()
        }
    }
}

impl<T> IntoAny for T
where
    T: Send,
    T: RenderHtml,
{
    fn into_any(self) -> AnyView {
        #[cfg(feature = "ssr")]
        fn dry_resolve<T: RenderHtml + 'static>(value: &mut Erased) {
            match value.get_mut::<T>() {
                Some(value) => value.dry_resolve(),
                None => type_mismatch("it is not resolved"),
            }
        }

        #[cfg(feature = "ssr")]
        fn resolve<T: RenderHtml + 'static>(
            value: Erased,
        ) -> Pin<Box<dyn Future<Output = AnyView> + Send>> {
            use futures::FutureExt;

            async move {
                match value.into_inner::<T>() {
                    Some(value) => value.resolve().await.into_any(),
                    None => {
                        type_mismatch("it resolves to an empty view");
                        ().into_any()
                    }
                }
            }
            .boxed()
        }

        #[cfg(feature = "ssr")]
        fn to_html<T: RenderHtml + 'static>(
            value: Erased,
            buf: &mut String,
            position: &mut Position,
            escape: bool,
            mark_branches: bool,
            extra_attrs: Vec<AnyAttribute>,
        ) {
            let Some(value) = value.into_inner::<T>() else {
                return type_mismatch("it renders nothing");
            };
            value.to_html_with_buf(
                buf,
                position,
                escape,
                mark_branches,
                extra_attrs,
            );
            if !T::EXISTS {
                buf.push_str("<!--<() />-->");
            }
        }

        #[cfg(feature = "ssr")]
        fn to_html_async<T: RenderHtml + 'static>(
            value: Erased,
            buf: &mut StreamBuilder,
            position: &mut Position,
            escape: bool,
            mark_branches: bool,
            extra_attrs: Vec<AnyAttribute>,
        ) {
            let Some(value) = value.into_inner::<T>() else {
                return type_mismatch("it renders nothing");
            };
            value.to_html_async_with_buf::<false>(
                buf,
                position,
                escape,
                mark_branches,
                extra_attrs,
            );
            if !T::EXISTS {
                buf.push_sync("<!--<() />-->");
            }
        }

        #[cfg(feature = "ssr")]
        fn to_html_async_ooo<T: RenderHtml + 'static>(
            value: Erased,
            buf: &mut StreamBuilder,
            position: &mut Position,
            escape: bool,
            mark_branches: bool,
            extra_attrs: Vec<AnyAttribute>,
        ) {
            let Some(value) = value.into_inner::<T>() else {
                return type_mismatch("it renders nothing");
            };
            value.to_html_async_with_buf::<true>(
                buf,
                position,
                escape,
                mark_branches,
                extra_attrs,
            );
            if !T::EXISTS {
                buf.push_sync("<!--<() />-->");
            }
        }

        fn build<T: RenderHtml + 'static>(value: Erased) -> AnyViewState {
            let Some(value) = value.into_inner::<T>() else {
                return mismatched_state();
            };
            let state = ErasedLocal::new(value.build());
            let placeholder = (!T::EXISTS).then(Rndr::create_placeholder);
            AnyViewState {
                type_id: TypeId::of::<T>(),
                state,
                mount: mount_any::<T>,
                unmount: unmount_any::<T>,
                insert_before_this: insert_before_this::<T>,
                elements: elements::<T>,
                placeholder,
            }
        }

        #[cfg(feature = "hydrate")]
        fn hydrate_from_server<T: RenderHtml + 'static>(
            value: Erased,
            cursor: &Cursor,
            position: &PositionState,
        ) -> AnyViewState {
            let Some(value) = value.into_inner::<T>() else {
                return mismatched_state();
            };
            let state =
                ErasedLocal::new(value.hydrate::<true>(cursor, position));
            let placeholder =
                (!T::EXISTS).then(|| cursor.next_placeholder(position));
            AnyViewState {
                type_id: TypeId::of::<T>(),
                state,
                mount: mount_any::<T>,
                unmount: unmount_any::<T>,
                insert_before_this: insert_before_this::<T>,
                elements: elements::<T>,
                placeholder,
            }
        }

        #[cfg(feature = "hydrate")]
        fn hydrate_async<T: RenderHtml + 'static>(
            value: Erased,
            cursor: &Cursor,
            position: &PositionState,
        ) -> Pin<Box<dyn Future<Output = AnyViewState>>> {
            let cursor = cursor.clone();
            let position = position.clone();
            Box::pin(async move {
                let Some(value) = value.into_inner::<T>() else {
                    return mismatched_state();
                };
                let state = ErasedLocal::new(
                    value.hydrate_async(&cursor, &position).await,
                );
                let placeholder =
                    (!T::EXISTS).then(|| cursor.next_placeholder(&position));
                AnyViewState {
                    type_id: TypeId::of::<T>(),
                    state,
                    mount: mount_any::<T>,
                    unmount: unmount_any::<T>,
                    insert_before_this: insert_before_this::<T>,
                    elements: elements::<T>,
                    placeholder,
                }
            })
        }

        fn rebuild<T: RenderHtml + 'static>(
            value: Erased,
            state: &mut AnyViewState,
        ) {
            match (
                value.into_inner::<T>(),
                state.state.get_mut::<<T as Render>::State>(),
            ) {
                (Some(value), Some(state)) => value.rebuild(state),
                _ => type_mismatch("it is not updated"),
            }
        }

        let value = self.into_owned();
        AnyView {
            type_id: TypeId::of::<T::Owned>(),
            build: build::<T::Owned>,
            rebuild: rebuild::<T::Owned>,
            #[cfg(feature = "ssr")]
            resolve: resolve::<T::Owned>,
            #[cfg(feature = "ssr")]
            dry_resolve: dry_resolve::<T::Owned>,
            #[cfg(feature = "ssr")]
            html_len: value.html_len(),
            #[cfg(feature = "ssr")]
            to_html: to_html::<T::Owned>,
            #[cfg(feature = "ssr")]
            to_html_async: to_html_async::<T::Owned>,
            #[cfg(feature = "ssr")]
            to_html_async_ooo: to_html_async_ooo::<T::Owned>,
            #[cfg(feature = "hydrate")]
            hydrate_from_server: hydrate_from_server::<T::Owned>,
            #[cfg(feature = "hydrate")]
            hydrate_async: hydrate_async::<T::Owned>,
            value: Erased::new(value),
        }
    }
}

impl Render for AnyView {
    type State = AnyViewState;

    fn build(self) -> Self::State {
        (self.build)(self.value)
    }

    fn rebuild(self, state: &mut Self::State) {
        if self.type_id == state.type_id {
            (self.rebuild)(self.value, state)
        } else {
            let mut new = self.build();
            if let Some(placeholder) = &mut state.placeholder {
                placeholder.insert_before_this(&mut new);
                placeholder.unmount();
            } else {
                state.insert_before_this(&mut new);
            }
            state.unmount();
            *state = new;
        }
    }
}

impl AddAnyAttr for AnyView {
    type Output<SomeNewAttr: Attribute> = AnyViewWithAttrs;

    #[allow(unused_variables)]
    fn add_any_attr<NewAttr: Attribute>(
        self,
        attr: NewAttr,
    ) -> Self::Output<NewAttr>
    where
        Self::Output<NewAttr>: RenderHtml,
    {
        AnyViewWithAttrs {
            view: self,
            attrs: vec![attr.into_cloneable_owned().into_any_attr()],
        }
    }
}

/// Without `ssr`, an `AnyView` keeps no HTML renderer (so that the browser bundle does not
/// carry one): rendering it to HTML renders nothing, logged once.
#[cfg(not(feature = "ssr"))]
fn rendered_without_ssr() {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    report_once(
        &REPORTED,
        &ViewError::RenderedWithoutSsr { what: "an AnyView" },
    );
}

/// Hydrating an `AnyView` it cannot hydrate creates it on the client instead (logged once):
/// the view works, detached from the server's HTML.
fn build_instead_of_hydrating(
    view: AnyView,
    error: &ViewError,
) -> AnyViewState {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    report_once(&REPORTED, error);
    view.build()
}

impl RenderHtml for AnyView {
    type AsyncOutput = Self;
    type Owned = Self;

    fn dry_resolve(&mut self) {
        // without `ssr` there is nothing to resolve; rendering logs
        #[cfg(feature = "ssr")]
        {
            (self.dry_resolve)(&mut self.value)
        }
    }

    async fn resolve(self) -> Self::AsyncOutput {
        // without `ssr` the view resolves to itself; rendering logs
        #[cfg(feature = "ssr")]
        {
            (self.resolve)(self.value).await
        }
        #[cfg(not(feature = "ssr"))]
        {
            self
        }
    }

    const MIN_LENGTH: usize = 0;

    fn to_html_with_buf(
        self,
        buf: &mut String,
        position: &mut Position,
        escape: bool,
        mark_branches: bool,
        extra_attrs: Vec<AnyAttribute>,
    ) {
        #[cfg(feature = "ssr")]
        {
            let type_id = if mark_branches && escape {
                format!("{:?}", self.type_id)
            } else {
                Default::default()
            };
            if mark_branches && escape {
                buf.open_branch(&type_id);
            }
            (self.to_html)(
                self.value,
                buf,
                position,
                escape,
                mark_branches,
                extra_attrs,
            );
            if mark_branches && escape {
                buf.close_branch(&type_id);
                if *position == Position::NextChildAfterText {
                    *position = Position::NextChild;
                }
            }
        }
        #[cfg(not(feature = "ssr"))]
        {
            _ = mark_branches;
            _ = buf;
            _ = position;
            _ = escape;
            _ = extra_attrs;
            rendered_without_ssr();
        }
    }

    fn to_html_async_with_buf<const OUT_OF_ORDER: bool>(
        self,
        buf: &mut StreamBuilder,
        position: &mut Position,
        escape: bool,
        mark_branches: bool,
        extra_attrs: Vec<AnyAttribute>,
    ) where
        Self: Sized,
    {
        #[cfg(feature = "ssr")]
        if OUT_OF_ORDER {
            let type_id = if mark_branches && escape {
                format!("{:?}", self.type_id)
            } else {
                Default::default()
            };
            if mark_branches && escape {
                buf.open_branch(&type_id);
            }
            (self.to_html_async_ooo)(
                self.value,
                buf,
                position,
                escape,
                mark_branches,
                extra_attrs,
            );
            if mark_branches && escape {
                buf.close_branch(&type_id);
                if *position == Position::NextChildAfterText {
                    *position = Position::NextChild;
                }
            }
        } else {
            let type_id = if mark_branches && escape {
                format!("{:?}", self.type_id)
            } else {
                Default::default()
            };
            if mark_branches && escape {
                buf.open_branch(&type_id);
            }
            (self.to_html_async)(
                self.value,
                buf,
                position,
                escape,
                mark_branches,
                extra_attrs,
            );
            if mark_branches && escape {
                buf.close_branch(&type_id);
                if *position == Position::NextChildAfterText {
                    *position = Position::NextChild;
                }
            }
        }
        #[cfg(not(feature = "ssr"))]
        {
            _ = buf;
            _ = position;
            _ = escape;
            _ = mark_branches;
            _ = extra_attrs;
            rendered_without_ssr();
        }
    }

    fn hydrate<const FROM_SERVER: bool>(
        self,
        cursor: &Cursor,
        position: &PositionState,
    ) -> Self::State {
        #[cfg(feature = "hydrate")]
        {
            if FROM_SERVER {
                (self.hydrate_from_server)(self.value, cursor, position)
            } else {
                // `AnyView` is not `ToTemplate`, so a template has no markup for it; only
                // a hand-written `ToTemplate` type can hydrate one from a template
                build_instead_of_hydrating(
                    self,
                    &ViewError::NotInTemplate { what: "an AnyView" },
                )
            }
        }
        #[cfg(not(feature = "hydrate"))]
        {
            _ = cursor;
            _ = position;
            build_instead_of_hydrating(
                self,
                &ViewError::HydratedWithoutHydrate { what: "an AnyView" },
            )
        }
    }

    async fn hydrate_async(
        self,
        cursor: &Cursor,
        position: &PositionState,
    ) -> Self::State {
        #[cfg(feature = "hydrate")]
        {
            let state =
                (self.hydrate_async)(self.value, cursor, position).await;
            state
        }
        #[cfg(not(feature = "hydrate"))]
        {
            _ = cursor;
            _ = position;
            build_instead_of_hydrating(
                self,
                &ViewError::HydratedWithoutHydrate { what: "an AnyView" },
            )
        }
    }

    fn html_len(&self) -> usize {
        #[cfg(feature = "ssr")]
        {
            self.html_len
        }
        #[cfg(not(feature = "ssr"))]
        {
            0
        }
    }

    fn into_owned(self) -> Self::Owned {
        self
    }
}

impl Mountable for AnyViewState {
    fn unmount(&mut self) {
        (self.unmount)(&mut self.state);
        if let Some(placeholder) = &mut self.placeholder {
            placeholder.unmount();
        }
    }

    fn mount(
        &mut self,
        parent: &crate::renderer::types::Element,
        marker: Option<&crate::renderer::types::Node>,
    ) {
        (self.mount)(&mut self.state, parent, marker);
        if let Some(placeholder) = &mut self.placeholder {
            placeholder.mount(parent, marker);
        }
    }

    fn insert_before_this(&self, child: &mut dyn Mountable) -> bool {
        let before_view = (self.insert_before_this)(&self.state, child);
        if before_view {
            return true;
        }

        if let Some(placeholder) = &self.placeholder {
            placeholder.insert_before_this(child)
        } else {
            false
        }
    }

    fn elements(&self) -> Vec<crate::renderer::types::Element> {
        (self.elements)(&self.state)
    }
}

/// wip
pub struct AnyViewWithAttrs {
    view: AnyView,
    attrs: Vec<AnyAttribute>,
}

impl Render for AnyViewWithAttrs {
    type State = AnyViewWithAttrsState;

    fn build(self) -> Self::State {
        let view = self.view.build();
        let elements = view.elements();
        let attrs = self
            .attrs
            .into_iter()
            .flat_map(|attr| {
                elements.iter().map(move |el| attr.clone().build(el))
            })
            .collect();
        AnyViewWithAttrsState { view, attrs }
    }

    fn rebuild(self, state: &mut Self::State) {
        self.view.rebuild(&mut state.view);

        // at this point, we have rebuilt the inner view
        // now we need to update attributes that were spread onto this
        // this approach is not ideal, but it avoids two edge cases:
        // 1) merging attributes from two unrelated views (https://github.com/leptos-rs/leptos/issues/4268)
        // 2) failing to re-create attributes from the same view (https://github.com/leptos-rs/leptos/issues/4512)
        for element in state.elements() {
            // first, remove the previous set of attributes
            self.attrs
                .clone()
                .rebuild(&mut (element.clone(), Vec::new()));
            // then, add the new set of attributes
            self.attrs.clone().build(&element);
        }
    }
}

impl RenderHtml for AnyViewWithAttrs {
    type AsyncOutput = Self;
    type Owned = Self;
    const MIN_LENGTH: usize = 0;

    fn dry_resolve(&mut self) {
        self.view.dry_resolve();
        for attr in &mut self.attrs {
            attr.dry_resolve();
        }
    }

    async fn resolve(self) -> Self::AsyncOutput {
        let resolve_view = self.view.resolve();
        let resolve_attrs =
            join_all(self.attrs.into_iter().map(|attr| attr.resolve()));
        let (view, attrs) = join(resolve_view, resolve_attrs).await;
        Self { view, attrs }
    }

    fn to_html_with_buf(
        self,
        buf: &mut String,
        position: &mut Position,
        escape: bool,
        mark_branches: bool,
        mut extra_attrs: Vec<AnyAttribute>,
    ) {
        // `extra_attrs` will be empty here in most cases, but it will have
        // attributes in it already if this is, itself, receiving additional attrs
        extra_attrs.extend(self.attrs);
        self.view.to_html_with_buf(
            buf,
            position,
            escape,
            mark_branches,
            extra_attrs,
        );
    }

    fn to_html_async_with_buf<const OUT_OF_ORDER: bool>(
        self,
        buf: &mut StreamBuilder,
        position: &mut Position,
        escape: bool,
        mark_branches: bool,
        mut extra_attrs: Vec<AnyAttribute>,
    ) where
        Self: Sized,
    {
        extra_attrs.extend(self.attrs);
        self.view.to_html_async_with_buf::<OUT_OF_ORDER>(
            buf,
            position,
            escape,
            mark_branches,
            extra_attrs,
        );
    }

    fn hydrate<const FROM_SERVER: bool>(
        self,
        cursor: &Cursor,
        position: &PositionState,
    ) -> Self::State {
        let view = self.view.hydrate::<FROM_SERVER>(cursor, position);
        let elements = view.elements();
        let attrs = self
            .attrs
            .into_iter()
            .flat_map(|attr| {
                elements
                    .iter()
                    .map(move |el| attr.clone().hydrate::<FROM_SERVER>(el))
            })
            .collect();
        AnyViewWithAttrsState { view, attrs }
    }

    async fn hydrate_async(
        self,
        cursor: &Cursor,
        position: &PositionState,
    ) -> Self::State {
        let view = self.view.hydrate_async(cursor, position).await;
        let elements = view.elements();
        let attrs = self
            .attrs
            .into_iter()
            .flat_map(|attr| {
                elements
                    .iter()
                    .map(move |el| attr.clone().hydrate::<true>(el))
            })
            .collect();
        AnyViewWithAttrsState { view, attrs }
    }

    fn html_len(&self) -> usize {
        self.attrs
            .iter()
            .map(|attr| attr.html_len())
            .fold(self.view.html_len(), usize::saturating_add)
    }

    fn into_owned(self) -> Self::Owned {
        self
    }
}

impl AddAnyAttr for AnyViewWithAttrs {
    type Output<SomeNewAttr: Attribute> = AnyViewWithAttrs;

    fn add_any_attr<NewAttr: Attribute>(
        mut self,
        attr: NewAttr,
    ) -> Self::Output<NewAttr>
    where
        Self::Output<NewAttr>: RenderHtml,
    {
        self.attrs.push(attr.into_cloneable_owned().into_any_attr());
        self
    }
}

/// State for any view with attributes spread onto it.
pub struct AnyViewWithAttrsState {
    view: AnyViewState,
    #[allow(dead_code)] // keeps attribute states alive until dropped
    attrs: Vec<AnyAttributeState>,
}

impl Mountable for AnyViewWithAttrsState {
    fn unmount(&mut self) {
        self.view.unmount();
    }

    fn mount(
        &mut self,
        parent: &crate::renderer::types::Element,
        marker: Option<&crate::renderer::types::Node>,
    ) {
        self.view.mount(parent, marker)
    }

    fn insert_before_this(&self, child: &mut dyn Mountable) -> bool {
        self.view.insert_before_this(child)
    }

    fn elements(&self) -> Vec<crate::renderer::types::Element> {
        self.view.elements()
    }
}

#[cfg(test)]
mod tests {
    use super::IntoAny;
    use crate::view::RenderHtml;
    use futures::{executor::block_on, StreamExt};

    /// Without the `ssr` feature an `AnyView` has no HTML renderer. Rendering it to HTML
    /// panicked; it renders nothing.
    #[cfg(not(feature = "ssr"))]
    #[test]
    fn any_view_renders_nothing_without_the_ssr_feature() {
        assert_eq!("hello".into_any().to_html(), "");
        assert_eq!("hello".into_any().to_html_branching(), "");
    }

    #[cfg(not(feature = "ssr"))]
    #[test]
    fn any_view_streams_nothing_without_the_ssr_feature() {
        let in_order = "hello".into_any().to_html_stream_in_order();
        assert_eq!(block_on(in_order.collect::<Vec<_>>()).concat(), "");
        let out_of_order = "hello".into_any().to_html_stream_out_of_order();
        assert_eq!(block_on(out_of_order.collect::<Vec<_>>()).concat(), "");
    }

    /// Resolving (waiting for async data before rendering) panicked without `ssr`; it
    /// returns the view as it is.
    #[cfg(not(feature = "ssr"))]
    #[test]
    fn any_view_resolves_to_itself_without_the_ssr_feature() {
        let mut view = "hello".into_any();
        let type_id = view.as_type_id();
        view.dry_resolve();
        let resolved = block_on(view.resolve());
        assert_eq!(resolved.as_type_id(), type_id);
    }

    #[cfg(feature = "ssr")]
    #[test]
    fn any_view_renders_its_view() {
        assert_eq!("hello".into_any().to_html(), "hello");
        let mut view = "hello".into_any();
        view.dry_resolve();
        assert_eq!(block_on(view.resolve()).to_html(), "hello");
        let stream = "hello".into_any().to_html_stream_in_order();
        assert_eq!(block_on(stream.collect::<Vec<_>>()).concat(), "hello");
    }

    /// The length estimate of a view with spread attributes added the view's estimate to
    /// the attributes' and overflowed for a view that estimates `usize::MAX`.
    #[cfg(feature = "ssr")]
    #[test]
    fn any_view_with_attrs_length_estimate_saturates() {
        use crate::{
            html::attribute::custom::custom_attribute,
            view::add_attr::AddAnyAttr, view_error::test_support::HugeView,
        };

        let view = HugeView
            .into_any()
            .add_any_attr(custom_attribute("data-x", "1"));
        assert_eq!(view.html_len(), usize::MAX);
        assert_eq!(view.to_html(), "huge");
    }

    /// An `AnyView` whose value has another type than its functions. `into_any` makes the
    /// two together, so only code in this module can get here. Reading the value panicked
    /// ("Erased: type mismatch"), and with `--cfg erase_components` read a `u8` as a
    /// `String`.
    fn mismatched() -> super::AnyView {
        let mut view = "hello".into_any();
        view.value = super::Erased::new(5u8);
        view
    }

    /// It builds an empty view instead (logged once).
    #[test]
    fn an_any_view_holding_another_type_builds_an_empty_view() {
        use crate::view::{Mountable, Render};

        // natively there is no document: the empty view's placeholder is a stand-in
        let state = mismatched().build();
        assert!(state.elements().is_empty());
    }

    /// It renders and resolves to nothing instead (logged once).
    #[cfg(feature = "ssr")]
    #[test]
    fn an_any_view_holding_another_type_renders_nothing() {
        assert_eq!(mismatched().to_html(), "");
        let stream = mismatched().to_html_stream_in_order();
        assert_eq!(block_on(stream.collect::<Vec<_>>()).concat(), "");

        let mut view = mismatched();
        view.dry_resolve();
        assert_eq!(block_on(view.resolve()).to_html(), ().into_any().to_html());
    }
}
