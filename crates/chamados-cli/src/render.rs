//! The `--json` forms of what the reading commands print, and the lines of `chamados watch`.
//!
//! The JSON is meant for scripts, so its shape is a promise: keys are English and stable, text is
//! UTF-8 as SUAP wrote it, ids are numbers, and a value that is not there is `null`.
//! Adding keys is allowed; renaming or removing them is not.

use chamados_core::{watch::WatchEvent, RemoteTicket, TicketDetails};
use serde_json::{json, Value};
use suap_core::{AppPaths, SuapConfig};

/// A ticket id as a JSON number (SUAP's ids are), or the text itself when it is not one.
pub fn id_json(id: &str) -> Value {
    id.parse::<u64>()
        .map_or_else(|_| json!(id), |number| json!(number))
}

/// `chamados list --json`: an array with one object per ticket.
pub fn tickets_json(tickets: &[RemoteTicket], title_of: &dyn Fn(&str) -> Option<String>) -> Value {
    let items: Vec<Value> = tickets
        .iter()
        .map(|ticket| {
            json!({
                "id": id_json(&ticket.id),
                "status": ticket.status,
                "title": title_of(&ticket.id),
                "subject": ticket.subject,
                "url": ticket.details_url,
            })
        })
        .collect();
    Value::Array(items)
}

/// `chamados show --json`.
pub fn details_json(details: &TicketDetails, local_title: Option<&str>) -> Value {
    let fields: Vec<Value> = details
        .fields
        .iter()
        .map(|(label, value)| json!({"label": label, "value": value}))
        .collect();
    let attachments: Vec<Value> = details
        .attachments
        .iter()
        .enumerate()
        .map(|(index, attachment)| {
            json!({"number": index + 1, "name": attachment.name, "path": attachment.path})
        })
        .collect();
    let timeline: Vec<Value> = details
        .timeline
        .iter()
        .map(|entry| json!({"date": entry.date, "text": entry.text}))
        .collect();
    json!({
        "id": id_json(&details.id),
        "title": details.title,
        "local_title": local_title,
        "statuses": details.statuses,
        "service": details.heading,
        "url": details.details_url,
        "fields": fields,
        "attachments": attachments,
        "timeline": timeline,
    })
}

/// `chamados profile show --json`.
pub fn profile_json(paths: &AppPaths, config: &SuapConfig) -> Value {
    let open = &config.open;
    json!({
        "profile": paths.profile(),
        "base_url": config.base_url.as_str(),
        "username": config.username,
        "open": {
            "service": open.service,
            "interested": open.interested,
            "campus": open.campus,
            "center": open.center,
        },
        "sync": config.sync,
        "session_saved": paths.session_file().exists(),
        "file": paths.config_file().display().to_string(),
    })
}

/// `chamados profile list --json`.
pub fn profiles_json(profiles: &[(String, SuapConfig)], default_name: &str) -> Value {
    let items: Vec<Value> = profiles
        .iter()
        .map(|(name, config)| {
            json!({
                "name": name,
                "default": name == default_name,
                "base_url": config.base_url.as_str(),
                "username": config.username,
                "sync": config.sync,
            })
        })
        .collect();
    Value::Array(items)
}

/// `chamados sync --check --json`.
pub fn sync_check_json(backend: &str, probed: bool, configured: bool, key_source: &str) -> Value {
    json!({
        "backend": backend,
        "conditional_writes": probed,
        "matches_configuration": probed == configured,
        "key_source": key_source,
        "key_present": true,
    })
}

/// `chamados sync --json`.
pub fn sync_report_json(
    profiles: usize,
    local_changes: usize,
    cloud_updated: bool,
    dry_run: bool,
) -> Value {
    json!({
        "profiles": profiles,
        "local_changes": local_changes,
        "cloud_updated": cloud_updated,
        "dry_run": dry_run,
    })
}

/// `seconds` since the epoch as `YYYY-MM-DDTHH:MM:SSZ` (UTC).
pub fn utc_timestamp(seconds: u64) -> String {
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let shifted = days + 719_468;
    let era = shifted / 146_097;
    let day_of_era = shifted % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_part + 2) / 5 + 1;
    let month = if month_part < 10 {
        month_part + 3
    } else {
        month_part - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    let (hour, minute, second) = (rest / 3_600, rest % 3_600 / 60, rest % 60);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// The line `chamados watch` prints for an event; a text with several lines continues indented.
pub fn watch_line(event: &WatchEvent, time: u64, local_title: Option<&str>) -> String {
    let dash = |value: &Option<String>| value.clone().unwrap_or_else(|| "-".to_owned());
    let title = local_title.map_or_else(String::new, |title| format!(" ({title})"));
    let what = match event {
        WatchEvent::New {
            status, subject, ..
        } => {
            format!("novo: {} - {}", dash(status), dash(subject))
        }
        WatchEvent::Status { from, to, .. } => {
            format!("situação: {} -> {}", dash(from), dash(to))
        }
        WatchEvent::Left { status, .. } => {
            format!("saiu da lista (última situação: {})", dash(status))
        }
        WatchEvent::Message { date, text, .. } => {
            // Continuation lines are indented; blank ones stay empty.
            let lines: Vec<String> = text
                .split('\n')
                .enumerate()
                .map(|(index, line)| match (index, line.is_empty()) {
                    (0, _) | (_, true) => line.to_owned(),
                    _ => format!("    {line}"),
                })
                .collect();
            let text = lines.join("\n");
            format!("nova mensagem em {date}: {text}")
        }
    };
    format!("{}  #{}{title}  {what}", utc_timestamp(time), event.id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_utc_dates() {
        for (seconds, expected) in [
            (0, "1970-01-01T00:00:00Z"),
            (86_399, "1970-01-01T23:59:59Z"),
            (951_782_400, "2000-02-29T00:00:00Z"),
            (1_791_558_245, "2026-10-09T15:04:05Z"),
            (4_102_444_799, "2099-12-31T23:59:59Z"),
            (1_709_251_199, "2024-02-29T23:59:59Z"),
            (1_709_251_200, "2024-03-01T00:00:00Z"),
        ] {
            assert_eq!(utc_timestamp(seconds), expected, "{seconds}");
        }
    }

    #[test]
    fn ids_are_numbers_when_they_are_numbers() {
        assert_eq!(id_json("559298"), json!(559_298));
        assert_eq!(id_json("x7"), json!("x7"));
    }

    #[test]
    fn watch_lines_name_the_ticket_and_what_happened() {
        let event = |event: WatchEvent| watch_line(&event, 0, None);
        let id = || "5".to_owned();
        assert_eq!(
            event(WatchEvent::New {
                id: id(),
                status: Some("Aberto".to_owned()),
                subject: None
            }),
            "1970-01-01T00:00:00Z  #5  novo: Aberto - -"
        );
        assert_eq!(
            event(WatchEvent::Status {
                id: id(),
                from: None,
                to: Some("Resolvido".to_owned())
            }),
            "1970-01-01T00:00:00Z  #5  situação: - -> Resolvido"
        );
        assert_eq!(
            event(WatchEvent::Left {
                id: id(),
                status: Some("Fechado".to_owned())
            }),
            "1970-01-01T00:00:00Z  #5  saiu da lista (última situação: Fechado)"
        );
        let message = WatchEvent::Message {
            id: id(),
            date: "09/10 10:00".to_owned(),
            text: "a\n\nb".to_owned(),
        };
        assert_eq!(
            watch_line(&message, 60, Some("Meu título")),
            "1970-01-01T00:01:00Z  #5 (Meu título)  nova mensagem em 09/10 10:00: a\n\n    b"
        );
    }
}
