# halyard

halyard is a full-stack Rust web framework: server-side rendering, hydration and
fine-grained reactivity, with an axum integration. It is its own project. It began on
2026-09-22 as a fork of [Leptos](https://github.com/leptos-rs/leptos) 0.8.20 (MIT, © 2022
Greg Johnston — see [`LICENSE`](./LICENSE) and [`NOTICE`](./NOTICE)) and no longer tracks
it. Every crate is renamed (`leptos` → `halyard`, `leptos_router` → `halyard_router`,
`tachys` → `halyard_tachys`, …; full map below) and published on crates.io under those
names (`halyard = "0.1"`); its build tool is
[`cargo-halyard`](https://github.com/cgorski/cargo-halyard).

## Why it began as a fork

We hit six defects in production (reproduced in Chromium and WebKit) that are better
fixed at the source than worked around in every application:

1. **WASM file name baked in at compile time.** Upstream decided between `<name>.wasm`
   and `<name>_bg.wasm` with `option_env!("LEPTOS_OUTPUT_NAME")`, so a server built by
   plain `cargo build` requested a file the build tool never wrote (404, hydration never
   ran). halyard resolves the name at **runtime** from `HalyardOptions` (new
   `wasm_file_name` option, default `output_name`); no `option_env!`/`env!` influences
   rendered output any more (`leptos/src/hydration/mod.rs`, `leptos_config`).
2. **`--cfg erase_components` mismatch.** Debug builds run through the build tool are
   compiled with `--cfg erase_components`, which changes the hydration marker comments the
   server emits; a server built without it made the client panic with
   "expected a marker node". halyard records the server's mode in a
   `<meta name="halyard-render-mode">` tag and the client checks it before hydrating: on a
   mismatch it logs **one** clear error naming both modes and how to fix it, then skips
   hydration — no panic (`leptos/src/hydration/mod.rs`, `leptos/src/mount.rs`,
   `leptos/tests/render_mode.rs`).
3. **Hydration mismatches were panics with useless context.** halyard logs the view's
   source location (in debug / `--cfg halyard_debuginfo` builds), what was expected, what
   was found (tag/text snippet) and the DOM path, then abandons hydration cleanly and
   renders the app on the client instead (`tachys/src/hydration.rs`).
4. **Bootstrap script had no rejection handler** (WebKit: "Unhandled Promise Rejection:
   TypeError: Load failed" when navigating away mid-load). The inline script now ends in a
   `.catch` that logs one concise `console.warn`.
5. **WebKit downloaded the WASM twice** because `<link rel="preload" as="fetch">` is not
   matched against wasm-bindgen's `fetch()`. halyard drops the preload and instead starts
   the `fetch()` itself, immediately, from a classic inline script, handing the pending
   `Response` to `init` — exactly one request in every browser, started as early as the
   preload was.
6. **Two unmaintained proc-macro helpers.** `paste` is replaced by
   [`pastey`](https://crates.io/crates/pastey); `proc-macro-error2` is replaced by
   `halyard_macro_diagnostics` (a few dozen lines on `syn::Error::to_compile_error`).
   Because `rstml` pulled `proc-macro-error2` in through `syn_derive`, both are vendored
   under `third_party/` with that dependency removed. Neither crate is in the lockfile.

## Crate map

| upstream (dir kept)                           | halyard                           |
| --------------------------------------------- | --------------------------------- |
| `leptos` (`leptos/`)                          | `halyard`                         |
| `leptos_macro`                                | `halyard_macro`                   |
| `leptos_router` (`router/`)                   | `halyard_router`                  |
| `leptos_router_macro` (`router_macro/`)       | `halyard_router_macro`            |
| `leptos_meta` (`meta/`)                       | `halyard_meta`                    |
| `leptos_axum` (`integrations/axum/`)          | `halyard_axum`                    |
| `leptos_integration_utils` (`integrations/utils/`) | `halyard_integration_utils`  |
| `leptos_server`, `leptos_config`, `leptos_dom` | `halyard_server`, `halyard_config`, `halyard_dom` |
| `tachys`                                      | `halyard_tachys`                  |
| `reactive_graph`                              | `halyard_reactive_graph`          |
| `hydration_context`                           | `halyard_hydration_context`       |
| `server_fn`, `server_fn_macro`, `server_fn_macro_default` | `halyard_server_fn`, `halyard_server_fn_macro`, `halyard_server_fn_macro_default` |
| `any_spawner`, `either_of`, `next_tuple`, `or_poisoned`, `const_str_slice_concat` | `halyard_any_spawner`, `halyard_either_of`, `halyard_next_tuple`, `halyard_or_poisoned`, `halyard_const_str_slice_concat` |
| `oco_ref` (`oco/`)                            | `halyard_oco`                     |
| `throw_error` (`any_error/`)                  | `halyard_throw_error`             |
| — (new)                                       | `halyard_macro_diagnostics`       |
| `rstml` 0.12.1, `syn_derive` 0.2.0 (vendored, `third_party/`) | `halyard_rstml`, `halyard_syn_derive` |

Upstream's actix integration, its view-patching hot reload and its stores crates were
removed because nothing used them. `AutoReload` still reloads the page when the build tool
rebuilds, and swaps the stylesheet when only the CSS changed.

halyard builds a site one way: pages rendered on the server (axum) and hydrated in the
browser with WebAssembly, optionally as islands, with lazy routes and wasm splitting. The
optional extras around that were removed because nothing used them: client-side-only
rendering (the `csr` feature), the executors other than Tokio and wasm-bindgen-futures
(glib, async-executor, the futures thread pool), resource encodings other than serde JSON
(and `ToString`/`FromStr`) and the codec and TLS features that `halyard` forwarded to
server functions, the `nightly` features (calling a signal as a function, static-string
attributes), `subsecond` hot patching, and the `panic-on-hydration-mismatch` features.
Server functions keep their own codec features for now.

Inside `halyard` the re-export names are unchanged: `halyard::tachys`, `halyard::server_fn`,
`halyard::reactive`, `halyard::prelude::*`, and the `view!`, `#[component]`, `#[server]`
macros keep their names. Types named `Leptos*` are now `Halyard*` (`HalyardOptions`,
`HalyardRoutes`, …).

## Configuration

Every runtime setting is read from `HALYARD_<NAME>` with the legacy `LEPTOS_<NAME>` as a
fallback (`HALYARD_OUTPUT_NAME` / `LEPTOS_OUTPUT_NAME`, `..._SITE_ROOT`, `..._SITE_PKG_DIR`,
`..._SITE_ADDR`, `..._RELOAD_PORT`, `..._ENV`, `..._HASH_FILES`, `..._HASH_FILE_NAME`,
`..._WATCH`, and the new `..._WASM_FILE_NAME`), so existing `cargo-leptos` configurations and
deployments keep working. Likewise `get_configuration(Some("Cargo.toml"))` reads
`[package.metadata.halyard]` and falls back to `[package.metadata.leptos]`.

## Checks

What CI runs (`.github/workflows/ci.yml`), each crate tested on its own (see the known
issue below):

```sh
cargo fmt --check
cargo clippy --workspace -- -D warnings
scripts/panic-ratchet.sh       # panic sites per crate may only fall (panic-baseline.txt)
cargo test -p <crate>          # for each crate
cargo check -p halyard --no-default-features --features hydrate --target wasm32-unknown-unknown
cargo test -p halyard --features ssr --test render_mode
RUSTFLAGS="--cfg erase_components" cargo test -p halyard --features ssr --test render_mode
```

Before a release, `cargo package --workspace` packages and verifies every crate against
crates.io the way `cargo publish` will. A dev-dependency on a crate that is published
*later* must be path-only (no `version`), or the publish of the earlier crate fails.

`examples/ssr_modes_axum` is kept as an SSR + hydration smoke test for the sibling build
tool (`cargo-halyard`).

The Leptos book (<https://book.leptos.dev/>) still describes most of the API, apart from
the crate names and the changes listed above. See [`ARCHITECTURE.md`](./ARCHITECTURE.md)
for how the crates fit together.

## Project policy

halyard is our framework, taken wherever the applications built on it need; it does
not follow Leptos.

- **Refactor freely.** Rename, restructure, delete and redesign. Nothing is kept
  mergeable with Leptos, and nothing is merged or ported from it on a schedule.
- **Improve as we go.** When an application needs something (a clearer error, a safer
  default, a missing hook), change halyard rather than working around it.
- **No panics, ever.** A panic in the browser kills the application (release wasm is
  built with `panic = "abort"`); on the server it fails a request. Library code returns
  typed errors (`Result`, `thiserror` enums) or recovers visibly (log, fall back to
  client rendering, render nothing), and uses the strongest types that are practical so
  the impossible states cannot be written. The inherited code does not meet this yet:
  `scripts/panic-ratchet.sh` counts every `unwrap`, `expect`, `panic!`, `unreachable!`,
  `todo!`, unchecked index and unchecked arithmetic per crate, CI fails if a count rises,
  and each crate's lints go to `deny` once its count reaches zero. The hydration and
  server request paths go first.
- **Never write to Leptos.** The old `upstream` remote is push-disabled; we do not open
  issues or pull requests there.
- **Attribution stays.** `LICENSE` (MIT, © 2022 Greg Johnston) and `NOTICE` travel with
  every copy.

## Known issues (fork backlog)

- `cargo test --workspace` fails 8 targets because Cargo unifies the
  `sandboxed-arenas` feature (enabled by the axum integration's tests) into
  crates whose tests assume it is off. Every crate passes when tested on its
  own (`cargo test -p <crate>`), which is how CI runs them. Inherited from
  upstream; to be fixed by making those tests feature-aware.
