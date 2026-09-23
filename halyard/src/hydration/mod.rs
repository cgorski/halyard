#![allow(clippy::needless_lifetimes)]

use crate::{prelude::*, WasmSplitManifest};
use halyard_config::HalyardOptions;
use halyard_macro::{component, view};
use std::{path::PathBuf, sync::OnceLock};

/// Inserts auto-reloading code used in `cargo-halyard`.
///
/// This should be included in the `<head>` of your application shell during development.
#[component]
pub fn AutoReload(
    /// Whether the file-watching feature should be disabled.
    #[prop(optional)]
    disable_watch: bool,
    /// Configuration options for this project.
    options: HalyardOptions,
) -> impl IntoView {
    (!disable_watch && halyard_config::halyard_env_is_set("WATCH")).then(|| {
        #[cfg(feature = "nonce")]
        let nonce = crate::nonce::use_nonce();
        #[cfg(not(feature = "nonce"))]
        let nonce = None::<()>;

        let reload_port = match options.reload_external_port {
            Some(val) => val,
            None => options.reload_port,
        };
        let protocol = match options.reload_ws_protocol {
            halyard_config::ReloadWSProtocol::WS => "'ws://'",
            halyard_config::ReloadWSProtocol::WSS => "'wss://'",
        };

        let script = format!(
            "(function (reload_port, protocol) {{ {} {} }})({reload_port:?}, \
             {protocol})",
            halyard_hot_reload::HOT_RELOAD_JS,
            include_str!("reload_script.js")
        );
        view! { <script nonce=nonce>{script}</script> }
    })
}

/// The view-tree representation this build of halyard uses: `"erased"` when compiled with
/// `--cfg erase_components` (the default for debug builds under `cargo-halyard`), `"typed"`
/// otherwise.
///
/// The two representations emit different hydration marker comments, so the server that
/// renders the HTML and the WASM that hydrates it must use the same one. [`HydrationScripts`]
/// records the server's mode in a [`RENDER_MODE_META_NAME`] `<meta>` tag and the client checks
/// it in `crate::mount::hydrate_body` & friends (`hydrate` feature) before touching the DOM.
pub const RENDER_MODE: &str = if cfg!(erase_components) {
    "erased"
} else {
    "typed"
};

/// The `name` of the `<meta>` tag whose `content` is the server's [`RENDER_MODE`].
pub const RENDER_MODE_META_NAME: &str = "halyard-render-mode";

/// Inserts hydration scripts that add interactivity to your server-rendered HTML.
///
/// This should be included in the `<head>` of your application shell.
///
/// Besides the bootstrap script it emits a `<meta name="halyard-render-mode">` tag recording
/// whether this server binary was compiled with `--cfg erase_components`; the client refuses
/// to hydrate (with one clear console error) if its own mode differs, since the two modes
/// produce different hydration markers.
#[component]
pub fn HydrationScripts(
    /// Configuration options for this project.
    options: HalyardOptions,
    /// Should be `true` to hydrate in `islands` mode.
    #[prop(optional)]
    islands: bool,
    /// Should be `true` to add the “islands router,” which enables limited client-side routing
    /// when running in islands mode.
    #[prop(optional)]
    islands_router: bool,
    /// A base url, not including a trailing slash
    #[prop(optional, into)]
    root: Option<String>,
) -> impl IntoView {
    static SPLIT_MANIFEST: OnceLock<Option<WasmSplitManifest>> =
        OnceLock::new();

    if let Some(splits) = SPLIT_MANIFEST.get_or_init(|| {
        load_split_manifest(&options, root.as_deref().unwrap_or_default())
            .unwrap_or_else(|err| {
                halyard::logging::error!(
                    "[halyard] Rendering pages without preloads for lazy-loaded \
                     code: {err}"
                );
                None
            })
    }) {
        provide_context(splits.clone());
    }

    let (js_file_name, wasm_file_name) = hydration_file_names(&options);

    let pkg_path = &options.site_pkg_dir;
    #[cfg(feature = "nonce")]
    let nonce = crate::nonce::use_nonce();
    #[cfg(not(feature = "nonce"))]
    let nonce = None::<String>;
    let script = if islands {
        if let Some(sc) = Owner::current_shared_context() {
            sc.set_is_hydrating(false);
        }
        include_str!("./island_script.js")
    } else {
        include_str!("./hydration_script.js")
    };

    let islands_router = islands_router
        .then_some(include_str!("./islands_routing.js"))
        .unwrap_or_default();

    let root = root.unwrap_or_default();
    // Notes on the markup below:
    // * There is deliberately no `<link rel="preload" as="fetch">` for the
    //   WASM file: WebKit does not match such a preload against the `fetch()`
    //   that wasm-bindgen performs, so Safari downloaded the binary twice.
    //   Instead the bootstrap script starts the `fetch()` itself and passes
    //   the pending `Response` to `init`, which is one request everywhere.
    // * The bootstrap is a classic (non-module) inline script so that it runs
    //   as soon as it is parsed and can kick off the JS + WASM downloads while
    //   the rest of the document (possibly a slow stream) is still arriving;
    //   it waits for `DOMContentLoaded` itself before calling `hydrate()`.
    view! {
        <meta name=RENDER_MODE_META_NAME content=RENDER_MODE/>
        <link rel="modulepreload" href=format!("{root}/{pkg_path}/{js_file_name}.js") crossorigin=nonce.clone()/>
        <script nonce=nonce>
            {format!("{script}({root:?}, {pkg_path:?}, {js_file_name:?}, {wasm_file_name:?});{islands_router}")}
        </script>
    }
}

/// Resolves the JS and WASM file stems (without extension) that the client
/// should load, entirely from runtime configuration.
///
/// * The JS file is always `<output_name>[.<hash>]`.
/// * The WASM file is `<wasm_file_stem>[.<hash>]`, where `wasm_file_stem` is
///   [`HalyardOptions::wasm_file_name`] if set and `output_name` otherwise.
///
/// Upstream Leptos chose between `<name>.wasm` and `<name>_bg.wasm` based on
/// `option_env!("LEPTOS_OUTPUT_NAME")`, i.e. on the environment of the
/// *compiler* that built the server binary. A server built with plain
/// `cargo build` therefore requested `_bg.wasm` while the build tool had
/// written `.wasm`, the request 404ed, and hydration never ran. Nothing here
/// depends on compile-time environment variables any more.
///
/// If file hashing is on but the hash file is missing or cannot be read, one error is logged
/// and the unhashed names are returned.
pub fn hydration_file_names(options: &HalyardOptions) -> (String, String) {
    let mut js_file_name = options.output_name.to_string();
    let mut wasm_file_name = options.wasm_file_stem().to_string();
    if options.hash_files {
        let hash_path = hash_file_path(options);
        if hash_path.exists() {
            match read_hash_file(hash_path) {
                Ok(hashes) => {
                    for line in hashes.lines() {
                        let line = line.trim();
                        if !line.is_empty() {
                            if let Some((file, hash)) = line.split_once(':') {
                                if file == "js" {
                                    js_file_name
                                        .push_str(&format!(".{}", hash.trim()));
                                } else if file == "wasm" {
                                    wasm_file_name
                                        .push_str(&format!(".{}", hash.trim()));
                                }
                            }
                        }
                    }
                }
                Err(err) => halyard::logging::error!(
                    "[halyard] Using the unhashed JS and WASM file names: {err}"
                ),
            }
        } else {
            halyard::logging::error!(
                "File hashing is active but no hash file was found"
            );
        }
    }
    (js_file_name, wasm_file_name)
}

/// A server-side file that [`HydrationScripts`] or [`hydration_file_names`] needs could not be
/// used. The failure is logged and the page is still rendered: without the hash file, with the
/// unhashed file names; without the split manifest (read once per process), with no preloads
/// for lazy-loaded code.
#[derive(Debug, thiserror::Error)]
enum HydrationFileError {
    /// `hash_files` is on, but the hash file could not be read.
    #[error("could not read the file-hash file {}: {source}", path.display())]
    ReadHashFile {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The wasm-split manifest exists but is not valid JSON of the expected shape.
    #[error("could not parse the wasm-split manifest {}: {source}", path.display())]
    ParseSplitManifest {
        path: PathBuf,
        source: serde_json::Error,
    },
}

/// Where the hash file written by the build tool is: next to the server binary.
fn hash_file_path(options: &HalyardOptions) -> PathBuf {
    std::env::current_exe()
        .map(|path| path.parent().map(|p| p.to_path_buf()).unwrap_or_default())
        .unwrap_or_default()
        .join(options.hash_file.as_ref())
}

fn read_hash_file(path: PathBuf) -> Result<String, HydrationFileError> {
    std::fs::read_to_string(&path)
        .map_err(|source| HydrationFileError::ReadHashFile { path, source })
}

/// Loads the wasm-split manifest (the preloads for lazy-loaded code) for [`HydrationScripts`].
/// `Ok(None)` if there is no manifest, i.e. the application has no lazy-loaded code.
fn load_split_manifest(
    options: &HalyardOptions,
    root: &str,
) -> Result<Option<WasmSplitManifest>, HydrationFileError> {
    let (wasm_split_js, wasm_split_manifest) = if options.hash_files {
        let hashes = read_hash_file(hash_file_path(options))?;

        let mut split = "__wasm_split.______________________.js".to_string();
        let mut manifest = "__wasm_split_manifest.json".to_string();
        for line in hashes.lines() {
            let line = line.trim();
            if !line.is_empty() {
                if let Some((file, hash)) = line.split_once(':') {
                    if file == "manifest" {
                        manifest.clear();
                        manifest.push_str("__wasm_split_manifest.");
                        manifest.push_str(hash.trim());
                        manifest.push_str(".json");
                    }
                    if file == "split" {
                        split.clear();
                        split.push_str("__wasm_split.");
                        split.push_str(hash.trim());
                        split.push_str(".js");
                    }
                }
            }
        }
        (split, manifest)
    } else {
        (
            "__wasm_split.______________________.js".to_string(),
            "__wasm_split_manifest.json".to_string(),
        )
    };

    let site_dir = &options.site_root;
    let pkg_dir = &options.site_pkg_dir;
    let path = PathBuf::from(site_dir.to_string());
    let path = path.join(pkg_dir.to_string()).join(wasm_split_manifest);
    let Ok(file) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    let preloads = serde_json::from_str(&file).map_err(|source| {
        HydrationFileError::ParseSplitManifest { path, source }
    })?;

    Ok(Some(WasmSplitManifest(ArcStoredValue::new((
        format!("{root}/{pkg_dir}"),
        preloads,
        wasm_split_js,
    )))))
}

/// If this is provided via context, it means that you are using the islands router and
/// this is a subsequent navigation, made from the client.
///
/// This should be provided automatically by a server integration if it detects that the
/// header `Islands-Router` is present in the request.
///
/// This is used to determine how much of the hydration script to include in the page.
/// If it is present, then the contents of the `<HydrationScripts>` component will not be
/// included, as they only need to be sent to the client once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IslandsRouterNavigation;

#[cfg(test)]
mod tests {
    use super::{load_split_manifest, HydrationFileError};
    use halyard_config::HalyardOptions;
    use std::{path::PathBuf, sync::Arc};

    /// A fresh directory under the system temp dir, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("halyard-{name}-{}", std::process::id()));
            _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn path(&self) -> String {
            self.0.to_string_lossy().into_owned()
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn split_manifest_reports_an_unreadable_hash_file() {
        // the "hash file" is a directory, which exists but cannot be read
        let dir = TempDir::new("unreadable-hash-file");
        let options = HalyardOptions::builder()
            .output_name("app")
            .hash_files(true)
            .hash_file(Arc::from(dir.path()))
            .build();
        let result = load_split_manifest(&options, "");
        assert!(
            matches!(result, Err(HydrationFileError::ReadHashFile { .. })),
            "{result:?}"
        );
    }

    #[test]
    fn split_manifest_reports_invalid_json() {
        let dir = TempDir::new("invalid-split-manifest");
        std::fs::create_dir_all(dir.0.join("pkg")).unwrap();
        std::fs::write(
            dir.0.join("pkg").join("__wasm_split_manifest.json"),
            "not json",
        )
        .unwrap();
        let options = HalyardOptions::builder()
            .output_name("app")
            .site_root(dir.path())
            .build();
        let result = load_split_manifest(&options, "");
        assert!(
            matches!(
                result,
                Err(HydrationFileError::ParseSplitManifest { .. })
            ),
            "{result:?}"
        );
    }

    #[test]
    fn split_manifest_is_optional() {
        let dir = TempDir::new("no-split-manifest");
        let options = HalyardOptions::builder()
            .output_name("app")
            .site_root(dir.path())
            .build();
        let result = load_split_manifest(&options, "");
        assert!(matches!(result, Ok(None)), "{result:?}");
    }
}
