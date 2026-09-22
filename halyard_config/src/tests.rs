use crate::{
    env_from_str, env_w_default, env_wo_default, get_config_from_str,
    halyard_env, halyard_env_is_set, halyard_env_w_default, ws_from_str, Env,
    HalyardOptions, ReloadWSProtocol,
};
use std::{net::SocketAddr, path::Ancestors, str::FromStr};

#[test]
fn env_from_str_test() {
    assert!(matches!(env_from_str("dev").unwrap(), Env::DEV));
    assert!(matches!(env_from_str("development").unwrap(), Env::DEV));
    assert!(matches!(env_from_str("DEV").unwrap(), Env::DEV));
    assert!(matches!(env_from_str("DEVELOPMENT").unwrap(), Env::DEV));
    assert!(matches!(env_from_str("prod").unwrap(), Env::PROD));
    assert!(matches!(env_from_str("production").unwrap(), Env::PROD));
    assert!(matches!(env_from_str("PROD").unwrap(), Env::PROD));
    assert!(matches!(env_from_str("PRODUCTION").unwrap(), Env::PROD));
    assert!(env_from_str("TEST").is_err());
    assert!(env_from_str("?").is_err());
}

#[test]
fn ws_from_str_test() {
    assert!(matches!(ws_from_str("ws").unwrap(), ReloadWSProtocol::WS));
    assert!(matches!(ws_from_str("WS").unwrap(), ReloadWSProtocol::WS));
    assert!(matches!(ws_from_str("wss").unwrap(), ReloadWSProtocol::WSS));
    assert!(matches!(ws_from_str("WSS").unwrap(), ReloadWSProtocol::WSS));
    assert!(ws_from_str("TEST").is_err());
    assert!(ws_from_str("?").is_err());
}

#[test]
fn env_w_default_test() {
    temp_env::with_var("HALYARD_CONFIG_ENV_TEST", Some("custom"), || {
        assert_eq!(
            env_w_default("HALYARD_CONFIG_ENV_TEST", "default").unwrap(),
            String::from("custom")
        );
    });

    temp_env::with_var_unset("HALYARD_CONFIG_ENV_TEST", || {
        assert_eq!(
            env_w_default("HALYARD_CONFIG_ENV_TEST", "default").unwrap(),
            String::from("default")
        );
    });
}

#[test]
fn env_wo_default_test() {
    temp_env::with_var("HALYARD_CONFIG_ENV_TEST", Some("custom"), || {
        assert_eq!(
            env_wo_default("HALYARD_CONFIG_ENV_TEST").unwrap(),
            Some(String::from("custom"))
        );
    });

    temp_env::with_var_unset("HALYARD_CONFIG_ENV_TEST", || {
        assert_eq!(env_wo_default("HALYARD_CONFIG_ENV_TEST").unwrap(), None);
    });
}

#[test]
fn try_from_env_test() {
    // Test config values from environment variables
    let config = temp_env::with_vars(
        [
            ("HALYARD_OUTPUT_NAME", Some("app_test")),
            ("HALYARD_SITE_ROOT", Some("my_target/site")),
            ("HALYARD_SITE_PKG_DIR", Some("my_pkg")),
            ("HALYARD_SITE_ADDR", Some("0.0.0.0:80")),
            ("HALYARD_RELOAD_PORT", Some("8080")),
            ("HALYARD_RELOAD_EXTERNAL_PORT", Some("8080")),
            ("HALYARD_ENV", Some("PROD")),
            ("HALYARD_RELOAD_WS_PROTOCOL", Some("WSS")),
        ],
        || HalyardOptions::try_from_env().unwrap(),
    );

    assert_eq!(config.output_name.as_ref(), "app_test");
    assert_eq!(config.site_root.as_ref(), "my_target/site");
    assert_eq!(config.site_pkg_dir.as_ref(), "my_pkg");
    assert_eq!(
        config.site_addr,
        SocketAddr::from_str("0.0.0.0:80").unwrap()
    );
    assert_eq!(config.reload_port, 8080);
    assert_eq!(config.reload_external_port, Some(8080));
    assert_eq!(config.env, Env::PROD);
    assert_eq!(config.reload_ws_protocol, ReloadWSProtocol::WSS)
}

#[test]
fn wasm_file_name_defaults_to_output_name() {
    let config = temp_env::with_vars(
        [
            ("HALYARD_OUTPUT_NAME", Some("app")),
            ("HALYARD_WASM_FILE_NAME", None),
            ("LEPTOS_WASM_FILE_NAME", None),
        ],
        || HalyardOptions::try_from_env().unwrap(),
    );
    assert_eq!(config.wasm_file_name, None);
    assert_eq!(config.wasm_file_stem(), "app");

    let config = temp_env::with_vars(
        [
            ("HALYARD_OUTPUT_NAME", Some("app")),
            ("HALYARD_WASM_FILE_NAME", Some("app_bg")),
        ],
        || HalyardOptions::try_from_env().unwrap(),
    );
    assert_eq!(config.wasm_file_stem(), "app_bg");
}

#[test]
fn try_from_env_accepts_legacy_leptos_vars() {
    // legacy `LEPTOS_*` names are honoured when `HALYARD_*` is unset...
    let config = temp_env::with_vars(
        [
            ("HALYARD_OUTPUT_NAME", None),
            ("LEPTOS_OUTPUT_NAME", Some("legacy_app")),
            ("HALYARD_SITE_ROOT", None),
            ("LEPTOS_SITE_ROOT", Some("legacy/site")),
            ("HALYARD_SITE_PKG_DIR", Some("new_pkg")),
            ("LEPTOS_SITE_PKG_DIR", Some("old_pkg")),
        ],
        || HalyardOptions::try_from_env().unwrap(),
    );
    assert_eq!(config.output_name.as_ref(), "legacy_app");
    assert_eq!(config.site_root.as_ref(), "legacy/site");
    // ...and `HALYARD_*` wins when both are set
    assert_eq!(config.site_pkg_dir.as_ref(), "new_pkg");
}

#[test]
fn halyard_env_helpers() {
    temp_env::with_vars(
        [
            ("HALYARD_CONFIG_ENV_TEST", None),
            ("LEPTOS_CONFIG_ENV_TEST", Some("legacy")),
        ],
        || {
            assert_eq!(
                halyard_env("CONFIG_ENV_TEST").unwrap(),
                Some("legacy".to_string())
            );
            assert!(halyard_env_is_set("CONFIG_ENV_TEST"));
        },
    );
    temp_env::with_vars(
        [
            ("HALYARD_CONFIG_ENV_TEST", None::<&str>),
            ("LEPTOS_CONFIG_ENV_TEST", None),
        ],
        || {
            assert_eq!(halyard_env("CONFIG_ENV_TEST").unwrap(), None);
            assert_eq!(
                halyard_env_w_default("CONFIG_ENV_TEST", "dflt").unwrap(),
                "dflt"
            );
            assert!(!halyard_env_is_set("CONFIG_ENV_TEST"));
        },
    );
}

#[test]
fn get_config_from_str_accepts_legacy_metadata_section() {
    let toml = r#"
[package]
name = "legacy"

[package.metadata.leptos]
output-name = "legacy_out"
site-pkg-dir = "legacy_pkg"
"#;
    let config = temp_env::with_vars(
        [
            ("HALYARD_SITE_PKG_DIR", None::<&str>),
            ("LEPTOS_SITE_PKG_DIR", None),
        ],
        || get_config_from_str(toml).unwrap(),
    );
    assert_eq!(config.output_name.as_ref(), "legacy_out");
    assert_eq!(config.site_pkg_dir.as_ref(), "legacy_pkg");
}

#[test]
fn get_config_from_str_env_precedence() {
    let toml = r#"
[package.metadata.halyard]
output-name = "from_file"
site-pkg-dir = "file_pkg"
site-root = "file_root"
"#;
    let config = temp_env::with_vars(
        [
            ("HALYARD_SITE_PKG_DIR", Some("new_pkg")),
            ("LEPTOS_SITE_PKG_DIR", Some("old_pkg")),
            ("HALYARD_SITE_ROOT", None),
            ("LEPTOS_SITE_ROOT", Some("old_root")),
            ("HALYARD_OUTPUT_NAME", None),
            ("LEPTOS_OUTPUT_NAME", None),
        ],
        || get_config_from_str(toml).unwrap(),
    );
    assert_eq!(config.output_name.as_ref(), "from_file");
    // HALYARD_* beats LEPTOS_*, which beats the file
    assert_eq!(config.site_pkg_dir.as_ref(), "new_pkg");
    assert_eq!(config.site_root.as_ref(), "old_root");
}

#[test]
fn halyard_options_css_file_path() {
    fn next_file_name<'a>(a: &'a mut Ancestors) -> Option<&'a str> {
        a.next()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
    }
    let options = HalyardOptions::builder().output_name("test").build();
    let path = options.css_file_path();
    let mut ancestors = path.ancestors();
    assert_eq!(next_file_name(&mut ancestors), Some("test.css"));
    assert_eq!(next_file_name(&mut ancestors), Some("pkg"));
    assert_eq!(next_file_name(&mut ancestors), None);

    let options = HalyardOptions::builder()
        .output_name("test")
        .site_pkg_dir("")
        .build();
    let path = options.css_file_path();
    let mut ancestors = path.ancestors();
    assert_eq!(next_file_name(&mut ancestors), Some("test.css"));
    assert_eq!(next_file_name(&mut ancestors), None);

    let options = HalyardOptions::builder()
        .output_name("test")
        .site_pkg_dir("my_pkg")
        .site_root("my_site")
        .build();
    let path = options.css_file_path();
    let mut ancestors = path.ancestors();
    assert_eq!(next_file_name(&mut ancestors), Some("test.css"));
    assert_eq!(next_file_name(&mut ancestors), Some("my_pkg"));
    assert_eq!(next_file_name(&mut ancestors), Some("my_site"));
    assert_eq!(next_file_name(&mut ancestors), None);
}

#[test]
fn halyard_options_css_path() {
    let options = HalyardOptions::builder().output_name("test").build();
    assert_eq!(options.css_path(), "/pkg/test.css");

    let options = HalyardOptions::builder()
        .output_name("test.css")
        .site_pkg_dir("my/pkg")
        .build();
    assert_eq!(options.css_path(), "/my/pkg/test.css.css");

    let options = HalyardOptions::builder()
        .output_name("test")
        .site_pkg_dir("/pkg/")
        .build();
    assert_eq!(options.css_path(), "/pkg/test.css");

    let options = HalyardOptions::builder()
        .output_name("test")
        .site_pkg_dir("")
        .build();
    assert_eq!(options.css_path(), "/test.css");
}
