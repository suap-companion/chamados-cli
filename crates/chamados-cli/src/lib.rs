//! Command-line interface logic for `chamados`.
//!
//! The binary entry point (`main.rs`) is a thin wrapper; everything testable lives here.

use std::{error::Error, ffi::OsString, fs, io::{Read, Write}, path::PathBuf};

use clap::{Args, Parser, Subcommand};
use chamados_core::{Attachment, NewTicket, SuapTicketSource, TicketDetails, TicketError, TicketQueue, TicketSource};
use suap_core::{load_config, save_config, AppPaths, SuapClient, SuapConfig, SuapError, DEFAULT_PROFILE};

#[derive(Debug, Parser)]
#[command(name = "chamados", version, about = "Cliente local para chamados do SUAP")]
struct Cli {
    /// Perfil (ambiente) a usar; cada perfil tem sua configuração e sua sessão.
    #[arg(long, global = true, default_value = DEFAULT_PROFILE)]
    profile: String,
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
    ConfigInit(ConfigInitArgs),
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
    Open(OpenArgs),
    /// Exibe uma mensagem sobre o estado inicial do projeto.
    Status,
}

fn parse_field(raw: &str) -> Result<(String, String), String> {
    match raw.split_once('=') {
        Some((name, value)) if !name.is_empty() => Ok((name.to_owned(), value.to_owned())),
        _ => Err(format!("campo inválido {raw:?}: use NOME=VALOR")),
    }
}

#[derive(Debug, Args)]
struct ConfigInitArgs {
    #[arg(long)]
    base_url: Option<String>,
    #[arg(long)]
    username: Option<String>,
    /// Serviço padrão do `open` neste perfil.
    #[arg(long)]
    service: Option<u64>,
    /// Interessado padrão do `open` neste perfil (id do vínculo).
    #[arg(long)]
    interested: Option<String>,
    /// Campus padrão do `open` neste perfil.
    #[arg(long)]
    campus: Option<String>,
    /// Centro de atendimento padrão do `open` neste perfil.
    #[arg(long)]
    center: Option<String>,
}

#[derive(Debug, Args)]
struct OpenArgs {
    /// Número do serviço no SUAP (o mesmo de /centralservicos/abrir_chamado/<serviço>/); por padrão, o do perfil.
    service: Option<u64>,
    /// Descrição (pode ter várias linhas). Com `-` ou omitida, é lida da entrada padrão.
    #[arg(long, short)]
    description: Option<String>,
    /// Campus (id da unidade organizacional); por padrão, o do perfil ou o do usuário.
    #[arg(long)]
    campus: Option<String>,
    /// Centro de atendimento (id); por padrão, o do perfil ou o único disponível para o campus.
    #[arg(long)]
    center: Option<String>,
    /// Interessado (id do vínculo no SUAP); por padrão, o do perfil. O formulário do SUAP exige este campo.
    #[arg(long)]
    interested: Option<String>,
    /// Anexa um arquivo (repetível, no máximo 3; tipos aceitos pelo SUAP: xlsx, xls, csv, docx, doc, pdf, jpg, jpeg, png).
    #[arg(long, short = 'a')]
    attach: Vec<PathBuf>,
    /// Não envia a cópia de abertura por e-mail aos interessados (o padrão é enviar).
    #[arg(long)]
    no_email_copy: bool,
    /// Assume o chamado (atribui a você) logo após abrir.
    #[arg(long)]
    assume: bool,
    /// Coloca o chamado em atendimento logo após abrir (implica --assume).
    #[arg(long)]
    start: bool,
    /// Campo extra do formulário no formato NOME=VALOR (repetível), ex.: --field patrimonio=123.
    #[arg(long = "field", value_parser = parse_field)]
    fields: Vec<(String, String)>,
}

/// Name of the environment variable that holds the SUAP password.
pub const PASSWORD_ENV: &str = "SUAP_PASSWORD";

/// Runs the CLI with `args`, writing to `out`/`err`, and returns the process exit code.
///
/// `password` is the value of [`PASSWORD_ENV`] and `input` the standard input (empty when it is a terminal);
/// both are read by the caller so they can be injected in tests.
pub fn run<I, T>(
    args: I,
    paths: &AppPaths,
    password: Option<String>,
    input: &mut dyn Read,
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

    let paths = match paths.clone().with_profile(&cli.profile) {
        Ok(paths) => paths,
        Err(error) => {
            let _ = writeln!(err, "erro: {error}");
            return 1;
        }
    };

    match execute(cli, &paths, password, input, out) {
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
    input: &mut dyn Read,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    match cli.command {
        Some(Command::Paths) => show_paths(paths, out),
        Some(Command::ConfigShow) => show_config(paths, out),
        Some(Command::ConfigInit(args)) => init_config(paths, args, out),
        Some(Command::Login { username }) => login(paths, username, password, out),
        Some(Command::List { meus }) => list(paths, meus, out),
        Some(Command::Show { id }) => show(paths, id, out),
        Some(Command::Open(args)) => open(paths, args, input, out),
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

/// Configuration of the selected profile; only `default` falls back to the built-in defaults.
fn profile_config(paths: &AppPaths) -> Result<SuapConfig, Box<dyn Error>> {
    match load_config(paths)? {
        Some(config) => Ok(config),
        None if paths.profile() == DEFAULT_PROFILE => Ok(SuapConfig::default()),
        None => Err(format!(
            "perfil {0:?} não configurado: execute `chamados config-init --profile {0}`",
            paths.profile()
        )
        .into()),
    }
}

fn show_paths(paths: &AppPaths, out: &mut dyn Write) -> Result<(), Box<dyn Error>> {
    writeln!(out, "profile: {}", paths.profile())?;
    writeln!(out, "config_dir: {}", paths.config_dir().display())?;
    writeln!(out, "config_file: {}", paths.config_file().display())?;
    writeln!(out, "data_dir: {}", paths.data_dir().display())?;
    writeln!(out, "session_file: {}", paths.session_file().display())?;
    Ok(())
}

fn show_config(paths: &AppPaths, out: &mut dyn Write) -> Result<(), Box<dyn Error>> {
    match load_config(paths)? {
        Some(config) => {
            writeln!(out, "profile: {}", paths.profile())?;
            writeln!(out, "base_url: {}", config.base_url)?;
            writeln!(out, "username: {}", config.username.as_deref().unwrap_or("<não configurado>"))?;
            let open = &config.open;
            let defaults = [
                ("service", open.service.map(|service| service.to_string())),
                ("interested", open.interested.clone()),
                ("campus", open.campus.clone()),
                ("center", open.center.clone()),
            ];
            for (name, value) in defaults.iter().filter_map(|(name, value)| Some((name, value.as_ref()?))) {
                writeln!(out, "open.{name}: {value}")?;
            }
            writeln!(out, "file: {}", paths.config_file().display())?;
        }
        None => {
            let (profile, file) = (paths.profile(), paths.config_file());
            writeln!(out, "Nenhuma configuração encontrada para o perfil {profile} em {}", file.display())?;
        }
    }
    Ok(())
}

fn login(
    paths: &AppPaths,
    username: Option<String>,
    password: Option<String>,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let config = profile_config(paths)?;
    let username = username
        .or_else(|| config.username.clone())
        .ok_or("usuário não informado: use --username ou `config-init --username`")?;
    let password = password.ok_or_else(|| format!("senha não informada: defina a variável de ambiente {PASSWORD_ENV}"))?;

    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let client = SuapClient::open(paths, &config)?;
    runtime.block_on(client.login(&username, &password))?;
    let (profile, session) = (paths.profile(), paths.session_file());
    writeln!(out, "Login realizado como {username} (perfil {profile}). Sessão salva em {}", session.display())?;
    Ok(())
}

fn list(paths: &AppPaths, mine: bool, out: &mut dyn Write) -> Result<(), Box<dyn Error>> {
    let config = profile_config(paths)?;
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
    let config = profile_config(paths)?;
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let client = SuapClient::open(paths, &config)?;
    let source = SuapTicketSource::new(&client, TicketQueue::Support);
    let details = runtime.block_on(source.get_ticket(&id.to_string())).map_err(explain)?;
    print_details(&details, out)?;
    Ok(())
}

/// Text given as an option value, or read from `input` when the value is `-` or missing.
///
/// Newlines inside the text are kept (multi-line); only the trailing line break is removed.
fn read_text(value: Option<String>, input: &mut dyn Read) -> Result<String, Box<dyn Error>> {
    let text = match value {
        Some(text) if text != "-" => text,
        _ => {
            let mut piped = String::new();
            input.read_to_string(&mut piped)?;
            piped
        }
    };
    let text = text.trim_end_matches(['\r', '\n']).to_owned();
    if text.trim().is_empty() {
        return Err("texto não informado: passe o valor na opção, use `-` ou envie pela entrada padrão".into());
    }
    Ok(text)
}

fn read_attachments(files: &[PathBuf]) -> Result<Vec<Attachment>, Box<dyn Error>> {
    let mut attachments = Vec::new();
    for path in files {
        let bytes = fs::read(path).map_err(|error| format!("não foi possível ler o anexo {}: {error}", path.display()))?;
        let file_name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        attachments.push(Attachment { file_name, bytes });
    }
    Ok(attachments)
}

/// Error for a step done after the ticket was already opened, so the new ticket id is not lost.
fn after_open(id: &str, action: &str, error: TicketError) -> Box<dyn Error> {
    format!("o chamado #{id} foi aberto, mas não foi possível {action}: {}", explain(error)).into()
}

fn open(paths: &AppPaths, args: OpenArgs, input: &mut dyn Read, out: &mut dyn Write) -> Result<(), Box<dyn Error>> {
    let config = profile_config(paths)?;
    let defaults = &config.open;
    let service_id = args.service.or(defaults.service).ok_or("serviço não informado: passe o número ou defina o padrão do perfil (config-init --service)")?;
    let interested = args.interested.or_else(|| defaults.interested.clone());
    if interested.is_none() {
        return Err("interessado não informado: use --interested ou defina o padrão do perfil (config-init --interested)".into());
    }
    let ticket = NewTicket {
        service_id,
        description: read_text(args.description, input)?,
        campus: args.campus.or_else(|| defaults.campus.clone()),
        center: args.center.or_else(|| defaults.center.clone()),
        interested,
        extra_fields: args.fields,
        copy_email: !args.no_email_copy,
        attachments: read_attachments(&args.attach)?,
    };

    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let client = SuapClient::open(paths, &config)?;
    let source = SuapTicketSource::new(&client, TicketQueue::Support);
    let id = runtime.block_on(source.open_ticket(&ticket)).map_err(explain)?;
    writeln!(out, "Chamado #{id} aberto: {}", client.base_url().join(&format!("centralservicos/chamado/{id}/"))?)?;

    if args.assume || args.start {
        runtime.block_on(source.assume_ticket(&id)).map_err(|error| after_open(&id, "assumi-lo", error))?;
        writeln!(out, "Chamado #{id} assumido.")?;
    }
    if args.start {
        runtime.block_on(source.start_service(&id)).map_err(|error| after_open(&id, "colocá-lo em atendimento", error))?;
        writeln!(out, "Chamado #{id} em atendimento.")?;
    }
    Ok(())
}

fn print_details(details: &TicketDetails, out: &mut dyn Write) -> std::io::Result<()> {
    writeln!(out, "{}", details.title)?;
    writeln!(out, "Situação: {}", details.statuses.join("; "))?;
    writeln!(out, "Serviço: {}", details.heading.as_deref().unwrap_or("-"))?;
    writeln!(out, "URL: {}", details.details_url)?;
    for (label, value) in &details.fields {
        writeln!(out, "{label}: {}", indent_continuation(value, "    "))?;
    }
    writeln!(out, "\nLinha do tempo:")?;
    for entry in &details.timeline {
        writeln!(out, "  {}  {}", entry.date, indent_continuation(&entry.text, "      "))?;
    }
    Ok(())
}

/// Indents every line but the first, so multi-line values stay readable under their label.
fn indent_continuation(text: &str, prefix: &str) -> String {
    text.replace('\n', &format!("\n{prefix}"))
}

/// Turns ticket errors into user-facing messages.
fn explain(error: TicketError) -> Box<dyn Error> {
    match error {
        TicketError::Suap(SuapError::NotAuthenticated) => "sessão ausente ou expirada: execute `chamados login`".into(),
        other => other.into(),
    }
}

fn init_config(paths: &AppPaths, args: ConfigInitArgs, out: &mut dyn Write) -> Result<(), Box<dyn Error>> {
    let mut config = load_config(paths)?.unwrap_or_default();

    if let Some(base_url) = args.base_url {
        config.base_url = base_url.parse()?;
    }
    if args.username.is_some() {
        config.username = args.username;
    }
    let open = &mut config.open;
    open.service = args.service.or(open.service);
    open.interested = args.interested.or(open.interested.take());
    open.campus = args.campus.or(open.campus.take());
    open.center = args.center.or(open.center.take());

    save_config(paths, &config)?;
    writeln!(out, "Configuração salva em {} (perfil {})", paths.config_file().display(), paths.profile())?;
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
        let code = run(std::iter::once("chamados").chain(args.iter().copied()), paths, None, &mut std::io::empty(), &mut out, &mut err);
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
    fn profiles_isolate_configuration_and_session() {
        let (_dir, paths) = paths();
        let (_runtime, server) = mock_server(true);
        let (code, out, _) = run_args(&["config-init", "--profile", "local", "--base-url", &server.uri()], &paths);
        assert!(code == 0 && out.contains("(perfil local)"));

        let (_, out, _) = run_args(&["config-show"], &paths);
        assert!(out.contains("Nenhuma configuração encontrada para o perfil default"));
        let (_, out, _) = run_args(&["--profile", "local", "config-show"], &paths);
        assert!(out.contains("profile: local") && out.contains(&server.uri()));
        let (_, out, _) = run_args(&["paths", "--profile", "local"], &paths);
        assert!(out.contains("profile: local") && out.contains("session-local.cookies"));

        let (code, out, err) = run_login(&["login", "--profile", "local", "--username", "dev"], &paths, Some("segredo"));
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(out.contains("(perfil local)"));
        assert!(paths.clone().with_profile("local").unwrap().session_file().exists());
        assert!(!paths.session_file().exists());
    }

    #[test]
    fn unconfigured_profile_is_an_error_but_default_has_defaults() {
        let (_dir, paths) = paths();
        let (code, _, err) = run_args(&["list", "--profile", "local"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("config-init --profile local"));
        assert!(profile_config(&paths).unwrap().username.is_none());
    }

    #[test]
    fn invalid_profile_name_is_rejected() {
        let (_dir, paths) = paths();
        let (code, _, err) = run_args(&["--profile", "../x", "paths"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("invalid profile name"));
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
            &mut std::io::empty(),
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
    fn show_keeps_line_breaks_of_multiline_text() {
        let (_dir, paths) = paths();
        let (runtime, server) = bare_server();
        run_args(&["config-init", "--base-url", &server.uri()], &paths);
        let html = r#"<main id="content"><div class="title-container"><h2>Chamado Interno 9</h2></div>
            <div class="accordion"><div class="accordion-body"><dl class="definition-list"><div class="list-item">
            <dt>Descrição</dt><dd>linha 1<br>linha 2</dd></div></dl></div></div>
            <div data-tab="linha_tempo"><ul class="timeline"><li><div class="timeline-date">01/01/2026 10:00:00</div>
            <div class="timeline-content">Fulano comentou:<p>a<br>b</p></div></li></ul></div></main>"#;
        mount_listing(&runtime, &server, "/centralservicos/chamado/9/", ResponseTemplate::new(200).set_body_string(html));
        let (code, out, _) = run_args(&["show", "9"], &paths);
        assert_eq!(code, 0);
        assert!(out.contains("Descrição: linha 1\n    linha 2\n"));
        assert!(out.contains("  01/01/2026 10:00:00  Fulano comentou: a\n      b\n"));
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

    fn run_with_input(args: &[&str], paths: &AppPaths, input: &str) -> (i32, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(
            std::iter::once("chamados").chain(args.iter().copied()),
            paths,
            None,
            &mut input.as_bytes(),
            &mut out,
            &mut err,
        );
        (code, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
    }

    const OPEN_FORM: &str = r#"<form method="post"><textarea name="descricao"></textarea>
        <input type="checkbox" name="enviar_copia_email" checked></form>"#;

    /// Profile with `open` defaults pointing at a mock SUAP that accepts tickets for service 7.
    fn open_setup() -> (TempDir, AppPaths, tokio::runtime::Runtime, MockServer) {
        let (dir, paths) = paths();
        let (runtime, server) = bare_server();
        let uri = server.uri();
        let init = ["config-init", "--base-url", &uri, "--service", "7", "--interested", "1", "--campus", "2", "--center", "3"];
        assert_eq!(run_args(&init, &paths).0, 0);
        let form = ResponseTemplate::new(200).set_body_string(OPEN_FORM);
        mount_text(&runtime, &server, "GET", "/centralservicos/abrir_chamado/7/", form);
        let created = ResponseTemplate::new(200).set_body_string("Número do chamado: 99");
        mount_text(&runtime, &server, "POST", "/centralservicos/abrir_chamado/7/", created);
        (dir, paths, runtime, server)
    }

    fn posted_body(runtime: &tokio::runtime::Runtime, server: &MockServer) -> String {
        let requests = runtime.block_on(server.received_requests()).unwrap();
        let post = requests.iter().find(|request| request.method == wiremock::http::Method::POST).unwrap();
        String::from_utf8_lossy(&post.body).into_owned()
    }

    #[test]
    fn open_uses_profile_defaults_multiline_text_and_email_copy() {
        let (_dir, paths, runtime, server) = open_setup();
        let (code, out, err) = run_args(&["open", "-d", "linha 1\nlinha 2\n"], &paths);
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(out.starts_with("Chamado #99 aberto: "));
        let body = posted_body(&runtime, &server);
        assert!(body.contains("descricao=linha+1%0Alinha+2&") || body.contains("descricao=linha+1%0Alinha+2"));
        assert!(body.contains("interessado=1") && body.contains("uo=2") && body.contains("centro_atendimento=3"));
        assert!(body.contains("enviar_copia_email=on"));
    }

    #[test]
    fn open_reads_the_description_from_standard_input() {
        let (_dir, paths, runtime, server) = open_setup();
        for args in [&["open"][..], &["open", "-d", "-"][..]] {
            let (code, _, err) = run_with_input(args, &paths, "vindo\ndo stdin\r\n");
            assert_eq!((code, err.as_str()), (0, ""));
        }
        assert!(posted_body(&runtime, &server).contains("descricao=vindo%0Ado+stdin&"));

        let (code, _, err) = run_with_input(&["open"], &paths, "  \n");
        assert_eq!(code, 1);
        assert!(err.contains("texto não informado"));
    }

    #[test]
    fn open_can_skip_the_email_copy() {
        let (_dir, paths, runtime, server) = open_setup();
        let (code, _, _) = run_args(&["open", "-d", "x", "--no-email-copy"], &paths);
        assert_eq!(code, 0);
        assert!(!posted_body(&runtime, &server).contains("enviar_copia_email"));
    }

    #[test]
    fn open_requires_service_and_interested_from_options_or_profile() {
        let (_dir, paths) = paths();
        let (_runtime, server) = bare_server();
        run_args(&["config-init", "--base-url", &server.uri()], &paths);
        let (code, _, err) = run_args(&["open", "-d", "x", "--interested", "1"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("serviço não informado"));
        let (code, _, err) = run_args(&["open", "7", "-d", "x"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("interessado não informado"));
    }

    #[test]
    fn open_attaches_files_and_validates_them() {
        let (dir, paths, runtime, server) = open_setup();
        let report = dir.path().join("relatorio.pdf");
        std::fs::write(&report, b"%PDF conteudo").unwrap();
        let (code, _, err) = run_args(&["open", "-d", "x", "-a", report.to_str().unwrap()], &paths);
        assert_eq!((code, err.as_str()), (0, ""));
        let body = posted_body(&runtime, &server);
        assert!(body.contains("name=\"chamadoanexo_set-0-anexo\"; filename=\"relatorio.pdf\"") && body.contains("%PDF conteudo"));

        let exe = dir.path().join("virus.exe");
        std::fs::write(&exe, b"MZ").unwrap();
        let (code, _, err) = run_args(&["open", "-d", "x", "-a", exe.to_str().unwrap()], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("unsupported type"));

        let missing = dir.path().join("nao-existe.pdf");
        let (code, _, err) = run_args(&["open", "-d", "x", "-a", missing.to_str().unwrap()], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("não foi possível ler o anexo"));
    }

    #[test]
    fn open_can_assume_and_start_the_ticket() {
        let (_dir, paths, runtime, server) = open_setup();
        let ok = || ResponseTemplate::new(200);
        mount_text(&runtime, &server, "GET", "/centralservicos/auto_atribuir_chamado/99/", ok());
        mount_text(&runtime, &server, "GET", "/centralservicos/colocar_em_atendimento/99/", ok());

        let (code, out, _) = run_args(&["open", "-d", "x", "--assume"], &paths);
        assert!(code == 0 && out.contains("assumido.") && !out.contains("em atendimento."));
        let (code, out, _) = run_args(&["open", "-d", "x", "--start"], &paths);
        assert!(code == 0 && out.contains("assumido.") && out.contains("Chamado #99 em atendimento."));
    }

    #[test]
    fn open_reports_failures_after_the_ticket_was_created() {
        let (_dir, paths, runtime, server) = open_setup();
        let refused = ResponseTemplate::new(200).set_body_string("<p class='alert-error'>Sem permissão</p>");
        mount_text(&runtime, &server, "GET", "/centralservicos/auto_atribuir_chamado/99/", refused);
        let (code, out, err) = run_args(&["open", "-d", "x", "--assume"], &paths);
        assert_eq!(code, 1);
        assert!(out.contains("Chamado #99 aberto") && err.contains("o chamado #99 foi aberto, mas não foi possível assumi-lo"));
        assert!(err.contains("Sem permissão"));
    }

    #[test]
    fn open_reports_failure_when_starting_service() {
        let (_dir, paths, runtime, server) = open_setup();
        mount_text(&runtime, &server, "GET", "/centralservicos/auto_atribuir_chamado/99/", ResponseTemplate::new(200));
        let (code, out, err) = run_args(&["open", "-d", "x", "--start"], &paths);
        assert_eq!(code, 1);
        assert!(out.contains("assumido.") && err.contains("colocá-lo em atendimento"));
    }

    #[test]
    fn config_show_lists_open_defaults() {
        let (_dir, paths, _runtime, _server) = open_setup();
        let (_, out, _) = run_args(&["config-show"], &paths);
        for expected in ["open.service: 7", "open.interested: 1", "open.campus: 2", "open.center: 3"] {
            assert!(out.contains(expected), "{expected}");
        }
        run_args(&["config-init", "--username", "mantem"], &paths);
        let (_, out, _) = run_args(&["config-show"], &paths);
        assert!(out.contains("open.service: 7") && out.contains("username: mantem"));
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
        let (code, out, err) = run_args(&["open", "7", "--description", "Teste", "--interested", "1", "--field", "telefone=1=2"], &paths);
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
            let (code, _, err) = run_args(&["open", "7", "-d", "x", "--interested", "1", "--field", bad], &paths);
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
        let code = run(["chamados", "status"], &paths, None, &mut std::io::empty(), &mut FailingWriter, &mut err);
        assert_eq!(code, 1);
        assert!(String::from_utf8(err).unwrap().contains("closed"));
        FailingWriter.flush().unwrap();
    }
}
