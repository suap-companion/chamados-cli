//! Following a list over several rounds against a mock SUAP whose pages change between rounds.

use chamados_core::{
    watch::{poll, WatchEvent},
    SuapTicketSource, TicketFilter, TicketQueue,
};
use suap_core::{AppPaths, SuapClient, SuapConfig};
use tempfile::{tempdir, TempDir};
use url::Url;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

async fn client_for(server: &MockServer) -> (TempDir, SuapClient) {
    let directory = tempdir().unwrap();
    let paths = AppPaths::from_dirs(directory.path().join("c"), directory.path().join("d"));
    let config = SuapConfig {
        base_url: Url::parse(&format!("{}/", server.uri())).unwrap(),
        ..SuapConfig::default()
    };
    (directory, SuapClient::open(&paths, &config).unwrap())
}

async fn page(server: &MockServer, at: &str, status: u16, body: &str) {
    Mock::given(method("GET"))
        .and(path(at.to_owned()))
        .respond_with(ResponseTemplate::new(status).set_body_string(body.to_owned()))
        .mount(server)
        .await;
}

fn listing(tickets: &[(u32, &str)]) -> String {
    let boxes: Vec<String> = tickets
        .iter()
        .map(|(id, status)| {
            format!(
                r#"<div class="general-box"><span class="status">{status}</span>
                <h4><a href="/centralservicos/chamado/{id}/">REQ #{id} <strong>Assunto {id}</strong></a></h4></div>"#
            )
        })
        .collect();
    boxes.concat()
}

/// A ticket page whose timeline has these entries, newest first.
fn ticket(entries: &[(&str, &str)]) -> String {
    let items: Vec<String> = entries
        .iter()
        .map(|(date, text)| {
            format!(
                r#"<li><div class="timeline-date">{date}</div><div class="timeline-content"><p>{text}</p></div></li>"#
            )
        })
        .collect();
    format!(
        r#"<main id="content"><div class="title-container"><h2>Chamado</h2></div>
        <div data-tab="linha_tempo"><ul class="timeline">{}</ul></div></main>"#,
        items.concat()
    )
}

fn all_pages() -> TicketFilter {
    TicketFilter {
        all_pages: true,
        ..TicketFilter::default()
    }
}

#[tokio::test]
async fn the_first_look_is_a_baseline_and_later_looks_report_changes() {
    let server = MockServer::start().await;
    let (_dir, client) = client_for(&server).await;
    let source = SuapTicketSource::new(&client, TicketQueue::Support);
    let list = "/centralservicos/listar_chamados_suporte/";
    let status = |event: &WatchEvent| match event {
        WatchEvent::Status { from, to, .. } => (from.clone().unwrap(), to.clone().unwrap()),
        other => panic!("not a status change: {other:?}"),
    };

    // Round 1: nothing is reported, whatever is there.
    page(
        &server,
        list,
        200,
        &listing(&[(1, "Aberto"), (2, "Em atendimento")]),
    )
    .await;
    let (first, events) = poll(&source, &all_pages(), "support:active", false, None)
        .await
        .unwrap();
    assert!(events.is_empty());
    assert_eq!(first.tickets.len(), 2);
    assert_eq!(first.tickets["1"].subject.as_deref(), Some("Assunto 1"));

    // Round 2: nothing changed.
    let (second, events) = poll(&source, &all_pages(), "support:active", false, Some(&first))
        .await
        .unwrap();
    assert!(events.is_empty());
    assert_eq!(second, first);

    // Round 3: #1 changes situation, #2 leaves, #3 appears.
    server.reset().await;
    page(
        &server,
        list,
        200,
        &listing(&[(1, "Resolvido"), (3, "Aberto")]),
    )
    .await;
    let (third, events) = poll(
        &source,
        &all_pages(),
        "support:active",
        false,
        Some(&second),
    )
    .await
    .unwrap();
    assert_eq!(events.len(), 3, "{events:?}");
    assert_eq!(events[0].id(), "1");
    assert_eq!(
        status(&events[0]),
        ("Aberto".to_owned(), "Resolvido".to_owned())
    );
    assert_eq!(
        events[1],
        WatchEvent::New {
            id: "3".to_owned(),
            status: Some("Aberto".to_owned()),
            subject: Some("Assunto 3".to_owned())
        }
    );
    assert_eq!(
        events[2],
        WatchEvent::Left {
            id: "2".to_owned(),
            status: Some("Em atendimento".to_owned())
        }
    );

    // Another list is another baseline: no event, even though everything differs.
    let (other, events) = poll(&source, &all_pages(), "mine:active", false, Some(&third))
        .await
        .unwrap();
    assert!(events.is_empty());
    assert_eq!(other.scope, "mine:active");

    // A failing list is an error, and the caller keeps its previous snapshot.
    server.reset().await;
    page(&server, list, 500, "").await;
    assert!(
        poll(&source, &all_pages(), "support:active", false, Some(&third))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn new_messages_are_reported_oldest_first_and_only_for_tickets_already_known() {
    let server = MockServer::start().await;
    let (_dir, client) = client_for(&server).await;
    let source = SuapTicketSource::new(&client, TicketQueue::Mine);
    let list = "/centralservicos/meus_chamados/";
    let scope = "mine:active";

    page(
        &server,
        list,
        200,
        &listing(&[(1, "Aberto"), (2, "Aberto")]),
    )
    .await;
    page(
        &server,
        "/centralservicos/chamado/1/",
        200,
        &ticket(&[("10:00", "abri")]),
    )
    .await;
    page(
        &server,
        "/centralservicos/chamado/2/",
        200,
        &ticket(&[("10:00", "outro")]),
    )
    .await;
    let (first, events) = poll(&source, &all_pages(), scope, true, None)
        .await
        .unwrap();
    assert!(events.is_empty());
    assert_eq!(first.tickets["1"].seen.len(), 1);

    // #1 gets two messages (listed newest first), #2 none, and #3 appears already with entries.
    server.reset().await;
    page(
        &server,
        list,
        200,
        &listing(&[(1, "Aberto"), (2, "Aberto"), (3, "Aberto")]),
    )
    .await;
    page(
        &server,
        "/centralservicos/chamado/1/",
        200,
        &ticket(&[
            ("12:00", "segunda<br>em duas linhas"),
            ("11:00", "primeira"),
            ("10:00", "abri"),
        ]),
    )
    .await;
    page(
        &server,
        "/centralservicos/chamado/2/",
        200,
        &ticket(&[("10:00", "outro")]),
    )
    .await;
    page(
        &server,
        "/centralservicos/chamado/3/",
        200,
        &ticket(&[("11:30", "ja nasceu com isto")]),
    )
    .await;
    let (second, events) = poll(&source, &all_pages(), scope, true, Some(&first))
        .await
        .unwrap();
    let described: Vec<(String, String)> = events
        .iter()
        .map(|event| (event.kind().to_owned(), event.id().to_owned()))
        .collect();
    assert_eq!(
        described,
        [
            ("message".to_owned(), "1".to_owned()),
            ("message".to_owned(), "1".to_owned()),
            ("new".to_owned(), "3".to_owned()),
        ]
    );
    let texts: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            WatchEvent::Message { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, ["primeira", "segunda\nem duas linhas"]);

    // Looking again reports nothing; so does turning messages off (the fingerprints are kept).
    let (third, events) = poll(&source, &all_pages(), scope, true, Some(&second))
        .await
        .unwrap();
    assert!(events.is_empty());
    let (quiet, events) = poll(&source, &all_pages(), scope, false, Some(&third))
        .await
        .unwrap();
    assert!(events.is_empty());
    assert_eq!(quiet.tickets["1"].seen, third.tickets["1"].seen);

    // Messages turned on for a snapshot taken without them: it starts from what is there now.
    let mut without = second.clone();
    without
        .tickets
        .values_mut()
        .for_each(|state| state.seen.clear());
    let (_, events) = poll(&source, &all_pages(), scope, true, Some(&without))
        .await
        .unwrap();
    assert!(events.is_empty(), "{events:?}");

    // A ticket page that cannot be read fails the whole round (nothing is half-recorded).
    server.reset().await;
    page(&server, list, 200, &listing(&[(1, "Aberto")])).await;
    page(&server, "/centralservicos/chamado/1/", 404, "").await;
    assert!(poll(&source, &all_pages(), scope, true, Some(&third))
        .await
        .is_err());
}
