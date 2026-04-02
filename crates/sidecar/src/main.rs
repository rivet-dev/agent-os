mod stdio;

fn main() {
    if let Err(error) = stdio::run() {
        eprintln!("agent-os-sidecar: {error}");
        std::process::exit(1);
    }
}
