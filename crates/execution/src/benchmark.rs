use crate::{
    CreateJavascriptContextRequest, JavascriptExecutionEngine, JavascriptExecutionError,
    StartJavascriptExecutionRequest,
};
use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const BENCHMARK_MARKER_PREFIX: &str = "__AGENT_OS_BENCH__:";
const LOCAL_GRAPH_MODULE_COUNT: usize = 24;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JavascriptBenchmarkConfig {
    pub iterations: usize,
    pub warmup_iterations: usize,
}

impl Default for JavascriptBenchmarkConfig {
    fn default() -> Self {
        Self {
            iterations: 5,
            warmup_iterations: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BenchmarkHost {
    pub node_binary: String,
    pub node_version: String,
    pub os: &'static str,
    pub arch: &'static str,
    pub logical_cpus: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BenchmarkStats {
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BenchmarkScenarioReport {
    pub id: &'static str,
    pub description: &'static str,
    pub fixture: &'static str,
    pub compile_cache: &'static str,
    pub wall_samples_ms: Vec<f64>,
    pub wall_stats: BenchmarkStats,
    pub guest_import_samples_ms: Option<Vec<f64>>,
    pub guest_import_stats: Option<BenchmarkStats>,
    pub startup_overhead_samples_ms: Option<Vec<f64>>,
    pub startup_overhead_stats: Option<BenchmarkStats>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JavascriptBenchmarkReport {
    pub generated_at_unix_ms: u128,
    pub config: JavascriptBenchmarkConfig,
    pub host: BenchmarkHost,
    pub repo_root: PathBuf,
    pub scenarios: Vec<BenchmarkScenarioReport>,
}

impl JavascriptBenchmarkReport {
    pub fn render_markdown(&self) -> String {
        let mut markdown = String::new();
        let _ = writeln!(&mut markdown, "# Agent OS Node Import Benchmark");
        let _ = writeln!(&mut markdown);
        let _ = writeln!(
            &mut markdown,
            "- Generated at unix ms: `{}`",
            self.generated_at_unix_ms
        );
        let _ = writeln!(&mut markdown, "- Node binary: `{}`", self.host.node_binary);
        let _ = writeln!(
            &mut markdown,
            "- Node version: `{}`",
            self.host.node_version.trim()
        );
        let _ = writeln!(
            &mut markdown,
            "- Host: `{}` / `{}` / `{}` logical CPUs",
            self.host.os, self.host.arch, self.host.logical_cpus
        );
        let _ = writeln!(&mut markdown, "- Repo root: `{}`", self.repo_root.display());
        let _ = writeln!(
            &mut markdown,
            "- Iterations: `{}` recorded, `{}` warmup",
            self.config.iterations, self.config.warmup_iterations
        );
        let _ = writeln!(
            &mut markdown,
            "- Reproduce: `cargo run -p agent-os-execution --bin node-import-bench -- --iterations {} --warmup-iterations {}`",
            self.config.iterations, self.config.warmup_iterations
        );
        let _ = writeln!(&mut markdown);
        let _ = writeln!(
            &mut markdown,
            "| Scenario | Fixture | Cache | Mean wall (ms) | P50 | P95 | Mean import (ms) | Mean startup overhead (ms) |"
        );
        let _ = writeln!(
            &mut markdown,
            "| --- | --- | --- | ---: | ---: | ---: | ---: | ---: |"
        );

        for scenario in &self.scenarios {
            let import_mean = scenario
                .guest_import_stats
                .as_ref()
                .map(|stats| format_ms(stats.mean_ms))
                .unwrap_or_else(|| String::from("n/a"));
            let startup_mean = scenario
                .startup_overhead_stats
                .as_ref()
                .map(|stats| format_ms(stats.mean_ms))
                .unwrap_or_else(|| String::from("n/a"));

            let _ = writeln!(
                &mut markdown,
                "| `{}` | {} | {} | {} | {} | {} | {} | {} |",
                scenario.id,
                scenario.fixture,
                scenario.compile_cache,
                format_ms(scenario.wall_stats.mean_ms),
                format_ms(scenario.wall_stats.p50_ms),
                format_ms(scenario.wall_stats.p95_ms),
                import_mean,
                startup_mean,
            );
        }

        let _ = writeln!(&mut markdown);
        let _ = writeln!(&mut markdown, "## Hotspot Guidance");
        let _ = writeln!(&mut markdown);

        for line in self.guidance_lines() {
            let _ = writeln!(&mut markdown, "- {line}");
        }

        let _ = writeln!(&mut markdown);
        let _ = writeln!(&mut markdown, "## Raw Samples");
        let _ = writeln!(&mut markdown);

        for scenario in &self.scenarios {
            let _ = writeln!(&mut markdown, "### `{}`", scenario.id);
            let _ = writeln!(&mut markdown, "- Description: {}", scenario.description);
            let _ = writeln!(
                &mut markdown,
                "- Wall samples (ms): {}",
                format_sample_list(&scenario.wall_samples_ms)
            );
            if let Some(samples) = &scenario.guest_import_samples_ms {
                let _ = writeln!(
                    &mut markdown,
                    "- Guest import samples (ms): {}",
                    format_sample_list(samples)
                );
            }
            if let Some(samples) = &scenario.startup_overhead_samples_ms {
                let _ = writeln!(
                    &mut markdown,
                    "- Startup overhead samples (ms): {}",
                    format_sample_list(samples)
                );
            }
            let _ = writeln!(&mut markdown);
        }

        markdown
    }

    fn guidance_lines(&self) -> Vec<String> {
        let isolate = self.scenario("isolate-startup");
        let cold_local = self.scenario("cold-local-import");
        let warm_local = self.scenario("warm-local-import");
        let builtin = self.scenario("builtin-import");
        let large = self.scenario("large-package-import");

        let mut guidance = Vec::new();

        if let (
            Some(cold_import),
            Some(warm_import),
            Some(warm_startup),
            Some(warm_wall),
            Some(isolate_wall),
        ) = (
            cold_local
                .and_then(|scenario| scenario.guest_import_stats.as_ref())
                .map(|stats| stats.mean_ms),
            warm_local
                .and_then(|scenario| scenario.guest_import_stats.as_ref())
                .map(|stats| stats.mean_ms),
            warm_local
                .and_then(|scenario| scenario.startup_overhead_stats.as_ref())
                .map(|stats| stats.mean_ms),
            warm_local.map(|scenario| scenario.wall_stats.mean_ms),
            isolate.map(|scenario| scenario.wall_stats.mean_ms),
        ) {
            guidance.push(format!(
                "Compile-cache reuse cuts the local import graph from {} to {} on average ({:.1}% faster), but the warm path still spends {} outside guest module evaluation. That keeps startup prewarm work in `ARC-021D` and sidecar warm-pool/snapshot work in `ARC-022` on the critical path above the `{}` empty-isolate floor.",
                format_ms(cold_import),
                format_ms(warm_import),
                percentage_reduction(cold_import, warm_import),
                format_ms(warm_startup),
                format_ms(isolate_wall),
            ));
            if warm_wall > 0.0 {
                guidance.push(format!(
                    "Warm local imports still spend {:.1}% of wall time in process startup, wrapper evaluation, and stdio handling instead of guest import work. Optimizations that only touch module compilation will not remove that floor.",
                    percentage_share(warm_startup, warm_wall),
                ));
            }
        }

        if let (Some(builtin_import), Some(large_import)) = (
            builtin
                .and_then(|scenario| scenario.guest_import_stats.as_ref())
                .map(|stats| stats.mean_ms),
            large
                .and_then(|scenario| scenario.guest_import_stats.as_ref())
                .map(|stats| stats.mean_ms),
        ) {
            guidance.push(format!(
                "The large real-world package import (`typescript`) is {:.1}x the builtin path ({} versus {}). That makes `ARC-021C` the right next import-path optimization story: cache sidecar-scoped resolution results, package-type lookups, and module-format classification before attempting deeper structural rewrites.",
                safe_ratio(large_import, builtin_import),
                format_ms(large_import),
                format_ms(builtin_import),
            ));
        }

        guidance.push(String::from(
            "No new PRD stories were added from this run. The measured hotspots already map cleanly onto existing follow-ons: `ARC-021C` for safe resolution and metadata caches, `ARC-021D` for builtin/polyfill prewarm, and `ARC-022` for broader warm-pool and timing-mitigation execution work.",
        ));

        guidance
    }

    fn scenario(&self, id: &str) -> Option<&BenchmarkScenarioReport> {
        self.scenarios.iter().find(|scenario| scenario.id == id)
    }
}

#[derive(Debug)]
pub enum JavascriptBenchmarkError {
    InvalidConfig(&'static str),
    InvalidWorkspaceRoot(PathBuf),
    Io(std::io::Error),
    Utf8(std::string::FromUtf8Error),
    Execution(JavascriptExecutionError),
    NodeVersion(std::io::Error),
    MissingBenchmarkMetric(&'static str),
    InvalidBenchmarkMetric {
        scenario: &'static str,
        raw_value: String,
    },
    NonZeroExit {
        scenario: &'static str,
        exit_code: i32,
        stderr: String,
    },
}

impl fmt::Display for JavascriptBenchmarkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(message) => write!(f, "invalid benchmark config: {message}"),
            Self::InvalidWorkspaceRoot(path) => {
                write!(
                    f,
                    "failed to resolve workspace root from execution crate path: {}",
                    path.display()
                )
            }
            Self::Io(err) => write!(f, "benchmark I/O failure: {err}"),
            Self::Utf8(err) => write!(f, "benchmark output was not valid UTF-8: {err}"),
            Self::Execution(err) => write!(f, "benchmark execution failed: {err}"),
            Self::NodeVersion(err) => write!(f, "failed to query node version: {err}"),
            Self::MissingBenchmarkMetric(scenario) => {
                write!(
                    f,
                    "benchmark scenario `{scenario}` did not emit a metric marker"
                )
            }
            Self::InvalidBenchmarkMetric {
                scenario,
                raw_value,
            } => write!(
                f,
                "benchmark scenario `{scenario}` emitted an invalid metric: {raw_value}"
            ),
            Self::NonZeroExit {
                scenario,
                exit_code,
                stderr,
            } => write!(
                f,
                "benchmark scenario `{scenario}` exited with code {exit_code}: {stderr}"
            ),
        }
    }
}

impl std::error::Error for JavascriptBenchmarkError {}

impl From<std::io::Error> for JavascriptBenchmarkError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<std::string::FromUtf8Error> for JavascriptBenchmarkError {
    fn from(err: std::string::FromUtf8Error) -> Self {
        Self::Utf8(err)
    }
}

impl From<JavascriptExecutionError> for JavascriptBenchmarkError {
    fn from(err: JavascriptExecutionError) -> Self {
        Self::Execution(err)
    }
}

pub fn run_javascript_benchmarks(
    config: &JavascriptBenchmarkConfig,
) -> Result<JavascriptBenchmarkReport, JavascriptBenchmarkError> {
    if config.iterations == 0 {
        return Err(JavascriptBenchmarkError::InvalidConfig(
            "iterations must be greater than zero",
        ));
    }

    let repo_root = workspace_root()?;
    let host = benchmark_host()?;
    let workspace = BenchmarkWorkspace::create(&repo_root)?;

    let mut scenarios = Vec::new();

    for scenario in benchmark_scenarios() {
        scenarios.push(run_scenario(&workspace, config, scenario)?);
    }

    Ok(JavascriptBenchmarkReport {
        generated_at_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
        config: config.clone(),
        host,
        repo_root,
        scenarios,
    })
}

#[derive(Debug)]
struct ScenarioDefinition {
    id: &'static str,
    description: &'static str,
    fixture: &'static str,
    entrypoint: &'static str,
    compile_cache: CompileCacheStrategy,
    expect_import_metric: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompileCacheStrategy {
    Disabled,
    Primed,
}

impl CompileCacheStrategy {
    fn label(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Primed => "primed",
        }
    }
}

#[derive(Debug)]
struct SampleMeasurement {
    wall_ms: f64,
    guest_import_ms: Option<f64>,
}

#[derive(Debug)]
struct BenchmarkWorkspace {
    root: PathBuf,
}

impl BenchmarkWorkspace {
    fn create(repo_root: &Path) -> Result<Self, JavascriptBenchmarkError> {
        let root = repo_root.join(format!(
            ".tmp-agent-os-execution-bench-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&root)?;
        write_benchmark_workspace(&root)?;
        Ok(Self { root })
    }
}

impl Drop for BenchmarkWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn benchmark_scenarios() -> [ScenarioDefinition; 5] {
    [
        ScenarioDefinition {
            id: "isolate-startup",
            description:
                "Minimal guest with no extra imports. Measures the current startup floor for create-context plus node process bootstrap.",
            fixture: "empty entrypoint",
            entrypoint: "./bench/isolate-startup.mjs",
            compile_cache: CompileCacheStrategy::Disabled,
            expect_import_metric: false,
        },
        ScenarioDefinition {
            id: "cold-local-import",
            description:
                "Cold import of a repo-local ESM graph that simulates layered application modules without compile-cache reuse.",
            fixture: "24-module local ESM graph",
            entrypoint: "./bench/cold-local-import.mjs",
            compile_cache: CompileCacheStrategy::Disabled,
            expect_import_metric: true,
        },
        ScenarioDefinition {
            id: "warm-local-import",
            description:
                "Warm import of the same local ESM graph after a compile-cache priming pass in an earlier isolate.",
            fixture: "24-module local ESM graph",
            entrypoint: "./bench/warm-local-import.mjs",
            compile_cache: CompileCacheStrategy::Primed,
            expect_import_metric: true,
        },
        ScenarioDefinition {
            id: "builtin-import",
            description:
                "Import of the common builtin path used by the wrappers and polyfill-adjacent bootstrap code.",
            fixture: "node:path + node:url + node:fs/promises",
            entrypoint: "./bench/builtin-import.mjs",
            compile_cache: CompileCacheStrategy::Disabled,
            expect_import_metric: true,
        },
        ScenarioDefinition {
            id: "large-package-import",
            description:
                "Cold import of the real-world `typescript` package from the workspace root `node_modules` tree.",
            fixture: "typescript",
            entrypoint: "./bench/large-package-import.mjs",
            compile_cache: CompileCacheStrategy::Disabled,
            expect_import_metric: true,
        },
    ]
}

fn run_scenario(
    workspace: &BenchmarkWorkspace,
    config: &JavascriptBenchmarkConfig,
    scenario: ScenarioDefinition,
) -> Result<BenchmarkScenarioReport, JavascriptBenchmarkError> {
    let compile_cache_root = workspace
        .root
        .join("compile-cache")
        .join(scenario.id.replace('-', "_"));

    if scenario.compile_cache == CompileCacheStrategy::Primed {
        run_sample(
            workspace,
            &scenario,
            Some(compile_cache_root.clone()),
            "prime-cache",
        )?;
    }

    for warmup_index in 0..config.warmup_iterations {
        let label = format!("warmup-{}", warmup_index + 1);
        run_sample(
            workspace,
            &scenario,
            compile_cache_root_for_strategy(scenario.compile_cache, &compile_cache_root),
            &label,
        )?;
    }

    let mut wall_samples_ms = Vec::with_capacity(config.iterations);
    let mut guest_import_samples_ms = if scenario.expect_import_metric {
        Some(Vec::with_capacity(config.iterations))
    } else {
        None
    };

    for iteration in 0..config.iterations {
        let label = format!("measure-{}", iteration + 1);
        let sample = run_sample(
            workspace,
            &scenario,
            compile_cache_root_for_strategy(scenario.compile_cache, &compile_cache_root),
            &label,
        )?;
        wall_samples_ms.push(sample.wall_ms);

        if let (Some(import_ms), Some(samples)) =
            (sample.guest_import_ms, guest_import_samples_ms.as_mut())
        {
            samples.push(import_ms);
        }
    }

    let startup_overhead_samples_ms = guest_import_samples_ms.as_ref().map(|guest_samples| {
        wall_samples_ms
            .iter()
            .zip(guest_samples.iter())
            .map(|(wall_ms, import_ms)| wall_ms - import_ms)
            .collect::<Vec<_>>()
    });

    Ok(BenchmarkScenarioReport {
        id: scenario.id,
        description: scenario.description,
        fixture: scenario.fixture,
        compile_cache: scenario.compile_cache.label(),
        wall_stats: compute_stats(&wall_samples_ms),
        guest_import_stats: guest_import_samples_ms
            .as_ref()
            .map(|samples| compute_stats(samples)),
        startup_overhead_stats: startup_overhead_samples_ms
            .as_ref()
            .map(|samples| compute_stats(samples)),
        wall_samples_ms,
        guest_import_samples_ms,
        startup_overhead_samples_ms,
    })
}

fn compile_cache_root_for_strategy(strategy: CompileCacheStrategy, root: &Path) -> Option<PathBuf> {
    match strategy {
        CompileCacheStrategy::Disabled => None,
        CompileCacheStrategy::Primed => Some(root.to_path_buf()),
    }
}

fn run_sample(
    workspace: &BenchmarkWorkspace,
    scenario: &ScenarioDefinition,
    compile_cache_root: Option<PathBuf>,
    _label: &str,
) -> Result<SampleMeasurement, JavascriptBenchmarkError> {
    let mut engine = JavascriptExecutionEngine::default();
    let started_at = Instant::now();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-bench"),
        bootstrap_module: None,
        compile_cache_root,
    });

    let execution = engine.start_execution(StartJavascriptExecutionRequest {
        vm_id: String::from("vm-bench"),
        context_id: context.context_id,
        argv: vec![String::from(scenario.entrypoint)],
        env: BTreeMap::new(),
        cwd: workspace.root.clone(),
    })?;

    let result = execution.wait()?;
    let wall_ms = started_at.elapsed().as_secs_f64() * 1000.0;
    let stdout = String::from_utf8(result.stdout)?;
    let stderr = String::from_utf8(result.stderr)?;

    if result.exit_code != 0 {
        return Err(JavascriptBenchmarkError::NonZeroExit {
            scenario: scenario.id,
            exit_code: result.exit_code,
            stderr,
        });
    }

    let guest_import_ms = if scenario.expect_import_metric {
        Some(parse_benchmark_metric(scenario.id, &stdout)?)
    } else {
        None
    };

    Ok(SampleMeasurement {
        wall_ms,
        guest_import_ms,
    })
}

fn parse_benchmark_metric(
    scenario_id: &'static str,
    stdout: &str,
) -> Result<f64, JavascriptBenchmarkError> {
    let raw_value = stdout
        .lines()
        .find_map(|line| line.strip_prefix(BENCHMARK_MARKER_PREFIX))
        .ok_or(JavascriptBenchmarkError::MissingBenchmarkMetric(
            scenario_id,
        ))?;

    raw_value
        .parse::<f64>()
        .map_err(|_| JavascriptBenchmarkError::InvalidBenchmarkMetric {
            scenario: scenario_id,
            raw_value: raw_value.to_owned(),
        })
}

fn workspace_root() -> Result<PathBuf, JavascriptBenchmarkError> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or(JavascriptBenchmarkError::InvalidWorkspaceRoot(manifest_dir))
}

fn benchmark_host() -> Result<BenchmarkHost, JavascriptBenchmarkError> {
    let node_binary = crate::node_process::node_binary();
    let output = Command::new(&node_binary)
        .arg("--version")
        .output()
        .map_err(JavascriptBenchmarkError::NodeVersion)?;
    let node_version = String::from_utf8(output.stdout)?;

    Ok(BenchmarkHost {
        node_binary,
        node_version,
        os: env::consts::OS,
        arch: env::consts::ARCH,
        logical_cpus: std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1),
    })
}

fn write_benchmark_workspace(root: &Path) -> Result<(), JavascriptBenchmarkError> {
    fs::create_dir_all(root.join("bench"))?;
    fs::create_dir_all(root.join("bench/local-graph"))?;
    fs::write(
        root.join("package.json"),
        "{\n  \"name\": \"agent-os-execution-bench\",\n  \"private\": true,\n  \"type\": \"module\"\n}\n",
    )?;

    for index in 0..LOCAL_GRAPH_MODULE_COUNT {
        let path = root
            .join("bench/local-graph")
            .join(format!("mod-{index:02}.mjs"));
        let source = if index == 0 {
            String::from("export const value = 1;\n")
        } else {
            format!(
                "import {{ value as previous }} from './mod-{previous:02}.mjs';\nexport const value = previous + {index};\n",
                previous = index - 1
            )
        };
        fs::write(path, source)?;
    }

    let final_value = local_graph_terminal_value();
    fs::write(
        root.join("bench/local-graph/root.mjs"),
        format!(
            "import {{ value }} from './mod-{last:02}.mjs';\nexport {{ value }};\nexport const expected = {final_value};\n",
            last = LOCAL_GRAPH_MODULE_COUNT - 1
        ),
    )?;

    fs::write(
        root.join("bench/isolate-startup.mjs"),
        "console.log('isolate-ready');\n",
    )?;
    fs::write(
        root.join("bench/cold-local-import.mjs"),
        local_import_entrypoint_source(final_value),
    )?;
    fs::write(
        root.join("bench/warm-local-import.mjs"),
        local_import_entrypoint_source(final_value),
    )?;
    fs::write(
        root.join("bench/builtin-import.mjs"),
        format!(
            "import {{ performance }} from 'node:perf_hooks';\nconst started = performance.now();\nconst [pathMod, fsMod, urlMod] = await Promise.all([\n  import('node:path'),\n  import('node:fs/promises'),\n  import('node:url'),\n]);\nif (typeof pathMod.basename !== 'function' || typeof fsMod.readFile !== 'function' || typeof urlMod.pathToFileURL !== 'function') {{\n  throw new Error('builtin import fixture did not load expected exports');\n}}\nconsole.log('{BENCHMARK_MARKER_PREFIX}' + String(performance.now() - started));\n",
        ),
    )?;
    fs::write(
        root.join("bench/large-package-import.mjs"),
        format!(
            "import {{ performance }} from 'node:perf_hooks';\nconst started = performance.now();\nconst typescript = await import('typescript');\nif (typeof typescript.transpileModule !== 'function') {{\n  throw new Error('typescript import did not expose transpileModule');\n}}\nconsole.log('{BENCHMARK_MARKER_PREFIX}' + String(performance.now() - started));\n",
        ),
    )?;

    Ok(())
}

fn local_import_entrypoint_source(final_value: usize) -> String {
    format!(
        "import {{ performance }} from 'node:perf_hooks';\nconst started = performance.now();\nconst graph = await import('./local-graph/root.mjs');\nif (graph.value !== {final_value} || graph.expected !== {final_value}) {{\n  throw new Error(`local graph import returned ${{
    graph.value
  }} instead of {final_value}`);\n}}\nconsole.log('{BENCHMARK_MARKER_PREFIX}' + String(performance.now() - started));\n"
    )
}

fn local_graph_terminal_value() -> usize {
    let mut value = 1;

    for index in 1..LOCAL_GRAPH_MODULE_COUNT {
        value += index;
    }

    value
}

fn compute_stats(samples: &[f64]) -> BenchmarkStats {
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let mean_ms = sorted.iter().sum::<f64>() / sorted.len() as f64;

    BenchmarkStats {
        mean_ms,
        p50_ms: percentile(&sorted, 50.0),
        p95_ms: percentile(&sorted, 95.0),
        min_ms: *sorted.first().unwrap_or(&0.0),
        max_ms: *sorted.last().unwrap_or(&0.0),
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }

    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    let index = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[index]
}

fn percentage_reduction(original: f64, current: f64) -> f64 {
    if original <= 0.0 {
        0.0
    } else {
        ((original - current) / original) * 100.0
    }
}

fn percentage_share(part: f64, total: f64) -> f64 {
    if total <= 0.0 {
        0.0
    } else {
        (part / total) * 100.0
    }
}

fn safe_ratio(lhs: f64, rhs: f64) -> f64 {
    if rhs <= 0.0 {
        0.0
    } else {
        lhs / rhs
    }
}

fn format_ms(value: f64) -> String {
    format!("{value:.2}")
}

fn format_sample_list(samples: &[f64]) -> String {
    let mut formatted = String::from("[");

    for (index, sample) in samples.iter().enumerate() {
        if index > 0 {
            formatted.push_str(", ");
        }
        let _ = write!(&mut formatted, "{sample:.2}");
    }

    formatted.push(']');
    formatted
}
