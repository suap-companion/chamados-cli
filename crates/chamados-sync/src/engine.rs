//! One synchronization round: download, decrypt, merge, upload and apply.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use suap_core::AppPaths;

use crate::{
    backend::{Condition, Object, PutOutcome, SyncBackend},
    crypto::{decrypt, encrypt, Key},
    document::{apply_merged, collect_local, merge, SyncDocument},
    SyncError,
};

/// Name of the one object that holds the encrypted document.
pub const OBJECT_NAME: &str = "chamados-sync-v1.bin";
const MAX_ATTEMPTS: usize = 5;
/// A lock older than this is considered abandoned by a crashed run.
const STALE_LOCK: Duration = Duration::from_secs(10 * 60);

/// What to synchronize.
#[derive(Debug, Clone, Copy, Default)]
pub struct SyncOptions<'a> {
    /// Restrict the round to this profile.
    pub only: Option<&'a str>,
    /// Compute and report everything, but write nothing (neither locally nor remotely).
    pub dry_run: bool,
}

/// The outcome of a round.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// Profiles in the merged document.
    pub profiles: usize,
    /// Local entries created or replaced by newer remote ones.
    pub local_changes: usize,
    /// Whether the remote copy was (or, in a dry run, would be) replaced.
    pub uploaded: bool,
    /// Whether a remote copy existed before this round.
    pub remote_existed: bool,
}

/// Runs one round against `backend`, encrypting everything with `key`.
///
/// The round is repeated (from a fresh download) when somebody else wrote meanwhile, and, for
/// backends without conditional writes, confirmed by reading the upload back.
pub async fn sync_once<B: SyncBackend>(
    paths: &AppPaths,
    backend: &B,
    key: &Key,
    options: &SyncOptions<'_>,
) -> Result<SyncReport, SyncError> {
    for _ in 0..MAX_ATTEMPTS {
        let local = collect_local(paths, options.only)?;
        let object = backend.get(OBJECT_NAME).await?;
        let remote = match &object {
            Some(object) => SyncDocument::decode(&decrypt(key, &object.bytes)?)?,
            None => SyncDocument::default(),
        };
        let merged = merge(&local, &remote);
        let uploaded = merged != remote;
        let mut report = SyncReport {
            profiles: merged.profiles.len(),
            local_changes: 0,
            uploaded,
            remote_existed: object.is_some(),
        };

        if options.dry_run {
            report.local_changes = apply_merged(paths, &merged, options.only, false)?.total();
            return Ok(report);
        }
        let version = object.as_ref().map(|object| object.version.as_str());
        let stored = !uploaded || upload(backend, key, &merged, version).await?;
        if !stored {
            continue;
        }
        report.local_changes = apply_merged(paths, &merged, options.only, true)?.total();
        return Ok(report);
    }
    Err(SyncError::Conflict)
}

/// Uploads `merged`; `false` means somebody else got there first and the round must restart.
async fn upload<B: SyncBackend>(
    backend: &B,
    key: &Key,
    merged: &SyncDocument,
    version: Option<&str>,
) -> Result<bool, SyncError> {
    let bytes = encrypt(key, &merged.encode()?);
    let conditional = backend.supports_conditional_writes();
    let condition = match (conditional, version) {
        (false, _) => Condition::Always,
        (true, Some(version)) => Condition::Version(version.to_owned()),
        (true, None) => Condition::Absent,
    };
    if backend.put(OBJECT_NAME, &bytes, condition).await? == PutOutcome::PreconditionFailed {
        return Ok(false);
    }
    if conditional {
        return Ok(true);
    }
    // Without conditional writes, read it back: if another writer won the race, start over.
    Ok(is_ours(backend.get(OBJECT_NAME).await?, &bytes))
}

/// Whether the object read back is exactly what was just uploaded.
fn is_ours(stored: Option<Object>, uploaded: &[u8]) -> bool {
    stored.is_some_and(|stored| stored.bytes == uploaded)
}

/// Keeps two `chamados sync` runs from overlapping; released when dropped.
#[derive(Debug)]
pub struct SyncLock {
    path: PathBuf,
}

impl SyncLock {
    /// Takes the lock at `path`; `Ok(None)` if another run holds it.
    pub fn acquire(path: &Path) -> Result<Option<Self>, SyncError> {
        fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
        if is_stale(path) {
            fs::remove_file(path)?;
        }
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(_) => Ok(Some(Self {
                path: path.to_path_buf(),
            })),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}

impl Drop for SyncLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn is_stale(path: &Path) -> bool {
    let age = fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok());
    age.is_some_and(|age| age > STALE_LOCK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::DirectoryBackend;
    use chamados_core::TitleStore;
    use std::cell::RefCell;
    use suap_core::{load_config, save_config, SuapConfig};
    use tempfile::{tempdir, TempDir};

    /// A machine: its own configuration and data directories.
    fn machine(root: &Path, name: &str) -> AppPaths {
        AppPaths::from_dirs(root.join(name).join("config"), root.join(name).join("data"))
    }

    fn set_title(paths: &AppPaths, id: &str, title: &str, at: u64) {
        let mut titles = TitleStore::open(paths.titles_file()).unwrap();
        titles.set(id, title, at).unwrap();
        titles.save().unwrap();
    }

    fn enable_sync(paths: &AppPaths, name: &str, at: u64) -> AppPaths {
        let profile = paths.clone().with_profile(name).unwrap();
        let config = SuapConfig {
            sync: true,
            updated_at: at,
            ..SuapConfig::default()
        };
        save_config(&profile, &config).unwrap();
        profile
    }

    /// Every round in these tests runs through `Scripted`, so the generic engine has one instantiation
    /// here and its coverage does not depend on which backend type happened to take which path.
    fn setup() -> (TempDir, AppPaths, AppPaths, Scripted, Key) {
        let root = tempdir().unwrap();
        let (a, b) = (machine(root.path(), "a"), machine(root.path(), "b"));
        let backend = scripted(root.path(), true, 0, None);
        (root, a, b, backend, Key::generate())
    }

    #[tokio::test]
    async fn two_machines_converge_through_an_encrypted_object() {
        let (root, a, b, backend, key) = setup();
        let options = SyncOptions::default();
        let a_profile = enable_sync(&a, "default", 1);
        set_title(&a_profile, "5", "Moodle 5.3", 10);

        let first = sync_once(&a, &backend, &key, &options).await.unwrap();
        assert_eq!(
            first,
            SyncReport {
                profiles: 1,
                local_changes: 0,
                uploaded: true,
                remote_existed: false
            }
        );

        // What the cloud holds is ciphertext only.
        let stored = std::fs::read(root.path().join("nuvem").join(OBJECT_NAME)).unwrap();
        assert!(!String::from_utf8_lossy(&stored).contains("Moodle"));

        // Machine B starts empty, receives the profile and the title, and changes the title.
        let second = sync_once(&b, &backend, &key, &options).await.unwrap();
        assert_eq!(
            (second.local_changes, second.uploaded, second.remote_existed),
            (2, false, true)
        );
        let b_profile = b.clone().with_profile("default").unwrap();
        assert_eq!(
            TitleStore::open(b_profile.titles_file()).unwrap().get("5"),
            Some("Moodle 5.3")
        );
        assert!(load_config(&b_profile).unwrap().unwrap().sync);
        set_title(&b_profile, "5", "Moodle 5.3 (feito)", 20);
        set_title(&b_profile, "6", "Outro", 21);
        sync_once(&b, &backend, &key, &options).await.unwrap();

        // Machine A receives both changes and a second round changes nothing.
        let third = sync_once(&a, &backend, &key, &options).await.unwrap();
        assert_eq!((third.local_changes, third.uploaded), (2, false));
        assert_eq!(
            TitleStore::open(a_profile.titles_file()).unwrap().get("5"),
            Some("Moodle 5.3 (feito)")
        );
        let again = sync_once(&a, &backend, &key, &options).await.unwrap();
        assert_eq!((again.local_changes, again.uploaded), (0, false));
    }

    #[tokio::test]
    async fn concurrent_edits_and_removals_merge_by_entry() {
        let (_root, a, b, backend, key) = setup();
        let options = SyncOptions::default();
        let (a_profile, b_profile) = (enable_sync(&a, "default", 1), enable_sync(&b, "default", 1));
        set_title(&a_profile, "1", "base", 5);
        sync_once(&a, &backend, &key, &options).await.unwrap();
        sync_once(&b, &backend, &key, &options).await.unwrap();

        // Offline: A edits ticket 1 and B removes it later, while A adds 2 and B adds 3.
        set_title(&a_profile, "1", "editado em A", 10);
        set_title(&a_profile, "2", "so em A", 11);
        let mut titles = TitleStore::open(b_profile.titles_file()).unwrap();
        titles.remove("1", 12).unwrap();
        titles.set("3", "so em B", 13).unwrap();
        titles.save().unwrap();

        sync_once(&a, &backend, &key, &options).await.unwrap();
        sync_once(&b, &backend, &key, &options).await.unwrap();
        sync_once(&a, &backend, &key, &options).await.unwrap();
        for profile in [&a_profile, &b_profile] {
            let titles = TitleStore::open(profile.titles_file()).unwrap();
            assert_eq!(titles.get("1"), None, "the newer removal wins");
            assert_eq!(
                (titles.get("2"), titles.get("3")),
                (Some("so em A"), Some("so em B"))
            );
        }
    }

    #[tokio::test]
    async fn dry_run_and_restricted_runs_write_nothing_they_should_not() {
        let (root, a, b, backend, key) = setup();
        let a_profile = enable_sync(&a, "default", 1);
        enable_sync(&a, "local", 1);
        set_title(&a_profile, "5", "x", 1);

        let dry = SyncOptions {
            dry_run: true,
            ..SyncOptions::default()
        };
        let report = sync_once(&a, &backend, &key, &dry).await.unwrap();
        assert!(report.uploaded && !report.remote_existed);
        assert!(!root.path().join("nuvem").exists());

        let only = SyncOptions {
            only: Some("local"),
            ..SyncOptions::default()
        };
        let report = sync_once(&a, &backend, &key, &only).await.unwrap();
        assert_eq!(report.profiles, 1);
        sync_once(&b, &backend, &key, &SyncOptions::default())
            .await
            .unwrap();
        assert!(load_config(&b.clone().with_profile("local").unwrap())
            .unwrap()
            .is_some());
        assert!(load_config(&b.clone().with_profile("default").unwrap())
            .unwrap()
            .is_none());

        // A dry run on the other machine reports the incoming change without applying it.
        let dry_b = sync_once(&a, &backend, &key, &dry).await.unwrap();
        assert!(dry_b.uploaded);
        assert_eq!(
            TitleStore::open(b.clone().with_profile("local").unwrap().titles_file())
                .unwrap()
                .get("5"),
            None
        );
    }

    #[tokio::test]
    async fn a_wrong_key_or_a_foreign_object_is_refused() {
        let (root, a, _b, backend, key) = setup();
        enable_sync(&a, "default", 1);
        sync_once(&a, &backend, &key, &SyncOptions::default())
            .await
            .unwrap();
        let error = sync_once(&a, &backend, &Key::generate(), &SyncOptions::default())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("wrong key or corrupted data"));

        // A valid envelope holding something that is not a document.
        let junk = encrypt(&key, b"nao e um documento");
        std::fs::write(root.path().join("nuvem").join(OBJECT_NAME), junk).unwrap();
        assert!(sync_once(&a, &backend, &key, &SyncOptions::default())
            .await
            .is_err());
    }

    /// A backend that loses the race a few times, or that cannot do conditional writes.
    struct Scripted {
        inner: DirectoryBackend,
        conditional: bool,
        fail_puts: RefCell<usize>,
        /// After the first write, reads return this object (another writer got in right after us).
        stale: Option<Vec<u8>>,
        writes: RefCell<usize>,
    }

    impl SyncBackend for Scripted {
        async fn get(&self, name: &str) -> Result<Option<Object>, SyncError> {
            if let (Some(stale), true) = (&self.stale, *self.writes.borrow() > 0) {
                let object = Object {
                    bytes: stale.clone(),
                    version: "outra".to_owned(),
                };
                return Ok(Some(object));
            }
            self.inner.get(name).await
        }

        async fn put(
            &self,
            name: &str,
            bytes: &[u8],
            condition: Condition,
        ) -> Result<PutOutcome, SyncError> {
            if *self.fail_puts.borrow() > 0 {
                *self.fail_puts.borrow_mut() -= 1;
                return Ok(PutOutcome::PreconditionFailed);
            }
            *self.writes.borrow_mut() += 1;
            self.inner.put(name, bytes, condition).await
        }

        fn supports_conditional_writes(&self) -> bool {
            self.conditional
        }
    }

    fn scripted(
        root: &Path,
        conditional: bool,
        fail_puts: usize,
        stale: Option<Vec<u8>>,
    ) -> Scripted {
        Scripted {
            inner: DirectoryBackend::new(root.join("nuvem")),
            conditional,
            fail_puts: RefCell::new(fail_puts),
            stale,
            writes: RefCell::new(0),
        }
    }

    #[tokio::test]
    async fn retries_after_losing_a_conditional_write_and_gives_up_eventually() {
        let (root, a, _b, _backend, key) = setup();
        enable_sync(&a, "default", 1);
        let flaky = scripted(root.path(), true, 2, None);
        let report = sync_once(&a, &flaky, &key, &SyncOptions::default())
            .await
            .unwrap();
        assert!(report.uploaded);

        let hopeless = scripted(root.path(), true, 99, None);
        set_title(&a.clone().with_profile("default").unwrap(), "9", "novo", 50);
        let error = sync_once(&a, &hopeless, &key, &SyncOptions::default())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("could not agree"));
    }

    #[tokio::test]
    async fn backends_without_conditional_writes_confirm_by_reading_back() {
        let (root, a, _b, _backend, key) = setup();
        enable_sync(&a, "default", 1);
        let plain = scripted(root.path(), false, 0, None);
        let report = sync_once(&a, &plain, &key, &SyncOptions::default())
            .await
            .unwrap();
        assert!(report.uploaded);

        // If the read-back does not match, the round restarts and, failing every time, gives up.
        set_title(&a.clone().with_profile("default").unwrap(), "9", "novo", 50);
        let other = encrypt(&key, &SyncDocument::default().encode().unwrap());
        let racing = scripted(root.path(), false, 0, Some(other));
        let error = sync_once(&a, &racing, &key, &SyncOptions::default())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("could not agree"));
    }

    #[test]
    fn read_back_must_match_what_was_uploaded() {
        let object = |bytes: &[u8]| {
            Some(Object {
                bytes: bytes.to_vec(),
                version: "v".to_owned(),
            })
        };
        assert!(is_ours(object(b"igual"), b"igual"));
        assert!(!is_ours(object(b"outro"), b"igual"));
        assert!(!is_ours(None, b"igual"));
    }

    #[test]
    fn the_lock_admits_one_run_at_a_time_and_recovers_from_stale_files() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("sub").join("sync.lock");
        let first = SyncLock::acquire(&path)
            .unwrap()
            .expect("first run gets the lock");
        assert!(SyncLock::acquire(&path).unwrap().is_none());
        drop(first);
        assert!(!path.exists());
        let again = SyncLock::acquire(&path).unwrap();
        assert!(again.is_some());
        drop(again);

        // A lock left by a crashed run, long ago, is taken over.
        fs::write(&path, "").unwrap();
        let old = SystemTime::now() - Duration::from_secs(3600);
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert!(SyncLock::acquire(&path).unwrap().is_some());
        // A file name the system refuses reports the I/O error; a parent that is a file does too.
        assert!(SyncLock::acquire(&directory.path().join("x".repeat(300))).is_err());
        let blocker = directory.path().join("arquivo");
        fs::write(&blocker, "").unwrap();
        assert!(SyncLock::acquire(&blocker.join("sync.lock")).is_err());
    }
}
