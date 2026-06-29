fn main() {
    if let Err(e) = rbind_lib::cli::run() {
        eprintln!("rbind: error: {e:#}");
        std::process::exit(1);
    }
}
