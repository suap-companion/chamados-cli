use std::io::{stderr, stdin, stdout, IsTerminal, Read, Write};

use chamados_cli::Prompt;
use suap_core::AppPaths;

/// What the process knows about its terminals: it asks questions only when standard input is a
/// terminal (the secret is read without echo), and styles tables only when standard output is one.
struct Console {
    interactive: bool,
    ansi_output: bool,
}

impl Console {
    fn detect() -> Self {
        Self {
            interactive: stdin().is_terminal(),
            ansi_output: stdout().is_terminal() && understands_ansi(),
        }
    }
}

/// The classic Windows console shows escape sequences as garbage; the modern terminals announce
/// themselves through these variables (everywhere else a terminal is assumed to understand ANSI).
fn understands_ansi() -> bool {
    if !cfg!(windows) {
        return true;
    }
    ["WT_SESSION", "TERM_PROGRAM", "TERM", "ConEmuANSI"]
        .iter()
        .any(|name| std::env::var_os(name).is_some())
}

impl Prompt for Console {
    fn line(&mut self, label: &str) -> std::io::Result<String> {
        if !self.interactive {
            return Err(std::io::Error::other("no interactive terminal"));
        }
        print!("{label}");
        stdout().flush()?;
        let mut answer = String::new();
        stdin().read_line(&mut answer)?;
        Ok(answer.trim().to_owned())
    }

    fn secret(&mut self, label: &str) -> std::io::Result<String> {
        if !self.interactive {
            return Err(std::io::Error::other("no interactive terminal"));
        }
        rpassword::prompt_password(label)
    }

    fn styled_output(&self) -> bool {
        self.ansi_output
    }
}

// Thin wrapper excluded from the coverage requirement (see README); logic lives in `lib.rs`.
fn main() {
    let code = match AppPaths::discover() {
        Ok(paths) => {
            let password = std::env::var(chamados_cli::PASSWORD_ENV).ok();
            // Only piped/redirected input is read as text; an interactive terminal is never waited on,
            // except when a command explicitly asks a question (see `Prompt`).
            let mut console = Console::detect();
            let mut input: Box<dyn Read> = if console.interactive {
                Box::new(std::io::empty())
            } else {
                Box::new(stdin())
            };
            chamados_cli::run(
                std::env::args_os(),
                &paths,
                password,
                &mut input,
                &mut console,
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
