//! Command-line interface logic for `chamados`.
//!
//! The binary entry point (`main.rs`) is a thin wrapper; everything testable lives here.

use std::{error::Error, ffi::OsString, io::Write};

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

/// Runs the CLI with `args`, writing to `out`/`err`, and returns the process exit code.
pub fn run<I, T>(args: I, paths: &AppPaths, out: &mut dyn Write, err: &mut dyn Write) -> i32
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

    match execute(cli, paths, out) {
        Ok(()) => 0,
        Err(error) => {
            let _ = writeln!(err, "erro: {error}");
            1
        }
    }
}

fn execute(cli: Cli, paths: &AppPaths, out: &mut dyn Write) -> Result<(), Box<dyn Error>> {
    match cli.command {
        Some(Command::Paths) => show_paths(paths, out),
        Some(Command::ConfigShow) => show_config(paths, out),
        Some(Command::ConfigInit { base_url, username }) => init_config(paths, base_url, username, out),
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

    fn paths() -> (TempDir, AppPaths) {
        let directory = tempdir().unwrap();
        let paths = AppPaths::from_dirs(directory.path().join("config"), directory.path().join("data"));
        (directory, paths)
    }

    fn run_args(args: &[&str], paths: &AppPaths) -> (i32, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(std::iter::once("chamados").chain(args.iter().copied()), paths, &mut out, &mut err);
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
        let code = run(["chamados", "status"], &paths, &mut FailingWriter, &mut err);
        assert_eq!(code, 1);
        assert!(String::from_utf8(err).unwrap().contains("closed"));
        FailingWriter.flush().unwrap();
    }
}
