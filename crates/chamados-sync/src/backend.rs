//! Transport for the encrypted document: where it is stored, never what it says.

use std::{fs, path::PathBuf};

use crate::SyncError;

/// A stored object and the version (an ETag-like validator) it was read at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    pub bytes: Vec<u8>,
    pub version: String,
}

/// What a write requires of the object currently stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Condition {
    /// Write unconditionally.
    Always,
    /// Write only if there is no object yet (`If-None-Match: *`).
    Absent,
    /// Write only if the object is still at this version (`If-Match`).
    Version(String),
}

/// Result of a write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PutOutcome {
    Stored,
    /// The [`Condition`] did not hold: somebody else wrote in the meantime.
    PreconditionFailed,
}

/// Storage the synchronization talks to (a directory, an S3-compatible bucket...).
#[allow(async_fn_in_trait)]
pub trait SyncBackend {
    /// Reads the object called `name`, if it exists.
    async fn get(&self, name: &str) -> Result<Option<Object>, SyncError>;
    /// Writes the object `name` if `condition` holds.
    async fn put(
        &self,
        name: &str,
        bytes: &[u8],
        condition: Condition,
    ) -> Result<PutOutcome, SyncError>;
    /// Whether [`Condition::Absent`] and [`Condition::Version`] are honored by the storage.
    fn supports_conditional_writes(&self) -> bool;
}

/// The version of `bytes`: a 64-bit FNV-1a hash in hexadecimal.
pub fn version_of(bytes: &[u8]) -> String {
    let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{hash:016x}")
}

/// Stores the objects as files of a directory (a synced folder, a shared drive, a test fixture).
#[derive(Debug, Clone)]
pub struct DirectoryBackend {
    directory: PathBuf,
}

impl DirectoryBackend {
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    fn path_of(&self, name: &str) -> Result<PathBuf, SyncError> {
        if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
            return Err(SyncError::Backend(format!("invalid object name {name:?}")));
        }
        Ok(self.directory.join(name))
    }
}

impl SyncBackend for DirectoryBackend {
    async fn get(&self, name: &str) -> Result<Option<Object>, SyncError> {
        let path = self.path_of(name)?;
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(path)?;
        let version = version_of(&bytes);
        Ok(Some(Object { bytes, version }))
    }

    async fn put(
        &self,
        name: &str,
        bytes: &[u8],
        condition: Condition,
    ) -> Result<PutOutcome, SyncError> {
        let current = self.get(name).await?;
        let holds = match (&condition, &current) {
            (Condition::Always, _) => true,
            (Condition::Absent, current) => current.is_none(),
            (Condition::Version(expected), Some(current)) => &current.version == expected,
            (Condition::Version(_), None) => false,
        };
        if !holds {
            return Ok(PutOutcome::PreconditionFailed);
        }
        fs::create_dir_all(&self.directory)?;
        let path = self.path_of(name)?;
        let temporary = path.with_extension("tmp");
        fs::write(&temporary, bytes)?;
        fs::rename(temporary, path)?;
        Ok(PutOutcome::Stored)
    }

    fn supports_conditional_writes(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn versions_depend_on_content() {
        assert_eq!(version_of(b"a"), version_of(b"a"));
        assert_ne!(version_of(b"a"), version_of(b"b"));
        assert_eq!(version_of(b"").len(), 16);
    }

    #[tokio::test]
    async fn directory_backend_stores_and_honors_conditions() {
        let directory = tempdir().unwrap();
        let backend = DirectoryBackend::new(directory.path().join("nuvem"));
        assert!(backend.supports_conditional_writes());
        assert_eq!(backend.get("doc.bin").await.unwrap(), None);

        // Create only if absent.
        let stored = backend
            .put("doc.bin", b"um", Condition::Absent)
            .await
            .unwrap();
        assert_eq!(stored, PutOutcome::Stored);
        let again = backend
            .put("doc.bin", b"outro", Condition::Absent)
            .await
            .unwrap();
        assert_eq!(again, PutOutcome::PreconditionFailed);

        let first = backend.get("doc.bin").await.unwrap().unwrap();
        assert_eq!(
            (first.bytes.as_slice(), first.version.as_str()),
            (&b"um"[..], version_of(b"um").as_str())
        );

        // Replace only at the version that was read.
        let stale = backend
            .put("doc.bin", b"dois", Condition::Version("antiga".to_owned()))
            .await
            .unwrap();
        assert_eq!(stale, PutOutcome::PreconditionFailed);
        let fresh = backend
            .put("doc.bin", b"dois", Condition::Version(first.version))
            .await
            .unwrap();
        assert_eq!(fresh, PutOutcome::Stored);
        assert_eq!(
            backend.get("doc.bin").await.unwrap().unwrap().bytes,
            b"dois"
        );

        // A version condition on a missing object fails; unconditional writes always work.
        let missing = backend
            .put("novo.bin", b"x", Condition::Version("v".to_owned()))
            .await
            .unwrap();
        assert_eq!(missing, PutOutcome::PreconditionFailed);
        assert_eq!(
            backend
                .put("novo.bin", b"x", Condition::Always)
                .await
                .unwrap(),
            PutOutcome::Stored
        );
        assert!(!directory.path().join("nuvem").join("doc.tmp").exists());
    }

    #[tokio::test]
    async fn directory_backend_rejects_unsafe_names() {
        let directory = tempdir().unwrap();
        let backend = DirectoryBackend::new(directory.path().to_path_buf());
        for name in ["", "../fora", "a/b", "a\\b", ".oculto"] {
            assert!(backend.get(name).await.is_err(), "{name:?}");
            assert!(
                backend.put(name, b"x", Condition::Always).await.is_err(),
                "{name:?}"
            );
        }
    }
}
