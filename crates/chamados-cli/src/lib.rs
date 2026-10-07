//! Command-line interface logic for `chamados`.
//!
//! The binary entry point (`main.rs`) is a thin wrapper; everything testable lives here.

use std::{error::Error, ffi::OsString, io::Write};

use clap::{Parser, Subcommand};
use chamados_core::{NewTicket, SuapTicketSource, TicketDetails, TicketError, TicketQueue, TicketSource};
use suap_core::{load_config, save_config, AppPaths, SuapClient, SuapError};

#[derive(Debug, Parser)]
#[command(name = "chamados", version, about = "Cliente local para chamados do SUAP")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Exibe os diretórios usados pela aplicação.
    Paths,
    /// Exibe a configuração local sem dados sensíveis.
    ConfigShow,
    /// Cria uma configuração local inicial.
    ConfigInit {
        #[arg(long)]
        base_url: Option<String>,
        #[arg(long)]
        username: Option<String>,
    },
    /// Autentica no SUAP usando a senha da variável de ambiente `SUAP_PASSWORD`.
    Login {
        /// Usuário do SUAP; se omitido, usa o `username` da configuração local.
        #[arg(long)]
        username: Option<String>,
    },
    /// Lista os chamados do SUAP usando a sessão salva por `login`.
    List {
        /// Lista os "Meus chamados" ativos em vez da fila de suporte.
        #[arg(long)]
        meus: bool,
    },
    /// Exibe os detalhes de um chamado do SUAP usando a sessão salva por `login`.
    Show {
        /// Número do chamado (ex.: 559298).
        id: u64,
    },
    /// Abre um novo chamado no SUAP usando a sessão salva por `login`.
    Open {
        /// Número do serviço no SUAP (o mesmo de /centralservicos/abrir_chamado/<serviço>/).
        service: u64,
        /// Descrição do chamado.
        #[arg(long, short)]
        description: String,
        /// Campus (id da unidade organizacional); por padrão, o do usuário.
        #[arg(long)]
        campus: Option<String>,
        /// Centro de atendimento (id); por padrão, o único disponível para o campus.
        #[arg(long)]
        center: Option<String>,
        /// Interessado (id do vínculo); por padrão, o usuário autenticado.
        #[arg(long)]
        interested: Option<String>,
        /// Campo extra do formulário no formato NOME=VALOR (repetível), ex.: --field patrimonio=123.
        #[arg(long = "field", value_parser = parse_field)]
        fields: Vec<(String, String)>,
    },
    /// Exibe uma mensagem sobre o estado inicial do projeto.
    Status,
}

fn parse_field(raw: &str) -> Result<(String, String), String> {
    match raw.split_once('=') {
        Some((name, value)) if !name.is_empty() => Ok((name.to_owned(), value.to_owned())),
        _ => Err(format!("campo inválido {raw:?}: use NOME=VALOR")),
    }
}

/// Name of the environment variable that holds the SUAP password.
pub const PASSWORD_ENV: &str = "SUAP_PASSWORD";

/// Runs the CLI with `args`, writing to `out`/`err`, and returns the process exit code.
///
/// `password` is the value of [`PASSWORD_ENV`], read by the caller so it can be injected in tests.
pub fn run<I, T>(
    args: I,
    paths: &AppPaths,
    password: Option<String>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => {
            let code = error.exit_code();
            if error.use_stderr() {
                let _ = write!(err, "{error}");
            } else {
                let _ = write!(out, "{error}");
            }
            return code;
        }
    };

    match execute(cli, paths, password, out) {
        Ok(()) => 0,
        Err(error) => {
            let _ = writeln!(err, "erro: {error}");
            1
        }
    }
}

fn execute(
    cli: Cli,
    paths: &AppPaths,
    password: Option<String>,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    match cli.command {
        Some(Command::Paths) => show_paths(paths, out),
        Some(Command::ConfigShow) => show_config(paths, out),
        Some(Command::ConfigInit { base_url, username }) => init_config(paths, base_url, username, out),
        Some(Command::Login { username }) => login(paths, username, password, out),
        Some(Command::List { meus }) => list(paths, meus, out),
        Some(Command::Show { id }) => show(paths, id, out),
        Some(Command::Open { service, description, campus, center, interested, fields }) => {
            let ticket = NewTicket { service_id: service, description, campus, center, interested, extra_fields: fields };
            open(paths, &ticket, out)
        }
        Some(Command::Status) => {
            writeln!(out, "chamados-cli: fundação inicial instalada; integração ainda não implementada.")?;
            Ok(())
        }
        None => {
            writeln!(out, "Use `chamados --help` para consultar os comandos disponíveis.")?;
            Ok(())
        }
    }
}

fn show_paths(paths: &AppPaths, out: &mut dyn Write) -> Result<(), Box<dyn Error>> {
    writeln!(out, "config_dir: {}", paths.config_dir().display())?;
    writeln!(out, "config_file: {}", paths.config_file().display())?;
    writeln!(out, "data_dir: {}", paths.data_dir().display())?;
    writeln!(out, "session_file: {}", paths.session_file().display())?;
    Ok(())
}

fn show_config(paths: &AppPaths, out: &mut dyn Write) -> Result<(), Box<dyn Error>> {
    match load_config(paths)? {
        Some(config) => {
            writeln!(out, "base_url: {}", config.base_url)?;
            writeln!(out, "username: {}", config.username.as_deref().unwrap_or("<não configurado>"))?;
            writeln!(out, "file: {}", paths.config_file().display())?;
        }
        None => writeln!(out, "Nenhuma configuração encontrada em {}", paths.config_file().display())?,
    }
    Ok(())
}

fn login(
    paths: &AppPaths,
    username: Option<String>,
    password: Option<String>,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let config = load_config(paths)?.unwrap_or_default();
    let username = username
        .or_else(|| config.username.clone())
        .ok_or("usuário não informado: use --username ou `config-init --username`")?;
    let password = password.ok_or_else(|| format!("senha não informada: defina a variável de ambiente {PASSWORD_ENV}"))?;

    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let client = SuapClient::open(paths, &config)?;
    runtime.block_on(client.login(&username, &password))?;
    writeln!(out, "Login realizado como {username}. Sessão salva em {}", paths.session_file().display())?;
    Ok(())
}

fn list(paths: &AppPaths, mine: bool, out: &mut dyn Write) -> Result<(), Box<dyn Error>> {
    let config = load_config(paths)?.unwrap_or_default();
    let queue = if mine { TicketQueue::Mine } else { TicketQueue::Support };
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let client = SuapClient::open(paths, &config)?;
    let source = SuapTicketSource::new(&client, queue);

    let tickets = runtime.block_on(source.list_tickets()).map_err(explain)?;

    if tickets.is_empty() {
        writeln!(out, "Nenhum chamado encontrado.")?;
    }
    for ticket in tickets {
        let status = ticket.status.as_deref().unwrap_or("-");
        let subject = ticket.subject.as_deref().unwrap_or("-");
        writeln!(out, "#{}\t{status}\t{subject}", ticket.id)?;
    }
    Ok(())
}

fn show(paths: &AppPaths, id: u64, out: &mut dyn Write) -> Result<(), Box<dyn Error>> {
    let config = load_config(paths)?.unwrap_or_default();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let client = SuapClient::open(paths, &config)?;
    let source = SuapTicketSource::new(&client, TicketQueue::Support);
    let details = runtime.block_on(source.get_ticket(&id.to_string())).map_err(explain)?;
    print_details(&details, out)?;
    Ok(())
}

fn open(paths: &AppPaths, ticket: &NewTicket, out: &mut dyn Write) -> Result<(), Box<dyn Error>> {
    let config = load_config(paths)?.unwrap_or_default();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let client = SuapClient::open(paths, &config)?;
    let source = SuapTicketSource::new(&client, TicketQueue::Support);
    let id = runtime.block_on(source.open_ticket(ticket)).map_err(explain)?;
    writeln!(out, "Chamado #{id} aberto: {}", client.base_url().join(&format!("centralservicos/chamado/{id}/"))?)?;
    Ok(())
}

fn print_details(details: &TicketDetails, out: &mut dyn Write) -> std::io::Result<()> {
    writeln!(out, "{}", details.title)?;
    writeln!(out, "Situação: {}", details.statuses.join("; "))?;
    writeln!(out, "Serviço: {}", details.heading.as_deref().unwrap_or("-"))?;
    writeln!(out, "URL: {}", details.details_url)?;
    for (label, value) in &details.fields {
        writeln!(out, "{label}: {value}")?;
    }
    writeln!(out, "\nLinha do tempo:")?;
    for entry in &details.timeline {
        writeln!(out, "  {}  {}", entry.date, entry.text)?;
    }
    Ok(())
}

/// Turns ticket errors into user-facing messages.
fn explain(error: TicketError) -> Box<dyn Error> {
    match error {
        TicketError::Suap(SuapError::NotAuthenticated) => "sessão ausente ou expirada: execute `chamados login`".into(),
        other => other.into(),
    }
}

fn init_config(
    paths: &AppPaths,
    base_url: Option<String>,
    username: Option<String>,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let mut config = load_config(paths)?.unwrap_or_default();

    if let Some(base_url) = base_url {
        config.base_url = base_url.parse()?;
    }
    if username.is_some() {
        config.username = username;
    }

    save_config(paths, &config)?;
    writeln!(out, "Configuração salva em {}", paths.config_file().display())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::{tempdir, TempDir};
    use wiremock::{matchers::{method, path}, Mock, MockServer, ResponseTemplate};

    fn paths() -> (TempDir, AppPaths) {
        let directory = tempdir().unwrap();
        let paths = AppPaths::from_dirs(directory.path().join("config"), directory.path().join("data"));
        (directory, paths)
    }

    fn run_args(args: &[&str], paths: &AppPaths) -> (i32, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(std::iter::once("chamados").chain(args.iter().copied()), paths, None, &mut out, &mut err);
        (code, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
    }

    #[test]
    fn paths_lists_directories() {
        let (_dir, paths) = paths();
        let (code, out, _) = run_args(&["paths"], &paths);
        assert_eq!(code, 0);
        assert!(out.contains("config_file:") && out.contains("session_file:"));
    }

    #[test]
    fn status_prints_message() {
        let (_dir, paths) = paths();
        let (code, out, _) = run_args(&["status"], &paths);
        assert_eq!(code, 0);
        assert!(out.contains("fundação inicial"));
    }

    #[test]
    fn no_subcommand_suggests_help() {
        let (_dir, paths) = paths();
        let (code, out, _) = run_args(&[], &paths);
        assert_eq!(code, 0);
        assert!(out.contains("--help"));
    }

    #[test]
    fn help_goes_to_stdout() {
        let (_dir, paths) = paths();
        let (code, out, err) = run_args(&["--help"], &paths);
        assert_eq!(code, 0);
        assert!(out.contains("config-init") && err.is_empty());
    }

    #[test]
    fn invalid_argument_goes_to_stderr() {
        let (_dir, paths) = paths();
        let (code, out, err) = run_args(&["--nope"], &paths);
        assert_eq!(code, 2);
        assert!(out.is_empty() && !err.is_empty());
    }

    #[test]
    fn config_show_without_file() {
        let (_dir, paths) = paths();
        let (code, out, _) = run_args(&["config-show"], &paths);
        assert_eq!(code, 0);
        assert!(out.contains("Nenhuma configuração"));
    }

    #[test]
    fn config_init_then_show() {
        let (_dir, paths) = paths();
        let (code, out, _) = run_args(&["config-init", "--base-url", "https://example.org/"], &paths);
        assert_eq!(code, 0);
        assert!(out.contains("Configuração salva"));

        let (_, out, _) = run_args(&["config-show"], &paths);
        assert!(out.contains("https://example.org/") && out.contains("<não configurado>"));

        let (code, _, _) = run_args(&["config-init", "--username", "kelson"], &paths);
        assert_eq!(code, 0);
        let (_, out, _) = run_args(&["config-show"], &paths);
        assert!(out.contains("https://example.org/") && out.contains("username: kelson"));
    }

    #[test]
    fn config_init_rejects_invalid_url() {
        let (_dir, paths) = paths();
        let (code, _, err) = run_args(&["config-init", "--base-url", "não é url"], &paths);
        assert_eq!(code, 1);
        assert!(err.starts_with("erro:"));
    }

    #[test]
    fn config_init_rejects_non_http_scheme() {
        let (_dir, paths) = paths();
        let (code, _, err) = run_args(&["config-init", "--base-url", "ftp://example.org/"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("HTTP or HTTPS"));
    }

    #[test]
    fn corrupt_config_is_reported() {
        let (_dir, paths) = paths();
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.config_file(), "base_url = [").unwrap();
        let (code, _, err) = run_args(&["config-show"], &paths);
        assert_eq!(code, 1);
        assert!(err.starts_with("erro:"));
    }

    fn run_login(args: &[&str], paths: &AppPaths, password: Option<&str>) -> (i32, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(
            std::iter::once("chamados").chain(args.iter().copied()),
            paths,
            password.map(str::to_owned),
            &mut out,
            &mut err,
        );
        (code, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
    }

    fn mock_server(login_succeeds: bool) -> (tokio::runtime::Runtime, MockServer) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let server = runtime.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/accounts/login/"))
                .respond_with(ResponseTemplate::new(200).set_body_string(
                    r#"<form><input type="hidden" name="csrfmiddlewaretoken" value="tok"></form>"#,
                ))
                .mount(&server)
                .await;
            let post = if login_succeeds {
                ResponseTemplate::new(302).insert_header("location", "/")
            } else {
                ResponseTemplate::new(401)
            };
            Mock::given(method("POST")).respond_with(post).mount(&server).await;
            Mock::given(method("GET")).and(path("/")).respond_with(ResponseTemplate::new(200)).mount(&server).await;
            server
        });
        (runtime, server)
    }

    #[test]
    fn login_uses_flag_username_and_saves_session() {
        let (_dir, paths) = paths();
        let (_runtime, server) = mock_server(true);
        run_args(&["config-init", "--base-url", &server.uri()], &paths);
        let (code, out, err) = run_login(&["login", "--username", "kelson"], &paths, Some("segredo"));
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(out.contains("Login realizado como kelson") && !out.contains("segredo"));
        assert!(paths.session_file().exists());
    }

    #[test]
    fn login_falls_back_to_configured_username() {
        let (_dir, paths) = paths();
        let (_runtime, server) = mock_server(true);
        run_args(&["config-init", "--base-url", &server.uri(), "--username", "cfg"], &paths);
        let (code, out, _) = run_login(&["login"], &paths, Some("segredo"));
        assert_eq!(code, 0);
        assert!(out.contains("como cfg"));
    }

    #[test]
    fn login_reports_rejected_credentials() {
        let (_dir, paths) = paths();
        let (_runtime, server) = mock_server(false);
        run_args(&["config-init", "--base-url", &server.uri()], &paths);
        let (code, _, err) = run_login(&["login", "--username", "kelson"], &paths, Some("errada"));
        assert_eq!(code, 1);
        assert!(err.contains("authentication failed"));
        assert!(!paths.session_file().exists());
    }

    fn mount_listing(runtime: &tokio::runtime::Runtime, server: &MockServer, request_path: &str, body: ResponseTemplate) {
        runtime.block_on(
            Mock::given(method("GET")).and(path(request_path.to_owned())).respond_with(body).mount(server),
        );
    }

    #[test]
    fn list_prints_support_and_own_tickets() {
        let (_dir, paths) = paths();
        let (runtime, server) = mock_server(true);
        run_args(&["config-init", "--base-url", &server.uri()], &paths);
        let html = r#"<div class="general-box"><span class="status">Em atendimento</span>
            <h4><a href="/centralservicos/chamado/7/">REQ #7 <strong>Assunto</strong></a></h4></div>
            <div class="general-box"><h4><a href="/centralservicos/chamado/8/">REQ #8</a></h4></div>"#;
        mount_listing(&runtime, &server, "/centralservicos/listar_chamados_suporte/", ResponseTemplate::new(200).set_body_string(html));
        mount_listing(&runtime, &server, "/centralservicos/meus_chamados/", ResponseTemplate::new(200).set_body_string(html));

        for args in [&["list"][..], &["list", "--meus"][..]] {
            let (code, out, err) = run_args(args, &paths);
            assert_eq!((code, err.as_str()), (0, ""));
            assert_eq!(out, "#7\tEm atendimento\tAssunto\n#8\t-\t-\n");
        }
    }

    #[test]
    fn show_prints_ticket_details() {
        let (_dir, paths) = paths();
        let (runtime, server) = mock_server(true);
        run_args(&["config-init", "--base-url", &server.uri()], &paths);
        let html = r#"<main id="content"><div class="title-container"><h2>Chamado Interno 7</h2>
            <div class="object-status"><span class="status">Aberto</span></div></div>
            <div class="accordion"><button class="accordion-button">Serviço | Assunto</button>
            <div class="accordion-body"><dl class="definition-list"><div class="list-item"><dt>Descrição</dt><dd>Texto</dd></div></dl></div></div>
            <div data-tab="linha_tempo"><ul class="timeline"><li><div class="timeline-date">01/01/2026 10:00:00</div>
            <div class="timeline-content">Chamado aberto</div></li></ul></div></main>"#;
        mount_listing(&runtime, &server, "/centralservicos/chamado/7/", ResponseTemplate::new(200).set_body_string(html));
        let (code, out, err) = run_args(&["show", "7"], &paths);
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(out.starts_with("Chamado Interno 7\nSituação: Aberto\nServiço: Serviço | Assunto\nURL: "));
        assert!(out.contains("Descrição: Texto\n\nLinha do tempo:\n  01/01/2026 10:00:00  Chamado aberto\n"));
    }

    #[test]
    fn show_prints_placeholder_without_heading() {
        let (_dir, paths) = paths();
        let (runtime, server) = mock_server(true);
        run_args(&["config-init", "--base-url", &server.uri()], &paths);
        let html = r#"<main id="content"><div class="title-container"><h2>Chamado Interno 8</h2></div></main>"#;
        mount_listing(&runtime, &server, "/centralservicos/chamado/8/", ResponseTemplate::new(200).set_body_string(html));
        let (code, out, _) = run_args(&["show", "8"], &paths);
        assert_eq!(code, 0);
        assert!(out.contains("Serviço: -"));
    }

    #[test]
    fn show_asks_for_login_and_rejects_bad_ids() {
        let (_dir, paths) = paths();
        let (runtime, server) = mock_server(true);
        run_args(&["config-init", "--base-url", &server.uri()], &paths);
        mount_listing(
            &runtime,
            &server,
            "/centralservicos/chamado/9/",
            ResponseTemplate::new(302).insert_header("location", "/accounts/login/"),
        );
        let (code, _, err) = run_args(&["show", "9"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("chamados login"));

        let (code, _, err) = run_args(&["show", "abc"], &paths);
        assert_eq!(code, 2);
        assert!(!err.is_empty());
    }

    fn bare_server() -> (tokio::runtime::Runtime, MockServer) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let server = runtime.block_on(MockServer::start());
        (runtime, server)
    }

    fn mount_text(runtime: &tokio::runtime::Runtime, server: &MockServer, verb: &str, request_path: &str, body: ResponseTemplate) {
        runtime.block_on(
            Mock::given(method(verb)).and(path(request_path.to_owned())).respond_with(body).mount(server),
        );
    }

    #[test]
    fn open_creates_ticket_with_suap_defaults() {
        let (_dir, paths) = paths();
        let (runtime, server) = bare_server();
        run_args(&["config-init", "--base-url", &server.uri()], &paths);
        let form = r#"<form method="post"><input type="hidden" name="csrfmiddlewaretoken" value="tok">
            <textarea name="descricao"></textarea></form>"#;
        mount_text(&runtime, &server, "GET", "/centralservicos/abrir_chamado/7/", ResponseTemplate::new(200).set_body_string(form));
        mount_text(&runtime, &server, "GET", "/centralservicos/get_campus_com_centros_atendimento/7/0/", ResponseTemplate::new(200).set_body_string(r#"{"campus": [[3, "ZL", true]]}"#));
        mount_text(&runtime, &server, "GET", "/centralservicos/get_centros_atendimento_por_servico_e_campus/7/3/", ResponseTemplate::new(200).set_body_string(r#"{"centros": [[9, "TI", true]]}"#));
        mount_text(&runtime, &server, "POST", "/centralservicos/abrir_chamado/7/", ResponseTemplate::new(302).insert_header("location", "/centralservicos/chamado/99/"));
        mount_text(&runtime, &server, "GET", "/centralservicos/chamado/99/", ResponseTemplate::new(200));
        let (code, out, err) = run_args(&["open", "7", "--description", "Teste", "--field", "telefone=1=2"], &paths);
        assert_eq!((code, err.as_str()), (0, ""));
        assert_eq!(out, format!("Chamado #99 aberto: {}/centralservicos/chamado/99/\n", server.uri()));
    }

    #[test]
    fn open_reports_rejection_and_invalid_fields() {
        let (_dir, paths) = paths();
        let (runtime, server) = bare_server();
        run_args(&["config-init", "--base-url", &server.uri()], &paths);
        let form = r#"<form method="post"><textarea name="descricao"></textarea></form>"#;
        mount_text(&runtime, &server, "GET", "/centralservicos/abrir_chamado/7/", ResponseTemplate::new(200).set_body_string(form));
        mount_text(&runtime, &server, "POST", "/centralservicos/abrir_chamado/7/", ResponseTemplate::new(200).set_body_string("<p>sem retorno</p>"));
        let (code, _, err) = run_args(&["open", "7", "-d", "Teste", "--campus", "1", "--center", "2", "--interested", "3"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("could not confirm"));

        for bad in ["semigual", "=semnome"] {
            let (code, _, err) = run_args(&["open", "7", "-d", "x", "--field", bad], &paths);
            assert_eq!(code, 2);
            assert!(err.contains("NOME=VALOR"));
        }
    }

    #[test]
    fn list_reports_empty_result() {
        let (_dir, paths) = paths();
        let (runtime, server) = mock_server(true);
        run_args(&["config-init", "--base-url", &server.uri()], &paths);
        mount_listing(&runtime, &server, "/centralservicos/listar_chamados_suporte/", ResponseTemplate::new(200));
        let (code, out, _) = run_args(&["list"], &paths);
        assert_eq!(code, 0);
        assert!(out.contains("Nenhum chamado"));
    }

    #[test]
    fn list_asks_for_login_when_session_is_missing() {
        let (_dir, paths) = paths();
        let (runtime, server) = mock_server(true);
        run_args(&["config-init", "--base-url", &server.uri()], &paths);
        mount_listing(
            &runtime,
            &server,
            "/centralservicos/listar_chamados_suporte/",
            ResponseTemplate::new(302).insert_header("location", "/accounts/login/"),
        );
        let (code, _, err) = run_args(&["list"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("chamados login"));
    }

    #[test]
    fn list_reports_other_errors() {
        let (_dir, paths) = paths();
        let (runtime, server) = mock_server(true);
        run_args(&["config-init", "--base-url", &server.uri()], &paths);
        mount_listing(&runtime, &server, "/centralservicos/listar_chamados_suporte/", ResponseTemplate::new(500));
        let (code, _, err) = run_args(&["list"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("unexpected status 500"));
    }

    #[test]
    fn login_requires_username() {
        let (_dir, paths) = paths();
        let (code, _, err) = run_login(&["login"], &paths, Some("segredo"));
        assert_eq!(code, 1);
        assert!(err.contains("--username"));
    }

    #[test]
    fn login_requires_password_env() {
        let (_dir, paths) = paths();
        let (code, _, err) = run_login(&["login", "--username", "kelson"], &paths, None);
        assert_eq!(code, 1);
        assert!(err.contains(PASSWORD_ENV));
    }

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("closed"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn output_failure_is_reported_as_error() {
        let (_dir, paths) = paths();
        let mut err = Vec::new();
        let code = run(["chamados", "status"], &paths, None, &mut FailingWriter, &mut err);
        assert_eq!(code, 1);
        assert!(String::from_utf8(err).unwrap().contains("closed"));
        FailingWriter.flush().unwrap();
    }
}
