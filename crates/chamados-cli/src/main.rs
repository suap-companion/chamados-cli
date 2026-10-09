use std::io::{stderr, stdin, stdout, IsTerminal, Read, Write};

use chamados_cli::{NoPrompt, Prompt};
use suap_core::AppPaths;

/// Asks questions on the terminal; the secret is read without echo.
struct TerminalPrompt;

impl Prompt for TerminalPrompt {
    fn line(&mut self, label: &str) -> std::io::Result<String> {
        print!("{label}");
        stdout().flush()?;
        let mut answer = String::new();
        stdin().read_line(&mut answer)?;
        Ok(answer.trim().to_owned())
    }

    fn secret(&mut self, label: &str) -> std::io::Result<String> {
        rpassword::prompt_password(label)
    }
}

// Thin wrapper excluded from the coverage requirement (see README); logic lives in `lib.rs`.
fn main() {
    let code = match AppPaths::discover() {
        Ok(paths) => {
            let password = std::env::var(chamados_cli::PASSWORD_ENV).ok();
            // Only piped/redirected input is read as text; an interactive terminal is never waited on,
            // except when a command explicitly asks a question (see `Prompt`).
            let interactive = stdin().is_terminal();
            let mut input: Box<dyn Read> = if interactive {
                Box::new(std::io::empty())
            } else {
                Box::new(stdin())
            };
            let mut prompt: Box<dyn Prompt> = if interactive {
                Box::new(TerminalPrompt)
            } else {
                Box::new(NoPrompt)
            };
            chamados_cli::run(
                std::env::args_os(),
                &paths,
                password,
                &mut input,
                &mut *prompt,
                &mut stdout(),
                &mut stderr(),
            )
        }
        Err(error) => {
            eprintln!("erro: {error}");
            1
        }
    };
    std::process::exit(code);
}
