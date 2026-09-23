//! The server-side files that `HydrationScripts` reads (the file-hash file and the
//! wasm-split manifest) are optional inputs from the build tool. A missing or broken one is
//! logged and the page is still rendered; these used to panic and fail every request.
//!
//! `HydrationScripts` loads the split manifest once per process, so only one test in this
//! file renders it. Rendering it needs the `ssr` feature:
//! `cargo test -p halyard --features ssr --test hydration_files`.

use halyard::{hydration::hydration_file_names, prelude::*};
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
fn hydration_file_names_fall_back_to_unhashed_names_when_the_hash_file_is_unreadable(
) {
    // the "hash file" is a directory: it exists, but cannot be read
    let dir = TempDir::new("hash-file-is-a-directory");
    let options = HalyardOptions::builder()
        .output_name("app")
        .hash_files(true)
        .hash_file(Arc::from(dir.path()))
        .build();
    assert_eq!(
        hydration_file_names(&options),
        ("app".to_string(), "app".to_string())
    );
}

#[cfg(feature = "ssr")]
#[test]
fn hydration_scripts_render_when_the_split_manifest_is_invalid() {
    use halyard::hydration::HydrationScripts;
    let dir = TempDir::new("invalid-split-manifest");
    std::fs::create_dir_all(dir.0.join("pkg")).unwrap();
    std::fs::write(
        dir.0.join("pkg").join("__wasm_split_manifest.json"),
        "{ not json",
    )
    .unwrap();
    let options = HalyardOptions::builder()
        .output_name("app")
        .site_root(dir.path())
        .build();

    let html = Owner::new()
        .with(|| view! { <HydrationScripts options=options/> }.to_html());
    assert!(html.contains("\"app\", \"app\");"), "{html}");
}
