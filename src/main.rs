fn main() {
    if let Err(err) = harness_cli::cli::run_terminal(std::env::args()) {
        eprintln!("{err}");
        std::process::exit(2);
    }
}
