use std::io::{stderr, stdin, stdout, IsTerminal, Read};

use suap_core::AppPaths;

// Thin wrapper excluded from the coverage requirement (see README); logic lives in `lib.rs`.
fn main() {
    let code = match AppPaths::discover() {
        Ok(paths) => {
            let password = std::env::var(chamados_cli::PASSWORD_ENV).ok();
            // Only piped/redirected input is read as text; an interactive terminal is never waited on.
            let mut input: Box<dyn Read> = if stdin().is_terminal() {
                Box::new(std::io::empty())
            } else {
                Box::new(stdin())
            };
            chamados_cli::run(
                std::env::args_os(),
                &paths,
                password,
                &mut input,
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
