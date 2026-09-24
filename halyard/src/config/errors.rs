use std::{net::AddrParseError, num::ParseIntError, str::ParseBoolError};
use thiserror::Error;

/// Why the configuration could not be read.
#[derive(Debug, Error, Clone)]
pub enum HalyardConfigError {
    /// There is no `Cargo.toml` in the package root.
    #[error("Cargo.toml not found in package root")]
    ConfigNotFound,
    /// `Cargo.toml` has no `[package.metadata.halyard]` section.
    #[error("package.metadata.halyard section missing from Cargo.toml")]
    ConfigSectionNotFound,
    /// The environment could not be read.
    #[error("Failed to get Halyard Environment. Did you set HALYARD_ENV?")]
    EnvError,
    /// The configuration file or a value in it could not be read.
    #[error("Config Error: {0}")]
    ConfigError(String),
    /// An environment variable could not be read.
    #[error("Config Error: {0}")]
    EnvVarError(String),
    /// The environment (`env`, `HALYARD_ENV`/`LEPTOS_ENV`) is not one halyard knows.
    #[error(
        "`{value}` is not a supported environment; use `dev`, `development`, `prod` or \
         `production` (in any case)"
    )]
    InvalidEnv {
        /// The value given.
        value: String,
    },
    /// The live-reload websocket protocol (`reload-ws-protocol`,
    /// `HALYARD_RELOAD_WS_PROTOCOL`/`LEPTOS_RELOAD_WS_PROTOCOL`) is not `ws` or `wss`.
    #[error(
        "`{value}` is not a supported websocket protocol; use `ws` or `wss` (in any case)"
    )]
    InvalidReloadWsProtocol {
        /// The value given.
        value: String,
    },
}
impl From<config::ConfigError> for HalyardConfigError {
    fn from(e: config::ConfigError) -> Self {
        Self::ConfigError(e.to_string())
    }
}

impl From<ParseIntError> for HalyardConfigError {
    fn from(e: ParseIntError) -> Self {
        Self::ConfigError(e.to_string())
    }
}

impl From<AddrParseError> for HalyardConfigError {
    fn from(e: AddrParseError) -> Self {
        Self::ConfigError(e.to_string())
    }
}

impl From<ParseBoolError> for HalyardConfigError {
    fn from(e: ParseBoolError) -> Self {
        Self::ConfigError(e.to_string())
    }
}
