fn main() {
    match casimir::cli::run() {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            eprintln!("casimir: {}", casimir::privacy::redact(&format!("{err:#}")));
            std::process::exit(1);
        }
    }
}
