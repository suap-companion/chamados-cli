//! Domain logic for the SUAP ticket companion.

use scraper::{ElementRef, Html, Selector};
use suap_core::{SuapClient, SuapError};
use thiserror::Error;
use url::Url;

const TICKET_PATH_PREFIX: &str = "/centralservicos/chamado/";

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
    #[error(transparent)]
    Suap(#[from] SuapError),
}

/// Which SUAP ticket listing to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TicketQueue {
    /// Tickets in the support queue ("Chamados" menu, for service agents).
    Support,
    /// Active tickets the user takes part in ("Meus chamados" menu).
    Mine,
}

impl TicketQueue {
    pub fn path(self) -> &'static str {
        match self {
            Self::Support => "/centralservicos/listar_chamados_suporte/",
            Self::Mine => "/centralservicos/meus_chamados/?tab=ativos",
        }
    }
}

#[allow(async_fn_in_trait)]
pub trait TicketSource {
    async fn list_tickets(&self) -> Result<Vec<RemoteTicket>, TicketError>;
    async fn get_ticket(&self, id: &str) -> Result<RemoteTicket, TicketError>;
}

/// Reads tickets from the SUAP web interface using an authenticated session.
pub struct SuapTicketSource<'a> {
    client: &'a SuapClient,
    queue: TicketQueue,
}

impl<'a> SuapTicketSource<'a> {
    pub fn new(client: &'a SuapClient, queue: TicketQueue) -> Self {
        Self { client, queue }
    }
}

impl TicketSource for SuapTicketSource<'_> {
    async fn list_tickets(&self) -> Result<Vec<RemoteTicket>, TicketError> {
        let html = self.client.fetch_page(self.queue.path()).await?;
        Ok(parse_ticket_list(&html, self.client.base_url()))
    }

    async fn get_ticket(&self, _id: &str) -> Result<RemoteTicket, TicketError> {
        Err(TicketError::NotImplemented)
    }
}

/// Extracts the tickets from a SUAP listing page. Boxes without a ticket link are ignored.
pub fn parse_ticket_list(html: &str, base_url: &Url) -> Vec<RemoteTicket> {
    let document = Html::parse_document(html);
    let boxes = Selector::parse(".general-box").expect("static selector is valid");
    document.select(&boxes).filter_map(|element| parse_ticket(element, base_url)).collect()
}

fn parse_ticket(element: ElementRef<'_>, base_url: &Url) -> Option<RemoteTicket> {
    let link = Selector::parse("h4 a").expect("static selector is valid");
    let subject = Selector::parse("h4 a strong").expect("static selector is valid");
    let status = Selector::parse(".status").expect("static selector is valid");

    let href = element.select(&link).next()?.value().attr("href")?;
    let id = href.strip_prefix(TICKET_PATH_PREFIX)?.trim_end_matches('/');
    Some(RemoteTicket {
        id: id.to_owned(),
        subject: element.select(&subject).next().map(text_of),
        status: element.select(&status).next().map(text_of),
        details_url: base_url.join(href).ok()?.to_string(),
    })
}

fn text_of(element: ElementRef<'_>) -> String {
    element.text().collect::<String>().trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use wiremock::{matchers::{method, path, query_param}, Mock, MockServer, ResponseTemplate};

    const LISTING: &str = r#"
        <div class="general-box danger"><div class="primary-info">
          <div class="status-info-container"><span class="status status-em-atendimento ">Em atendimento</span></div>
          <h4><a href="/centralservicos/chamado/559298/">REQ #559298 <strong>Atualização de versão do Moodle</strong></a></h4>
        </div></div>
        <div class="general-box"><h4><a href="/centralservicos/chamado/1/">REQ #1</a></h4></div>
        <div class="general-box"><p>sem link</p></div>
        <div class="general-box"><h4><a href="/outra/pagina/">REQ #2</a></h4></div>
        <div class="general-box"><h4><a>sem href</a></h4></div>
        <div class="general-box"><h4><a href="http://[::1">REQ #3</a></h4></div>
    "#;

    fn base() -> Url {
        Url::parse("https://suap.example/").unwrap()
    }

    #[test]
    fn parses_tickets_and_skips_invalid_boxes() {
        let tickets = parse_ticket_list(LISTING, &base());
        assert_eq!(tickets.len(), 2);
        assert_eq!(
            tickets[0],
            RemoteTicket {
                id: "559298".to_owned(),
                subject: Some("Atualização de versão do Moodle".to_owned()),
                status: Some("Em atendimento".to_owned()),
                details_url: "https://suap.example/centralservicos/chamado/559298/".to_owned(),
            }
        );
        assert_eq!((tickets[1].id.as_str(), tickets[1].subject.clone(), tickets[1].status.clone()), ("1", None, None));
    }

    #[test]
    fn page_without_tickets_is_empty() {
        assert!(parse_ticket_list("<p>nada</p>", &base()).is_empty());
    }

    #[test]
    fn queues_have_distinct_paths() {
        assert!(TicketQueue::Support.path().contains("listar_chamados_suporte"));
        assert!(TicketQueue::Mine.path().contains("meus_chamados"));
    }

    #[test]
    fn errors_are_displayed() {
        assert_eq!(TicketError::NotImplemented.to_string(), "ticket source is not implemented yet");
        assert_eq!(TicketError::Source("x".to_owned()).to_string(), "ticket source error: x");
        assert_eq!(
            TicketError::from(SuapError::NotAuthenticated).to_string(),
            "SUAP session is not authenticated"
        );
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

    async fn client_for(server: &MockServer) -> (tempfile::TempDir, SuapClient) {
        let directory = tempdir().unwrap();
        let paths = suap_core::AppPaths::from_dirs(directory.path().join("c"), directory.path().join("d"));
        let config = suap_core::SuapConfig {
            base_url: Url::parse(&format!("{}/", server.uri())).unwrap(),
            username: None,
        };
        (directory, SuapClient::open(&paths, &config).unwrap())
    }

    #[tokio::test]
    async fn source_lists_tickets_from_the_selected_queue() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/centralservicos/meus_chamados/"))
            .and(query_param("tab", "ativos"))
            .respond_with(ResponseTemplate::new(200).set_body_string(LISTING))
            .mount(&server)
            .await;
        let (_dir, client) = client_for(&server).await;
        let source = SuapTicketSource::new(&client, TicketQueue::Mine);
        assert_eq!(source.list_tickets().await.unwrap().len(), 2);
        assert!(matches!(source.get_ticket("1").await, Err(TicketError::NotImplemented)));
    }

    #[tokio::test]
    async fn source_reports_unauthenticated_session() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/centralservicos/listar_chamados_suporte/"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/accounts/login/"))
            .mount(&server)
            .await;
        Mock::given(method("GET")).and(path("/accounts/login/")).respond_with(ResponseTemplate::new(200)).mount(&server).await;
        let (_dir, client) = client_for(&server).await;
        let source = SuapTicketSource::new(&client, TicketQueue::Support);
        assert!(matches!(source.list_tickets().await, Err(TicketError::Suap(SuapError::NotAuthenticated))));
    }
}
