use agent_os_execution::benchmark::{
    run_javascript_benchmarks_with_recovery, JavascriptBenchmarkConfig,
};
use std::path::PathBuf;

struct CliConfig {
    benchmark: JavascriptBenchmarkConfig,
    baseline_path: Option<PathBuf>,
}

fn main() {
    match parse_config(std::env::args().skip(1)) {
        Ok(cli_config) => match run_javascript_benchmarks_with_recovery(
            &cli_config.benchmark,
            cli_config.baseline_path.as_deref(),
        ) {
            Ok(output) => {
                if output.resumed_stage_count > 0 {
                    eprintln!(
                        "Resumed {} completed benchmark stages from {}",
                        output.resumed_stage_count,
                        output
                            .artifact_paths
                            .json_path
                            .parent()
                            .expect("benchmark artifact parent directory")
                            .join("run-state.json")
                            .display()
                    );
                }
                if let Some(path) = &cli_config.baseline_path {
                    eprintln!("Compared against baseline {}", path.display());
                }
                eprintln!(
                    "Wrote Markdown report to {}",
                    output.artifact_paths.markdown_path.display()
                );
                eprintln!(
                    "Wrote JSON report to {}",
                    output.artifact_paths.json_path.display()
                );
                match std::fs::read_to_string(&output.artifact_paths.markdown_path) {
                    Ok(markdown) => print!("{markdown}"),
                    Err(err) => {
                        eprintln!("failed to read generated markdown report: {err}");
                        std::process::exit(1);
                    }
                }
            }
            Err(err) => {
                eprintln!("{err}");
                std::process::exit(1);
            }
        },
        Err(err) => {
            eprintln!("{err}");
            eprintln!();
            eprintln!("Usage: cargo run -p agent-os-execution --bin node-import-bench -- [--iterations N] [--warmup-iterations N] [--baseline PATH]");
            std::process::exit(2);
        }
    }
}

fn parse_config(args: impl IntoIterator<Item = String>) -> Result<CliConfig, String> {
    let mut benchmark = JavascriptBenchmarkConfig::default();
    let mut baseline_path = None;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--iterations" => {
                let value = args
                    .next()
                    .ok_or_else(|| String::from("missing value for --iterations"))?;
                benchmark.iterations = parse_usize_flag("--iterations", &value)?;
            }
            "--warmup-iterations" => {
                let value = args
                    .next()
                    .ok_or_else(|| String::from("missing value for --warmup-iterations"))?;
                benchmark.warmup_iterations = parse_usize_flag("--warmup-iterations", &value)?;
            }
            "--baseline" => {
                let value = args
                    .next()
                    .ok_or_else(|| String::from("missing value for --baseline"))?;
                baseline_path = Some(PathBuf::from(value));
            }
            "--help" | "-h" => {
                return Err(String::from("help requested"));
            }
            unknown => {
                return Err(format!("unknown argument: {unknown}"));
            }
        }
    }

    Ok(CliConfig {
        benchmark,
        baseline_path,
    })
}

fn parse_usize_flag(flag: &str, value: &str) -> Result<usize, String> {
    value
        .parse::<usize>()
        .map_err(|_| format!("invalid value for {flag}: {value}"))
}
