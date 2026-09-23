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
after `c426d9a3`. CI fails if a crate's count rises. The table is the first count, plus
what clippy cannot see:

| Class | Where | Count | Reaches users? |
|---|---|---|---|
| `unwrap`/`expect` | everywhere; worst `halyard_tachys` (renderer/dom, keyed/either views), `halyard_router`, `halyard_reactive_graph` | 257 | yes |
| disposed reactive value read (`unwrap_signal!`) | `halyard_reactive_graph` accessors: `get`, `with`, `read`, `write`, `get_value`, `with_value`, `write_value`, `Action::dispatch`, ... | 42 macro sites behind every `.get()` in every app | **yes, the most likely remaining runtime panic** |
| unchecked index / arithmetic | keyed diffing, string builders, macros | 247 | partly |
| `unreachable!`/`panic!`/`todo!` | `AnyView` without its feature, `any_attribute`, `either`, macros | 107 | mostly misuse, some real |
| `RefCell` borrow conflicts | `halyard_tachys` 45, router 8, reactive graph 10 | 65 | yes (re-entrant event handlers) |
| lock held while user code runs | `Callback::run` (`with_value(\|f\| f(input))`), `StoredValue::with_value`, `debounce` (`cb.write().unwrap()(arg)`) | several | yes: re-entry **deadlocks** natively and **aborts** in wasm (std's single-threaded lock calls `rtabort!` on a conflicting acquisition) |
| `unwrap_throw`/`expect_throw` | `halyard_dom` 7, router 2, `halyard` 1 | 10 | yes |
| proc-macro panics | `halyard_macro`, `halyard_rstml`, `halyard_server_fn_macro`, `halyard_hot_reload` | ~120 | compile time only: should be `compile_error!` spans |

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
   and window listeners in `halyard_dom` register their cancellation with the current
   owner too (`debounce` already does). Until this is released, applications should use
   `spawn_local_scoped_with_cancellation` and can forbid the unscoped spawns with
   clippy's `disallowed-methods`.
2. **No user code under a guard.** `Callback`, `StoredValue::with_value`, `debounce`,
   memo and effect runners: take what is needed out of the lock, release it, then call.
3. **The DOM layer returns typed errors.** `Renderer`/`Mountable` operations return
   `Result<_, DomError>`, and the view layer handles a failure by logging it with the
   node's DOM path and keeping the tree consistent (a placeholder, or skipping that
   node), never by panicking. `window()`/`document()` are fetched once at mount, as a
   typed capability, not unwrapped from thread-locals.
4. **Keyed and either views without index arithmetic.** Rewrite the diff over checked
   iteration; on an invariant violation, log it and rebuild the list from scratch.
5. **Server request path.** `halyard_axum`, `halyard_integration_utils`,
   `halyard_server_fn`: every `unwrap` becomes a typed error that renders a 500 with a
   request id.
6. **Macros.** Every macro panic becomes a `syn::Error` on the offending span.
7. **Per crate: panic lints to `deny`** once the crate's count is zero.

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
measure; do it before the application grows (the transaction form's 179 types).

**Decided (2026-09-22): A, then B.** halyard stays a full client framework (SSR plus
hydrated WebAssembly), and the guarantee is by type. The model is the standard library's
`Rc`/`Weak`: a `Copy` arena handle does not keep its value alive, so, like
`Weak::upgrade`, reading through it returns an `Option` (`try_get`, `try_with`, `try_run`,
...). A reference-counted handle (`ArcRwSignal`, `ArcMemo`, `ArcCallback`, ...) keeps its
value alive, so reading through it is total (`get`, `with`, `run`). The panicking
accessors on `Copy` handles are removed, not deprecated. Inside halyard, reactive
rendering of a weak handle whose value is gone renders or updates nothing and logs once.

## Dependencies considered

Well-maintained crates can remove code we would otherwise have to make panic-free, as
long as they stay private implementation details: never in a public halyard type, so
they can be swapped later.

| Crate | Maintained (2026-09) | Use here | Verdict |
|---|---|---|---|
| `slotmap` | 1.1.1, 26 M downloads / 90 d | already the arena; generational keys make a stale handle read as "gone", not as another value | keep |
| `futures` channels | rust-lang, 0.3.34 (2026-08) | oneshot/mpsc in actions, server functions and streaming | keep |
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
- [ ] Changes 1 to 7 above, and B
