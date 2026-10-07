//! Domain logic for the SUAP ticket companion.

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteTicket {
    pub id: String,
    pub subject: Option<String>,
    pub status: Option<String>,
    pub details_url: String,
}

#[derive(Debug, Error)]
pub enum TicketError {
    #[error("ticket source is not implemented yet")]
    NotImplemented,
    #[error("ticket source error: {0}")]
    Source(String),
}

pub trait TicketSource {
    fn list_tickets(&self) -> Result<Vec<RemoteTicket>, TicketError>;
    fn get_ticket(&self, id: &str) -> Result<RemoteTicket, TicketError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubSource;

    impl TicketSource for StubSource {
        fn list_tickets(&self) -> Result<Vec<RemoteTicket>, TicketError> {
            Err(TicketError::NotImplemented)
        }

        fn get_ticket(&self, id: &str) -> Result<RemoteTicket, TicketError> {
            Err(TicketError::Source(format!("ticket {id} not found")))
        }
    }

    #[test]
    fn source_errors_are_displayed() {
        let source = StubSource;
        assert_eq!(source.list_tickets().unwrap_err().to_string(), "ticket source is not implemented yet");
        assert_eq!(source.get_ticket("1").unwrap_err().to_string(), "ticket source error: ticket 1 not found");
    }

    #[test]
    fn remote_ticket_is_comparable_and_cloneable() {
        let ticket = RemoteTicket {
            id: "1".to_owned(),
            subject: Some("Assunto".to_owned()),
            status: None,
            details_url: "https://example.org/1".to_owned(),
        };
        assert_eq!(ticket.clone(), ticket);
        assert!(format!("{ticket:?}").contains("Assunto"));
    }
}
