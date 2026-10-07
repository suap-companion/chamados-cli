use std::io::{stderr, stdout};

use suap_core::AppPaths;

// Thin wrapper excluded from the coverage requirement (see README); logic lives in `lib.rs`.
fn main() {
    let code = match AppPaths::discover() {
        Ok(paths) => {
            let password = std::env::var(chamados_cli::PASSWORD_ENV).ok();
            chamados_cli::run(std::env::args_os(), &paths, password, &mut stdout(), &mut stderr())
        },
        Err(error) => {
            eprintln!("erro: {error}");
            1
        }
    };
    std::process::exit(code);
}
