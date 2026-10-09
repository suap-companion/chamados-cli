//! Domain logic for the SUAP ticket companion.

use std::collections::HashSet;

use scraper::{node::Node, ElementRef, Html, Selector};
use serde::Deserialize;
use suap_core::{FormFile, SuapClient, SuapError};
use thiserror::Error;
use url::Url;

pub mod filter;
pub mod titles;

pub use filter::{suap_date, TicketFilter, ASSIGNMENTS, MAX_PAGES, ORDERS, RELATIONS, STATUSES};
pub use titles::{validate_title, TitleEntry, TitleStore, MAX_TITLE_CHARS};

const TICKET_PATH_PREFIX: &str = "/centralservicos/chamado/";
const OPEN_PATH_PREFIX: &str = "/centralservicos/abrir_chamado/";
const CAMPUS_PATH_PREFIX: &str = "/centralservicos/get_campus_com_centros_atendimento/";
const CENTERS_PATH_PREFIX: &str = "/centralservicos/get_centros_atendimento_por_servico_e_campus/";
const OPENED_MARKER: &str = "Número do chamado:";
const ASSUME_PATH_PREFIX: &str = "/centralservicos/auto_atribuir_chamado/";
const START_PATH_PREFIX: &str = "/centralservicos/colocar_em_atendimento/";
const SUSPEND_PATH_PREFIX: &str = "/centralservicos/suspender_chamado/";
const RESOLVE_PATH_PREFIX: &str = "/centralservicos/resolver_chamado/";
const ATTACHMENT_PATH_PREFIX: &str = "/djtools/arquivo/centralservicos/chamadoanexo/";
const CANCEL_PATH_PREFIX: &str = "/centralservicos/cancelar_chamado/";

/// File types SUAP accepts as ticket attachments (it rejects any other).
pub const ALLOWED_ATTACHMENT_EXTENSIONS: [&str; 9] = [
    "xlsx", "xls", "csv", "docx", "doc", "pdf", "jpg", "jpeg", "png",
];
/// Number of attachment slots in SUAP's "open ticket" form.
pub const MAX_ATTACHMENTS: usize = 3;
const ATTACHMENT_DESCRIPTION_MAX: usize = 80;

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

/// One entry of a ticket timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineEntry {
    pub date: String,
    pub text: String,
}

/// A file attached to a ticket, as linked from its timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentLink {
    /// File name as shown by SUAP.
    pub name: String,
    /// Path of the download link, relative to the SUAP address.
    pub path: String,
}

/// Full details of a ticket, as shown on its SUAP page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketDetails {
    pub id: String,
    pub title: String,
    pub heading: Option<String>,
    pub statuses: Vec<String>,
    pub fields: Vec<(String, String)>,
    pub timeline: Vec<TimelineEntry>,
    /// Files attached to the ticket, in the order the timeline lists them.
    pub attachments: Vec<AttachmentLink>,
    pub details_url: String,
}

/// A file to attach when opening a ticket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub file_name: String,
    pub bytes: Vec<u8>,
}

/// Data needed to open a new ticket.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewTicket {
    /// SUAP service id (`/centralservicos/abrir_chamado/<id>/`).
    pub service_id: u64,
    pub description: String,
    /// Campus (`uo`) id; defaults to the user's campus as suggested by SUAP.
    pub campus: Option<String>,
    /// Service center id; defaults to the only center available for the campus.
    pub center: Option<String>,
    /// Interested person (a SUAP "vínculo" id); SUAP defaults to the logged user.
    pub interested: Option<String>,
    /// Any other form field, overriding the form defaults (e.g. `patrimonio`).
    pub extra_fields: Vec<(String, String)>,
    /// Send SUAP's "copy of the opening" e-mail to the interested people.
    pub copy_email: bool,
    /// Files to attach (at most [`MAX_ATTACHMENTS`], of an allowed type).
    pub attachments: Vec<Attachment>,
}

/// A message added to a ticket's thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Message {
    /// A comment, visible to the interested people.
    Comment,
    /// An internal note, visible only to the service team.
    InternalNote,
}

impl Message {
    fn action_prefix(self) -> &'static str {
        match self {
            Self::Comment => "/centralservicos/adicionar_comentario/",
            Self::InternalNote => "/centralservicos/adicionar_nota_interna/",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Comment => "comment",
            Self::InternalNote => "internal note",
        }
    }
}

/// How to resolve a ticket.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resolution {
    /// The resolution message (multi-line text).
    pub text: String,
    /// Related knowledge-base articles. SUAP requires at least one when it offers any; when empty,
    /// the first article offered is used.
    pub articles: Vec<String>,
    /// Ids of other tickets to resolve together.
    pub also: Vec<String>,
    /// Id of a standard reply to use.
    pub standard_reply: Option<String>,
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
    /// Lists the tickets that match `filter` (the page it asks for, or every page).
    async fn list_filtered(&self, filter: &TicketFilter) -> Result<Vec<RemoteTicket>, TicketError>;
    async fn get_ticket(&self, id: &str) -> Result<TicketDetails, TicketError>;
    /// Downloads the content of an attachment listed in the ticket details.
    async fn download_attachment(&self, link: &AttachmentLink) -> Result<Vec<u8>, TicketError>;
    /// Opens a new ticket and returns its id.
    async fn open_ticket(&self, ticket: &NewTicket) -> Result<String, TicketError>;
    /// Adds a comment or an internal note (multi-line text) to the ticket.
    async fn add_message(&self, id: &str, kind: Message, text: &str) -> Result<(), TicketError>;
    /// Moves the ticket to "Suspenso", with the suspension message (multi-line text).
    async fn suspend_ticket(&self, id: &str, text: &str) -> Result<(), TicketError>;
    /// Moves the ticket to "Cancelado", with the reason. It cannot be undone.
    async fn cancel_ticket(&self, id: &str, text: &str) -> Result<(), TicketError>;
    /// Moves the ticket to "Resolvido".
    async fn resolve_ticket(&self, id: &str, resolution: &Resolution) -> Result<(), TicketError>;
    /// Assigns the ticket to the logged user ("assumir").
    async fn assume_ticket(&self, id: &str) -> Result<(), TicketError>;
    /// Moves the ticket to "Em atendimento" (the user must have assumed it first).
    async fn start_service(&self, id: &str) -> Result<(), TicketError>;
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

    async fn list_page(
        &self,
        filter: &TicketFilter,
        page: u32,
    ) -> Result<Vec<RemoteTicket>, TicketError> {
        let html = self
            .client
            .fetch_page(&filter.path(self.queue, page)?)
            .await?;
        Ok(parse_ticket_list(&html, self.client.base_url()))
    }

    /// Loads the SUAP form behind a status change (the one containing `field`); returns its path,
    /// the page and the form defaults.
    async fn open_status_form(
        &self,
        prefix: &str,
        id: &str,
        what: &str,
        field: &str,
    ) -> Result<(String, String, Vec<(String, String)>), TicketError> {
        check_ticket_id(id)?;
        let path = format!("{prefix}{id}/");
        let html = match self.client.fetch_page(&path).await {
            Err(SuapError::Transport(status)) => {
                return Err(TicketError::Source(format!(
                    "ticket {id} cannot be {what} ({status}): check its situation and your permissions"
                )));
            }
            other => other?,
        };
        let fields = parse_form_with_field(&html, field)
            .ok_or_else(|| TicketError::Source(format!("ticket {id} has no {what} form")))?;
        Ok((path, html, fields))
    }

    /// Posts a status-change form back to its own URL and reports what SUAP rejected, if anything.
    async fn send_status_form(
        &self,
        path: &str,
        fields: &[(String, String)],
        what: &str,
    ) -> Result<(), TicketError> {
        let response = match self.client.submit_form(path, fields).await {
            Err(SuapError::Transport(status)) => {
                return Err(TicketError::Source(format!(
                    "SUAP refused {what} ({status}): the ticket may already be in that situation, or you lack permission"
                )));
            }
            other => other?,
        };
        let errors = page_errors(&response.body);
        if errors.is_empty() {
            return Ok(());
        }
        Err(TicketError::Source(format!(
            "SUAP rejected {what}: {}",
            errors.join("; ")
        )))
    }

    /// Calls a SUAP ticket action (a GET) and surfaces the error flash message, if any.
    async fn run_action(&self, prefix: &str, id: &str) -> Result<(), TicketError> {
        check_ticket_id(id)?;
        let body = match self.client.fetch_page(&format!("{prefix}{id}/")).await {
            Err(SuapError::Transport(status)) => {
                return Err(TicketError::Source(format!(
                    "SUAP refused the action ({status}): check the ticket's situation (starting needs it assumed first) and your permissions"
                )));
            }
            other => other?,
        };
        let errors = flash_errors(&body);
        if errors.is_empty() {
            return Ok(());
        }
        Err(TicketError::Source(format!(
            "SUAP refused the action: {}",
            errors.join("; ")
        )))
    }
}

/// The message without its trailing line break; an error if nothing is left.
fn non_empty_text<'a>(text: &'a str, what: &str) -> Result<&'a str, TicketError> {
    let text = text.trim_end_matches(['\r', '\n']);
    if text.trim().is_empty() {
        return Err(TicketError::Source(format!("the {what} text is empty")));
    }
    Ok(text)
}

pub(crate) fn check_ticket_id(id: &str) -> Result<(), TicketError> {
    if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(TicketError::Source(format!("invalid ticket id {id:?}")));
    }
    Ok(())
}

/// Checks the attachment count and file types against what SUAP accepts.
pub fn validate_attachments(attachments: &[Attachment]) -> Result<(), TicketError> {
    if attachments.len() > MAX_ATTACHMENTS {
        return Err(TicketError::Source(format!(
            "at most {MAX_ATTACHMENTS} attachments are allowed"
        )));
    }
    for attachment in attachments {
        let extension = attachment
            .file_name
            .rsplit_once('.')
            .map(|(_, extension)| extension.to_ascii_lowercase());
        if !extension
            .is_some_and(|extension| ALLOWED_ATTACHMENT_EXTENSIONS.contains(&extension.as_str()))
        {
            return Err(TicketError::Source(format!(
                "attachment {:?} has an unsupported type; allowed: {}",
                attachment.file_name,
                ALLOWED_ATTACHMENT_EXTENSIONS.join(", ")
            )));
        }
    }
    Ok(())
}

/// Error messages SUAP flashes on the page (`<p class="... alert-error">`), without the close button text.
fn flash_errors(body: &str) -> Vec<String> {
    let document = Html::parse_document(body);
    document
        .select(&selector("p.alert-error"))
        .map(|message| {
            let own_text = message.children().filter_map(|node| node.value().as_text());
            own_text
                .map(|text| text.trim())
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|message| !message.is_empty())
        .collect()
}

impl TicketSource for SuapTicketSource<'_> {
    async fn list_tickets(&self) -> Result<Vec<RemoteTicket>, TicketError> {
        let html = self.client.fetch_page(self.queue.path()).await?;
        Ok(parse_ticket_list(&html, self.client.base_url()))
    }

    async fn list_filtered(&self, filter: &TicketFilter) -> Result<Vec<RemoteTicket>, TicketError> {
        if !filter.all_pages {
            return self.list_page(filter, filter.page.unwrap_or(1)).await;
        }
        // SUAP paginates; stop at the first page that brings nothing new (it repeats the last one
        // when asked for a page that does not exist).
        let mut seen = HashSet::new();
        let mut tickets = Vec::new();
        for page in 1..=MAX_PAGES {
            let before = tickets.len();
            for ticket in self.list_page(filter, page).await? {
                if seen.insert(ticket.id.clone()) {
                    tickets.push(ticket);
                }
            }
            if tickets.len() == before {
                break;
            }
        }
        Ok(tickets)
    }

    async fn get_ticket(&self, id: &str) -> Result<TicketDetails, TicketError> {
        check_ticket_id(id)?;
        let html = self
            .client
            .fetch_page(&format!("{TICKET_PATH_PREFIX}{id}/"))
            .await?;
        parse_ticket_details(&html, self.client.base_url(), id)
            .ok_or_else(|| TicketError::Source(format!("could not parse ticket {id}")))
    }

    async fn download_attachment(&self, link: &AttachmentLink) -> Result<Vec<u8>, TicketError> {
        if !link.path.starts_with(ATTACHMENT_PATH_PREFIX) {
            // Only files SUAP serves for tickets: never an arbitrary address found in a page.
            return Err(TicketError::Source(format!(
                "{:?} is not a ticket attachment link",
                link.path
            )));
        }
        Ok(self.client.fetch_bytes(&link.path).await?)
    }

    async fn add_message(&self, id: &str, kind: Message, text: &str) -> Result<(), TicketError> {
        check_ticket_id(id)?;
        let text = text.trim_end_matches(['\r', '\n']);
        if text.trim().is_empty() {
            return Err(TicketError::Source(format!(
                "the {} text is empty",
                kind.label()
            )));
        }
        let page = self
            .client
            .fetch_page(&format!("{TICKET_PATH_PREFIX}{id}/"))
            .await?;
        let action = format!("{}{id}/", kind.action_prefix());
        let mut fields = parse_form_by_action(&page, &action).ok_or_else(|| {
            TicketError::Source(format!(
                "ticket {id} has no {} form (no permission, or the ticket is closed)",
                kind.label()
            ))
        })?;
        set_field(&mut fields, "texto", text);
        let response = self.client.submit_form(&action, &fields).await?;
        let errors = page_errors(&response.body);
        if errors.is_empty() {
            return Ok(());
        }
        Err(TicketError::Source(format!(
            "SUAP rejected the {}: {}",
            kind.label(),
            errors.join("; ")
        )))
    }

    async fn suspend_ticket(&self, id: &str, text: &str) -> Result<(), TicketError> {
        let text = non_empty_text(text, "suspension")?;
        let (path, _, mut fields) = self
            .open_status_form(SUSPEND_PATH_PREFIX, id, "suspended", "observacao")
            .await?;
        set_field(&mut fields, "observacao", text);
        self.send_status_form(&path, &fields, "the suspension")
            .await
    }

    async fn cancel_ticket(&self, id: &str, text: &str) -> Result<(), TicketError> {
        let text = non_empty_text(text, "cancellation")?;
        let (path, _, mut fields) = self
            .open_status_form(CANCEL_PATH_PREFIX, id, "cancelled", "observacao")
            .await?;
        set_field(&mut fields, "observacao", text);
        self.send_status_form(&path, &fields, "the cancellation")
            .await
    }

    async fn resolve_ticket(&self, id: &str, resolution: &Resolution) -> Result<(), TicketError> {
        let text = non_empty_text(&resolution.text, "resolution")?;
        let (path, html, mut fields) = self
            .open_status_form(RESOLVE_PATH_PREFIX, id, "resolved", "comentario")
            .await?;
        let offered = form_choices(&html, "comentario", "bases_conhecimento");
        let articles = match (offered.first(), resolution.articles.is_empty()) {
            (None, true) => Vec::new(),
            (None, false) => {
                return Err(TicketError::Source(
                    "the resolve form offers no articles to relate".to_owned(),
                ));
            }
            (Some(first), true) => vec![first.clone()],
            (Some(_), false) => resolution.articles.clone(),
        };
        if let Some(unknown) = articles.iter().find(|article| !offered.contains(article)) {
            return Err(TicketError::Source(format!(
                "article {unknown} is not offered; available: {}",
                offered.join(", ")
            )));
        }
        fields.retain(|(name, _)| {
            name != "bases_conhecimento" && name != "outros_chamados_a_resolver"
        });
        for article in articles {
            fields.push(("bases_conhecimento".to_owned(), article));
        }
        for other in &resolution.also {
            fields.push(("outros_chamados_a_resolver".to_owned(), other.clone()));
        }
        if let Some(reply) = &resolution.standard_reply {
            set_field(&mut fields, "resposta_padrao", reply);
        }
        set_field(&mut fields, "comentario", text);
        self.send_status_form(&path, &fields, "the resolution")
            .await
    }

    async fn assume_ticket(&self, id: &str) -> Result<(), TicketError> {
        self.run_action(ASSUME_PATH_PREFIX, id).await
    }

    async fn start_service(&self, id: &str) -> Result<(), TicketError> {
        self.run_action(START_PATH_PREFIX, id).await
    }

    async fn open_ticket(&self, ticket: &NewTicket) -> Result<String, TicketError> {
        validate_attachments(&ticket.attachments)?;
        let form_path = format!("{OPEN_PATH_PREFIX}{}/", ticket.service_id);
        let html = self.client.fetch_page(&form_path).await?;
        let mut fields = parse_open_form(&html).ok_or_else(|| {
            TicketError::Source(format!("service {} has no ticket form", ticket.service_id))
        })?;

        let campus = match &ticket.campus {
            Some(campus) => campus.clone(),
            None => {
                let reply = self
                    .client
                    .fetch_page(&format!("{CAMPUS_PATH_PREFIX}{}/0/", ticket.service_id))
                    .await?;
                default_campus(&reply)?
            }
        };
        let center = match &ticket.center {
            Some(center) => center.clone(),
            None => {
                let path = format!("{CENTERS_PATH_PREFIX}{}/{campus}/", ticket.service_id);
                default_center(&self.client.fetch_page(&path).await?)?
            }
        };

        set_field(&mut fields, "descricao", &ticket.description);
        set_field(&mut fields, "uo", &campus);
        set_field(&mut fields, "centro_atendimento", &center);
        if let Some(interested) = &ticket.interested {
            set_field(&mut fields, "interessado", interested);
        }
        fields.retain(|(name, _)| name != "enviar_copia_email");
        if ticket.copy_email {
            fields.push(("enviar_copia_email".to_owned(), "on".to_owned()));
        }
        let mut files = Vec::new();
        for (index, attachment) in ticket.attachments.iter().enumerate() {
            let description: String = attachment
                .file_name
                .chars()
                .take(ATTACHMENT_DESCRIPTION_MAX)
                .collect();
            set_field(
                &mut fields,
                &format!("chamadoanexo_set-{index}-descricao"),
                &description,
            );
            files.push(FormFile {
                field: format!("chamadoanexo_set-{index}-anexo"),
                file_name: attachment.file_name.clone(),
                bytes: attachment.bytes.clone(),
            });
        }
        for (name, value) in &ticket.extra_fields {
            set_field(&mut fields, name, value);
        }

        let response = if files.is_empty() {
            self.client.submit_form(&form_path, &fields).await?
        } else {
            self.client
                .submit_multipart(&form_path, &fields, &files)
                .await?
        };
        parse_open_result(&response.path, &response.body)
    }
}

/// Extracts the tickets from a SUAP listing page. Boxes without a ticket link are ignored.
pub fn parse_ticket_list(html: &str, base_url: &Url) -> Vec<RemoteTicket> {
    let document = Html::parse_document(html);
    let boxes = Selector::parse(".general-box").expect("static selector is valid");
    document
        .select(&boxes)
        .filter_map(|element| parse_ticket(element, base_url))
        .collect()
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

/// Text of `element` with every run of whitespace (and element boundaries) collapsed to one space.
fn flat_text(element: ElementRef<'_>) -> String {
    element
        .text()
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Like [`flat_text`], but keeps the line breaks SUAP renders as `<br>` and the blank line between
/// paragraphs (`<p>`), so multi-line descriptions and comments read as they were written.
fn rich_text(element: ElementRef<'_>) -> String {
    let mut text = String::new();
    let mut paragraphs = 0;
    for node in element.descendants() {
        match node.value() {
            Node::Text(chunk) => {
                let words = chunk.split_whitespace().collect::<Vec<_>>().join(" ");
                if words.is_empty() {
                    continue;
                }
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push(' ');
                }
                text.push_str(&words);
            }
            Node::Element(tag) if tag.name() == "br" => text.push('\n'),
            Node::Element(tag) if tag.name() == "p" => {
                if paragraphs > 0 {
                    text.push_str("\n\n");
                }
                paragraphs += 1;
            }
            _ => {}
        }
    }
    text.trim().to_owned()
}

fn selector(css: &str) -> Selector {
    Selector::parse(css).expect("static selector is valid")
}

/// Sets `name` to `value`, replacing every existing entry with that name.
fn set_field(fields: &mut Vec<(String, String)>, name: &str, value: &str) {
    fields.retain(|(existing, _)| existing != name);
    fields.push((name.to_owned(), value.to_owned()));
}

/// Collects the default values of the "open ticket" form (hidden fields, CSRF token, selected options...).
///
/// Returns `None` if the page has no form with a `descricao` field.
pub fn parse_open_form(html: &str) -> Option<Vec<(String, String)>> {
    parse_form_with_field(html, "descricao")
}

/// Collects the default values of the form that has a control named `field`.
///
/// Returns `None` if the page has no such form.
pub fn parse_form_with_field(html: &str, field: &str) -> Option<Vec<(String, String)>> {
    let document = Html::parse_document(html);
    let form = form_with_field(&document, field)?;
    Some(collect_fields(form))
}

fn form_with_field<'a>(document: &'a Html, field: &str) -> Option<ElementRef<'a>> {
    let (forms, anchor) = (selector("form"), selector(&format!("[name={field}]")));
    document
        .select(&forms)
        .find(|form| form.select(&anchor).next().is_some())
}

/// Values the form containing `anchor` offers for the control called `name` (checkboxes, radios or options).
fn form_choices(html: &str, anchor: &str, name: &str) -> Vec<String> {
    let document = Html::parse_document(html);
    let Some(form) = form_with_field(&document, anchor) else {
        return Vec::new();
    };
    let choices = selector(&format!("input[name={name}], select[name={name}] option"));
    form.select(&choices)
        .filter_map(|choice| choice.value().attr("value"))
        .map(str::to_owned)
        .collect()
}

/// Collects the default values of the form whose `action` is exactly `action`.
///
/// Returns `None` if the page has no such form.
pub fn parse_form_by_action(html: &str, action: &str) -> Option<Vec<(String, String)>> {
    let document = Html::parse_document(html);
    let form = document
        .select(&selector("form"))
        .find(|form| form.value().attr("action") == Some(action))?;
    Some(collect_fields(form))
}

/// Default values of every control of `form` (hidden fields, CSRF token, selected options...).
fn collect_fields(form: ElementRef<'_>) -> Vec<(String, String)> {
    let (controls, options) = (selector("input, textarea, select"), selector("option"));
    let mut fields = Vec::new();
    for control in form.select(&controls) {
        let Some(name) = control.value().attr("name") else {
            continue;
        };
        let value = match control.value().name() {
            "textarea" => text_of(control),
            "select" => {
                let all: Vec<_> = control.select(&options).collect();
                let chosen = all
                    .iter()
                    .find(|option| option.value().attr("selected").is_some())
                    .or(all.first());
                match chosen {
                    Some(option) => option
                        .value()
                        .attr("value")
                        .map_or_else(|| text_of(*option), str::to_owned),
                    None => continue,
                }
            }
            _ => {
                let kind = control.value().attr("type").unwrap_or("text");
                let toggled = matches!(kind, "checkbox" | "radio");
                let skipped = matches!(kind, "submit" | "button" | "file" | "image" | "reset");
                if skipped || (toggled && control.value().attr("checked").is_none()) {
                    continue;
                }
                control
                    .value()
                    .attr("value")
                    .unwrap_or(if toggled { "on" } else { "" })
                    .to_owned()
            }
        };
        fields.push((name.to_owned(), value));
    }
    fields
}

/// Error messages on a page SUAP returned: flashed errors and form (field) errors, without repeats.
fn page_errors(body: &str) -> Vec<String> {
    let document = Html::parse_document(body);
    let mut seen = HashSet::new();
    let mut errors = flash_errors(body);
    errors.extend(
        document
            .select(&selector(".errorlist li, .errornote"))
            .map(flat_text),
    );
    errors.retain(|message| seen.insert(message.clone()));
    errors
}

#[derive(Deserialize)]
struct CampusReply {
    /// `[id, acronym, selected]`
    campus: Vec<(u64, String, bool)>,
}

#[derive(Deserialize)]
struct CentersReply {
    /// `[id, name, is_local]`
    centros: Vec<(u64, String, bool)>,
}

/// Picks the campus SUAP marks as selected (the user's own), or the first one available.
fn default_campus(json: &str) -> Result<String, TicketError> {
    let reply: CampusReply = serde_json::from_str(json)
        .map_err(|error| TicketError::Source(format!("campus list: {error}")))?;
    let chosen = reply
        .campus
        .iter()
        .find(|campus| campus.2)
        .or(reply.campus.first());
    chosen
        .map(|campus| campus.0.to_string())
        .ok_or_else(|| TicketError::Source("no campus available for this service".to_owned()))
}

/// Picks the only service center available; asks for an explicit one when there are several.
fn default_center(json: &str) -> Result<String, TicketError> {
    let reply: CentersReply = serde_json::from_str(json)
        .map_err(|error| TicketError::Source(format!("center list: {error}")))?;
    match reply.centros.as_slice() {
        [] => Err(TicketError::Source(
            "no service center available for this campus".to_owned(),
        )),
        [only] => Ok(only.0.to_string()),
        several => {
            let options: Vec<_> = several
                .iter()
                .map(|center| format!("{} ({})", center.0, center.1))
                .collect();
            Err(TicketError::Source(format!(
                "several service centers available, choose one: {}",
                options.join(", ")
            )))
        }
    }
}

/// Interprets the page SUAP returned after submitting the "open ticket" form.
fn parse_open_result(path: &str, body: &str) -> Result<String, TicketError> {
    if let Some(id) = path.strip_prefix(TICKET_PATH_PREFIX) {
        return Ok(id.trim_end_matches('/').to_owned());
    }
    if let Some((_, after)) = body.split_once(OPENED_MARKER) {
        return Ok(after
            .trim_start()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect());
    }
    let document = Html::parse_document(body);
    let mut seen = HashSet::new();
    let errors: Vec<_> = document
        .select(&selector(".errorlist li, .errornote"))
        .map(flat_text)
        .filter(|message| seen.insert(message.clone()))
        .collect();
    if errors.is_empty() {
        return Err(TicketError::Source(
            "could not confirm that the ticket was opened".to_owned(),
        ));
    }
    Err(TicketError::Source(format!(
        "SUAP rejected the ticket: {}",
        errors.join("; ")
    )))
}

/// Extracts the details of ticket `id` from its SUAP page; `None` if the page is not a ticket page.
pub fn parse_ticket_details(html: &str, base_url: &Url, id: &str) -> Option<TicketDetails> {
    let document = Html::parse_document(html);
    let title = flat_text(
        document
            .select(&selector("main#content .title-container h2"))
            .next()?,
    );
    let heading = document
        .select(&selector("main#content .accordion-button"))
        .next()
        .map(flat_text);
    let statuses = document
        .select(&selector("main#content .object-status .status"))
        .map(flat_text)
        .collect();

    let (term, definition) = (selector("dt"), selector("dd"));
    let fields = document
        .select(&selector(
            "main#content .accordion-body .definition-list .list-item",
        ))
        .filter_map(|item| {
            Some((
                flat_text(item.select(&term).next()?),
                rich_text(item.select(&definition).next()?),
            ))
        })
        .collect();

    let (date, content) = (selector(".timeline-date"), selector(".timeline-content"));
    let timeline = document
        .select(&selector("[data-tab=linha_tempo] ul.timeline > li"))
        .filter_map(|item| {
            Some(TimelineEntry {
                date: flat_text(item.select(&date).next()?),
                text: rich_text(item.select(&content).next()?),
            })
        })
        .collect();

    let mut attachments: Vec<AttachmentLink> = Vec::new();
    let links = selector("[data-tab=linha_tempo] ul.timeline > li .timeline-content a[href]");
    for link in document.select(&links) {
        let path = link.value().attr("href").unwrap_or_default();
        let known = attachments.iter().any(|known| known.path == path);
        if path.starts_with(ATTACHMENT_PATH_PREFIX) && !known {
            attachments.push(AttachmentLink {
                name: flat_text(link),
                path: path.to_owned(),
            });
        }
    }

    Some(TicketDetails {
        id: id.to_owned(),
        title,
        heading,
        statuses,
        fields,
        timeline,
        attachments,
        details_url: base_url
            .join(&format!("{TICKET_PATH_PREFIX}{id}/"))
            .expect("ticket path is valid")
            .to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use wiremock::{
        matchers::{body_string_contains, method, path, query_param},
        Mock, MockServer, ResponseTemplate,
    };

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

    const DETAILS: &str = r#"<main id="content">
        <div class="title-container"><h2>Chamado Interno 559298</h2>
          <div class="object-status"><span class="status status-em-atendimento ">Em atendimento</span>
          <span class="status status-error">Tempo previsto ultrapassado</span></div></div>
        <div class="accordion"><button class="accordion-button">
            1.2 Gestão | Atualização de versão
        </button><div class="accordion-body">
          <dl class="definition-list"><div class="list-item"><dt>Interessado</dt><dd><h4>Wagner Oliveira</h4></dd></div>
          <div class="list-item"><dt>Sem valor</dt></div>
          <div class="list-item"><dd>Sem rótulo</dd></div>
          <div class="list-item"><dt>Descrição</dt><dd>Atualizar para a
             versão 5.3.0<br>segunda linha<br><br>depois do vazio</dd></div></dl></div></div>
        <div data-tab="linha_tempo"><ul class="timeline">
          <li><div class="timeline-content"><h4>Adicionar comentário:</h4></div></li>
          <li><div class="timeline-date">06/10/2026 19:15:03</div><div class="timeline-content"><h4><a>Kelson</a><small>comentou:</small></h4>
            <p>Build pronto.<br>Segunda linha.</p>
            <p>Outro parágrafo.</p></div></li>
          <li><div class="timeline-date">06/10/2026 19:14:31</div></li>
        </ul></div></main>"#;

    const OPEN_FORM: &str = r#"
        <form name="busca"><input name="q" value="x"></form>
        <form method="post">
          <input type="hidden" name="csrfmiddlewaretoken" value="tok">
          <textarea name="descricao"></textarea>
          <textarea name="local_atendimento">Sala 1</textarea>
          <input name="telefone">
          <input type="number" name="ramal" value="12">
          <select name="uo"></select>
          <select name="meio_abertura"><option value="">---</option><option value="web" selected>Web</option><option value="email">Email</option></select>
          <select name="prioridade"><option>Normal</option><option>Alta</option></select>
          <select name="vazio"></select>
          <input type="radio" name="centro_atendimento" value="1" checked>
          <input type="radio" name="centro_atendimento" value="2">
          <input type="checkbox" name="enviar_copia_email">
          <input type="checkbox" name="aceite" checked>
          <input type="file" name="anexo">
          <input type="submit" value="Abrir chamado">
          <input value="sem nome">
        </form>"#;

    fn pair(name: &str, value: &str) -> (String, String) {
        (name.to_owned(), value.to_owned())
    }

    #[test]
    fn parses_open_form_defaults() {
        let fields = parse_open_form(OPEN_FORM).unwrap();
        assert_eq!(
            fields,
            [
                pair("csrfmiddlewaretoken", "tok"),
                pair("descricao", ""),
                pair("local_atendimento", "Sala 1"),
                pair("telefone", ""),
                pair("ramal", "12"),
                pair("meio_abertura", "web"),
                pair("prioridade", "Normal"),
                pair("centro_atendimento", "1"),
                pair("aceite", "on"),
            ]
        );
        assert!(parse_open_form("<form><input name=\"x\"></form>").is_none());
    }

    #[test]
    fn set_field_replaces_existing_values() {
        let mut fields = vec![pair("a", "1"), pair("b", "2"), pair("a", "3")];
        set_field(&mut fields, "a", "9");
        assert_eq!(fields, [pair("b", "2"), pair("a", "9")]);
    }

    #[test]
    fn picks_default_campus_and_center() {
        assert_eq!(
            default_campus(r#"{"campus": [[1, "A", false], [2, "B", true]]}"#).unwrap(),
            "2"
        );
        assert_eq!(
            default_campus(r#"{"campus": [[1, "A", false], [2, "B", false]]}"#).unwrap(),
            "1"
        );
        assert!(default_campus(r#"{"campus": []}"#)
            .unwrap_err()
            .to_string()
            .contains("no campus"));
        assert!(default_campus("não é json")
            .unwrap_err()
            .to_string()
            .contains("campus list"));

        assert_eq!(
            default_center(r#"{"centros": [[5, "Local", true]]}"#).unwrap(),
            "5"
        );
        assert!(default_center(r#"{"centros": []}"#)
            .unwrap_err()
            .to_string()
            .contains("no service center"));
        let several = default_center(r#"{"centros": [[5, "Local", true], [6, "Remoto", false]]}"#)
            .unwrap_err()
            .to_string();
        assert!(several.contains("5 (Local), 6 (Remoto)"));
        assert!(default_center("não é json")
            .unwrap_err()
            .to_string()
            .contains("center list"));
    }

    #[test]
    fn interprets_open_results() {
        assert_eq!(
            parse_open_result("/centralservicos/chamado/42/", "").unwrap(),
            "42"
        );
        let flash = "<li>Chamado aberto com sucesso. Número do chamado: 77 </li>";
        assert_eq!(parse_open_result("/outra/", flash).unwrap(), "77");
        let errors = r#"<ul class="errorlist"><li>Campo obrigatório.</li></ul><p class="errornote">Campo obrigatório.</p>"#;
        assert_eq!(
            parse_open_result("/abrir/", errors)
                .unwrap_err()
                .to_string(),
            "ticket source error: SUAP rejected the ticket: Campo obrigatório."
        );
        assert!(parse_open_result("/abrir/", "<p>nada</p>")
            .unwrap_err()
            .to_string()
            .contains("could not confirm"));
    }

    fn attachment(name: &str) -> Attachment {
        Attachment {
            file_name: name.to_owned(),
            bytes: b"conteudo".to_vec(),
        }
    }

    #[test]
    fn validates_attachments() {
        assert!(validate_attachments(&[
            attachment("a.PDF"),
            attachment("b.xlsx"),
            attachment("c.JpEg")
        ])
        .is_ok());
        assert!(validate_attachments(&[]).is_ok());
        let too_many = [
            attachment("1.pdf"),
            attachment("2.pdf"),
            attachment("3.pdf"),
            attachment("4.pdf"),
        ];
        assert!(validate_attachments(&too_many)
            .unwrap_err()
            .to_string()
            .contains("at most 3"));
        for name in ["virus.exe", "semextensao", "arquivo.pdf.zip"] {
            let error = validate_attachments(&[attachment(name)])
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("unsupported type") && error.contains("pdf, jpg"),
                "{name}"
            );
        }
    }

    const THREAD_PAGE: &str = r#"
        <form method="post" action="/centralservicos/adicionar_comentario/5/">
          <input type="hidden" name="csrfmiddlewaretoken" value="tok-comentario">
          <textarea name="texto"></textarea>
          <select name="usuarios_citados" multiple></select>
        </form>
        <form method="post" action="/centralservicos/adicionar_nota_interna/5/">
          <input type="hidden" name="csrfmiddlewaretoken" value="tok-nota">
          <textarea name="texto"></textarea>
        </form>"#;

    #[test]
    fn picks_the_form_by_its_action() {
        let comment =
            parse_form_by_action(THREAD_PAGE, "/centralservicos/adicionar_comentario/5/").unwrap();
        assert_eq!(
            comment,
            [
                pair("csrfmiddlewaretoken", "tok-comentario"),
                pair("texto", "")
            ]
        );
        let note = parse_form_by_action(THREAD_PAGE, "/centralservicos/adicionar_nota_interna/5/")
            .unwrap();
        assert_eq!(note[0], pair("csrfmiddlewaretoken", "tok-nota"));
        assert!(
            parse_form_by_action(THREAD_PAGE, "/centralservicos/adicionar_comentario/6/").is_none()
        );
        assert!(parse_form_by_action("<form><input name=\"x\"></form>", "/a/").is_none());
    }

    const SUSPEND_FORM: &str = r#"<form action="" method="POST">
        <input type="hidden" name="csrfmiddlewaretoken" value="tok">
        <textarea name="observacao"></textarea>
        <input type="submit" name="alterarstatuschamado_form"></form>"#;

    const RESOLVE_FORM: &str = r#"<form action="" method="POST">
        <input type="hidden" name="csrfmiddlewaretoken" value="tok">
        <input type="checkbox" name="bases_conhecimento" value="11">
        <input type="checkbox" name="bases_conhecimento" value="12">
        <input type="checkbox" name="outros_chamados_a_resolver" value="7">
        <select name="resposta_padrao"><option value="">---</option><option value="3">Padrão</option></select>
        <textarea name="comentario"></textarea></form>"#;

    const RESOLVE_FORM_WITHOUT_ARTICLES: &str = r#"<form action="" method="POST">
        <input type="hidden" name="csrfmiddlewaretoken" value="tok">
        <textarea name="comentario"></textarea></form>"#;

    #[test]
    fn finds_forms_and_choices_by_field() {
        let fields = parse_form_with_field(RESOLVE_FORM, "comentario").unwrap();
        assert_eq!(fields[0], pair("csrfmiddlewaretoken", "tok"));
        assert!(parse_form_with_field(RESOLVE_FORM, "observacao").is_none());
        assert_eq!(
            form_choices(RESOLVE_FORM, "comentario", "bases_conhecimento"),
            ["11", "12"]
        );
        assert_eq!(
            form_choices(RESOLVE_FORM, "comentario", "resposta_padrao"),
            ["", "3"]
        );
        assert!(form_choices(RESOLVE_FORM, "nada", "bases_conhecimento").is_empty());
        assert!(form_choices(
            RESOLVE_FORM_WITHOUT_ARTICLES,
            "comentario",
            "bases_conhecimento"
        )
        .is_empty());
    }

    #[tokio::test]
    async fn suspends_a_ticket_with_a_multiline_message() {
        let server = MockServer::start().await;
        mount_text(
            &server,
            "GET",
            "/centralservicos/suspender_chamado/5/",
            SUSPEND_FORM,
        )
        .await;
        Mock::given(method("POST"))
            .and(path("/centralservicos/suspender_chamado/5/"))
            .and(body_string_contains("csrfmiddlewaretoken=tok"))
            .and(body_string_contains("observacao=aguardando%0Aresposta"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;
        let (_dir, client) = client_for(&server).await;
        let source = SuapTicketSource::new(&client, TicketQueue::Support);
        source
            .suspend_ticket("5", "aguardando\nresposta\n")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn suspend_reports_failures() {
        let server = MockServer::start().await;
        mount_text(
            &server,
            "GET",
            "/centralservicos/suspender_chamado/5/",
            SUSPEND_FORM,
        )
        .await;
        mount_text(
            &server,
            "GET",
            "/centralservicos/suspender_chamado/6/",
            "<p>sem formulário</p>",
        )
        .await;
        mount_text(
            &server,
            "POST",
            "/centralservicos/suspender_chamado/5/",
            "<p class='alert-error'>Não pode suspender</p>",
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/centralservicos/suspender_chamado/7/"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let (_dir, client) = client_for(&server).await;
        let source = SuapTicketSource::new(&client, TicketQueue::Support);
        let rejected = source
            .suspend_ticket("5", "x")
            .await
            .unwrap_err()
            .to_string();
        assert!(rejected.contains("SUAP rejected the suspension: Não pode suspender"));
        let no_form = source
            .suspend_ticket("6", "x")
            .await
            .unwrap_err()
            .to_string();
        assert!(no_form.contains("ticket 6 has no suspended form"));
        let forbidden = source
            .suspend_ticket("7", "x")
            .await
            .unwrap_err()
            .to_string();
        assert!(
            forbidden.contains("ticket 7 cannot be suspended (unexpected status 403 Forbidden)")
        );
        assert!(source
            .suspend_ticket("5", "  ")
            .await
            .unwrap_err()
            .to_string()
            .contains("suspension text is empty"));
        assert!(source.suspend_ticket("x", "x").await.is_err());
    }

    #[tokio::test]
    async fn status_change_refused_with_an_http_error_is_explained() {
        let server = MockServer::start().await;
        mount_text(
            &server,
            "GET",
            "/centralservicos/resolver_chamado/5/",
            RESOLVE_FORM_WITHOUT_ARTICLES,
        )
        .await;
        Mock::given(method("POST"))
            .and(path("/centralservicos/resolver_chamado/5/"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let (_dir, client) = client_for(&server).await;
        let source = SuapTicketSource::new(&client, TicketQueue::Support);
        let resolution = Resolution {
            text: "ok".to_owned(),
            ..Resolution::default()
        };
        let error = source
            .resolve_ticket("5", &resolution)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("SUAP refused the resolution (unexpected status 403 Forbidden)"));
    }

    #[tokio::test]
    async fn resolves_with_the_first_article_by_default() {
        let server = MockServer::start().await;
        mount_text(
            &server,
            "GET",
            "/centralservicos/resolver_chamado/5/",
            RESOLVE_FORM,
        )
        .await;
        Mock::given(method("POST"))
            .and(path("/centralservicos/resolver_chamado/5/"))
            .and(body_string_contains("bases_conhecimento=11"))
            .and(body_string_contains("comentario=resolvido%0Acom+sucesso"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;
        let (_dir, client) = client_for(&server).await;
        let source = SuapTicketSource::new(&client, TicketQueue::Support);
        let resolution = Resolution {
            text: "resolvido\ncom sucesso".to_owned(),
            ..Resolution::default()
        };
        source.resolve_ticket("5", &resolution).await.unwrap();
        let received = server.received_requests().await.unwrap();
        let body = String::from_utf8_lossy(&received.last().unwrap().body).into_owned();
        assert!(!body.contains("bases_conhecimento=12"));
    }

    #[tokio::test]
    async fn resolves_with_chosen_articles_other_tickets_and_standard_reply() {
        let server = MockServer::start().await;
        mount_text(
            &server,
            "GET",
            "/centralservicos/resolver_chamado/5/",
            RESOLVE_FORM,
        )
        .await;
        Mock::given(method("POST"))
            .and(path("/centralservicos/resolver_chamado/5/"))
            .and(body_string_contains("bases_conhecimento=12"))
            .and(body_string_contains("outros_chamados_a_resolver=7"))
            .and(body_string_contains("resposta_padrao=3"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;
        let (_dir, client) = client_for(&server).await;
        let source = SuapTicketSource::new(&client, TicketQueue::Support);
        let resolution = Resolution {
            text: "ok".to_owned(),
            articles: vec!["12".to_owned()],
            also: vec!["7".to_owned()],
            standard_reply: Some("3".to_owned()),
        };
        source.resolve_ticket("5", &resolution).await.unwrap();
    }

    #[tokio::test]
    async fn resolves_forms_that_offer_no_articles() {
        let server = MockServer::start().await;
        mount_text(
            &server,
            "GET",
            "/centralservicos/resolver_chamado/5/",
            RESOLVE_FORM_WITHOUT_ARTICLES,
        )
        .await;
        mount_text(
            &server,
            "POST",
            "/centralservicos/resolver_chamado/5/",
            "ok",
        )
        .await;
        let (_dir, client) = client_for(&server).await;
        let source = SuapTicketSource::new(&client, TicketQueue::Support);
        let resolution = Resolution {
            text: "ok".to_owned(),
            ..Resolution::default()
        };
        source.resolve_ticket("5", &resolution).await.unwrap();
        let with_article = Resolution {
            articles: vec!["1".to_owned()],
            ..resolution
        };
        let error = source
            .resolve_ticket("5", &with_article)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("offers no articles"));
    }

    #[tokio::test]
    async fn resolve_reports_failures() {
        let server = MockServer::start().await;
        mount_text(
            &server,
            "GET",
            "/centralservicos/resolver_chamado/5/",
            RESOLVE_FORM,
        )
        .await;
        mount_text(
            &server,
            "POST",
            "/centralservicos/resolver_chamado/5/",
            r#"<ul class="errorlist"><li>Comentário inválido.</li></ul>"#,
        )
        .await;
        let (_dir, client) = client_for(&server).await;
        let source = SuapTicketSource::new(&client, TicketQueue::Support);
        let unknown = Resolution {
            text: "ok".to_owned(),
            articles: vec!["99".to_owned()],
            ..Resolution::default()
        };
        let error = source
            .resolve_ticket("5", &unknown)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("article 99 is not offered; available: 11, 12"));
        let rejected = Resolution {
            text: "ok".to_owned(),
            ..Resolution::default()
        };
        let error = source
            .resolve_ticket("5", &rejected)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("SUAP rejected the resolution: Comentário inválido."));
        let empty = Resolution::default();
        assert!(source
            .resolve_ticket("5", &empty)
            .await
            .unwrap_err()
            .to_string()
            .contains("resolution text is empty"));
    }

    #[test]
    fn collects_flash_and_form_errors_without_repeats() {
        let page = r#"<p class="alert-error">Sem permissão <button>Fechar</button></p>
            <ul class="errorlist"><li>Campo obrigatório.</li></ul>
            <p class="errornote">Campo obrigatório.</p>"#;
        assert_eq!(page_errors(page), ["Sem permissão", "Campo obrigatório."]);
        assert!(page_errors("<p>ok</p>").is_empty());
    }

    #[tokio::test]
    async fn adds_comments_and_internal_notes() {
        let server = MockServer::start().await;
        mount_text(&server, "GET", "/centralservicos/chamado/5/", THREAD_PAGE).await;
        Mock::given(method("POST"))
            .and(path("/centralservicos/adicionar_comentario/5/"))
            .and(body_string_contains("csrfmiddlewaretoken=tok-comentario"))
            .and(body_string_contains("texto=linha+1%0Alinha+2"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/centralservicos/adicionar_nota_interna/5/"))
            .and(body_string_contains("csrfmiddlewaretoken=tok-nota"))
            .and(body_string_contains("texto=nota+interna"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;
        let (_dir, client) = client_for(&server).await;
        let source = SuapTicketSource::new(&client, TicketQueue::Support);
        source
            .add_message("5", Message::Comment, "linha 1\nlinha 2\n")
            .await
            .unwrap();
        source
            .add_message("5", Message::InternalNote, "nota interna")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn add_message_reports_failures() {
        let server = MockServer::start().await;
        mount_text(&server, "GET", "/centralservicos/chamado/5/", THREAD_PAGE).await;
        mount_text(
            &server,
            "GET",
            "/centralservicos/chamado/6/",
            "<p>sem formulários</p>",
        )
        .await;
        mount_text(
            &server,
            "POST",
            "/centralservicos/adicionar_comentario/5/",
            r#"<ul class="errorlist"><li>Texto inválido.</li></ul>"#,
        )
        .await;
        mount_text(
            &server,
            "POST",
            "/centralservicos/adicionar_nota_interna/5/",
            "<p class='alert-error'>Sem permissão</p>",
        )
        .await;
        let (_dir, client) = client_for(&server).await;
        let source = SuapTicketSource::new(&client, TicketQueue::Support);

        let rejected = source
            .add_message("5", Message::Comment, "x")
            .await
            .unwrap_err();
        assert_eq!(
            rejected.to_string(),
            "ticket source error: SUAP rejected the comment: Texto inválido."
        );
        let refused = source
            .add_message("5", Message::InternalNote, "x")
            .await
            .unwrap_err();
        assert!(refused
            .to_string()
            .contains("rejected the internal note: Sem permissão"));
        let no_form = source
            .add_message("6", Message::Comment, "x")
            .await
            .unwrap_err();
        assert!(no_form.to_string().contains("ticket 6 has no comment form"));
        let empty = source
            .add_message("5", Message::Comment, " \n")
            .await
            .unwrap_err();
        assert!(empty.to_string().contains("the comment text is empty"));
        assert!(source
            .add_message("x", Message::Comment, "x")
            .await
            .is_err());
        let missing = source.add_message("404", Message::Comment, "x").await;
        assert!(matches!(
            missing,
            Err(TicketError::Suap(SuapError::Transport(_)))
        ));
    }

    #[tokio::test]
    async fn cancels_a_ticket_with_a_multiline_reason() {
        let server = MockServer::start().await;
        mount_text(
            &server,
            "GET",
            "/centralservicos/cancelar_chamado/5/",
            SUSPEND_FORM,
        )
        .await;
        Mock::given(method("POST"))
            .and(path("/centralservicos/cancelar_chamado/5/"))
            .and(body_string_contains("csrfmiddlewaretoken=tok"))
            .and(body_string_contains(
                "observacao=aberto+por+engano%0Adesculpe",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;
        mount_text(
            &server,
            "GET",
            "/centralservicos/cancelar_chamado/6/",
            "<p>sem formulário</p>",
        )
        .await;
        mount_text(
            &server,
            "GET",
            "/centralservicos/cancelar_chamado/8/",
            SUSPEND_FORM,
        )
        .await;
        mount_text(
            &server,
            "POST",
            "/centralservicos/cancelar_chamado/8/",
            "<p class='alert-error'>Não pode cancelar</p>",
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/centralservicos/cancelar_chamado/7/"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let (_dir, client) = client_for(&server).await;
        let source = SuapTicketSource::new(&client, TicketQueue::Support);
        source
            .cancel_ticket("5", "aberto por engano\ndesculpe\n")
            .await
            .unwrap();
        let rejected = source.cancel_ticket("8", "x").await.unwrap_err();
        assert!(rejected
            .to_string()
            .contains("SUAP rejected the cancellation: Não pode cancelar"));
        let no_form = source.cancel_ticket("6", "x").await.unwrap_err();
        assert!(no_form
            .to_string()
            .contains("ticket 6 has no cancelled form"));
        let forbidden = source.cancel_ticket("7", "x").await.unwrap_err();
        assert!(forbidden
            .to_string()
            .contains("ticket 7 cannot be cancelled (unexpected status 403 Forbidden)"));
        let empty = source.cancel_ticket("5", "  ").await.unwrap_err();
        assert!(empty.to_string().contains("the cancellation text is empty"));
        assert!(source.cancel_ticket("x", "x").await.is_err());
    }

    #[test]
    fn extracts_flash_errors_without_button_text() {
        let page = r#"<p class="x alert-error">Não pode. <button>Fechar</button></p>
            <p class="alert-error"><button>Fechar</button></p>
            <p class="alert-success">Feito</p>"#;
        assert_eq!(flash_errors(page), ["Não pode."]);
        assert!(flash_errors("<p>nada</p>").is_empty());
    }

    #[tokio::test]
    async fn assumes_and_starts_service() {
        let server = MockServer::start().await;
        mount_text(
            &server,
            "GET",
            "/centralservicos/auto_atribuir_chamado/5/",
            "<p class='alert-success'>ok</p>",
        )
        .await;
        mount_text(
            &server,
            "GET",
            "/centralservicos/colocar_em_atendimento/5/",
            "ok",
        )
        .await;
        mount_text(
            &server,
            "GET",
            "/centralservicos/auto_atribuir_chamado/6/",
            "<p class='alert-error'>Já resolvido <button>Fechar</button></p>",
        )
        .await;
        let (_dir, client) = client_for(&server).await;
        let source = SuapTicketSource::new(&client, TicketQueue::Support);
        source.assume_ticket("5").await.unwrap();
        source.start_service("5").await.unwrap();
        let refused = source.assume_ticket("6").await.unwrap_err().to_string();
        assert_eq!(
            refused,
            "ticket source error: SUAP refused the action: Já resolvido"
        );
        assert!(matches!(
            source.start_service("x").await,
            Err(TicketError::Source(_))
        ));
        let missing = source.assume_ticket("404").await.unwrap_err().to_string();
        assert!(
            missing.contains("SUAP refused the action (unexpected status 404"),
            "{missing}"
        );
    }

    #[tokio::test]
    async fn opens_ticket_with_attachments_and_email_copy() {
        let server = MockServer::start().await;
        mount_text(
            &server,
            "GET",
            "/centralservicos/abrir_chamado/7/",
            OPEN_FORM,
        )
        .await;
        Mock::given(method("POST"))
            .and(path("/centralservicos/abrir_chamado/7/"))
            .and(body_string_contains("name=\"enviar_copia_email\""))
            .and(body_string_contains(
                "name=\"chamadoanexo_set-0-descricao\"",
            ))
            .and(body_string_contains("relatorio.pdf"))
            .and(body_string_contains(
                "name=\"chamadoanexo_set-0-anexo\"; filename=\"relatorio.pdf\"",
            ))
            .and(body_string_contains(
                "name=\"chamadoanexo_set-1-anexo\"; filename=\"foto.png\"",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string("Número do chamado: 31"))
            .mount(&server)
            .await;
        let (_dir, client) = client_for(&server).await;
        let source = SuapTicketSource::new(&client, TicketQueue::Support);
        let ticket = NewTicket {
            campus: Some("1".to_owned()),
            center: Some("2".to_owned()),
            copy_email: true,
            attachments: vec![attachment("relatorio.pdf"), attachment("foto.png")],
            ..new_ticket()
        };
        assert_eq!(source.open_ticket(&ticket).await.unwrap(), "31");
    }

    #[tokio::test]
    async fn open_ticket_rejects_bad_attachments_before_any_request() {
        let server = MockServer::start().await;
        let (_dir, client) = client_for(&server).await;
        let source = SuapTicketSource::new(&client, TicketQueue::Support);
        let ticket = NewTicket {
            attachments: vec![attachment("virus.exe")],
            ..new_ticket()
        };
        assert!(source
            .open_ticket(&ticket)
            .await
            .unwrap_err()
            .to_string()
            .contains("unsupported type"));
    }

    #[tokio::test]
    async fn open_without_email_copy_drops_the_form_default() {
        let server = MockServer::start().await;
        let form = OPEN_FORM.replace(
            "name=\"aceite\" checked",
            "name=\"enviar_copia_email\" checked",
        );
        mount_text(&server, "GET", "/centralservicos/abrir_chamado/7/", &form).await;
        mount_text(
            &server,
            "POST",
            "/centralservicos/abrir_chamado/7/",
            "Número do chamado: 32",
        )
        .await;
        let (_dir, client) = client_for(&server).await;
        let source = SuapTicketSource::new(&client, TicketQueue::Support);
        let ticket = NewTicket {
            campus: Some("1".to_owned()),
            center: Some("2".to_owned()),
            ..new_ticket()
        };
        assert_eq!(source.open_ticket(&ticket).await.unwrap(), "32");
        let received = server.received_requests().await.unwrap();
        let post = received
            .iter()
            .find(|request| request.method == wiremock::http::Method::POST)
            .unwrap();
        assert!(!String::from_utf8_lossy(&post.body).contains("enviar_copia_email"));
    }

    async fn mount_text(server: &MockServer, verb: &str, request_path: &str, body: &str) {
        Mock::given(method(verb))
            .and(path(request_path.to_owned()))
            .respond_with(ResponseTemplate::new(200).set_body_string(body.to_owned()))
            .mount(server)
            .await;
    }

    fn new_ticket() -> NewTicket {
        NewTicket {
            service_id: 7,
            description: "Teste".to_owned(),
            ..NewTicket::default()
        }
    }

    #[tokio::test]
    async fn opens_ticket_using_suap_defaults() {
        let server = MockServer::start().await;
        mount_text(
            &server,
            "GET",
            "/centralservicos/abrir_chamado/7/",
            OPEN_FORM,
        )
        .await;
        mount_text(
            &server,
            "GET",
            "/centralservicos/get_campus_com_centros_atendimento/7/0/",
            r#"{"campus": [[3, "ZL", true]]}"#,
        )
        .await;
        mount_text(
            &server,
            "GET",
            "/centralservicos/get_centros_atendimento_por_servico_e_campus/7/3/",
            r#"{"centros": [[9, "TI", true]]}"#,
        )
        .await;
        Mock::given(method("POST"))
            .and(path("/centralservicos/abrir_chamado/7/"))
            .and(body_string_contains("descricao=Teste"))
            .and(body_string_contains("uo=3"))
            .and(body_string_contains("centro_atendimento=9"))
            .and(body_string_contains("csrfmiddlewaretoken=tok"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", "/centralservicos/chamado/99/"),
            )
            .mount(&server)
            .await;
        mount_text(&server, "GET", "/centralservicos/chamado/99/", "ok").await;
        let (_dir, client) = client_for(&server).await;
        let source = SuapTicketSource::new(&client, TicketQueue::Support);
        assert_eq!(source.open_ticket(&new_ticket()).await.unwrap(), "99");
    }

    #[tokio::test]
    async fn opens_ticket_with_explicit_values() {
        let server = MockServer::start().await;
        mount_text(
            &server,
            "GET",
            "/centralservicos/abrir_chamado/7/",
            OPEN_FORM,
        )
        .await;
        Mock::given(method("POST"))
            .and(path("/centralservicos/abrir_chamado/7/"))
            .and(body_string_contains("uo=4"))
            .and(body_string_contains("centro_atendimento=8"))
            .and(body_string_contains("interessado=55"))
            .and(body_string_contains("telefone=9999"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("Chamado aberto com sucesso. Número do chamado:123"),
            )
            .mount(&server)
            .await;
        let (_dir, client) = client_for(&server).await;
        let source = SuapTicketSource::new(&client, TicketQueue::Support);
        let ticket = NewTicket {
            campus: Some("4".to_owned()),
            center: Some("8".to_owned()),
            interested: Some("55".to_owned()),
            extra_fields: vec![pair("telefone", "9999")],
            ..new_ticket()
        };
        assert_eq!(source.open_ticket(&ticket).await.unwrap(), "123");
    }

    #[tokio::test]
    async fn open_ticket_reports_failures() {
        let server = MockServer::start().await;
        mount_text(
            &server,
            "GET",
            "/centralservicos/abrir_chamado/7/",
            OPEN_FORM,
        )
        .await;
        mount_text(
            &server,
            "GET",
            "/centralservicos/abrir_chamado/8/",
            "<p>sem formulário</p>",
        )
        .await;
        mount_text(
            &server,
            "POST",
            "/centralservicos/abrir_chamado/7/",
            r#"<ul class="errorlist"><li>Descrição inválida.</li></ul>"#,
        )
        .await;
        let (_dir, client) = client_for(&server).await;
        let source = SuapTicketSource::new(&client, TicketQueue::Support);

        let no_form = NewTicket {
            service_id: 8,
            ..new_ticket()
        };
        assert!(source
            .open_ticket(&no_form)
            .await
            .unwrap_err()
            .to_string()
            .contains("no ticket form"));

        let missing_campus_list = source.open_ticket(&new_ticket()).await;
        assert!(matches!(
            missing_campus_list,
            Err(TicketError::Suap(SuapError::Transport(_)))
        ));

        let explicit_campus = NewTicket {
            campus: Some("1".to_owned()),
            ..new_ticket()
        };
        let missing_center_list = source.open_ticket(&explicit_campus).await;
        assert!(matches!(
            missing_center_list,
            Err(TicketError::Suap(SuapError::Transport(_)))
        ));

        let explicit_all = NewTicket {
            center: Some("2".to_owned()),
            ..explicit_campus
        };
        let rejected = source
            .open_ticket(&explicit_all)
            .await
            .unwrap_err()
            .to_string();
        assert!(rejected.contains("Descrição inválida."));
    }

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
        assert_eq!(
            (
                tickets[1].id.as_str(),
                tickets[1].subject.clone(),
                tickets[1].status.clone()
            ),
            ("1", None, None)
        );
    }

    #[test]
    fn parses_ticket_details() {
        let details = parse_ticket_details(DETAILS, &base(), "559298").unwrap();
        assert_eq!(details.title, "Chamado Interno 559298");
        assert_eq!(
            details.heading.as_deref(),
            Some("1.2 Gestão | Atualização de versão")
        );
        assert_eq!(
            details.statuses,
            ["Em atendimento", "Tempo previsto ultrapassado"]
        );
        assert_eq!(
            details.fields,
            [
                ("Interessado".to_owned(), "Wagner Oliveira".to_owned()),
                (
                    "Descrição".to_owned(),
                    "Atualizar para a versão 5.3.0\nsegunda linha\n\ndepois do vazio".to_owned()
                ),
            ]
        );
        assert_eq!(
            details.timeline,
            [TimelineEntry {
                date: "06/10/2026 19:15:03".to_owned(),
                text: "Kelson comentou: Build pronto.\nSegunda linha.\n\nOutro parágrafo."
                    .to_owned()
            }]
        );
        assert_eq!(
            details.details_url,
            "https://suap.example/centralservicos/chamado/559298/"
        );
        assert_eq!(details.clone(), details);
    }

    const ATTACHED: &str = r#"<main id="content"><div class="title-container"><h2>Chamado 3</h2></div>
        <div data-tab="linha_tempo"><ul class="timeline">
          <li><div class="timeline-date">1</div><div class="timeline-content"><p>Ana adicionou o seguinte anexo:
            <a href="/djtools/arquivo/centralservicos/chamadoanexo/1/anexo/" target="_blank">dados.csv</a></p></div></li>
          <li><div class="timeline-date">2</div><div class="timeline-content"><p>de novo
            <a href="/djtools/arquivo/centralservicos/chamadoanexo/1/anexo/">dados.csv</a>
            <a href="/djtools/arquivo/centralservicos/chamadoanexo/2/anexo/">foto.png</a>
            <a href="/rh/servidor/2080882/">Ana</a> <a href="https://fora.example/x.pdf">fora</a> <a>sem href</a></p></div></li>
        </ul></div></main>"#;

    #[test]
    fn attachments_are_the_distinct_ticket_files_linked_in_the_timeline() {
        let details = parse_ticket_details(ATTACHED, &base(), "3").unwrap();
        assert_eq!(
            details.attachments,
            [
                AttachmentLink {
                    name: "dados.csv".to_owned(),
                    path: "/djtools/arquivo/centralservicos/chamadoanexo/1/anexo/".to_owned(),
                },
                AttachmentLink {
                    name: "foto.png".to_owned(),
                    path: "/djtools/arquivo/centralservicos/chamadoanexo/2/anexo/".to_owned(),
                },
            ]
        );
        let plain = parse_ticket_details(DETAILS, &base(), "559298").unwrap();
        assert!(plain.attachments.is_empty());
    }

    #[tokio::test]
    async fn downloads_only_ticket_attachments_following_the_redirect() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/djtools/arquivo/centralservicos/chamadoanexo/1/anexo/",
            ))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/media/x.csv"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/media/x.csv"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0u8, 159, 146, 150, 10]))
            .mount(&server)
            .await;
        let (_dir, client) = client_for(&server).await;
        let source = SuapTicketSource::new(&client, TicketQueue::Support);
        let link = |path: &str| AttachmentLink {
            name: "x".to_owned(),
            path: path.to_owned(),
        };
        let bytes = source
            .download_attachment(&link(
                "/djtools/arquivo/centralservicos/chamadoanexo/1/anexo/",
            ))
            .await
            .unwrap();
        assert_eq!(
            bytes,
            [0, 159, 146, 150, 10],
            "binary content is kept as is"
        );
        // Anything else (another host, another SUAP path) is refused without a request.
        for foreign in [
            "https://fora.example/x.pdf",
            "/rh/servidor/1/",
            "/djtools/arquivo/outro/",
        ] {
            let refused = source
                .download_attachment(&link(foreign))
                .await
                .unwrap_err();
            assert!(
                refused.to_string().contains("not a ticket attachment link"),
                "{foreign}"
            );
        }
        let missing = link("/djtools/arquivo/centralservicos/chamadoanexo/9/anexo/");
        assert!(source.download_attachment(&missing).await.is_err());
    }

    #[test]
    fn details_of_minimal_page_have_no_optional_parts() {
        let html =
            r#"<main id="content"><div class="title-container"><h2>Chamado</h2></div></main>"#;
        let details = parse_ticket_details(html, &base(), "1").unwrap();
        assert!(details.heading.is_none() && details.statuses.is_empty());
        assert!(details.fields.is_empty() && details.timeline.is_empty());
    }

    #[test]
    fn non_ticket_page_has_no_details() {
        assert!(parse_ticket_details("<p>nada</p>", &base(), "1").is_none());
    }

    #[test]
    fn page_without_tickets_is_empty() {
        assert!(parse_ticket_list("<p>nada</p>", &base()).is_empty());
    }

    #[test]
    fn queues_have_distinct_paths() {
        assert!(TicketQueue::Support
            .path()
            .contains("listar_chamados_suporte"));
        assert!(TicketQueue::Mine.path().contains("meus_chamados"));
    }

    #[test]
    fn errors_are_displayed() {
        assert_eq!(
            TicketError::NotImplemented.to_string(),
            "ticket source is not implemented yet"
        );
        assert_eq!(
            TicketError::Source("x".to_owned()).to_string(),
            "ticket source error: x"
        );
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
        let paths =
            suap_core::AppPaths::from_dirs(directory.path().join("c"), directory.path().join("d"));
        let config = suap_core::SuapConfig {
            base_url: Url::parse(&format!("{}/", server.uri())).unwrap(),
            ..suap_core::SuapConfig::default()
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
    }

    fn listing_of(ids: &[u32]) -> String {
        let boxes: Vec<String> = ids
            .iter()
            .map(|id| {
                format!(r#"<div class="general-box"><h4><a href="/centralservicos/chamado/{id}/">REQ #{id}</a></h4></div>"#)
            })
            .collect();
        boxes.concat()
    }

    #[tokio::test]
    async fn filtered_listings_send_the_options_and_follow_the_pages() {
        let server = MockServer::start().await;
        // The most specific mocks first: the first one that matches answers.
        let pages = [
            (Some("2"), vec![3]),
            (Some("3"), vec![3]),
            (None, vec![1, 2]),
        ];
        for (page, ids) in pages {
            let mut mock = Mock::given(method("GET"))
                .and(path("/centralservicos/listar_chamados_suporte/"))
                .and(query_param("status", "1"))
                .and(query_param("texto", "moodle"));
            mock = match page {
                Some(page) => mock.and(query_param("page", page)),
                None => mock,
            };
            mock.respond_with(ResponseTemplate::new(200).set_body_string(listing_of(&ids)))
                .mount(&server)
                .await;
        }
        let (_dir, client) = client_for(&server).await;
        let source = SuapTicketSource::new(&client, TicketQueue::Support);
        let ids = |tickets: Vec<RemoteTicket>| -> Vec<String> {
            tickets.into_iter().map(|ticket| ticket.id).collect()
        };
        let mut filter = TicketFilter {
            statuses: vec!["aberto".to_owned()],
            text: Some("moodle".to_owned()),
            ..TicketFilter::default()
        };
        // One page: the first by default, or the one asked for.
        assert_eq!(
            ids(source.list_filtered(&filter).await.unwrap()),
            ["1", "2"]
        );
        filter.page = Some(2);
        assert_eq!(ids(source.list_filtered(&filter).await.unwrap()), ["3"]);
        // Every page, stopping when a page brings nothing new.
        filter.all_pages = true;
        assert_eq!(
            ids(source.list_filtered(&filter).await.unwrap()),
            ["1", "2", "3"]
        );
        // A filter SUAP cannot take is refused before any request.
        let wrong = TicketFilter {
            relation: Some("algum".to_owned()),
            ..TicketFilter::default()
        };
        assert!(source.list_filtered(&wrong).await.is_err());
    }

    #[tokio::test]
    async fn source_gets_ticket_details_and_validates_input() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/centralservicos/chamado/559298/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(DETAILS))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/centralservicos/chamado/7/"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<p>outra coisa</p>"))
            .mount(&server)
            .await;
        let (_dir, client) = client_for(&server).await;
        let source = SuapTicketSource::new(&client, TicketQueue::Support);
        assert_eq!(
            source.get_ticket("559298").await.unwrap().title,
            "Chamado Interno 559298"
        );
        assert!(matches!(
            source.get_ticket("7").await,
            Err(TicketError::Source(_))
        ));
        assert!(matches!(
            source.get_ticket("").await,
            Err(TicketError::Source(_))
        ));
        assert!(matches!(
            source.get_ticket("../x").await,
            Err(TicketError::Source(_))
        ));
        assert!(matches!(
            source.get_ticket("404").await,
            Err(TicketError::Suap(SuapError::Transport(_)))
        ));
    }

    #[tokio::test]
    async fn source_reports_unauthenticated_session() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/centralservicos/listar_chamados_suporte/"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/accounts/login/"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/accounts/login/"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let (_dir, client) = client_for(&server).await;
        let source = SuapTicketSource::new(&client, TicketQueue::Support);
        assert!(matches!(
            source.list_tickets().await,
            Err(TicketError::Suap(SuapError::NotAuthenticated))
        ));
    }
}
