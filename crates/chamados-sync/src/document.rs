//! The syncable document of each profile, and its entry-by-entry merge.
//!
//! One [`ProfileDoc`] per profile that has `sync = true`: the profile settings (one entry) and the
//! local ticket titles (one entry per ticket). Every entry carries the time of its last change; merging
//! keeps, per entry, the newest one (ties are broken by comparing the content, so the result does not
//! depend on the order of the arguments). Removed titles stay as entries without a title (tombstones).
//!
//! Sessions, passwords and cloud credentials are not part of the document: the types here simply have
//! nowhere to put them.

use std::collections::BTreeMap;

use chamados_core::{TitleEntry, TitleStore};
use serde::{Deserialize, Serialize};
use suap_core::{list_profiles, load_config, save_config, AppPaths, OpenDefaults, SuapConfig};
use url::Url;

use crate::SyncError;

const FORMAT_VERSION: u32 = 1;

/// A profile's settings, as synchronized (everything but the `sync` flag itself).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsEntry {
    pub base_url: String,
    pub username: Option<String>,
    pub open: OpenDefaults,
    pub updated_at: u64,
}

impl SettingsEntry {
    pub fn from_config(config: &SuapConfig) -> Self {
        Self {
            base_url: config.base_url.to_string(),
            username: config.username.clone(),
            open: config.open.clone(),
            updated_at: config.updated_at,
        }
    }

    /// A configuration for a profile that takes part in the synchronization.
    pub fn to_config(&self) -> Result<SuapConfig, SyncError> {
        let base_url = Url::parse(&self.base_url).map_err(suap_core::SuapError::from)?;
        Ok(SuapConfig {
            base_url,
            username: self.username.clone(),
            open: self.open.clone(),
            sync: true,
            updated_at: self.updated_at,
        })
    }

    /// Ordering used to pick the winner between two copies of the settings.
    fn rank(&self) -> (u64, String) {
        let content = serde_json::to_string(self).unwrap_or_default();
        (self.updated_at, content)
    }
}

/// Settings and titles of one profile.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileDoc {
    pub settings: Option<SettingsEntry>,
    pub titles: BTreeMap<String, TitleEntry>,
}

/// Everything that is synchronized.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncDocument {
    pub version: u32,
    pub profiles: BTreeMap<String, ProfileDoc>,
}

impl Default for SyncDocument {
    fn default() -> Self {
        Self {
            version: FORMAT_VERSION,
            profiles: BTreeMap::new(),
        }
    }
}

impl SyncDocument {
    /// The document as JSON bytes (to be encrypted).
    pub fn encode(&self) -> Result<Vec<u8>, SyncError> {
        Ok(serde_json::to_vec(self)?)
    }

    /// Parses JSON bytes; a document from a different format version is refused.
    pub fn decode(bytes: &[u8]) -> Result<Self, SyncError> {
        let document: Self = serde_json::from_slice(bytes)?;
        if document.version != FORMAT_VERSION {
            return Err(SyncError::Backend(format!(
                "unsupported document version {} (this program reads version {FORMAT_VERSION})",
                document.version
            )));
        }
        Ok(document)
    }
}

/// Merges two documents: per profile, the newest settings and, per ticket, the newest title entry.
///
/// Commutative, associative and idempotent, so devices converge whatever the order they sync in.
pub fn merge(left: &SyncDocument, right: &SyncDocument) -> SyncDocument {
    let mut profiles = left.profiles.clone();
    for (name, incoming) in &right.profiles {
        let merged = match profiles.remove(name) {
            Some(existing) => merge_profiles(&existing, incoming),
            None => incoming.clone(),
        };
        profiles.insert(name.clone(), merged);
    }
    SyncDocument {
        version: FORMAT_VERSION,
        profiles,
    }
}

fn merge_profiles(left: &ProfileDoc, right: &ProfileDoc) -> ProfileDoc {
    let settings = match (&left.settings, &right.settings) {
        (Some(a), Some(b)) => Some(if b.rank() > a.rank() { b } else { a }.clone()),
        (Some(only), None) | (None, Some(only)) => Some(only.clone()),
        (None, None) => None,
    };
    let mut titles = left.titles.clone();
    for (id, incoming) in &right.titles {
        match titles.get(id) {
            Some(existing) if !incoming.newer_than(existing) => {}
            _ => {
                titles.insert(id.clone(), incoming.clone());
            }
        }
    }
    ProfileDoc { settings, titles }
}

/// The local state of every profile with `sync = true` (only `only`, when given).
pub fn collect_local(paths: &AppPaths, only: Option<&str>) -> Result<SyncDocument, SyncError> {
    let mut document = SyncDocument::default();
    for (name, config) in list_profiles(paths)? {
        if !config.sync || only.is_some_and(|only| only != name) {
            continue;
        }
        let profile_paths = paths.clone().with_profile(&name)?;
        let titles = TitleStore::open(profile_paths.titles_file())?;
        document.profiles.insert(
            name,
            ProfileDoc {
                settings: Some(SettingsEntry::from_config(&config)),
                titles: titles.entries().clone(),
            },
        );
    }
    Ok(document)
}

/// What applying a merged document changed locally.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ApplyReport {
    /// Profiles created on this machine.
    pub profiles_created: usize,
    /// Profiles whose settings were replaced by a newer copy.
    pub settings_updated: usize,
    /// Title entries added or replaced.
    pub titles_updated: usize,
}

impl ApplyReport {
    pub fn total(&self) -> usize {
        self.profiles_created + self.settings_updated + self.titles_updated
    }
}

/// Applies `merged` to the local profiles (only `only`, when given); with `write` off, only counts.
///
/// A local profile with `sync = false` is left alone. Profile names come from the remote copy, so
/// they are validated before they become file names.
pub fn apply_merged(
    paths: &AppPaths,
    merged: &SyncDocument,
    only: Option<&str>,
    write: bool,
) -> Result<ApplyReport, SyncError> {
    let mut report = ApplyReport::default();
    for (name, remote) in &merged.profiles {
        if only.is_some_and(|only| only != name.as_str()) {
            continue;
        }
        let profile_paths = paths.clone().with_profile(name)?;
        let local = load_config(&profile_paths)?;
        if local.as_ref().is_some_and(|local| !local.sync) {
            continue;
        }
        if let Some(settings) = &remote.settings {
            let newer = local
                .as_ref()
                .is_none_or(|local| settings.rank() > SettingsEntry::from_config(local).rank());
            if newer {
                match local {
                    Some(_) => report.settings_updated += 1,
                    None => report.profiles_created += 1,
                }
                if write {
                    save_config(&profile_paths, &settings.to_config()?)?;
                }
            }
        }
        let mut titles = TitleStore::open(profile_paths.titles_file())?;
        let mut changed = 0;
        for (id, entry) in &remote.titles {
            changed += usize::from(titles.merge_entry(id, entry)?);
        }
        report.titles_updated += changed;
        if write && changed > 0 {
            titles.save()?;
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::{tempdir, TempDir};

    fn entry(title: Option<&str>, updated_at: u64) -> TitleEntry {
        TitleEntry {
            title: title.map(str::to_owned),
            updated_at,
        }
    }

    fn settings(username: &str, updated_at: u64) -> SettingsEntry {
        SettingsEntry {
            base_url: "https://suap.example/".to_owned(),
            username: Some(username.to_owned()),
            open: OpenDefaults::default(),
            updated_at,
        }
    }

    fn profile(settings: Option<SettingsEntry>, titles: &[(&str, TitleEntry)]) -> ProfileDoc {
        ProfileDoc {
            settings,
            titles: titles
                .iter()
                .map(|(id, e)| ((*id).to_owned(), e.clone()))
                .collect(),
        }
    }

    fn document(profiles: &[(&str, ProfileDoc)]) -> SyncDocument {
        SyncDocument {
            version: FORMAT_VERSION,
            profiles: profiles
                .iter()
                .map(|(n, p)| ((*n).to_owned(), p.clone()))
                .collect(),
        }
    }

    #[test]
    fn merge_keeps_the_newest_entry_and_is_commutative_and_idempotent() {
        let a = document(&[(
            "default",
            profile(
                Some(settings("a", 10)),
                &[
                    ("1", entry(Some("velho"), 5)),
                    ("2", entry(Some("so em a"), 1)),
                ],
            ),
        )]);
        let b = document(&[
            (
                "default",
                profile(
                    Some(settings("b", 20)),
                    &[("1", entry(Some("novo"), 9)), ("3", entry(None, 4))],
                ),
            ),
            ("local", profile(None, &[])),
        ]);
        let merged = merge(&a, &b);
        assert_eq!(merged, merge(&b, &a));
        assert_eq!(merge(&merged, &merged), merged);
        assert_eq!(merge(&merge(&a, &b), &b), merged);

        let default = &merged.profiles["default"];
        assert_eq!(
            default.settings.as_ref().unwrap().username.as_deref(),
            Some("b")
        );
        assert_eq!(default.titles["1"].title.as_deref(), Some("novo"));
        assert_eq!(default.titles["2"].title.as_deref(), Some("so em a"));
        assert_eq!(default.titles["3"].title, None);
        assert!(merged.profiles.contains_key("local"));
    }

    #[test]
    fn profiles_with_settings_on_one_side_only_keep_them() {
        let with = document(&[("p", profile(Some(settings("so aqui", 3)), &[]))]);
        let without = document(&[("p", profile(None, &[("1", entry(Some("t"), 1))]))]);
        let neither = document(&[("p", profile(None, &[]))]);
        for merged in [merge(&with, &without), merge(&without, &with)] {
            assert_eq!(
                merged.profiles["p"]
                    .settings
                    .as_ref()
                    .unwrap()
                    .username
                    .as_deref(),
                Some("so aqui")
            );
            assert_eq!(merged.profiles["p"].titles.len(), 1);
        }
        assert_eq!(merge(&neither, &neither).profiles["p"].settings, None);
    }

    #[test]
    fn newer_removal_beats_older_edit_and_the_reverse() {
        let edit_then_remove = merge(
            &document(&[("p", profile(None, &[("1", entry(Some("texto"), 5))]))]),
            &document(&[("p", profile(None, &[("1", entry(None, 6))]))]),
        );
        assert_eq!(edit_then_remove.profiles["p"].titles["1"], entry(None, 6));
        let remove_then_edit = merge(
            &document(&[("p", profile(None, &[("1", entry(None, 6))]))]),
            &document(&[("p", profile(None, &[("1", entry(Some("texto"), 7))]))]),
        );
        assert_eq!(
            remove_then_edit.profiles["p"].titles["1"],
            entry(Some("texto"), 7)
        );
    }

    #[test]
    fn ties_are_broken_by_content_in_both_directions() {
        let a = document(&[(
            "p",
            profile(Some(settings("ana", 5)), &[("1", entry(Some("a"), 5))]),
        )]);
        let b = document(&[(
            "p",
            profile(Some(settings("zeca", 5)), &[("1", entry(Some("b"), 5))]),
        )]);
        let merged = merge(&a, &b);
        assert_eq!(merged, merge(&b, &a));
        assert_eq!(
            merged.profiles["p"]
                .settings
                .as_ref()
                .unwrap()
                .username
                .as_deref(),
            Some("zeca")
        );
        assert_eq!(merged.profiles["p"].titles["1"].title.as_deref(), Some("b"));
        // At the same time, a title beats a removal.
        let value = document(&[("p", profile(None, &[("1", entry(Some("x"), 5))]))]);
        let removal = document(&[("p", profile(None, &[("1", entry(None, 5))]))]);
        assert_eq!(merge(&value, &removal), merge(&removal, &value));
        assert_eq!(
            merge(&value, &removal).profiles["p"].titles["1"]
                .title
                .as_deref(),
            Some("x")
        );
    }

    #[test]
    fn documents_round_trip_and_reject_other_versions() {
        let original = document(&[(
            "p",
            profile(Some(settings("u", 1)), &[("7", entry(Some("t"), 2))]),
        )]);
        assert_eq!(
            SyncDocument::decode(&original.encode().unwrap()).unwrap(),
            original
        );
        assert_eq!(SyncDocument::default().profiles.len(), 0);
        let future = br#"{"version": 9, "profiles": {}}"#;
        assert!(SyncDocument::decode(future)
            .unwrap_err()
            .to_string()
            .contains("unsupported document version 9"));
        assert!(SyncDocument::decode(b"nao e json").is_err());
    }

    fn paths() -> (TempDir, AppPaths) {
        let directory = tempdir().unwrap();
        let paths = AppPaths::from_dirs(
            directory.path().join("config"),
            directory.path().join("data"),
        );
        (directory, paths)
    }

    fn profile_paths(paths: &AppPaths, name: &str) -> AppPaths {
        paths.clone().with_profile(name).unwrap()
    }

    fn save(paths: &AppPaths, name: &str, sync: bool, updated_at: u64) -> AppPaths {
        let profile = profile_paths(paths, name);
        let config = SuapConfig {
            sync,
            updated_at,
            ..SuapConfig::default()
        };
        save_config(&profile, &config).unwrap();
        profile
    }

    #[test]
    fn collects_only_profiles_that_sync_and_never_sessions() {
        let (_dir, paths) = paths();
        let default = save(&paths, "default", true, 3);
        save(&paths, "privado", false, 3);
        let mut titles = TitleStore::open(default.titles_file()).unwrap();
        titles.set("5", "Meu titulo", 8).unwrap();
        titles.save().unwrap();
        std::fs::write(default.session_file(), "COOKIE-SECRETO").unwrap();

        let document = collect_local(&paths, None).unwrap();
        assert_eq!(document.profiles.keys().collect::<Vec<_>>(), ["default"]);
        assert_eq!(
            document.profiles["default"].titles["5"].title.as_deref(),
            Some("Meu titulo")
        );
        let json = String::from_utf8(document.encode().unwrap()).unwrap();
        assert!(!json.contains("COOKIE") && !json.contains("privado") && !json.contains("session"));

        save(&paths, "local", true, 1);
        assert_eq!(collect_local(&paths, None).unwrap().profiles.len(), 2);
        let only = collect_local(&paths, Some("local")).unwrap();
        assert_eq!(only.profiles.keys().collect::<Vec<_>>(), ["local"]);
    }

    #[test]
    fn applies_remote_settings_titles_and_new_profiles() {
        let (_dir, paths) = paths();
        let default = save(&paths, "default", true, 5);
        let private = save(&paths, "privado", false, 5);
        let remote = document(&[
            (
                "default",
                profile(
                    Some(settings("remoto", 9)),
                    &[("1", entry(Some("da nuvem"), 2))],
                ),
            ),
            (
                "novo",
                profile(Some(settings("n", 1)), &[("2", entry(Some("tit"), 1))]),
            ),
            (
                "privado",
                profile(Some(settings("intruso", 99)), &[("3", entry(Some("x"), 1))]),
            ),
        ]);

        let dry = apply_merged(&paths, &remote, None, false).unwrap();
        assert_eq!(
            dry,
            ApplyReport {
                profiles_created: 1,
                settings_updated: 1,
                titles_updated: 2
            }
        );
        assert_eq!(dry.total(), 4);
        assert!(load_config(&profile_paths(&paths, "novo"))
            .unwrap()
            .is_none());

        let report = apply_merged(&paths, &remote, None, true).unwrap();
        assert_eq!(report, dry);
        assert_eq!(
            load_config(&default).unwrap().unwrap().username.as_deref(),
            Some("remoto")
        );
        let created = load_config(&profile_paths(&paths, "novo"))
            .unwrap()
            .unwrap();
        assert!(created.sync && created.updated_at == 1);
        assert_eq!(
            TitleStore::open(default.titles_file()).unwrap().get("1"),
            Some("da nuvem")
        );
        // A profile the user keeps private is never touched.
        assert_eq!(load_config(&private).unwrap().unwrap().username, None);
        assert_eq!(
            TitleStore::open(private.titles_file()).unwrap().get("3"),
            None
        );

        // Applying again changes nothing; an older remote copy does not win.
        assert_eq!(
            apply_merged(&paths, &remote, None, true).unwrap().total(),
            0
        );
        let older = document(&[("default", profile(Some(settings("antigo", 1)), &[]))]);
        assert_eq!(apply_merged(&paths, &older, None, true).unwrap().total(), 0);

        // `only` restricts the profiles touched.
        let more = document(&[("outro", profile(Some(settings("o", 1)), &[]))]);
        assert_eq!(
            apply_merged(&paths, &more, Some("default"), true)
                .unwrap()
                .total(),
            0
        );
        assert_eq!(
            apply_merged(&paths, &more, Some("outro"), true)
                .unwrap()
                .profiles_created,
            1
        );
    }

    #[test]
    fn remote_data_is_validated_before_it_touches_the_disk() {
        let (_dir, paths) = paths();
        let evil_name = document(&[("../fora", profile(Some(settings("x", 1)), &[]))]);
        assert!(apply_merged(&paths, &evil_name, None, true).is_err());
        let evil_id = document(&[("p", profile(None, &[("../1", entry(Some("x"), 1))]))]);
        assert!(apply_merged(&paths, &evil_id, None, true).is_err());
        let evil_url = SettingsEntry {
            base_url: "não é url".to_owned(),
            ..settings("x", 1)
        };
        let broken = document(&[("p", profile(Some(evil_url), &[]))]);
        assert!(apply_merged(&paths, &broken, None, true).is_err());
    }
}
