//! Turns the global `[sync]` settings into a backend and a key source.

use std::path::PathBuf;

use suap_core::{AppPaths, SyncSettings};

use crate::{backend::DirectoryBackend, keys::KeySource, SyncError};

/// Name of the folder backend in the settings.
pub const DIRECTORY_BACKEND: &str = "directory";

/// The configured backend; fails if none is configured or it is not available yet.
pub fn backend_from(settings: &SyncSettings) -> Result<DirectoryBackend, SyncError> {
    match (settings.backend.as_deref(), settings.path.as_deref()) {
        (Some(DIRECTORY_BACKEND), Some(path)) => Ok(DirectoryBackend::new(PathBuf::from(path))),
        (Some(DIRECTORY_BACKEND), None) => Err(SyncError::Backend(
            "the directory backend needs a path".to_owned(),
        )),
        (Some(other), _) => Err(SyncError::Backend(format!(
            "unknown backend {other:?}: only {DIRECTORY_BACKEND:?} is available for now"
        ))),
        (None, _) => Err(SyncError::Backend("no backend configured".to_owned())),
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

    fn settings(
        backend: Option<&str>,
        path: Option<&str>,
        source: Option<&str>,
        file: Option<&str>,
    ) -> SyncSettings {
        SyncSettings {
            backend: backend.map(str::to_owned),
            path: path.map(str::to_owned),
            key_source: source.map(str::to_owned),
            key_file: file.map(str::to_owned),
        }
    }

    #[test]
    fn builds_the_directory_backend_or_explains_what_is_missing() {
        assert!(backend_from(&settings(Some("directory"), Some("/nuvem"), None, None)).is_ok());
        let no_path = backend_from(&settings(Some("directory"), None, None, None)).unwrap_err();
        assert!(no_path.to_string().contains("needs a path"));
        let unknown = backend_from(&settings(Some("s3"), None, None, None)).unwrap_err();
        assert!(unknown.to_string().contains("unknown backend"));
        let none = backend_from(&SyncSettings::default()).unwrap_err();
        assert!(none.to_string().contains("no backend configured"));
    }

    #[test]
    fn key_source_defaults_to_the_keyring_and_the_default_key_file() {
        let paths = AppPaths::from_dirs(PathBuf::from("c"), PathBuf::from("d"));
        let default = key_source_from(&SyncSettings::default(), &paths).unwrap();
        assert_eq!(default, KeySource::Keyring);
        let file = key_source_from(&settings(None, None, Some("file"), None), &paths).unwrap();
        assert_eq!(file, KeySource::File(paths.default_key_file()));
        let custom =
            key_source_from(&settings(None, None, Some("file"), Some("/k")), &paths).unwrap();
        assert_eq!(custom, KeySource::File(PathBuf::from("/k")));
        assert_eq!(
            key_source_from(&settings(None, None, Some("env"), None), &paths).unwrap(),
            KeySource::Env
        );
        assert!(key_source_from(&settings(None, None, Some("nuvem"), None), &paths).is_err());
    }
}
