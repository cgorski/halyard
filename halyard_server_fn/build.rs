use rustc_version::{version_meta, Channel};

fn main() {
    // Set cfg flags depending on release channel
    match version_meta() {
        Ok(meta) => {
            if matches!(meta.channel, Channel::Nightly) {
                println!("cargo:rustc-cfg=rustc_nightly");
            }
        }
        Err(err) => println!(
            "cargo:warning=could not determine the rustc release channel \
             ({err}); building as on stable"
        ),
    }
}
