fn main() {
    use std::io::Write;

    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let code = secureexec_awk::main(args);
    if let Err(error) = std::io::stdout().flush() {
        eprintln!("Error flushing stdout: {error}");
    }
    std::process::exit(code);
}
