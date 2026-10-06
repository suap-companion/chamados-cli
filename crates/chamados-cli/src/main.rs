use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "chamados", version, about = "Cliente local para chamados do SUAP")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Exibe uma mensagem sobre o estado inicial do projeto.
    Status,
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Status) => {
            println!("chamados-cli: fundação inicial instalada; integração ainda não implementada.");
        }
        None => {
            println!("Use `chamados --help` para consultar os comandos disponíveis.");
        }
    }
}
