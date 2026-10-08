//! Shared SUAP configuration, authentication and persistent session primitives.

use std::{collections::BTreeMap, fs, io::{BufReader, Write}, path::{Path, PathBuf}, sync::Arc};

use directories::{BaseDirs, ProjectDirs};
use reqwest::{Client, StatusCode, Url};
use reqwest_cookie_store::{CookieStore, CookieStoreMutex};
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const QUALIFIER: &str = "br.edu.ifrn";
const ORGANIZATION: &str = "suap-companion";
const APPLICATION: &str = "chamados";
const CONFIG_FILE: &str = "config.toml";
const SESSION_FILE: &str = "session.cookies";
const CONFIG_DIR_IN_HOME: [&str; 2] = [".config", "suap"];
const LOGIN_PATH: &str = "/accounts/login/";

/// Profile used when none is requested.
pub const DEFAULT_PROFILE: &str = "default";
const MAX_PROFILE_LEN: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    config_dir: PathBuf,
    data_dir: PathBuf,
    profile: String,
}

impl AppPaths {
    /// Configuration lives in `~/.config/suap` on every platform; session data uses the OS data directory.
    pub fn discover() -> Result<Self, SuapError> {
        let home = BaseDirs::new().ok_or(SuapError::DirectoriesUnavailable)?;
        let project = ProjectDirs::from(QUALIFIER, ORGANIZATION, APPLICATION)
            .ok_or(SuapError::DirectoriesUnavailable)?;
        Ok(Self::from_dirs(config_dir_in(home.home_dir()), project.data_dir().to_path_buf()))
    }

    pub fn from_dirs(config_dir: PathBuf, data_dir: PathBuf) -> Self {
        Self { config_dir, data_dir, profile: DEFAULT_PROFILE.to_owned() }
    }

    /// Selects the profile whose configuration and session these paths refer to.
    pub fn with_profile(mut self, profile: &str) -> Result<Self, SuapError> {
        validate_profile(profile)?;
        self.profile = profile.to_owned();
        Ok(self)
    }

    pub fn profile(&self) -> &str { &self.profile }

    pub fn config_dir(&self) -> &Path { &self.config_dir }
    pub fn data_dir(&self) -> &Path { &self.data_dir }
    pub fn config_file(&self) -> PathBuf { self.config_dir.join(CONFIG_FILE) }

    /// Session cookies of the selected profile (`session.cookies` for `default`, else `session-<profile>.cookies`).
    pub fn session_file(&self) -> PathBuf {
        if self.profile == DEFAULT_PROFILE {
            return self.data_dir.join(SESSION_FILE);
        }
        self.data_dir.join(format!("session-{}.cookies", self.profile))
    }

    pub fn ensure_dirs(&self) -> Result<(), SuapError> {
        fs::create_dir_all(&self.config_dir)?;
        fs::create_dir_all(&self.data_dir)?;
        Ok(())
    }
}

fn validate_profile(profile: &str) -> Result<(), SuapError> {
    let allowed = |character: char| character.is_ascii_alphanumeric() || character == '-' || character == '_';
    if !profile.is_empty() && profile.len() <= MAX_PROFILE_LEN && profile.chars().all(allowed) {
        return Ok(());
    }
    Err(SuapError::InvalidConfiguration(format!(
        "invalid profile name {profile:?}: use letters, digits, '-' or '_' (max {MAX_PROFILE_LEN})"
    )))
}

/// Directory holding `config.toml` for a given home directory (`<home>/.config/suap`).
fn config_dir_in(home: &Path) -> PathBuf {
    CONFIG_DIR_IN_HOME.iter().fold(home.to_path_buf(), |path, part| path.join(part))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SuapConfig {
    pub base_url: Url,
    pub username: Option<String>,
    /// Defaults for `chamados open`, kept per profile.
    #[serde(default, skip_serializing_if = "OpenDefaults::is_empty")]
    pub open: OpenDefaults,
}

/// Per-profile defaults used when opening tickets; every explicit option overrides them.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenDefaults {
    /// SUAP service id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<u64>,
    /// Interested person (a SUAP "vínculo" id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interested: Option<String>,
    /// Campus (`uo`) id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub campus: Option<String>,
    /// Service center id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub center: Option<String>,
}

impl OpenDefaults {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

impl Default for SuapConfig {
    fn default() -> Self {
        Self {
            base_url: Url::parse("https://suap.ifrn.edu.br/").expect("default URL is valid"),
            username: None,
            open: OpenDefaults::default(),
        }
    }
}

/// On-disk layout of `config.toml`: one `[profiles.<name>]` table per profile.
#[derive(Debug, Default, Serialize, Deserialize)]
struct ConfigFile {
    // Pre-profiles layout: top-level keys belong to the `default` profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base_url: Option<Url>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    username: Option<String>,
    #[serde(default)]
    profiles: BTreeMap<String, SuapConfig>,
}

impl ConfigFile {
    fn read(paths: &AppPaths) -> Result<Self, SuapError> {
        let path = paths.config_file();
        if !path.exists() { return Ok(Self::default()); }
        Ok(toml::from_str(&fs::read_to_string(path)?)?)
    }

    /// Moves the pre-profiles top-level keys into the `default` profile (unless it already exists).
    fn migrate_legacy(&mut self) {
        let (base_url, username) = (self.base_url.take(), self.username.take());
        if (base_url.is_some() || username.is_some()) && !self.profiles.contains_key(DEFAULT_PROFILE) {
            let base_url = base_url.unwrap_or_else(|| SuapConfig::default().base_url);
            self.profiles.insert(DEFAULT_PROFILE.to_owned(), SuapConfig { base_url, username, ..SuapConfig::default() });
        }
    }
}

/// Loads the configuration of the profile selected in `paths`; `None` if it was never configured.
pub fn load_config(paths: &AppPaths) -> Result<Option<SuapConfig>, SuapError> {
    let mut file = ConfigFile::read(paths)?;
    file.migrate_legacy();
    let config = file.profiles.remove(paths.profile());
    if let Some(config) = &config { validate_config(config)?; }
    Ok(config)
}

/// Saves the configuration of the profile selected in `paths`, keeping the other profiles.
pub fn save_config(paths: &AppPaths, config: &SuapConfig) -> Result<(), SuapError> {
    validate_config(config)?;
    let mut file = ConfigFile::read(paths)?;
    file.migrate_legacy();
    file.profiles.insert(paths.profile().to_owned(), config.clone());
    paths.ensure_dirs()?;
    let temporary = paths.config_file().with_extension("toml.tmp");
    fs::write(&temporary, toml::to_string_pretty(&file)?)?;
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
    cookies: Arc<CookieStoreMutex>,
}

impl SessionStore {
    pub fn open(path: PathBuf) -> Result<Self, SuapError> {
        let cookies = if path.exists() {
            // Sessions are disposable: a file in an older or unreadable format is dropped (log in again).
            cookie_store::serde::json::load(BufReader::new(fs::File::open(&path)?)).unwrap_or_default()
        } else {
            CookieStore::default()
        };
        Ok(Self { path, cookies: Arc::new(CookieStoreMutex::new(cookies)) })
    }

    pub fn cookie_provider(&self) -> Arc<CookieStoreMutex> { Arc::clone(&self.cookies) }

    pub async fn save(&self) -> Result<(), SuapError> {
        let parent = self.path.parent().ok_or_else(|| SuapError::InvalidConfiguration("session path has no parent directory".to_owned()))?;
        fs::create_dir_all(parent)?;
        let temporary = self.path.with_extension("cookies.tmp");
        let file = fs::File::create(&temporary)?;
        let mut writer = std::io::BufWriter::new(file);
        let cookies = self.cookies.lock().map_err(|_| SuapError::CookieStore("cookie store lock poisoned".to_owned()))?;
        cookie_store::serde::json::save(&cookies, &mut writer)?;
        drop(cookies);
        writer.flush()?;
        fs::rename(temporary, &self.path)?;
        restrict_permissions(&self.path)?;
        Ok(())
    }

    pub fn path(&self) -> &Path { &self.path }
}

/// Result of submitting a form: the path reached after redirects and the page body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormResponse {
    pub path: String,
    pub body: String,
}

/// A file to upload with [`SuapClient::submit_multipart`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormFile {
    pub field: String,
    pub file_name: String,
    pub bytes: Vec<u8>,
}

async fn form_response(response: reqwest::Response) -> Result<FormResponse, SuapError> {
    if response.url().path() == LOGIN_PATH { return Err(SuapError::NotAuthenticated); }
    if !response.status().is_success() {
        return Err(SuapError::Transport(format!("unexpected status {}", response.status())));
    }
    let final_path = response.url().path().to_owned();
    Ok(FormResponse { path: final_path, body: response.text().await? })
}

pub struct SuapClient {
    client: Client,
    session: SessionStore,
    base_url: Url,
}

impl SuapClient {
    pub fn open(paths: &AppPaths, config: &SuapConfig) -> Result<Self, SuapError> {
        let session = SessionStore::open(paths.session_file())?;
        let builder = Client::builder()
            .cookie_provider(session.cookie_provider())
            .redirect(reqwest::redirect::Policy::limited(10));
        let client = builder.build()?;
        Ok(Self { client, session, base_url: config.base_url.clone() })
    }

    pub async fn login(&self, username: &str, password: &str) -> Result<(), SuapError> {
        let login_url = self.base_url.join(LOGIN_PATH)?;
        let page = self.client.get(login_url.clone()).send().await?;
        if !page.status().is_success() { return Err(SuapError::AuthenticationFailed); }
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

    /// Fetches `path` (relative to the base URL) with the stored session and returns the body.
    ///
    /// Fails with [`SuapError::NotAuthenticated`] when SUAP redirects to the login page.
    pub async fn fetch_page(&self, path: &str) -> Result<String, SuapError> {
        let response = self.client.get(self.base_url.join(path)?).send().await?;
        if response.url().path() == LOGIN_PATH { return Err(SuapError::NotAuthenticated); }
        if !response.status().is_success() {
            return Err(SuapError::Transport(format!("unexpected status {}", response.status())));
        }
        Ok(response.text().await?)
    }

    /// Posts `fields` as a form to `path` and returns where SUAP ended up after redirects.
    ///
    /// Fails with [`SuapError::NotAuthenticated`] when SUAP redirects to the login page.
    pub async fn submit_form(&self, path: &str, fields: &[(String, String)]) -> Result<FormResponse, SuapError> {
        let url = self.base_url.join(path)?;
        let response = self.client.post(url.clone()).form(fields).header("Referer", url.as_str()).send().await?;
        form_response(response).await
    }

    /// Like [`SuapClient::submit_form`], but as `multipart/form-data` with file uploads.
    pub async fn submit_multipart(
        &self,
        path: &str,
        fields: &[(String, String)],
        files: &[FormFile],
    ) -> Result<FormResponse, SuapError> {
        let url = self.base_url.join(path)?;
        let mut form = reqwest::multipart::Form::new();
        for (name, value) in fields {
            form = form.text(name.clone(), value.clone());
        }
        for file in files {
            let part = reqwest::multipart::Part::bytes(file.bytes.clone()).file_name(file.file_name.clone());
            form = form.part(file.field.clone(), part);
        }
        let response = self.client.post(url.clone()).multipart(form).header("Referer", url.as_str()).send().await?;
        form_response(response).await
    }

    pub fn base_url(&self) -> &Url { &self.base_url }
    pub fn http_client(&self) -> &Client { &self.client }
    pub fn session(&self) -> &SessionStore { &self.session }
}

fn extract_csrf_token(html: &str) -> Result<String, SuapError> {
    let document = Html::parse_document(html);
    let selector = Selector::parse("input[name=csrfmiddlewaretoken]").expect("static selector is valid");
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
    use tempfile::{tempdir, TempDir};
    use wiremock::{matchers::{body_string_contains, method, path}, Mock, MockServer, ResponseTemplate};

    const LOGIN_FORM: &str = r#"<form><input type="hidden" name="csrfmiddlewaretoken" value="tok"></form>"#;

    fn paths() -> (TempDir, AppPaths) {
        let directory = tempdir().unwrap();
        let paths = AppPaths::from_dirs(directory.path().join("config"), directory.path().join("data"));
        (directory, paths)
    }

    fn config_for(server: &MockServer) -> SuapConfig {
        SuapConfig { base_url: Url::parse(&format!("{}/", server.uri())).unwrap(), ..SuapConfig::default() }
    }

    async fn mount_login_page(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path(LOGIN_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_string(LOGIN_FORM))
            .mount(server)
            .await;
    }

    #[test]
    fn extracts_csrf_token_from_form() {
        let html = r#"<form><input type="hidden" name="csrfmiddlewaretoken" value="abc123"></form>"#;
        assert_eq!(extract_csrf_token(html).unwrap(), "abc123");
    }

    #[test]
    fn reports_missing_csrf_token() {
        assert!(extract_csrf_token("<form></form>").is_err());
    }

    #[test]
    fn discovers_and_exposes_paths() {
        // Result depends on the host environment (HOME); only exercise the code path.
        let _ = AppPaths::discover();
        let (_dir, paths) = paths();
        assert!(paths.config_dir().ends_with("config"));
        assert!(paths.data_dir().ends_with("data"));
        assert!(paths.config_file().ends_with("config.toml"));
        assert!(paths.session_file().ends_with("session.cookies"));
        paths.ensure_dirs().unwrap();
        assert!(paths.config_dir().is_dir() && paths.data_dir().is_dir());
    }

    #[test]
    fn config_dir_is_dot_config_suap_under_home() {
        let home = Path::new("home").join("kelson");
        assert_eq!(config_dir_in(&home), home.join(".config").join("suap"));
        let paths = AppPaths::discover().expect("home directory is available");
        assert!(paths.config_dir().ends_with(Path::new(".config").join("suap")));
    }

    #[test]
    fn config_round_trip_and_defaults() {
        let (_dir, paths) = paths();
        assert_eq!(load_config(&paths).unwrap(), None);
        let config = SuapConfig { username: Some("kelson".to_owned()), ..SuapConfig::default() };
        save_config(&paths, &config).unwrap();
        assert_eq!(load_config(&paths).unwrap(), Some(config));
    }

    #[test]
    fn profiles_have_separate_configuration_and_sessions() {
        let (_dir, default_paths) = paths();
        let local_paths = default_paths.clone().with_profile("local-1").unwrap();
        assert_eq!((default_paths.profile(), local_paths.profile()), (DEFAULT_PROFILE, "local-1"));
        assert!(default_paths.session_file().ends_with("session.cookies"));
        assert!(local_paths.session_file().ends_with("session-local-1.cookies"));

        let local = SuapConfig { base_url: Url::parse("http://localhost:8000/").unwrap(), username: Some("dev".to_owned()), ..SuapConfig::default() };
        save_config(&local_paths, &local).unwrap();
        assert_eq!(load_config(&default_paths).unwrap(), None);
        let production = SuapConfig::default();
        save_config(&default_paths, &production).unwrap();
        assert_eq!(load_config(&local_paths).unwrap(), Some(local));
        assert_eq!(load_config(&default_paths).unwrap(), Some(production));
    }

    #[test]
    fn rejects_invalid_profile_names() {
        for name in ["", "a b", "../x", "ç", &"a".repeat(MAX_PROFILE_LEN + 1)] {
            let (_dir, paths) = paths();
            assert!(matches!(paths.with_profile(name), Err(SuapError::InvalidConfiguration(_))), "{name:?}");
        }
    }

    #[test]
    fn legacy_flat_config_is_the_default_profile_and_is_migrated_on_save() {
        let (_dir, paths) = paths();
        paths.ensure_dirs().unwrap();
        fs::write(paths.config_file(), "base_url = \"https://legacy.example/\"\nusername = \"antigo\"\n").unwrap();
        let legacy = load_config(&paths).unwrap().unwrap();
        assert_eq!((legacy.base_url.as_str(), legacy.username.as_deref()), ("https://legacy.example/", Some("antigo")));
        let other = paths.clone().with_profile("other").unwrap();
        assert_eq!(load_config(&other).unwrap(), None);

        let other_config = SuapConfig { username: Some("novo".to_owned()), ..SuapConfig::default() };
        save_config(&other, &other_config).unwrap();
        let saved = fs::read_to_string(paths.config_file()).unwrap();
        assert!(saved.contains("[profiles.default]") && saved.contains("[profiles.other]"));
        assert!(!saved.starts_with("base_url"));
        assert_eq!(load_config(&paths).unwrap().unwrap(), legacy);
    }

    #[test]
    fn legacy_username_only_keeps_default_url_and_existing_default_wins() {
        let (_dir, paths) = paths();
        paths.ensure_dirs().unwrap();
        fs::write(paths.config_file(), "username = \"so-usuario\"\n").unwrap();
        let config = load_config(&paths).unwrap().unwrap();
        assert_eq!((config.base_url, config.username.as_deref()), (SuapConfig::default().base_url, Some("so-usuario")));

        fs::write(
            paths.config_file(),
            "username = \"ignorado\"\n[profiles.default]\nbase_url = \"https://novo.example/\"\n",
        )
        .unwrap();
        let config = load_config(&paths).unwrap().unwrap();
        assert_eq!((config.base_url.as_str(), config.username), ("https://novo.example/", None));
    }

    #[test]
    fn config_rejects_invalid_scheme_and_toml() {
        let (_dir, paths) = paths();
        let bad = SuapConfig { base_url: Url::parse("ftp://example.org/").unwrap(), ..SuapConfig::default() };
        assert!(matches!(save_config(&paths, &bad), Err(SuapError::InvalidConfiguration(_))));

        paths.ensure_dirs().unwrap();
        fs::write(paths.config_file(), "base_url = [").unwrap();
        assert!(matches!(load_config(&paths), Err(SuapError::TomlDe(_))));

        fs::write(paths.config_file(), "base_url = \"ftp://example.org/\"").unwrap();
        assert!(matches!(load_config(&paths), Err(SuapError::InvalidConfiguration(_))));
    }

    #[tokio::test]
    async fn session_store_persists_and_reloads() {
        let (_dir, paths) = paths();
        let store = SessionStore::open(paths.session_file()).unwrap();
        assert_eq!(store.path(), paths.session_file());
        let url = Url::parse("https://suap.example/").unwrap();
        store.cookie_provider().lock().unwrap().parse("sessionid=1; Max-Age=3600", &url).unwrap();
        store.save().await.unwrap();

        let reloaded = SessionStore::open(paths.session_file()).unwrap();
        assert!(reloaded.cookie_provider().lock().unwrap().get("suap.example", "/", "sessionid").is_some());
    }

    #[test]
    fn session_store_discards_unreadable_or_legacy_files() {
        let (_dir, paths) = paths();
        paths.ensure_dirs().unwrap();
        // Not JSON at all, and the pre-`cookie_store::serde` format (one JSON object per line).
        let legacy = r#"{"raw_cookie":"sessionid=1; Path=/","path":["/",true],"domain":{"HostOnly":"suap.example"},"expires":{"AtUtc":"2999-01-01T00:00:00Z"}}"#;
        for content in ["not json", legacy] {
            fs::write(paths.session_file(), content).unwrap();
            let store = SessionStore::open(paths.session_file()).unwrap();
            assert!(store.cookie_provider().lock().unwrap().get("suap.example", "/", "sessionid").is_none());
        }
    }

    #[tokio::test]
    async fn session_save_requires_parent_directory() {
        let store = SessionStore::open(PathBuf::new()).unwrap();
        assert!(matches!(store.save().await, Err(SuapError::InvalidConfiguration(_))));
    }

    #[tokio::test]
    async fn session_save_reports_poisoned_lock() {
        let (_dir, paths) = paths();
        let store = SessionStore::open(paths.session_file()).unwrap();
        let provider = store.cookie_provider();
        let _ = std::thread::spawn(move || {
            let _guard = provider.lock().unwrap();
            panic!("poison the cookie store");
        })
        .join();
        assert!(matches!(store.save().await, Err(SuapError::CookieStore(_))));
    }

    #[tokio::test]
    async fn login_fails_when_login_page_is_unavailable() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).respond_with(ResponseTemplate::new(500)).mount(&server).await;
        let (_dir, paths) = paths();
        let client = SuapClient::open(&paths, &config_for(&server)).unwrap();
        assert!(matches!(client.login("u", "p").await, Err(SuapError::AuthenticationFailed)));
        let _ = (client.http_client(), client.session());
    }

    #[tokio::test]
    async fn login_fails_on_unauthorized_and_server_error() {
        for status in [401, 500] {
            let server = MockServer::start().await;
            mount_login_page(&server).await;
            Mock::given(method("POST")).respond_with(ResponseTemplate::new(status)).mount(&server).await;
            let (_dir, paths) = paths();
            let client = SuapClient::open(&paths, &config_for(&server)).unwrap();
            assert!(matches!(client.login("u", "p").await, Err(SuapError::AuthenticationFailed)));
        }
    }

    #[tokio::test]
    async fn login_succeeds_and_persists_session() {
        let server = MockServer::start().await;
        mount_login_page(&server).await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/"))
            .mount(&server)
            .await;
        Mock::given(method("GET")).and(path("/")).respond_with(ResponseTemplate::new(200)).mount(&server).await;
        let (_dir, paths) = paths();
        let client = SuapClient::open(&paths, &config_for(&server)).unwrap();
        client.login("u", "p").await.unwrap();
        assert!(paths.session_file().exists());
        assert!(client.is_authenticated().await.unwrap());
    }

    #[tokio::test]
    async fn login_fails_when_redirect_target_errors() {
        let server = MockServer::start().await;
        mount_login_page(&server).await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/"))
            .mount(&server)
            .await;
        Mock::given(method("GET")).and(path("/")).respond_with(ResponseTemplate::new(500)).mount(&server).await;
        let (_dir, paths) = paths();
        let client = SuapClient::open(&paths, &config_for(&server)).unwrap();
        assert!(matches!(client.login("u", "p").await, Err(SuapError::AuthenticationFailed)));
    }

    #[tokio::test]
    async fn login_fails_without_csrf_token() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).respond_with(ResponseTemplate::new(200).set_body_string("<form></form>")).mount(&server).await;
        let (_dir, paths) = paths();
        let client = SuapClient::open(&paths, &config_for(&server)).unwrap();
        assert!(matches!(client.login("u", "p").await, Err(SuapError::Parse(_))));
    }

    #[tokio::test]
    async fn is_authenticated_reflects_response_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/")).respond_with(ResponseTemplate::new(403)).mount(&server).await;
        let (_dir, paths) = paths();
        let client = SuapClient::open(&paths, &config_for(&server)).unwrap();
        assert!(!client.is_authenticated().await.unwrap());
    }

    #[tokio::test]
    async fn fetch_page_returns_body_and_maps_failures() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/ok/")).respond_with(ResponseTemplate::new(200).set_body_string("corpo")).mount(&server).await;
        Mock::given(method("GET")).and(path("/erro/")).respond_with(ResponseTemplate::new(500)).mount(&server).await;
        Mock::given(method("GET"))
            .and(path("/protegido/"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", LOGIN_PATH))
            .mount(&server)
            .await;
        Mock::given(method("GET")).and(path(LOGIN_PATH)).respond_with(ResponseTemplate::new(200)).mount(&server).await;
        let (_dir, paths) = paths();
        let client = SuapClient::open(&paths, &config_for(&server)).unwrap();
        assert_eq!(client.base_url().as_str(), format!("{}/", server.uri()));
        assert_eq!(client.fetch_page("/ok/").await.unwrap(), "corpo");
        assert!(matches!(client.fetch_page("/erro/").await, Err(SuapError::Transport(_))));
        assert!(matches!(client.fetch_page("/protegido/").await, Err(SuapError::NotAuthenticated)));
    }

    #[tokio::test]
    async fn submit_form_posts_fields_and_reports_destination() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/ok/"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/destino/"))
            .mount(&server)
            .await;
        Mock::given(method("GET")).and(path("/destino/")).respond_with(ResponseTemplate::new(200).set_body_string("fim")).mount(&server).await;
        Mock::given(method("POST")).and(path("/erro/")).respond_with(ResponseTemplate::new(500)).mount(&server).await;
        Mock::given(method("POST"))
            .and(path("/protegido/"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", LOGIN_PATH))
            .mount(&server)
            .await;
        Mock::given(method("GET")).and(path(LOGIN_PATH)).respond_with(ResponseTemplate::new(200)).mount(&server).await;
        let (_dir, paths) = paths();
        let client = SuapClient::open(&paths, &config_for(&server)).unwrap();
        let fields = [("a".to_owned(), "1".to_owned())];
        let response = client.submit_form("/ok/", &fields).await.unwrap();
        assert_eq!(response, FormResponse { path: "/destino/".to_owned(), body: "fim".to_owned() });
        assert!(matches!(client.submit_form("/erro/", &fields).await, Err(SuapError::Transport(_))));
        assert!(matches!(client.submit_form("/protegido/", &fields).await, Err(SuapError::NotAuthenticated)));
    }

    #[tokio::test]
    async fn submit_multipart_uploads_files_and_reports_destination() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/upload/"))
            .and(body_string_contains("name=\"campo\""))
            .and(body_string_contains("name=\"arquivo\"; filename=\"a.pdf\""))
            .and(body_string_contains("conteudo"))
            .respond_with(ResponseTemplate::new(200).set_body_string("recebido"))
            .mount(&server)
            .await;
        Mock::given(method("POST")).and(path("/erro/")).respond_with(ResponseTemplate::new(500)).mount(&server).await;
        let (_dir, paths) = paths();
        let client = SuapClient::open(&paths, &config_for(&server)).unwrap();
        let fields = [("campo".to_owned(), "valor".to_owned())];
        let files = [FormFile { field: "arquivo".to_owned(), file_name: "a.pdf".to_owned(), bytes: b"conteudo".to_vec() }];
        let response = client.submit_multipart("/upload/", &fields, &files).await.unwrap();
        assert_eq!(response, FormResponse { path: "/upload/".to_owned(), body: "recebido".to_owned() });
        assert!(matches!(client.submit_multipart("/erro/", &fields, &files).await, Err(SuapError::Transport(_))));
    }

    #[test]
    fn open_defaults_are_serialized_only_when_set() {
        let (_dir, paths) = paths();
        let plain = SuapConfig::default();
        save_config(&paths, &plain).unwrap();
        assert!(!fs::read_to_string(paths.config_file()).unwrap().contains("open"));

        let defaults = OpenDefaults {
            service: Some(7),
            interested: Some("1".to_owned()),
            campus: Some("2".to_owned()),
            center: Some("3".to_owned()),
        };
        let config = SuapConfig { open: defaults, ..plain };
        save_config(&paths, &config).unwrap();
        assert!(fs::read_to_string(paths.config_file()).unwrap().contains("[profiles.default.open]"));
        assert_eq!(load_config(&paths).unwrap(), Some(config));
    }

    #[test]
    fn errors_convert_and_display() {
        let boxed: Box<dyn std::error::Error + Send + Sync> = "boom".into();
        assert_eq!(SuapError::from(boxed).to_string(), "cookie store error: boom");
        for error in [
            SuapError::DirectoriesUnavailable,
            SuapError::NotAuthenticated,
            SuapError::Transport("t".to_owned()),
        ] {
            assert!(!error.to_string().is_empty());
        }
    }
}
