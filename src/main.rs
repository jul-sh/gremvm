fn main() {
    if let Err(error) = gremvm::run() {
        eprintln!("gremvm: {error:#}");
        std::process::exit(1);
    }
}
