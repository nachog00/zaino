fn main() {
    if let Err(e) = xtask::cli::run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
