//! Actions on a ticket beyond its status: handing it to another attendant (assign, escalate,
//! return), reclassifying it, and managing its tags and other interested people.
//!
//! Like the status changes, each action opens the SUAP form, fills what the user chose and posts it
//! back, so SUAP keeps applying its own permission rules. People, tags and centers are chosen by the
//! words a user knows (a registration number, a name, a tag name); the form's own options are the
//! source of truth, and a choice that is missing or ambiguous is reported together with the options.

use scraper::{node::Node, ElementRef, Html};
use serde::Deserialize;
use url::form_urlencoded::Serializer;

use suap_core::FormFile;

use crate::{
    flat_text, parse_form_by_action, selector, set_field, validate_attachments, Attachment,
    SuapTicketSource, TicketError, ATTACHMENT_DESCRIPTION_MAX, CAMPUS_PATH_PREFIX,
    CENTERS_PATH_PREFIX, TICKET_PATH_PREFIX,
};

const ATTACH_PATH_PREFIX: &str = "/centralservicos/adicionar_anexo/";
const ASSIGN_PATH_PREFIX: &str = "/centralservicos/atribuir_chamado/";
const ESCALATE_PATH_PREFIX: &str = "/centralservicos/escalar_atendimento_chamado/";
const RETURN_PATH_PREFIX: &str = "/centralservicos/retornar_atendimento_chamado/";
const RECLASSIFY_PATH_PREFIX: &str = "/centralservicos/reclassificar_chamado/";
const ADD_TAGS_PATH_PREFIX: &str = "/centralservicos/adicionar_tags_ao_chamado/";
const REMOVE_TAG_PATH_PREFIX: &str = "/centralservicos/remover_tag_do_chamado/";
const ADD_INTERESTED_PATH_PREFIX: &str = "/centralservicos/adicionar_outros_interessados/";
const REMOVE_INTERESTED_PATH_PREFIX: &str = "/centralservicos/remover_outros_interessados/";
const PEOPLE_SEARCH_PATH: &str = "/json/comum/vinculo/";

/// An option a SUAP form offers: what is submitted and what a person reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub value: String,
    pub label: String,
}

impl Choice {
    fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
        }
    }

    fn describe(&self) -> String {
        format!("{} ({})", self.value, self.label)
    }
}

/// Which way a ticket moves between attendance groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// To the group above.
    Escalate,
    /// Back to the group below.
    Return,
}

/// What to change when reclassifying a ticket; anything left out stays as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reclassification {
    pub service: Option<String>,
    pub campus: Option<String>,
    pub center: Option<String>,
}

/// The options of the control called `name` in the form that has a control named `anchor`:
/// `<option>`s of a select, or checkboxes/radios (labelled by the `<label>` around them).
pub(crate) fn form_choices_with_labels(html: &str, anchor: &str, name: &str) -> Vec<Choice> {
    let document = Html::parse_document(html);
    let (forms, anchor) = (selector("form"), selector(&format!("[name={anchor}]")));
    let Some(form) = document
        .select(&forms)
        .find(|form| form.select(&anchor).next().is_some())
    else {
        return Vec::new();
    };
    let options = selector(&format!("select[name={name}] option"));
    let inputs = selector(&format!("input[name={name}]"));
    let mut choices: Vec<Choice> = form
        .select(&options)
        .filter_map(|option| {
            let value = option.value().attr("value")?;
            Some(Choice::new(value, flat_text(option)))
        })
        .collect();
    for input in form.select(&inputs) {
        let Some(value) = input.value().attr("value") else {
            continue;
        };
        let label = input
            .ancestors()
            .filter_map(ElementRef::wrap)
            .find(|ancestor| ancestor.value().name() == "label")
            .map_or_else(|| value.to_owned(), flat_text);
        choices.push(Choice::new(value, label));
    }
    choices.retain(|choice| !choice.value.is_empty());
    choices
}

/// The choice the user meant by `wanted`: its value, its whole label or, failing both, the only
/// label that contains it (ignoring case).
pub fn pick_choice<'a>(
    choices: &'a [Choice],
    wanted: &str,
    what: &str,
) -> Result<&'a Choice, TicketError> {
    let wanted = wanted.trim();
    if wanted.is_empty() {
        return Err(TicketError::Source(format!("the {what} name is empty")));
    }
    if choices.is_empty() {
        return Err(TicketError::Source(format!(
            "there is no {what} to choose from"
        )));
    }
    let lower = wanted.to_lowercase();
    let exact: Vec<&Choice> = choices
        .iter()
        .filter(|choice| choice.value == wanted || choice.label.to_lowercase() == lower)
        .collect();
    let found = if exact.len() == 1 {
        exact
    } else {
        let partial = |choice: &&Choice| choice.label.to_lowercase().contains(&lower);
        choices.iter().filter(partial).collect()
    };
    let list = |items: &[&Choice]| {
        let described: Vec<String> = items.iter().map(|choice| choice.describe()).collect();
        described.join(", ")
    };
    match found.as_slice() {
        [only] => Ok(only),
        [] => {
            let all: Vec<&Choice> = choices.iter().collect();
            Err(TicketError::Source(format!(
                "no {what} matches {wanted:?}; available: {}",
                list(&all)
            )))
        }
        several => Err(TicketError::Source(format!(
            "{wanted:?} matches several {what}s, be more specific: {}",
            list(several)
        ))),
    }
}

#[derive(Deserialize)]
struct PeopleReply {
    items: Vec<PersonItem>,
}

#[derive(Deserialize)]
struct PersonItem {
    id: u64,
    html: String,
}

/// The people found by SUAP's person search, as `(link id, name)`.
fn parse_people(json: &str) -> Result<Vec<Choice>, TicketError> {
    let reply: PeopleReply = serde_json::from_str(json)
        .map_err(|error| TicketError::Source(format!("person search: {error}")))?;
    let title = selector(".title");
    Ok(reply
        .items
        .into_iter()
        .map(|item| {
            let card = Html::parse_fragment(&item.html);
            let name = card
                .select(&title)
                .next()
                .map_or_else(String::new, flat_text);
            Choice::new(item.id.to_string(), name)
        })
        .collect())
}

/// The search token SUAP embeds in the page next to the person field.
fn search_control(html: &str) -> Option<&str> {
    let start = html.find("control: '")? + "control: '".len();
    let length = html[start..].find('\'')?;
    Some(&html[start..start + length])
}

/// The forms of a ticket page that remove something (their `action` starts with `prefix`), each
/// with the id at the end of the action, what the person reads about it and the action itself.
fn removal_forms(html: &str, prefix: &str, container: &str) -> Vec<(Choice, String)> {
    let document = Html::parse_document(html);
    let mut found = Vec::new();
    for form in document.select(&selector("form")) {
        let Some(action) = form.value().attr("action") else {
            continue;
        };
        let Some(rest) = action.strip_prefix(prefix) else {
            continue;
        };
        let id = rest
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(rest);
        let around =
            form.ancestors()
                .filter_map(ElementRef::wrap)
                .find(|ancestor| match container {
                    "li" => ancestor.value().name() == "li",
                    other => ancestor.value().classes().any(|class| class == other),
                });
        let label = around.map_or_else(String::new, |around| removal_label(around, container));
        found.push((Choice::new(id, label), action.to_owned()));
    }
    found
}

/// What names the item being removed: a tag's text, or a person's name with the registration
/// number from the link to their record.
fn removal_label(around: ElementRef<'_>, container: &str) -> String {
    if container == "li" {
        // The item's own words, without the text of the button that removes it.
        let mut words = Vec::new();
        for child in around.children() {
            match (child.value(), ElementRef::wrap(child)) {
                (Node::Text(text), _) => words.push(text.to_string()),
                (_, Some(element)) if element.value().name() != "form" => {
                    words.push(flat_text(element));
                }
                _ => {}
            }
        }
        return words
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
    }
    let link = selector(".popup-user a, dd a");
    around
        .select(&link)
        .next()
        .map_or_else(String::new, |link| {
            let record = link.value().attr("href").unwrap_or_default();
            let number = record
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or_default();
            format!("{} ({number})", flat_text(link))
        })
}

impl SuapTicketSource<'_> {
    /// Attaches one file to a ticket that is already open. `description` (at most 80 characters)
    /// defaults to the file name. SUAP accepts one file per request, and only for services that
    /// allow attachments, so several files are several calls.
    pub async fn attach_file(
        &self,
        id: &str,
        attachment: &Attachment,
        description: Option<&str>,
    ) -> Result<(), TicketError> {
        validate_attachments(std::slice::from_ref(attachment))?;
        let (path, _, mut fields) = self
            .open_status_form(ATTACH_PATH_PREFIX, id, "attached", "descricao")
            .await?;
        let description: String = description
            .unwrap_or(&attachment.file_name)
            .trim()
            .chars()
            .take(ATTACHMENT_DESCRIPTION_MAX)
            .collect();
        fields.retain(|(name, _)| name != "anexo");
        set_field(&mut fields, "descricao", &description);
        let file = FormFile {
            field: "anexo".to_owned(),
            file_name: attachment.file_name.clone(),
            bytes: attachment.bytes.clone(),
        };
        self.send_form_with_files(&path, &fields, &[file], "the attachment")
            .await
    }

    /// Hands the ticket to another attendant of its group; returns who got it.
    pub async fn assign_ticket(&self, id: &str, to: &str) -> Result<String, TicketError> {
        let (path, html, mut fields) = self
            .open_status_form(ASSIGN_PATH_PREFIX, id, "assigned", "atribuido_para")
            .await?;
        let attendants = form_choices_with_labels(&html, "atribuido_para", "atribuido_para");
        let chosen = pick_choice(&attendants, to, "attendant")?;
        set_field(&mut fields, "atribuido_para", &chosen.value);
        self.send_status_form(&path, &fields, "the assignment")
            .await?;
        Ok(chosen.label.clone())
    }

    /// Moves the ticket to the attendance group above or below, with an internal note, and
    /// optionally hands it to one of that group's attendants.
    pub async fn move_ticket(
        &self,
        id: &str,
        direction: Direction,
        text: &str,
        to: Option<&str>,
    ) -> Result<(), TicketError> {
        let (prefix, what) = match direction {
            Direction::Escalate => (ESCALATE_PATH_PREFIX, "escalated"),
            Direction::Return => (RETURN_PATH_PREFIX, "returned"),
        };
        let text = crate::non_empty_text(text, "note")?;
        let (path, html, mut fields) = self.open_status_form(prefix, id, what, "texto").await?;
        if let Some(to) = to {
            let attendants = form_choices_with_labels(&html, "texto", "atribuido_para");
            let chosen = pick_choice(&attendants, to, "attendant")?;
            set_field(&mut fields, "atribuido_para", &chosen.value);
        }
        set_field(&mut fields, "texto", text);
        let sent = match direction {
            Direction::Escalate => "the escalation",
            Direction::Return => "the return",
        };
        self.send_status_form(&path, &fields, sent).await
    }

    /// Changes the ticket's service, campus and/or service center, with the justification.
    pub async fn reclassify_ticket(
        &self,
        id: &str,
        change: &Reclassification,
        text: &str,
    ) -> Result<(), TicketError> {
        let text = crate::non_empty_text(text, "justification")?;
        if *change == Reclassification::default() {
            return Err(TicketError::Source(
                "nothing to change: give a service, a campus or a center".to_owned(),
            ));
        }
        let (path, _, mut fields) = self
            .open_status_form(RECLASSIFY_PATH_PREFIX, id, "reclassified", "justificativa")
            .await?;
        let current = |name: &str| {
            let found = fields.iter().find(|(field, _)| field == name);
            found.map(|(_, value)| value.clone())
        };
        let service = change.service.clone().or_else(|| current("servico"));
        let Some(service) = service else {
            return Err(TicketError::Source(
                "the reclassify form has no service".to_owned(),
            ));
        };
        let campus = match &change.campus {
            Some(campus) => campus.clone(),
            None => {
                let listing = format!("{CAMPUS_PATH_PREFIX}{service}/{id}/");
                crate::default_campus(&self.client.fetch_page(&listing).await?)?
            }
        };
        let listing = format!("{CENTERS_PATH_PREFIX}{service}/{campus}/");
        let centers = self.client.fetch_page(&listing).await?;
        let center = match (&change.center, current("centro_atendimento")) {
            (Some(center), _) => center.clone(),
            (None, Some(kept)) if crate::center_offered(&centers, &kept) => kept,
            (None, _) => crate::default_center(&centers)?,
        };
        set_field(&mut fields, "servico", &service);
        set_field(&mut fields, "uo", &campus);
        set_field(&mut fields, "centro_atendimento", &center);
        set_field(&mut fields, "justificativa", text);
        self.send_status_form(&path, &fields, "the reclassification")
            .await
    }

    /// Adds tags (by id or name) to the ticket; returns their names.
    pub async fn add_tags(&self, id: &str, tags: &[String]) -> Result<Vec<String>, TicketError> {
        let (path, html, mut fields) = self
            .open_status_form(ADD_TAGS_PATH_PREFIX, id, "tagged", "tags")
            .await?;
        let offered = form_choices_with_labels(&html, "tags", "tags");
        let mut chosen = Vec::new();
        for wanted in tags {
            chosen.push(pick_choice(&offered, wanted, "tag")?);
        }
        fields.retain(|(name, _)| name != "tags");
        for tag in &chosen {
            fields.push(("tags".to_owned(), tag.value.clone()));
        }
        self.send_status_form(&path, &fields, "the tagging").await?;
        Ok(chosen.iter().map(|tag| tag.label.clone()).collect())
    }

    /// Removes tags (by id or name) from the ticket; returns their names.
    pub async fn remove_tags(&self, id: &str, tags: &[String]) -> Result<Vec<String>, TicketError> {
        crate::check_ticket_id(id)?;
        let page = self
            .client
            .fetch_page(&format!("{TICKET_PATH_PREFIX}{id}/"))
            .await?;
        let prefix = format!("{REMOVE_TAG_PATH_PREFIX}{id}/");
        let present = removal_forms(&page, &prefix, "li");
        let choices: Vec<Choice> = present.iter().map(|(choice, _)| choice.clone()).collect();
        let mut names = Vec::new();
        let mut actions = Vec::new();
        for wanted in tags {
            let chosen = pick_choice(&choices, wanted, "tag on this ticket")?;
            let found = present.iter().find(|(choice, _)| choice == chosen);
            actions.extend(found.map(|(_, action)| action.clone()));
            names.push(chosen.label.clone());
        }
        self.post_removals(&page, &actions, "the tag removal")
            .await?;
        Ok(names)
    }

    /// Adds people (found by registration number or name) as other interested people of the
    /// ticket; returns their names.
    pub async fn add_interested(
        &self,
        id: &str,
        people: &[String],
    ) -> Result<Vec<String>, TicketError> {
        let (path, html, mut fields) = self
            .open_status_form(
                ADD_INTERESTED_PATH_PREFIX,
                id,
                "extended",
                "outros_interessados",
            )
            .await?;
        let control = search_control(&html)
            .ok_or_else(|| TicketError::Source("the form has no person search".to_owned()))?;
        let mut chosen = Vec::new();
        for wanted in people {
            let query = Serializer::new(String::new())
                .append_pair("q", wanted.trim())
                .append_pair("control", control)
                .finish();
            let reply = self
                .client
                .fetch_page(&format!("{PEOPLE_SEARCH_PATH}?{query}"))
                .await?;
            let found = parse_people(&reply)?;
            chosen.push(pick_choice(&found, &found_value(&found, wanted), "person")?.clone());
        }
        fields.retain(|(name, _)| name != "outros_interessados");
        for person in &chosen {
            fields.push(("outros_interessados".to_owned(), person.value.clone()));
        }
        self.send_status_form(&path, &fields, "the addition of interested people")
            .await?;
        Ok(chosen.into_iter().map(|person| person.label).collect())
    }

    /// Removes other interested people (by user id, registration number or name); returns who.
    pub async fn remove_interested(
        &self,
        id: &str,
        people: &[String],
    ) -> Result<Vec<String>, TicketError> {
        crate::check_ticket_id(id)?;
        let page = self
            .client
            .fetch_page(&format!("{TICKET_PATH_PREFIX}{id}/"))
            .await?;
        let prefix = format!("{REMOVE_INTERESTED_PATH_PREFIX}{id}/");
        let present = removal_forms(&page, &prefix, "person");
        let choices: Vec<Choice> = present.iter().map(|(choice, _)| choice.clone()).collect();
        let mut names = Vec::new();
        let mut actions = Vec::new();
        for wanted in people {
            let chosen = pick_choice(&choices, wanted, "other interested person")?;
            let found = present.iter().find(|(choice, _)| choice == chosen);
            actions.extend(found.map(|(_, action)| action.clone()));
            names.push(chosen.label.clone());
        }
        self.post_removals(&page, &actions, "the removal").await?;
        Ok(names)
    }

    /// Posts, for each action, the form of `page` that has it.
    async fn post_removals(
        &self,
        page: &str,
        actions: &[String],
        what: &str,
    ) -> Result<(), TicketError> {
        for action in actions {
            let fields = parse_form_by_action(page, action).unwrap_or_default();
            self.send_status_form(action, &fields, what).await?;
        }
        Ok(())
    }
}

/// What to pick among the people SUAP found for `wanted`: when exactly one came back it is the
/// one (SUAP already matched the words), otherwise the words decide among them.
fn found_value(found: &[Choice], wanted: &str) -> String {
    match found {
        [only] => only.value.clone(),
        _ => wanted.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choices() -> Vec<Choice> {
        vec![
            Choice::new("1", "Ana Souza (111)"),
            Choice::new("2", "Ana Maria (222)"),
            Choice::new("3", "Bruno (333)"),
        ]
    }

    fn message(result: Result<&Choice, TicketError>) -> String {
        result.unwrap_err().to_string()
    }

    #[test]
    fn a_choice_is_found_by_value_whole_label_or_a_unique_part() {
        let all = choices();
        assert_eq!(
            pick_choice(&all, "3", "person").unwrap().label,
            "Bruno (333)"
        );
        assert_eq!(
            pick_choice(&all, " bruno (333) ", "person").unwrap().value,
            "3"
        );
        assert_eq!(pick_choice(&all, "222", "person").unwrap().value, "2");
        assert_eq!(pick_choice(&all, "souza", "person").unwrap().value, "1");
        // The value wins over a label that merely contains it.
        let tricky = vec![Choice::new("1", "item 2"), Choice::new("2", "outro")];
        assert_eq!(pick_choice(&tricky, "2", "item").unwrap().label, "outro");
    }

    #[test]
    fn missing_ambiguous_and_empty_choices_list_the_options() {
        let all = choices();
        let ambiguous = message(pick_choice(&all, "ana", "person"));
        assert!(ambiguous.contains("matches several persons"), "{ambiguous}");
        assert!(
            ambiguous.contains("1 (Ana Souza (111))") && ambiguous.contains("2 (Ana Maria (222))")
        );
        assert!(!ambiguous.contains("Bruno"));
        let missing = message(pick_choice(&all, "zé", "person"));
        assert!(
            missing.contains("no person matches \"zé\"") && missing.contains("Bruno"),
            "{missing}"
        );
        assert!(message(pick_choice(&all, "  ", "person")).contains("name is empty"));
        assert!(message(pick_choice(&[], "x", "tag")).contains("there is no tag to choose from"));
    }

    #[test]
    fn choices_come_from_selects_checkboxes_and_radios() {
        let html = r#"<form><select name="quem"><option value="">---</option>
              <option value="7">Ana (111)</option><option value="8"> Bruno  (222) </option></select>
            <ul><li><label for="a"><input type="checkbox" name="tags" value="1" id="a"> Tag A</label></li>
            <li><label><input type="radio" name="tags" value="2"> Tag   B</label></li>
            <li><input type="checkbox" name="tags" value="3"><input type="checkbox" name="tags"></li></ul></form>
            <form><input name="outro" value="x"></form>"#;
        assert_eq!(
            form_choices_with_labels(html, "quem", "quem"),
            [
                Choice::new("7", "Ana (111)"),
                Choice::new("8", "Bruno (222)")
            ]
        );
        assert_eq!(
            form_choices_with_labels(html, "quem", "tags"),
            [
                Choice::new("1", "Tag A"),
                Choice::new("2", "Tag B"),
                Choice::new("3", "3"),
            ]
        );
        assert!(form_choices_with_labels(html, "nada", "tags").is_empty());
        assert!(form_choices_with_labels(html, "outro", "tags").is_empty());
    }

    #[test]
    fn people_and_the_search_token_are_read_from_suap_replies() {
        let reply = r#"{"total": 2, "items": [
            {"id": 2, "html": "<div class=\"person\"><dd class=\"title\">Pessoa (Mat. 2080883)</dd></div>"},
            {"id": 9, "html": "<div>sem nome</div>"}]}"#;
        assert_eq!(
            parse_people(reply).unwrap(),
            [
                Choice::new("2", "Pessoa (Mat. 2080883)"),
                Choice::new("9", "")
            ]
        );
        assert!(parse_people("nao e json").is_err());
        let page = r#"data: function(){ return { q: 1, control: '{"data": "abc=="}' }; }"#;
        assert_eq!(search_control(page), Some(r#"{"data": "abc=="}"#));
        assert_eq!(search_control("sem token"), None);
        assert_eq!(search_control("control: 'aberto"), None);
        let one = [Choice::new("2", "X")];
        assert_eq!(found_value(&one, "qualquer"), "2");
        assert_eq!(found_value(&[], "ana"), "ana");
        assert_eq!(found_value(&choices(), "ana"), "ana");
    }

    #[test]
    fn removal_forms_name_the_tag_or_person_they_belong_to() {
        let page = r#"<ul class="tags"><li> Tag A <form method="post" action="/centralservicos/remover_tag_do_chamado/2/1/">
              <button>x</button></form></li><li>Tag B<form action="/centralservicos/remover_tag_do_chamado/2/5/"></form></li>
              <li><b>Tag</b> <i>C</i> <!-- nota --><form action="/centralservicos/remover_tag_do_chamado/2/7/"></form></li></ul>
            <div class="person sm"><div class="popup-user"><a href="/rh/servidor/2080883/" class="popup-user-trigger">Pessoa</a></div>
              <form method="post" action="/centralservicos/remover_outros_interessados/2/2/"><button>r</button></form></div>
            <div class="person"><form action="/centralservicos/remover_outros_interessados/2/3/"></form></div>
            <form action="/centralservicos/remover_tag_do_chamado/9/1/"></form><form></form>
            <form action="/centralservicos/remover_tag_do_chamado/2/6/"></form>"#;
        let tags = removal_forms(page, "/centralservicos/remover_tag_do_chamado/2/", "li");
        assert_eq!(
            tags,
            [
                (
                    Choice::new("1", "Tag A"),
                    "/centralservicos/remover_tag_do_chamado/2/1/".to_owned()
                ),
                (
                    Choice::new("5", "Tag B"),
                    "/centralservicos/remover_tag_do_chamado/2/5/".to_owned()
                ),
                (
                    Choice::new("7", "Tag C"),
                    "/centralservicos/remover_tag_do_chamado/2/7/".to_owned()
                ),
                (
                    Choice::new("6", ""),
                    "/centralservicos/remover_tag_do_chamado/2/6/".to_owned()
                ),
            ]
        );
        let people = removal_forms(
            page,
            "/centralservicos/remover_outros_interessados/2/",
            "person",
        );
        assert_eq!(
            people,
            [
                (
                    Choice::new("2", "Pessoa (2080883)"),
                    "/centralservicos/remover_outros_interessados/2/2/".to_owned()
                ),
                (
                    Choice::new("3", ""),
                    "/centralservicos/remover_outros_interessados/2/3/".to_owned()
                ),
            ]
        );
    }
}
