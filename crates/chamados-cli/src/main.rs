use std::io::{stderr, stdout};

use suap_core::AppPaths;

// Thin wrapper excluded from the coverage requirement (see README); logic lives in `lib.rs`.
fn main() {
    let code = match AppPaths::discover() {
        Ok(paths) => chamados_cli::run(std::env::args_os(), &paths, &mut stdout(), &mut stderr()),
        Err(error) => {
            eprintln!("erro: {error}");
            1
        }
    };
    std::process::exit(code);
}
