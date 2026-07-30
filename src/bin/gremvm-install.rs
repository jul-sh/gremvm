fn main() {
    if let Err(error) = gremvm::run_installer() {
        eprintln!("gremvm: {error:#}");
        std::process::exit(1);
    }
}
