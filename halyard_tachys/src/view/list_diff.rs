//! The keyed list diff behind `<For>` ([`keyed`](super::keyed::keyed)), and what the list
//! views do instead of panicking (README, "Project policy": no panics, ever).
//!
//! An update is planned by [`diff`], a pure function of the old and the new keys, and
//! carried out by [`apply`] through [`ListOps`], which `Keyed` implements on the DOM and the
//! tests on a model of it. Every index is checked. If keys repeat (the diff matches rows by
//! key, so it cannot update such a list) or a plan does not fit the rows (a state the diff
//! holds impossible), that is logged once and the list is rendered again from scratch:
//! every row unmounted, every item built and mounted in order, so the page is right. A
//! plan is checked in full before any new row is built, so every item is still there.

use indexmap::IndexSet;
use rustc_hash::FxHasher;
use std::{
    hash::{BuildHasherDefault, Hash},
    sync::atomic::{AtomicBool, Ordering},
};

pub(super) type FxIndexSet<T> = IndexSet<T, BuildHasherDefault<FxHasher>>;

#[derive(Debug, thiserror::Error)]
pub(super) enum ListError {
    /// Keys should be unique. SSR, hydration and the first render show every item anyway;
    /// updates do too, from scratch.
    #[error(
        "a keyed list (<For>) has items with the same key ({items} items, {keys} \
         distinct keys); keys must be unique. While keys repeat, each update renders \
         the list again from scratch, showing every item"
    )]
    RepeatedKeys { items: usize, keys: usize },
    /// The diff's plan does not fit the rendered rows: a bug in the diff.
    #[error(
        "the keyed list diff reached a state it holds impossible ({0}); the list is \
         rendered again from scratch"
    )]
    Invariant(&'static str),
    #[error(
        "a keyed list (<For>) was hydrated with no element around it; rows it adds \
         later are not shown until it is mounted again"
    )]
    NoParent,
    #[cfg(not(feature = "islands"))]
    #[error(
        "SerializableKey::ser_key was called without the `islands` feature, which it \
         needs; it returns an empty key"
    )]
    SerializableKeyWithoutIslands,
    #[cfg(feature = "islands")]
    #[error(
        "a key of a keyed list could not be serialized ({0}); an empty key is used \
         instead"
    )]
    KeyNotSerializable(serde_json::Error),
    /// Collecting the results for an array's items gave another number of results than
    /// the array has items, which the standard library and `futures` rule out.
    #[error(
        "{what} an array of {expected} views gave {found} results; it never \
         completes"
    )]
    ArrayLength {
        what: &'static str,
        expected: usize,
        found: usize,
    },
}

static REPORTED_REPEATED_KEYS: AtomicBool = AtomicBool::new(false);
static REPORTED_INVARIANT: AtomicBool = AtomicBool::new(false);

/// Logs an error that a list view recovered from: with `tracing` when that feature is on,
/// otherwise in the browser console or on standard error. Logged in every build.
pub(super) fn report(error: &ListError) {
    #[cfg(feature = "tracing")]
    tracing::error!("{error}");
    #[cfg(not(feature = "tracing"))]
    log_error(&format!("[halyard] {error}"));
}

/// Logs `error` the first time `reported` is seen unset: for errors that would otherwise
/// be logged on every update.
pub(super) fn report_once(reported: &AtomicBool, error: &ListError) {
    if !reported.swap(true, Ordering::Relaxed) {
        report(error);
    }
}

#[cfg(all(
    not(feature = "tracing"),
    target_arch = "wasm32",
    not(any(target_os = "emscripten", target_os = "wasi"))
))]
fn log_error(message: &str) {
    web_sys::console::error_1(&wasm_bindgen::JsValue::from_str(message));
}

#[cfg(all(
    not(feature = "tracing"),
    not(all(
        target_arch = "wasm32",
        not(any(target_os = "emscripten", target_os = "wasi"))
    ))
))]
fn log_error(message: &str) {
    use std::io::Write;
    // `eprintln!` panics if standard error is closed; with nowhere left to report the
    // error, it is dropped
    _ = writeln!(std::io::stderr(), "{message}");
}

/// For a result that a list view cannot produce, in a state that cannot happen: logs
/// `error` and never completes (there is no value to complete with).
pub(super) async fn never<T>(error: ListError) -> T {
    report(&error);
    std::future::pending().await
}

/// The rendered list that the diff updates.
pub(super) trait ListOps {
    /// An item of the new list, from which a row is built.
    type Item;
    /// A rendered row.
    type Row;

    /// Builds the row for `item`, the item at `index` of the new list, without mounting it.
    fn build(&mut self, index: usize, item: Self::Item) -> Self::Row;

    /// Takes `row` off the page.
    fn unmount(&mut self, row: &mut Self::Row);

    /// Puts `row` on the page (moving it if it is there) before `before`, or at the end of
    /// the list if `before` is `None`.
    fn mount(&mut self, row: &mut Self::Row, before: Option<&Self::Row>);

    /// Tells `row` its new index.
    fn set_index(&mut self, row: &Self::Row, index: usize);
}

/// Updates `rows`, rendered for `old_keys` (one row per key, in order), to show `items`,
/// whose keys are `new_keys`.
pub(super) fn reconcile<K: Eq + Hash, O: ListOps>(
    old_keys: &FxIndexSet<K>,
    new_keys: &FxIndexSet<K>,
    rows: &mut Vec<Option<O::Row>>,
    items: Vec<Option<O::Item>>,
    ops: &mut O,
) {
    // a key set has each key once: fewer keys than items or rows means repeated keys
    if items.len() != new_keys.len() || rows.len() != old_keys.len() {
        let (count, keys) = if items.len() != new_keys.len() {
            (items.len(), new_keys.len())
        } else {
            (rows.len(), old_keys.len())
        };
        report_once(
            &REPORTED_REPEATED_KEYS,
            &ListError::RepeatedKeys { items: count, keys },
        );
        rebuild_from_scratch(rows, items, ops);
        return;
    }
    match diff(old_keys, new_keys) {
        Ok(diff) => apply_or_rebuild(diff, rows, items, ops),
        Err(error) => {
            report_once(&REPORTED_INVARIANT, &error);
            rebuild_from_scratch(rows, items, ops);
        }
    }
}

/// Carries out `diff`; if it does not fit the rows, logs that and renders the list again
/// from scratch.
fn apply_or_rebuild<O: ListOps>(
    diff: Diff,
    rows: &mut Vec<Option<O::Row>>,
    mut items: Vec<Option<O::Item>>,
    ops: &mut O,
) {
    if let Err(error) = apply(&diff, rows, &mut items, ops) {
        report_once(&REPORTED_INVARIANT, &error);
        rebuild_from_scratch(rows, items, ops);
    }
}

/// Unmounts every row, then builds and mounts every item, in order.
fn rebuild_from_scratch<O: ListOps>(
    rows: &mut Vec<Option<O::Row>>,
    items: Vec<Option<O::Item>>,
    ops: &mut O,
) {
    for mut row in rows.drain(..).flatten() {
        ops.unmount(&mut row);
    }
    rows.reserve(items.len());
    // an item is `None` only if `apply` failed after building it, which its checks rule
    // out; the rows then no longer match the keys, so the next update starts from scratch
    for (index, item) in items.into_iter().enumerate() {
        if let Some(item) = item {
            let mut row = ops.build(index, item);
            ops.mount(&mut row, None);
            rows.push(Some(row));
        }
    }
}

/// What to do to the rows of the old keys to get the rows of the new keys.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Diff {
    /// Old indices of the rows to remove, ascending.
    removed: Vec<usize>,
    /// The rows that are kept at another index, ascending by old index.
    moved: Vec<DiffOpMove>,
    /// New indices of the rows to build, ascending.
    added: Vec<DiffOpAdd>,
    /// Remove every row (the new list is empty).
    clear: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DiffOpMove {
    /// Index in the old list.
    from: usize,
    /// Index in the new list.
    to: usize,
    /// Whether the row moves on the page, or only in `rows` (its place on the page is
    /// already right, relative to the rows around it that do not move on the page).
    move_in_dom: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DiffOpAdd {
    /// Index in the new list.
    at: usize,
    mode: DiffOpAddMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiffOpAddMode {
    /// Before the next row in place.
    Normal,
    /// At the end of the list (the old list was empty).
    Append,
}

/// Plans the update from the rows of `from` to the rows of `to`.
///
/// A kept row moves on the page unless the rows added and removed up to its index shift it
/// by exactly as many places as it moves (it moves 2 places after 2 insertions) and it does
/// not move past a row that stays at its index. (The previous diff did not check the
/// second: such a row stayed on the wrong side of the row that stayed, and the page showed
/// the list out of order.) A run of adjacent moves (the same rows, next to each other, in
/// both lists) moves on the page or stays as its first row does. `Err` only in states that
/// cannot happen.
pub(super) fn diff<K: Eq + Hash>(
    from: &FxIndexSet<K>,
    to: &FxIndexSet<K>,
) -> Result<Diff, ListError> {
    if to.is_empty() {
        return Ok(Diff {
            clear: !from.is_empty(),
            ..Diff::default()
        });
    }
    if from.is_empty() {
        return Ok(Diff {
            added: (0..to.len())
                .map(|at| DiffOpAdd {
                    at,
                    mode: DiffOpAddMode::Append,
                })
                .collect(),
            ..Diff::default()
        });
    }

    let len = from.len().max(to.len());
    let mut removed = Vec::new();
    let mut moved = Vec::new();
    let mut added = Vec::new();
    // `stays_before[i]`: how many rows before index `i` stay at their index
    let mut stays_before = Vec::with_capacity(len.saturating_add(1));
    let mut stays = 0usize;
    for index in 0..len {
        stays_before.push(stays);
        let from_key = from.get_index(index);
        let to_key = to.get_index(index);
        if from_key == to_key {
            stays = stays.saturating_add(1);
            continue;
        }
        let new_index = from_key.and_then(|key| to.get_index_of(key));
        if from_key.is_some() && new_index.is_none() {
            removed.push(index);
        }
        if to_key.is_some_and(|key| !from.contains(key)) {
            added.push(DiffOpAdd {
                at: index,
                mode: DiffOpAddMode::Normal,
            });
        }
        if let Some(to_index) = new_index {
            let (Some(shifted), Some(target)) = (
                index.checked_add(added.len()),
                to_index.checked_add(removed.len()),
            ) else {
                return Err(ListError::Invariant("an index overflows"));
            };
            moved.push(DiffOpMove {
                from: index,
                to: to_index,
                move_in_dom: shifted != target,
            });
        }
    }
    stays_before.push(stays);

    let mut previous: Option<DiffOpMove> = None;
    for op in &mut moved {
        op.move_in_dom = match previous {
            Some(run)
                if run.from.checked_add(1) == Some(op.from)
                    && run.to.checked_add(1) == Some(op.to) =>
            {
                run.move_in_dom
            }
            _ => {
                op.move_in_dom
                    || passes_a_row_that_stays(&stays_before, op.from, op.to)?
            }
        };
        previous = Some(*op);
    }

    Ok(Diff {
        removed,
        moved,
        added,
        clear: false,
    })
}

/// Whether a row that stays at its index lies strictly between `from` and `to`. A row
/// that moves past it without moving on the page would stay on its old side of it.
fn passes_a_row_that_stays(
    stays_before: &[usize],
    from: usize,
    to: usize,
) -> Result<bool, ListError> {
    let (low, high) = (from.min(to), from.max(to));
    match (
        low.checked_add(1).and_then(|next| stays_before.get(next)),
        stays_before.get(high),
    ) {
        (Some(after_low), Some(before_high)) => Ok(before_high > after_low),
        _ => Err(ListError::Invariant("an index is past the end of the list")),
    }
}

/// Carries out `diff` on `rows` (one per old key), building rows from `items` (one per
/// new key). `Err` if the plan does not fit the rows; the rows taken out to be moved are
/// then unmounted, the rest are left for [`rebuild_from_scratch`].
fn apply<O: ListOps>(
    diff: &Diff,
    rows: &mut Vec<Option<O::Row>>,
    items: &mut [Option<O::Item>],
    ops: &mut O,
) -> Result<(), ListError> {
    let mut moving = Vec::with_capacity(diff.moved.len());
    let result = apply_plan(diff, rows, items, &mut moving, ops);
    if result.is_err() {
        // taken out of `rows` but still on the page
        for mut row in moving.into_iter().flatten() {
            ops.unmount(&mut row);
        }
    }
    result
}

fn apply_plan<O: ListOps>(
    diff: &Diff,
    rows: &mut Vec<Option<O::Row>>,
    items: &mut [Option<O::Item>],
    moving: &mut Vec<Option<O::Row>>,
    ops: &mut O,
) -> Result<(), ListError> {
    let new_len = items.len();
    let kept = if diff.clear {
        Some(0)
    } else {
        rows.len().checked_sub(diff.removed.len())
    };
    if kept.and_then(|kept| kept.checked_add(diff.added.len())) != Some(new_len)
    {
        return Err(ListError::Invariant(
            "the plan does not account for every row",
        ));
    }

    if diff.clear {
        for mut row in rows.drain(..).flatten() {
            ops.unmount(&mut row);
        }
        if diff.added.is_empty() {
            return Ok(());
        }
    }

    for &at in &diff.removed {
        let mut row = rows
            .get_mut(at)
            .and_then(Option::take)
            .ok_or(ListError::Invariant("a row to remove is missing"))?;
        ops.unmount(&mut row);
    }

    for op in &diff.moved {
        let row = rows
            .get_mut(op.from)
            .and_then(Option::take)
            .ok_or(ListError::Invariant("a row to move is missing"))?;
        moving.push(Some(row));
    }

    let len = rows
        .len()
        .checked_add(diff.added.len())
        .ok_or(ListError::Invariant("an index overflows"))?;
    rows.resize_with(len, || None);

    // rows whose place on the page is right: the other rows move around them
    for (op, row) in diff.moved.iter().zip(moving.iter_mut()) {
        if op.move_in_dom {
            continue;
        }
        let (place, _) = free_place(rows, op.to, new_len)?;
        let row = row
            .take()
            .ok_or(ListError::Invariant("a row moves twice"))?;
        ops.set_index(&row, op.to);
        *place = Some(row);
    }

    // rows that move on the page, each before the next row that is in place
    for (op, row) in diff.moved.iter().zip(moving.iter_mut()) {
        if !op.move_in_dom {
            continue;
        }
        let (place, after) = free_place(rows, op.to, new_len)?;
        let mut row = row
            .take()
            .ok_or(ListError::Invariant("a row moves twice"))?;
        ops.mount(&mut row, after.iter().flatten().next());
        ops.set_index(&row, op.to);
        *place = Some(row);
    }

    // once an item is built, the list can no longer be rendered from scratch
    check_new_rows(diff, rows, items, new_len)?;

    for add in &diff.added {
        let (place, after) = free_place(rows, add.at, new_len)?;
        let item = items
            .get_mut(add.at)
            .and_then(Option::take)
            .ok_or(ListError::Invariant("an item to add is missing"))?;
        let mut row = ops.build(add.at, item);
        let before = match add.mode {
            DiffOpAddMode::Normal => after.iter().flatten().next(),
            DiffOpAddMode::Append => None,
        };
        ops.mount(&mut row, before);
        *place = Some(row);
    }

    // every row is in its place; what is left past the end is free places
    rows.truncate(new_len);
    Ok(())
}

/// The free place at `index` of the new list (of `new_len` rows), and the places after it.
fn free_place<R>(
    rows: &mut [Option<R>],
    index: usize,
    new_len: usize,
) -> Result<(&mut Option<R>, &[Option<R>]), ListError> {
    if index >= new_len {
        return Err(ListError::Invariant(
            "a row's new index is past the end of the list",
        ));
    }
    match rows
        .get_mut(index..)
        .and_then(<[Option<R>]>::split_first_mut)
    {
        Some((place, after)) if place.is_none() => Ok((place, after)),
        _ => Err(ListError::Invariant("two rows are given the same index")),
    }
}

/// Checks that the new rows of `diff` fill exactly the free places of the new list, each
/// from an item that is there.
fn check_new_rows<R, I>(
    diff: &Diff,
    rows: &[Option<R>],
    items: &[Option<I>],
    new_len: usize,
) -> Result<(), ListError> {
    let Some((places, past_end)) = rows.split_at_checked(new_len) else {
        return Err(ListError::Invariant("the new list has too few places"));
    };
    if past_end.iter().any(Option::is_some) {
        return Err(ListError::Invariant(
            "a row is left past the end of the list",
        ));
    }
    let free = places.iter().filter(|place| place.is_none()).count();
    if free != diff.added.len() {
        return Err(ListError::Invariant(
            "the new rows do not fill the free places",
        ));
    }
    let mut previous = None;
    for add in &diff.added {
        let fits = previous.is_none_or(|previous| add.at > previous)
            && matches!(places.get(add.at), Some(None))
            && matches!(items.get(add.at), Some(Some(_)));
        if !fits {
            return Err(ListError::Invariant(
                "a new row does not fit a free place",
            ));
        }
        previous = Some(add.at);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        apply_or_rebuild, reconcile, Diff, DiffOpAdd, DiffOpAddMode,
        DiffOpMove, FxIndexSet, ListOps, REPORTED_INVARIANT,
        REPORTED_REPEATED_KEYS,
    };
    use std::{collections::HashMap, sync::atomic::Ordering};

    /// A plan written by hand, to test what happens when one does not fit the rows.
    fn plan(
        removed: &[usize],
        moved: &[(usize, usize, bool)],
        added: &[usize],
    ) -> Diff {
        Diff {
            removed: removed.to_vec(),
            moved: moved
                .iter()
                .map(|&(from, to, move_in_dom)| DiffOpMove {
                    from,
                    to,
                    move_in_dom,
                })
                .collect(),
            added: added
                .iter()
                .map(|&at| DiffOpAdd {
                    at,
                    mode: DiffOpAddMode::Normal,
                })
                .collect(),
            clear: false,
        }
    }

    type Key = u16;

    /// A rendered row: its key, and which build made it.
    #[derive(Debug)]
    struct Row {
        key: Key,
        id: usize,
    }

    /// What the diff asked the list to do, in order.
    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Op {
        Build { index: usize, key: Key },
        Unmount { key: Key },
        Mount { key: Key, before: Option<Key> },
        SetIndex { key: Key, index: usize },
    }

    /// The list's parent in a model DOM: the ids of the rows it shows, in order. It does
    /// what the browser does: mounting a row that is already shown moves it, and mounting
    /// before a row that is not shown mounts at the end (before the list's marker).
    #[derive(Default)]
    struct Model {
        dom: Vec<usize>,
        keys: HashMap<usize, Key>,
        next_id: usize,
        ops: Vec<Op>,
    }

    impl ListOps for Model {
        type Item = Key;
        type Row = Row;

        fn build(&mut self, index: usize, key: Key) -> Row {
            let id = self.next_id;
            self.next_id += 1;
            self.keys.insert(id, key);
            self.ops.push(Op::Build { index, key });
            Row { key, id }
        }

        fn unmount(&mut self, row: &mut Row) {
            self.dom.retain(|&id| id != row.id);
            self.ops.push(Op::Unmount { key: row.key });
        }

        fn mount(&mut self, row: &mut Row, before: Option<&Row>) {
            self.dom.retain(|&id| id != row.id);
            let at = before
                .and_then(|anchor| {
                    self.dom.iter().position(|&id| id == anchor.id)
                })
                .unwrap_or(self.dom.len());
            self.dom.insert(at, row.id);
            self.ops.push(Op::Mount {
                key: row.key,
                before: before.map(|anchor| anchor.key),
            });
        }

        fn set_index(&mut self, row: &Row, index: usize) {
            self.ops.push(Op::SetIndex {
                key: row.key,
                index,
            });
        }
    }

    /// A keyed list in the model DOM, updated as `Keyed::rebuild` updates one.
    struct List {
        keys: FxIndexSet<Key>,
        rows: Vec<Option<Row>>,
        model: Model,
    }

    impl List {
        /// Renders and mounts `keys` as `Keyed::build` and `mount` do: every item,
        /// repeated keys included.
        fn new(keys: &[Key]) -> Self {
            let mut model = Model::default();
            let rows = keys
                .iter()
                .enumerate()
                .map(|(index, &key)| {
                    let mut row = model.build(index, key);
                    model.mount(&mut row, None);
                    Some(row)
                })
                .collect();
            model.ops.clear();
            Self {
                keys: keys.iter().copied().collect(),
                rows,
                model,
            }
        }

        /// Updates the list to `keys`; returns what the diff did.
        fn update(&mut self, keys: &[Key]) -> Vec<Op> {
            let new_keys: FxIndexSet<Key> = keys.iter().copied().collect();
            let items = keys.iter().copied().map(Some).collect();
            reconcile(
                &self.keys,
                &new_keys,
                &mut self.rows,
                items,
                &mut self.model,
            );
            self.keys = new_keys;
            std::mem::take(&mut self.model.ops)
        }

        /// Applies a hand-written `plan` for the update to `keys`.
        fn apply(&mut self, plan: Diff, keys: &[Key]) -> Vec<Op> {
            let items = keys.iter().copied().map(Some).collect();
            apply_or_rebuild(plan, &mut self.rows, items, &mut self.model);
            self.keys = keys.iter().copied().collect();
            std::mem::take(&mut self.model.ops)
        }

        /// The keys the page shows, in order.
        fn shown(&self) -> Vec<Key> {
            self.model
                .dom
                .iter()
                .map(|id| self.model.keys[id])
                .collect()
        }

        /// The keys of the retained rows, in order (`None` for a hole).
        fn row_keys(&self) -> Vec<Option<Key>> {
            self.rows
                .iter()
                .map(|row| row.as_ref().map(|row| row.key))
                .collect()
        }

        /// The row ids by key (for lists of distinct keys).
        fn ids(&self) -> HashMap<Key, usize> {
            self.rows
                .iter()
                .flatten()
                .map(|row| (row.key, row.id))
                .collect()
        }
    }

    fn some(keys: &[Key]) -> Vec<Option<Key>> {
        keys.iter().copied().map(Some).collect()
    }

    /// Updates `list` (of distinct keys) to `new` (distinct keys) and checks the result:
    /// the page shows `new` in order, the rows match it, every kept key keeps its row,
    /// every new key gets one new row and every removed row is unmounted.
    fn check_update(list: &mut List, new: &[Key]) -> Result<Vec<Op>, String> {
        let old: Vec<Key> = list.keys.iter().copied().collect();
        let old_ids = list.ids();
        let first_new_id = list.model.next_id;
        let ops = list.update(new);
        let failure = |list: &List, what: &str| {
            format!(
                "{old:?} -> {new:?}: {what} (shown {:?}, rows {:?}, ops {ops:?})",
                list.shown(),
                list.row_keys()
            )
        };
        if list.shown() != new {
            return Err(failure(list, "the page does not show the new order"));
        }
        if list.row_keys() != some(new) {
            return Err(failure(list, "the rows are not in the new order"));
        }
        for row in list.rows.iter().flatten() {
            let reused = match old_ids.get(&row.key) {
                Some(&id) => id == row.id,
                None => row.id >= first_new_id,
            };
            if !reused {
                return Err(failure(list, "a row was built again or reused"));
            }
        }
        let built = ops
            .iter()
            .filter(|op| matches!(op, Op::Build { .. }))
            .count();
        let unmounted = ops
            .iter()
            .filter(|op| matches!(op, Op::Unmount { .. }))
            .count();
        let added = new.iter().filter(|key| !old_ids.contains_key(key)).count();
        let removed = old.iter().filter(|key| !new.contains(key)).count();
        if built != added || unmounted != removed {
            return Err(failure(
                list,
                "rows were built or unmounted needlessly",
            ));
        }
        Ok(ops)
    }

    /// Calls `f` with every pair of lists of distinct keys with at most `max` keys each,
    /// up to renaming keys (the diff only compares keys): the old list is `0..m`, the new
    /// one any arrangement of some of those and of fresh keys (`m`, `m + 1`, ..., named in
    /// the order they appear).
    fn for_each_small_pair(max: usize, mut f: impl FnMut(&[Key], &[Key])) {
        fn extend(
            old: &[Key],
            new: &mut Vec<Key>,
            used: &mut [bool],
            fresh: Key,
            max: usize,
            f: &mut dyn FnMut(&[Key], &[Key]),
        ) {
            f(old, new);
            if new.len() == max {
                return;
            }
            for key in 0..old.len() {
                if !used[key] {
                    used[key] = true;
                    new.push(key as Key);
                    extend(old, new, used, fresh, max, f);
                    new.pop();
                    used[key] = false;
                }
            }
            new.push(fresh);
            extend(old, new, used, fresh + 1, max, f);
            new.pop();
        }

        for m in 0..=max {
            let old: Vec<Key> = (0..m as Key).collect();
            let mut used = vec![false; m];
            extend(&old, &mut Vec::new(), &mut used, m as Key, max, &mut f);
        }
    }

    /// xorshift64*: deterministic, so a failure can be reproduced.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn below(&mut self, n: usize) -> usize {
            if n == 0 {
                0
            } else {
                (self.next() % n as u64) as usize
            }
        }

        fn shuffle(&mut self, keys: &mut [Key]) {
            for i in (1..keys.len()).rev() {
                keys.swap(i, self.below(i + 1));
            }
        }
    }

    /// A random edit of `old`: the kinds of change a real list sees (a few moves,
    /// insertions and removals; a filter; a sort; a reversal), and arbitrary ones.
    fn random_edit(rng: &mut Rng, old: &[Key], fresh: &mut Key) -> Vec<Key> {
        let mut new = old.to_vec();
        match rng.below(5) {
            0 => {
                for _ in 0..1 + rng.below(4) {
                    match rng.below(4) {
                        0 if new.len() >= 2 => {
                            let (a, b) =
                                (rng.below(new.len()), rng.below(new.len()));
                            new.swap(a, b);
                        }
                        1 if !new.is_empty() => {
                            new.remove(rng.below(new.len()));
                        }
                        2 => {
                            new.insert(rng.below(new.len() + 1), *fresh);
                            *fresh += 1;
                        }
                        _ if !new.is_empty() => {
                            let key = new.remove(rng.below(new.len()));
                            new.insert(rng.below(new.len() + 1), key);
                        }
                        _ => {}
                    }
                }
            }
            1 => new.retain(|_| rng.below(3) != 0),
            2 => new.sort_unstable(),
            3 => new.reverse(),
            _ => {
                new.retain(|_| rng.below(4) != 0);
                for _ in 0..rng.below(6) {
                    new.push(*fresh);
                    *fresh += 1;
                }
                rng.shuffle(&mut new);
            }
        }
        new
    }

    #[test]
    fn every_pair_of_small_lists_ends_in_the_new_order() {
        let mut cases = 0usize;
        let mut wrong = 0usize;
        let mut first = Vec::new();
        for_each_small_pair(7, |old, new| {
            cases += 1;
            if let Err(failure) = check_update(&mut List::new(old), new) {
                wrong += 1;
                if first.len() < 3 {
                    first.push(failure);
                }
            }
        });
        assert!(cases > 200_000, "only {cases} cases");
        assert_eq!(
            wrong,
            0,
            "{wrong} of {cases} updates went wrong; the first:\n{}",
            first.join("\n")
        );
    }

    #[test]
    fn random_longer_lists_end_in_the_new_order() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let mut updates = 0usize;
        let mut wrong = 0usize;
        let mut first = Vec::new();
        for _ in 0..2_000 {
            let len = rng.below(48);
            let mut start: Vec<Key> = (0..len as Key).collect();
            rng.shuffle(&mut start);
            let mut fresh = len as Key;
            let mut list = List::new(&start);
            for _ in 0..6 {
                let old: Vec<Key> = list.keys.iter().copied().collect();
                let new = random_edit(&mut rng, &old, &mut fresh);
                updates += 1;
                if let Err(failure) = check_update(&mut list, &new) {
                    wrong += 1;
                    if first.len() < 3 {
                        first.push(failure);
                    }
                    // the model no longer matches the rows; start a new list
                    break;
                }
            }
        }
        assert_eq!(
            wrong,
            0,
            "{wrong} of {updates} updates went wrong; the first:\n{}",
            first.join("\n")
        );
    }

    /// Wherever the previous diff left the page right, the rewrite does the same DOM
    /// operations in the same order: the same moves, insertions and removals.
    #[test]
    fn same_operations_as_the_previous_diff_wherever_it_was_right() {
        let mut same = 0usize;
        let mut previously_wrong = 0usize;
        let mut compare = |old: &[Key], new: &[Key]| {
            let mut before = List::new(old);
            let expected = previous::update(&mut before, new);
            if before.shown() != new {
                previously_wrong += 1;
                return;
            }
            let mut after = List::new(old);
            assert_eq!(after.update(new), expected, "{old:?} -> {new:?}");
            same += 1;
        };
        for_each_small_pair(7, &mut compare);
        let mut rng = Rng(0x0DDB_1A5E_5BAD_5EED);
        for _ in 0..5_000 {
            let len = rng.below(48);
            let mut old: Vec<Key> = (0..len as Key).collect();
            rng.shuffle(&mut old);
            let mut fresh = len as Key;
            let new = random_edit(&mut rng, &old, &mut fresh);
            compare(&old, &new);
        }
        assert!(same > 200_000, "only {same} cases compared");
        assert!(previously_wrong > 0);
    }

    /// A row that keeps its place on the page must not cross a row that stays at its
    /// index. The previous diff left `1` in place (two rows were added before it and it
    /// moved two places) although `2`, which stays at index 2, is now before it; the page
    /// showed `[1, 3, 4, 2, 0]`.
    #[test]
    fn a_row_moving_past_a_row_that_stays_is_moved_in_the_dom() {
        let mut list = List::new(&[0, 1, 2]);
        let ops = list.update(&[3, 4, 2, 1, 0]);
        assert_eq!(list.shown(), [3, 4, 2, 1, 0]);
        assert_eq!(
            ops,
            [
                Op::Mount {
                    key: 0,
                    before: None
                },
                Op::SetIndex { key: 0, index: 4 },
                Op::Mount {
                    key: 1,
                    before: Some(0)
                },
                Op::SetIndex { key: 1, index: 3 },
                Op::Build { index: 0, key: 3 },
                Op::Mount {
                    key: 3,
                    before: Some(2)
                },
                Op::Build { index: 1, key: 4 },
                Op::Mount {
                    key: 4,
                    before: Some(2)
                },
            ]
        );
    }

    #[test]
    fn empty_lists() {
        let mut list = List::new(&[]);
        assert_eq!(list.update(&[]), []);
        assert_eq!(list.shown(), Vec::<Key>::new());
        check_update(&mut list, &[1, 2]).unwrap();
        check_update(&mut list, &[]).unwrap();
        assert!(list.rows.is_empty());
        check_update(&mut list, &[]).unwrap();
        check_update(&mut list, &[3]).unwrap();
    }

    /// Keys should be unique, but nothing stops a list from repeating one. Every item is
    /// shown, in order, as the first render and the server render show them. The previous
    /// diff dropped the second `2` and rendered the item `2` for the key `3`.
    #[test]
    fn repeated_keys_in_an_update_show_every_item_in_order() {
        let mut list = List::new(&[1]);
        list.update(&[2, 2, 3]);
        assert_eq!(list.shown(), [2, 2, 3]);
        assert_eq!(list.row_keys(), some(&[2, 2, 3]));
        assert!(REPORTED_REPEATED_KEYS.load(Ordering::Relaxed));
    }

    /// The first render shows both `1`s; the update must remove both. The previous diff
    /// removed one and left the other on the page for good.
    #[test]
    fn repeated_keys_in_the_first_render_leave_no_stale_rows() {
        let mut list = List::new(&[1, 1]);
        list.update(&[2]);
        assert_eq!(list.shown(), [2]);
        assert_eq!(list.row_keys(), some(&[2]));
        assert!(REPORTED_REPEATED_KEYS.load(Ordering::Relaxed));
    }

    /// Once the keys are unique again, the list is diffed by key again: rows are kept.
    #[test]
    fn a_list_whose_keys_stop_repeating_is_diffed_by_key_again() {
        let mut list = List::new(&[1, 1]);
        list.update(&[1, 2]);
        assert_eq!(list.shown(), [1, 2]);
        let ids = list.ids();
        check_update(&mut list, &[2, 1, 3]).unwrap();
        assert_eq!(list.ids()[&1], ids[&1]);
        assert_eq!(list.ids()[&2], ids[&2]);
    }

    /// Applies a plan that does not fit the rows of `[1, 2, 3]` for the update to
    /// `[3, 4]`: the list is rendered again from scratch, so the page is right.
    fn recovers_from(plan: Diff) {
        let mut list = List::new(&[1, 2, 3]);
        let old_ids = list.ids();
        list.apply(plan, &[3, 4]);
        assert_eq!(list.shown(), [3, 4]);
        assert_eq!(list.row_keys(), some(&[3, 4]));
        assert!(
            list.rows
                .iter()
                .flatten()
                .all(|row| !old_ids.values().any(|&id| id == row.id)),
            "every row is rendered again"
        );
        assert!(REPORTED_INVARIANT.load(Ordering::Relaxed));
    }

    #[test]
    fn a_plan_removing_past_the_end_renders_the_list_again() {
        recovers_from(plan(&[0, 7], &[(2, 0, true)], &[1]));
    }

    #[test]
    fn a_plan_removing_a_row_twice_renders_the_list_again() {
        recovers_from(plan(&[0, 0], &[(2, 0, true)], &[1]));
    }

    #[test]
    fn a_plan_moving_a_missing_row_renders_the_list_again() {
        recovers_from(plan(&[0, 1], &[(9, 0, true)], &[1]));
    }

    #[test]
    fn a_plan_moving_past_the_end_renders_the_list_again() {
        recovers_from(plan(&[0, 1], &[(2, 9, false)], &[1]));
    }

    #[test]
    fn a_plan_adding_past_the_end_renders_the_list_again() {
        recovers_from(plan(&[0, 1], &[(2, 0, true)], &[5]));
    }

    #[test]
    fn a_plan_moving_two_rows_to_one_place_renders_the_list_again() {
        recovers_from(plan(&[0], &[(1, 0, false), (2, 0, false)], &[]));
    }

    #[test]
    fn a_plan_leaving_a_hole_renders_the_list_again() {
        recovers_from(plan(&[0, 1], &[], &[1]));
    }

    /// The diff as it was before the rewrite (Leptos 0.8.20's, made generic over the list
    /// it updates), kept to check that the rewrite does the same DOM operations wherever
    /// it was right.
    mod previous {
        use super::{Key, List, Op};
        use crate::view::list_diff::{FxIndexSet, ListOps};
        use std::hash::Hash;

        pub(super) fn update(list: &mut List, keys: &[Key]) -> Vec<Op> {
            let new_keys: FxIndexSet<Key> = keys.iter().copied().collect();
            let items = keys.iter().copied().map(Some).collect();
            let cmds = diff(&list.keys, &new_keys);
            apply_diff(cmds, &mut list.rows, items, &mut list.model);
            list.keys = new_keys;
            std::mem::take(&mut list.model.ops)
        }

        fn diff<K: Eq + Hash>(
            from: &FxIndexSet<K>,
            to: &FxIndexSet<K>,
        ) -> Diff {
            if from.is_empty() && to.is_empty() {
                return Diff::default();
            } else if to.is_empty() {
                return Diff {
                    clear: true,
                    ..Default::default()
                };
            } else if from.is_empty() {
                return Diff {
                    added: to
                        .iter()
                        .enumerate()
                        .map(|(at, _)| DiffOpAdd {
                            at,
                            mode: DiffOpAddMode::Append,
                        })
                        .collect(),
                    ..Default::default()
                };
            }

            let mut removed = vec![];
            let mut moved = vec![];
            let mut added = vec![];
            let max_len = std::cmp::max(from.len(), to.len());

            for index in 0..max_len {
                let from_item = from.get_index(index);
                let to_item = to.get_index(index);
                if from_item != to_item {
                    if from_item.is_some() && !to.contains(from_item.unwrap()) {
                        removed.push(DiffOpRemove { at: index });
                    }
                    if to_item.is_some() && !from.contains(to_item.unwrap()) {
                        added.push(DiffOpAdd {
                            at: index,
                            mode: DiffOpAddMode::Normal,
                        });
                    }
                    if let Some(from_item) = from_item {
                        if let Some(to_item) = to.get_full(from_item) {
                            let moves_forward_by =
                                (to_item.0 as i32) - (index as i32);
                            let move_in_dom = moves_forward_by
                                != (added.len() as i32)
                                    - (removed.len() as i32);
                            moved.push(DiffOpMove {
                                from: index,
                                len: 1,
                                to: to_item.0,
                                move_in_dom,
                            });
                        }
                    }
                }
            }

            moved = group_adjacent_moves(moved);

            Diff {
                removed,
                items_to_move: moved.iter().map(|m| m.len).sum(),
                moved,
                added,
                clear: false,
            }
        }

        fn group_adjacent_moves(moved: Vec<DiffOpMove>) -> Vec<DiffOpMove> {
            let mut prev: Option<DiffOpMove> = None;
            let mut new_moved = Vec::with_capacity(moved.len());
            for m in moved {
                match prev {
                    Some(mut p) => {
                        if (m.from == p.from + p.len) && (m.to == p.to + p.len)
                        {
                            p.len += 1;
                            prev = Some(p);
                        } else {
                            new_moved.push(prev.take().unwrap());
                            prev = Some(m);
                        }
                    }
                    None => prev = Some(m),
                }
            }
            if let Some(prev) = prev {
                new_moved.push(prev)
            }
            new_moved
        }

        #[derive(Debug, Default)]
        struct Diff {
            removed: Vec<DiffOpRemove>,
            moved: Vec<DiffOpMove>,
            items_to_move: usize,
            added: Vec<DiffOpAdd>,
            clear: bool,
        }

        #[derive(Clone, Copy, Debug)]
        struct DiffOpMove {
            from: usize,
            len: usize,
            to: usize,
            move_in_dom: bool,
        }

        #[derive(Clone, Copy, Debug)]
        struct DiffOpAdd {
            at: usize,
            mode: DiffOpAddMode,
        }

        #[derive(Debug)]
        struct DiffOpRemove {
            at: usize,
        }

        #[derive(Clone, Copy, Debug)]
        enum DiffOpAddMode {
            Normal,
            Append,
        }

        fn next_row<T>(rows: &[Option<T>], start_at: usize) -> Option<&T> {
            rows[start_at..].iter().find(|s| s.is_some())?.as_ref()
        }

        fn apply_diff<O: ListOps>(
            diff: Diff,
            children: &mut Vec<Option<O::Row>>,
            mut items: Vec<Option<O::Item>>,
            ops: &mut O,
        ) {
            if diff.clear {
                for mut child in children.drain(0..).flatten() {
                    ops.unmount(&mut child);
                }
                if diff.added.is_empty() {
                    return;
                }
            }

            for DiffOpRemove { at } in &diff.removed {
                let mut item_to_remove = children[*at].take().unwrap();
                ops.unmount(&mut item_to_remove);
            }

            let (move_cmds, add_cmds) = unpack_moves(&diff);

            let mut moved_children = move_cmds
                .iter()
                .map(|move_| children[move_.from].take())
                .collect::<Vec<_>>();

            children.resize_with(children.len() + diff.added.len(), || None);

            for (i, DiffOpMove { to, .. }) in move_cmds
                .iter()
                .enumerate()
                .filter(|(_, move_)| !move_.move_in_dom)
            {
                children[*to] = moved_children[i]
                    .take()
                    .inspect(|row| ops.set_index(row, *to));
            }

            for (i, DiffOpMove { to, .. }) in move_cmds
                .into_iter()
                .enumerate()
                .filter(|(_, move_)| move_.move_in_dom)
            {
                let mut each_item = moved_children[i].take().unwrap();
                ops.mount(&mut each_item, next_row(children, to));
                ops.set_index(&each_item, to);
                children[to] = Some(each_item);
            }

            for DiffOpAdd { at, mode } in add_cmds {
                let item = items[at].take().unwrap();
                let mut item = ops.build(at, item);
                let before = match mode {
                    DiffOpAddMode::Normal => next_row(children, at),
                    DiffOpAddMode::Append => None,
                };
                ops.mount(&mut item, before);
                children[at] = Some(item);
            }

            children.retain(Option::is_some);
        }

        fn unpack_moves(diff: &Diff) -> (Vec<DiffOpMove>, Vec<DiffOpAdd>) {
            let mut moves = Vec::with_capacity(diff.items_to_move);
            let mut adds = Vec::with_capacity(diff.added.len());

            let mut removes_iter = diff.removed.iter();
            let mut adds_iter = diff.added.iter();
            let mut moves_iter = diff.moved.iter();

            let mut removes_next = removes_iter.next();
            let mut adds_next = adds_iter.next();
            let mut moves_next = moves_iter.next().copied();

            for i in
                0..diff.items_to_move + diff.added.len() + diff.removed.len()
            {
                if let Some(DiffOpRemove { at, .. }) = removes_next {
                    if i == *at {
                        removes_next = removes_iter.next();
                        continue;
                    }
                }

                match (adds_next, &mut moves_next) {
                    (Some(add), Some(move_)) => {
                        if add.at == i {
                            adds.push(*add);
                            adds_next = adds_iter.next();
                        } else {
                            let mut single_move = *move_;
                            single_move.len = 1;
                            moves.push(single_move);
                            move_.len -= 1;
                            move_.from += 1;
                            move_.to += 1;
                            if move_.len == 0 {
                                moves_next = moves_iter.next().copied();
                            }
                        }
                    }
                    (Some(add), None) => {
                        adds.push(*add);
                        adds_next = adds_iter.next();
                    }
                    (None, Some(move_)) => {
                        let mut single_move = *move_;
                        single_move.len = 1;
                        moves.push(single_move);
                        move_.len -= 1;
                        move_.from += 1;
                        move_.to += 1;
                        if move_.len == 0 {
                            moves_next = moves_iter.next().copied();
                        }
                    }
                    (None, None) => break,
                }
            }

            (moves, adds)
        }
    }
}
