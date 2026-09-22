# halyard

halyard is a maintained fork of the [Leptos](https://github.com/leptos-rs/leptos) web
framework (MIT, © 2022 Greg Johnston — see [`LICENSE`](./LICENSE) and
[`NOTICE`](./NOTICE)). It was forked on 2026-09-22 from upstream commit `c94f4aefd`
(leptos 0.8.20). Every crate is renamed so nothing collides with crates.io
(`leptos` → `halyard`, `leptos_router` → `halyard_router`, `tachys` → `halyard_tachys`,
…; full map below). The crates are consumed by path/git and are never published.

## Why fork

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
   renders the app on the client instead. The old behaviour is behind the
   `panic-on-hydration-mismatch` feature (`tachys/src/hydration.rs`).
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
| `leptos_actix` (`integrations/actix/`)        | `halyard_actix`                   |
| `leptos_integration_utils` (`integrations/utils/`) | `halyard_integration_utils`  |
| `leptos_server`, `leptos_config`, `leptos_dom`, `leptos_hot_reload` | `halyard_server`, `halyard_config`, `halyard_dom`, `halyard_hot_reload` |
| `tachys`                                      | `halyard_tachys`                  |
| `reactive_graph`, `reactive_stores`, `reactive_stores_macro` | `halyard_reactive_graph`, `halyard_reactive_stores`, `halyard_reactive_stores_macro` |
| `hydration_context`                           | `halyard_hydration_context`       |
| `server_fn`, `server_fn_macro`, `server_fn_macro_default` | `halyard_server_fn`, `halyard_server_fn_macro`, `halyard_server_fn_macro_default` |
| `any_spawner`, `either_of`, `next_tuple`, `or_poisoned`, `const_str_slice_concat` | `halyard_any_spawner`, `halyard_either_of`, `halyard_next_tuple`, `halyard_or_poisoned`, `halyard_const_str_slice_concat` |
| `oco_ref` (`oco/`)                            | `halyard_oco`                     |
| `throw_error` (`any_error/`)                  | `halyard_throw_error`             |
| — (new)                                       | `halyard_macro_diagnostics`       |
| `rstml` 0.12.1, `syn_derive` 0.2.0 (vendored, `third_party/`) | `halyard_rstml`, `halyard_syn_derive` |

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

## Tracking upstream

The `upstream` remote points at `leptos-rs/leptos` (push disabled). Upstream directory names
were kept so that `git fetch upstream && git merge upstream/main` applies cleanly; after a
merge, re-run the rename for any new `leptos` paths and the checks in `.github/workflows/ci.yml`:

```sh
cargo fmt --check
cargo clippy --workspace -- -D warnings
cargo test --workspace
cargo check -p halyard --no-default-features --features hydrate --target wasm32-unknown-unknown
cargo test -p halyard --features ssr --test render_mode
RUSTFLAGS="--cfg erase_components" cargo test -p halyard --features ssr --test render_mode
```

`examples/ssr_modes_axum` is kept as an SSR + hydration smoke test for the sibling build
tool (`cargo-halyard`).

Upstream documentation: <https://book.leptos.dev/> — the API is the same apart from the
crate names and the changes listed above. See [`ARCHITECTURE.md`](./ARCHITECTURE.md) for
how the crates fit together.

## Fork policy: this is our framework now

halyard is **not** a patch set that has to stay mergeable with upstream. It is a
framework we own and improve for our own needs:

- **Refactor freely.** Rename directories, modules, types and features; delete
  what we do not use; restructure crates. Do not hold back a good change to keep
  `git merge upstream/main` clean.
- **Improve as we go.** When our application needs something (a clearer error, a
  safer default, a missing hook), change halyard rather than working around it.
- **Upstream is a source of ideas, not a merge target.** When Leptos ships
  something we want, we read it and port it deliberately — by hand if the trees
  have diverged — and record where it came from. We will worry about that when
  the time comes, not before.
- **Never write upstream.** The `upstream` remote is read-only (its push URL is
  disabled). We do not open issues or pull requests there from this project.
- **Attribution stays.** `LICENSE` (MIT, © 2022 Greg Johnston) and `NOTICE`
  travel with every copy.

## Known issues (fork backlog)

- `cargo test --workspace` fails 8 targets because Cargo unifies the
  `sandboxed-arenas` feature (enabled by the axum integration's tests) into
  crates whose tests assume it is off. Every crate passes when tested on its
  own (`cargo test -p <crate>`), which is how CI runs them. Inherited from
  upstream; to be fixed by making those tests feature-aware.
