use std::process;

use xa11y::cli::CliError;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = xa11y::cli::run(&args) {
        // `CliError`'s Display already prefixes usage errors with
        // "usage error: "; everything else gets the generic prefix.
        // Exit codes: 1 = operation failed / no match, 2 = usage error
        // (see `CliError::exit_code` and the CLI help text).
        match &e {
            CliError::Usage(_) => eprintln!("{e}"),
            _ => eprintln!("error: {e}"),
        }
        process::exit(e.exit_code());
    }
}
