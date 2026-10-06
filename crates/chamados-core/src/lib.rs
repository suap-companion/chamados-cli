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
