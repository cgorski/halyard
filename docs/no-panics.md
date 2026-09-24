# No panics: audit and programme

Written 2026-09-22. halyard's rule is **no panics, ever** (README, "Project policy").
This page records where the inherited code can still panic, what actually caused the
hydration panics we saw, the structural changes that remove whole classes of panic, and
the one API decision that needs the maintainers' agreement before it is made.

## Why it matters

- **Browser:** release wasm is built with `panic = "abort"`. One panic ends the whole
  application: no error boundary, no recovery, a dead page until reload.
- **Server:** panics unwind, so a panic fails one request. But a panic while a
  `std::sync` lock is held *poisons* that lock, and the inherited `or_poisoned()`
  turned every later use of it into another panic. That cascade is fixed (`c426d9a3`:
  a poisoned lock hands back its guard).

## Where the hydration panics came from

Both panics we saw in production (every page, 2026-09-22) were one failure: the server
and the browser rendered with different hydration markers (`--cfg erase_components` on
one build and not the other), and hydration hit a marker it did not expect and panicked
with `{unknown}` as its location. Since the fork:

- the render mode is fingerprinted in the page and checked before hydrating
  (`halyard::mount::check_render_mode`);
- a hydration mismatch is recoverable: the cursor stops reading the server DOM, the
  half-hydrated tree is dropped and the app is rendered on the client
  (`halyard_tachys::hydration`, `halyard::mount::client_render_fallback`), with the view
  location, what was expected, what was found and the DOM path in the console.

No hydration panic has been seen since (Playwright fails a test on any console error, in
Chromium and WebKit). What remains are the panic sites below. Any of them can still end
the app, from a DOM that a browser extension or a translation tool edited under us, from
a disposed signal read after an `await`, or from a re-entrant lock.

## Inventory

`scripts/panic-ratchet.sh` counts panicking constructs in library code, in three builds
(default features; server; browser on wasm32), per crate: 616 at the first count, 613
after `c426d9a3`, 93 on 2026-09-24 (in `halyard_macro` 42, `halyard_tachys` 31, the vendored
`halyard_rstml` 17, `halyard_reactive_graph` 3, `halyard` 0). CI fails if a crate's count
rises. The 36 `unwrap_signal!` sites, which clippy does not see (a macro expanded into every
accessor), are gone with B (2026-09-24). The table is the first count, plus what clippy cannot see. It names the crates of
that time: `halyard_router`, `halyard_dom`, `halyard_axum` and `halyard_integration_utils`
are modules of `halyard` now (`router`, `dom`, `axum`, `integration_utils`), and
`halyard_server_fn_macro` was removed with server functions.

| Class | Where | Count | Reaches users? |
|---|---|---|---|
| `unwrap`/`expect` | everywhere; worst `halyard_tachys` (renderer/dom, keyed/either views), `halyard_router`, `halyard_reactive_graph` | 257 | yes |
| disposed reactive value read (`unwrap_signal!`) | `halyard_reactive_graph` accessors: `get`, `with`, `read`, `write`, `get_value`, `with_value`, `write_value`, `Action::dispatch`, ... | 42 macro sites behind every `.get()` in every app; **0 since B** | was the most likely runtime panic; removed by type (B) |
| unchecked index / arithmetic | keyed diffing, string builders, macros | 247 | partly |
| `unreachable!`/`panic!`/`todo!` | `AnyView` without its feature, `any_attribute`, `either`, macros | 107 | mostly misuse, some real |
| `RefCell` borrow conflicts | `halyard_tachys` 45, router 8, reactive graph 10 | 65 | yes (re-entrant event handlers) |
| lock held while user code runs | `Callback::run` (`with_value(\|f\| f(input))`), `StoredValue::with_value`, `debounce` (`cb.write().unwrap()(arg)`) | several | yes: re-entry **deadlocks** natively and **aborts** in wasm (std's single-threaded lock calls `rtabort!` on a conflicting acquisition) |
| `unwrap_throw`/`expect_throw` | `halyard_dom` 7, router 2, `halyard` 1 | 10 | yes |
| proc-macro panics | `halyard_macro`, `halyard_rstml`, `halyard_server_fn_macro` | ~120 | compile time only: should be `compile_error!` spans |

## Is the poisoning a smell?

Poisoning itself is a symptom: it only happens *after* a panic while a lock is held, and
we have never seen one (no panic or poison in any server log to date). The smells behind
it are real, though:

1. **Panics** (the whole programme).
2. **User code runs while framework locks are held.** A callback that calls itself, or a
   stored closure that updates the value it is stored in, re-enters the same lock: a
   deadlock on the server, an abort in the browser. This is a design defect, not a
   poisoning issue, and it is fixed by never calling out under a guard: clone the
   `Arc<dyn Fn>` out, drop the guard, then call.
3. **`std::sync::RwLock` in single-threaded wasm.** The reactive graph uses thread-safe
   locks so the same types serve the multi-threaded server. In the browser they are
   pure overhead plus an abort-on-re-entry trap. A storage abstraction that is `RefCell`
   with `try_borrow` in the browser and a lock on the server (the graph already has
   `SyncStorage`/`LocalStorage`) would make re-entry a handled `Err`, not an abort.

## Structural changes, in order

Each change removes a class of panic, has tests that fail without it, and lowers the
ratchet.

1. **Owner-bound tasks by default.** Most disposed-signal reads come from a task that
   outlives its component and reads a signal after an `.await`.
   `task::spawn_local` becomes owner-scoped and cancelled on the owner's cleanup;
   `spawn_local_detached` is the explicit opt-out. Timers, intervals, animation frames
   and window listeners in `halyard::dom` register their cancellation with the current
   owner too (`debounce` already does). Until this is released, applications should use
   `spawn_local_scoped_with_cancellation` and can forbid the unscoped spawns with
   clippy's `disallowed-methods`.
2. **No user code under a guard.** `Callback`, `StoredValue::with_value`, `debounce`,
   memo and effect runners: take what is needed out of the lock, release it, then call.
   For signals this is done: see "Re-entrant access to a signal" below.
3. **The DOM layer returns typed errors.** `Renderer`/`Mountable` operations return
   `Result<_, DomError>`, and the view layer handles a failure by logging it with the
   node's DOM path and keeping the tree consistent (a placeholder, or skipping that
   node), never by panicking. `window()`/`document()` are fetched once at mount, as a
   typed capability, not unwrapped from thread-locals.
4. **Keyed and either views without index arithmetic.** Rewrite the diff over checked
   iteration; on an invariant violation, log it and rebuild the list from scratch.
5. **Server request path.** `halyard::axum` and `halyard::integration_utils`: every
   `unwrap` becomes a typed error that renders a 500 with a request id.
6. **Macros.** Every macro panic becomes a `syn::Error` on the offending span.
7. **Per crate: panic lints to `deny`** once the crate's count is zero.

## Re-entrant access to a signal

Decided and implemented 2026-09-24 (`halyard_reactive_graph`: `reentry.rs`,
`signal/commit.rs`; tests in `tests/reentrant_writes.rs` and `tests/reentry.rs`). A signal
can be reached again by code that is already using it: a read inside its own `update`, a
`set` inside its own `with`, a memo or an effect that writes what it reads. That never
aborts, deadlocks or panics. The rule, for applications as for halyard itself:

1. **An update works on a copy.** `update(|v| ...)` and `maybe_update` clone the committed
   value, run the closure outside every lock, then commit the result and notify the
   subscribers once. Inside the closure, reading the same signal (`get`, `with`, `read`, or
   indirectly through memos and derived signals) gives the last committed value. `update`
   needs `T: Clone`.
2. **A write never lands in the middle of a read.** A write (`set`, `update`, `notify`, the
   drop of a write guard) made while this thread is using the signal (inside its `with`,
   while a read or write guard of it is alive, inside its `update`) is deferred, and applied
   in order when this thread's outermost use of the signal ends. A deferred `update` runs its
   closure at once, on the value it will be committed over (the committed value and this
   thread's earlier deferred writes), and defers the result. So an `update` nested in an
   `update` of the same signal starts from the same committed value as the outer one and is
   committed after it, replacing its result; that is almost always a mistake, and it is
   logged (once).
3. **A write guard holds a copy.** `write()` returns a guard over a clone of the value,
   committed (or deferred, as above) when it is dropped. Nothing is locked while it lives,
   so the signal can still be read (the committed value) and written (deferred).
   `write()` needs `T: Clone`.
4. **Values that are not `Clone` change in place.** `set` works for every value, like any
   write. `try_update`, `update_untracked` and the untracked guard (`write_untracked`)
   change the value in place, under its lock: started while this thread is using the
   signal, they return `None` without running; while they run, the signal cannot be read
   on this thread (the `try_*` reads give `None`, logged once). Do not read a signal from
   inside its own in-place update.
5. **Threads serialize their writes.** Each signal has a *writer turn*, a mutex that is
   not the value's lock: one thread writes at a time, so concurrent updates never lose one
   another's changes. Reads never wait for an update's closure; they wait only while
   another thread swaps a new value in. Nested reads on one thread share one lock guard.

How: every thread keeps a record of the signals it is using (keyed by the address of the
value's lock), with the writes it has deferred. It is consulted before any lock is tried,
so the browser's single-threaded lock never sees a conflicting acquisition, and natively no
thread waits for itself. Writes from other threads wait for their turn as before.

In short, for application code: read a signal anywhere, write it anywhere; a write never
takes effect in the middle of this thread's read of the same signal, and an update's
closure sees the value as it was committed. Stored values (`StoredValue`) are not signals:
their closures run on the borrowed value, and reaching the same value again from there is
refused (`None` from the `try_*` forms). Resources and async derived values change in
place; reaching one again from inside its own update is refused rather than waited for
(natively it used to deadlock).

## Decision needed: reading a disposed signal

`count.get()` on a signal whose owner is gone has no value to return. Leptos panics there
and offers `try_get()`; Dioxus's `read()` does the same with `try_read()`. Changes 1 and
2 remove most ways to get there, but not the possibility. The options:

- **A. Keep `get()` and close the doors.** Tasks, timers and listeners become owner-bound
  (change 1), and the remaining ways a handle can outlive its owner are named in the docs.
  `get()` still panics if someone detaches on purpose. Smallest change; not "never".
- **B. Make the infallible accessors unreachable where they can fail.** Reference-counted
  signals (`ArcRwSignal`, ...) cannot be disposed while a handle exists, so `get()` on
  them is total. The arena (`Copy`) signals keep only `try_get()` (an `Option`).
  Components use the `Arc` forms (a `.clone()` per closure instead of `Copy`), or
  `try_get()` where a `Copy` handle is worth it. Removes the panic by construction. The
  cost is ergonomics: every page is touched, about 15 today.
- **C. Scope lifetimes.** Signals borrow a `Scope<'a>`, so a handle cannot outlive its
  owner (early Leptos, Sycamore). Removes the panic at compile time, but it is a rewrite
  of the reactive system and of every component, and costs the most in ergonomics.

Recommendation: **A now, B next.** A is needed under both B and C, and lands without
touching application code. B is the one that makes the guarantee total at a cost we can
measure; do it before applications grow many more components that use the accessors.

**Decided (2026-09-22): A, then B.** halyard stays a full client framework (SSR plus
hydrated WebAssembly), and the guarantee is by type. The model is the standard library's
`Rc`/`Weak`: a `Copy` arena handle does not keep its value alive, so, like
`Weak::upgrade`, reading through it returns an `Option` (`try_get`, `try_with`, `try_run`,
...). A reference-counted handle (`ArcRwSignal`, `ArcMemo`, `ArcCallback`, ...) keeps its
value alive, so reading through it is total (`get`, `with`, `run`). The panicking
accessors on `Copy` handles are removed, not deprecated. Inside halyard, reactive
rendering of a weak handle whose value is gone renders or updates nothing and logs once.

## Design: weak arena handles (B)

Written 2026-09-24 and implemented the same day; where the implementation departs from the
text below, "As implemented" (at the end of this section) says how and why. It builds on
"Re-entrant access to a signal".

### The rule

- A `Copy` arena handle is **weak**, like `std::rc::Weak`: it does not keep its value
  alive. Every accessor that returns a value and would have to panic when the value is
  gone is removed; its `try_*` form (an `Option`) stays.
- A reference-counted handle (`Arc...`) is **strong**: every read through it is total.
- A write through a weak handle whose value is gone (`set`, `update`, `notify`, `dispatch`,
  `refetch`, running a callback that returns `()`) does nothing, and that is logged once.
- Rendering a weak handle whose value is gone (as a `view!` child or attribute value)
  renders or updates nothing, and that is logged once.
- `weak.upgrade() -> Option<Arc...>` replaces the conversions `From<weak> for Arc...`,
  which panic today; `From<Arc...> for weak` (a downgrade) stays.
- In debug builds, a `try_*` read of a gone weak handle is logged once (creation site and
  call site), so that code which quietly skips work because of it is still visible.

### Types and methods

"Reads" are `get`, `get_untracked`, `with`, `with_untracked`, `read`, `read_untracked`;
"try reads" are their `try_*` forms. The 36 `unwrap_signal!` sites of today (13 default
methods in `traits.rs`, 4 in `trait_options.rs`, 6 `From` conversions to `Arc` forms, 8
`Action` methods, 2 `MultiAction` methods, 3 `AsyncDerived` methods) all go.

| Weak (`Copy`) type | Removed | Kept | Changed or added | Strong form |
|---|---|---|---|---|
| `RwSignal`, `MappedSignal` | reads, `write`, `write_untracked`, `update_untracked`, `From` to `Arc` | try reads, `try_write`, `try_write_untracked`, `try_update_untracked`, `set`, `try_set`, `update`, `maybe_update`, `try_update`, `try_maybe_update`, `notify`, `track`, `read_only`, `write_only`, `split`, `unite`, `dispose` | writes to a gone value logged; `upgrade` | `ArcRwSignal`, `ArcMappedSignal` |
| `ReadSignal` | reads, `From` | try reads, `track` | `upgrade` | `ArcReadSignal` |
| `WriteSignal` | `write`, `write_untracked`, `update_untracked`, `From` | `set`, `update` and their `try_*` forms, `try_write`, `notify` | writes logged when gone; `upgrade` | `ArcWriteSignal` |
| `Memo` | reads, `From` | try reads | `upgrade`; `Memo::new_try` (closure returns `Option`) | `ArcMemo` |
| `Signal` | reads, `From` to `ArcSignal` | try reads | `upgrade`; `Signal::derive_try`; `map` | `ArcSignal` (built only from strong sources) |
| `MaybeProp` | reads | try reads | renders directly (nothing when unset or gone) | none needed |
| `StoredValue` | `get_value`, `with_value`, `read_value`, `write_value`, `From` | `try_get_value`, `try_with_value`, `try_read_value`, `try_write_value`, `set_value`, `update_value` and their `try_*` forms | `upgrade` | `ArcStoredValue` |
| `Callback`, `UnsyncCallback` | `run` when `Out` is not `()` | `try_run`; `run` for `Out = ()` (a write: logged no-op when gone) | `upgrade` | new `ArcCallback`, `ArcUnsyncCallback` (total `run`) |
| `Action` | nothing | `dispatch`, `dispatch_local`, `version`, `pending`, `input`, `value` | gone: `dispatch` is a logged no-op returning an inert abort handle; the accessors return gone weak handles (logged) instead of panicking | `ArcAction` |
| `MultiAction` | nothing | `dispatch`, `submissions`, `version` | as `Action` | `ArcMultiAction` |
| `AsyncDerived` | reads, `From` | try reads, `.await`, `ready`, `by_ref` | gone: the futures stay pending (logged once); they are awaited by code owned with the value, which is cancelled with it | `ArcAsyncDerived` |
| `Resource`, `LocalResource`, `OnceResource` | reads | try reads, `.await`, `refetch` | as `AsyncDerived`; `refetch` logged when gone | `ArcResource`, `ArcLocalResource`, `ArcOnceResource` |
| `NodeRef` | reads, `write`, `write_untracked` | try reads, `on_load`, `set` | `element() -> Option<E::Output>`: `None` when not mounted or gone (both mean "no element") | none needed |
| `Trigger`, `SignalSetter` | nothing (no value to return) | `track`, `notify`, `set` | logged no-op when gone | `ArcTrigger` |
| Router: `use_params_map`, `use_params`, `use_query_map`, `use_query`, `use_matched` (`Memo`), `use_url` (`ReadSignal`), `use_location` (`Location`: four `Memo`s and a `ReadSignal`), `query_signal` (`Memo`, `SignalSetter`) | as the types they return | as the types they return | signatures unchanged: the handles belong to the calling component | none needed |
| `Option<weak handle>` (`trait_options.rs`) | reads | try reads | | |

### Structural obstacles

1. **The accessor traits are blanket traits** (`Get` for every `With`, `With` for every
   `Read`, ...), and each holds both the total and the `try_*` form. They split: `TryGet`,
   `TryWith`, `TryRead` (and `_untracked`, `Value` forms) for every readable handle, and
   `Get`, `With`, `Read` only for strong handles, closures and plain values (a sealed
   `Strong` marker in the blanket impls). A call of `get` on a weak handle then fails to
   compile, with a `#[diagnostic::on_unimplemented]` message that names `try_get`, the
   view, and `upgrade`. Generic code bounded on `Get`/`With` (about 110 bounds in the
   library) moves to the `Try*` traits.
2. **Rendering.** The `Render`/`RenderHtml`/attribute impls for signals in
   `halyard_tachys::reactive_graph` (33 call sites, behind `#[allow(deprecated)]`) read
   with `get`; they read with `try_get` and render nothing when it is `None`.
3. **`Signal` and `ArcSignal` wrap derived closures** as well as handles. A closure that
   reads weak handles is fallible, so `Signal::derive_try(|| Some(a.try_get()? + 1))`
   joins `Signal::derive`, and `ArcSignal` is built only from strong sources.
4. **Strong reads and in-place writes.** Under "Re-entrant access", an in-place update
   (`try_update`, `update_untracked`, `write_untracked`) lends out the value: a read of the
   same signal on the same thread inside it has nothing to return. For strong reads to be
   total, the in-place forms exist only on weak handles (where every read is a `try_*`);
   strong handles write through a copy (`update`, `write()`) or a replacement (`set`). A
   value that is not `Clone` and must change in place is changed through its weak handle
   (`arc.downgrade().try_update(...)`) or kept in a `StoredValue`.
5. **Serde impls** of weak handles serialize through `try_with`; a gone value is a
   serialization error, not a panic.

### What an application writes

The cost of B falls on application code, so halyard adds what keeps it short, without a
panic and without a default that hides a gone value:

- **Handles as view values.** A weak handle is a `view!` child (`{count}`), an attribute
  or property value (`prop:value=name`, `disabled=busy`, `class:active=selected`), and
  the source of `<For each=items>` and `<Show when=flag>`. When its value is gone it
  renders nothing (logged once). Most `move || x.get()` closures disappear.
- **`map` for derived values.** `count.map(|n| n * 2)` and, over a tuple of handles,
  `(name, email).map(|(name, email)| ...)` give a derived `Signal` (the closure gets
  references, nothing is cloned) that is gone when any source is gone; `.memo(...)` gives
  a memoised one. With `?`, any shape is possible: `Signal::derive_try(move ||
  Some(a.try_get()? + b.try_get()?))`.
- **Event handlers.** Writes are unchanged (`set`, `update` are total: a gone value is a
  logged no-op), and so are `dispatch` and callbacks returning `()`. Reads use a tuple
  `try_get` and `let ... else`: `let Some((name, email)) = (name, email).try_get() else
  { return };`. A handler runs only while its view is mounted, so this `else` is taken
  only when the handler reads a handle owned by a part of the page that is already gone.
- **Async tasks.** Read what the task needs before its first `.await`. After an `.await`,
  read with `try_*` (the component may be gone: then there is nothing to update), or
  take `upgrade()` before the `.await` when the task must still see the value. With
  owner-bound tasks (change 1) the task is cancelled with its component, so the `None`
  branch is rare.
- **Strong handles and a clone helper** are for state shared beyond one component
  (contexts, stores, long-lived tasks): `ArcRwSignal` plus `clone!(a, b => move |_| ...)`
  to capture clones. As the default inside components they cost one clone per closure,
  which the items above avoid.
- Not offered: `get_or_default`/`unwrap_or` accessors (a silent default), or a panicking
  `get` in debug builds (still a panic).

A form with two fields, a derived `valid` memo, a Save button that dispatches an async
action and reads a field after the `.await`, and a list. Today:

```rust
#[component]
pub fn ContactForm(contacts: RwSignal<Vec<Contact>>) -> impl IntoView {
    let name = RwSignal::new(String::new());
    let email = RwSignal::new(String::new());
    let valid = Memo::new(move |_| {
        !name.get().trim().is_empty() && email.with(|e| e.contains('@'))
    });
    let save = Action::new(move |contact: &Contact| {
        let contact = contact.clone();
        async move {
            let saved = api::save(contact).await?;
            if email.get() == saved.email {          // panics if the form is gone
                name.set(String::new());
                email.set(String::new());
            }
            contacts.update(|list| list.push(saved));
            Ok::<_, ApiError>(())
        }
    });
    view! {
        <form on:submit=move |ev| {
            ev.prevent_default();
            save.dispatch(Contact { name: name.get(), email: email.get() });
        }>
            <input prop:value=move || name.get()
                on:input=move |ev| name.set(event_target_value(&ev)) />
            <input prop:value=move || email.get()
                on:input=move |ev| email.set(event_target_value(&ev)) />
            <button disabled=move || !valid.get() || save.pending().get()>"Save"</button>
        </form>
        <ul>
            <For each=move || contacts.get() key=|c| c.email.clone() let:contact>
                <li>{contact.name}" <"{contact.email}">"</li>
            </For>
        </ul>
    }
}
```

Under B (same handles, same number of lines):

```rust
#[component]
pub fn ContactForm(contacts: RwSignal<Vec<Contact>>) -> impl IntoView {
    let name = RwSignal::new(String::new());
    let email = RwSignal::new(String::new());
    let valid = (name, email)
        .memo(|(name, email)| !name.trim().is_empty() && email.contains('@'));
    let save = Action::new(move |contact: &Contact| {
        let contact = contact.clone();
        async move {
            let saved = api::save(contact).await?;
            // the form may be gone by now: then there is nothing to clear
            if email.try_with(|e| *e == saved.email) == Some(true) {
                name.set(String::new());
                email.set(String::new());
            }
            contacts.update(|list| list.push(saved)); // a logged no-op if gone
            Ok::<_, ApiError>(())
        }
    });
    view! {
        <form on:submit=move |ev| {
            ev.prevent_default();
            let Some((name, email)) = (name, email).try_get() else { return };
            save.dispatch(Contact { name, email });
        }>
            <input prop:value=name
                on:input=move |ev| name.set(event_target_value(&ev)) />
            <input prop:value=email
                on:input=move |ev| email.set(event_target_value(&ev)) />
            <button disabled=(valid, save.pending()).map(|(v, p)| !v || *p)>"Save"</button>
        </form>
        <ul>
            <For each=contacts key=|c| c.email.clone() let:contact>
                <li>{contact.name}" <"{contact.email}">"</li>
            </For>
        </ul>
    }
}
```

The one read that could panic today (after the `.await`) is now explicit; the view
closures are gone; the handler says what happens if its fields are gone.

### Size of the change

Counted by compiling halyard with `#[deprecated]` twins of the removed accessors on every
weak type (a scratch copy; every feature set: default, `ssr`, `axum`, and the browser
build with islands; `#[allow(deprecated)]` lifted), on 2026-09-24:

| Where | Call sites |
|---|---|
| Library code | 54: rendering of signals in `halyard_tachys` 33, router 13, `AnimatedShow` 4, resources 1, `TextProp` 1, callbacks 2 |
| Tests | 181: `halyard_reactive_graph` 141, `halyard_macro` 18, `halyard` 22 |
| Example app (`examples/`, outside the workspace; by search) | 5 |
| Doc examples (by search; also counts strong handles, so an upper bound) | at most 299, most in `halyard_reactive_graph` |

Plus the 36 accessor definitions and about 110 generic bounds (obstacle 1). The work, in
order: the trait split and `upgrade`/`downgrade` (2 to 3 days, most of it in
`halyard_reactive_graph` and its tests); rendering of handles, `map`/`memo`, tuple
`try_get`, `For`/`Show` sources, `ArcCallback` (2 days); halyard's own call sites, doc
examples and tests (2 days); then each application (about 15 pages today: mostly
mechanical, a view closure becoming the handle, a handler read gaining a `let ... else`).

### As implemented

The design above is implemented as written, with these changes and details:

- **Strong reads are total except in a cycle.** A weak and a strong handle can share a
  value (`upgrade`, `downgrade`), so a strong read inside the value's own in-place change
  (made through the weak handle, obstacle 4) has nothing to read; nor has a memo read inside
  its own computation, or (on a server) a memo that another thread is recomputing. Rather
  than a panic or a default, the total forms (`get`, `with`, `read`, their `_untracked` and
  `Value` forms, `write`, `write_value`) are built on the `try_*` form and, if it is `None`,
  report once and wait for the value (`gone::wait_for`). Only the cross-thread case can end;
  the same-thread cases are cycles in the application's code, documented on `Strong`.
  Refusing in-place changes while strong handles exist would close the first case, but needs
  a count of strong handles that the arena's own copies do not hold; not done.
- **Traits.** `TryReadUntracked`, `TryRead`, `TryWithUntracked`, `TryWith`,
  `TryGetUntracked`, `TryGet`, `TryReadValue`, `TryWithValue`, `TryGetValue` for every
  readable handle; `Read`, `ReadUntracked`, `With`, `WithUntracked`, `Get`, `GetUntracked`,
  `ReadValue`, `WithValue`, `GetValue`, `StrongWrite` (`write`) and `StrongWriteValue`
  (`write_value`) only for `Strong` types. The sealed markers `Strong` and `Weak` are
  implemented with `impl_strong!`/`impl_weak!` (doc hidden, so that `halyard` and
  `halyard_tachys` can mark their handles). The in-place forms are `UpdateInPlace`
  (`try_update`, `try_maybe_update`), `UpdateUntracked` (`try_update_untracked`) and
  `WriteUntracked` (`try_write_untracked`), for `Weak` types only; the `Write` hook they use
  is `try_write_in_place` (doc hidden). Closures and plain values do not get `Get`: a
  closure is called, a plain value is used as it is.
- **`downgrade()`** is added to the `Arc` signal, memo, stored-value and async-derived types
  (next to the `From` conversions), since obstacle 4 recommends it.
- **Derived values.** `Map::map`/`Map::memo` over a handle or a tuple of up to 8 handles
  (trait `TryWithAll`), and `TryGetAll::try_get` for tuples. A memo made with
  `Memo::new_try` (so also `.memo(...)` and the `memo!` macro) and a signal made with
  `Signal::derive_try` (so also `.map(...)`) have no strong form: `upgrade` gives `None`,
  because a strong handle to them could not read total.
- **Rendering.** A weak handle rendered as a child or as an attribute, property, class,
  style or inner-HTML value renders as an `Option` of its value: nothing when gone
  (reported once per handle). A class toggle (`class:name=flag`) renders without the
  class, and a `TextProp` made from a handle renders empty text. `<Show when>`, `<For each>`
  and `<ForEnumerate each>` take a closure or a handle (trait `ViewSource`): when gone,
  `<Show>` renders nothing (neither children nor fallback) and `<For>` renders no rows.
  `<ShowLet some=signal>` also renders nothing when the signal is gone.
- **Resources.** Their `From<weak> for Arc...` conversions did not panic (they gave a
  resource that never loads, with a warning); they are replaced by `upgrade()` all the
  same, so that a gone resource is visible as `None`. `refetch` is an `update` (a logged
  no-op when gone).
- **Callbacks.** `Callable` keeps `try_run`; the total `run` is the `Run` trait, for
  `ArcCallback`/`ArcUnsyncCallback` (any output) and for `Callback`/`UnsyncCallback` whose
  output is `()`.
- **Not done:** the debug-build report of a `try_*` read of a gone weak handle (the last
  bullet of "The rule"): the `try_*` result says so already, and every place that reads
  through the arena would have to carry the call site. `gone::report_gone_read` is there for
  it.

## Dependencies considered

Well-maintained crates can remove code we would otherwise have to make panic-free, as
long as they stay private implementation details: never in a public halyard type, so
they can be swapped later.

| Crate | Maintained (2026-09) | Use here | Verdict |
|---|---|---|---|
| `slotmap` | 1.1.1, 26 M downloads / 90 d | already the arena; generational keys make a stale handle read as "gone", not as another value | keep |
| `futures` channels | rust-lang, 0.3.34 (2026-08) | oneshot/mpsc in actions and streaming | keep |
| `gloo-timers`, `gloo-events` | rustwasm, 0.4/0.3 (2026-03) | timers and listeners that cancel on `Drop` (RAII), which is change 1 by construction; `gloo-utils`/`gloo-net` are already dependencies | adopt privately for change 1 |
| `parking_lot` | 0.12.5, 218 M / 90 d | no poisoning, smaller, `read_recursive`; but in single-threaded wasm a conflicting acquisition parks forever (a hang, worse than an abort) | server-side locks only, if at all; the browser wants `RefCell` + `try_borrow` (smell 3) |
| `arc-swap` | 1.9.2, 85 M / 90 d | lock-free read-mostly state (route tables, configuration): no guard to hold across user code | candidate for change 2 where the data is read-mostly |
| `flume`, `async-channel`, `crossbeam-channel` | all healthy | multi-producer/multi-consumer channels | not needed: halyard has no MPMC use, and `futures` already covers oneshot/mpsc; adding one would be a second channel type with no gain |
| distributed primitives (job queues, pub/sub) | n/a | none: halyard is an in-process framework, and the application keeps its state in Postgres by design (plan D5, D12) | not applicable |

## Status

- [x] Ratchet in CI (`scripts/panic-ratchet.sh`, `panic-baseline.txt`)
- [x] `or_poisoned` recovers instead of panicking (removes the panic at 228 call sites)
- [x] Interim guidance for applications: owner-scoped spawns
      (`spawn_local_scoped_with_cancellation`, the unscoped ones forbidden by lint), and
      `try_with_value` for a read after an `await`
- [x] Decision: A then B (all-Rust, hardened)
- [x] Change 2 for signals: re-entrant access is total (updates on a copy, deferred
      writes, write guards on a copy, writer turn); `AsyncDerived` re-entry no longer
      deadlocks natively; `batch` of `ImmediateEffect`s is per thread
- [x] Design of B written ("Design: weak arena handles (B)"), with its size
- [x] B implemented: weak `Copy` handles (`try_*` reads, logged no-op writes, rendering
      nothing when gone, `upgrade`), strong `Arc` handles (total reads), no
      `unwrap_signal!` left; halyard's own call sites, tests, doc examples and example
      converted ("As implemented" lists the departures from the design)
- [ ] Changes 1 to 7 above (2 remains for `StoredValue` closures and the DOM layer)
