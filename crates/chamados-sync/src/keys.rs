//! Where the encryption key lives.
//!
//! - [`KeySource::Keyring`]: the system secret store (Windows Credential Manager, macOS Keychain,
//!   Linux Secret Service). It needs an unlocked desktop session, so it does not suit `cron`.
//! - [`KeySource::File`]: a hexadecimal key in a file only the owner can read (`0600` on Unix).
//! - [`KeySource::Env`]: the hexadecimal key in the [`KEY_ENV`] environment variable (read only).

use std::{fs, path::PathBuf};

use crate::{crypto::Key, SyncError};

/// Environment variable holding the hexadecimal key for [`KeySource::Env`].
pub const KEY_ENV: &str = "CHAMADOS_SYNC_KEY";
const KEYRING_SERVICE: &str = "chamados-sync";
const KEYRING_USER: &str = "encryption-key";

/// Where the key is read from and written to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
    Keyring,
    File(PathBuf),
    Env,
}

impl KeySource {
    /// The source named `name` (`keyring`, `file` or `env`); `file` needs a path.
    pub fn parse(name: &str, file: Option<PathBuf>) -> Result<Self, SyncError> {
        match (name, file) {
            ("keyring", _) => Ok(Self::Keyring),
            ("env", _) => Ok(Self::Env),
            ("file", Some(path)) => Ok(Self::File(path)),
            ("file", None) => Err(SyncError::Key(
                "the file key source needs a key file path".to_owned(),
            )),
            (other, _) => Err(SyncError::Key(format!(
                "unknown key source {other:?}: use keyring, file or env"
            ))),
        }
    }

    /// Short name, as accepted by [`KeySource::parse`].
    pub fn name(&self) -> &'static str {
        match self {
            Self::Keyring => "keyring",
            Self::File(_) => "file",
            Self::Env => "env",
        }
    }
}

/// Reads the key from `source`; `Ok(None)` when none has been stored yet.
pub fn load_key(source: &KeySource) -> Result<Option<Key>, SyncError> {
    load_key_with_env(source, &|name| std::env::var(name).ok())
}

/// Like [`load_key`], with the environment lookup injected (for tests).
pub fn load_key_with_env(
    source: &KeySource,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Option<Key>, SyncError> {
    match source {
        KeySource::Env => env(KEY_ENV).map(|hex| Key::from_hex(&hex)).transpose(),
        KeySource::File(path) => {
            if !path.exists() {
                return Ok(None);
            }
            check_private(path)?;
            Ok(Some(Key::from_hex(&fs::read_to_string(path)?)?))
        }
        KeySource::Keyring => match keyring_entry()?.get_secret() {
            Err(keyring_core::Error::NoEntry) => Ok(None),
            found => Ok(Some(Key::from_bytes(&found?)?)),
        },
    }
}

/// Stores `key` in `source`; an existing key is only replaced when `overwrite` is set.
pub fn store_key(source: &KeySource, key: &Key, overwrite: bool) -> Result<(), SyncError> {
    if matches!(source, KeySource::Env) {
        return Err(SyncError::Key(format!(
            "the env key source is read only: set {KEY_ENV} yourself"
        )));
    }
    if !overwrite && load_key(source)?.is_some() {
        return Err(SyncError::Key(
            "a key already exists; use --force to replace it (data encrypted with it becomes unreadable)"
                .to_owned(),
        ));
    }
    match source {
        KeySource::File(path) => write_private(path, &key.to_hex()),
        _ => Ok(keyring_entry()?.set_secret(key.as_bytes())?),
    }
}

fn keyring_entry() -> Result<keyring_core::Entry, SyncError> {
    keyring_entry_named(KEYRING_USER)
}

/// The keyring entry called `user` of this program's service, installing the native store if needed.
pub(crate) fn keyring_entry_named(user: &str) -> Result<keyring_core::Entry, SyncError> {
    if keyring_core::get_default_store().is_none() {
        install_native_store()?;
    }
    Ok(keyring_core::Entry::new(KEYRING_SERVICE, user)?)
}

/// Serializes the tests that replace the process-wide default keyring store.
#[cfg(test)]
pub(crate) static KEYRING_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Selects the platform's native secret store as the keyring default.
pub fn install_native_store() -> Result<(), SyncError> {
    install_store(native_store)
}

/// Makes the store built by `make` the keyring default (the seam that lets tests use a mock).
fn install_store(
    make: impl FnOnce() -> Result<std::sync::Arc<keyring_core::CredentialStore>, SyncError>,
) -> Result<(), SyncError> {
    let store = make()?;
    keyring_core::set_default_store(store);
    Ok(())
}

#[cfg(windows)]
fn native_store() -> Result<std::sync::Arc<keyring_core::CredentialStore>, SyncError> {
    Ok(windows_native_keyring_store::Store::new()?)
}

#[cfg(target_os = "macos")]
fn native_store() -> Result<std::sync::Arc<keyring_core::CredentialStore>, SyncError> {
    Ok(apple_native_keyring_store::keychain::Store::new()?)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn native_store() -> Result<std::sync::Arc<keyring_core::CredentialStore>, SyncError> {
    Ok(zbus_secret_service_keyring_store::Store::new()?)
}

/// Refuses a key file other users can read (Unix); other platforms rely on the profile directory.
fn check_private(path: &std::path::Path) -> Result<(), SyncError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path)?.permissions().mode();
        if mode & 0o077 != 0 {
            return Err(SyncError::Key(format!(
                "{} is readable by other users (mode {:o}); run chmod 600",
                path.display(),
                mode & 0o777
            )));
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn write_private(path: &std::path::Path, content: &str) -> Result<(), SyncError> {
    fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new(".")))?;
    #[cfg(unix)]
    {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(content.as_bytes())?;
    }
    #[cfg(not(unix))]
    fs::write(path, content)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn use_mock_keyring() {
        keyring_core::set_default_store(keyring_core::mock::Store::new().unwrap());
    }

    #[test]
    fn parses_key_sources() {
        let file = Some(PathBuf::from("k"));
        assert_eq!(
            KeySource::parse("keyring", None).unwrap(),
            KeySource::Keyring
        );
        assert_eq!(
            KeySource::parse("env", file.clone()).unwrap(),
            KeySource::Env
        );
        assert_eq!(
            KeySource::parse("file", file).unwrap(),
            KeySource::File(PathBuf::from("k"))
        );
        assert!(KeySource::parse("file", None).is_err());
        assert!(KeySource::parse("nuvem", None)
            .unwrap_err()
            .to_string()
            .contains("unknown key source"));
        let names: Vec<_> = [
            KeySource::Keyring,
            KeySource::Env,
            KeySource::File(PathBuf::new()),
        ]
        .iter()
        .map(KeySource::name)
        .collect();
        assert_eq!(names, ["keyring", "env", "file"]);
    }

    #[test]
    fn env_source_is_read_only_and_validated() {
        let key = Key::generate();
        let hex = key.to_hex();
        let with = |value: Option<String>| move |_: &str| value.clone();
        assert_eq!(
            load_key_with_env(&KeySource::Env, &with(Some(hex))).unwrap(),
            Some(key.clone())
        );
        assert_eq!(
            load_key_with_env(&KeySource::Env, &with(None)).unwrap(),
            None
        );
        assert!(load_key_with_env(&KeySource::Env, &with(Some("curta".to_owned()))).is_err());
        assert!(load_key(&KeySource::Env).is_ok());
        let error = store_key(&KeySource::Env, &key, true)
            .unwrap_err()
            .to_string();
        assert!(error.contains("read only") && error.contains(KEY_ENV));
    }

    #[test]
    fn file_source_stores_privately_and_refuses_overwrite_without_force() {
        let directory = tempdir().unwrap();
        let source = KeySource::File(directory.path().join("sub").join("sync.key"));
        assert_eq!(load_key(&source).unwrap(), None);

        let key = Key::generate();
        store_key(&source, &key, false).unwrap();
        assert_eq!(load_key(&source).unwrap(), Some(key.clone()));

        let other = Key::generate();
        let error = store_key(&source, &other, false).unwrap_err().to_string();
        assert!(error.contains("already exists"));
        assert_eq!(load_key(&source).unwrap(), Some(key));
        store_key(&source, &other, true).unwrap();
        assert_eq!(load_key(&source).unwrap(), Some(other));
    }

    #[cfg(unix)]
    #[test]
    fn file_source_refuses_keys_readable_by_others() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempdir().unwrap();
        let path = directory.path().join("sync.key");
        let source = KeySource::File(path.clone());
        store_key(&source, &Key::generate(), false).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let error = load_key(&source).unwrap_err().to_string();
        assert!(error.contains("readable by other users") && error.contains("chmod 600"));
    }

    #[test]
    fn file_source_rejects_garbage_content() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("sync.key");
        write_private(&path, "isto nao e uma chave").unwrap();
        assert!(load_key(&KeySource::File(path)).is_err());
    }

    /// One test on purpose: both parts change the process-wide default keyring store.
    #[test]
    fn keyring_source_round_trips_with_a_mock_store() {
        let _guard = KEYRING_TEST_LOCK.lock().unwrap();
        use_mock_keyring();
        let source = KeySource::Keyring;
        assert_eq!(load_key(&source).unwrap(), None);
        let key = Key::generate();
        store_key(&source, &key, false).unwrap();
        assert_eq!(load_key(&source).unwrap(), Some(key));
        assert!(store_key(&source, &Key::generate(), false).is_err());
        store_key(&source, &Key::generate(), true).unwrap();

        // Installing the native store works or fails depending on the machine (a container has no
        // secret service); either way it must not panic. The mock is put back afterwards.
        let _ = install_native_store();
        // With no default store at all, opening the keyring installs the native one by itself.
        keyring_core::unset_default_store();
        let _ = load_key(&source);
        // The seam: a constructor that fails is reported, one that works becomes the default.
        let failing = install_store(|| Err(SyncError::Key("sem chaveiro".to_owned())));
        assert!(failing.unwrap_err().to_string().contains("sem chaveiro"));
        install_store(|| Ok(keyring_core::mock::Store::new()?)).unwrap();
        assert_eq!(load_key(&source).unwrap(), None);
    }
}
