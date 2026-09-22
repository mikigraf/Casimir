fn main() {
    match casimir::cli::run() {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            eprintln!("casimir: {err:#}");
            std::process::exit(1);
        }
    }
}
