//! Command-line interface logic for `chamados`.
//!
//! The binary entry point (`main.rs`) is a thin wrapper; everything testable lives here.

mod render;

use std::{
    error::Error,
    ffi::OsString,
    fs,
    io::{Read, Write},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use chamados_core::{
    validate_title,
    watch::{poll, Snapshot},
    Attachment, Direction, Message, NewTicket, Reclassification, Resolution, SuapTicketSource,
    TicketDetails, TicketError, TicketFilter, TicketQueue, TicketSource, TitleStore, ASSIGNMENTS,
    ORDERS, RELATIONS, STATUSES,
};
use chamados_sync::{
    backend_from, key_exists, key_source_from, load_key, record_removal, store_key, sync_once,
    validate_settings, Key, KeySource, Passphrase, S3Credentials, SyncBackend, SyncLock,
    SyncOptions, DIRECTORY_BACKEND, PASSPHRASE_ENV, R2_BACKEND, S3_BACKEND,
};
use clap::{builder::PossibleValuesParser, Args, Parser, Subcommand, ValueEnum};
use suap_core::{
    list_profiles, load_config, load_sync_settings, remove_config, save_config, save_sync_settings,
    AppPaths, SuapClient, SuapConfig, SuapError, SyncSettings, DEFAULT_PROFILE,
};

#[derive(Debug, Parser)]
#[command(
    name = "chamados",
    version,
    about = "Cliente local para chamados do SUAP"
)]
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
    /// Gerencia perfis (ambientes): cada um tem configuração e sessão próprias.
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },
    /// Informa se a sessão salva do perfil ainda é válida.
    SessionStatus,
    /// Autentica no SUAP usando a senha da variável de ambiente `SUAP_PASSWORD`.
    Login {
        /// Usuário do SUAP; se omitido, usa o `username` da configuração local.
        #[arg(long)]
        username: Option<String>,
    },
    /// Lista os chamados do SUAP usando a sessão salva por `login`, com filtros, busca e páginas.
    List(Box<ListArgs>),
    /// Exibe os detalhes de um chamado do SUAP usando a sessão salva por `login`.
    Show {
        /// Número do chamado (ex.: 559298).
        id: u64,
        /// Imprime JSON (para scripts) em vez de texto.
        #[arg(long)]
        json: bool,
    },
    /// Acompanha chamados e avisa quando algo muda (situação, chamado novo, saída da lista, mensagens).
    Watch(WatchArgs),
    /// Abre um novo chamado no SUAP usando a sessão salva por `login`.
    Open(OpenArgs),
    /// Adiciona um comentário (visível ao interessado) a um ou mais chamados, usando a sessão salva por `login`.
    Comment(BatchMessageArgs),
    /// Adiciona uma nota interna (visível só à equipe de atendimento) a um ou mais chamados.
    Note(BatchMessageArgs),
    /// Suspende um chamado (situação "Suspenso"), com a mensagem de suspensão.
    Suspend(MessageArgs),
    /// Resolve um chamado (situação "Resolvido"), com a mensagem de resolução.
    Resolve(ResolveArgs),
    /// Baixa os anexos de um chamado (todos, ou os escolhidos com --anexo).
    Download {
        /// Número do chamado.
        id: u64,
        /// Número do anexo, como o `show` lista (repetível). Sem esta opção, baixa todos.
        #[arg(long)]
        anexo: Vec<usize>,
        /// Pasta de destino (criada se não existir); por padrão, a pasta atual.
        #[arg(long, short = 'o')]
        saida: Option<PathBuf>,
        /// Sobrescreve arquivos que já existam no destino.
        #[arg(long)]
        force: bool,
    },
    /// Assume um chamado que já existe (atribui a você).
    Assume {
        /// Número do chamado.
        id: u64,
    },
    /// Coloca um chamado em atendimento (situação "Em atendimento").
    Start {
        /// Número do chamado.
        id: u64,
        /// Assume o chamado antes, se ele ainda não for seu.
        #[arg(long)]
        assume: bool,
    },
    /// Anexa arquivos a um chamado que já está aberto (tipos aceitos pelo SUAP: xlsx, xls, csv, docx, doc, pdf, jpg, jpeg, png).
    Attach {
        /// Número do chamado.
        id: u64,
        /// Arquivos a anexar (um envio por arquivo).
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Descrição (uma linha, até 80 caracteres), igual para todos os arquivos; por padrão, o nome do arquivo.
        #[arg(long)]
        descricao: Option<String>,
    },
    /// Passa o chamado para outro atendente do mesmo grupo de atendimento.
    Assign {
        /// Número do chamado.
        id: u64,
        /// Quem recebe: matrícula, nome (ou parte dele) ou o id que o SUAP oferece.
        #[arg(long)]
        para: String,
    },
    /// Escala o chamado para o grupo de atendimento acima, com uma nota interna.
    Escalate(HandoffArgs),
    /// Devolve o chamado ao grupo de atendimento abaixo, com uma nota interna.
    Return(HandoffArgs),
    /// Troca o serviço, o campus ou o centro de atendimento do chamado, com a justificativa.
    Reclassify {
        #[command(flatten)]
        message: MessageArgs,
        /// Novo serviço (id do serviço no SUAP).
        #[arg(long)]
        servico: Option<String>,
        /// Novo campus (id da unidade organizacional).
        #[arg(long)]
        campus: Option<String>,
        /// Novo centro de atendimento (id); por padrão, mantém o atual se o SUAP ainda o oferece.
        #[arg(long)]
        centro: Option<String>,
    },
    /// Adiciona ou remove tags de um chamado.
    Tag {
        #[command(subcommand)]
        command: TagCommand,
    },
    /// Adiciona ou remove outros interessados de um chamado.
    Interested {
        #[command(subcommand)]
        command: InterestedCommand,
    },
    /// Reabre um chamado resolvido (situação "Reaberto"), com o motivo.
    Reopen(MessageArgs),
    /// Fecha um chamado resolvido (situação "Fechado"); o interessado pode avaliar o atendimento.
    Close {
        /// Número do chamado.
        id: u64,
        /// Nota do atendimento, de 1 a 5 (só vale quando quem fecha é o interessado).
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=5))]
        nota: Option<u8>,
        /// Comentário sobre a avaliação (várias linhas). Com `-`, lê a entrada padrão; omitido, fecha sem comentário.
        #[arg(long, short = 'm')]
        message: Option<String>,
    },
    /// Cancela um chamado (situação "Cancelado"), com o motivo. Não pode ser desfeito.
    Cancel {
        #[command(flatten)]
        message: MessageArgs,
        /// Confirma o cancelamento (irreversível).
        #[arg(long)]
        yes: bool,
    },
    /// Sincroniza, cifrados, os títulos e as configurações dos perfis com `sync` ligado.
    Sync(SyncArgs),
    /// Define, exibe ou remove o título local de um chamado (o SUAP não tem título; fica só nesta máquina).
    Title {
        /// Número do chamado.
        id: u64,
        /// Novo título (uma linha). Sem texto, exibe o título atual.
        #[arg(conflicts_with = "remove")]
        text: Option<String>,
        /// Remove o título local.
        #[arg(long)]
        remove: bool,
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

#[derive(Debug, Subcommand)]
enum ProfileCommand {
    /// Cria um perfil (sem nome, usa o perfil selecionado por --profile, `default` por padrão).
    Init(ProfileArgs),
    /// Altera campos de um perfil existente.
    Update(ProfileArgs),
    /// Apaga um perfil e a sessão dele.
    Remove {
        /// Nome do perfil.
        name: String,
        /// Confirma a remoção (apaga a configuração e a sessão do perfil).
        #[arg(long)]
        yes: bool,
    },
    /// Exibe a configuração de um perfil, sem dados sensíveis.
    Show {
        /// Nome do perfil; por padrão, o selecionado por --profile.
        name: Option<String>,
        /// Imprime JSON (para scripts) em vez de texto.
        #[arg(long)]
        json: bool,
    },
    /// Lista os perfis configurados.
    List {
        /// Imprime JSON (para scripts) em vez de texto.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Args)]
struct ProfileArgs {
    /// Nome do perfil; por padrão, o selecionado por --profile (`default`).
    name: Option<String>,
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
    /// Inclui (true) ou não (false) este perfil na sincronização em nuvem; começa desligado.
    #[arg(long)]
    sync: Option<bool>,
}

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
struct SyncArgs {
    #[command(subcommand)]
    action: Option<SyncAction>,
    /// Sincroniza só este perfil (por padrão, todos os que têm `sync` ligado).
    #[arg(long)]
    only: Option<String>,
    /// Não imprime nada em caso de sucesso (para `cron`/timers).
    #[arg(long, short)]
    quiet: bool,
    /// Mostra o que mudaria, sem gravar nada (nem local, nem na nuvem).
    #[arg(long)]
    dry_run: bool,
    /// Confere o backend (escrita condicional) e a chave, sem sincronizar.
    #[arg(long)]
    check: bool,
    /// Imprime JSON (para scripts) em vez de texto.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum SyncAction {
    /// Configura onde e como sincronizar (configuração global, fica só nesta máquina).
    Setup(Box<SyncSetupArgs>),
    /// Gerencia a chave de criptografia.
    Key {
        #[command(subcommand)]
        command: KeyCommand,
    },
    /// Gerencia as credenciais do backend `s3` (guardadas no chaveiro do sistema).
    Credentials {
        #[command(subcommand)]
        command: CredentialsCommand,
    },
    /// Mostra (ou instala) o agendamento que roda `chamados sync` de tempos em tempos.
    Automation(AutomationArgs),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Platform {
    /// Timer do systemd do usuário (Linux).
    Systemd,
    /// Linha para o `crontab`.
    Cron,
    /// Tarefa agendada do Windows (`schtasks`).
    Windows,
}

#[derive(Debug, Args)]
struct AutomationArgs {
    /// Agendador; por padrão, `windows` no Windows e `systemd` nos demais sistemas.
    #[arg(long, value_enum)]
    platform: Option<Platform>,
    /// Intervalo em minutos entre as sincronizações (1 a 59).
    #[arg(long, default_value_t = 5)]
    interval: u32,
    /// Grava os arquivos do timer do systemd em ~/.config/systemd/user (não os ativa).
    #[arg(long)]
    install: bool,
}

#[derive(Debug, Subcommand)]
enum CredentialsCommand {
    /// Pede o Access Key ID e o Secret Access Key (sem eco) e os guarda no chaveiro. Com entrada
    /// redirecionada, lê duas linhas: o Access Key ID e o Secret. Nunca passe segredos como argumento.
    Set,
    /// Informa se há credenciais (variáveis de ambiente ou chaveiro), sem mostrá-las.
    Status,
}

#[derive(Debug, Args)]
struct SyncSetupArgs {
    /// Backend de armazenamento: `directory` (uma pasta) ou `s3` (S3-compatível) ou `r2` (Cloudflare R2, o mesmo protocolo do `s3`).
    #[arg(long, default_value = DIRECTORY_BACKEND)]
    backend: String,
    /// Pasta do backend `directory` (ex.: uma pasta sincronizada ou um disco de rede).
    #[arg(long)]
    path: Option<PathBuf>,
    /// Endpoint do backend `s3` (https), ex.: https://<ACCOUNT_ID>.r2.cloudflarestorage.com.
    #[arg(long)]
    endpoint: Option<String>,
    /// Bucket do backend `s3` (privado).
    #[arg(long)]
    bucket: Option<String>,
    /// Região do backend `s3` (padrão: `auto`, do Cloudflare R2).
    #[arg(long)]
    region: Option<String>,
    /// Prefixo das chaves dentro do bucket.
    #[arg(long)]
    prefix: Option<String>,
    /// Se o armazenamento aceita escrita condicional (`If-Match`); `false` usa a leitura de conferência.
    #[arg(long)]
    conditional_writes: Option<bool>,
    /// Onde fica a chave: `keyring` (padrão), `file`, `protected-file` (arquivo com frase-senha) ou `env`.
    #[arg(long)]
    key_source: Option<String>,
    /// Sincroniza logo após cada alteração local (`title`, `profile ...`), além do agendamento.
    #[arg(long)]
    auto: Option<bool>,
    /// Arquivo da chave, para `--key-source file` ou `protected-file` (padrão: ~/.config/suap/sync.key).
    #[arg(long)]
    key_file: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum KeyCommand {
    /// Gera uma chave aleatória e a guarda na fonte configurada.
    Generate {
        /// Substitui a chave existente (o que foi cifrado com ela fica ilegível).
        #[arg(long)]
        force: bool,
    },
    /// Imprime a chave em hexadecimal, para levá-la a outro dispositivo. Guarde-a em segredo.
    Export,
    /// Guarda a chave lida da entrada padrão (hexadecimal), vinda de `key export`.
    Import {
        /// Substitui a chave existente.
        #[arg(long)]
        force: bool,
    },
    /// Informa a fonte da chave e se ela existe.
    Status,
}

#[derive(Debug, Args)]
struct BatchMessageArgs {
    /// Números dos chamados (um ou mais); o mesmo texto é enviado a cada um.
    #[arg(required = true)]
    ids: Vec<u64>,
    /// Texto (pode ter várias linhas). Com `-` ou omitido, é lido da entrada padrão.
    #[arg(long, short = 'm')]
    message: Option<String>,
}

#[derive(Debug, Args)]
struct MessageArgs {
    /// Número do chamado.
    id: u64,
    /// Texto (pode ter várias linhas). Com `-` ou omitido, é lido da entrada padrão.
    #[arg(long, short = 'm')]
    message: Option<String>,
}

/// The readable names of a table of options, for clap to accept and list.
fn names(table: &[(&'static str, &'static str)]) -> Vec<&'static str> {
    table.iter().map(|(name, _)| *name).collect()
}

#[derive(Debug, Args)]
struct ListArgs {
    /// Lista os "Meus chamados" ativos em vez da fila de suporte.
    #[arg(long)]
    meus: bool,
    /// Lista os chamados resolvidos que você pode fechar (só aceita --pagina e --todas-paginas).
    #[arg(long, conflicts_with = "meus")]
    a_fechar: bool,
    /// Só este número de chamado.
    #[arg(long)]
    id: Option<u64>,
    /// Texto procurado nas descrições, comentários e notas internas (fila de suporte).
    #[arg(long)]
    busca: Option<String>,
    /// Situação (repetível; fila de suporte). Sem esta opção o SUAP esconde os chamados já encerrados.
    #[arg(long, value_parser = PossibleValuesParser::new(names(&STATUSES)))]
    status: Vec<String>,
    /// Todas as situações, inclusive resolvidos, fechados e cancelados.
    #[arg(long)]
    todos: bool,
    /// Abertos a partir deste dia (AAAA-MM-DD).
    #[arg(long)]
    desde: Option<String>,
    /// Abertos até este dia (AAAA-MM-DD).
    #[arg(long)]
    ate: Option<String>,
    /// Atribuição (fila de suporte).
    #[arg(long, value_parser = PossibleValuesParser::new(names(&ASSIGNMENTS)))]
    atribuidos: Option<String>,
    /// Ordem (fila de suporte).
    #[arg(long, value_parser = PossibleValuesParser::new(names(&ORDERS)))]
    ordenar: Option<String>,
    /// Do último para o primeiro (precisa de --ordenar).
    #[arg(long)]
    desc: bool,
    /// Só os chamados com SLA estourado (fila de suporte).
    #[arg(long)]
    sla_estourado: bool,
    /// Minha relação com o chamado (com --meus).
    #[arg(long, value_parser = PossibleValuesParser::new(names(&RELATIONS)))]
    relacao: Option<String>,
    /// Página a mostrar (o SUAP mostra 15 chamados por página).
    #[arg(long, conflicts_with = "todas_paginas")]
    pagina: Option<u32>,
    /// Percorre todas as páginas.
    #[arg(long)]
    todas_paginas: bool,
    /// Só os chamados cujo título local contém este texto (sem diferenciar maiúsculas).
    #[arg(long)]
    titulo: Option<String>,
    /// Mostra no máximo este número de chamados.
    #[arg(long)]
    limite: Option<usize>,
    /// Imprime JSON (para scripts) em vez de texto.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct WatchArgs {
    /// Acompanha a fila de suporte em vez dos seus chamados.
    #[arg(long)]
    suporte: bool,
    /// Inclui os chamados já encerrados (sem isto, o que sai da lista aparece como "saiu da lista").
    #[arg(long)]
    todos: bool,
    /// Avisa também de cada mensagem nova na linha do tempo (lê a página de cada chamado a cada rodada).
    #[arg(long)]
    mensagens: bool,
    /// Segundos entre uma consulta e a seguinte.
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..=86_400))]
    intervalo: u64,
    /// Para depois de tantas consultas (a primeira só registra o estado); sem a opção, segue até ser interrompido.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    rodadas: Option<u32>,
    /// Imprime um objeto JSON por linha em vez de texto.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct HandoffArgs {
    #[command(flatten)]
    message: MessageArgs,
    /// Já atribui a este atendente do outro grupo (matrícula, nome ou id); sem a opção, o chamado fica sem atendente.
    #[arg(long)]
    para: Option<String>,
}

#[derive(Debug, Subcommand)]
enum TagCommand {
    /// Adiciona tags ao chamado.
    Add {
        /// Número do chamado.
        id: u64,
        /// Tags: nome (ou parte dele) ou id, como o SUAP as oferece.
        #[arg(required = true)]
        tags: Vec<String>,
    },
    /// Remove tags do chamado.
    Remove {
        /// Número do chamado.
        id: u64,
        /// Tags que o chamado tem: nome (ou parte dele) ou id.
        #[arg(required = true)]
        tags: Vec<String>,
    },
}

#[derive(Debug, Subcommand)]
enum InterestedCommand {
    /// Adiciona outros interessados ao chamado.
    Add {
        /// Número do chamado.
        id: u64,
        /// Pessoas: matrícula ou nome, como a busca do SUAP as encontra.
        #[arg(required = true)]
        people: Vec<String>,
    },
    /// Remove outros interessados do chamado.
    Remove {
        /// Número do chamado.
        id: u64,
        /// Pessoas que já são interessadas: matrícula, nome (ou parte dele) ou id do usuário.
        #[arg(required = true)]
        people: Vec<String>,
    },
}

#[derive(Debug, Args)]
struct ResolveArgs {
    #[command(flatten)]
    message: MessageArgs,
    /// Artigo relacionado da base de conhecimento (id; repetível). Sem esta opção, usa o primeiro que o SUAP oferecer.
    #[arg(long)]
    article: Vec<String>,
    /// Resolve também este outro chamado (repetível).
    #[arg(long)]
    also: Vec<u64>,
    /// Resposta padrão a usar (id).
    #[arg(long)]
    standard_reply: Option<String>,
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
    /// Título local do chamado (uma linha), guardado só nesta máquina; também vale `chamados title`.
    #[arg(long, short = 't')]
    title: Option<String>,
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

const UNSAVED_DEFAULT_NOTE: &str = "(perfil padrão ainda não gravado; valores padrão abaixo)";
const NO_PROFILES_HINT: &str = "Nenhum perfil configurado. Crie um com `chamados profile init`.";
const HELP_HINT: &str = "Use `chamados --help` para consultar os comandos disponíveis.";

/// Asks the user for values interactively; `main` implements it over the terminal.
pub trait Prompt {
    /// Asks for a value that may be shown on screen.
    fn line(&mut self, label: &str) -> std::io::Result<String>;
    /// Asks for a secret, without echoing it.
    fn secret(&mut self, label: &str) -> std::io::Result<String>;
}

/// The prompt used when there is no interactive terminal: every question fails.
pub struct NoPrompt;

impl Prompt for NoPrompt {
    fn line(&mut self, _label: &str) -> std::io::Result<String> {
        Err(std::io::Error::other("no interactive terminal"))
    }

    fn secret(&mut self, _label: &str) -> std::io::Result<String> {
        Err(std::io::Error::other("no interactive terminal"))
    }
}

/// Name of the environment variable that holds the SUAP password.
pub const PASSWORD_ENV: &str = "SUAP_PASSWORD";

/// Runs the CLI with `args`, writing to `out`/`err`, and returns the process exit code.
///
/// `password` is the value of [`PASSWORD_ENV`], `input` the standard input (empty when it is a terminal)
/// and `prompt` how to ask the user questions; all are provided by the caller so they can be injected
/// in tests.
pub fn run<I, T>(
    args: I,
    paths: &AppPaths,
    password: Option<String>,
    input: &mut dyn Read,
    prompt: &mut dyn Prompt,
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

    match execute(cli, &paths, password, input, prompt, out) {
        Ok(()) => 0,
        Err(error) => {
            let _ = writeln!(err, "erro: {error}");
            1
        }
    }
}

/// Whether the command changes data that is synchronized.
fn changes_synced_data(command: &Option<Command>) -> bool {
    match command {
        Some(Command::Title { text, remove, .. }) => text.is_some() || *remove,
        Some(Command::Profile { command }) => matches!(
            command,
            ProfileCommand::Init(_) | ProfileCommand::Update(_) | ProfileCommand::Remove { .. }
        ),
        _ => false,
    }
}

fn execute(
    cli: Cli,
    paths: &AppPaths,
    password: Option<String>,
    input: &mut dyn Read,
    prompt: &mut dyn Prompt,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let changes = changes_synced_data(&cli.command);
    dispatch(cli, paths, password, input, prompt, out)?;
    if changes {
        auto_sync(paths, prompt, out);
    }
    Ok(())
}

/// With `sync.auto` on, synchronizes right after a local change. It never fails the command that
/// made the change: a problem is only reported.
fn auto_sync(paths: &AppPaths, prompt: &mut dyn Prompt, out: &mut dyn Write) {
    let quiet = SyncArgs {
        action: None,
        only: None,
        quiet: true,
        dry_run: false,
        check: false,
        json: false,
    };
    let outcome = auto_sync_enabled(paths).and_then(|enabled| {
        if enabled {
            sync_run(paths, &quiet, prompt, out)
        } else {
            Ok(())
        }
    });
    if let Err(error) = outcome {
        let _ = writeln!(out, "aviso: a sincronização automática falhou: {error}");
    }
}

/// `sync.auto` is on, a backend is configured and there is something to synchronize.
fn auto_sync_enabled(paths: &AppPaths) -> Result<bool, Box<dyn Error>> {
    let settings = load_sync_settings(paths)?;
    if settings.auto != Some(true) || settings.backend.is_none() {
        return Ok(false);
    }
    let any_profile = list_profiles(paths)?.iter().any(|(_, config)| config.sync);
    Ok(any_profile || paths.removed_profiles_file().exists())
}

fn dispatch(
    cli: Cli,
    paths: &AppPaths,
    password: Option<String>,
    input: &mut dyn Read,
    prompt: &mut dyn Prompt,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    match cli.command {
        Some(Command::Paths) => show_paths(paths, out),
        Some(Command::Profile { command }) => match command {
            ProfileCommand::Init(args) => profile_init(paths, args, out),
            ProfileCommand::Update(args) => profile_update(paths, args, out),
            ProfileCommand::Remove { name, yes } => profile_remove(paths, &name, yes, out),
            ProfileCommand::Show { name, json } => profile_show(paths, name.as_deref(), json, out),
            ProfileCommand::List { json } => profile_list(paths, json, out),
        },
        Some(Command::SessionStatus) => session_status(paths, out),
        Some(Command::Login { username }) => login(paths, username, password, out),
        Some(Command::List(args)) => list(paths, *args, out),
        Some(Command::Show { id, json }) => show(paths, id, json, out),
        Some(Command::Watch(args)) => watch(paths, &args, out),
        Some(Command::Open(args)) => open(paths, args, input, out),
        Some(Command::Comment(args)) => send_message(paths, args, Message::Comment, input, out),
        Some(Command::Note(args)) => send_message(paths, args, Message::InternalNote, input, out),
        Some(Command::Sync(args)) => sync(paths, args, input, prompt, out),
        Some(Command::Suspend(args)) => suspend(paths, args, input, out),
        Some(Command::Resolve(args)) => resolve(paths, args, input, out),
        Some(Command::Download {
            id,
            anexo,
            saida,
            force,
        }) => download(paths, id, &anexo, saida, force, out),
        Some(Command::Attach {
            id,
            files,
            descricao,
        }) => attach(paths, id, &files, descricao.as_deref(), out),
        Some(Command::Assign { id, para }) => assign(paths, id, &para, out),
        Some(Command::Escalate(args)) => hand_off(paths, args, Direction::Escalate, input, out),
        Some(Command::Return(args)) => hand_off(paths, args, Direction::Return, input, out),
        Some(Command::Reclassify {
            message,
            servico,
            campus,
            centro,
        }) => {
            let change = Reclassification {
                service: servico,
                campus,
                center: centro,
            };
            reclassify(paths, message, &change, input, out)
        }
        Some(Command::Tag { command }) => tag(paths, command, out),
        Some(Command::Interested { command }) => interested(paths, command, out),
        Some(Command::Reopen(args)) => reopen(paths, args, input, out),
        Some(Command::Close { id, nota, message }) => close(paths, id, nota, message, input, out),
        Some(Command::Assume { id }) => assume(paths, id, out),
        Some(Command::Start { id, assume }) => start(paths, id, assume, out),
        Some(Command::Cancel { message, yes }) => cancel(paths, message, yes, input, out),
        Some(Command::Title { id, text, remove }) => title(paths, id, text, remove, out),
        Some(Command::Status) => {
            writeln!(
                out,
                "chamados-cli: fundação inicial instalada; integração ainda não implementada."
            )?;
            Ok(())
        }
        None => {
            writeln!(out, "{HELP_HINT}")?;
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
            "perfil {0:?} não configurado: execute `chamados profile init {0}`",
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

/// Paths for the profile named in a `profile` subcommand, or the one selected by `--profile`.
fn paths_for(paths: &AppPaths, name: Option<&str>) -> Result<AppPaths, Box<dyn Error>> {
    match name {
        Some(name) => Ok(paths.clone().with_profile(name)?),
        None => Ok(paths.clone()),
    }
}

fn print_profile(
    paths: &AppPaths,
    config: &SuapConfig,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    writeln!(out, "profile: {}", paths.profile())?;
    writeln!(out, "base_url: {}", config.base_url)?;
    let username = config.username.as_deref().unwrap_or("<não configurado>");
    writeln!(out, "username: {username}")?;
    let open = &config.open;
    let defaults = [
        ("service", open.service.map(|service| service.to_string())),
        ("interested", open.interested.clone()),
        ("campus", open.campus.clone()),
        ("center", open.center.clone()),
    ];
    for (name, value) in defaults
        .iter()
        .filter_map(|(name, value)| Some((name, value.as_ref()?)))
    {
        writeln!(out, "open.{name}: {value}")?;
    }
    let sync = if config.sync { "ligado" } else { "desligado" };
    writeln!(out, "sync: {sync}")?;
    let session = if paths.session_file().exists() {
        "salva"
    } else {
        "ausente"
    };
    writeln!(out, "session: {session}")?;
    writeln!(out, "file: {}", paths.config_file().display())?;
    Ok(())
}

fn profile_show(
    paths: &AppPaths,
    name: Option<&str>,
    json: bool,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let paths = paths_for(paths, name)?;
    let present = |paths: &AppPaths, config: &SuapConfig, out: &mut dyn Write| {
        if json {
            let profile = render::profile_json(paths, config);
            writeln!(out, "{profile}")?;
            return Ok(());
        }
        print_profile(paths, config, out)
    };
    match load_config(&paths)? {
        Some(config) => present(&paths, &config, out),
        None if paths.profile() == DEFAULT_PROFILE => {
            if !json {
                writeln!(out, "{UNSAVED_DEFAULT_NOTE}")?;
            }
            present(&paths, &SuapConfig::default(), out)
        }
        None => Err(missing_profile(paths.profile())),
    }
}

fn missing_profile(name: &str) -> Box<dyn Error> {
    format!("o perfil {name:?} não existe: crie-o com `chamados profile init {name}`").into()
}

fn profile_list(paths: &AppPaths, json: bool, out: &mut dyn Write) -> Result<(), Box<dyn Error>> {
    let profiles = list_profiles(paths)?;
    if json {
        let profiles = render::profiles_json(&profiles, DEFAULT_PROFILE);
        writeln!(out, "{profiles}")?;
        return Ok(());
    }
    if profiles.is_empty() {
        writeln!(out, "{NO_PROFILES_HINT}")?;
    }
    for (name, config) in profiles {
        let marker = if name == DEFAULT_PROFILE {
            "\t(padrão)"
        } else {
            ""
        };
        let username = config.username.as_deref().unwrap_or("-");
        writeln!(out, "{name}\t{}\t{username}{marker}", config.base_url)?;
    }
    Ok(())
}

fn apply_profile_args(config: &mut SuapConfig, args: ProfileArgs) -> Result<(), Box<dyn Error>> {
    if let Some(base_url) = args.base_url {
        config.base_url = base_url.parse()?;
    }
    if args.username.is_some() {
        config.username = args.username;
    }
    if let Some(sync) = args.sync {
        config.sync = sync;
    }
    config.updated_at = unix_now();
    let open = &mut config.open;
    open.service = args.service.or(open.service);
    open.interested = args.interested.or(open.interested.take());
    open.campus = args.campus.or(open.campus.take());
    open.center = args.center.or(open.center.take());
    Ok(())
}

fn profile_init(
    paths: &AppPaths,
    args: ProfileArgs,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let paths = paths_for(paths, args.name.as_deref())?;
    let name = paths.profile();
    if load_config(&paths)?.is_some() {
        return Err(format!(
            "o perfil {name:?} já existe: altere-o com `chamados profile update {name}`"
        )
        .into());
    }
    let mut config = SuapConfig::default();
    apply_profile_args(&mut config, args)?;
    save_config(&paths, &config)?;
    let file = paths.config_file();
    writeln!(out, "Perfil {name} criado em {}", file.display())?;
    Ok(())
}

fn profile_update(
    paths: &AppPaths,
    args: ProfileArgs,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let paths = paths_for(paths, args.name.as_deref())?;
    let name = paths.profile();
    let Some(mut config) = load_config(&paths)? else {
        return Err(missing_profile(name));
    };
    let unchanged = [
        &args.base_url,
        &args.username,
        &args.interested,
        &args.campus,
        &args.center,
    ]
    .iter()
    .all(|option| option.is_none())
        && args.service.is_none()
        && args.sync.is_none();
    if unchanged {
        return Err("nada a alterar: informe ao menos uma opção (ex.: --base-url)".into());
    }
    apply_profile_args(&mut config, args)?;
    save_config(&paths, &config)?;
    let file = paths.config_file();
    writeln!(out, "Perfil {name} atualizado em {}", file.display())?;
    Ok(())
}

fn profile_remove(
    paths: &AppPaths,
    name: &str,
    yes: bool,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let paths = paths_for(paths, Some(name))?;
    let Some(config) = load_config(&paths)? else {
        return Err(missing_profile(name));
    };
    if !yes {
        return Err(format!(
            "a remoção apaga a configuração e a sessão do perfil {name:?}: confirme com --yes"
        )
        .into());
    }
    if config.sync {
        // Tell the other devices on the next sync, or the profile would come back from the cloud.
        record_removal(&paths, name, unix_now())?;
    }
    remove_config(&paths)?;
    let session = paths.session_file();
    if session.exists() {
        fs::remove_file(&session)?;
    }
    writeln!(out, "Perfil {name} removido (configuração e sessão).")?;
    Ok(())
}

fn session_status(paths: &AppPaths, out: &mut dyn Write) -> Result<(), Box<dyn Error>> {
    let config = profile_config(paths)?;
    let profile = paths.profile();
    if !paths.session_file().exists() {
        return Err(format!(
            "nenhuma sessão salva para o perfil {profile:?}: execute `chamados login`"
        )
        .into());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let client = SuapClient::open(paths, &config)?;
    if !runtime.block_on(client.is_authenticated())? {
        return Err(format!(
            "a sessão do perfil {profile:?} expirou: execute `chamados login` (a sessão salva não foi apagada)"
        )
        .into());
    }
    let url = &config.base_url;
    writeln!(out, "Sessão válida (perfil {profile}, {url}).")?;
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
        .ok_or("usuário não informado: use --username ou `profile update --username`")?;
    let password = password.ok_or_else(|| {
        format!("senha não informada: defina a variável de ambiente {PASSWORD_ENV}")
    })?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let client = SuapClient::open(paths, &config)?;
    runtime.block_on(client.login(&username, &password))?;
    let (profile, session) = (paths.profile(), paths.session_file());
    let message = format!(
        "Login realizado como {username} (perfil {profile}). Sessão salva em {}",
        session.display()
    );
    writeln!(out, "{message}")?;
    Ok(())
}

fn list(paths: &AppPaths, args: ListArgs, out: &mut dyn Write) -> Result<(), Box<dyn Error>> {
    let config = profile_config(paths)?;
    let queue = match (args.meus, args.a_fechar) {
        (true, _) => TicketQueue::Mine,
        (false, true) => TicketQueue::ToClose,
        (false, false) => TicketQueue::Support,
    };
    let filter = TicketFilter {
        id: args.id,
        text: args.busca,
        statuses: args.status,
        all_statuses: args.todos,
        since: args.desde,
        until: args.ate,
        assignment: args.atribuidos,
        order_by: args.ordenar,
        descending: args.desc,
        sla_exceeded: args.sla_estourado,
        relation: args.relacao,
        page: args.pagina,
        all_pages: args.todas_paginas,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let client = SuapClient::open(paths, &config)?;
    let source = SuapTicketSource::new(&client, queue);

    let tickets = runtime
        .block_on(source.list_filtered(&filter))
        .map_err(explain)?;
    let titles = TitleStore::open(paths.titles_file())?;
    // The local titles never leave this machine, so this filter is applied here.
    let needle = args.titulo.map(|text| text.to_lowercase());
    let matches_title = |id: &str| {
        needle.as_ref().is_none_or(|needle| {
            let title = titles.get(id).unwrap_or_default();
            title.to_lowercase().contains(needle.as_str())
        })
    };
    let tickets: Vec<_> = tickets
        .into_iter()
        .filter(|ticket| matches_title(&ticket.id))
        .take(args.limite.unwrap_or(usize::MAX))
        .collect();
    if args.json {
        let local_title = |id: &str| titles.get(id).map(str::to_owned);
        let json = render::tickets_json(&tickets, &local_title);
        writeln!(out, "{json}")?;
        return Ok(());
    }

    if tickets.is_empty() {
        writeln!(out, "Nenhum chamado encontrado.")?;
    }
    for ticket in tickets {
        let status = ticket.status.as_deref().unwrap_or("-");
        let subject = ticket.subject.as_deref().unwrap_or("-");
        let title = titles.get(&ticket.id).unwrap_or("-");
        writeln!(out, "#{}\t{status}\t{title}\t{subject}", ticket.id)?;
    }
    Ok(())
}

fn show(paths: &AppPaths, id: u64, json: bool, out: &mut dyn Write) -> Result<(), Box<dyn Error>> {
    let config = profile_config(paths)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let client = SuapClient::open(paths, &config)?;
    let source = SuapTicketSource::new(&client, TicketQueue::Support);
    let details = runtime
        .block_on(source.get_ticket(&id.to_string()))
        .map_err(explain)?;
    let titles = TitleStore::open(paths.titles_file())?;
    let local_title = titles.get(&id.to_string());
    if json {
        let details = render::details_json(&details, local_title);
        writeln!(out, "{details}")?;
        return Ok(());
    }
    print_details(&details, local_title, out)?;
    Ok(())
}

/// Sends the same comment or internal note to each ticket, one after the other. A ticket that
/// refuses it does not stop the others; the failures are reported together at the end (and make
/// the command fail). Without a session nothing can work, so that stops at the first ticket.
fn send_message(
    paths: &AppPaths,
    args: BatchMessageArgs,
    kind: Message,
    input: &mut dyn Read,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let text = read_text(args.message, input)?;
    let (runtime, client) = connect(paths)?;
    let source = SuapTicketSource::new(&client, TicketQueue::Support);
    let mut ids = Vec::new();
    for id in args.ids {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    let mut failures = Vec::new();
    for id in &ids {
        let id = id.to_string();
        match runtime.block_on(source.add_message(&id, kind, &text)) {
            Ok(()) => {
                let message = match kind {
                    Message::Comment => format!("Comentário adicionado ao chamado #{id}."),
                    Message::InternalNote => format!("Nota interna adicionada ao chamado #{id}."),
                };
                writeln!(out, "{message}")?;
            }
            Err(error @ TicketError::Suap(SuapError::NotAuthenticated)) => {
                return Err(explain(error));
            }
            Err(error) => failures.push((id, explain(error).to_string())),
        }
    }
    if ids.len() == 1 && !failures.is_empty() {
        return Err(failures.remove(0).1.into());
    }
    if ids.len() > 1 {
        let sent = ids.len() - failures.len();
        let summary = format!("Resumo: {sent} de {} enviados.", ids.len());
        writeln!(out, "{summary}")?;
    }
    if failures.is_empty() {
        return Ok(());
    }
    let details: Vec<String> = failures
        .iter()
        .map(|(id, error)| format!("  #{id}: {error}"))
        .collect();
    Err(format!(
        "falhou em {} de {} chamados:\n{}",
        failures.len(),
        ids.len(),
        details.join("\n")
    )
    .into())
}

/// Runtime and client for the selected profile.
fn connect(paths: &AppPaths) -> Result<(tokio::runtime::Runtime, SuapClient), Box<dyn Error>> {
    let config = profile_config(paths)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    Ok((runtime, SuapClient::open(paths, &config)?))
}

fn suspend(
    paths: &AppPaths,
    args: MessageArgs,
    input: &mut dyn Read,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let text = read_text(args.message, input)?;
    let (runtime, client) = connect(paths)?;
    let source = SuapTicketSource::new(&client, TicketQueue::Support);
    let id = args.id.to_string();
    runtime
        .block_on(source.suspend_ticket(&id, &text))
        .map_err(explain)?;
    writeln!(out, "Chamado #{id} suspenso.")?;
    Ok(())
}

/// Names Windows reserves for devices, whatever the extension.
const RESERVED_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];
const MAX_FILE_NAME_CHARS: usize = 150;

/// A file name that is safe to create in a chosen folder, from a name SUAP gave: no folders, no
/// characters the file systems refuse, no reserved or empty names. `fallback` is used when nothing
/// usable is left.
fn safe_file_name(name: &str, fallback: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = last
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .take(MAX_FILE_NAME_CHARS)
        .collect();
    let cleaned = cleaned.trim().trim_end_matches('.').trim().to_owned();
    if cleaned.is_empty() {
        return fallback.to_owned();
    }
    let stem = cleaned.split('.').next().unwrap_or_default().to_uppercase();
    if RESERVED_NAMES.contains(&stem.as_str()) {
        return format!("_{cleaned}");
    }
    cleaned
}

/// `name` with the extension of the stored file added when it does not already end with it. The
/// name SUAP shows for an attachment is the description given to it, which may have no extension.
fn with_stored_extension(name: &str, stored_name: &str) -> String {
    let extension = match stored_name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() && !extension.is_empty() => extension,
        _ => return name.to_owned(),
    };
    let suffix = format!(".{}", extension.to_lowercase());
    if name.to_lowercase().ends_with(&suffix) {
        return name.to_owned();
    }
    format!("{name}.{extension}")
}

/// `name` made different from every name in `taken` by adding " (2)", " (3)"... before the extension.
fn unique_name(name: &str, taken: &[String]) -> String {
    let (stem, extension) = match name.rfind('.') {
        Some(dot) if dot > 0 => name.split_at(dot),
        _ => (name, ""),
    };
    let mut candidate = name.to_owned();
    let mut number = 1;
    while taken.contains(&candidate) {
        number += 1;
        candidate = format!("{stem} ({number}){extension}");
    }
    candidate
}

fn download(
    paths: &AppPaths,
    id: u64,
    wanted: &[usize],
    folder: Option<PathBuf>,
    force: bool,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let (runtime, client) = connect(paths)?;
    let source = SuapTicketSource::new(&client, TicketQueue::Support);
    let details = runtime
        .block_on(source.get_ticket(&id.to_string()))
        .map_err(explain)?;
    let total = details.attachments.len();
    if total == 0 {
        return Err(format!("o chamado #{id} não tem anexos").into());
    }
    let mut numbers: Vec<usize> = if wanted.is_empty() {
        (1..=total).collect()
    } else {
        wanted.to_vec()
    };
    let mut seen = Vec::new();
    numbers.retain(|number| {
        let first = !seen.contains(number);
        seen.push(*number);
        first
    });
    if let Some(bad) = numbers.iter().find(|number| !(1..=total).contains(number)) {
        return Err(format!("o anexo {bad} não existe: o chamado #{id} tem {total}").into());
    }

    // Download everything first: the name SUAP shows may only be a description, and the kind of
    // file is known from the name it is stored under. Nothing is written to disk before every
    // destination is checked.
    let folder = folder.unwrap_or(PathBuf::from("."));
    let mut files = Vec::new();
    for number in &numbers {
        let link = &details.attachments[number - 1];
        let file = runtime
            .block_on(source.download_attachment(link))
            .map_err(explain)?;
        files.push(file);
    }
    let mut names: Vec<String> = Vec::new();
    for (number, file) in numbers.iter().zip(&files) {
        let link = &details.attachments[number - 1];
        let safe = safe_file_name(&link.name, &format!("anexo-{number}"));
        let name = unique_name(&with_stored_extension(&safe, &file.stored_name), &names);
        names.push(name);
    }
    let existing: Vec<String> = names
        .iter()
        .filter(|name| folder.join(name).exists())
        .cloned()
        .collect();
    if !force && !existing.is_empty() {
        return Err(format!(
            "já existe em {}: {} (use --force para sobrescrever)",
            folder.display(),
            existing.join(", ")
        )
        .into());
    }
    fs::create_dir_all(&folder)?;
    for ((number, name), file) in numbers.iter().zip(&names).zip(&files) {
        let target = folder.join(name);
        fs::write(&target, &file.bytes)?;
        let size = file.bytes.len();
        writeln!(out, "Anexo {number}: {} ({size} bytes)", target.display())?;
    }
    Ok(())
}

fn attach(
    paths: &AppPaths,
    id: u64,
    files: &[PathBuf],
    description: Option<&str>,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    // Read every file before sending any, so a typo does not leave the ticket half attached.
    let attachments = read_attachments(files)?;
    let (runtime, client) = connect(paths)?;
    let source = SuapTicketSource::new(&client, TicketQueue::Support);
    let id = id.to_string();
    for attachment in &attachments {
        runtime
            .block_on(source.attach_file(&id, attachment, description))
            .map_err(explain)?;
        let message = format!("Chamado #{id}: anexo {} enviado.", attachment.file_name);
        writeln!(out, "{message}")?;
    }
    Ok(())
}

fn assign(paths: &AppPaths, id: u64, to: &str, out: &mut dyn Write) -> Result<(), Box<dyn Error>> {
    let (runtime, client) = connect(paths)?;
    let source = SuapTicketSource::new(&client, TicketQueue::Support);
    let id = id.to_string();
    let who = runtime
        .block_on(source.assign_ticket(&id, to))
        .map_err(explain)?;
    writeln!(out, "Chamado #{id} atribuído a {who}.")?;
    Ok(())
}

fn hand_off(
    paths: &AppPaths,
    args: HandoffArgs,
    direction: Direction,
    input: &mut dyn Read,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let text = read_text(args.message.message, input)?;
    let (runtime, client) = connect(paths)?;
    let source = SuapTicketSource::new(&client, TicketQueue::Support);
    let id = args.message.id.to_string();
    runtime
        .block_on(source.move_ticket(&id, direction, &text, args.para.as_deref()))
        .map_err(explain)?;
    let done = match direction {
        Direction::Escalate => "escalado para o grupo acima",
        Direction::Return => "devolvido ao grupo abaixo",
    };
    writeln!(out, "Chamado #{id} {done}.")?;
    Ok(())
}

fn reclassify(
    paths: &AppPaths,
    args: MessageArgs,
    change: &Reclassification,
    input: &mut dyn Read,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let text = read_text(args.message, input)?;
    let (runtime, client) = connect(paths)?;
    let source = SuapTicketSource::new(&client, TicketQueue::Support);
    let id = args.id.to_string();
    runtime
        .block_on(source.reclassify_ticket(&id, change, &text))
        .map_err(explain)?;
    writeln!(out, "Chamado #{id} reclassificado.")?;
    Ok(())
}

fn tag(paths: &AppPaths, command: TagCommand, out: &mut dyn Write) -> Result<(), Box<dyn Error>> {
    let (runtime, client) = connect(paths)?;
    let source = SuapTicketSource::new(&client, TicketQueue::Support);
    let (id, names, done) = match command {
        TagCommand::Add { id, tags } => {
            let id = id.to_string();
            let names = runtime.block_on(source.add_tags(&id, &tags));
            (id, names.map_err(explain)?, "adicionadas")
        }
        TagCommand::Remove { id, tags } => {
            let id = id.to_string();
            let names = runtime.block_on(source.remove_tags(&id, &tags));
            (id, names.map_err(explain)?, "removidas")
        }
    };
    writeln!(out, "Chamado #{id}: tags {done}: {}.", names.join(", "))?;
    Ok(())
}

fn interested(
    paths: &AppPaths,
    command: InterestedCommand,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let (runtime, client) = connect(paths)?;
    let source = SuapTicketSource::new(&client, TicketQueue::Support);
    let (id, names, done) = match command {
        InterestedCommand::Add { id, people } => {
            let id = id.to_string();
            let names = runtime.block_on(source.add_interested(&id, &people));
            (id, names.map_err(explain)?, "adicionados")
        }
        InterestedCommand::Remove { id, people } => {
            let id = id.to_string();
            let names = runtime.block_on(source.remove_interested(&id, &people));
            (id, names.map_err(explain)?, "removidos")
        }
    };
    let message = format!(
        "Chamado #{id}: outros interessados {done}: {}.",
        names.join(", ")
    );
    writeln!(out, "{message}")?;
    Ok(())
}

fn reopen(
    paths: &AppPaths,
    args: MessageArgs,
    input: &mut dyn Read,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let text = read_text(args.message, input)?;
    let (runtime, client) = connect(paths)?;
    let source = SuapTicketSource::new(&client, TicketQueue::Support);
    let id = args.id.to_string();
    runtime
        .block_on(source.reopen_ticket(&id, &text))
        .map_err(explain)?;
    writeln!(out, "Chamado #{id} reaberto.")?;
    Ok(())
}

fn close(
    paths: &AppPaths,
    id: u64,
    rating: Option<u8>,
    message: Option<String>,
    input: &mut dyn Read,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let comment = read_optional_text(message, input)?;
    let (runtime, client) = connect(paths)?;
    let source = SuapTicketSource::new(&client, TicketQueue::Support);
    let id = id.to_string();
    runtime
        .block_on(source.close_ticket(&id, rating, comment.as_deref()))
        .map_err(explain)?;
    writeln!(out, "Chamado #{id} fechado.")?;
    Ok(())
}

fn assume(paths: &AppPaths, id: u64, out: &mut dyn Write) -> Result<(), Box<dyn Error>> {
    let (runtime, client) = connect(paths)?;
    let source = SuapTicketSource::new(&client, TicketQueue::Support);
    let id = id.to_string();
    runtime
        .block_on(source.assume_ticket(&id))
        .map_err(explain)?;
    writeln!(out, "Chamado #{id} assumido.")?;
    Ok(())
}

fn start(
    paths: &AppPaths,
    id: u64,
    assume: bool,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let (runtime, client) = connect(paths)?;
    let source = SuapTicketSource::new(&client, TicketQueue::Support);
    let id = id.to_string();
    if assume {
        runtime
            .block_on(source.assume_ticket(&id))
            .map_err(explain)?;
        writeln!(out, "Chamado #{id} assumido.")?;
    }
    runtime
        .block_on(source.start_service(&id))
        .map_err(explain)?;
    writeln!(out, "Chamado #{id} em atendimento.")?;
    Ok(())
}

fn cancel(
    paths: &AppPaths,
    args: MessageArgs,
    yes: bool,
    input: &mut dyn Read,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    if !yes {
        return Err(format!(
            "o cancelamento do chamado #{} não pode ser desfeito: confirme com --yes",
            args.id
        )
        .into());
    }
    let text = read_text(args.message, input)?;
    let (runtime, client) = connect(paths)?;
    let source = SuapTicketSource::new(&client, TicketQueue::Support);
    let id = args.id.to_string();
    runtime
        .block_on(source.cancel_ticket(&id, &text))
        .map_err(explain)?;
    writeln!(out, "Chamado #{id} cancelado.")?;
    Ok(())
}

fn resolve(
    paths: &AppPaths,
    args: ResolveArgs,
    input: &mut dyn Read,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let resolution = Resolution {
        text: read_text(args.message.message, input)?,
        articles: args.article,
        also: args.also.iter().map(u64::to_string).collect(),
        standard_reply: args.standard_reply,
    };
    let (runtime, client) = connect(paths)?;
    let source = SuapTicketSource::new(&client, TicketQueue::Support);
    let id = args.message.id.to_string();
    runtime
        .block_on(source.resolve_ticket(&id, &resolution))
        .map_err(explain)?;
    let others = resolution.also.iter().map(|other| format!("#{other}"));
    let together = others.collect::<Vec<_>>().join(", ");
    let message = if together.is_empty() {
        format!("Chamado #{id} resolvido.")
    } else {
        format!("Chamado #{id} resolvido, junto com {together}.")
    };
    writeln!(out, "{message}")?;
    Ok(())
}

fn key_source(paths: &AppPaths) -> Result<(SyncSettings, KeySource), Box<dyn Error>> {
    let settings = load_sync_settings(paths)?;
    let source = key_source_from(&settings, paths)?;
    Ok((settings, source))
}

fn sync(
    paths: &AppPaths,
    args: SyncArgs,
    input: &mut dyn Read,
    prompt: &mut dyn Prompt,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    match args.action {
        Some(SyncAction::Setup(setup)) => sync_setup(paths, *setup, out),
        Some(SyncAction::Key { command }) => sync_key(paths, command, input, prompt, out),
        Some(SyncAction::Automation(automation)) => sync_automation(paths, &automation, out),
        Some(SyncAction::Credentials { command }) => sync_credentials(command, input, prompt, out),
        None => sync_run(paths, &args, prompt, out),
    }
}

fn sync_setup(
    paths: &AppPaths,
    args: SyncSetupArgs,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let mut settings = load_sync_settings(paths)?;
    settings.backend = Some(args.backend);
    settings.path = args
        .path
        .or_else(|| settings.path.take().map(PathBuf::from))
        .map(|path| path.display().to_string());
    settings.key_source = args.key_source.or(settings.key_source.take());
    settings.auto = args.auto.or(settings.auto);
    settings.key_file = args
        .key_file
        .map(|file| file.display().to_string())
        .or(settings.key_file.take());
    settings.endpoint = args.endpoint.or(settings.endpoint.take());
    settings.bucket = args.bucket.or(settings.bucket.take());
    settings.region = args.region.or(settings.region.take());
    settings.prefix = args.prefix.or(settings.prefix.take());
    settings.conditional_writes = args.conditional_writes.or(settings.conditional_writes);
    // Validate before saving, so a bad setup never replaces a working one.
    validate_settings(&settings)?;
    key_source_from(&settings, paths)?;
    save_sync_settings(paths, &settings)?;
    let file = paths.config_file();
    writeln!(out, "Sincronização configurada em {}.", file.display())?;
    Ok(())
}

fn no_terminal(_: std::io::Error) -> Box<dyn Error> {
    "sem terminal interativo: envie as duas linhas (Access Key ID e Secret) pela entrada padrão"
        .into()
}

fn sync_credentials(
    command: CredentialsCommand,
    input: &mut dyn Read,
    prompt: &mut dyn Prompt,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    match command {
        CredentialsCommand::Set => {
            let mut text = String::new();
            input.read_to_string(&mut text)?;
            if text.trim().is_empty() {
                // Nothing piped in: ask on the terminal, keeping the secret off the screen.
                let id = prompt.line("Access Key ID: ").map_err(no_terminal)?;
                let secret = prompt
                    .secret("Secret Access Key (não aparece na tela): ")
                    .map_err(no_terminal)?;
                text = format!("{id}\n{secret}");
            }
            S3Credentials::parse(&text)?.store()?;
            writeln!(out, "Credenciais guardadas no chaveiro.")?;
        }
        CredentialsCommand::Status => {
            let present = match S3Credentials::load()? {
                Some(found) => format!("sim ({})", found.describe()),
                None => "não".to_owned(),
            };
            writeln!(out, "credenciais presentes: {present}")?;
        }
    }
    Ok(())
}

fn no_passphrase(_: std::io::Error) -> Box<dyn Error> {
    format!("sem terminal interativo: informe a frase-senha na variável {PASSPHRASE_ENV}").into()
}

/// Gets the passphrase of a protected key file from `env` or by asking (twice, when `confirm` is
/// set, as for a new passphrase). Other key sources need none, and neither does a key file that
/// does not exist yet when it is only going to be read.
fn unlock_with(
    source: KeySource,
    prompt: &mut dyn Prompt,
    confirm: bool,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<KeySource, Box<dyn Error>> {
    if !source.needs_passphrase() || (!confirm && !key_exists(&source)?) {
        return Ok(source);
    }
    if let Some(text) = env(PASSPHRASE_ENV).filter(|text| !text.is_empty()) {
        return Ok(source.with_passphrase(Passphrase::new(text)));
    }
    let first = prompt
        .secret("Frase-senha da chave: ")
        .map_err(no_passphrase)?;
    if confirm {
        let again = prompt
            .secret("Repita a frase-senha: ")
            .map_err(no_passphrase)?;
        if first != again {
            return Err("as frases-senha não conferem".into());
        }
    }
    Ok(source.with_passphrase(Passphrase::new(first)))
}

fn unlock(
    source: KeySource,
    prompt: &mut dyn Prompt,
    confirm: bool,
) -> Result<KeySource, Box<dyn Error>> {
    unlock_with(source, prompt, confirm, &|name| std::env::var(name).ok())
}

fn sync_key(
    paths: &AppPaths,
    command: KeyCommand,
    input: &mut dyn Read,
    prompt: &mut dyn Prompt,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let (_, source) = key_source(paths)?;
    match command {
        KeyCommand::Generate { force } => {
            let source = unlock(source, prompt, true)?;
            store_key(&source, &Key::generate(), force)?;
            writeln!(out, "Chave criada ({}).", source.name())?;
        }
        KeyCommand::Export => {
            let source = unlock(source, prompt, false)?;
            let key = load_key(&source)?.ok_or_else(|| no_key(&source))?;
            writeln!(out, "{}", key.to_hex())?;
        }
        KeyCommand::Import { force } => {
            let key = Key::from_hex(&read_text(None, input)?)?;
            let source = unlock(source, prompt, true)?;
            store_key(&source, &key, force)?;
            writeln!(out, "Chave importada ({}).", source.name())?;
        }
        KeyCommand::Status => {
            let present = if key_exists(&source)? { "sim" } else { "não" };
            writeln!(out, "fonte: {}", source.name())?;
            writeln!(out, "chave presente: {present}")?;
        }
    }
    Ok(())
}

const AUTOMATION_NOTE: &str = "Dica: o chaveiro do sistema só abre com a sessão do usuário; para rodar sozinho, guarde a chave em arquivo (`chamados sync setup --key-source file`) ou na variável CHAMADOS_SYNC_KEY. Uma chave com frase-senha não serve para isso.";
const SERVICE_UNIT: &str = "chamados-sync.service";
const TIMER_UNIT: &str = "chamados-sync.timer";

fn systemd_service(exe: &str) -> String {
    format!("[Unit]\nDescription=Sincroniza os dados do chamados\n\n[Service]\nType=oneshot\nExecStart=\"{exe}\" sync --quiet\n")
}

fn systemd_timer(interval: u32) -> String {
    format!("[Unit]\nDescription=Sincroniza o chamados a cada {interval} minuto(s)\n\n[Timer]\nOnBootSec=2min\nOnUnitActiveSec={interval}min\n\n[Install]\nWantedBy=timers.target\n")
}

fn sync_automation(
    paths: &AppPaths,
    args: &AutomationArgs,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    if !(1..=59).contains(&args.interval) {
        return Err("o intervalo deve ficar entre 1 e 59 minutos".into());
    }
    let default = [Platform::Systemd, Platform::Windows][usize::from(cfg!(windows))];
    let platform = args.platform.unwrap_or(default);
    if args.install && platform != Platform::Systemd {
        return Err(
            "--install só existe para o systemd; nos demais, use o comando mostrado".into(),
        );
    }
    let exe = std::env::current_exe()?.display().to_string();
    let interval = args.interval;
    let text = match platform {
        Platform::Cron => format!("# crontab -e\n*/{interval} * * * * \"{exe}\" sync --quiet"),
        Platform::Windows => format!(
            "schtasks /Create /SC MINUTE /MO {interval} /TN chamados-sync /TR \"\\\"{exe}\\\" sync --quiet\" /F"
        ),
        Platform::Systemd => {
            let enable = format!(
                "systemctl --user daemon-reload && systemctl --user enable --now {TIMER_UNIT}"
            );
            if args.install {
                let base = paths.config_dir().parent().unwrap_or(paths.config_dir());
                let units = base.join("systemd").join("user");
                fs::create_dir_all(&units)?;
                fs::write(units.join(SERVICE_UNIT), systemd_service(&exe))?;
                fs::write(units.join(TIMER_UNIT), systemd_timer(interval))?;
                format!(
                    "Arquivos gravados em {}.\nPara ativar:\n{enable}",
                    units.display()
                )
            } else {
                format!(
                    "# ~/.config/systemd/user/{SERVICE_UNIT}\n{}\n# ~/.config/systemd/user/{TIMER_UNIT}\n{}\n# depois:\n{enable}",
                    systemd_service(&exe),
                    systemd_timer(interval)
                )
            }
        }
    };
    writeln!(out, "{text}\n\n{AUTOMATION_NOTE}")?;
    Ok(())
}

const CONDITIONAL_MISMATCH_NOTE: &str = "aviso: o resultado difere da configuração; ajuste com `chamados sync setup --conditional-writes true|false`";
const LOCK_HELD_NOTE: &str = "Outra sincronização já está em andamento; nada a fazer.";

fn support_text(supported: bool) -> &'static str {
    if supported {
        "suportada"
    } else {
        "não suportada"
    }
}

fn no_key(source: &KeySource) -> Box<dyn Error> {
    format!(
        "nenhuma chave de criptografia ({}): execute `chamados sync key generate` (ou `key import`)",
        source.name()
    )
    .into()
}

fn sync_run(
    paths: &AppPaths,
    args: &SyncArgs,
    prompt: &mut dyn Prompt,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let (settings, source) = key_source(paths)?;
    if settings.backend.is_none() {
        return Err("sincronização não configurada: execute `chamados sync setup`".into());
    }
    let credentials = if matches!(settings.backend.as_deref(), Some(S3_BACKEND | R2_BACKEND)) {
        S3Credentials::load()?
    } else {
        None
    };
    let backend = backend_from(&settings, credentials)?;
    let source = unlock(source, prompt, false)?;
    let key = load_key(&source)?.ok_or_else(|| no_key(&source))?;
    let Some(_lock) = SyncLock::acquire(&paths.sync_lock_file())? else {
        if !args.quiet {
            writeln!(out, "{LOCK_HELD_NOTE}")?;
        }
        return Ok(());
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    if args.check {
        let probed = runtime.block_on(backend.probe_conditional_writes())?;
        let name = settings.backend.as_deref().unwrap_or_default();
        if args.json {
            let configured = backend.supports_conditional_writes();
            let check = render::sync_check_json(name, probed, configured, source.name());
            writeln!(out, "{check}")?;
            return Ok(());
        }
        writeln!(out, "backend: {name}")?;
        writeln!(out, "escrita condicional: {}", support_text(probed))?;
        if probed != backend.supports_conditional_writes() {
            writeln!(out, "{CONDITIONAL_MISMATCH_NOTE}")?;
        }
        writeln!(out, "chave: {} (presente)", source.name())?;
        return Ok(());
    }
    let options = SyncOptions {
        only: args.only.as_deref(),
        dry_run: args.dry_run,
    };
    let report = runtime.block_on(sync_once(paths, &backend, &key, &options))?;
    if args.quiet {
        return Ok(());
    }
    if args.json {
        let done = render::sync_report_json(
            report.profiles,
            report.local_changes,
            report.uploaded,
            args.dry_run,
        );
        writeln!(out, "{done}")?;
        return Ok(());
    }
    let cloud = match (report.uploaded, args.dry_run) {
        (true, true) => "seria atualizada",
        (true, false) => "atualizada",
        (false, _) => "já estava em dia",
    };
    let prefix = if args.dry_run {
        "Simulação"
    } else {
        "Sincronização concluída"
    };
    let message = format!(
        "{prefix}: {} perfil(is), {} alteração(ões) local(is), nuvem {cloud}.",
        report.profiles, report.local_changes
    );
    writeln!(out, "{message}")?;
    Ok(())
}

/// Looks at a list again and again, saying what changed since the previous look.
fn watch(paths: &AppPaths, args: &WatchArgs, out: &mut dyn Write) -> Result<(), Box<dyn Error>> {
    let (queue, kind) = match args.suporte {
        true => (TicketQueue::Support, "support"),
        false => (TicketQueue::Mine, "mine"),
    };
    let scope = format!("{kind}:{}", if args.todos { "all" } else { "active" });
    let filter = TicketFilter {
        all_statuses: args.todos,
        all_pages: true,
        ..TicketFilter::default()
    };
    let (runtime, client) = connect(paths)?;
    let source = SuapTicketSource::new(&client, queue);
    let titles = TitleStore::open(paths.titles_file())?;
    let state_file = paths.watch_file();
    let mut previous = Snapshot::load(&state_file).map_err(explain)?;
    if previous.as_ref().is_none_or(|known| known.scope != scope) && !args.json {
        let note =
            format!("Acompanhando ({kind}); a primeira consulta só registra o estado atual.");
        writeln!(out, "{note}")?;
    }
    let mut round = 0;
    loop {
        round += 1;
        let polled = runtime.block_on(poll(
            &source,
            &filter,
            &scope,
            args.mensagens,
            previous.as_ref(),
        ));
        match polled {
            Ok((snapshot, events)) => {
                let now = unix_now() / 1_000;
                for event in &events {
                    let title = titles.get(event.id());
                    let line = match args.json {
                        true => {
                            let mut object = event.to_json(now);
                            object["title"] = title.into();
                            object.to_string()
                        }
                        false => render::watch_line(event, now, title),
                    };
                    writeln!(out, "{line}")?;
                }
                snapshot.save(&state_file).map_err(explain)?;
                previous = Some(snapshot);
            }
            // Without a session there is nothing to retry: the user has to log in again.
            Err(TicketError::Suap(SuapError::NotAuthenticated)) => {
                return Err(explain(TicketError::Suap(SuapError::NotAuthenticated)));
            }
            // A failed look (network, SUAP busy) is reported and tried again at the next round.
            Err(error) => {
                let message = error.to_string();
                let line = match args.json {
                    true => serde_json::json!({"time": unix_now() / 1_000, "event": "error", "message": message}).to_string(),
                    false => format!("aviso: consulta falhou ({message}); tento de novo na próxima rodada"),
                };
                writeln!(out, "{line}")?;
            }
        }
        out.flush()?;
        if args.rodadas.is_some_and(|limit| round >= limit) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(args.intervalo));
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

fn title(
    paths: &AppPaths,
    id: u64,
    text: Option<String>,
    remove: bool,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let mut titles = TitleStore::open(paths.titles_file())?;
    let id = id.to_string();
    if remove {
        let had_title = titles.remove(&id, unix_now())?;
        titles.save()?;
        let message = if had_title {
            format!("Título local do chamado #{id} removido.")
        } else {
            format!("O chamado #{id} não tinha título local.")
        };
        writeln!(out, "{message}")?;
    } else if let Some(text) = text {
        titles.set(&id, &text, unix_now())?;
        titles.save()?;
        writeln!(out, "Título local do chamado #{id} salvo.")?;
    } else {
        let message = match titles.get(&id) {
            Some(title) => title.to_owned(),
            None => format!("O chamado #{id} não tem título local."),
        };
        writeln!(out, "{message}")?;
    }
    Ok(())
}

/// Text given as an option value, or read from `input` when the value is `-` or missing, without
/// the trailing line break (newlines inside the text are kept: multi-line).
fn raw_text(value: Option<String>, input: &mut dyn Read) -> Result<String, Box<dyn Error>> {
    let text = match value {
        Some(text) if text != "-" => text,
        _ => {
            let mut piped = String::new();
            input.read_to_string(&mut piped)?;
            piped
        }
    };
    Ok(text.trim_end_matches(['\r', '\n']).to_owned())
}

/// Like [`read_text`], but the text is optional: when omitted there is none (standard input is not
/// read, so a script that leaves it open never hangs); `-` reads it, and blank text means "none".
fn read_optional_text(
    value: Option<String>,
    input: &mut dyn Read,
) -> Result<Option<String>, Box<dyn Error>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let text = raw_text(Some(value), input)?;
    Ok((!text.trim().is_empty()).then_some(text))
}

/// Text given as an option value, or read from `input` when the value is `-` or missing.
///
/// Newlines inside the text are kept (multi-line); only the trailing line break is removed.
fn read_text(value: Option<String>, input: &mut dyn Read) -> Result<String, Box<dyn Error>> {
    let text = raw_text(value, input)?;
    if text.trim().is_empty() {
        return Err(
            "texto não informado: passe o valor na opção, use `-` ou envie pela entrada padrão"
                .into(),
        );
    }
    Ok(text)
}

fn read_attachments(files: &[PathBuf]) -> Result<Vec<Attachment>, Box<dyn Error>> {
    let mut attachments = Vec::new();
    for path in files {
        let bytes = fs::read(path)
            .map_err(|error| format!("não foi possível ler o anexo {}: {error}", path.display()))?;
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        attachments.push(Attachment { file_name, bytes });
    }
    Ok(attachments)
}

/// Error for a step done after the ticket was already opened, so the new ticket id is not lost.
fn after_open(id: &str, action: &str, error: TicketError) -> Box<dyn Error> {
    format!(
        "o chamado #{id} foi aberto, mas não foi possível {action}: {}",
        explain(error)
    )
    .into()
}

fn open(
    paths: &AppPaths,
    args: OpenArgs,
    input: &mut dyn Read,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let config = profile_config(paths)?;
    let defaults = &config.open;
    let service_id = args.service.or(defaults.service).ok_or("serviço não informado: passe o número ou defina o padrão do perfil (profile update --service)")?;
    let interested = args.interested.or_else(|| defaults.interested.clone());
    if interested.is_none() {
        return Err("interessado não informado: use --interested ou defina o padrão do perfil (profile update --interested)".into());
    }
    let local_title = args.title.as_deref().map(validate_title).transpose()?;
    let mut titles = TitleStore::open(paths.titles_file())?;
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

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let client = SuapClient::open(paths, &config)?;
    let source = SuapTicketSource::new(&client, TicketQueue::Support);
    let id = runtime
        .block_on(source.open_ticket(&ticket))
        .map_err(explain)?;
    let url = client
        .base_url()
        .join(&format!("centralservicos/chamado/{id}/"))?;
    writeln!(out, "Chamado #{id} aberto: {url}")?;

    if let Some(local_title) = local_title {
        let saved = titles
            .set(&id, &local_title, unix_now())
            .and_then(|()| titles.save());
        saved.map_err(|error| after_open(&id, "salvar o título local", error))?;
        writeln!(out, "Título local salvo: {local_title}")?;
    }

    if args.assume || args.start {
        runtime
            .block_on(source.assume_ticket(&id))
            .map_err(|error| after_open(&id, "assumi-lo", error))?;
        writeln!(out, "Chamado #{id} assumido.")?;
    }
    if args.start {
        runtime
            .block_on(source.start_service(&id))
            .map_err(|error| after_open(&id, "colocá-lo em atendimento", error))?;
        writeln!(out, "Chamado #{id} em atendimento.")?;
    }
    Ok(())
}

fn print_details(
    details: &TicketDetails,
    local_title: Option<&str>,
    out: &mut dyn Write,
) -> std::io::Result<()> {
    writeln!(out, "{}", details.title)?;
    if let Some(local_title) = local_title {
        writeln!(out, "Título: {local_title}")?;
    }
    writeln!(out, "Situação: {}", details.statuses.join("; "))?;
    let heading = details.heading.as_deref().unwrap_or("-");
    writeln!(out, "Serviço: {heading}")?;
    writeln!(out, "URL: {}", details.details_url)?;
    for (label, value) in &details.fields {
        writeln!(out, "{label}: {}", indent_continuation(value, "    "))?;
    }
    if !details.attachments.is_empty() {
        let heading = format!("\nAnexos (baixe com `chamados download {}`):", details.id);
        writeln!(out, "{heading}")?;
        for (index, attachment) in details.attachments.iter().enumerate() {
            writeln!(out, "  {}. {}", index + 1, attachment.name)?;
        }
    }
    writeln!(out, "\nLinha do tempo:")?;
    for entry in &details.timeline {
        let text = indent_continuation(&entry.text, "      ");
        writeln!(out, "  {}  {text}", entry.date)?;
    }
    Ok(())
}

/// Indents every line but the first, so multi-line values stay readable under their label.
fn indent_continuation(text: &str, prefix: &str) -> String {
    let mut lines = text.split('\n');
    let first = lines.next().unwrap_or_default();
    lines.fold(first.to_owned(), |mut indented, line| {
        indented.push('\n');
        if !line.is_empty() {
            indented.push_str(prefix);
            indented.push_str(line);
        }
        indented
    })
}

/// Turns ticket errors into user-facing messages.
fn explain(error: TicketError) -> Box<dyn Error> {
    match error {
        TicketError::Suap(SuapError::NotAuthenticated) => {
            "sessão ausente ou expirada: execute `chamados login`".into()
        }
        other => other.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use tempfile::{tempdir, TempDir};
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    fn paths() -> (TempDir, AppPaths) {
        let directory = tempdir().unwrap();
        let paths = AppPaths::from_dirs(
            directory.path().join("config"),
            directory.path().join("data"),
        );
        (directory, paths)
    }

    fn run_args(args: &[&str], paths: &AppPaths) -> (i32, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(
            std::iter::once("chamados").chain(args.iter().copied()),
            paths,
            None,
            &mut std::io::empty(),
            &mut NoPrompt,
            &mut out,
            &mut err,
        );
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
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
        assert!(out.contains("profile") && err.is_empty());
    }

    #[test]
    fn invalid_argument_goes_to_stderr() {
        let (_dir, paths) = paths();
        let (code, out, err) = run_args(&["--nope"], &paths);
        assert_eq!(code, 2);
        assert!(out.is_empty() && !err.is_empty());
    }

    #[test]
    fn profile_show_without_file_describes_the_default_profile() {
        let (_dir, paths) = paths();
        let (code, out, _) = run_args(&["profile", "show"], &paths);
        assert_eq!(code, 0);
        assert!(out.contains("perfil padrão ainda não gravado"));
        assert!(
            out.contains("base_url: https://suap.ifrn.edu.br/") && out.contains("session: ausente")
        );
    }

    #[test]
    fn profile_init_update_and_show() {
        let (_dir, paths) = paths();
        let (code, out, _) = run_args(
            &["profile", "init", "--base-url", "https://example.org/"],
            &paths,
        );
        assert_eq!(code, 0);
        assert!(out.contains("Perfil default criado em"));

        let (_, out, _) = run_args(&["profile", "show"], &paths);
        assert!(out.contains("https://example.org/") && out.contains("<não configurado>"));

        let (code, out, _) = run_args(&["profile", "update", "--username", "kelson"], &paths);
        assert_eq!(code, 0);
        assert!(out.contains("Perfil default atualizado em"));
        let (_, out, _) = run_args(&["profile", "show", "default"], &paths);
        assert!(out.contains("https://example.org/") && out.contains("username: kelson"));
    }

    #[test]
    fn profile_init_refuses_existing_and_update_refuses_missing_or_empty() {
        let (_dir, paths) = paths();
        run_args(&["profile", "init", "local"], &paths);
        let (code, _, err) = run_args(&["profile", "init", "local"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("já existe") && err.contains("profile update local"));

        let (code, _, err) = run_args(&["profile", "update", "outro", "--username", "x"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("não existe") && err.contains("profile init outro"));

        let (code, _, err) = run_args(&["profile", "update", "local"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("nada a alterar"));

        let (code, _, err) = run_args(&["profile", "show", "outro"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("não existe"));
    }

    #[test]
    fn profile_update_changes_each_option_and_keeps_the_others() {
        let (_dir, paths) = paths();
        run_args(
            &["profile", "init", "--service", "1", "--interested", "2"],
            &paths,
        );
        for args in [
            ["--base-url", "http://h/"],
            ["--username", "u"],
            ["--service", "9"],
            ["--interested", "8"],
            ["--campus", "7"],
            ["--center", "6"],
        ] {
            let mut full = vec!["profile", "update"];
            full.extend(args);
            assert_eq!(run_args(&full, &paths).0, 0, "{args:?}");
        }
        let (_, out, _) = run_args(&["profile", "show"], &paths);
        for expected in [
            "base_url: http://h/",
            "username: u",
            "open.service: 9",
            "open.interested: 8",
            "open.campus: 7",
            "open.center: 6",
        ] {
            assert!(out.contains(expected), "{expected}");
        }
    }

    #[test]
    fn profile_list_marks_the_default_profile() {
        let (_dir, paths) = paths();
        let (_, out, _) = run_args(&["profile", "list"], &paths);
        assert!(out.contains("Nenhum perfil configurado"));

        run_args(
            &[
                "profile",
                "init",
                "local",
                "--base-url",
                "http://localhost:8000",
            ],
            &paths,
        );
        run_args(&["profile", "init", "--username", "kelson"], &paths);
        let (code, out, _) = run_args(&["profile", "list"], &paths);
        assert_eq!(code, 0);
        let lines: Vec<_> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            "default\thttps://suap.ifrn.edu.br/\tkelson\t(padrão)"
        );
        assert_eq!(lines[1], "local\thttp://localhost:8000/\t-");
    }

    #[test]
    fn profile_remove_deletes_configuration_and_session() {
        let (_dir, paths) = paths();
        run_args(&["profile", "init", "local"], &paths);
        run_args(&["profile", "init"], &paths);
        let local = paths.clone().with_profile("local").unwrap();
        local.ensure_dirs().unwrap();
        std::fs::write(local.session_file(), "x").unwrap();

        let (_, out, _) = run_args(&["profile", "show", "local"], &paths);
        assert!(out.contains("session: salva"));

        let (code, _, err) = run_args(&["profile", "remove", "local"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("--yes") && local.session_file().exists());

        let (code, out, _) = run_args(&["profile", "remove", "local", "--yes"], &paths);
        assert_eq!(code, 0);
        assert!(out.contains("Perfil local removido"));
        assert!(!local.session_file().exists());
        let (_, out, _) = run_args(&["profile", "list"], &paths);
        assert!(out.starts_with("default"));

        let (code, _, err) = run_args(&["profile", "remove", "local", "--yes"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("não existe"));
        // A profile without a saved session is removed too.
        assert_eq!(
            run_args(&["profile", "remove", "default", "--yes"], &paths).0,
            0
        );
    }

    #[test]
    fn session_status_reports_missing_valid_and_expired_sessions() {
        let (_dir, paths) = paths();
        let (runtime, server) = bare_server();
        run_args(&["profile", "init", "--base-url", &server.uri()], &paths);
        let (code, _, err) = run_args(&["session-status"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("nenhuma sessão salva") && err.contains("chamados login"));

        paths.ensure_dirs().unwrap();
        std::fs::write(paths.session_file(), "").unwrap();
        mount_text(
            &runtime,
            &server,
            "GET",
            "/",
            ResponseTemplate::new(302).insert_header("location", "/accounts/login/"),
        );
        mount_text(
            &runtime,
            &server,
            "GET",
            "/accounts/login/",
            ResponseTemplate::new(200),
        );
        let (code, _, err) = run_args(&["session-status"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("expirou") && paths.session_file().exists());
    }

    #[test]
    fn session_status_accepts_a_valid_session() {
        let (_dir, paths) = paths();
        let (_runtime, server) = mock_server(true);
        run_args(&["profile", "init", "--base-url", &server.uri()], &paths);
        let (code, _, _) = run_login(&["login", "--username", "u"], &paths, Some("p"));
        assert_eq!(code, 0);
        let (code, out, err) = run_args(&["session-status"], &paths);
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(out.starts_with("Sessão válida (perfil default, "));
    }

    #[test]
    fn profiles_isolate_configuration_and_session() {
        let (_dir, paths) = paths();
        let (_runtime, server) = mock_server(true);
        let (code, out, _) = run_args(
            &[
                "profile",
                "init",
                "--profile",
                "local",
                "--base-url",
                &server.uri(),
            ],
            &paths,
        );
        assert!(code == 0 && out.contains("Perfil local criado"));

        let (_, out, _) = run_args(&["profile", "show"], &paths);
        assert!(out.contains("perfil padrão ainda não gravado"));
        let (_, out, _) = run_args(&["--profile", "local", "profile", "show"], &paths);
        assert!(out.contains("profile: local") && out.contains(&server.uri()));
        let (_, out, _) = run_args(&["paths", "--profile", "local"], &paths);
        assert!(out.contains("profile: local") && out.contains("session-local.cookies"));

        let (code, out, err) = run_login(
            &["login", "--profile", "local", "--username", "dev"],
            &paths,
            Some("segredo"),
        );
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(out.contains("(perfil local)"));
        assert!(paths
            .clone()
            .with_profile("local")
            .unwrap()
            .session_file()
            .exists());
        assert!(!paths.session_file().exists());
    }

    #[test]
    fn unconfigured_profile_is_an_error_but_default_has_defaults() {
        let (_dir, paths) = paths();
        let (code, _, err) = run_args(&["list", "--profile", "local"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("profile init local"));
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
        let (code, _, err) = run_args(&["profile", "init", "--base-url", "não é url"], &paths);
        assert_eq!(code, 1);
        assert!(err.starts_with("erro:"));
    }

    #[test]
    fn config_init_rejects_non_http_scheme() {
        let (_dir, paths) = paths();
        let (code, _, err) = run_args(
            &["profile", "init", "--base-url", "ftp://example.org/"],
            &paths,
        );
        assert_eq!(code, 1);
        assert!(err.contains("HTTP or HTTPS"));
    }

    #[test]
    fn corrupt_config_is_reported() {
        let (_dir, paths) = paths();
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.config_file(), "base_url = [").unwrap();
        let (code, _, err) = run_args(&["profile", "show"], &paths);
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
            &mut NoPrompt,
            &mut out,
            &mut err,
        );
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    fn mock_server(login_succeeds: bool) -> (tokio::runtime::Runtime, MockServer) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
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
            Mock::given(method("POST"))
                .respond_with(post)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/"))
                .respond_with(ResponseTemplate::new(200))
                .mount(&server)
                .await;
            server
        });
        (runtime, server)
    }

    #[test]
    fn login_uses_flag_username_and_saves_session() {
        let (_dir, paths) = paths();
        let (_runtime, server) = mock_server(true);
        run_args(&["profile", "init", "--base-url", &server.uri()], &paths);
        let (code, out, err) =
            run_login(&["login", "--username", "kelson"], &paths, Some("segredo"));
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(out.contains("Login realizado como kelson") && !out.contains("segredo"));
        assert!(paths.session_file().exists());
    }

    #[test]
    fn login_falls_back_to_configured_username() {
        let (_dir, paths) = paths();
        let (_runtime, server) = mock_server(true);
        run_args(
            &[
                "profile",
                "init",
                "--base-url",
                &server.uri(),
                "--username",
                "cfg",
            ],
            &paths,
        );
        let (code, out, _) = run_login(&["login"], &paths, Some("segredo"));
        assert_eq!(code, 0);
        assert!(out.contains("como cfg"));
    }

    #[test]
    fn login_reports_rejected_credentials() {
        let (_dir, paths) = paths();
        let (_runtime, server) = mock_server(false);
        run_args(&["profile", "init", "--base-url", &server.uri()], &paths);
        let (code, _, err) = run_login(&["login", "--username", "kelson"], &paths, Some("errada"));
        assert_eq!(code, 1);
        assert!(err.contains("authentication failed"));
        assert!(!paths.session_file().exists());
    }

    fn mount_listing(
        runtime: &tokio::runtime::Runtime,
        server: &MockServer,
        request_path: &str,
        body: ResponseTemplate,
    ) {
        runtime.block_on(
            Mock::given(method("GET"))
                .and(path(request_path.to_owned()))
                .respond_with(body)
                .mount(server),
        );
    }

    #[test]
    fn list_prints_support_and_own_tickets() {
        let (_dir, paths) = paths();
        let (runtime, server) = mock_server(true);
        run_args(&["profile", "init", "--base-url", &server.uri()], &paths);
        let html = r#"<div class="general-box"><span class="status">Em atendimento</span>
            <h4><a href="/centralservicos/chamado/7/">REQ #7 <strong>Assunto</strong></a></h4></div>
            <div class="general-box"><h4><a href="/centralservicos/chamado/8/">REQ #8</a></h4></div>"#;
        mount_listing(
            &runtime,
            &server,
            "/centralservicos/listar_chamados_suporte/",
            ResponseTemplate::new(200).set_body_string(html),
        );
        mount_listing(
            &runtime,
            &server,
            "/centralservicos/meus_chamados/",
            ResponseTemplate::new(200).set_body_string(html),
        );

        for args in [&["list"][..], &["list", "--meus"][..]] {
            let (code, out, err) = run_args(args, &paths);
            assert_eq!((code, err.as_str()), (0, ""));
            assert_eq!(out, "#7\tEm atendimento\t-\tAssunto\n#8\t-\t-\t-\n");
        }
    }

    #[test]
    fn list_filters_searches_and_pages() {
        let (_dir, paths) = paths();
        let (runtime, server) = mock_server(true);
        run_args(&["profile", "init", "--base-url", &server.uri()], &paths);
        let html = r#"<div class="general-box"><span class="status">Em atendimento</span>
            <h4><a href="/centralservicos/chamado/7/">REQ #7 <strong>Assunto</strong></a></h4></div>
            <div class="general-box"><h4><a href="/centralservicos/chamado/8/">REQ #8</a></h4></div>"#;
        for queue in ["listar_chamados_suporte", "meus_chamados"] {
            mount_listing(
                &runtime,
                &server,
                &format!("/centralservicos/{queue}/"),
                ResponseTemplate::new(200).set_body_string(html),
            );
        }
        run_args(&["title", "7", "Atualizar o Moodle"], &paths);
        let queries = || -> Vec<String> {
            let requests = runtime.block_on(server.received_requests()).unwrap();
            requests
                .iter()
                .map(|request| request.url.query().unwrap_or_default().to_owned())
                .collect()
        };

        // The options go to SUAP, which does the filtering.
        let (code, out, err) = run_args(
            &[
                "list",
                "--busca",
                "moodle",
                "--status",
                "aberto",
                "--status",
                "suspenso",
                "--desde",
                "2026-01-02",
                "--ate",
                "2026-10-08",
                "--atribuidos",
                "mim",
                "--ordenar",
                "abertura",
                "--desc",
                "--sla-estourado",
                "--id",
                "7",
                "--pagina",
                "2",
            ],
            &paths,
        );
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(out.starts_with("#7\t"), "{out}");
        assert_eq!(
            queries()[0],
            "texto=moodle&status=1&status=6&atribuicoes=1&ordenar_por=aberto_em&tipo_ordenacao=-\
             &sla_estourado=on&chamado_id=7&data_inicial=02%2F01%2F2026&data_final=08%2F10%2F2026&page=2"
        );
        let (code, _, _) = run_args(&["list", "--todos"], &paths);
        assert_eq!(code, 0);
        assert_eq!(queries()[1], "todos_status=on");
        let (code, _, _) = run_args(
            &[
                "list",
                "--meus",
                "--todos",
                "--relacao",
                "algum",
                "--id",
                "7",
            ],
            &paths,
        );
        assert_eq!(code, 0);
        assert_eq!(queries()[2], "tab=todos&tipo_usuario=RIO&chamado_id=7");

        // The local title and the limit are applied here.
        let (_, out, _) = run_args(&["list", "--titulo", "MOODLE"], &paths);
        assert_eq!(out, "#7\tEm atendimento\tAtualizar o Moodle\tAssunto\n");
        let (_, out, _) = run_args(&["list", "--titulo", "nada"], &paths);
        assert!(out.contains("Nenhum chamado"), "{out}");
        let (_, out, _) = run_args(&["list", "--limite", "1"], &paths);
        assert_eq!(out.lines().count(), 1);

        // Every page: it stops at the first page with nothing new (the mock repeats itself).
        let before = queries().len();
        let (code, out, _) = run_args(&["list", "--todas-paginas"], &paths);
        assert_eq!((code, out.lines().count()), (0, 2));
        assert_eq!(&queries()[before..], ["", "page=2"]);

        // Mistakes are reported before anything is sent.
        let before = queries().len();
        for (args, expected) in [
            (&["list", "--desc"][..], "needs a sort key"),
            (&["list", "--relacao", "algum"], "--meus"),
            (&["list", "--meus", "--busca", "x"], "support queue"),
            (&["list", "--desde", "ontem"], "invalid date"),
            (&["list", "--status", "pronto"], "pronto"),
            (
                &["list", "--pagina", "2", "--todas-paginas"],
                "cannot be used",
            ),
        ] {
            let (code, _, err) = run_args(args, &paths);
            // clap itself refuses some of them (a usage error, code 2)
            let expected_code =
                if err.starts_with("error: invalid") || err.contains("cannot be used") {
                    2
                } else {
                    1
                };
            assert_eq!(code, expected_code, "{args:?}: {err}");
            assert!(err.contains(expected), "{args:?}: {err}");
        }
        assert_eq!(queries().len(), before);
    }

    #[test]
    fn titles_are_set_shown_removed_and_listed() {
        let (_dir, paths) = paths();
        let (runtime, server) = mock_server(true);
        run_args(&["profile", "init", "--base-url", &server.uri()], &paths);
        let (_, out, _) = run_args(&["title", "7"], &paths);
        assert!(out.contains("não tem título local"));

        let (code, out, _) = run_args(&["title", "7", "  Atualizar o Moodle  "], &paths);
        assert!(code == 0 && out.contains("Título local do chamado #7 salvo"));
        let (_, out, _) = run_args(&["title", "7"], &paths);
        assert_eq!(out, "Atualizar o Moodle\n");

        let listing = r#"<div class="general-box"><span class="status">Em atendimento</span>
            <h4><a href="/centralservicos/chamado/7/">REQ #7 <strong>Assunto</strong></a></h4></div>
            <div class="general-box"><h4><a href="/centralservicos/chamado/8/">REQ #8</a></h4></div>"#;
        mount_listing(
            &runtime,
            &server,
            "/centralservicos/listar_chamados_suporte/",
            ResponseTemplate::new(200).set_body_string(listing),
        );
        let (_, out, _) = run_args(&["list"], &paths);
        assert_eq!(
            out,
            "#7\tEm atendimento\tAtualizar o Moodle\tAssunto\n#8\t-\t-\t-\n"
        );

        let page = r#"<main id="content"><div class="title-container"><h2>Chamado Interno 7</h2></div></main>"#;
        mount_listing(
            &runtime,
            &server,
            "/centralservicos/chamado/7/",
            ResponseTemplate::new(200).set_body_string(page),
        );
        let (_, out, _) = run_args(&["show", "7"], &paths);
        assert!(out.starts_with("Chamado Interno 7\nTítulo: Atualizar o Moodle\n"));

        let (_, out, _) = run_args(&["title", "7", "--remove"], &paths);
        assert!(out.contains("removido"));
        let (_, out, _) = run_args(&["title", "7", "--remove"], &paths);
        assert!(out.contains("não tinha título local"));
        let (_, out, _) = run_args(&["show", "7"], &paths);
        assert!(!out.contains("Título:"));
        assert!(paths.titles_file().exists());
    }

    #[test]
    fn titles_are_kept_per_profile() {
        let (_dir, paths) = paths();
        run_args(&["title", "7", "No default"], &paths);
        run_args(&["title", "7", "No local", "--profile", "local"], &paths);
        let (_, out, _) = run_args(&["title", "7"], &paths);
        assert_eq!(out, "No default\n");
        let (_, out, _) = run_args(&["title", "7", "--profile", "local"], &paths);
        assert_eq!(out, "No local\n");
    }

    #[test]
    fn title_rejects_invalid_input() {
        let (_dir, paths) = paths();
        let (code, _, err) = run_args(&["title", "7", "duas\nlinhas"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("single line"));
        let (code, _, err) = run_args(&["title", "7", "x", "--remove"], &paths);
        assert_eq!(code, 2);
        assert!(!err.is_empty());

        paths.ensure_dirs().unwrap();
        std::fs::write(paths.titles_file(), "quebrado").unwrap();
        let (code, _, err) = run_args(&["title", "7"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("titles file"));
    }

    const THREAD_PAGE: &str = r#"
        <form method="post" action="/centralservicos/adicionar_comentario/5/">
          <input type="hidden" name="csrfmiddlewaretoken" value="tok-comentario">
          <textarea name="texto"></textarea></form>
        <form method="post" action="/centralservicos/adicionar_nota_interna/5/">
          <input type="hidden" name="csrfmiddlewaretoken" value="tok-nota">
          <textarea name="texto"></textarea></form>"#;

    fn thread_setup() -> (TempDir, AppPaths, tokio::runtime::Runtime, MockServer) {
        let (dir, paths) = paths();
        let (runtime, server) = bare_server();
        run_args(&["profile", "init", "--base-url", &server.uri()], &paths);
        let page = ResponseTemplate::new(200).set_body_string(THREAD_PAGE);
        mount_text(
            &runtime,
            &server,
            "GET",
            "/centralservicos/chamado/5/",
            page,
        );
        let ok = || ResponseTemplate::new(200).set_body_string("ok");
        mount_text(
            &runtime,
            &server,
            "POST",
            "/centralservicos/adicionar_comentario/5/",
            ok(),
        );
        mount_text(
            &runtime,
            &server,
            "POST",
            "/centralservicos/adicionar_nota_interna/5/",
            ok(),
        );
        (dir, paths, runtime, server)
    }

    #[test]
    fn comment_and_note_accept_multiline_text_and_standard_input() {
        let (_dir, paths, runtime, server) = thread_setup();
        let (code, out, err) = run_args(&["comment", "5", "-m", "linha 1\nlinha 2\n"], &paths);
        assert_eq!((code, err.as_str()), (0, ""));
        assert_eq!(out, "Comentário adicionado ao chamado #5.\n");
        let (code, out, _) =
            run_with_input(&["note", "5", "-m", "-"], &paths, "vindo\ndo stdin\r\n");
        assert_eq!(code, 0);
        assert_eq!(out, "Nota interna adicionada ao chamado #5.\n");
        let (code, _, _) = run_with_input(&["note", "5"], &paths, "sem opção\n");
        assert_eq!(code, 0);

        let requests = runtime.block_on(server.received_requests()).unwrap();
        let bodies: Vec<String> = requests
            .iter()
            .filter(|request| request.method == wiremock::http::Method::POST)
            .map(|request| String::from_utf8_lossy(&request.body).into_owned())
            .collect();
        assert!(
            bodies[0].contains("csrfmiddlewaretoken=tok-comentario")
                && bodies[0].contains("texto=linha+1%0Alinha+2")
        );
        assert!(
            bodies[1].contains("csrfmiddlewaretoken=tok-nota")
                && bodies[1].contains("texto=vindo%0Ado+stdin")
        );
        assert!(bodies[2].contains("texto=sem+op%C3%A7%C3%A3o"));
    }

    #[test]
    fn comment_reports_empty_text_and_missing_forms() {
        let (_dir, paths, _runtime, _server) = thread_setup();
        let (code, _, err) = run_with_input(&["comment", "5"], &paths, "  \n");
        assert_eq!(code, 1);
        assert!(err.contains("texto não informado"));
        let (code, _, err) = run_args(&["note", "9", "-m", "x"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("unexpected status 404"));
    }

    #[test]
    fn suspend_sends_a_multiline_message_from_the_option_or_standard_input() {
        let (_dir, paths) = paths();
        let (runtime, server) = bare_server();
        run_args(&["profile", "init", "--base-url", &server.uri()], &paths);
        let form = r#"<form action="" method="POST"><input type="hidden" name="csrfmiddlewaretoken" value="tok">
            <textarea name="observacao"></textarea></form>"#;
        mount_text(
            &runtime,
            &server,
            "GET",
            "/centralservicos/suspender_chamado/5/",
            ResponseTemplate::new(200).set_body_string(form),
        );
        mount_text(
            &runtime,
            &server,
            "POST",
            "/centralservicos/suspender_chamado/5/",
            ResponseTemplate::new(200),
        );

        let (code, out, err) = run_args(&["suspend", "5", "-m", "aguardando\nretorno"], &paths);
        assert_eq!(
            (code, err.as_str(), out.as_str()),
            (0, "", "Chamado #5 suspenso.\n")
        );
        let (code, _, _) = run_with_input(&["suspend", "5"], &paths, "pelo stdin\n");
        assert_eq!(code, 0);
        let requests = runtime.block_on(server.received_requests()).unwrap();
        let bodies: Vec<String> = requests
            .iter()
            .filter(|request| request.method == wiremock::http::Method::POST)
            .map(|request| String::from_utf8_lossy(&request.body).into_owned())
            .collect();
        assert!(bodies[0].contains("observacao=aguardando%0Aretorno"));
        assert!(bodies[1].contains("observacao=pelo+stdin"));

        let (code, _, err) = run_with_input(&["suspend", "5"], &paths, "\n");
        assert_eq!(code, 1);
        assert!(err.contains("texto não informado"));
        let (code, _, err) = run_args(&["suspend", "9", "-m", "x"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("ticket 9 cannot be suspended"));
    }

    #[test]
    fn file_names_from_suap_are_made_safe_and_unique() {
        for (given, expected) in [
            ("dados.csv", "dados.csv"),
            ("relatório final.pdf", "relatório final.pdf"),
            ("../../etc/passwd", "passwd"),
            ("C:\\Users\\x\\segredo.docx", "segredo.docx"),
            ("a<b>c:d\"e|f?g*h.txt", "a_b_c_d_e_f_g_h.txt"),
            ("  espaço.png  ", "espaço.png"),
            ("termina.com.ponto...", "termina.com.ponto"),
            ("con.txt", "_con.txt"),
            ("NUL", "_NUL"),
            ("Lpt1.pdf", "_Lpt1.pdf"),
            ("com10.txt", "com10.txt"),
            ("", "anexo-3"),
            ("...", "anexo-3"),
            ("..", "anexo-3"),
            ("pasta/", "anexo-3"),
            ("tab\there.txt", "tab_here.txt"),
        ] {
            assert_eq!(safe_file_name(given, "anexo-3"), expected, "{given:?}");
        }
        let long = format!("{}.pdf", "x".repeat(400));
        assert_eq!(
            safe_file_name(&long, "f").chars().count(),
            MAX_FILE_NAME_CHARS
        );

        for (name, stored, expected) in [
            ("Tela do erro", "foto-3f2a.png", "Tela do erro.png"),
            ("dados.csv", "dados-3f2a.csv", "dados.csv"),
            ("DADOS.CSV", "dados-3f2a.csv", "DADOS.CSV"),
            ("Versão 1.2", "relatorio.pdf", "Versão 1.2.pdf"),
            ("relatorio.pdf", "relatorio.docx", "relatorio.pdf.docx"),
            ("sem extensão", "anexo", "sem extensão"),
            ("sem extensão", "", "sem extensão"),
            ("sem extensão", ".oculto", "sem extensão"),
            ("sem extensão", "arquivo.", "sem extensão"),
        ] {
            assert_eq!(
                with_stored_extension(name, stored),
                expected,
                "{name} {stored}"
            );
        }

        let taken = [
            "a.csv".to_owned(),
            "a (2).csv".to_owned(),
            "semponto".to_owned(),
        ];
        assert_eq!(unique_name("b.csv", &taken), "b.csv");
        assert_eq!(unique_name("a.csv", &taken), "a (3).csv");
        assert_eq!(unique_name("semponto", &taken), "semponto (2)");
        assert_eq!(
            unique_name(".oculto", &[".oculto".to_owned()]),
            ".oculto (2)"
        );
    }

    const ATTACHED_TICKET: &str = r#"<main id="content"><div class="title-container"><h2>Chamado 5</h2></div>
        <div data-tab="linha_tempo"><ul class="timeline">
          <li><div class="timeline-date">1</div><div class="timeline-content"><p>Ana anexou
            <a href="/djtools/arquivo/centralservicos/chamadoanexo/1/anexo/">dados.csv</a>
            <a href="/djtools/arquivo/centralservicos/chamadoanexo/2/anexo/">../foto.png</a>
            <a href="/djtools/arquivo/centralservicos/chamadoanexo/3/anexo/">dados.csv</a></p></div></li>
        </ul></div></main>"#;

    const VANISHED_TICKET: &str = r#"<main id="content"><div class="title-container"><h2>Sumiu</h2></div>
        <div data-tab="linha_tempo"><ul class="timeline"><li><div class="timeline-date">1</div>
        <div class="timeline-content"><a href="/djtools/arquivo/centralservicos/chamadoanexo/99/anexo/">x.pdf</a></div></li></ul></div></main>"#;

    const DESCRIBED_TICKET: &str = r#"<main id="content"><div class="title-container"><h2>Descrito</h2></div>
        <div data-tab="linha_tempo"><ul class="timeline"><li><div class="timeline-date">1</div>
        <div class="timeline-content"><a href="/djtools/arquivo/centralservicos/chamadoanexo/20/anexo/">Tela do erro</a></div></li></ul></div></main>"#;

    const PLAIN_TICKET: &str =
        r#"<main id="content"><div class="title-container"><h2>Sem anexos</h2></div></main>"#;

    #[test]
    fn show_lists_attachments_and_download_saves_them_safely() {
        let (dir, paths) = paths();
        let (runtime, server) = bare_server();
        run_args(&["profile", "init", "--base-url", &server.uri()], &paths);
        for (id, body) in [
            ("5", ATTACHED_TICKET),
            ("6", PLAIN_TICKET),
            ("7", VANISHED_TICKET),
        ] {
            mount_text(
                &runtime,
                &server,
                "GET",
                &format!("/centralservicos/chamado/{id}/"),
                ResponseTemplate::new(200).set_body_string(body),
            );
        }
        for (number, content) in [(1, "um"), (2, "dois"), (3, "três")] {
            mount_text(
                &runtime,
                &server,
                "GET",
                &format!("/djtools/arquivo/centralservicos/chamadoanexo/{number}/anexo/"),
                ResponseTemplate::new(200).set_body_bytes(content.as_bytes().to_vec()),
            );
        }
        // An attachment shown by its description is named after the stored file's extension.
        mount_text(
            &runtime,
            &server,
            "GET",
            "/centralservicos/chamado/10/",
            ResponseTemplate::new(200).set_body_string(DESCRIBED_TICKET),
        );
        mount_text(
            &runtime,
            &server,
            "GET",
            "/djtools/arquivo/centralservicos/chamadoanexo/20/anexo/",
            ResponseTemplate::new(302).insert_header("location", "/media/foto-3f2a.png"),
        );
        mount_text(
            &runtime,
            &server,
            "GET",
            "/media/foto-3f2a.png",
            ResponseTemplate::new(200).set_body_bytes(b"imagem".to_vec()),
        );

        let (_, shown, _) = run_args(&["show", "5"], &paths);
        let listed = "Anexos (baixe com `chamados download 5`):\n  1. dados.csv\n  2. ../foto.png\n  3. dados.csv\n";
        assert!(shown.contains(listed), "{shown}");
        assert!(!run_args(&["show", "6"], &paths).1.contains("Anexos"));

        // All of them, into a folder that does not exist yet; names are sanitized and made unique.
        let target = dir.path().join("baixados");
        let target_text = target.to_str().unwrap();
        let (code, out, err) = run_args(&["download", "5", "-o", target_text], &paths);
        assert_eq!((code, err.as_str()), (0, ""));
        assert_eq!(out.lines().count(), 3, "{out}");
        assert!(
            out.contains("Anexo 2: ") && out.contains("(4 bytes)"),
            "{out}"
        );
        assert_eq!(
            std::fs::read_to_string(target.join("dados.csv")).unwrap(),
            "um"
        );
        assert_eq!(
            std::fs::read_to_string(target.join("foto.png")).unwrap(),
            "dois"
        );
        assert_eq!(
            std::fs::read_to_string(target.join("dados (2).csv")).unwrap(),
            "três"
        );

        // An existing file is never overwritten silently, and nothing is downloaded in that case.
        std::fs::write(target.join("dados.csv"), "meu").unwrap();
        let before = runtime.block_on(server.received_requests()).unwrap().len();
        let (code, _, err) = run_args(&["download", "5", "-o", target_text], &paths);
        assert_eq!(code, 1);
        let refused = err.contains("já existe") && err.contains("dados.csv");
        assert!(refused && err.contains("--force"), "{err}");
        assert_eq!(
            std::fs::read_to_string(target.join("dados.csv")).unwrap(),
            "meu"
        );
        // The files were fetched (their stored names matter), but none was written.
        let after = runtime.block_on(server.received_requests()).unwrap().len();
        assert_eq!(after, before + 1 + 3, "the ticket page and its three files");
        assert!(!target.join("dados (2).csv.tmp").exists());
        let (code, _, _) = run_args(&["download", "5", "-o", target_text, "--force"], &paths);
        assert_eq!(code, 0);
        assert_eq!(
            std::fs::read_to_string(target.join("dados.csv")).unwrap(),
            "um"
        );

        let (code, out, _) = run_args(&["download", "10", "-o", target_text], &paths);
        assert_eq!(code, 0, "{out}");
        assert_eq!(
            std::fs::read_to_string(target.join("Tela do erro.png")).unwrap(),
            "imagem"
        );

        // Chosen attachments only (repeated numbers count once).
        let only = dir.path().join("so-um");
        let only_text = only.to_str().unwrap();
        let (code, out, _) = run_args(
            &[
                "download", "5", "--anexo", "2", "--anexo", "2", "-o", only_text,
            ],
            &paths,
        );
        assert_eq!((code, out.lines().count()), (0, 1));
        assert!(only.join("foto.png").exists() && !only.join("dados.csv").exists());

        // Mistakes.
        let (code, _, err) = run_args(&["download", "5", "--anexo", "4"], &paths);
        assert_eq!(code, 1);
        assert!(
            err.contains("o anexo 4 não existe") && err.contains("tem 3"),
            "{err}"
        );
        let (code, _, err) = run_args(&["download", "5", "--anexo", "0"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("o anexo 0 não existe"), "{err}");
        let (code, _, err) = run_args(&["download", "6"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("não tem anexos"), "{err}");
        let (code, _, err) = run_args(&["download", "9"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("404"), "{err}");
        // A file SUAP no longer serves is reported, and nothing is left behind.
        let (code, _, err) = run_args(&["download", "7", "-o", target_text], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("404"), "{err}");
        assert!(!target.join("x.pdf").exists());
    }

    #[test]
    fn assume_and_start_work_on_existing_tickets() {
        let (_dir, paths) = paths();
        let (runtime, server) = bare_server();
        run_args(&["profile", "init", "--base-url", &server.uri()], &paths);
        for (prefix, id, body) in [
            ("auto_atribuir_chamado", "5", "ok"),
            ("colocar_em_atendimento", "5", "ok"),
            (
                "colocar_em_atendimento",
                "6",
                "<p class='alert-error'>Assuma antes</p>",
            ),
        ] {
            mount_text(
                &runtime,
                &server,
                "GET",
                &format!("/centralservicos/{prefix}/{id}/"),
                ResponseTemplate::new(200).set_body_string(body),
            );
        }

        let (code, out, err) = run_args(&["assume", "5"], &paths);
        assert_eq!(
            (code, err.as_str(), out.as_str()),
            (0, "", "Chamado #5 assumido.\n")
        );
        let (code, out, err) = run_args(&["start", "5"], &paths);
        assert_eq!(
            (code, err.as_str(), out.as_str()),
            (0, "", "Chamado #5 em atendimento.\n")
        );
        let (code, out, err) = run_args(&["start", "5", "--assume"], &paths);
        assert_eq!(
            (code, err.as_str(), out.as_str()),
            (0, "", "Chamado #5 assumido.\nChamado #5 em atendimento.\n")
        );

        // The SUAP's refusal is reported, and nothing else is claimed.
        let (code, out, err) = run_args(&["start", "6"], &paths);
        assert_eq!((code, out.as_str()), (1, ""));
        assert!(err.contains("Assuma antes"), "{err}");
        let (code, _, err) = run_args(&["assume", "9"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("404"), "{err}");
        let (code, out, _) = run_args(&["start", "9", "--assume"], &paths);
        assert_eq!((code, out.as_str()), (1, ""));
    }

    #[test]
    fn cancel_needs_confirmation_and_sends_the_reason() {
        let (_dir, paths) = paths();
        let (runtime, server) = bare_server();
        run_args(&["profile", "init", "--base-url", &server.uri()], &paths);
        let form = r#"<form action="" method="POST"><input type="hidden" name="csrfmiddlewaretoken" value="tok">
            <textarea name="observacao"></textarea></form>"#;
        mount_text(
            &runtime,
            &server,
            "GET",
            "/centralservicos/cancelar_chamado/5/",
            ResponseTemplate::new(200).set_body_string(form),
        );
        mount_text(
            &runtime,
            &server,
            "POST",
            "/centralservicos/cancelar_chamado/5/",
            ResponseTemplate::new(200),
        );

        // Without --yes nothing is read or sent.
        let (code, _, err) = run_args(&["cancel", "5", "-m", "x"], &paths);
        assert_eq!(code, 1);
        assert!(
            err.contains("não pode ser desfeito") && err.contains("--yes"),
            "{err}"
        );
        assert!(runtime
            .block_on(server.received_requests())
            .unwrap()
            .is_empty());

        let (code, out, err) = run_args(
            &["cancel", "5", "--yes", "-m", "aberto por engano\ndesculpe"],
            &paths,
        );
        assert_eq!(
            (code, err.as_str(), out.as_str()),
            (0, "", "Chamado #5 cancelado.\n")
        );
        let (code, _, _) = run_with_input(&["cancel", "5", "--yes"], &paths, "pelo stdin\n");
        assert_eq!(code, 0);
        let requests = runtime.block_on(server.received_requests()).unwrap();
        let bodies: Vec<String> = requests
            .iter()
            .filter(|request| request.method == wiremock::http::Method::POST)
            .map(|request| String::from_utf8_lossy(&request.body).into_owned())
            .collect();
        assert!(bodies[0].contains("observacao=aberto+por+engano%0Adesculpe"));
        assert!(bodies[1].contains("observacao=pelo+stdin"));

        let (code, _, err) = run_with_input(&["cancel", "5", "--yes"], &paths, "\n");
        assert_eq!(code, 1);
        assert!(err.contains("texto não informado"));
        let (code, _, err) = run_args(&["cancel", "9", "--yes", "-m", "x"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("ticket 9 cannot be cancelled"));
    }

    #[test]
    fn resolve_sends_the_message_articles_and_other_tickets() {
        let (_dir, paths) = paths();
        let (runtime, server) = bare_server();
        run_args(&["profile", "init", "--base-url", &server.uri()], &paths);
        let form = r#"<form action="" method="POST"><input type="hidden" name="csrfmiddlewaretoken" value="tok">
            <input type="checkbox" name="bases_conhecimento" value="11">
            <input type="checkbox" name="bases_conhecimento" value="12">
            <input type="checkbox" name="outros_chamados_a_resolver" value="7">
            <textarea name="comentario"></textarea></form>"#;
        mount_text(
            &runtime,
            &server,
            "GET",
            "/centralservicos/resolver_chamado/5/",
            ResponseTemplate::new(200).set_body_string(form),
        );
        mount_text(
            &runtime,
            &server,
            "POST",
            "/centralservicos/resolver_chamado/5/",
            ResponseTemplate::new(200),
        );

        let (code, out, err) = run_args(&["resolve", "5", "-m", "feito"], &paths);
        assert_eq!(
            (code, err.as_str(), out.as_str()),
            (0, "", "Chamado #5 resolvido.\n")
        );
        let args = [
            "resolve",
            "5",
            "-m",
            "feito\nok",
            "--article",
            "12",
            "--also",
            "7",
            "--standard-reply",
            "3",
        ];
        let (code, out, _) = run_args(&args, &paths);
        assert_eq!(code, 0);
        assert_eq!(out, "Chamado #5 resolvido, junto com #7.\n");

        let requests = runtime.block_on(server.received_requests()).unwrap();
        let bodies: Vec<String> = requests
            .iter()
            .filter(|request| request.method == wiremock::http::Method::POST)
            .map(|request| String::from_utf8_lossy(&request.body).into_owned())
            .collect();
        assert!(
            bodies[0].contains("bases_conhecimento=11")
                && !bodies[0].contains("bases_conhecimento=12")
        );
        assert!(
            bodies[1].contains("bases_conhecimento=12")
                && bodies[1].contains("outros_chamados_a_resolver=7")
        );
        assert!(
            bodies[1].contains("comentario=feito%0Aok") && bodies[1].contains("resposta_padrao=3")
        );

        let (code, _, err) = run_args(&["resolve", "5", "-m", "x", "--article", "99"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("article 99 is not offered"));
    }

    /// Commands that take free text, with the option that carries it, the form field SUAP receives
    /// it in and the path it is posted to. Each must satisfy requirement RS-02.
    const TEXT_COMMANDS: [(&[&str], &str, &str, &str); 10] = [
        (
            &["open"],
            "-d",
            "descricao",
            "/centralservicos/abrir_chamado/7/",
        ),
        (
            &["comment", "5"],
            "-m",
            "texto",
            "/centralservicos/adicionar_comentario/5/",
        ),
        (
            &["note", "5"],
            "-m",
            "texto",
            "/centralservicos/adicionar_nota_interna/5/",
        ),
        (
            &["suspend", "5"],
            "-m",
            "observacao",
            "/centralservicos/suspender_chamado/5/",
        ),
        (
            &["resolve", "5"],
            "-m",
            "comentario",
            "/centralservicos/resolver_chamado/5/",
        ),
        (
            &["cancel", "5", "--yes"],
            "-m",
            "observacao",
            "/centralservicos/cancelar_chamado/5/",
        ),
        (
            &["reopen", "5"],
            "-m",
            "observacao",
            "/centralservicos/reabrir_chamado/5/",
        ),
        (
            &["escalate", "5"],
            "-m",
            "texto",
            "/centralservicos/escalar_atendimento_chamado/5/",
        ),
        (
            &["return", "5"],
            "-m",
            "texto",
            "/centralservicos/retornar_atendimento_chamado/5/",
        ),
        (
            &["reclassify", "5", "--servico", "2"],
            "-m",
            "justificativa",
            "/centralservicos/reclassificar_chamado/5/",
        ),
    ];

    /// Commands whose text is optional: the same multi-line and `-` (standard input) rules apply, but
    /// omitted or blank text means "no text" instead of an error.
    const OPTIONAL_TEXT_COMMANDS: [(&[&str], &str, &str, &str); 1] = [(
        &["close", "5"],
        "-m",
        "comentario",
        "/centralservicos/fechar_chamado/5/",
    )];

    /// Commands without free text. A new command must be added to one of the two lists, which forces
    /// a decision about RS-02 (and a test for it) whenever a command is created.
    const NON_TEXT_COMMANDS: [&str; 17] = [
        "sync",
        "attach",
        "watch",
        "assign",
        "tag",
        "interested",
        "download",
        "assume",
        "start",
        "paths",
        "profile",
        "session-status",
        "login",
        "list",
        "show",
        "title",
        "status",
    ];

    #[test]
    fn every_command_is_classified_for_the_text_input_requirement() {
        let mut declared: Vec<&str> = TEXT_COMMANDS.iter().map(|command| command.0[0]).collect();
        declared.extend(OPTIONAL_TEXT_COMMANDS.iter().map(|command| command.0[0]));
        declared.extend(NON_TEXT_COMMANDS);
        declared.sort_unstable();
        let command = Cli::command();
        let mut actual: Vec<&str> = command
            .get_subcommands()
            .map(|sub| sub.get_name())
            .collect();
        actual.sort_unstable();
        assert_eq!(declared, actual, "classify the new command for RS-02");
    }

    /// One mock SUAP that accepts every text-carrying command.
    fn text_requirement_setup() -> (TempDir, AppPaths, tokio::runtime::Runtime, MockServer) {
        let (dir, paths, runtime, server) = open_setup();
        let get = |path: &str, body: &str| {
            let page = ResponseTemplate::new(200).set_body_string(body.to_owned());
            mount_text(&runtime, &server, "GET", path, page);
        };
        get("/centralservicos/chamado/5/", THREAD_PAGE);
        let field = |name: &str| {
            format!(r#"<form action="" method="POST"><textarea name="{name}"></textarea></form>"#)
        };
        get(
            "/centralservicos/suspender_chamado/5/",
            &field("observacao"),
        );
        get("/centralservicos/resolver_chamado/5/", &field("comentario"));
        get("/centralservicos/cancelar_chamado/5/", &field("observacao"));
        get("/centralservicos/reabrir_chamado/5/", &field("observacao"));
        get(
            "/centralservicos/escalar_atendimento_chamado/5/",
            &field("texto"),
        );
        get(
            "/centralservicos/retornar_atendimento_chamado/5/",
            &field("texto"),
        );
        get(
            "/centralservicos/reclassificar_chamado/5/",
            r#"<form method="POST"><input type="hidden" name="servico" value="1">
                <textarea name="justificativa"></textarea></form>"#,
        );
        get(
            "/centralservicos/get_campus_com_centros_atendimento/2/5/",
            r#"{"campus": [[1, "ZL", true]]}"#,
        );
        get(
            "/centralservicos/get_centros_atendimento_por_servico_e_campus/2/1/",
            r#"{"centros": [[1, "A", true]]}"#,
        );
        get("/centralservicos/fechar_chamado/5/", &field("comentario"));
        for path in [
            "/centralservicos/adicionar_comentario/5/",
            "/centralservicos/adicionar_nota_interna/5/",
            "/centralservicos/suspender_chamado/5/",
            "/centralservicos/resolver_chamado/5/",
            "/centralservicos/cancelar_chamado/5/",
            "/centralservicos/reabrir_chamado/5/",
            "/centralservicos/fechar_chamado/5/",
            "/centralservicos/escalar_atendimento_chamado/5/",
            "/centralservicos/retornar_atendimento_chamado/5/",
            "/centralservicos/reclassificar_chamado/5/",
        ] {
            let accepted = ResponseTemplate::new(200).set_body_string("ok");
            mount_text(&runtime, &server, "POST", path, accepted);
        }
        (dir, paths, runtime, server)
    }

    fn posts_to(runtime: &tokio::runtime::Runtime, server: &MockServer, path: &str) -> Vec<String> {
        let requests = runtime.block_on(server.received_requests()).unwrap();
        requests
            .iter()
            .filter(|request| request.method == wiremock::http::Method::POST)
            .filter(|request| request.url.path() == path)
            .map(|request| String::from_utf8_lossy(&request.body).into_owned())
            .collect()
    }

    #[test]
    fn rs02_every_text_input_accepts_multiple_lines_and_standard_input() {
        let (_dir, paths, runtime, server) = text_requirement_setup();
        for (command, flag, field, post_path) in TEXT_COMMANDS {
            let expected = format!("{field}=linha+1%0Alinha+2");
            let with_flag = |value: &'static str| [command, &[flag, value]].concat();

            // (a) several lines in the option itself
            let (code, _, err) = run_args(&with_flag("linha 1\nlinha 2"), &paths);
            assert_eq!((code, err.as_str()), (0, ""), "{command:?} option");
            // (b) standard input with `-`
            let (code, _, err) = run_with_input(&with_flag("-"), &paths, "linha 1\nlinha 2\n");
            assert_eq!((code, err.as_str()), (0, ""), "{command:?} dash");
            // (c) standard input when the option is omitted (CRLF line ending removed)
            let (code, _, err) = run_with_input(command, &paths, "linha 1\nlinha 2\r\n");
            assert_eq!((code, err.as_str()), (0, ""), "{command:?} omitted");

            let posts = posts_to(&runtime, &server, post_path);
            assert_eq!(posts.len(), 3, "{command:?}");
            for body in &posts {
                let encoded = body.split('&').any(|pair| pair == expected);
                assert!(encoded, "{command:?} sent {body}");
            }

            // (d) empty input is refused before anything is sent
            let (code, _, err) = run_with_input(command, &paths, " \n");
            assert_eq!(code, 1, "{command:?} empty");
            assert!(err.contains("texto não informado"), "{command:?}: {err}");
            assert_eq!(
                posts_to(&runtime, &server, post_path).len(),
                3,
                "{command:?}"
            );
        }
    }

    #[test]
    fn rs02_optional_texts_follow_the_same_rules_and_may_be_blank() {
        let (_dir, paths, runtime, server) = text_requirement_setup();
        for (command, flag, field, post_path) in OPTIONAL_TEXT_COMMANDS {
            let expected = format!("{field}=linha+1%0Alinha+2");
            let with_flag = |value: &'static str| [command, &[flag, value]].concat();
            let (code, _, err) = run_args(&with_flag("linha 1\nlinha 2"), &paths);
            assert_eq!((code, err.as_str()), (0, ""), "{command:?} option");
            let (code, _, err) = run_with_input(&with_flag("-"), &paths, "linha 1\nlinha 2\n");
            assert_eq!((code, err.as_str()), (0, ""), "{command:?} dash");
            let posts = posts_to(&runtime, &server, post_path);
            assert_eq!(posts.len(), 2, "{command:?}");
            for body in &posts {
                let encoded = body.split('&').any(|pair| pair == expected);
                assert!(encoded, "{command:?} sent {body}");
            }

            // Omitted: there is no text, and standard input is left alone (never read).
            let (code, _, err) = run_with_input(command, &paths, "linha 1\nlinha 2\r\n");
            assert_eq!((code, err.as_str()), (0, ""), "{command:?} omitted");
            // Blank text is not an error either: it is sent without the comment.
            let (code, _, err) = run_with_input(&with_flag("-"), &paths, " \n");
            assert_eq!((code, err.as_str()), (0, ""), "{command:?} blank");
            let posts = posts_to(&runtime, &server, post_path);
            assert_eq!(posts.len(), 4, "{command:?}");
            let sent_text = |body: &String| body.contains(&format!("{field}=linha"));
            assert!(!posts[2..].iter().any(sent_text));
        }
    }

    const MANAGE_TICKET: &str = r#"<main id="content"><ul class="tags">
        <li>Rede <form method="post" action="/centralservicos/remover_tag_do_chamado/5/1/"></form></li></ul>
        <div class="person"><div class="popup-user"><a href="/rh/servidor/2080883/">Ana Souza</a></div>
          <form method="post" action="/centralservicos/remover_outros_interessados/5/2/"></form></div></main>"#;

    #[test]
    fn management_commands_report_what_they_did_and_what_suap_refuses() {
        let (_dir, paths) = paths();
        let (runtime, server) = bare_server();
        run_args(&["profile", "init", "--base-url", &server.uri()], &paths);
        let ok = |body: &str| ResponseTemplate::new(200).set_body_string(body.to_owned());
        let people = r#"<select name="atribuido_para"><option value="">-</option>
            <option value="2">Ana (2080883)</option></select>"#;
        for (verb, at, body) in [
            ("GET", "/centralservicos/atribuir_chamado/5/", format!("<form>{people}</form>")),
            ("POST", "/centralservicos/atribuir_chamado/5/", "ok".to_owned()),
            ("GET", "/centralservicos/adicionar_tags_ao_chamado/5/", r#"<form><label><input type="checkbox" name="tags" value="1"> Rede</label></form>"#.to_owned()),
            ("POST", "/centralservicos/adicionar_tags_ao_chamado/5/", "ok".to_owned()),
            ("GET", "/centralservicos/chamado/5/", MANAGE_TICKET.to_owned()),
            ("POST", "/centralservicos/remover_tag_do_chamado/5/1/", "ok".to_owned()),
            ("POST", "/centralservicos/remover_outros_interessados/5/2/", "ok".to_owned()),
            ("GET", "/centralservicos/adicionar_outros_interessados/5/", r#"<form><select name="outros_interessados"></select><script>control: '{"x": 1}'</script></form>"#.to_owned()),
            ("POST", "/centralservicos/adicionar_outros_interessados/5/", "ok".to_owned()),
            ("GET", "/json/comum/vinculo/", r#"{"items": [{"id": 2, "html": "<dd class=\"title\">Ana (Mat. 2080883)</dd>"}]}"#.to_owned()),
            ("GET", "/centralservicos/escalar_atendimento_chamado/5/", format!("<form><textarea name=\"texto\"></textarea>{people}</form>")),
            ("POST", "/centralservicos/escalar_atendimento_chamado/5/", "ok".to_owned()),
            ("GET", "/centralservicos/retornar_atendimento_chamado/5/", "<form><textarea name=\"texto\"></textarea></form>".to_owned()),
            ("POST", "/centralservicos/retornar_atendimento_chamado/5/", "ok".to_owned()),
            ("GET", "/centralservicos/reclassificar_chamado/5/", r#"<form><input type="hidden" name="servico" value="1"><textarea name="justificativa"></textarea></form>"#.to_owned()),
            ("POST", "/centralservicos/reclassificar_chamado/5/", "ok".to_owned()),
            ("GET", "/centralservicos/get_campus_com_centros_atendimento/3/5/", r#"{"campus": [[1, "ZL", true]]}"#.to_owned()),
            ("GET", "/centralservicos/get_centros_atendimento_por_servico_e_campus/3/1/", r#"{"centros": [[7, "G", true]]}"#.to_owned()),
        ] {
            mount_text(&runtime, &server, verb, at, ok(&body));
        }

        let expected = [
            (
                vec!["assign", "5", "--para", "2080883"],
                "Chamado #5 atribuído a Ana (2080883).\n",
            ),
            (
                vec!["escalate", "5", "-m", "sobe", "--para", "ana"],
                "Chamado #5 escalado para o grupo acima.\n",
            ),
            (
                vec!["return", "5", "-m", "volta"],
                "Chamado #5 devolvido ao grupo abaixo.\n",
            ),
            (
                vec!["reclassify", "5", "--servico", "3", "-m", "outro serviço"],
                "Chamado #5 reclassificado.\n",
            ),
            (
                vec!["tag", "add", "5", "rede"],
                "Chamado #5: tags adicionadas: Rede.\n",
            ),
            (
                vec!["tag", "remove", "5", "rede"],
                "Chamado #5: tags removidas: Rede.\n",
            ),
            (
                vec!["interested", "add", "5", "2080883"],
                "Chamado #5: outros interessados adicionados: Ana (Mat. 2080883).\n",
            ),
            (
                vec!["interested", "remove", "5", "2080883"],
                "Chamado #5: outros interessados removidos: Ana Souza (2080883).\n",
            ),
        ];
        for (args, message) in expected {
            let (code, out, err) = run_args(&args, &paths);
            assert_eq!(
                (code, err.as_str(), out.as_str()),
                (0, "", message),
                "{args:?}"
            );
        }
        let bodies = posts_to(
            &runtime,
            &server,
            "/centralservicos/reclassificar_chamado/5/",
        );
        let sent = bodies[0].contains("servico=3") && bodies[0].contains("centro_atendimento=7");
        assert!(sent);

        // Mistakes are reported with the options, or by clap before anything is sent.
        let (code, _, err) = run_args(&["assign", "5", "--para", "zé"], &paths);
        assert_eq!(code, 1);
        assert!(
            err.contains("no attendant matches") && err.contains("2080883"),
            "{err}"
        );
        let (code, _, err) = run_args(&["reclassify", "5", "-m", "x"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("nothing to change"), "{err}");
        let (code, _, err) = run_args(&["tag", "add", "9", "rede"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("ticket 9 cannot be tagged"), "{err}");
        for args in [
            &["tag", "add", "5"][..],
            &["interested", "remove", "5"],
            &["assign", "5"],
        ] {
            let (code, _, err) = run_args(args, &paths);
            assert_eq!(code, 2, "{args:?}: {err}");
        }
    }

    fn json_of(text: &str) -> serde_json::Value {
        serde_json::from_str(text.trim()).expect("the command printed valid JSON")
    }

    #[test]
    fn reading_commands_print_json_for_scripts() {
        use serde_json::json;
        let (_empty_dir, empty) = paths();
        let (_dir, paths) = paths();
        let (runtime, server) = bare_server();
        run_args(
            &[
                "profile",
                "init",
                "--base-url",
                &server.uri(),
                "--username",
                "ana",
            ],
            &paths,
        );
        let listing = r#"<div class="general-box"><span class="status">Em atendimento</span>
            <h4><a href="/centralservicos/chamado/7/">REQ #7 <strong>Assunto</strong></a></h4></div>
            <div class="general-box"><h4><a href="/centralservicos/chamado/8/">REQ #8</a></h4></div>"#;
        mount_text(
            &runtime,
            &server,
            "GET",
            "/centralservicos/listar_chamados_suporte/",
            ResponseTemplate::new(200).set_body_string(listing),
        );
        mount_text(
            &runtime,
            &server,
            "GET",
            "/centralservicos/chamado/5/",
            ResponseTemplate::new(200).set_body_string(ATTACHED_TICKET),
        );
        mount_text(
            &runtime,
            &server,
            "GET",
            "/centralservicos/chamado/6/",
            ResponseTemplate::new(200).set_body_string(
                r#"<main id="content"><div class="title-container"><h2>Chamado 6</h2></div>
                <div class="accordion"><button class="accordion-button">1.2 Gestão</button><div class="accordion-body">
                <dl class="definition-list"><div class="list-item"><dt>Interessado</dt><dd>Ana</dd></div></dl></div></div></main>"#,
            ),
        );
        run_args(&["title", "7", "Meu título"], &paths);

        // list: ids are numbers, missing values are null.
        let (code, out, err) = run_args(&["list", "--json"], &paths);
        assert_eq!((code, err.as_str()), (0, ""));
        let tickets = json_of(&out);
        assert_eq!(tickets[0]["id"], json!(7));
        assert_eq!(tickets[0]["status"], json!("Em atendimento"));
        assert_eq!(tickets[0]["title"], json!("Meu título"));
        assert_eq!(tickets[0]["subject"], json!("Assunto"));
        assert!(tickets[0]["url"]
            .as_str()
            .unwrap()
            .ends_with("/centralservicos/chamado/7/"));
        assert_eq!(tickets[1]["status"], json!(null));
        assert_eq!(tickets[1]["title"], json!(null));
        let (_, out, _) = run_args(&["list", "--json", "--titulo", "nada"], &paths);
        assert_eq!(json_of(&out), json!([]));

        // show
        let (code, out, _) = run_args(&["show", "5", "--json"], &paths);
        assert_eq!(code, 0);
        let details = json_of(&out);
        assert_eq!(details["id"], json!(5));
        assert_eq!(details["title"], json!("Chamado 5"));
        assert_eq!(details["local_title"], json!(null));
        assert_eq!(
            details["attachments"][1],
            json!({
                "number": 2,
                "name": "../foto.png",
                "path": "/djtools/arquivo/centralservicos/chamadoanexo/2/anexo/",
            })
        );
        assert_eq!(details["timeline"][0]["date"], json!("1"));
        assert!(details["fields"].as_array().unwrap().is_empty());
        let (_, out, _) = run_args(&["show", "6", "--json"], &paths);
        let with_fields = json_of(&out);
        assert_eq!(
            with_fields["fields"],
            json!([{"label": "Interessado", "value": "Ana"}])
        );
        assert_eq!(with_fields["service"], json!("1.2 Gestão"));

        // profile show / list
        let (_, out, _) = run_args(&["profile", "show", "--json"], &paths);
        let profile = json_of(&out);
        assert_eq!(profile["profile"], json!("default"));
        assert_eq!(profile["username"], json!("ana"));
        assert_eq!(profile["sync"], json!(false));
        assert_eq!(profile["session_saved"], json!(false));
        assert_eq!(profile["open"]["service"], json!(null));
        let (_, out, _) = run_args(&["profile", "list", "--json"], &paths);
        let profiles = json_of(&out);
        assert_eq!(profiles[0]["name"], json!("default"));
        assert_eq!(profiles[0]["default"], json!(true));
        // Without a saved profile the built-in default is described, with no hint line before the JSON.

        let (_, out, _) = run_args(&["profile", "show", "--json"], &empty);
        assert_eq!(json_of(&out)["username"], json!(null));
        let (_, out, _) = run_args(&["profile", "list", "--json"], &empty);
        assert_eq!(json_of(&out), json!([]));
    }

    #[test]
    fn sync_prints_json_for_scripts() {
        use serde_json::json;
        let root = tempdir().unwrap();
        let cloud = root.path().join("nuvem");
        let (_dir, paths) = paths();
        setup_sync(&paths, &cloud, &root.path().join("k"));
        run_args(&["sync", "key", "generate"], &paths);
        run_args(&["profile", "init", "--sync", "true"], &paths);

        let (code, out, _) = run_args(&["sync", "--check", "--json"], &paths);
        assert_eq!(code, 0);
        assert_eq!(
            json_of(&out),
            json!({
                "backend": "directory",
                "conditional_writes": true,
                "matches_configuration": true,
                "key_source": "file",
                "key_present": true,
            })
        );
        let (_, out, _) = run_args(&["sync", "--dry-run", "--json"], &paths);
        assert_eq!(
            json_of(&out),
            json!({"profiles": 1, "local_changes": 0, "cloud_updated": true, "dry_run": true})
        );
        let (_, out, _) = run_args(&["sync", "--json"], &paths);
        assert_eq!(json_of(&out)["dry_run"], json!(false));
        // Quiet wins over JSON.
        assert_eq!(run_args(&["sync", "--json", "--quiet"], &paths).1, "");
    }

    #[test]
    fn watch_reports_what_changed_between_looks() {
        use serde_json::json;
        let (_dir, paths) = paths();
        let (runtime, server) = bare_server();
        run_args(&["profile", "init", "--base-url", &server.uri()], &paths);
        let boxes = |tickets: &[(u32, &str)]| -> String {
            let items: Vec<String> = tickets
                .iter()
                .map(|(id, status)| {
                    format!(
                        r#"<div class="general-box"><span class="status">{status}</span>
                        <h4><a href="/centralservicos/chamado/{id}/">REQ #{id} <strong>Assunto {id}</strong></a></h4></div>"#
                    )
                })
                .collect();
            items.concat()
        };
        let serve = |body: String| {
            runtime.block_on(server.reset());
            mount_text(
                &runtime,
                &server,
                "GET",
                "/centralservicos/meus_chamados/",
                ResponseTemplate::new(200).set_body_string(body),
            );
        };
        run_args(&["title", "7", "Meu título"], &paths);

        // First look: only a baseline, and it says so.
        serve(boxes(&[(7, "Aberto"), (8, "Aberto")]));
        let (code, out, err) = run_args(&["watch", "--rodadas", "1"], &paths);
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(
            out.contains("Acompanhando (mine)") && out.lines().count() == 1,
            "{out}"
        );
        assert!(paths.watch_file().exists());
        // Looking again at the same list: nothing to say.
        let (_, out, _) = run_args(&["watch", "--rodadas", "1"], &paths);
        assert_eq!(out, "");

        // #7 changes, #8 leaves, #9 appears.
        serve(boxes(&[(7, "Resolvido"), (9, "Aberto")]));
        let (_, out, _) = run_args(&["watch", "--rodadas", "1"], &paths);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3, "{out}");
        assert!(
            lines[0].contains("#7 (Meu título)  situação: Aberto -> Resolvido"),
            "{out}"
        );
        assert!(lines[1].contains("#9  novo: Aberto - Assunto 9"), "{out}");
        assert!(
            lines[2].contains("#8  saiu da lista (última situação: Aberto)"),
            "{out}"
        );
        assert!(lines[0].starts_with("20") && lines[0].contains('T') && lines[0].contains("Z  #"));

        // JSON lines, with the local title.
        serve(boxes(&[(7, "Fechado"), (9, "Aberto")]));
        let (_, out, _) = run_args(&["watch", "--rodadas", "1", "--json"], &paths);
        let event = json_of(&out);
        assert_eq!(event["event"], json!("status"));
        assert_eq!(event["id"], json!(7));
        assert_eq!(event["from"], json!("Resolvido"));
        assert_eq!(event["to"], json!("Fechado"));
        assert_eq!(event["title"], json!("Meu título"));
        assert!(event["time"].as_u64().unwrap() > 1_700_000_000);

        // Another list (the support queue) is another baseline.
        mount_text(
            &runtime,
            &server,
            "GET",
            "/centralservicos/listar_chamados_suporte/",
            ResponseTemplate::new(200).set_body_string(boxes(&[(1, "Aberto")])),
        );
        let (_, out, _) = run_args(&["watch", "--rodadas", "1", "--suporte", "--todos"], &paths);
        assert!(
            out.contains("Acompanhando (support)") && out.lines().count() == 1,
            "{out}"
        );

        // A failed look is reported and the next round tries again (here it recovers).
        serve(boxes(&[(7, "Fechado")]));
        let (_, _, _) = run_args(&["watch", "--rodadas", "1"], &paths);
        runtime.block_on(server.reset());
        mount_text(
            &runtime,
            &server,
            "GET",
            "/centralservicos/meus_chamados/",
            ResponseTemplate::new(500),
        );
        let (code, out, _) = run_args(&["watch", "--rodadas", "1"], &paths);
        assert_eq!(code, 0);
        assert!(
            out.contains("aviso: consulta falhou") && out.contains("próxima rodada"),
            "{out}"
        );
        let (_, out, _) = run_args(&["watch", "--rodadas", "1", "--json"], &paths);
        let failure = json_of(&out);
        assert_eq!(failure["event"], json!("error"));
        assert!(failure["message"].as_str().unwrap().contains("500"));
    }

    #[test]
    fn watch_pauses_between_rounds_follows_messages_and_needs_a_session() {
        use serde_json::json;
        let (_dir, paths) = paths();
        let (runtime, server) = bare_server();
        run_args(&["profile", "init", "--base-url", &server.uri()], &paths);
        let listing = r#"<div class="general-box"><span class="status">Aberto</span>
            <h4><a href="/centralservicos/chamado/7/">REQ #7 <strong>Assunto</strong></a></h4></div>"#;
        mount_text(
            &runtime,
            &server,
            "GET",
            "/centralservicos/meus_chamados/",
            ResponseTemplate::new(200).set_body_string(listing),
        );
        let page = |entries: &str| {
            format!(
                r#"<main id="content"><div class="title-container"><h2>Chamado 7</h2></div>
                <div data-tab="linha_tempo"><ul class="timeline">{entries}</ul></div></main>"#
            )
        };
        let entry = |date: &str, text: &str| {
            format!(
                r#"<li><div class="timeline-date">{date}</div><div class="timeline-content"><p>{text}</p></div></li>"#
            )
        };
        mount_text(
            &runtime,
            &server,
            "GET",
            "/centralservicos/chamado/7/",
            ResponseTemplate::new(200).set_body_string(page(&entry("10:00", "abri"))),
        );
        let (_, _, _) = run_args(&["watch", "--rodadas", "1", "--mensagens"], &paths);

        runtime.block_on(server.reset());
        mount_text(
            &runtime,
            &server,
            "GET",
            "/centralservicos/meus_chamados/",
            ResponseTemplate::new(200).set_body_string(listing),
        );
        let newest = format!(
            "{}{}",
            entry("11:00", "resposta nova"),
            entry("10:00", "abri")
        );
        mount_text(
            &runtime,
            &server,
            "GET",
            "/centralservicos/chamado/7/",
            ResponseTemplate::new(200).set_body_string(page(&newest)),
        );
        // Two rounds, one second apart: the second one has nothing new to say.
        let started = std::time::Instant::now();
        let (code, out, err) = run_args(
            &[
                "watch",
                "--rodadas",
                "2",
                "--intervalo",
                "1",
                "--mensagens",
                "--json",
            ],
            &paths,
        );
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(started.elapsed() >= std::time::Duration::from_secs(1));
        let event = json_of(&out);
        assert_eq!(event["event"], json!("message"));
        assert_eq!(event["text"], json!("resposta nova"));
        assert_eq!(out.lines().count(), 1, "{out}");

        // Out-of-range options are refused by clap.
        for bad in [
            &["watch", "--intervalo", "0"][..],
            &["watch", "--rodadas", "0"],
        ] {
            assert_eq!(run_args(bad, &paths).0, 2, "{bad:?}");
        }

        // Without a session the loop does not spin: it asks for a login.
        runtime.block_on(server.reset());
        mount_text(
            &runtime,
            &server,
            "GET",
            "/centralservicos/meus_chamados/",
            ResponseTemplate::new(302).insert_header("location", "/accounts/login/"),
        );
        mount_text(
            &runtime,
            &server,
            "GET",
            "/accounts/login/",
            ResponseTemplate::new(200),
        );
        let (code, _, err) = run_args(&["watch", "--rodadas", "3", "--intervalo", "1"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("chamados login"), "{err}");

        // A damaged state file is reported, not discarded.
        std::fs::write(paths.watch_file(), "isto nao e json").unwrap();
        let (code, _, err) = run_args(&["watch", "--rodadas", "1"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("watch state"), "{err}");
    }

    #[test]
    fn attach_sends_each_file_and_stops_at_the_first_problem() {
        let (dir, paths) = paths();
        let (runtime, server) = bare_server();
        run_args(&["profile", "init", "--base-url", &server.uri()], &paths);
        let form = r#"<form method="POST"><input type="hidden" name="csrfmiddlewaretoken" value="tok">
            <input name="descricao"><input type="file" name="anexo"></form>"#;
        for (verb, at, body, status) in [
            ("GET", "/centralservicos/adicionar_anexo/5/", form, 200),
            ("POST", "/centralservicos/adicionar_anexo/5/", "ok", 200),
            ("GET", "/centralservicos/adicionar_anexo/9/", "", 403),
        ] {
            mount_text(
                &runtime,
                &server,
                verb,
                at,
                ResponseTemplate::new(status).set_body_string(body),
            );
        }
        let csv = dir.path().join("dados.csv");
        let pdf = dir.path().join("relatório.pdf");
        std::fs::write(&csv, "a,b\n").unwrap();
        std::fs::write(&pdf, "%PDF").unwrap();
        let (csv_text, pdf_text) = (csv.to_str().unwrap(), pdf.to_str().unwrap());

        let (code, out, err) = run_args(
            &[
                "attach",
                "5",
                csv_text,
                pdf_text,
                "--descricao",
                "Anexos do teste",
            ],
            &paths,
        );
        assert_eq!((code, err.as_str()), (0, ""));
        assert_eq!(
            out,
            "Chamado #5: anexo dados.csv enviado.\nChamado #5: anexo relatório.pdf enviado.\n"
        );
        let bodies = posts_to(&runtime, &server, "/centralservicos/adicionar_anexo/5/");
        assert_eq!(bodies.len(), 2);
        assert!(bodies.iter().all(|body| body.contains("Anexos do teste")));

        // Nothing is sent when a file cannot be read or has a type SUAP refuses.
        let missing = dir.path().join("nao-existe.pdf");
        let (code, _, err) = run_args(
            &["attach", "5", csv_text, missing.to_str().unwrap()],
            &paths,
        );
        assert_eq!(code, 1);
        assert!(err.contains("não foi possível ler o anexo"), "{err}");
        let exe = dir.path().join("programa.exe");
        std::fs::write(&exe, "MZ").unwrap();
        let (code, _, err) = run_args(&["attach", "5", exe.to_str().unwrap()], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("unsupported type"), "{err}");
        assert_eq!(
            posts_to(&runtime, &server, "/centralservicos/adicionar_anexo/5/").len(),
            2
        );

        let (code, _, err) = run_args(&["attach", "9", csv_text], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("ticket 9 cannot be attached"), "{err}");
        assert_eq!(run_args(&["attach", "5"], &paths).0, 2);
    }

    #[test]
    fn one_comment_or_note_can_go_to_several_tickets_and_failures_do_not_stop_the_rest() {
        let (_dir, paths) = paths();
        let (runtime, server) = bare_server();
        run_args(&["profile", "init", "--base-url", &server.uri()], &paths);
        for id in ["5", "6", "8"] {
            let page = THREAD_PAGE.replace("/5/", &format!("/{id}/"));
            mount_text(
                &runtime,
                &server,
                "GET",
                &format!("/centralservicos/chamado/{id}/"),
                ResponseTemplate::new(200).set_body_string(page),
            );
            for action in ["adicionar_comentario", "adicionar_nota_interna"] {
                mount_text(
                    &runtime,
                    &server,
                    "POST",
                    &format!("/centralservicos/{action}/{id}/"),
                    ResponseTemplate::new(200),
                );
            }
        }
        // #7 is not there, and #9 refuses.
        mount_text(
            &runtime,
            &server,
            "GET",
            "/centralservicos/chamado/9/",
            ResponseTemplate::new(200).set_body_string("<p>sem formulário</p>"),
        );

        let (code, out, err) = run_args(&["comment", "5", "6", "-m", "aviso\npara todos"], &paths);
        assert_eq!((code, err.as_str()), (0, ""));
        assert_eq!(
            out,
            "Comentário adicionado ao chamado #5.\nComentário adicionado ao chamado #6.\nResumo: 2 de 2 enviados.\n"
        );
        for id in ["5", "6"] {
            let bodies = posts_to(
                &runtime,
                &server,
                &format!("/centralservicos/adicionar_comentario/{id}/"),
            );
            assert_eq!(bodies.len(), 1);
            let carries_text = bodies[0].contains("texto=aviso%0Apara+todos");
            assert!(carries_text);
        }
        // The text is read once, from standard input too; repeated numbers count once.
        let (code, out, _) =
            run_with_input(&["note", "5", "5", "8", "-m", "-"], &paths, "pelo stdin\n");
        assert_eq!(code, 0);
        assert_eq!(
            out,
            "Nota interna adicionada ao chamado #5.\nNota interna adicionada ao chamado #8.\nResumo: 2 de 2 enviados.\n"
        );

        // Failures are listed at the end, the others still get the text, and the command fails.
        let (code, out, err) = run_args(&["comment", "5", "7", "9", "8", "-m", "x"], &paths);
        assert_eq!(code, 1);
        assert_eq!(
            out,
            "Comentário adicionado ao chamado #5.\nComentário adicionado ao chamado #8.\nResumo: 2 de 4 enviados.\n"
        );
        assert!(err.contains("falhou em 2 de 4 chamados:"), "{err}");
        assert!(err.contains("  #7: ") && err.contains("404"), "{err}");
        assert!(
            err.contains("  #9: ") && err.contains("no comment form"),
            "{err}"
        );
        assert_eq!(
            posts_to(
                &runtime,
                &server,
                "/centralservicos/adicionar_comentario/8/"
            )
            .len(),
            1
        );
        // A single ticket keeps its plain error, without the summary.
        let (code, out, err) = run_args(&["comment", "7", "-m", "x"], &paths);
        assert_eq!((code, out.as_str()), (1, ""));
        assert!(
            err.starts_with("erro: ") && !err.contains("falhou em"),
            "{err}"
        );

        // The text is checked before any ticket is touched; at least one ticket is required.
        let before = runtime.block_on(server.received_requests()).unwrap().len();
        let (code, _, err) = run_with_input(&["comment", "5", "6"], &paths, " \n");
        assert_eq!(code, 1);
        assert!(err.contains("texto não informado"), "{err}");
        assert_eq!(
            runtime.block_on(server.received_requests()).unwrap().len(),
            before
        );
        assert_eq!(run_args(&["comment", "-m", "x"], &paths).0, 2);
        assert_eq!(run_args(&["note", "5", "x", "-m", "x"], &paths).0, 2);
    }

    #[test]
    fn a_missing_session_stops_a_batch_at_the_first_ticket() {
        let (_dir, paths) = paths();
        let (runtime, server) = bare_server();
        run_args(&["profile", "init", "--base-url", &server.uri()], &paths);
        mount_text(
            &runtime,
            &server,
            "GET",
            "/centralservicos/chamado/5/",
            ResponseTemplate::new(302).insert_header("location", "/accounts/login/"),
        );
        mount_text(
            &runtime,
            &server,
            "GET",
            "/accounts/login/",
            ResponseTemplate::new(200),
        );
        let (code, out, err) = run_args(&["comment", "5", "6", "7", "-m", "x"], &paths);
        assert_eq!((code, out.as_str()), (1, ""));
        assert!(
            err.contains("chamados login") && !err.contains("falhou em"),
            "{err}"
        );
        let requests = runtime.block_on(server.received_requests()).unwrap();
        let tickets = requests
            .iter()
            .filter(|r| r.url.path().starts_with("/centralservicos/chamado/"))
            .count();
        assert_eq!(tickets, 1, "the other tickets were not tried");
    }

    #[test]
    fn reopen_and_close_report_what_they_did() {
        let (_dir, paths, runtime, server) = text_requirement_setup();
        let (code, out, err) = run_args(&["reopen", "5", "-m", "ainda falha"], &paths);
        assert_eq!(
            (code, err.as_str(), out.as_str()),
            (0, "", "Chamado #5 reaberto.\n")
        );
        let (code, out, err) = run_args(&["close", "5"], &paths);
        assert_eq!(
            (code, err.as_str(), out.as_str()),
            (0, "", "Chamado #5 fechado.\n")
        );
        let (code, out, _) = run_args(&["close", "5", "--nota", "5", "-m", "obrigado"], &paths);
        assert_eq!((code, out.as_str()), (0, "Chamado #5 fechado.\n"));
        let posts = posts_to(&runtime, &server, "/centralservicos/fechar_chamado/5/");
        assert!(!posts[0].contains("nota_avaliacao"), "{}", posts[0]);
        assert!(posts[1].contains("nota_avaliacao=5") && posts[1].contains("comentario=obrigado"));

        // The rating is checked by clap (1 to 5); SUAP's refusals are shown as they are.
        for bad in ["0", "6", "x"] {
            let (code, _, err) = run_args(&["close", "5", "--nota", bad], &paths);
            assert_eq!(code, 2, "{bad}");
            assert!(err.contains("--nota"), "{err}");
        }
        let (code, _, err) = run_args(&["close", "9"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("ticket 9 cannot be closed"), "{err}");
        let (code, _, err) = run_args(&["reopen", "9", "-m", "x"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("ticket 9 cannot be reopened"), "{err}");
    }

    #[test]
    fn list_can_show_the_tickets_the_user_may_close() {
        let (_dir, paths) = paths();
        let (runtime, server) = mock_server(true);
        run_args(&["profile", "init", "--base-url", &server.uri()], &paths);
        let html = r#"<div class="general-box"><span class="status">Resolvido</span>
            <h4><a href="/centralservicos/chamado/7/">REQ #7 <strong>Pronto</strong></a></h4></div>"#;
        mount_listing(
            &runtime,
            &server,
            "/centralservicos/listar_chamados_a_fechar/",
            ResponseTemplate::new(200).set_body_string(html),
        );
        let (code, out, err) = run_args(&["list", "--a-fechar"], &paths);
        assert_eq!((code, err.as_str()), (0, ""));
        assert_eq!(out, "#7\tResolvido\t-\tPronto\n");
        let (code, _, err) = run_args(&["list", "--a-fechar", "--todas-paginas"], &paths);
        assert_eq!((code, err.as_str()), (0, ""));
        let (code, _, err) = run_args(&["list", "--a-fechar", "--busca", "x"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("only paging"), "{err}");
        let (code, _, err) = run_args(&["list", "--a-fechar", "--meus"], &paths);
        assert_eq!(code, 2);
        assert!(err.contains("cannot be used"), "{err}");
    }

    /// Configures `paths` to sync to `cloud` with the key in `key_file`.
    fn setup_sync(paths: &AppPaths, cloud: &std::path::Path, key_file: &std::path::Path) {
        let args = [
            "sync",
            "setup",
            "--path",
            cloud.to_str().unwrap(),
            "--key-source",
            "file",
            "--key-file",
            key_file.to_str().unwrap(),
        ];
        let (code, _, err) = run_args(&args, paths);
        assert_eq!((code, err.as_str()), (0, ""));
    }

    #[test]
    fn two_machines_sync_titles_and_profiles_through_an_encrypted_folder() {
        let root = tempdir().unwrap();
        let cloud = root.path().join("nuvem");
        let (_dir_a, a) = paths();
        let (_dir_b, b) = paths();
        setup_sync(&a, &cloud, &root.path().join("a.key"));
        setup_sync(&b, &cloud, &root.path().join("b.key"));

        // Machine A creates the key and a synced profile with a title.
        let (code, out, _) = run_args(&["sync", "key", "generate"], &a);
        assert_eq!((code, out.as_str()), (0, "Chave criada (file).\n"));
        let (code, _, _) = run_args(
            &["profile", "init", "--username", "ana", "--sync", "true"],
            &a,
        );
        assert_eq!(code, 0);
        run_args(&["title", "5", "Moodle 5.3"], &a);
        let (code, out, err) = run_args(&["sync"], &a);
        assert_eq!((code, err.as_str()), (0, ""));
        assert_eq!(out, "Sincronização concluída: 1 perfil(is), 0 alteração(ões) local(is), nuvem atualizada.\n");
        let stored = std::fs::read(cloud.join("chamados-sync-v1.bin")).unwrap();
        assert!(!String::from_utf8_lossy(&stored).contains("Moodle"));

        // Machine B gets the key (never through the cloud), then the profile and the title.
        let (_, key, _) = run_args(&["sync", "key", "export"], &a);
        assert_eq!(key.trim().len(), 64);
        let (code, out, _) = run_with_input(&["sync", "key", "import"], &b, &key);
        assert_eq!((code, out.as_str()), (0, "Chave importada (file).\n"));
        let (code, out, _) = run_args(&["sync"], &b);
        assert_eq!(code, 0);
        assert!(
            out.contains("2 alteração(ões) local(is), nuvem já estava em dia"),
            "{out}"
        );
        let (_, shown, _) = run_args(&["profile", "show"], &b);
        assert!(shown.contains("username: ana") && shown.contains("sync: ligado"));
        let (_, title, _) = run_args(&["title", "5"], &b);
        assert_eq!(title, "Moodle 5.3\n");

        // B changes the title; A receives it.
        run_args(&["title", "5", "Moodle 5.3 (feito)"], &b);
        run_args(&["sync", "--quiet"], &b);
        let (_, out, _) = run_args(&["sync"], &a);
        assert!(out.contains("1 alteração(ões) local(is)"), "{out}");
        let (_, title, _) = run_args(&["title", "5"], &a);
        assert_eq!(title, "Moodle 5.3 (feito)\n");

        // Removing the profile on A removes it on B too, and it does not come back from the cloud.
        assert_eq!(
            run_args(&["profile", "remove", "default", "--yes"], &a).0,
            0
        );
        assert_eq!(run_args(&["sync", "--quiet"], &a).0, 0);
        let (_, out, _) = run_args(&["sync"], &b);
        assert!(out.contains("1 alteração(ões) local(is)"), "{out}");
        let unsaved = "perfil padrão ainda não gravado";
        assert!(run_args(&["profile", "show"], &b).1.contains(unsaved));
        assert_eq!(run_args(&["sync", "--quiet"], &a).0, 0);
        assert!(run_args(&["profile", "show"], &a).1.contains(unsaved));
    }

    #[test]
    fn sync_flags_check_quiet_dry_run_only_and_lock() {
        let root = tempdir().unwrap();
        let cloud = root.path().join("nuvem");
        let (_dir, paths) = paths();
        setup_sync(&paths, &cloud, &root.path().join("k"));
        run_args(&["sync", "key", "generate"], &paths);
        run_args(&["profile", "init", "--sync", "true"], &paths);
        run_args(&["profile", "init", "local", "--sync", "true"], &paths);

        let (code, out, _) = run_args(&["sync", "--check"], &paths);
        assert_eq!(code, 0);
        assert!(
            out.contains("backend: directory") && out.contains("escrita condicional: suportada")
        );
        assert!(out.contains("chave: file (presente)"));

        let (code, out, _) = run_args(&["sync", "--dry-run"], &paths);
        assert_eq!(code, 0);
        assert!(
            out.starts_with("Simulação: 2 perfil(is)") && out.contains("nuvem seria atualizada"),
            "{out}"
        );
        assert!(!cloud.exists(), "a dry run writes nothing");

        let (code, out, _) = run_args(&["sync", "--only", "local", "--quiet"], &paths);
        assert_eq!((code, out.as_str()), (0, ""));
        let (_, out, _) = run_args(&["sync", "--dry-run"], &paths);
        assert!(
            out.contains("nuvem seria atualizada"),
            "default is still pending: {out}"
        );

        // A run that finds the lock taken does nothing, quietly or not.
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.sync_lock_file(), "").unwrap();
        let (code, out, _) = run_args(&["sync"], &paths);
        assert!(code == 0 && out.contains("Outra sincronização já está em andamento"));
        let (code, out, _) = run_args(&["sync", "--quiet"], &paths);
        assert_eq!((code, out.as_str()), (0, ""));
        std::fs::remove_file(paths.sync_lock_file()).unwrap();

        let (_, out, _) = run_args(&["sync"], &paths);
        assert!(out.contains("nuvem atualizada"));
        let (_, out, _) = run_args(&["sync"], &paths);
        assert!(out.contains("nuvem já estava em dia"));
    }

    #[test]
    fn sync_reports_missing_setup_keys_and_bad_settings() {
        let root = tempdir().unwrap();
        let (_dir, paths) = paths();
        let (code, _, err) = run_args(&["sync"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("sincronização não configurada"));

        let bad = [
            (vec!["--backend", "ftp", "--path", "/x"], "unknown backend"),
            (vec![], "needs a path"),
            (
                vec!["--path", "/x", "--key-source", "nuvem"],
                "unknown key source",
            ),
        ];
        for (extra, expected) in bad {
            let mut args = vec!["sync", "setup"];
            args.extend(extra);
            let (code, _, err) = run_args(&args, &paths);
            assert_eq!(code, 1, "{expected}");
            assert!(err.contains(expected), "{expected}: {err}");
        }
        assert!(!paths.config_file().exists(), "a bad setup saves nothing");

        let cloud = root.path().join("nuvem");
        let key_file = root.path().join("k");
        setup_sync(&paths, &cloud, &key_file);
        let (code, _, err) = run_args(&["sync"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("nenhuma chave de criptografia"));
        let (code, _, err) = run_args(&["sync", "key", "export"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("sync key generate"));
        let (_, out, _) = run_args(&["sync", "key", "status"], &paths);
        assert_eq!(out, "fonte: file\nchave presente: não\n");

        run_args(&["sync", "key", "generate"], &paths);
        let (_, out, _) = run_args(&["sync", "key", "status"], &paths);
        assert!(out.contains("chave presente: sim"));
        let (code, _, err) = run_args(&["sync", "key", "generate"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("already exists"));
        assert_eq!(
            run_args(&["sync", "key", "generate", "--force"], &paths).0,
            0
        );

        let (code, _, err) = run_with_input(&["sync", "key", "import"], &paths, "curta");
        assert_eq!(code, 1);
        assert!(err.contains("hexadecimal"));
        let (code, _, err) = run_with_input(&["sync", "key", "import"], &paths, "\n");
        assert_eq!(code, 1);
        assert!(err.contains("texto não informado"));

        // The environment key source is read only.
        run_args(
            &[
                "sync",
                "setup",
                "--path",
                cloud.to_str().unwrap(),
                "--key-source",
                "env",
            ],
            &paths,
        );
        let (code, _, err) = run_args(&["sync", "key", "generate"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("read only"));
        let (_, out, _) = run_args(&["sync", "key", "status"], &paths);
        assert!(out.starts_with("fonte: env"));
    }

    trait OwnedStrings {
        fn concat_owned(self) -> Vec<String>;
    }

    impl OwnedStrings for Vec<&str> {
        fn concat_owned(self) -> Vec<String> {
            self.into_iter().map(str::to_owned).collect()
        }
    }

    /// Serializes the tests that replace the process-wide default keyring store.
    static KEYRING: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn use_mock_keyring() -> std::sync::MutexGuard<'static, ()> {
        let guard = KEYRING.lock().unwrap();
        keyring_core::set_default_store(keyring_core::mock::Store::new().unwrap());
        guard
    }

    #[test]
    fn the_key_can_live_in_the_system_keyring() {
        let _guard = use_mock_keyring();
        let root = tempdir().unwrap();
        let (_dir, paths) = paths();
        let args = ["sync", "setup", "--path", root.path().to_str().unwrap()];
        assert_eq!(run_args(&args, &paths).0, 0);
        let (_, out, _) = run_args(&["sync", "key", "status"], &paths);
        assert_eq!(out, "fonte: keyring\nchave presente: não\n");
        assert_eq!(run_args(&["sync", "key", "generate"], &paths).0, 0);
        let (_, key, _) = run_args(&["sync", "key", "export"], &paths);
        assert_eq!(key.trim().len(), 64);
        let (code, _, _) = run_args(&["sync", "key", "generate"], &paths);
        assert_eq!(code, 1);
        assert_eq!(
            run_with_input(&["sync", "key", "import", "--force"], &paths, &key).0,
            0
        );
    }

    fn s3_setup_args<'a>(endpoint: &'a str, key_file: &'a str) -> Vec<&'a str> {
        vec![
            "sync",
            "setup",
            "--backend",
            "s3",
            "--endpoint",
            endpoint,
            "--bucket",
            "cofre",
            "--prefix",
            "dados",
            "--key-source",
            "file",
            "--key-file",
            key_file,
        ]
    }

    #[test]
    fn s3_credentials_are_stored_in_the_keyring_and_never_shown() {
        let _guard = use_mock_keyring();
        let (_dir, paths) = paths();
        let (_, out, _) = run_args(&["sync", "credentials", "status"], &paths);
        assert_eq!(out, "credenciais presentes: não\n");
        for bad in ["so-uma-linha", "a\nb\nc"] {
            let (code, _, err) = run_with_input(&["sync", "credentials", "set"], &paths, bad);
            assert_eq!(code, 1, "{bad:?}");
            assert!(err.contains("expected two lines"), "{err}");
        }
        // No input and no terminal: there is nobody to ask.
        let (code, _, err) = run_args(&["sync", "credentials", "set"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("sem terminal interativo"), "{err}");
        let (code, out, err) = run_with_input(
            &["sync", "credentials", "set"],
            &paths,
            "AKIAEXEMPLO\nsegredo-que-nao-aparece\n",
        );
        assert_eq!((code, err.as_str()), (0, ""));
        assert_eq!(out, "Credenciais guardadas no chaveiro.\n");
        assert!(!out.contains("segredo") && !out.contains("AKIA"));
        let (_, out, _) = run_args(&["sync", "credentials", "status"], &paths);
        assert_eq!(
            out,
            "credenciais presentes: sim (Access Key ID: 11 caracteres; Secret: 23 caracteres)\n"
        );
    }

    /// A terminal that answers from a script and remembers what it was asked.
    struct ScriptedTerminal {
        id: String,
        secret: Option<String>,
        asked: Vec<String>,
    }

    impl Prompt for ScriptedTerminal {
        fn line(&mut self, label: &str) -> std::io::Result<String> {
            self.asked.push(label.to_owned());
            Ok(self.id.clone())
        }

        fn secret(&mut self, label: &str) -> std::io::Result<String> {
            self.asked.push(label.to_owned());
            self.secret
                .clone()
                .ok_or_else(|| std::io::Error::other("sem resposta"))
        }
    }

    #[test]
    fn credentials_are_asked_for_on_the_terminal_when_nothing_is_piped() {
        let _guard = use_mock_keyring();
        let (_dir, paths) = paths();
        let mut terminal = ScriptedTerminal {
            id: "AKIAINTERATIVO".to_owned(),
            secret: Some("segredo-digitado".to_owned()),
            asked: Vec::new(),
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(
            ["chamados", "sync", "credentials", "set"],
            &paths,
            None,
            &mut std::io::empty(),
            &mut terminal,
            &mut out,
            &mut err,
        );
        let (out, err) = (
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        );
        assert_eq!(
            (code, err.as_str(), out.as_str()),
            (0, "", "Credenciais guardadas no chaveiro.\n")
        );
        assert_eq!(
            terminal.asked,
            [
                "Access Key ID: ",
                "Secret Access Key (não aparece na tela): "
            ]
        );
        assert!(!out.contains("segredo-digitado"));
        let (_, status, _) = run_args(&["sync", "credentials", "status"], &paths);
        assert!(
            status.starts_with("credenciais presentes: sim ("),
            "{status}"
        );
        assert!(!status.contains("segredo-digitado"), "{status}");

        // Cancelled or failing prompts are reported, and nothing is stored.
        terminal.secret = None;
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(
            ["chamados", "sync", "credentials", "set"],
            &paths,
            None,
            &mut std::io::empty(),
            &mut terminal,
            &mut out,
            &mut err,
        );
        assert_eq!(code, 1);
        assert!(String::from_utf8(err)
            .unwrap()
            .contains("sem terminal interativo"));
    }

    #[test]
    fn without_a_terminal_every_question_fails() {
        assert!(NoPrompt.line("Pergunta: ").is_err());
        assert!(NoPrompt.secret("Segredo: ").is_err());
    }

    /// A terminal that answers each secret from a queue and remembers what it was asked.
    struct QueuedTerminal {
        answers: Vec<String>,
        asked: Vec<String>,
    }

    impl QueuedTerminal {
        fn new(answers: &[&str]) -> Self {
            Self {
                answers: answers.iter().map(|text| (*text).to_owned()).collect(),
                asked: Vec::new(),
            }
        }
    }

    impl Prompt for QueuedTerminal {
        fn line(&mut self, label: &str) -> std::io::Result<String> {
            self.asked.push(label.to_owned());
            Ok(String::new())
        }

        fn secret(&mut self, label: &str) -> std::io::Result<String> {
            self.asked.push(label.to_owned());
            match self.answers.is_empty() {
                true => Err(std::io::Error::other("sem resposta")),
                false => Ok(self.answers.remove(0)),
            }
        }
    }

    fn run_prompted(
        args: &[&str],
        paths: &AppPaths,
        prompt: &mut dyn Prompt,
    ) -> (i32, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(
            std::iter::once("chamados").chain(args.iter().copied()),
            paths,
            None,
            &mut std::io::empty(),
            prompt,
            &mut out,
            &mut err,
        );
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[test]
    fn a_key_file_protected_by_a_passphrase_is_unlocked_on_the_terminal() {
        let root = tempdir().unwrap();
        let cloud = root.path().join("nuvem");
        let key_file = root.path().join("protegida.key");
        let (_dir, paths) = paths();
        let setup = [
            "sync",
            "setup",
            "--path",
            cloud.to_str().unwrap(),
            "--key-source",
            "protected-file",
            "--key-file",
            key_file.to_str().unwrap(),
        ];
        assert_eq!(run_args(&setup, &paths).0, 0);

        // Nothing to unlock yet: reading never asks for a passphrase.
        let mut terminal = QueuedTerminal::new(&["nao deveria perguntar"]);
        let (code, _, err) = run_prompted(&["sync", "key", "export"], &paths, &mut terminal);
        assert_eq!(code, 1);
        assert!(err.contains("nenhuma chave"), "{err}");
        assert!(terminal.asked.is_empty());

        // Creating one asks twice; without a terminal there is nobody to ask.
        let (code, _, err) = run_args(&["sync", "key", "generate"], &paths);
        assert_eq!(code, 1);
        assert!(
            err.contains("sem terminal") && err.contains(PASSPHRASE_ENV),
            "{err}"
        );
        let mut terminal = QueuedTerminal::new(&["uma", "outra"]);
        let (code, _, err) = run_prompted(&["sync", "key", "generate"], &paths, &mut terminal);
        assert_eq!(code, 1);
        assert!(err.contains("não conferem"), "{err}");
        assert!(!key_file.exists());
        let mut terminal = QueuedTerminal::new(&["frase boa", "frase boa"]);
        let (code, out, _) = run_prompted(&["sync", "key", "generate"], &paths, &mut terminal);
        assert_eq!(
            (code, out.as_str()),
            (0, "Chave criada (protected-file).\n")
        );
        assert_eq!(
            terminal.asked,
            ["Frase-senha da chave: ", "Repita a frase-senha: "]
        );
        assert!(!std::fs::read_to_string(&key_file).unwrap().is_empty());

        // The status needs no passphrase; exporting does, and the right one.
        let (_, out, _) = run_args(&["sync", "key", "status"], &paths);
        assert_eq!(out, "fonte: protected-file\nchave presente: sim\n");
        let mut terminal = QueuedTerminal::new(&["errada"]);
        let (code, _, err) = run_prompted(&["sync", "key", "export"], &paths, &mut terminal);
        assert_eq!(code, 1);
        assert!(err.contains("wrong passphrase"), "{err}");
        let mut terminal = QueuedTerminal::new(&["frase boa"]);
        let (code, hex, _) = run_prompted(&["sync", "key", "export"], &paths, &mut terminal);
        assert_eq!((code, hex.trim().len()), (0, 64));

        // `sync` unlocks the key the same way.
        assert_eq!(
            run_args(
                &["profile", "init", "--username", "ana", "--sync", "true"],
                &paths
            )
            .0,
            0
        );
        let (code, _, err) = run_args(&["sync"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("sem terminal"), "{err}");
        let mut terminal = QueuedTerminal::new(&["frase boa"]);
        let (code, out, err) = run_prompted(&["sync"], &paths, &mut terminal);
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(out.contains("nuvem atualizada"), "{out}");
        assert_eq!(terminal.line("?").unwrap(), "");
    }

    #[test]
    fn the_passphrase_may_come_from_the_environment() {
        let source = KeySource::parse("protected-file", Some(PathBuf::from("k"))).unwrap();
        let with = |value: Option<&'static str>| {
            move |name: &str| {
                assert_eq!(name, PASSPHRASE_ENV);
                value.map(str::to_owned)
            }
        };
        let mut terminal = QueuedTerminal::new(&["do terminal"]);
        let unlocked = unlock_with(
            source.clone(),
            &mut terminal,
            true,
            &with(Some("do ambiente")),
        );
        assert!(!unlocked.unwrap().needs_passphrase());
        assert!(terminal.asked.is_empty());
        // An empty variable is ignored and the terminal is asked.
        let asked = unlock_with(source, &mut terminal, false, &with(Some("")));
        // (the key file does not exist, so reading it needs nothing)
        assert!(asked.unwrap().needs_passphrase());
        assert_eq!(terminal.secret("a").unwrap(), "do terminal");
        assert!(terminal.secret("b").is_err());
        // Other key sources never need one.
        let plain = unlock_with(KeySource::Keyring, &mut terminal, true, &with(None));
        assert_eq!(plain.unwrap(), KeySource::Keyring);
    }

    #[test]
    fn auto_sync_sends_each_local_change_and_never_fails_the_command() {
        let root = tempdir().unwrap();
        let cloud = root.path().join("nuvem");
        let object = cloud.join("chamados-sync-v1.bin");
        let (_dir_a, a) = paths();
        let (_dir_b, b) = paths();
        setup_sync(&a, &cloud, &root.path().join("a.key"));
        setup_sync(&b, &cloud, &root.path().join("b.key"));
        assert_eq!(run_args(&["sync", "key", "generate"], &a).0, 0);
        let (_, key, _) = run_args(&["sync", "key", "export"], &a);
        assert_eq!(run_with_input(&["sync", "key", "import"], &b, &key).0, 0);

        // Off by default: nothing leaves the machine by itself.
        run_args(
            &["profile", "init", "--username", "ana", "--sync", "true"],
            &a,
        );
        run_args(&["title", "5", "Primeiro"], &a);
        assert!(!object.exists());

        // On, with nothing to synchronize yet (a private profile only): still nothing.
        assert_eq!(run_args(&["sync", "setup", "--auto", "true"], &b).0, 0);
        let (code, out, _) = run_args(&["profile", "init", "privado"], &b);
        assert_eq!((code, out.contains("aviso")), (0, false));
        assert!(!object.exists());

        // On, with a synchronized profile: every change goes up right away.
        assert_eq!(run_args(&["sync", "setup", "--auto", "true"], &a).0, 0);
        let (code, out, err) = run_args(&["title", "5", "Segundo"], &a);
        assert_eq!(
            (code, out.as_str(), err.as_str()),
            (0, "Título local do chamado #5 salvo.\n", "")
        );
        assert!(object.exists());
        assert_eq!(run_args(&["sync", "--quiet"], &b).0, 0);
        assert_eq!(run_args(&["title", "5"], &b).1, "Segundo\n");
        // Reading a title is not a change.
        let before = std::fs::read(&object).unwrap();
        run_args(&["title", "5"], &a);
        assert_eq!(std::fs::read(&object).unwrap(), before);

        // Removing a synchronized profile is announced too, even though the profile is gone.
        assert_eq!(
            run_args(&["profile", "remove", "default", "--yes"], &a).0,
            0
        );
        assert_eq!(run_args(&["sync", "--quiet"], &b).0, 0);
        let shown = run_args(&["profile", "show"], &b).1;
        assert!(shown.contains("perfil padrão ainda não gravado"), "{shown}");

        // A failure is only reported; the command itself succeeded.
        std::fs::remove_file(root.path().join("a.key")).unwrap();
        let (code, out, err) = run_args(&["profile", "init", "--sync", "true"], &a);
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(
            out.contains("aviso: a sincronização automática falhou"),
            "{out}"
        );
        assert!(out.contains("nenhuma chave"), "{out}");

        // Turned off again.
        assert_eq!(run_args(&["sync", "setup", "--auto", "false"], &a).0, 0);
        let (_, out, _) = run_args(&["title", "6", "Terceiro"], &a);
        assert!(!out.contains("aviso"), "{out}");
    }

    #[test]
    fn only_changes_to_synced_data_trigger_the_automatic_sync() {
        let command = |args: &[&str]| {
            let argv = std::iter::once("chamados").chain(args.iter().copied());
            Cli::try_parse_from(argv).unwrap().command
        };
        for changes in [
            &["title", "5", "x"][..],
            &["title", "5", "--remove"],
            &["profile", "init"],
            &["profile", "update", "--sync", "true"],
            &["profile", "remove", "p", "--yes"],
        ] {
            assert!(changes_synced_data(&command(changes)), "{changes:?}");
        }
        for reads in [
            &["title", "5"][..],
            &["profile", "list"],
            &["profile", "show"],
            &["list"],
            &["status"],
        ] {
            assert!(!changes_synced_data(&command(reads)), "{reads:?}");
        }
        assert!(!changes_synced_data(&None));
    }

    #[test]
    fn the_scheduler_templates_are_printed_or_installed() {
        let (dir, paths) = paths();
        let cron = run_args(&["sync", "automation", "--platform", "cron"], &paths).1;
        assert!(
            cron.contains("*/5 * * * *") && cron.contains("sync --quiet"),
            "{cron}"
        );
        assert!(cron.contains("Dica:"), "{cron}");
        let windows = run_args(
            &[
                "sync",
                "automation",
                "--platform",
                "windows",
                "--interval",
                "15",
            ],
            &paths,
        )
        .1;
        assert!(
            windows.contains("schtasks /Create /SC MINUTE /MO 15"),
            "{windows}"
        );
        let systemd = run_args(&["sync", "automation", "--platform", "systemd"], &paths).1;
        for expected in [
            "[Timer]",
            "OnUnitActiveSec=5min",
            "ExecStart=",
            "systemctl --user enable --now chamados-sync.timer",
        ] {
            assert!(systemd.contains(expected), "{expected}: {systemd}");
        }
        // Without a platform, the one of this system is used.
        let default = run_args(&["sync", "automation"], &paths);
        assert_eq!(default.0, 0);
        assert!(default.1.contains("sync --quiet"));

        for interval in ["0", "60"] {
            let (code, _, err) = run_args(&["sync", "automation", "--interval", interval], &paths);
            assert_eq!(code, 1);
            assert!(err.contains("entre 1 e 59"), "{err}");
        }
        let (code, _, err) = run_args(
            &["sync", "automation", "--platform", "cron", "--install"],
            &paths,
        );
        assert_eq!(code, 1);
        assert!(err.contains("só existe para o systemd"), "{err}");

        let (code, out, err) = run_args(
            &[
                "sync",
                "automation",
                "--platform",
                "systemd",
                "--install",
                "--interval",
                "10",
            ],
            &paths,
        );
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(out.contains("Arquivos gravados em"), "{out}");
        let units = dir.path().join("systemd").join("user");
        let timer = std::fs::read_to_string(units.join("chamados-sync.timer")).unwrap();
        assert!(timer.contains("OnUnitActiveSec=10min"));
        let service = std::fs::read_to_string(units.join("chamados-sync.service")).unwrap();
        assert!(service.contains("sync --quiet"));
    }

    #[test]
    fn s3_setup_is_validated_before_it_is_saved() {
        let (_dir, paths) = paths();
        let cases = [
            (
                vec!["--backend", "s3", "--bucket", "b"],
                "needs an endpoint",
            ),
            (
                vec!["--backend", "s3", "--endpoint", "https://x.example"],
                "needs a bucket",
            ),
            (
                vec![
                    "--backend",
                    "s3",
                    "--endpoint",
                    "http://x.example",
                    "--bucket",
                    "b",
                ],
                "must use https",
            ),
            (
                vec![
                    "--backend",
                    "s3",
                    "--endpoint",
                    "https://x.example",
                    "--bucket",
                    "b",
                    "--prefix",
                    "../x",
                ],
                "invalid prefix",
            ),
        ];
        for (extra, expected) in cases {
            let mut args = vec!["sync", "setup"];
            args.extend(extra);
            args.extend(["--key-source", "env"]);
            let (code, _, err) = run_args(&args, &paths);
            assert_eq!(code, 1, "{expected}");
            assert!(err.contains(expected), "{expected}: {err}");
        }
        assert!(!paths.config_file().exists());
        let good = [
            "sync",
            "setup",
            "--backend",
            "s3",
            "--endpoint",
            "https://conta.r2.cloudflarestorage.com",
            "--bucket",
            "cofre",
            "--region",
            "auto",
            "--conditional-writes",
            "true",
            "--key-source",
            "env",
        ];
        assert_eq!(run_args(&good, &paths).0, 0);
        let saved = std::fs::read_to_string(paths.config_file()).unwrap();
        assert!(saved.contains("backend = \"s3\"") && saved.contains("bucket = \"cofre\""));
        assert!(saved.contains("endpoint = \"https://conta.r2.cloudflarestorage.com\""));
        // Credentials and keys never reach the configuration file.
        assert!(!saved.to_lowercase().contains("secret") && !saved.contains("AKIA"));
    }

    #[test]
    fn sync_through_an_s3_bucket_sends_only_signed_encrypted_requests() {
        let _guard = use_mock_keyring();
        let root = tempdir().unwrap();
        let (_dir, paths) = paths();
        let (runtime, server) = bare_server();
        let key_file = root.path().join("k");
        let args = s3_setup_args(&server.uri(), key_file.to_str().unwrap()).concat_owned();
        assert_eq!(
            run_args(&args.iter().map(String::as_str).collect::<Vec<_>>(), &paths).0,
            0
        );
        run_with_input(
            &["sync", "credentials", "set"],
            &paths,
            "AKIAEXEMPLO\nsegredo-que-nao-aparece\n",
        );
        run_args(&["sync", "key", "generate"], &paths);
        run_args(
            &[
                "profile",
                "init",
                "--sync",
                "true",
                "--username",
                "ana-secreta",
            ],
            &paths,
        );
        run_args(&["title", "5", "Titulo secreto do chamado"], &paths);

        mount_text(
            &runtime,
            &server,
            "GET",
            "/cofre/dados/chamados-sync-v1.bin",
            ResponseTemplate::new(404),
        );
        mount_text(
            &runtime,
            &server,
            "PUT",
            "/cofre/dados/chamados-sync-v1.bin",
            ResponseTemplate::new(200),
        );
        let (code, out, err) = run_args(&["sync"], &paths);
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(out.contains("nuvem atualizada"), "{out}");

        let requests = runtime.block_on(server.received_requests()).unwrap();
        let put = requests
            .iter()
            .find(|request| request.method == wiremock::http::Method::PUT)
            .unwrap();
        assert!(put.headers["authorization"]
            .to_str()
            .unwrap()
            .starts_with("AWS4-HMAC-SHA256 Credential=AKIAEXEMPLO/"));
        assert!(
            put.headers.contains_key("if-none-match"),
            "a new object is created conditionally"
        );
        let sent = String::from_utf8_lossy(&put.body).into_owned();
        assert!(sent.starts_with("CSYN1"));
        for secret in ["Titulo secreto", "ana-secreta", "segredo-que-nao-aparece"] {
            assert!(
                !sent.contains(secret) && !format!("{:?}", put.headers).contains(secret),
                "{secret}"
            );
        }
    }

    #[test]
    fn sync_check_probes_the_bucket_for_conditional_writes() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let _guard = use_mock_keyring();
        let root = tempdir().unwrap();
        let key_file = root.path().join("k");
        for honors in [true, false] {
            let (_dir, paths) = paths();
            let (runtime, server) = bare_server();
            let args = s3_setup_args(&server.uri(), key_file.to_str().unwrap()).concat_owned();
            run_args(&args.iter().map(String::as_str).collect::<Vec<_>>(), &paths);
            run_with_input(
                &["sync", "credentials", "set"],
                &paths,
                "AKIAEXEMPLO\nsegredo\n",
            );
            run_args(&["sync", "key", "generate", "--force"], &paths);
            // A bucket that refuses the second `If-None-Match: *` write, or one that accepts everything.
            let puts = std::sync::Arc::new(AtomicUsize::new(0));
            let counter = puts.clone();
            runtime.block_on(
                Mock::given(method("PUT"))
                    .respond_with(move |_: &wiremock::Request| {
                        let n = counter.fetch_add(1, Ordering::SeqCst);
                        ResponseTemplate::new(if honors && n > 0 { 412 } else { 200 })
                    })
                    .mount(&server),
            );
            mount_text(
                &runtime,
                &server,
                "DELETE",
                "/cofre/dados/",
                ResponseTemplate::new(204),
            );
            runtime.block_on(
                Mock::given(method("DELETE"))
                    .respond_with(ResponseTemplate::new(204))
                    .mount(&server),
            );
            let (code, out, err) = run_args(&["sync", "--check"], &paths);
            assert_eq!((code, err.as_str()), (0, ""), "{honors}");
            assert!(out.contains("backend: s3"));
            if honors {
                assert!(
                    out.contains("escrita condicional: suportada") && !out.contains("aviso"),
                    "{out}"
                );
            } else {
                assert!(
                    out.contains("escrita condicional: não suportada")
                        && out.contains("--conditional-writes"),
                    "{out}"
                );
            }
            assert_eq!(puts.load(Ordering::SeqCst), 2, "the probe writes twice");
        }
    }

    #[test]
    fn nothing_is_sent_without_a_key_or_credentials() {
        let _guard = use_mock_keyring();
        let root = tempdir().unwrap();
        let (_dir, paths) = paths();
        let (runtime, server) = bare_server();
        let key_file = root.path().join("k");
        let args = s3_setup_args(&server.uri(), key_file.to_str().unwrap()).concat_owned();
        run_args(&args.iter().map(String::as_str).collect::<Vec<_>>(), &paths);
        run_args(&["profile", "init", "--sync", "true"], &paths);

        let (code, _, err) = run_args(&["sync"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("no S3 credentials"), "{err}");
        run_with_input(
            &["sync", "credentials", "set"],
            &paths,
            "AKIAEXEMPLO\nsegredo\n",
        );
        let (code, _, err) = run_args(&["sync"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("nenhuma chave de criptografia"), "{err}");
        assert!(
            runtime
                .block_on(server.received_requests())
                .unwrap()
                .is_empty(),
            "RS-03: no key, no request"
        );
    }

    #[test]
    fn sync_check_words_cover_both_answers() {
        assert_eq!(support_text(true), "suportada");
        assert_eq!(support_text(false), "não suportada");
    }

    #[test]
    fn profiles_track_when_they_changed_and_whether_they_sync() {
        let (_dir, paths) = paths();
        run_args(&["profile", "init"], &paths);
        let created = load_config(&paths).unwrap().unwrap();
        assert!(!created.sync && created.updated_at > 0);
        let (_, out, _) = run_args(&["profile", "show"], &paths);
        assert!(out.contains("sync: desligado"));

        let (code, _, _) = run_args(&["profile", "update", "--sync", "true"], &paths);
        assert_eq!(code, 0, "--sync alone is a change");
        assert!(load_config(&paths).unwrap().unwrap().sync);
        run_args(&["profile", "update", "--sync", "false"], &paths);
        assert!(!load_config(&paths).unwrap().unwrap().sync);
    }

    #[test]
    fn open_saves_the_local_title_and_validates_it_first() {
        let (_dir, paths, runtime, server) = open_setup();
        let (code, out, err) = run_args(&["open", "-d", "x", "--title", " Meu título "], &paths);
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(out.contains("Título local salvo: Meu título"));
        let (_, out, _) = run_args(&["title", "99"], &paths);
        assert_eq!(out, "Meu título\n");

        let before = runtime.block_on(server.received_requests()).unwrap().len();
        let (code, _, err) = run_args(&["open", "-d", "x", "-t", "a\nb"], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("single line"));
        let after = runtime.block_on(server.received_requests()).unwrap().len();
        assert_eq!(before, after, "no request may be made for an invalid title");
    }

    #[test]
    fn open_reports_a_title_that_could_not_be_saved_after_the_ticket_was_created() {
        let (_dir, paths, _runtime, _server) = open_setup();
        paths.ensure_dirs().unwrap();
        // The temporary file name is taken by a directory, so saving the titles fails.
        std::fs::create_dir(paths.titles_file().with_extension("json.tmp")).unwrap();
        let (code, out, err) = run_args(&["open", "-d", "x", "-t", "Titulo"], &paths);
        assert_eq!(code, 1);
        assert!(out.contains("Chamado #99 aberto"));
        assert!(
            err.contains("o chamado #99 foi aberto, mas não foi possível salvar o título local")
        );
    }

    #[test]
    fn show_prints_ticket_details() {
        let (_dir, paths) = paths();
        let (runtime, server) = mock_server(true);
        run_args(&["profile", "init", "--base-url", &server.uri()], &paths);
        let html = r#"<main id="content"><div class="title-container"><h2>Chamado Interno 7</h2>
            <div class="object-status"><span class="status">Aberto</span></div></div>
            <div class="accordion"><button class="accordion-button">Serviço | Assunto</button>
            <div class="accordion-body"><dl class="definition-list"><div class="list-item"><dt>Descrição</dt><dd>Texto</dd></div></dl></div></div>
            <div data-tab="linha_tempo"><ul class="timeline"><li><div class="timeline-date">01/01/2026 10:00:00</div>
            <div class="timeline-content">Chamado aberto</div></li></ul></div></main>"#;
        mount_listing(
            &runtime,
            &server,
            "/centralservicos/chamado/7/",
            ResponseTemplate::new(200).set_body_string(html),
        );
        let (code, out, err) = run_args(&["show", "7"], &paths);
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(out
            .starts_with("Chamado Interno 7\nSituação: Aberto\nServiço: Serviço | Assunto\nURL: "));
        assert!(out.contains(
            "Descrição: Texto\n\nLinha do tempo:\n  01/01/2026 10:00:00  Chamado aberto\n"
        ));
    }

    #[test]
    fn show_keeps_line_breaks_of_multiline_text() {
        let (_dir, paths) = paths();
        let (runtime, server) = bare_server();
        run_args(&["profile", "init", "--base-url", &server.uri()], &paths);
        let html = r#"<main id="content"><div class="title-container"><h2>Chamado Interno 9</h2></div>
            <div class="accordion"><div class="accordion-body"><dl class="definition-list"><div class="list-item">
            <dt>Descrição</dt><dd>linha 1<br>linha 2<br><br>depois</dd></div></dl></div></div>
            <div data-tab="linha_tempo"><ul class="timeline"><li><div class="timeline-date">01/01/2026 10:00:00</div>
            <div class="timeline-content">Fulano comentou:<p>a<br>b</p><p>c</p></div></li></ul></div></main>"#;
        mount_listing(
            &runtime,
            &server,
            "/centralservicos/chamado/9/",
            ResponseTemplate::new(200).set_body_string(html),
        );
        let (code, out, _) = run_args(&["show", "9"], &paths);
        assert_eq!(code, 0);
        assert!(out.contains("Descrição: linha 1\n    linha 2\n\n    depois\n"));
        assert!(out.contains("  01/01/2026 10:00:00  Fulano comentou: a\n      b\n\n      c\n"));
    }

    #[test]
    fn show_prints_placeholder_without_heading() {
        let (_dir, paths) = paths();
        let (runtime, server) = mock_server(true);
        run_args(&["profile", "init", "--base-url", &server.uri()], &paths);
        let html = r#"<main id="content"><div class="title-container"><h2>Chamado Interno 8</h2></div></main>"#;
        mount_listing(
            &runtime,
            &server,
            "/centralservicos/chamado/8/",
            ResponseTemplate::new(200).set_body_string(html),
        );
        let (code, out, _) = run_args(&["show", "8"], &paths);
        assert_eq!(code, 0);
        assert!(out.contains("Serviço: -"));
    }

    #[test]
    fn show_asks_for_login_and_rejects_bad_ids() {
        let (_dir, paths) = paths();
        let (runtime, server) = mock_server(true);
        run_args(&["profile", "init", "--base-url", &server.uri()], &paths);
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
            &mut NoPrompt,
            &mut out,
            &mut err,
        );
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    const OPEN_FORM: &str = r#"<form method="post"><textarea name="descricao"></textarea>
        <input type="checkbox" name="enviar_copia_email" checked></form>"#;

    /// Profile with `open` defaults pointing at a mock SUAP that accepts tickets for service 7.
    fn open_setup() -> (TempDir, AppPaths, tokio::runtime::Runtime, MockServer) {
        let (dir, paths) = paths();
        let (runtime, server) = bare_server();
        let uri = server.uri();
        let init = [
            "profile",
            "init",
            "--base-url",
            &uri,
            "--service",
            "7",
            "--interested",
            "1",
            "--campus",
            "2",
            "--center",
            "3",
        ];
        assert_eq!(run_args(&init, &paths).0, 0);
        let form = ResponseTemplate::new(200).set_body_string(OPEN_FORM);
        mount_text(
            &runtime,
            &server,
            "GET",
            "/centralservicos/abrir_chamado/7/",
            form,
        );
        let created = ResponseTemplate::new(200).set_body_string("Número do chamado: 99");
        mount_text(
            &runtime,
            &server,
            "POST",
            "/centralservicos/abrir_chamado/7/",
            created,
        );
        (dir, paths, runtime, server)
    }

    fn posted_body(runtime: &tokio::runtime::Runtime, server: &MockServer) -> String {
        let requests = runtime.block_on(server.received_requests()).unwrap();
        let post = requests
            .iter()
            .find(|request| request.method == wiremock::http::Method::POST)
            .unwrap();
        String::from_utf8_lossy(&post.body).into_owned()
    }

    #[test]
    fn open_uses_profile_defaults_multiline_text_and_email_copy() {
        let (_dir, paths, runtime, server) = open_setup();
        let (code, out, err) = run_args(&["open", "-d", "linha 1\nlinha 2\n"], &paths);
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(out.starts_with("Chamado #99 aberto: "));
        let body = posted_body(&runtime, &server);
        assert!(body.contains("descricao=linha+1%0Alinha+2&"));
        assert!(
            body.contains("interessado=1")
                && body.contains("uo=2")
                && body.contains("centro_atendimento=3")
        );
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
        run_args(&["profile", "init", "--base-url", &server.uri()], &paths);
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
        assert!(
            body.contains("name=\"chamadoanexo_set-0-anexo\"; filename=\"relatorio.pdf\"")
                && body.contains("%PDF conteudo")
        );

        let exe = dir.path().join("virus.exe");
        std::fs::write(&exe, b"MZ").unwrap();
        let (code, _, err) = run_args(&["open", "-d", "x", "-a", exe.to_str().unwrap()], &paths);
        assert_eq!(code, 1);
        assert!(err.contains("unsupported type"));

        let missing = dir.path().join("nao-existe.pdf");
        let (code, _, err) = run_args(
            &["open", "-d", "x", "-a", missing.to_str().unwrap()],
            &paths,
        );
        assert_eq!(code, 1);
        assert!(err.contains("não foi possível ler o anexo"));
    }

    #[test]
    fn open_can_assume_and_start_the_ticket() {
        let (_dir, paths, runtime, server) = open_setup();
        let ok = || ResponseTemplate::new(200);
        mount_text(
            &runtime,
            &server,
            "GET",
            "/centralservicos/auto_atribuir_chamado/99/",
            ok(),
        );
        mount_text(
            &runtime,
            &server,
            "GET",
            "/centralservicos/colocar_em_atendimento/99/",
            ok(),
        );

        let (code, out, _) = run_args(&["open", "-d", "x", "--assume"], &paths);
        assert!(code == 0 && out.contains("assumido.") && !out.contains("em atendimento."));
        let (code, out, _) = run_args(&["open", "-d", "x", "--start"], &paths);
        assert!(
            code == 0 && out.contains("assumido.") && out.contains("Chamado #99 em atendimento.")
        );
    }

    #[test]
    fn open_reports_failures_after_the_ticket_was_created() {
        let (_dir, paths, runtime, server) = open_setup();
        let refused =
            ResponseTemplate::new(200).set_body_string("<p class='alert-error'>Sem permissão</p>");
        mount_text(
            &runtime,
            &server,
            "GET",
            "/centralservicos/auto_atribuir_chamado/99/",
            refused,
        );
        let (code, out, err) = run_args(&["open", "-d", "x", "--assume"], &paths);
        assert_eq!(code, 1);
        assert!(
            out.contains("Chamado #99 aberto")
                && err.contains("o chamado #99 foi aberto, mas não foi possível assumi-lo")
        );
        assert!(err.contains("Sem permissão"));
    }

    #[test]
    fn open_reports_failure_when_starting_service() {
        let (_dir, paths, runtime, server) = open_setup();
        mount_text(
            &runtime,
            &server,
            "GET",
            "/centralservicos/auto_atribuir_chamado/99/",
            ResponseTemplate::new(200),
        );
        let (code, out, err) = run_args(&["open", "-d", "x", "--start"], &paths);
        assert_eq!(code, 1);
        assert!(out.contains("assumido.") && err.contains("colocá-lo em atendimento"));
    }

    #[test]
    fn profile_show_lists_open_defaults() {
        let (_dir, paths, _runtime, _server) = open_setup();
        let (_, out, _) = run_args(&["profile", "show"], &paths);
        for expected in [
            "open.service: 7",
            "open.interested: 1",
            "open.campus: 2",
            "open.center: 3",
        ] {
            assert!(out.contains(expected), "{expected}");
        }
        run_args(&["profile", "update", "--username", "mantem"], &paths);
        let (_, out, _) = run_args(&["profile", "show"], &paths);
        assert!(out.contains("open.service: 7") && out.contains("username: mantem"));
    }

    fn bare_server() -> (tokio::runtime::Runtime, MockServer) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let server = runtime.block_on(MockServer::start());
        (runtime, server)
    }

    fn mount_text(
        runtime: &tokio::runtime::Runtime,
        server: &MockServer,
        verb: &str,
        request_path: &str,
        body: ResponseTemplate,
    ) {
        runtime.block_on(
            Mock::given(method(verb))
                .and(path(request_path.to_owned()))
                .respond_with(body)
                .mount(server),
        );
    }

    #[test]
    fn open_creates_ticket_with_suap_defaults() {
        let (_dir, paths) = paths();
        let (runtime, server) = bare_server();
        run_args(&["profile", "init", "--base-url", &server.uri()], &paths);
        let form = r#"<form method="post"><input type="hidden" name="csrfmiddlewaretoken" value="tok">
            <textarea name="descricao"></textarea></form>"#;
        mount_text(
            &runtime,
            &server,
            "GET",
            "/centralservicos/abrir_chamado/7/",
            ResponseTemplate::new(200).set_body_string(form),
        );
        mount_text(
            &runtime,
            &server,
            "GET",
            "/centralservicos/get_campus_com_centros_atendimento/7/0/",
            ResponseTemplate::new(200).set_body_string(r#"{"campus": [[3, "ZL", true]]}"#),
        );
        mount_text(
            &runtime,
            &server,
            "GET",
            "/centralservicos/get_centros_atendimento_por_servico_e_campus/7/3/",
            ResponseTemplate::new(200).set_body_string(r#"{"centros": [[9, "TI", true]]}"#),
        );
        mount_text(
            &runtime,
            &server,
            "POST",
            "/centralservicos/abrir_chamado/7/",
            ResponseTemplate::new(302).insert_header("location", "/centralservicos/chamado/99/"),
        );
        mount_text(
            &runtime,
            &server,
            "GET",
            "/centralservicos/chamado/99/",
            ResponseTemplate::new(200),
        );
        let (code, out, err) = run_args(
            &[
                "open",
                "7",
                "--description",
                "Teste",
                "--interested",
                "1",
                "--field",
                "telefone=1=2",
            ],
            &paths,
        );
        assert_eq!((code, err.as_str()), (0, ""));
        assert_eq!(
            out,
            format!(
                "Chamado #99 aberto: {}/centralservicos/chamado/99/\n",
                server.uri()
            )
        );
    }

    #[test]
    fn open_reports_rejection_and_invalid_fields() {
        let (_dir, paths) = paths();
        let (runtime, server) = bare_server();
        run_args(&["profile", "init", "--base-url", &server.uri()], &paths);
        let form = r#"<form method="post"><textarea name="descricao"></textarea></form>"#;
        mount_text(
            &runtime,
            &server,
            "GET",
            "/centralservicos/abrir_chamado/7/",
            ResponseTemplate::new(200).set_body_string(form),
        );
        mount_text(
            &runtime,
            &server,
            "POST",
            "/centralservicos/abrir_chamado/7/",
            ResponseTemplate::new(200).set_body_string("<p>sem retorno</p>"),
        );
        let (code, _, err) = run_args(
            &[
                "open",
                "7",
                "-d",
                "Teste",
                "--campus",
                "1",
                "--center",
                "2",
                "--interested",
                "3",
            ],
            &paths,
        );
        assert_eq!(code, 1);
        assert!(err.contains("could not confirm"));

        for bad in ["semigual", "=semnome"] {
            let (code, _, err) = run_args(
                &["open", "7", "-d", "x", "--interested", "1", "--field", bad],
                &paths,
            );
            assert_eq!(code, 2);
            assert!(err.contains("NOME=VALOR"));
        }
    }

    #[test]
    fn list_reports_empty_result() {
        let (_dir, paths) = paths();
        let (runtime, server) = mock_server(true);
        run_args(&["profile", "init", "--base-url", &server.uri()], &paths);
        mount_listing(
            &runtime,
            &server,
            "/centralservicos/listar_chamados_suporte/",
            ResponseTemplate::new(200),
        );
        let (code, out, _) = run_args(&["list"], &paths);
        assert_eq!(code, 0);
        assert!(out.contains("Nenhum chamado"));
    }

    #[test]
    fn list_asks_for_login_when_session_is_missing() {
        let (_dir, paths) = paths();
        let (runtime, server) = mock_server(true);
        run_args(&["profile", "init", "--base-url", &server.uri()], &paths);
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
        run_args(&["profile", "init", "--base-url", &server.uri()], &paths);
        mount_listing(
            &runtime,
            &server,
            "/centralservicos/listar_chamados_suporte/",
            ResponseTemplate::new(500),
        );
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
        let code = run(
            ["chamados", "status"],
            &paths,
            None,
            &mut std::io::empty(),
            &mut NoPrompt,
            &mut FailingWriter,
            &mut err,
        );
        assert_eq!(code, 1);
        assert!(String::from_utf8(err).unwrap().contains("closed"));
        FailingWriter.flush().unwrap();
    }
}
