//! Shared SUAP authentication and session primitives.

use std::path::PathBuf;

use thiserror::Error;
use url::Url;

#[derive(Debug, Clone)]
pub struct SuapConfig {
    pub base_url: Url,
    pub username: Option<String>,
    pub session_file: PathBuf,
}

#[derive(Debug, Error)]
pub enum SuapError {
    #[error("SUAP configuration is invalid: {0}")]
    InvalidConfiguration(String),
    #[error("SUAP session is not authenticated")]
    NotAuthenticated,
    #[error("SUAP authentication failed")]
    AuthenticationFailed,
    #[error("SUAP transport error: {0}")]
    Transport(String),
    #[error("SUAP parsing error: {0}")]
    Parse(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

pub struct SuapClient {
    config: SuapConfig,
}

impl SuapClient {
    pub fn new(config: SuapConfig) -> Result<Self, SuapError> {
        if config.base_url.scheme() != "http" && config.base_url.scheme() != "https" {
            return Err(SuapError::InvalidConfiguration(
                "base_url must use HTTP or HTTPS".to_owned(),
            ));
        }

        Ok(Self { config })
    }

    pub fn config(&self) -> &SuapConfig {
        &self.config
    }
}
