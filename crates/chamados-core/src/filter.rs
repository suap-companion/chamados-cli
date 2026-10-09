//! Search options of the ticket listings, and the query strings SUAP expects for them.
//!
//! SUAP does the filtering: the support queue (`listar_chamados_suporte`) and "Meus chamados"
//! (`meus_chamados`) read their form from the query string. The options are kept by their readable
//! names (`aberto`, `mim`...) and translated to SUAP's codes only when the request is built, so an
//! unknown name is reported instead of being sent.

use url::form_urlencoded::Serializer;

use crate::{TicketError, TicketQueue};

/// Ticket situations: readable name and SUAP code.
pub const STATUSES: [(&str, &str); 7] = [
    ("aberto", "1"),
    ("atendimento", "2"),
    ("resolvido", "3"),
    ("fechado", "4"),
    ("reaberto", "5"),
    ("suspenso", "6"),
    ("cancelado", "7"),
];

/// Who a ticket is assigned to, as the support queue filters it.
pub const ASSIGNMENTS: [(&str, &str); 5] = [
    ("mim", "1"),
    ("outros", "2"),
    ("ninguem", "3"),
    ("mim-ou-ninguem", "4"),
    ("alguem", "5"),
];

/// Sort keys of the support queue.
pub const ORDERS: [(&str, &str); 7] = [
    ("limite", "data_limite_atendimento"),
    ("interacao", "data_ultima_interacao"),
    ("abertura", "aberto_em"),
    ("situacao", "status"),
    ("servico", "servico__nome"),
    (
        "aberto-por",
        "aberto_por__vinculo__pessoa__pessoafisica__nome",
    ),
    (
        "interessado",
        "interessado__vinculo__pessoa__pessoafisica__nome",
    ),
];

/// The user's relation to the ticket, as "Meus chamados" filters it.
pub const RELATIONS: [(&str, &str); 4] = [
    ("requisitante", "R"),
    ("interessado", "I"),
    ("outro", "O"),
    ("algum", "RIO"),
];

/// Pages fetched at most when every page is requested (SUAP shows 15 tickets per page).
pub const MAX_PAGES: u32 = 200;

/// What to ask SUAP for when listing tickets. Everything is optional.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TicketFilter {
    /// Only this ticket number.
    pub id: Option<u64>,
    /// Free text searched in descriptions, comments and internal notes (support queue).
    pub text: Option<String>,
    /// Situations to include (names from [`STATUSES`]); the support queue hides the finished ones by default.
    pub statuses: Vec<String>,
    /// Every situation, finished ones included.
    pub all_statuses: bool,
    /// Opened on or after this day (`YYYY-MM-DD`).
    pub since: Option<String>,
    /// Opened on or before this day (`YYYY-MM-DD`).
    pub until: Option<String>,
    /// Assignment (name from [`ASSIGNMENTS`], support queue).
    pub assignment: Option<String>,
    /// Sort key (name from [`ORDERS`], support queue).
    pub order_by: Option<String>,
    /// Sort from the last to the first.
    pub descending: bool,
    /// Only tickets whose SLA was exceeded (support queue).
    pub sla_exceeded: bool,
    /// The user's relation to the ticket (name from [`RELATIONS`], "Meus chamados").
    pub relation: Option<String>,
    /// Page to read (the first by default).
    pub page: Option<u32>,
    /// Read every page.
    pub all_pages: bool,
}

/// The SUAP code of `name` in `table`.
fn code(
    table: &[(&str, &'static str)],
    what: &str,
    name: &str,
) -> Result<&'static str, TicketError> {
    let known = table.iter().find(|(known, _)| *known == name);
    known.map(|(_, code)| *code).ok_or_else(|| {
        let names: Vec<&str> = table.iter().map(|(known, _)| *known).collect();
        TicketError::Source(format!("unknown {what} {name:?}: use {}", names.join(", ")))
    })
}

/// SUAP's `dd/mm/yyyy` for a `YYYY-MM-DD` day.
pub fn suap_date(day: &str) -> Result<String, TicketError> {
    let parts: Vec<&str> = day.split('-').collect();
    let number = |part: &str, digits: usize| {
        let valid = part.len() == digits && part.bytes().all(|byte| byte.is_ascii_digit());
        valid.then(|| part.parse::<u32>().unwrap_or(0))
    };
    if let [year, month, date] = parts[..] {
        let valid = number(year, 4).is_some()
            && number(month, 2).is_some_and(|month| (1..=12).contains(&month))
            && number(date, 2).is_some_and(|date| (1..=31).contains(&date));
        if valid {
            return Ok(format!("{date}/{month}/{year}"));
        }
    }
    Err(TicketError::Source(format!(
        "invalid date {day:?}: use YYYY-MM-DD"
    )))
}

impl TicketFilter {
    fn support_only(&self) -> Option<&'static str> {
        let used = [
            (self.text.is_some(), "text search"),
            (!self.statuses.is_empty(), "status"),
            (self.assignment.is_some(), "assignment"),
            (self.order_by.is_some() || self.descending, "sorting"),
            (self.sla_exceeded, "SLA"),
        ];
        used.iter().find(|(used, _)| *used).map(|(_, what)| *what)
    }

    /// The path, with its query string, that lists `page` of `queue` for this filter.
    pub fn path(&self, queue: TicketQueue, page: u32) -> Result<String, TicketError> {
        let mut query = Serializer::new(String::new());
        let base = match queue {
            TicketQueue::Support => {
                if self.relation.is_some() {
                    return Err(TicketError::Source(
                        "the relation filter only applies to your own tickets (--meus)".to_owned(),
                    ));
                }
                if let Some(text) = &self.text {
                    query.append_pair("texto", text);
                }
                for name in &self.statuses {
                    query.append_pair("status", code(&STATUSES, "status", name)?);
                }
                if self.all_statuses {
                    query.append_pair("todos_status", "on");
                }
                if let Some(name) = &self.assignment {
                    query.append_pair("atribuicoes", code(&ASSIGNMENTS, "assignment", name)?);
                }
                match (&self.order_by, self.descending) {
                    (Some(name), descending) => {
                        query.append_pair("ordenar_por", code(&ORDERS, "sort key", name)?);
                        query.append_pair("tipo_ordenacao", if descending { "-" } else { "" });
                    }
                    (None, true) => {
                        return Err(TicketError::Source(
                            "descending order needs a sort key".to_owned(),
                        ));
                    }
                    (None, false) => {}
                }
                if self.sla_exceeded {
                    query.append_pair("sla_estourado", "on");
                }
                "/centralservicos/listar_chamados_suporte/"
            }
            TicketQueue::Mine => {
                if let Some(what) = self.support_only() {
                    return Err(TicketError::Source(format!(
                        "the {what} filter only applies to the support queue"
                    )));
                }
                query.append_pair("tab", if self.all_statuses { "todos" } else { "ativos" });
                if let Some(name) = &self.relation {
                    query.append_pair("tipo_usuario", code(&RELATIONS, "relation", name)?);
                }
                "/centralservicos/meus_chamados/"
            }
        };
        if let Some(id) = self.id {
            query.append_pair("chamado_id", &id.to_string());
        }
        if let Some(day) = &self.since {
            query.append_pair("data_inicial", &suap_date(day)?);
        }
        if let Some(day) = &self.until {
            query.append_pair("data_final", &suap_date(day)?);
        }
        if let (Some(since), Some(until)) = (&self.since, &self.until) {
            if since > until {
                return Err(TicketError::Source(
                    "the start date is after the end date".to_owned(),
                ));
            }
        }
        if page > 1 {
            query.append_pair("page", &page.to_string());
        }
        let query = query.finish();
        Ok(match query.is_empty() {
            true => base.to_owned(),
            false => format!("{base}?{query}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter() -> TicketFilter {
        TicketFilter::default()
    }

    fn support(filter: &TicketFilter, page: u32) -> String {
        filter.path(TicketQueue::Support, page).unwrap()
    }

    fn mine(filter: &TicketFilter, page: u32) -> String {
        filter.path(TicketQueue::Mine, page).unwrap()
    }

    fn error(result: Result<String, TicketError>) -> String {
        result.unwrap_err().to_string()
    }

    #[test]
    fn an_empty_filter_asks_for_the_plain_listings() {
        assert_eq!(
            support(&filter(), 1),
            "/centralservicos/listar_chamados_suporte/"
        );
        assert_eq!(
            mine(&filter(), 1),
            "/centralservicos/meus_chamados/?tab=ativos"
        );
        assert!(support(&filter(), 3).ends_with("/?page=3"));
        assert!(mine(&filter(), 2).ends_with("?tab=ativos&page=2"));
    }

    #[test]
    fn the_support_queue_gets_every_option_translated() {
        let all = TicketFilter {
            id: Some(5),
            text: Some("moodle & plugin".to_owned()),
            statuses: vec!["aberto".to_owned(), "suspenso".to_owned()],
            all_statuses: true,
            since: Some("2026-01-02".to_owned()),
            until: Some("2026-10-08".to_owned()),
            assignment: Some("mim".to_owned()),
            order_by: Some("abertura".to_owned()),
            descending: true,
            sla_exceeded: true,
            ..filter()
        };
        assert_eq!(
            support(&all, 2),
            "/centralservicos/listar_chamados_suporte/?texto=moodle+%26+plugin&status=1&status=6\
             &todos_status=on&atribuicoes=1&ordenar_por=aberto_em&tipo_ordenacao=-&sla_estourado=on\
             &chamado_id=5&data_inicial=02%2F01%2F2026&data_final=08%2F10%2F2026&page=2"
        );
        let ascending = TicketFilter {
            order_by: Some("limite".to_owned()),
            ..filter()
        };
        assert!(
            support(&ascending, 1).ends_with("ordenar_por=data_limite_atendimento&tipo_ordenacao=")
        );
    }

    #[test]
    fn my_tickets_take_their_own_options() {
        let wanted = TicketFilter {
            id: Some(7),
            all_statuses: true,
            relation: Some("interessado".to_owned()),
            since: Some("2026-03-04".to_owned()),
            ..filter()
        };
        assert_eq!(
            mine(&wanted, 1),
            "/centralservicos/meus_chamados/?tab=todos&tipo_usuario=I&chamado_id=7&data_inicial=04%2F03%2F2026"
        );
    }

    #[test]
    fn options_for_the_other_listing_are_refused() {
        let relation = TicketFilter {
            relation: Some("algum".to_owned()),
            ..filter()
        };
        assert!(error(relation.path(TicketQueue::Support, 1)).contains("--meus"));
        let only_support = [
            (
                TicketFilter {
                    text: Some("x".to_owned()),
                    ..filter()
                },
                "text search",
            ),
            (
                TicketFilter {
                    statuses: vec!["aberto".to_owned()],
                    ..filter()
                },
                "status",
            ),
            (
                TicketFilter {
                    assignment: Some("mim".to_owned()),
                    ..filter()
                },
                "assignment",
            ),
            (
                TicketFilter {
                    order_by: Some("limite".to_owned()),
                    ..filter()
                },
                "sorting",
            ),
            (
                TicketFilter {
                    descending: true,
                    ..filter()
                },
                "sorting",
            ),
            (
                TicketFilter {
                    sla_exceeded: true,
                    ..filter()
                },
                "SLA",
            ),
        ];
        for (options, what) in only_support {
            let message = error(options.path(TicketQueue::Mine, 1));
            assert!(
                message.contains(what) && message.contains("support queue"),
                "{message}"
            );
        }
    }

    #[test]
    fn unknown_names_and_bad_dates_are_reported_before_anything_is_sent() {
        let bad_status = TicketFilter {
            statuses: vec!["pronto".to_owned()],
            ..filter()
        };
        let message = error(bad_status.path(TicketQueue::Support, 1));
        assert!(
            message.contains("unknown status \"pronto\"")
                && message.contains("aberto, atendimento")
        );
        let bad_assignment = TicketFilter {
            assignment: Some("x".to_owned()),
            ..filter()
        };
        assert!(error(bad_assignment.path(TicketQueue::Support, 1)).contains("unknown assignment"));
        let bad_order = TicketFilter {
            order_by: Some("x".to_owned()),
            ..filter()
        };
        assert!(error(bad_order.path(TicketQueue::Support, 1)).contains("unknown sort key"));
        let bad_relation = TicketFilter {
            relation: Some("x".to_owned()),
            ..filter()
        };
        assert!(error(bad_relation.path(TicketQueue::Mine, 1)).contains("unknown relation"));
        let lonely_desc = TicketFilter {
            descending: true,
            ..filter()
        };
        assert!(error(lonely_desc.path(TicketQueue::Support, 1)).contains("needs a sort key"));

        for bad in [
            "",
            "2026",
            "2026-13-01",
            "2026-00-10",
            "2026-01-32",
            "26-01-01",
            "2026-1-01",
            "2026-01-0x",
            "a-b-c",
            "2026-01-01-01",
        ] {
            let dated = TicketFilter {
                since: Some(bad.to_owned()),
                ..filter()
            };
            assert!(
                error(dated.path(TicketQueue::Support, 1)).contains("invalid date"),
                "{bad:?}"
            );
            let dated = TicketFilter {
                until: Some(bad.to_owned()),
                ..filter()
            };
            assert!(dated.path(TicketQueue::Mine, 1).is_err(), "{bad:?}");
        }
        let backwards = TicketFilter {
            since: Some("2026-10-08".to_owned()),
            until: Some("2026-10-07".to_owned()),
            ..filter()
        };
        assert!(error(backwards.path(TicketQueue::Support, 1)).contains("after the end date"));
        assert_eq!(suap_date("2026-02-28").unwrap(), "28/02/2026");
    }
}
