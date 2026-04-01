use std::process;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = xa11y::cli::run(&args) {
        eprintln!("error: {e}");
        process::exit(1);
    }
}
