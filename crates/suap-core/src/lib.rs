//! Shared SUAP configuration and session primitives.

use std::{fs, path::{Path, PathBuf}};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

const QUALIFIER: &str = "br.edu.ifrn";
const ORGANIZATION: &str = "suap-companion";
const APPLICATION: &str = "chamados";
const CONFIG_FILE: &str = "config.toml";
const SESSION_FILE: &str = "session.cookies";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    config_dir: PathBuf,
    data_dir: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self, SuapError> {
        let project = ProjectDirs::from(QUALIFIER, ORGANIZATION, APPLICATION)
            .ok_or(SuapError::DirectoriesUnavailable)?;

        Ok(Self {
            config_dir: project.config_dir().to_path_buf(),
            data_dir: project.data_dir().to_path_buf(),
        })
    }

    pub fn from_dirs(config_dir: PathBuf, data_dir: PathBuf) -> Self {
        Self { config_dir, data_dir }
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join(CONFIG_FILE)
    }

    pub fn session_file(&self) -> PathBuf {
        self.data_dir.join(SESSION_FILE)
    }

    pub fn ensure_dirs(&self) -> Result<(), SuapError> {
        fs::create_dir_all(&self.config_dir)?;
        fs::create_dir_all(&self.data_dir)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SuapConfig {
    pub base_url: Url,
    pub username: Option<String>,
}

impl Default for SuapConfig {
    fn default() -> Self {
        Self {
            base_url: Url::parse("https://suap.ifrn.edu.br/").expect("default URL is valid"),
            username: None,
        }
    }
}

pub fn load_config(paths: &AppPaths) -> Result<Option<SuapConfig>, SuapError> {
    let path = paths.config_file();
    if !path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(path)?;
    let config = toml::from_str(&content)?;
    validate_config(&config)?;
    Ok(Some(config))
}

pub fn save_config(paths: &AppPaths, config: &SuapConfig) -> Result<(), SuapError> {
    validate_config(config)?;
    paths.ensure_dirs()?;
    let content = toml::to_string_pretty(config)?;
    let temporary = paths.config_file().with_extension("toml.tmp");
    fs::write(&temporary, content)?;
    fs::rename(temporary, paths.config_file())?;
    Ok(())
}

fn validate_config(config: &SuapConfig) -> Result<(), SuapError> {
    match config.base_url.scheme() {
        "http" | "https" => Ok(()),
        scheme => Err(SuapError::InvalidConfiguration(format!(
            "base_url must use HTTP or HTTPS, got {scheme:?}"
        ))),
    }
}

#[derive(Debug, Error)]
pub enum SuapError {
    #[error("application directories are unavailable")]
    DirectoriesUnavailable,
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
    #[error("TOML error: {0}")]
    Toml(#[from] toml::ser::Error),
    #[error("TOML parsing error: {0}")]
    TomlDe(#[from] toml::de::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trip_preserves_values() {
        let config = SuapConfig {
            base_url: Url::parse("https://suap.example/").unwrap(),
            username: Some("kelson".to_owned()),
        };
        let serialized = toml::to_string(&config).unwrap();
        let restored: SuapConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(config, restored);
    }

    #[test]
    fn rejects_non_http_urls() {
        let config = SuapConfig {
            base_url: Url::parse("file:///tmp/suap").unwrap(),
            username: None,
        };
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn derives_config_and_session_files() {
        let paths = AppPaths::from_dirs(PathBuf::from("config"), PathBuf::from("data"));
        assert_eq!(paths.config_file(), PathBuf::from("config/config.toml"));
        assert_eq!(paths.session_file(), PathBuf::from("data/session.cookies"));
    }
}
