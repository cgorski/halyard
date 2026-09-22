use std::{net::AddrParseError, num::ParseIntError, str::ParseBoolError};
use thiserror::Error;

#[derive(Debug, Error, Clone)]
pub enum HalyardConfigError {
    #[error("Cargo.toml not found in package root")]
    ConfigNotFound,
    #[error("package.metadata.halyard section missing from Cargo.toml")]
    ConfigSectionNotFound,
    #[error("Failed to get Halyard Environment. Did you set HALYARD_ENV?")]
    EnvError,
    #[error("Config Error: {0}")]
    ConfigError(String),
    #[error("Config Error: {0}")]
    EnvVarError(String),
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
