//! Turns the global `[sync]` settings into a backend and a key source.

use std::path::PathBuf;

use suap_core::{AppPaths, SyncSettings};

use crate::{
    backend::{Condition, DirectoryBackend, Object, PutOutcome, SyncBackend},
    credentials::S3Credentials,
    keys::KeySource,
    s3::{S3Backend, S3Settings},
    SyncError,
};

/// Name of the folder backend in the settings.
pub const DIRECTORY_BACKEND: &str = "directory";
/// Name of the S3-compatible backend in the settings.
pub const S3_BACKEND: &str = "s3";

/// Whichever backend the settings name.
pub enum AnyBackend {
    Directory(DirectoryBackend),
    S3(Box<S3Backend>),
}

impl AnyBackend {
    /// Whether the storage really honors conditional writes (a folder always does; an S3-compatible
    /// service is asked, with two writes to a temporary object).
    pub async fn probe_conditional_writes(&self) -> Result<bool, SyncError> {
        match self {
            Self::Directory(_) => Ok(true),
            Self::S3(backend) => backend.probe_conditional_writes().await,
        }
    }
}

impl SyncBackend for AnyBackend {
    async fn get(&self, name: &str) -> Result<Option<Object>, SyncError> {
        match self {
            Self::Directory(backend) => backend.get(name).await,
            Self::S3(backend) => backend.get(name).await,
        }
    }

    async fn put(
        &self,
        name: &str,
        bytes: &[u8],
        condition: Condition,
    ) -> Result<PutOutcome, SyncError> {
        match self {
            Self::Directory(backend) => backend.put(name, bytes, condition).await,
            Self::S3(backend) => backend.put(name, bytes, condition).await,
        }
    }

    fn supports_conditional_writes(&self) -> bool {
        match self {
            Self::Directory(backend) => backend.supports_conditional_writes(),
            Self::S3(backend) => backend.supports_conditional_writes(),
        }
    }
}

fn directory_from(settings: &SyncSettings) -> Result<DirectoryBackend, SyncError> {
    match settings.path.as_deref() {
        Some(path) => Ok(DirectoryBackend::new(PathBuf::from(path))),
        None => Err(SyncError::Backend(
            "the directory backend needs a path".to_owned(),
        )),
    }
}

fn unavailable(settings: &SyncSettings) -> SyncError {
    match settings.backend.as_deref() {
        Some(other) => SyncError::Backend(format!(
            "unknown backend {other:?}: use {DIRECTORY_BACKEND:?} or {S3_BACKEND:?}"
        )),
        None => SyncError::Backend("no backend configured".to_owned()),
    }
}

/// Checks that the settings describe a usable backend, without needing any credentials.
pub fn validate_settings(settings: &SyncSettings) -> Result<(), SyncError> {
    match settings.backend.as_deref() {
        Some(DIRECTORY_BACKEND) => directory_from(settings).map(drop),
        Some(S3_BACKEND) => S3Settings::from_settings(settings).map(drop),
        _ => Err(unavailable(settings)),
    }
}

/// The configured backend. An `s3` backend also needs its `credentials`.
pub fn backend_from(
    settings: &SyncSettings,
    credentials: Option<S3Credentials>,
) -> Result<AnyBackend, SyncError> {
    match settings.backend.as_deref() {
        Some(DIRECTORY_BACKEND) => Ok(AnyBackend::Directory(directory_from(settings)?)),
        Some(S3_BACKEND) => {
            let s3_settings = S3Settings::from_settings(settings)?;
            let credentials = credentials.ok_or_else(|| {
                SyncError::Key(
                    "no S3 credentials: set CHAMADOS_S3_ACCESS_KEY_ID and CHAMADOS_S3_SECRET_ACCESS_KEY, \
                     or run `chamados sync credentials set`"
                        .to_owned(),
                )
            })?;
            Ok(AnyBackend::S3(Box::new(S3Backend::new(
                s3_settings,
                credentials,
            ))))
        }
        _ => Err(unavailable(settings)),
    }
}

/// The configured key source; the system keyring when none is set, and the default key file when the
/// `file` source has no path.
pub fn key_source_from(settings: &SyncSettings, paths: &AppPaths) -> Result<KeySource, SyncError> {
    let name = settings.key_source.as_deref().unwrap_or("keyring");
    let file = settings
        .key_file
        .as_ref()
        .map_or_else(|| paths.default_key_file(), PathBuf::from);
    KeySource::parse(name, Some(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directory(path: Option<&str>) -> SyncSettings {
        SyncSettings {
            backend: Some(DIRECTORY_BACKEND.to_owned()),
            path: path.map(str::to_owned),
            ..SyncSettings::default()
        }
    }

    fn s3(endpoint: Option<&str>, bucket: Option<&str>) -> SyncSettings {
        SyncSettings {
            backend: Some(S3_BACKEND.to_owned()),
            endpoint: endpoint.map(str::to_owned),
            bucket: bucket.map(str::to_owned),
            ..SyncSettings::default()
        }
    }

    fn credentials() -> Option<S3Credentials> {
        Some(S3Credentials {
            access_key_id: "id".to_owned(),
            secret_access_key: "segredo".to_owned(),
        })
    }

    #[test]
    fn builds_each_backend_or_explains_what_is_missing() {
        let folder = backend_from(&directory(Some("/nuvem")), None).unwrap();
        assert!(matches!(folder, AnyBackend::Directory(_)) && folder.supports_conditional_writes());
        let bucket = backend_from(
            &s3(Some("https://conta.r2.cloudflarestorage.com"), Some("b")),
            credentials(),
        )
        .unwrap();
        assert!(matches!(bucket, AnyBackend::S3(_)) && bucket.supports_conditional_writes());

        let no_path = backend_from(&directory(None), None).err().unwrap();
        assert!(no_path.to_string().contains("needs a path"));
        let no_credentials = backend_from(&s3(Some("https://x.example"), Some("b")), None)
            .err()
            .unwrap();
        assert!(no_credentials
            .to_string()
            .contains("chamados sync credentials set"));
        let bad_s3 = backend_from(&s3(None, Some("b")), credentials())
            .err()
            .unwrap();
        assert!(bad_s3.to_string().contains("needs an endpoint"));
        let unknown = SyncSettings {
            backend: Some("ftp".to_owned()),
            ..SyncSettings::default()
        };
        assert!(backend_from(&unknown, None)
            .err()
            .unwrap()
            .to_string()
            .contains("unknown backend"));
        let none = backend_from(&SyncSettings::default(), None).err().unwrap();
        assert!(none.to_string().contains("no backend configured"));
    }

    #[test]
    fn validates_settings_without_credentials() {
        assert!(validate_settings(&directory(Some("/nuvem"))).is_ok());
        assert!(validate_settings(&directory(None)).is_err());
        assert!(validate_settings(&s3(Some("https://x.example"), Some("b"))).is_ok());
        assert!(validate_settings(&s3(Some("http://x.example"), Some("b"))).is_err());
        assert!(validate_settings(&SyncSettings::default()).is_err());
        let unknown = SyncSettings {
            backend: Some("ftp".to_owned()),
            ..SyncSettings::default()
        };
        assert!(validate_settings(&unknown).is_err());
    }

    #[tokio::test]
    async fn a_folder_backend_needs_no_probe() {
        let folder = backend_from(&directory(Some("/nuvem")), None).unwrap();
        assert!(folder.probe_conditional_writes().await.unwrap());
        // Reads and writes go to the folder variant (the S3 variant is exercised against a fake server).
        let temporary = tempfile::tempdir().unwrap();
        let settings = directory(temporary.path().to_str());
        let backend = backend_from(&settings, None).unwrap();
        assert_eq!(backend.get("x.bin").await.unwrap(), None);
        assert_eq!(
            backend.put("x.bin", b"1", Condition::Absent).await.unwrap(),
            PutOutcome::Stored
        );
        assert_eq!(backend.get("x.bin").await.unwrap().unwrap().bytes, b"1");
    }

    #[test]
    fn key_source_defaults_to_the_keyring_and_the_default_key_file() {
        let paths = AppPaths::from_dirs(PathBuf::from("c"), PathBuf::from("d"));
        let with = |source: Option<&str>, file: Option<&str>| SyncSettings {
            key_source: source.map(str::to_owned),
            key_file: file.map(str::to_owned),
            ..SyncSettings::default()
        };
        assert_eq!(
            key_source_from(&SyncSettings::default(), &paths).unwrap(),
            KeySource::Keyring
        );
        assert_eq!(
            key_source_from(&with(Some("file"), None), &paths).unwrap(),
            KeySource::File(paths.default_key_file())
        );
        assert_eq!(
            key_source_from(&with(Some("file"), Some("/k")), &paths).unwrap(),
            KeySource::File(PathBuf::from("/k"))
        );
        assert_eq!(
            key_source_from(&with(Some("env"), None), &paths).unwrap(),
            KeySource::Env
        );
        assert!(key_source_from(&with(Some("nuvem"), None), &paths).is_err());
    }
}
