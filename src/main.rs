fn main() {
    if let Err(error) = gremvm::run() {
        eprintln!("{}: {error:#}", gremvm::command_name());
        std::process::exit(1);
    }
}
