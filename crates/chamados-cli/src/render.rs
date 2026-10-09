//! The `--json` forms of what the reading commands print, and the lines of `chamados watch`.
//!
//! The JSON is meant for scripts, so its shape is a promise: keys are English and stable, text is
//! UTF-8 as SUAP wrote it, ids are numbers, and a value that is not there is `null`.
//! Adding keys is allowed; renaming or removing them is not.

use chamados_core::{watch::WatchEvent, RemoteTicket, TicketDetails};
use clap::ValueEnum;
use serde_json::{json, Value};
use suap_core::{AppPaths, SuapConfig};

/// When the tables of `list` and `profile list` are aligned, colored and linked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ColorMode {
    /// On a terminal that understands ANSI, unless `NO_COLOR` is set; plain tab-separated text otherwise.
    Auto,
    /// Always aligned, colored and linked, even in a pipe.
    Always,
    /// Never: plain tab-separated text, the format for scripts.
    Never,
}

/// How a table is printed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    /// Columns padded to the widest value, separated by spaces (instead of tabs).
    pub aligned: bool,
    /// ANSI colors and hyperlinks.
    pub color: bool,
}

impl Style {
    /// Tab-separated text without escape sequences: what scripts get.
    pub const PLAIN: Self = Self {
        aligned: false,
        color: false,
    };
}

/// The style for `mode`, given whether standard output is a terminal that understands ANSI
/// escape sequences and whether `NO_COLOR` asks for no colors (which only matters in `auto`).
pub fn resolve_style(mode: ColorMode, ansi_terminal: bool, no_color: bool) -> Style {
    match (mode, ansi_terminal) {
        (ColorMode::Never, _) | (ColorMode::Auto, false) => Style::PLAIN,
        (ColorMode::Always, _) => Style {
            aligned: true,
            color: true,
        },
        (ColorMode::Auto, true) => Style {
            aligned: true,
            color: !no_color,
        },
    }
}

/// One cell of a table: its text and how it may be dressed up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    text: String,
    color: Option<&'static str>,
    link: Option<String>,
}

impl Cell {
    pub fn plain(text: &str) -> Self {
        // Text comes from SUAP or the user: a control character (even an escape sequence) must
        // never reach the terminal, and a tab would break the columns.
        let text = text
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        Self {
            text,
            color: None,
            link: None,
        }
    }

    /// `text` in the ANSI color `code` (the number of an SGR sequence, such as `"32"`).
    pub fn colored(text: &str, code: &'static str) -> Self {
        Self {
            color: Some(code),
            ..Self::plain(text)
        }
    }

    /// The same cell, a hyperlink to `url` where hyperlinks are used.
    pub fn linked(self, url: &str) -> Self {
        let link = Some(url.chars().filter(|c| !c.is_control()).collect());
        Self { link, ..self }
    }

    /// Characters shown (accents composed of a separate mark do not take a column of their own).
    fn width(&self) -> usize {
        self.text
            .chars()
            .filter(|c| !('\u{300}'..='\u{36f}').contains(c))
            .count()
    }

    fn dressed(&self, style: Style) -> String {
        if !style.color {
            return self.text.clone();
        }
        let mut shown = self.text.clone();
        if let Some(code) = self.color {
            shown = format!("\x1b[{code}m{shown}\x1b[0m");
        }
        if let Some(url) = &self.link {
            shown = format!("\x1b]8;;{url}\x1b\\{shown}\x1b]8;;\x1b\\");
        }
        shown
    }
}

/// The lines of a table. Plain style joins the cells with tabs; the aligned style pads every
/// column but the last to the width of its widest cell (by characters shown, not bytes) and
/// separates columns with two spaces.
pub fn render_table(rows: &[Vec<Cell>], style: Style) -> Vec<String> {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..columns)
        .map(|column| {
            let cells = rows.iter().filter_map(|row| row.get(column));
            cells.map(Cell::width).max().unwrap_or(0)
        })
        .collect();
    rows.iter()
        .map(|row| {
            let mut line = String::new();
            for (column, cell) in row.iter().enumerate() {
                if column > 0 {
                    line.push_str(if style.aligned { "  " } else { "\t" });
                }
                line.push_str(&cell.dressed(style));
                let last = column + 1 == row.len();
                if style.aligned && !last {
                    line.push_str(&" ".repeat(widths[column] - cell.width()));
                }
            }
            line
        })
        .collect()
}

/// The color of a ticket situation, by name.
pub fn status_color(status: &str) -> &'static str {
    match status {
        "Aberto" => "32",
        "Em atendimento" => "34",
        "Suspenso" => "33",
        "Resolvido" => "92",
        "Fechado" => "90",
        "Cancelado" => "31",
        "Reaberto" => "91",
        _ => "0",
    }
}

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

    const ALIGNED: Style = Style {
        aligned: true,
        color: false,
    };
    const COLORED: Style = Style {
        aligned: true,
        color: true,
    };

    fn row(cells: &[&str]) -> Vec<Cell> {
        cells.iter().map(|text| Cell::plain(text)).collect()
    }

    #[test]
    fn the_style_follows_the_mode_the_terminal_and_no_color() {
        use ColorMode::{Always, Auto, Never};
        assert_eq!(resolve_style(Auto, false, false), Style::PLAIN);
        assert_eq!(resolve_style(Auto, false, true), Style::PLAIN);
        assert_eq!(resolve_style(Auto, true, false), COLORED);
        // NO_COLOR keeps the alignment but drops colors and links.
        assert_eq!(resolve_style(Auto, true, true), ALIGNED);
        assert_eq!(resolve_style(Always, false, false), COLORED);
        assert_eq!(
            resolve_style(Always, true, true),
            COLORED,
            "a flag beats NO_COLOR"
        );
        assert_eq!(resolve_style(Never, true, false), Style::PLAIN);
        assert_eq!(resolve_style(Never, false, true), Style::PLAIN);
    }

    #[test]
    fn plain_tables_are_tab_separated_and_aligned_ones_are_padded_by_characters() {
        let rows = vec![
            row(&["#7", "Em atendimento", "Título", "Assunto A"]),
            row(&["#12345", "Aberto", "-", "Assunto com acentuação"]),
            row(&["#9", "Suspenso"]),
        ];
        assert_eq!(
            render_table(&rows, Style::PLAIN),
            [
                "#7\tEm atendimento\tTítulo\tAssunto A",
                "#12345\tAberto\t-\tAssunto com acentuação",
                "#9\tSuspenso",
            ]
        );
        assert_eq!(
            render_table(&rows, ALIGNED),
            [
                "#7      Em atendimento  Título  Assunto A",
                "#12345  Aberto          -       Assunto com acentuação",
                "#9      Suspenso",
            ]
        );
        assert!(render_table(&[], ALIGNED).is_empty());
        // Accents count as one column, composed or not; a separate mark takes none.
        let accents = vec![
            row(&["ação", "x"]),
            row(&["a\u{303}cao", "y"]),
            row(&["acao", "z"]),
        ];
        let lines = render_table(&accents, ALIGNED);
        assert!(lines
            .iter()
            .all(|line| line.chars().filter(|c| *c != '\u{303}').count() == 7));
    }

    #[test]
    fn colors_and_links_wrap_the_text_without_changing_the_widths() {
        let rows = vec![
            vec![
                Cell::colored("#7", "36").linked("https://suap.example/chamado/7/"),
                Cell::colored("Aberto", "32"),
                Cell::plain("fim"),
            ],
            vec![
                Cell::plain("#123"),
                Cell::plain("Em atendimento"),
                Cell::plain("fim"),
            ],
        ];
        let lines = render_table(&rows, COLORED);
        assert_eq!(
            lines[0],
            "\x1b]8;;https://suap.example/chamado/7/\x1b\\\x1b[36m#7\x1b[0m\x1b]8;;\x1b\\    \
             \x1b[32mAberto\x1b[0m          fim"
        );
        assert_eq!(lines[1], "#123  Em atendimento  fim");
        // Without colors the same cells print as plain text.
        assert_eq!(render_table(&rows, ALIGNED)[0], "#7    Aberto          fim");
        assert_eq!(render_table(&rows, Style::PLAIN)[0], "#7\tAberto\tfim");
    }

    #[test]
    fn control_characters_from_suap_never_reach_the_terminal() {
        let hostile = Cell::plain("ok\x1b[2J\tx\ny\u{7}");
        assert_eq!(hostile.text, "ok [2J x y ");
        let link = Cell::plain("a").linked("https://x/\x1b]0;título\x07");
        let rows = vec![vec![link, hostile]];
        let line = &render_table(&rows, COLORED)[0];
        assert!(
            !line.contains("\x07") && !line.contains("\x1b[2J"),
            "{line:?}"
        );
    }

    #[test]
    fn each_situation_has_its_color() {
        for (status, code) in [
            ("Aberto", "32"),
            ("Em atendimento", "34"),
            ("Suspenso", "33"),
            ("Resolvido", "92"),
            ("Fechado", "90"),
            ("Cancelado", "31"),
            ("Reaberto", "91"),
            ("Outra", "0"),
        ] {
            assert_eq!(status_color(status), code, "{status}");
        }
    }

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
