//! Following tickets over time: what changed since the last look.
//!
//! A [`Snapshot`] remembers, per ticket, its situation and (optionally) which timeline entries
//! were already seen. [`poll`] reads the current state from SUAP and compares it with the previous
//! snapshot, producing [`WatchEvent`]s:
//!
//! - a ticket that **appeared** in the list ([`WatchEvent::New`]);
//! - a ticket whose **situation** changed ([`WatchEvent::Status`]);
//! - a ticket that **left** the list, usually because it was resolved or closed ([`WatchEvent::Left`]);
//! - with `messages` on, each **new timeline entry** of a ticket already known
//!   ([`WatchEvent::Message`]). This reads every ticket's page on each round, so it is opt-in.
//!
//! The first look at a list (no previous snapshot, or a snapshot of another list) is only a
//! baseline: it reports nothing, so starting to watch never floods the user with what already was.
//! Snapshots stay on this machine; they are not part of the synchronized data.

use std::{collections::BTreeMap, fs, path::Path};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{TicketError, TicketFilter, TicketSource, TimelineEntry};

const FORMAT_VERSION: u32 = 1;

/// What is remembered about one ticket.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketState {
    pub subject: Option<String>,
    pub status: Option<String>,
    /// Fingerprints of the timeline entries already seen (empty unless messages are watched).
    #[serde(default)]
    pub seen: Vec<String>,
}

/// The tickets of one list, as last seen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    version: u32,
    /// Which list this is (`mine:active`, `support:all`...): another list is a different baseline.
    pub scope: String,
    pub tickets: BTreeMap<String, TicketState>,
}

impl Snapshot {
    pub fn new(scope: &str) -> Self {
        Self {
            version: FORMAT_VERSION,
            scope: scope.to_owned(),
            tickets: BTreeMap::new(),
        }
    }

    /// The snapshot saved at `path`; `Ok(None)` if there is none yet. A damaged file is reported,
    /// not discarded: it is the user's state.
    pub fn load(path: &Path) -> Result<Option<Self>, TicketError> {
        if !path.exists() {
            return Ok(None);
        }
        let problem =
            |what: &str| TicketError::Source(format!("watch state {}: {what}", path.display()));
        let text = fs::read_to_string(path).map_err(|error| problem(&error.to_string()))?;
        let snapshot: Self =
            serde_json::from_str(&text).map_err(|error| problem(&error.to_string()))?;
        if snapshot.version != FORMAT_VERSION {
            return Err(problem(&format!(
                "unsupported version {} (this program reads version {FORMAT_VERSION})",
                snapshot.version
            )));
        }
        Ok(Some(snapshot))
    }

    /// Saves the snapshot at `path`, replacing the previous one.
    pub fn save(&self, path: &Path) -> Result<(), TicketError> {
        let problem =
            |error: String| TicketError::Source(format!("watch state {}: {error}", path.display()));
        if let Some(folder) = path.parent() {
            fs::create_dir_all(folder).map_err(|error| problem(error.to_string()))?;
        }
        // Plain strings and numbers only: serializing a snapshot cannot fail.
        let text = serde_json::to_string(self).expect("a snapshot always serializes");
        fs::write(path, text).map_err(|error| problem(error.to_string()))
    }
}

/// Something that changed between two looks at a list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    New {
        id: String,
        status: Option<String>,
        subject: Option<String>,
    },
    Status {
        id: String,
        from: Option<String>,
        to: Option<String>,
    },
    Left {
        id: String,
        status: Option<String>,
    },
    Message {
        id: String,
        date: String,
        text: String,
    },
}

impl WatchEvent {
    pub fn id(&self) -> &str {
        match self {
            Self::New { id, .. }
            | Self::Status { id, .. }
            | Self::Left { id, .. }
            | Self::Message { id, .. } => id,
        }
    }

    /// Stable name of the kind of event: `new`, `status`, `left` or `message`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::New { .. } => "new",
            Self::Status { .. } => "status",
            Self::Left { .. } => "left",
            Self::Message { .. } => "message",
        }
    }

    /// The event as a JSON object: `time` (seconds since the epoch), `event`, `id` (a number when
    /// it is one) and the fields of its kind.
    pub fn to_json(&self, time: u64) -> Value {
        let id = self
            .id()
            .parse::<u64>()
            .map_or_else(|_| json!(self.id()), |number| json!(number));
        let mut object = json!({"time": time, "event": self.kind(), "id": id});
        let details = match self {
            Self::New {
                status, subject, ..
            } => json!({"status": status, "subject": subject}),
            Self::Status { from, to, .. } => json!({"from": from, "to": to}),
            Self::Left { status, .. } => json!({"status": status}),
            Self::Message { date, text, .. } => json!({"date": date, "text": text}),
        };
        if let (Some(object), Some(details)) = (object.as_object_mut(), details.as_object()) {
            object.extend(details.clone());
        }
        object
    }
}

/// A short fingerprint (FNV-1a, 64 bits, in hexadecimal) of a timeline entry. It only has to tell
/// entries of one ticket apart, and it must not change between runs or versions.
fn fingerprint(entry: &TimelineEntry) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in entry.date.bytes().chain(*b"\n").chain(entry.text.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Reads the list `filter` asks for and compares it with `previous`.
///
/// Returns the new snapshot (to be saved) and the events, oldest first. `previous` is ignored,
/// making this a baseline, when it is `None` or was taken from another list (`scope`).
pub async fn poll<S: TicketSource>(
    source: &S,
    filter: &TicketFilter,
    scope: &str,
    messages: bool,
    previous: Option<&Snapshot>,
) -> Result<(Snapshot, Vec<WatchEvent>), TicketError> {
    let previous = previous.filter(|previous| previous.scope == scope);
    let mut next = Snapshot::new(scope);
    let mut events = Vec::new();
    for ticket in source.list_filtered(filter).await? {
        let known = previous.and_then(|previous| previous.tickets.get(&ticket.id));
        let mut state = TicketState {
            subject: ticket.subject.clone(),
            status: ticket.status.clone(),
            seen: Vec::new(),
        };
        if previous.is_some() {
            match known {
                None => events.push(WatchEvent::New {
                    id: ticket.id.clone(),
                    status: ticket.status.clone(),
                    subject: ticket.subject.clone(),
                }),
                Some(known) if known.status != ticket.status => events.push(WatchEvent::Status {
                    id: ticket.id.clone(),
                    from: known.status.clone(),
                    to: ticket.status.clone(),
                }),
                Some(_) => {}
            }
        }
        if messages {
            let details = source.get_ticket(&ticket.id).await?;
            state.seen = details.timeline.iter().map(fingerprint).collect();
            // Entries are listed newest first; report them oldest first. A ticket that was not
            // being watched for messages (no fingerprints yet) starts from what it has now.
            if let Some(watched) = known.filter(|known| !known.seen.is_empty()) {
                for entry in details.timeline.iter().rev() {
                    if !watched.seen.contains(&fingerprint(entry)) {
                        events.push(WatchEvent::Message {
                            id: ticket.id.clone(),
                            date: entry.date.clone(),
                            text: entry.text.clone(),
                        });
                    }
                }
            }
        } else if let Some(known) = known {
            state.seen.clone_from(&known.seen);
        }
        next.tickets.insert(ticket.id, state);
    }
    for (id, known) in previous.iter().flat_map(|previous| &previous.tickets) {
        if !next.tickets.contains_key(id) {
            events.push(WatchEvent::Left {
                id: id.clone(),
                status: known.status.clone(),
            });
        }
    }
    Ok((next, events))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn entry(date: &str, text: &str) -> TimelineEntry {
        TimelineEntry {
            date: date.to_owned(),
            text: text.to_owned(),
        }
    }

    fn state(status: &str, seen: &[&TimelineEntry]) -> TicketState {
        TicketState {
            subject: Some("Assunto".to_owned()),
            status: Some(status.to_owned()),
            seen: seen.iter().map(|entry| fingerprint(entry)).collect(),
        }
    }

    #[test]
    fn fingerprints_are_stable_and_tell_entries_apart() {
        let first = entry("09/10/2026 10:00", "texto");
        // Known value: it must never change, or saved state would look entirely new.
        assert_eq!(fingerprint(&entry("", "")), "af63c74c8601c8dd");
        assert_eq!(fingerprint(&first), fingerprint(&first.clone()));
        assert_ne!(
            fingerprint(&first),
            fingerprint(&entry("09/10/2026 10:00", "outro"))
        );
        assert_ne!(
            fingerprint(&first),
            fingerprint(&entry("09/10/2026 10:01", "texto"))
        );
        // The separator keeps "ab"+"c" and "a"+"bc" different.
        assert_ne!(
            fingerprint(&entry("ab", "c")),
            fingerprint(&entry("a", "bc"))
        );
    }

    #[test]
    fn snapshots_round_trip_and_damaged_files_are_reported() {
        let directory = tempdir().unwrap();
        let file = directory.path().join("pasta").join("watch.json");
        assert_eq!(Snapshot::load(&file).unwrap(), None);
        let mut snapshot = Snapshot::new("mine:active");
        snapshot
            .tickets
            .insert("5".to_owned(), state("Aberto", &[&entry("d", "t")]));
        snapshot.save(&file).unwrap();
        assert_eq!(Snapshot::load(&file).unwrap(), Some(snapshot.clone()));

        fs::write(&file, "isto nao e json").unwrap();
        let broken = Snapshot::load(&file).unwrap_err().to_string();
        assert!(
            broken.contains("watch state") && broken.contains("watch.json"),
            "{broken}"
        );
        fs::write(&file, r#"{"version": 9, "scope": "x", "tickets": {}}"#).unwrap();
        let future = Snapshot::load(&file).unwrap_err().to_string();
        assert!(future.contains("unsupported version 9"), "{future}");
        // A folder where the file should be cannot be read nor written.
        let blocked = directory.path().join("bloqueado");
        fs::create_dir(&blocked).unwrap();
        assert!(Snapshot::load(&blocked).is_err());
        assert!(snapshot.save(&blocked).is_err());
        // A path with no folder part (the root) is written to directly, and cannot be a file.
        assert!(snapshot.save(Path::new("/")).is_err());
        let under_a_file = file.join("filho.json");
        assert!(snapshot.save(&under_a_file).is_err());
        // Files saved by an older version may lack the fingerprints.
        fs::write(
            &file,
            r#"{"version": 1, "scope": "x", "tickets": {"1": {"subject": null, "status": null}}}"#,
        )
        .unwrap();
        let loaded = Snapshot::load(&file).unwrap().unwrap();
        assert!(loaded.tickets["1"].seen.is_empty());
    }

    #[test]
    fn events_have_a_kind_an_id_and_a_json_form() {
        let events = [
            WatchEvent::New {
                id: "5".to_owned(),
                status: Some("Aberto".to_owned()),
                subject: None,
            },
            WatchEvent::Status {
                id: "5".to_owned(),
                from: Some("Aberto".to_owned()),
                to: None,
            },
            WatchEvent::Left {
                id: "x7".to_owned(),
                status: None,
            },
            WatchEvent::Message {
                id: "5".to_owned(),
                date: "d".to_owned(),
                text: "oi\ntchau".to_owned(),
            },
        ];
        let kinds: Vec<&str> = events.iter().map(WatchEvent::kind).collect();
        assert_eq!(kinds, ["new", "status", "left", "message"]);
        assert_eq!(events[2].id(), "x7");
        assert_eq!(
            events[0].to_json(100),
            json!({"time": 100, "event": "new", "id": 5, "status": "Aberto", "subject": null})
        );
        assert_eq!(
            events[1].to_json(1),
            json!({"time": 1, "event": "status", "id": 5, "from": "Aberto", "to": null})
        );
        assert_eq!(
            events[2].to_json(1),
            json!({"time": 1, "event": "left", "id": "x7", "status": null})
        );
        assert_eq!(
            events[3].to_json(1),
            json!({"time": 1, "event": "message", "id": 5, "date": "d", "text": "oi\ntchau"})
        );
    }
}
