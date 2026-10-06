use clap::{Parser, Subcommand};
use suap_core::{load_config, save_config, AppPaths};

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
    /// Exibe uma mensagem sobre o estado inicial do projeto.
    Status,
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Some(Command::Paths) => show_paths(),
        Some(Command::ConfigShow) => show_config(),
        Some(Command::ConfigInit { base_url, username }) => init_config(base_url, username),
        Some(Command::Status) => {
            println!("chamados-cli: fundação inicial instalada; integração ainda não implementada.");
            Ok(())
        }
        None => {
            println!("Use `chamados --help` para consultar os comandos disponíveis.");
            Ok(())
        }
    };

    if let Err(error) = result {
        eprintln!("erro: {error}");
        std::process::exit(1);
    }
}

fn show_paths() -> Result<(), Box<dyn std::error::Error>> {
    let paths = AppPaths::discover()?;
    println!("config_dir: {}", paths.config_dir().display());
    println!("config_file: {}", paths.config_file().display());
    println!("data_dir: {}", paths.data_dir().display());
    println!("session_file: {}", paths.session_file().display());
    Ok(())
}

fn show_config() -> Result<(), Box<dyn std::error::Error>> {
    let paths = AppPaths::discover()?;
    match load_config(&paths)? {
        Some(config) => {
            println!("base_url: {}", config.base_url);
            println!("username: {}", config.username.as_deref().unwrap_or("<não configurado>"));
            println!("file: {}", paths.config_file().display());
        }
        None => println!("Nenhuma configuração encontrada em {}", paths.config_file().display()),
    }
    Ok(())
}

fn init_config(
    base_url: Option<String>,
    username: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let paths = AppPaths::discover()?;
    let mut config = load_config(&paths)?.unwrap_or_default();

    if let Some(base_url) = base_url {
        config.base_url = base_url.parse()?;
    }
    if username.is_some() {
        config.username = username;
    }

    save_config(&paths, &config)?;
    println!("Configuração salva em {}", paths.config_file().display());
    Ok(())
}
