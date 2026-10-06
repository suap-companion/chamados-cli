//! Shared SUAP configuration, authentication and persistent session primitives.

use std::{fs, io::{BufReader, Write}, path::{Path, PathBuf}, sync::Arc};

use directories::ProjectDirs;
use reqwest::{Client, StatusCode, Url};
use reqwest_cookie_store::CookieStore;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Mutex;

const QUALIFIER: &str = "br.edu.ifrn";
const ORGANIZATION: &str = "suap-companion";
const APPLICATION: &str = "chamados";
const CONFIG_FILE: &str = "config.toml";
const SESSION_FILE: &str = "session.cookies";
const LOGIN_PATH: &str = "/accounts/login/";

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

    pub fn config_dir(&self) -> &Path { &self.config_dir }
    pub fn data_dir(&self) -> &Path { &self.data_dir }
    pub fn config_file(&self) -> PathBuf { self.config_dir.join(CONFIG_FILE) }
    pub fn session_file(&self) -> PathBuf { self.data_dir.join(SESSION_FILE) }

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
    if !path.exists() { return Ok(None); }
    let content = fs::read_to_string(path)?;
    let config = toml::from_str(&content)?;
    validate_config(&config)?;
    Ok(Some(config))
}

pub fn save_config(paths: &AppPaths, config: &SuapConfig) -> Result<(), SuapError> {
    validate_config(config)?;
    paths.ensure_dirs()?;
    let temporary = paths.config_file().with_extension("toml.tmp");
    fs::write(&temporary, toml::to_string_pretty(config)?)?;
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
            CookieStore::load_json(BufReader::new(fs::File::open(&path)?))?
        } else {
            CookieStore::default()
        };
        Ok(Self { path, cookies: Arc::new(Mutex::new(cookies)) })
    }

    pub fn cookie_provider(&self) -> Arc<Mutex<CookieStore>> { Arc::clone(&self.cookies) }

    pub async fn save(&self) -> Result<(), SuapError> {
        let parent = self.path.parent().ok_or_else(|| SuapError::InvalidConfiguration("session path has no parent directory".to_owned()))?;
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

    pub fn path(&self) -> &Path { &self.path }
}

pub struct SuapClient {
    client: Client,
    session: SessionStore,
    base_url: Url,
}

impl SuapClient {
    pub fn open(paths: &AppPaths, config: &SuapConfig) -> Result<Self, SuapError> {
        let session = SessionStore::open(paths.session_file())?;
        let client = Client::builder()
            .cookie_provider(session.cookie_provider())
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()?;
        Ok(Self { client, session, base_url: config.base_url.clone() })
    }

    pub async fn login(&self, username: &str, password: &str) -> Result<(), SuapError> {
        let login_url = self.base_url.join(LOGIN_PATH)?;
        let page = self.client.get(login_url.clone()).send().await?;
        if !page.status().is_success() {
            return Err(SuapError::AuthenticationFailed);
        }
        let html = page.text().await?;
        let csrf = extract_csrf_token(&html)?;
        let response = self.client.post(login_url).form(&[
            ("username", username),
            ("password", password),
            ("csrfmiddlewaretoken", csrf.as_str()),
            ("next", "/"),
        ]).header("Referer", self.base_url.as_str()).send().await?;
        if response.status() == StatusCode::UNAUTHORIZED || response.url().path() == LOGIN_PATH {
            return Err(SuapError::AuthenticationFailed);
        }
        if !response.status().is_success() && !response.status().is_redirection() {
            return Err(SuapError::AuthenticationFailed);
        }
        self.session.save().await
    }

    pub async fn is_authenticated(&self) -> Result<bool, SuapError> {
        let response = self.client.get(self.base_url.clone()).send().await?;
        Ok(response.status().is_success() && response.url().path() != LOGIN_PATH)
    }

    pub fn http_client(&self) -> &Client { &self.client }
    pub fn session(&self) -> &SessionStore { &self.session }
}

fn extract_csrf_token(html: &str) -> Result<String, SuapError> {
    let document = Html::parse_document(html);
    let selector = Selector::parse("input[name=csrfmiddlewaretoken]")
        .map_err(|error| SuapError::Parse(error.to_string()))?;
    document.select(&selector)
        .next()
        .and_then(|element| element.value().attr("value"))
        .map(str::to_owned)
        .ok_or_else(|| SuapError::Parse("CSRF token not found in login form".to_owned()))
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
    #[error("URL error: {0}")]
    Url(#[from] url::ParseError),
    #[error("cookie store error: {0}")]
    CookieStore(String),
    #[error("TOML error: {0}")]
    Toml(#[from] toml::ser::Error),
    #[error("TOML parsing error: {0}")]
    TomlDe(#[from] toml::de::Error),
}

impl From<Box<dyn std::error::Error + Send + Sync>> for SuapError {
    fn from(error: Box<dyn std::error::Error + Send + Sync>) -> Self { Self::CookieStore(error.to_string()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_csrf_token_from_form() {
        let html = r#"<form><input type="hidden" name="csrfmiddlewaretoken" value="abc123"></form>"#;
        assert_eq!(extract_csrf_token(html).unwrap(), "abc123");
    }

    #[test]
    fn reports_missing_csrf_token() {
        assert!(extract_csrf_token("<form></form>").is_err());
    }
}
