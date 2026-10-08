//! Ticket titles kept only on this machine (SUAP tickets have no title).
//!
//! One JSON document per profile, one entry per ticket. Every entry carries the Unix time of its last
//! change and removals are kept as entries without a title (tombstones), so two copies of the file can
//! later be merged entry by entry, the newest change winning.

use std::{
    collections::BTreeMap,
    fmt::Display,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{check_ticket_id, TicketError};

const FORMAT_VERSION: u32 = 1;
/// Longest title accepted, in characters.
pub const MAX_TITLE_CHARS: usize = 120;

/// The local title of one ticket; `title` is `None` for a removed title (tombstone).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TitleEntry {
    pub title: Option<String>,
    /// Unix time (seconds) of the last change.
    pub updated_at: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct TitleFile {
    version: u32,
    tickets: BTreeMap<String, TitleEntry>,
}

/// Local titles of one profile, backed by a JSON file.
#[derive(Debug)]
pub struct TitleStore {
    path: PathBuf,
    entries: BTreeMap<String, TitleEntry>,
}

fn file_error(path: &Path, error: impl Display) -> TicketError {
    TicketError::Source(format!("titles file {}: {error}", path.display()))
}

/// Trims `title` and checks it is a non-empty, single-line text of at most [`MAX_TITLE_CHARS`].
pub fn validate_title(title: &str) -> Result<String, TicketError> {
    let title = title.trim();
    if title.is_empty() {
        return Err(TicketError::Source("the title is empty".to_owned()));
    }
    if title.contains(['\r', '\n']) {
        return Err(TicketError::Source(
            "the title must be a single line".to_owned(),
        ));
    }
    if title.chars().count() > MAX_TITLE_CHARS {
        return Err(TicketError::Source(format!(
            "the title is longer than {MAX_TITLE_CHARS} characters"
        )));
    }
    Ok(title.to_owned())
}

impl TitleStore {
    /// Loads the store at `path`; a missing file is an empty store, an unreadable one is an error
    /// (the titles are user data and are never discarded silently).
    pub fn open(path: PathBuf) -> Result<Self, TicketError> {
        let mut entries = BTreeMap::new();
        if path.exists() {
            let content = fs::read_to_string(&path).map_err(|error| file_error(&path, error))?;
            let file: TitleFile =
                serde_json::from_str(&content).map_err(|error| file_error(&path, error))?;
            if file.version != FORMAT_VERSION {
                let message = format!("unsupported format version {}", file.version);
                return Err(file_error(&path, message));
            }
            entries = file.tickets;
        }
        Ok(Self { path, entries })
    }

    /// The local title of ticket `id`, if it has one.
    pub fn get(&self, id: &str) -> Option<&str> {
        self.entries.get(id)?.title.as_deref()
    }

    /// Sets the title of ticket `id` (validated) with `now` as the change time.
    pub fn set(&mut self, id: &str, title: &str, now: u64) -> Result<(), TicketError> {
        check_ticket_id(id)?;
        let title = Some(validate_title(title)?);
        self.entries.insert(
            id.to_owned(),
            TitleEntry {
                title,
                updated_at: now,
            },
        );
        Ok(())
    }

    /// Removes the title of ticket `id`, keeping a tombstone; returns whether it had a title.
    pub fn remove(&mut self, id: &str, now: u64) -> Result<bool, TicketError> {
        check_ticket_id(id)?;
        let had_title = self.get(id).is_some();
        if had_title {
            let entry = TitleEntry {
                title: None,
                updated_at: now,
            };
            self.entries.insert(id.to_owned(), entry);
        }
        Ok(had_title)
    }

    /// Writes the store atomically (temporary file, then rename), creating the directory if needed.
    pub fn save(&self) -> Result<(), TicketError> {
        let file = TitleFile {
            version: FORMAT_VERSION,
            tickets: self.entries.clone(),
        };
        let content = serde_json::to_string_pretty(&file).expect("titles are serializable");
        let parent = self.path.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(parent).map_err(|error| file_error(&self.path, error))?;
        let temporary = self.path.with_extension("json.tmp");
        fs::write(&temporary, content).map_err(|error| file_error(&self.path, error))?;
        fs::rename(&temporary, &self.path).map_err(|error| file_error(&self.path, error))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn validates_titles() {
        assert_eq!(
            validate_title("  Mover o Moodle  ").unwrap(),
            "Mover o Moodle"
        );
        assert!(validate_title("   ")
            .unwrap_err()
            .to_string()
            .contains("empty"));
        assert!(validate_title("a\nb")
            .unwrap_err()
            .to_string()
            .contains("single line"));
        assert!(validate_title("a\rb").is_err());
        assert!(validate_title(&"é".repeat(MAX_TITLE_CHARS)).is_ok());
        let too_long = validate_title(&"é".repeat(MAX_TITLE_CHARS + 1)).unwrap_err();
        assert!(too_long.to_string().contains("longer than 120"));
    }

    #[test]
    fn sets_gets_removes_and_persists_titles() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("dados").join("titles.json");
        let mut store = TitleStore::open(path.clone()).unwrap();
        assert_eq!(store.get("1"), None);
        assert!(!store.remove("1", 5).unwrap());

        store.set("1", "Primeiro", 10).unwrap();
        store.set("2", "Segundo", 11).unwrap();
        store.set("1", "Primeiro (revisado)", 12).unwrap();
        assert!(store.remove("2", 13).unwrap());
        store.save().unwrap();

        let reloaded = TitleStore::open(path.clone()).unwrap();
        assert_eq!(reloaded.get("1"), Some("Primeiro (revisado)"));
        assert_eq!(reloaded.get("2"), None);
        let saved = fs::read_to_string(&path).unwrap();
        assert!(saved.contains("\"updated_at\": 13") && saved.contains("\"version\": 1"));
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn rejects_bad_ids_titles_and_files() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("titles.json");
        let mut store = TitleStore::open(path.clone()).unwrap();
        assert!(store.set("x", "Título", 1).is_err());
        assert!(store.remove("../1", 1).is_err());
        assert!(store.set("1", "a\nb", 1).is_err());

        fs::write(&path, "não é json").unwrap();
        let corrupt = TitleStore::open(path.clone()).unwrap_err().to_string();
        assert!(corrupt.contains("titles file") && corrupt.contains("titles.json"));

        fs::write(&path, r#"{"version": 2, "tickets": {}}"#).unwrap();
        let version = TitleStore::open(path).unwrap_err().to_string();
        assert!(version.contains("unsupported format version 2"));

        let unreadable = TitleStore::open(directory.path().to_path_buf()).unwrap_err();
        assert!(unreadable.to_string().contains("titles file"));
    }

    #[test]
    fn save_reports_unwritable_locations() {
        let directory = tempdir().unwrap();
        let blocker = directory.path().join("arquivo");
        fs::write(&blocker, "x").unwrap();
        // The parent "directory" is a file, so it cannot be created.
        let store = TitleStore::open(blocker.join("titles.json")).unwrap();
        assert!(store.save().is_err());

        // The temporary file's name is taken by a directory, so writing it fails.
        let path = directory.path().join("ocupado.json");
        fs::create_dir(path.with_extension("json.tmp")).unwrap();
        assert!(TitleStore::open(path).unwrap().save().is_err());

        // The destination is a directory, so the final rename fails.
        let target = directory.path().join("destino");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("dentro"), "x").unwrap();
        let mut store = TitleStore::open(directory.path().join("titles.json")).unwrap();
        store.path = target;
        assert!(store.save().is_err());
    }
}
