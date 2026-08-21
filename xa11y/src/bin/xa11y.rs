use std::process;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Exit codes: 0 = success, 1 = operation failed / no match, 2 = usage
    // error. See `xa11y::cli::CliError::exit_code` and the CLI help text.
    process::exit(xa11y::cli::run_main(&args));
}
