use crate::meta::{error::MetaError, register};
use halyard::{
    attr::global::GlobalAttributes, component, prelude::HalyardOptions,
    tachys::html::element::link, IntoView,
};

/// Injects an [`HTMLLinkElement`](https://developer.mozilla.org/en-US/docs/Web/API/HTMLLinkElement) into the document
/// head that loads a stylesheet from the URL given by the `href` property.
///
/// Note that this does *not* work with the `cargo-halyard` `hash-files` feature: if you are using file
/// hashing, you should use [`HashedStylesheet`](crate::meta::HashedStylesheet).
///
/// ```
/// use halyard::prelude::*;
/// use halyard::meta::*;
///
/// #[component]
/// fn MyApp() -> impl IntoView {
///     provide_meta_context();
///
///     view! {
///       <main>
///         <Stylesheet href="/style.css"/>
///       </main>
///     }
/// }
/// ```
#[component]
pub fn Stylesheet(
    /// The URL at which the stylesheet is located.
    #[prop(into)]
    href: String,
    /// An ID for the stylesheet.
    #[prop(optional, into)]
    id: Option<String>,
) -> impl IntoView {
    // TODO additional attributes
    register(link().id(id).rel("stylesheet").href(href))
}

/// Injects an [`HTMLLinkElement`](https://developer.mozilla.org/en-US/docs/Web/API/HTMLLinkElement) into the document head that loads a `cargo-halyard`-hashed stylesheet.
///
/// This should only be used in the application’s server-side `shell` function, as
/// [`HalyardOptions`] is not available in the browser. Unlike other `halyard::meta` components, it
/// will render the `<link>` it creates exactly where it is called.
#[component]
pub fn HashedStylesheet(
    /// Halyard options
    options: HalyardOptions,
    /// An ID for the stylesheet.
    #[prop(optional, into)]
    id: Option<String>,
    /// A base url, not including a trailing slash
    #[prop(optional, into)]
    root: Option<String>,
) -> impl IntoView {
    let css_file_name = css_file_name(&options);
    let pkg_path = &options.site_pkg_dir;
    let root = root.unwrap_or_default();

    link()
        .id(id)
        .rel("stylesheet")
        .href(format!("{root}/{pkg_path}/{css_file_name}"))
}

/// The file name of the stylesheet: the output name, with the `css` hash from the hash file
/// when file hashing is on. A hash file that cannot be read is logged, and the unhashed
/// name is used.
fn css_file_name(options: &HalyardOptions) -> String {
    let mut css_file_name = options.output_name.to_string();
    if options.hash_files {
        let hash_path = std::env::current_exe()
            .map(|path| {
                path.parent().map(|p| p.to_path_buf()).unwrap_or_default()
            })
            .unwrap_or_default()
            .join(options.hash_file.as_ref());
        if hash_path.exists() {
            let hashes = match std::fs::read_to_string(&hash_path) {
                Ok(hashes) => hashes,
                Err(source) => {
                    MetaError::HashFile {
                        path: hash_path,
                        source,
                    }
                    .warn("The stylesheet is linked by its unhashed name.");
                    String::new()
                }
            };
            for line in hashes.lines() {
                let line = line.trim();
                if !line.is_empty() {
                    if let Some((file, hash)) = line.split_once(':') {
                        if file == "css" {
                            css_file_name
                                .push_str(&format!(".{}", hash.trim()));
                        }
                    }
                }
            }
        }
    }
    css_file_name.push_str(".css");
    css_file_name
}

/// `<HashedStylesheet>` is rendered by the application's shell on every request. (Rendering
/// the `<link>` itself to HTML needs the `ssr` feature, and these tests run in every build of
/// the crate, so they check the file name it links.)
#[cfg(test)]
mod tests {
    use super::*;
    use std::{path::Path, sync::Arc};

    /// Options with file hashing on and the hash file at `hash_file`: an absolute path,
    /// which replaces the server binary's directory that the name is joined to.
    fn hashing_options(hash_file: &Path) -> HalyardOptions {
        HalyardOptions::builder()
            .output_name("app")
            .hash_files(true)
            .hash_file(Arc::<str>::from(hash_file.to_string_lossy().as_ref()))
            .build()
    }

    /// A hash file that exists but cannot be read (here, a directory) was
    /// `.expect("failed to read hash file")`: a panic that failed the request. The page now
    /// renders, linking the unhashed file name.
    #[test]
    fn an_unreadable_hash_file_links_the_unhashed_stylesheet() {
        let unreadable = std::env::temp_dir();
        assert!(unreadable.is_dir());

        assert_eq!(css_file_name(&hashing_options(&unreadable)), "app.css");
    }

    /// The hash from the `css:` line of the hash file goes into the stylesheet's name.
    #[test]
    fn a_readable_hash_file_links_the_hashed_stylesheet() {
        let hash_file = std::env::temp_dir()
            .join(format!("halyard_meta_hash_{}.txt", std::process::id()));
        std::fs::write(&hash_file, "js: 111\ncss: abc123\nwasm: 222\n")
            .expect("the test writes its hash file");

        let name = css_file_name(&hashing_options(&hash_file));
        _ = std::fs::remove_file(&hash_file);

        assert_eq!(name, "app.abc123.css");
    }

    /// Without file hashing, the name is the output name, whatever the hash file says.
    #[test]
    fn without_hashing_the_stylesheet_is_the_output_name() {
        let options = HalyardOptions::builder().output_name("app").build();

        assert_eq!(css_file_name(&options), "app.css");
    }
}
