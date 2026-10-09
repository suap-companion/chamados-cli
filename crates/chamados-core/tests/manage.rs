//! The ticket management actions (assign, escalate/return, reclassify, tags, other interested
//! people) against a mock SUAP that serves the same forms the real one does.

use chamados_core::{Direction, Reclassification, SuapTicketSource, TicketQueue};
use suap_core::{AppPaths, SuapClient, SuapConfig};
use tempfile::{tempdir, TempDir};
use url::Url;
use wiremock::{
    matchers::{method, path, query_param},
    Mock, MockServer, ResponseTemplate,
};

const TOKEN: &str = r#"<input type="hidden" name="csrfmiddlewaretoken" value="tok">"#;

async fn client_for(server: &MockServer) -> (TempDir, SuapClient) {
    let directory = tempdir().unwrap();
    let paths = AppPaths::from_dirs(directory.path().join("c"), directory.path().join("d"));
    let config = SuapConfig {
        base_url: Url::parse(&format!("{}/", server.uri())).unwrap(),
        ..SuapConfig::default()
    };
    (directory, SuapClient::open(&paths, &config).unwrap())
}

async fn page(server: &MockServer, verb: &str, at: &str, status: u16, body: &str) {
    Mock::given(method(verb))
        .and(path(at.to_owned()))
        .respond_with(ResponseTemplate::new(status).set_body_string(body.to_owned()))
        .mount(server)
        .await;
}

async fn posted(server: &MockServer, at: &str) -> Vec<String> {
    let requests = server.received_requests().await.unwrap();
    requests
        .iter()
        .filter(|request| request.method.as_str() == "POST" && request.url.path() == at)
        .map(|request| String::from_utf8_lossy(&request.body).into_owned())
        .collect()
}

fn attendants_form(extra: &str) -> String {
    format!(
        r#"<form method="POST">{TOKEN}<textarea name="texto"></textarea>{extra}
        <select name="atribuido_para"><option value="" selected>---</option>
        <option value="2">Pessoa (2080883)</option><option value="3">Outra (2080884)</option></select></form>"#
    )
}

#[tokio::test]
async fn assigns_a_ticket_to_an_attendant_chosen_by_registration_number() {
    let server = MockServer::start().await;
    page(
        &server,
        "GET",
        "/centralservicos/atribuir_chamado/5/",
        200,
        &attendants_form(""),
    )
    .await;
    page(
        &server,
        "POST",
        "/centralservicos/atribuir_chamado/5/",
        200,
        "ok",
    )
    .await;
    page(
        &server,
        "GET",
        "/centralservicos/atribuir_chamado/6/",
        200,
        "<p>sem formulário</p>",
    )
    .await;
    page(
        &server,
        "GET",
        "/centralservicos/atribuir_chamado/7/",
        403,
        "",
    )
    .await;
    let (_dir, client) = client_for(&server).await;
    let source = SuapTicketSource::new(&client, TicketQueue::Support);

    let who = source.assign_ticket("5", "2080883").await.unwrap();
    assert_eq!(who, "Pessoa (2080883)");
    let body = &posted(&server, "/centralservicos/atribuir_chamado/5/").await[0];
    assert!(
        body.contains("atribuido_para=2") && body.contains("csrfmiddlewaretoken=tok"),
        "{body}"
    );

    let ambiguous = source
        .assign_ticket("5", "(2080")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        ambiguous.contains("matches several attendants"),
        "{ambiguous}"
    );
    let unknown = source
        .assign_ticket("5", "ninguem")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        unknown.contains("no attendant matches") && unknown.contains("2080884"),
        "{unknown}"
    );
    let no_form = source
        .assign_ticket("6", "x")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        no_form.contains("ticket 6 has no assigned form"),
        "{no_form}"
    );
    let forbidden = source
        .assign_ticket("7", "x")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        forbidden.contains("ticket 7 cannot be assigned"),
        "{forbidden}"
    );
    assert_eq!(
        posted(&server, "/centralservicos/atribuir_chamado/5/")
            .await
            .len(),
        1
    );
}

#[tokio::test]
async fn escalates_and_returns_with_a_note_and_maybe_an_attendant() {
    let server = MockServer::start().await;
    for direction in ["escalar", "retornar"] {
        let at = format!("/centralservicos/{direction}_atendimento_chamado/5/");
        page(&server, "GET", &at, 200, &attendants_form("")).await;
        page(&server, "POST", &at, 200, "ok").await;
    }
    // SUAP refuses with a message on the page it redirects to, which has no form.
    page(
        &server,
        "GET",
        "/centralservicos/escalar_atendimento_chamado/6/",
        200,
        r#"<p class="alert-error">Este atendimento não pode ser escalado. <button>Fechar</button></p>"#,
    )
    .await;
    let (_dir, client) = client_for(&server).await;
    let source = SuapTicketSource::new(&client, TicketQueue::Support);

    source
        .move_ticket(
            "5",
            Direction::Escalate,
            "passando adiante\nobrigado\n",
            Some("2080884"),
        )
        .await
        .unwrap();
    source
        .move_ticket("5", Direction::Return, "voltou", None)
        .await
        .unwrap();
    let escalated = &posted(&server, "/centralservicos/escalar_atendimento_chamado/5/").await[0];
    assert!(
        escalated.contains("texto=passando+adiante%0Aobrigado"),
        "{escalated}"
    );
    assert!(escalated.contains("atribuido_para=3"), "{escalated}");
    let returned = &posted(&server, "/centralservicos/retornar_atendimento_chamado/5/").await[0];
    assert!(returned.contains("texto=voltou"), "{returned}");

    let refused = source
        .move_ticket("6", Direction::Escalate, "x", None)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        refused.contains("no escalated form: Este atendimento não pode ser escalado."),
        "{refused}"
    );
    let empty = source
        .move_ticket("5", Direction::Return, " ", None)
        .await
        .unwrap_err();
    assert!(empty.to_string().contains("the note text is empty"));
    let unknown = source
        .move_ticket("5", Direction::Escalate, "x", Some("zé"))
        .await
        .unwrap_err();
    assert!(unknown.to_string().contains("no attendant matches"));
    assert_eq!(
        posted(&server, "/centralservicos/escalar_atendimento_chamado/5/")
            .await
            .len(),
        1
    );
}

const RECLASSIFY_FORM: &str = r#"<form method="POST"><input type="hidden" name="csrfmiddlewaretoken" value="tok">
    <input type="hidden" id="id_servico" name="servico" value="1"><select name="uo"></select>
    <ul><li><label><input type="radio" name="centro_atendimento" value="1" checked> Centro 1</label></li></ul>
    <textarea name="justificativa"></textarea></form>"#;

async fn reclassify_server() -> MockServer {
    let server = MockServer::start().await;
    page(
        &server,
        "GET",
        "/centralservicos/reclassificar_chamado/5/",
        200,
        RECLASSIFY_FORM,
    )
    .await;
    page(
        &server,
        "POST",
        "/centralservicos/reclassificar_chamado/5/",
        200,
        "ok",
    )
    .await;
    // The current campus is the selected one; service 1 offers the current center, service 2 does not.
    for service in ["1", "2", "3"] {
        let campus = format!("/centralservicos/get_campus_com_centros_atendimento/{service}/5/");
        page(
            &server,
            "GET",
            &campus,
            200,
            r#"{"campus": [[8, "XX", false], [9, "ZL", true]]}"#,
        )
        .await;
    }
    let centers = "/centralservicos/get_centros_atendimento_por_servico_e_campus";
    page(
        &server,
        "GET",
        &format!("{centers}/1/9/"),
        200,
        r#"{"centros": [[1, "A", true], [4, "D", true]]}"#,
    )
    .await;
    page(
        &server,
        "GET",
        &format!("{centers}/2/9/"),
        200,
        r#"{"centros": [[4, "D", true], [5, "E", true]]}"#,
    )
    .await;
    page(
        &server,
        "GET",
        &format!("{centers}/3/9/"),
        200,
        r#"{"centros": [[6, "F", true]]}"#,
    )
    .await;
    page(
        &server,
        "GET",
        &format!("{centers}/1/3/"),
        200,
        "nao e json",
    )
    .await;
    server
}

#[tokio::test]
async fn reclassifies_keeping_what_the_user_did_not_change() {
    let server = reclassify_server().await;
    let (_dir, client) = client_for(&server).await;
    let source = SuapTicketSource::new(&client, TicketQueue::Support);
    let change =
        |service: Option<&str>, campus: Option<&str>, center: Option<&str>| Reclassification {
            service: service.map(str::to_owned),
            campus: campus.map(str::to_owned),
            center: center.map(str::to_owned),
        };

    // Only the center: service and campus stay (campus is SUAP's selected one).
    source
        .reclassify_ticket(
            "5",
            &change(None, None, Some("4")),
            "motivo\nem duas linhas\n",
        )
        .await
        .unwrap();
    // Only the service, to one that has a single center: it is used.
    source
        .reclassify_ticket("5", &change(Some("3"), None, None), "troca")
        .await
        .unwrap();
    // Only the campus: the current center is kept because it is still offered.
    source
        .reclassify_ticket("5", &change(None, Some("9"), None), "campus")
        .await
        .unwrap();
    let bodies = posted(&server, "/centralservicos/reclassificar_chamado/5/").await;
    assert!(
        bodies[0].contains("servico=1") && bodies[0].contains("uo=9"),
        "{}",
        bodies[0]
    );
    assert!(
        bodies[0].contains("centro_atendimento=4")
            && bodies[0].contains("justificativa=motivo%0Aem+duas+linhas"),
        "{}",
        bodies[0]
    );
    assert!(
        bodies[1].contains("servico=3") && bodies[1].contains("centro_atendimento=6"),
        "{}",
        bodies[1]
    );
    assert!(
        bodies[2].contains("servico=1") && bodies[2].contains("centro_atendimento=1"),
        "{}",
        bodies[2]
    );

    // Service 2 does not offer the current center and has two: the user must choose.
    let several = source
        .reclassify_ticket("5", &change(Some("2"), None, None), "x")
        .await
        .unwrap_err();
    assert!(
        several.to_string().contains("several service centers"),
        "{several}"
    );
    // An unreadable list of centers means the current one cannot be confirmed: SUAP's own check decides.
    let unreadable = source
        .reclassify_ticket("5", &change(None, Some("3"), None), "x")
        .await
        .unwrap_err();
    assert!(
        unreadable.to_string().contains("center list"),
        "{unreadable}"
    );

    let nothing = source
        .reclassify_ticket("5", &Reclassification::default(), "x")
        .await
        .unwrap_err();
    assert!(nothing.to_string().contains("nothing to change"));
    let empty = source
        .reclassify_ticket("5", &change(None, None, Some("4")), " ")
        .await
        .unwrap_err();
    assert!(empty
        .to_string()
        .contains("the justification text is empty"));
    assert_eq!(
        posted(&server, "/centralservicos/reclassificar_chamado/5/")
            .await
            .len(),
        3
    );
}

#[tokio::test]
async fn reclassification_reports_what_suap_refuses() {
    let server = MockServer::start().await;
    page(
        &server,
        "GET",
        "/centralservicos/reclassificar_chamado/5/",
        200,
        RECLASSIFY_FORM,
    )
    .await;
    page(
        &server,
        "POST",
        "/centralservicos/reclassificar_chamado/5/",
        200,
        r#"<ul class="errorlist"><li>Nenhuma mudança foi identificada</li></ul>"#,
    )
    .await;
    page(
        &server,
        "GET",
        "/centralservicos/get_campus_com_centros_atendimento/1/5/",
        200,
        r#"{"campus": [[9, "ZL", true]]}"#,
    )
    .await;
    page(
        &server,
        "GET",
        "/centralservicos/get_centros_atendimento_por_servico_e_campus/1/9/",
        200,
        r#"{"centros": [[1, "A", true]]}"#,
    )
    .await;
    // A form without a service cannot be completed.
    page(
        &server,
        "GET",
        "/centralservicos/reclassificar_chamado/6/",
        200,
        r#"<form method="POST"><textarea name="justificativa"></textarea></form>"#,
    )
    .await;
    let (_dir, client) = client_for(&server).await;
    let source = SuapTicketSource::new(&client, TicketQueue::Support);
    let same = Reclassification {
        center: Some("1".to_owned()),
        ..Reclassification::default()
    };
    let rejected = source.reclassify_ticket("5", &same, "x").await.unwrap_err();
    assert!(
        rejected
            .to_string()
            .contains("SUAP rejected the reclassification: Nenhuma mudança"),
        "{rejected}"
    );
    let no_service = source.reclassify_ticket("6", &same, "x").await.unwrap_err();
    assert!(
        no_service.to_string().contains("has no service"),
        "{no_service}"
    );
}

fn tags_form() -> String {
    format!(
        r#"<form method="POST">{TOKEN}<ul>
        <li><label><input type="checkbox" name="tags" value="1"> Rede</label></li>
        <li><label><input type="checkbox" name="tags" value="2"> Redes sociais</label></li>
        <li><label><input type="checkbox" name="tags" value="3"> Moodle</label></li></ul></form>"#
    )
}

fn ticket_page() -> String {
    r#"<main id="content"><ul class="tags">
        <li> Rede <form method="post" action="/centralservicos/remover_tag_do_chamado/5/1/"><input type="hidden" name="csrfmiddlewaretoken" value="t1"><button>x</button></form></li>
        <li> Moodle <form method="post" action="/centralservicos/remover_tag_do_chamado/5/3/"><input type="hidden" name="csrfmiddlewaretoken" value="t3"></form></li></ul>
        <div class="person sm"><div class="popup-user"><a href="/rh/servidor/2080883/">Ana Souza</a></div>
          <form method="post" action="/centralservicos/remover_outros_interessados/5/2/"><input type="hidden" name="csrfmiddlewaretoken" value="p2"></form></div>
        <div class="person sm"><div class="popup-user"><a href="/rh/servidor/2080884/">Bruno Lima</a></div>
          <form method="post" action="/centralservicos/remover_outros_interessados/5/3/"><input type="hidden" name="csrfmiddlewaretoken" value="p3"></form></div>
        </main>"#
        .to_owned()
}

#[tokio::test]
async fn adds_and_removes_tags_by_name() {
    let server = MockServer::start().await;
    page(
        &server,
        "GET",
        "/centralservicos/adicionar_tags_ao_chamado/5/",
        200,
        &tags_form(),
    )
    .await;
    page(
        &server,
        "POST",
        "/centralservicos/adicionar_tags_ao_chamado/5/",
        200,
        "ok",
    )
    .await;
    page(
        &server,
        "GET",
        "/centralservicos/chamado/5/",
        200,
        &ticket_page(),
    )
    .await;
    page(
        &server,
        "POST",
        "/centralservicos/remover_tag_do_chamado/5/1/",
        200,
        "ok",
    )
    .await;
    page(
        &server,
        "POST",
        "/centralservicos/remover_tag_do_chamado/5/3/",
        200,
        "ok",
    )
    .await;
    let (_dir, client) = client_for(&server).await;
    let source = SuapTicketSource::new(&client, TicketQueue::Support);
    let names =
        |list: &[&str]| -> Vec<String> { list.iter().map(|name| (*name).to_owned()).collect() };

    let added = source
        .add_tags("5", &names(&["moodle", "2"]))
        .await
        .unwrap();
    assert_eq!(added, ["Moodle", "Redes sociais"]);
    let body = &posted(&server, "/centralservicos/adicionar_tags_ao_chamado/5/").await[0];
    assert!(
        body.contains("tags=3")
            && body.contains("tags=2")
            && body.contains("csrfmiddlewaretoken=tok"),
        "{body}"
    );
    // "Rede" is a whole label even though "Redes sociais" also contains it.
    assert_eq!(
        source.add_tags("5", &names(&["Rede"])).await.unwrap(),
        ["Rede"]
    );
    let ambiguous = source.add_tags("5", &names(&["red"])).await.unwrap_err();
    assert!(
        ambiguous.to_string().contains("matches several tags"),
        "{ambiguous}"
    );
    assert_eq!(
        posted(&server, "/centralservicos/adicionar_tags_ao_chamado/5/")
            .await
            .len(),
        2
    );

    let removed = source
        .remove_tags("5", &names(&["rede", "3"]))
        .await
        .unwrap();
    assert_eq!(removed, ["Rede", "Moodle"]);
    assert!(
        posted(&server, "/centralservicos/remover_tag_do_chamado/5/1/").await[0]
            .contains("csrfmiddlewaretoken=t1")
    );
    assert!(
        posted(&server, "/centralservicos/remover_tag_do_chamado/5/3/").await[0]
            .contains("csrfmiddlewaretoken=t3")
    );
    let absent = source
        .remove_tags("5", &names(&["redes"]))
        .await
        .unwrap_err();
    assert!(
        absent.to_string().contains("no tag on this ticket matches"),
        "{absent}"
    );
    assert!(source.remove_tags("x", &names(&["a"])).await.is_err());
}

const PEOPLE_PAGE: &str = r#"<form method="POST"><input type="hidden" name="csrfmiddlewaretoken" value="tok">
    <select name="outros_interessados" multiple></select>
    <script>input.select2({ ajax: { data: function(params){ return { q: params.term, control: '{"data": "abc"}' }; } } });</script></form>"#;

#[tokio::test]
async fn adds_and_removes_other_interested_people() {
    let server = MockServer::start().await;
    page(
        &server,
        "GET",
        "/centralservicos/adicionar_outros_interessados/5/",
        200,
        PEOPLE_PAGE,
    )
    .await;
    page(
        &server,
        "POST",
        "/centralservicos/adicionar_outros_interessados/5/",
        200,
        "ok",
    )
    .await;
    page(
        &server,
        "GET",
        "/centralservicos/adicionar_outros_interessados/6/",
        200,
        "<form><select name=\"outros_interessados\"></select></form>",
    )
    .await;
    for (term, reply) in [
        (
            "2080883",
            r#"{"items": [{"id": 2, "html": "<dd class=\"title\">Ana Souza (Mat. 2080883)</dd>"}]}"#,
        ),
        (
            "souza",
            r#"{"items": [{"id": 2, "html": "<dd class=\"title\">Ana Souza (Mat. 2080883)</dd>"},
                {"id": 7, "html": "<dd class=\"title\">Carlos Souza (Mat. 111)</dd>"}]}"#,
        ),
        ("nunca", r#"{"items": []}"#),
    ] {
        Mock::given(method("GET"))
            .and(path("/json/comum/vinculo/"))
            .and(query_param("q", term))
            .and(query_param("control", r#"{"data": "abc"}"#))
            .respond_with(ResponseTemplate::new(200).set_body_string(reply))
            .mount(&server)
            .await;
    }
    page(
        &server,
        "GET",
        "/centralservicos/chamado/5/",
        200,
        &ticket_page(),
    )
    .await;
    page(
        &server,
        "POST",
        "/centralservicos/remover_outros_interessados/5/2/",
        200,
        "ok",
    )
    .await;
    page(
        &server,
        "POST",
        "/centralservicos/remover_outros_interessados/5/3/",
        200,
        "ok",
    )
    .await;
    let (_dir, client) = client_for(&server).await;
    let source = SuapTicketSource::new(&client, TicketQueue::Support);
    let names =
        |list: &[&str]| -> Vec<String> { list.iter().map(|name| (*name).to_owned()).collect() };

    // One result is the person; several are narrowed down by the words, and still reported if unclear.
    let added = source
        .add_interested("5", &names(&["2080883", "souza"]))
        .await;
    assert!(added
        .unwrap_err()
        .to_string()
        .contains("matches several persons"));
    let added = source
        .add_interested("5", &names(&["2080883"]))
        .await
        .unwrap();
    assert_eq!(added, ["Ana Souza (Mat. 2080883)"]);
    let body = &posted(&server, "/centralservicos/adicionar_outros_interessados/5/").await[0];
    assert!(
        body.contains("outros_interessados=2") && body.contains("csrfmiddlewaretoken=tok"),
        "{body}"
    );
    let none = source
        .add_interested("5", &names(&["nunca"]))
        .await
        .unwrap_err();
    assert!(
        none.to_string()
            .contains("there is no person to choose from"),
        "{none}"
    );
    let no_search = source
        .add_interested("6", &names(&["x"]))
        .await
        .unwrap_err();
    assert!(
        no_search.to_string().contains("has no person search"),
        "{no_search}"
    );

    let removed = source
        .remove_interested("5", &names(&["2080883", "bruno"]))
        .await
        .unwrap();
    assert_eq!(removed, ["Ana Souza (2080883)", "Bruno Lima (2080884)"]);
    assert!(
        posted(&server, "/centralservicos/remover_outros_interessados/5/2/").await[0]
            .contains("csrfmiddlewaretoken=p2")
    );
    assert!(
        posted(&server, "/centralservicos/remover_outros_interessados/5/3/").await[0]
            .contains("csrfmiddlewaretoken=p3")
    );
    let absent = source
        .remove_interested("5", &names(&["carlos"]))
        .await
        .unwrap_err();
    assert!(
        absent
            .to_string()
            .contains("no other interested person matches"),
        "{absent}"
    );
    assert!(source.remove_interested("x", &names(&["a"])).await.is_err());
}
