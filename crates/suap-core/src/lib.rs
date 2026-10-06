//! Shared SUAP configuration and persistent session primitives.

use std::{fs, io::{BufReader, Write}, path::{Path, PathBuf}, sync::Arc};

use directories::ProjectDirs;
use reqwest::Client;
use reqwest_cookie_store::CookieStore;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Mutex;
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

#[derive(Debug)]
pub struct SessionStore {
    path: PathBuf,
    cookies: Arc<Mutex<CookieStore>>,
}

impl SessionStore {
    pub fn open(path: PathBuf) -> Result<Self, SuapError> {
        let cookies = if path.exists() {
            let file = fs::File::open(&path)?;
            CookieStore::load_json(BufReader::new(file))?
        } else {
            CookieStore::default()
        };

        Ok(Self {
            path,
            cookies: Arc::new(Mutex::new(cookies)),
        })
    }

    pub fn cookie_provider(&self) -> Arc<Mutex<CookieStore>> {
        Arc::clone(&self.cookies)
    }

    pub async fn save(&self) -> Result<(), SuapError> {
        let parent = self.path.parent().ok_or_else(|| {
            SuapError::InvalidConfiguration("session path has no parent directory".to_owned())
        })?;
        fs::create_dir_all(parent)?;
        let temporary = self.path.with_extension("cookies.tmp");
        let file = fs::File::create(&temporary)?;
        let mut writer = std::io::BufWriter::new(file);
        let cookies = self.cookies.lock().await;
        cookies.save_json(&mut writer)?;
        drop(cookies);
        writer.flush()?;
        fs::rename(temporary, &self.path)?;
        restrict_permissions(&self.path)?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

pub struct SuapClient {
    client: Client,
    session: SessionStore,
}

impl SuapClient {
    pub fn open(paths: &AppPaths) -> Result<Self, SuapError> {
        let session = SessionStore::open(paths.session_file())?;
        let client = Client::builder()
            .cookie_provider(session.cookie_provider())
            .build()?;
        Ok(Self { client, session })
    }

    pub fn http_client(&self) -> &Client {
        &self.client
    }

    pub fn session(&self) -> &SessionStore {
        &self.session
    }
}

fn restrict_permissions(path: &Path) -> Result<(), SuapError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
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
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("cookie store error: {0}")]
    CookieStore(String),
    #[error("TOML error: {0}")]
    Toml(#[from] toml::ser::Error),
    #[error("TOML parsing error: {0}")]
    TomlDe(#[from] toml::de::Error),
}

impl From<Box<dyn std::error::Error + Send + Sync>> for SuapError {
    fn from(error: Box<dyn std::error::Error + Send + Sync>) -> Self {
        Self::CookieStore(error.to_string())
    }
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
