use agent_os_execution::benchmark::{run_javascript_benchmarks, JavascriptBenchmarkConfig};

fn main() {
    match parse_config(std::env::args().skip(1)) {
        Ok(config) => match run_javascript_benchmarks(&config) {
            Ok(report) => {
                print!("{}", report.render_markdown());
            }
            Err(err) => {
                eprintln!("{err}");
                std::process::exit(1);
            }
        },
        Err(err) => {
            eprintln!("{err}");
            eprintln!();
            eprintln!("Usage: cargo run -p agent-os-execution --bin node-import-bench -- [--iterations N] [--warmup-iterations N]");
            std::process::exit(2);
        }
    }
}

fn parse_config(
    args: impl IntoIterator<Item = String>,
) -> Result<JavascriptBenchmarkConfig, String> {
    let mut config = JavascriptBenchmarkConfig::default();
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--iterations" => {
                let value = args
                    .next()
                    .ok_or_else(|| String::from("missing value for --iterations"))?;
                config.iterations = parse_usize_flag("--iterations", &value)?;
            }
            "--warmup-iterations" => {
                let value = args
                    .next()
                    .ok_or_else(|| String::from("missing value for --warmup-iterations"))?;
                config.warmup_iterations = parse_usize_flag("--warmup-iterations", &value)?;
            }
            "--help" | "-h" => {
                return Err(String::from("help requested"));
            }
            unknown => {
                return Err(format!("unknown argument: {unknown}"));
            }
        }
    }

    Ok(config)
}

fn parse_usize_flag(flag: &str, value: &str) -> Result<usize, String> {
    value
        .parse::<usize>()
        .map_err(|_| format!("invalid value for {flag}: {value}"))
}
