use crate::{
    children::{TypedChildrenFn, ViewFn},
    IntoView,
};
use halyard_macro::component;
use halyard_reactive_graph::{
    computed::{ArcMemo, Memo},
    gone::render_value,
    owner::Storage,
    signal::{
        ArcMappedSignal, ArcReadSignal, ArcRwSignal, MappedSignal, ReadSignal,
        RwSignal,
    },
    traits::{Get, IsDisposed, TryGet},
    wrappers::read::{ArcSignal, Signal},
};
use halyard_tachys::either::EitherOf3;

/// The source of a [`<Show when=…>`](Show) or a [`<For each=…>`](crate::control_flow::For):
/// a closure, or a signal handle (weak or strong: a signal, memo, [`Signal`], mapped signal,
/// or what [`map`](halyard_reactive_graph::map::Map::map) and
/// [`memo`](halyard_reactive_graph::map::Map::memo) give), so that `when=flag` and
/// `each=items` work without a closure.
///
/// A weak handle whose value is gone gives `None` (reported once): `<Show>` renders nothing,
/// `<For>` renders no rows.
pub trait ViewSource {
    /// The value read.
    type Value;

    /// Reads the source, tracking it; `None` if it is a handle whose value is gone.
    #[track_caller]
    fn read_source(&self) -> Option<Self::Value>;
}

impl<F, T> ViewSource for F
where
    F: Fn() -> T,
{
    type Value = T;

    fn read_source(&self) -> Option<T> {
        Some(self())
    }
}

macro_rules! handle_sources {
    ($([$($gen:tt)*] $ty:ty),* $(,)?) => {
        $(
            impl<T, $($gen)*> ViewSource for $ty
            where
                $ty: TryGet<Value = T> + IsDisposed,
            {
                type Value = T;

                #[track_caller]
                fn read_source(&self) -> Option<T> {
                    render_value(self)
                }
            }
        )*
    };
}

handle_sources!(
    [S] RwSignal<T, S>,
    [S] ReadSignal<T, S>,
    [S: Storage<T>] Memo<T, S>,
    [S: Storage<T>] Signal<T, S>,
    [] ArcRwSignal<T>,
    [] ArcReadSignal<T>,
    [S: Storage<T>] ArcMemo<T, S>,
    [S: Storage<T>] ArcSignal<T, S>,
    [] MappedSignal<T>,
    [] ArcMappedSignal<T>,
);

#[component(transparent)]
pub fn Show<W, C>(
    /// The children will be shown whenever the condition in `when` is `true`: a closure that
    /// returns a `bool`, or a `bool` signal. A signal whose value is gone shows nothing.
    children: TypedChildrenFn<C>,
    /// A closure that returns a bool, or a signal of a bool, that determines whether this
    /// thing runs
    when: W,
    /// A closure that returns what gets rendered if the when statement is false. By default this is the empty view.
    #[prop(optional, into)]
    fallback: ViewFn,
) -> impl IntoView
where
    W: ViewSource<Value = bool> + Send + Sync + 'static,
    C: IntoView + 'static,
{
    let memoized_when = ArcMemo::new(move |_| when.read_source());
    let children = children.into_inner();

    move || match memoized_when.get() {
        Some(true) => EitherOf3::A(children()),
        Some(false) => EitherOf3::B(fallback.run()),
        None => EitherOf3::C(()),
    }
}
