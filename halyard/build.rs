use rustc_version::{version_meta, Channel};

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();

    // Set cfg flags depending on release channel
    match version_meta() {
        Ok(meta) => {
            if matches!(meta.channel, Channel::Nightly) {
                println!("cargo:rustc-cfg=rustc_nightly");
            }
        }
        Err(err) => println!(
            "cargo:warning=could not determine the rustc release channel \
             ({err}); building without the nightly-only optimisations"
        ),
    }
    // Set cfg flag for getrandom wasm_js
    if target == "wasm32-unknown-unknown" {
        // Set a custom cfg flag for wasm builds
        println!("cargo:rustc-cfg=getrandom_backend=\"wasm_js\"");
    }
}
