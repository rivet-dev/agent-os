use crate::{
    CreateJavascriptContextRequest, JavascriptExecutionEngine, JavascriptExecutionError,
    StartJavascriptExecutionRequest,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const BENCHMARK_MARKER_PREFIX: &str = "__AGENT_OS_BENCH__:";
const LOCAL_GRAPH_MODULE_COUNT: usize = 24;
const BENCHMARK_ARTIFACT_VERSION: u32 = 5;
const BENCHMARK_ARTIFACT_DIR: &str = "target/benchmark-reports/node-import-bench";
const BENCHMARK_RUN_STATE_FILE: &str = "run-state.json";
const TRANSPORT_RTT_CHANNEL: &str = "execution-stdio-echo";
const TRANSPORT_RTT_PAYLOAD_BYTES: [usize; 3] = [32, 4 * 1024, 64 * 1024];
const TRANSPORT_POLL_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BenchmarkHost {
    pub node_binary: String,
    pub node_version: String,
    pub os: &'static str,
    pub arch: &'static str,
    pub logical_cpus: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenchmarkScenarioPhases<T> {
    pub context_setup_ms: T,
    pub startup_ms: T,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub guest_execution_ms: Option<T>,
    pub completion_ms: T,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BenchmarkStats {
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
    pub stddev_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BenchmarkDistributionStats {
    pub mean: f64,
    pub p50: f64,
    pub p95: f64,
    pub min: f64,
    pub max: f64,
    pub stddev: f64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BenchmarkResourceUsage<T> {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub rss_bytes: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub heap_used_bytes: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cpu_user_us: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cpu_system_us: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cpu_total_us: Option<T>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BenchmarkTransportRttReport {
    pub channel: &'static str,
    pub payload_bytes: usize,
    pub samples_ms: Vec<f64>,
    pub stats: BenchmarkStats,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BenchmarkScenarioReport {
    pub id: &'static str,
    pub workload: &'static str,
    pub runtime: &'static str,
    pub mode: &'static str,
    pub description: &'static str,
    pub fixture: &'static str,
    pub compile_cache: &'static str,
    pub wall_samples_ms: Vec<f64>,
    pub wall_stats: BenchmarkStats,
    pub guest_import_samples_ms: Option<Vec<f64>>,
    pub guest_import_stats: Option<BenchmarkStats>,
    pub startup_overhead_samples_ms: Option<Vec<f64>>,
    pub startup_overhead_stats: Option<BenchmarkStats>,
    pub phase_samples_ms: BenchmarkScenarioPhases<Vec<f64>>,
    pub phase_stats: BenchmarkScenarioPhases<BenchmarkStats>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub resource_usage_samples: Option<BenchmarkResourceUsage<Vec<f64>>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub resource_usage_stats: Option<BenchmarkResourceUsage<BenchmarkDistributionStats>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JavascriptBenchmarkReport {
    pub generated_at_unix_ms: u128,
    pub config: JavascriptBenchmarkConfig,
    pub host: BenchmarkHost,
    pub repo_root: PathBuf,
    pub transport_rtt: Vec<BenchmarkTransportRttReport>,
    pub scenarios: Vec<BenchmarkScenarioReport>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BenchmarkComparison {
    pub baseline: BenchmarkComparisonBaseline,
    pub summary: BenchmarkComparisonSummary,
    pub scenario_deltas: Vec<BenchmarkScenarioDelta>,
    pub scenarios_missing_from_baseline: Vec<String>,
    pub baseline_only_scenarios: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BenchmarkComparisonBaseline {
    pub artifact_version: u32,
    pub generated_at_unix_ms: u128,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BenchmarkComparisonSummary {
    pub compared_scenario_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub largest_wall_improvement: Option<BenchmarkDeltaHighlight>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub largest_wall_regression: Option<BenchmarkDeltaHighlight>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BenchmarkDeltaHighlight {
    pub id: String,
    pub delta_ms: f64,
    pub delta_pct: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BenchmarkScenarioDelta {
    pub id: String,
    pub description: String,
    pub wall_mean_ms: BenchmarkMetricDelta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guest_import_mean_ms: Option<BenchmarkMetricDelta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub startup_overhead_mean_ms: Option<BenchmarkMetricDelta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase_mean_ms: Option<BenchmarkScenarioPhases<BenchmarkMetricDelta>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BenchmarkMetricDelta {
    pub baseline_ms: f64,
    pub current_ms: f64,
    pub delta_ms: f64,
    pub delta_pct: f64,
}

impl JavascriptBenchmarkReport {
    pub fn render_markdown(&self) -> String {
        self.render_markdown_with_comparison(None)
    }

    pub fn render_markdown_with_comparison(
        &self,
        comparison: Option<&BenchmarkComparison>,
    ) -> String {
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
        let _ = writeln!(&mut markdown, "## Transport RTT");
        let _ = writeln!(&mut markdown);
        let _ = writeln!(
            &mut markdown,
            "| Channel | Payload (bytes) | Mean RTT (ms) | P50 | P95 |"
        );
        let _ = writeln!(&mut markdown, "| --- | ---: | ---: | ---: | ---: |");

        for transport in &self.transport_rtt {
            let _ = writeln!(
                &mut markdown,
                "| `{}` | {} | {} | {} | {} |",
                transport.channel,
                transport.payload_bytes,
                format_ms(transport.stats.mean_ms),
                format_ms(transport.stats.p50_ms),
                format_ms(transport.stats.p95_ms),
            );
        }

        let _ = writeln!(&mut markdown, "## Control Matrix");
        let _ = writeln!(&mut markdown);

        for row in self.control_matrix() {
            let _ = writeln!(
                &mut markdown,
                "- Workload `{}`: runtimes {}, modes {}, scenarios {}",
                row.workload,
                format_label_list(&row.runtimes),
                format_label_list(&row.modes),
                format_label_list(&row.scenario_ids),
            );
        }

        let _ = writeln!(&mut markdown);
        let _ = writeln!(&mut markdown, "## Scenario Summary");
        let _ = writeln!(&mut markdown);
        let _ = writeln!(
            &mut markdown,
            "| Scenario | Workload | Runtime | Mode | Fixture | Cache | Mean wall (ms) | Mean context (ms) | Mean startup (ms) | Mean guest exec (ms) | Mean completion (ms) | Mean startup overhead (ms) |"
        );
        let _ = writeln!(
            &mut markdown,
            "| --- | --- | --- | --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |"
        );

        for scenario in &self.scenarios {
            let guest_execution_mean = scenario
                .phase_stats
                .guest_execution_ms
                .as_ref()
                .map(|stats| format_ms(stats.mean_ms))
                .unwrap_or_else(|| String::from("n/a"));
            let startup_overhead_mean = scenario
                .startup_overhead_stats
                .as_ref()
                .map(|stats| format_ms(stats.mean_ms))
                .unwrap_or_else(|| String::from("n/a"));

            let _ = writeln!(
                &mut markdown,
                "| `{}` | `{}` | `{}` | `{}` | {} | {} | {} | {} | {} | {} | {} | {} |",
                scenario.id,
                scenario.workload,
                scenario.runtime,
                scenario.mode,
                scenario.fixture,
                scenario.compile_cache,
                format_ms(scenario.wall_stats.mean_ms),
                format_ms(scenario.phase_stats.context_setup_ms.mean_ms),
                format_ms(scenario.phase_stats.startup_ms.mean_ms),
                guest_execution_mean,
                format_ms(scenario.phase_stats.completion_ms.mean_ms),
                startup_overhead_mean,
            );
        }

        let _ = writeln!(&mut markdown);
        let _ = writeln!(&mut markdown, "## Stability And Resource Summary");
        let _ = writeln!(&mut markdown);
        let _ = writeln!(
            &mut markdown,
            "| Scenario | Wall P50 (ms) | Wall min-max (ms) | Wall stddev (ms) | Mean RSS (MiB) | Mean heap (MiB) | Mean total CPU (ms) |"
        );
        let _ = writeln!(
            &mut markdown,
            "| --- | ---: | --- | ---: | ---: | ---: | ---: |"
        );

        for scenario in &self.scenarios {
            let _ = writeln!(
                &mut markdown,
                "| `{}` | {} | {}-{} | {} | {} | {} | {} |",
                scenario.id,
                format_ms(scenario.wall_stats.p50_ms),
                format_ms(scenario.wall_stats.min_ms),
                format_ms(scenario.wall_stats.max_ms),
                format_ms(scenario.wall_stats.stddev_ms),
                scenario
                    .resource_usage_stats
                    .as_ref()
                    .and_then(|stats| stats.rss_bytes.as_ref())
                    .map(|stats| format_mib(bytes_to_mib(stats.mean)))
                    .unwrap_or_else(|| String::from("n/a")),
                scenario
                    .resource_usage_stats
                    .as_ref()
                    .and_then(|stats| stats.heap_used_bytes.as_ref())
                    .map(|stats| format_mib(bytes_to_mib(stats.mean)))
                    .unwrap_or_else(|| String::from("n/a")),
                scenario
                    .resource_usage_stats
                    .as_ref()
                    .and_then(|stats| stats.cpu_total_us.as_ref())
                    .map(|stats| format_ms(micros_to_ms(stats.mean)))
                    .unwrap_or_else(|| String::from("n/a")),
            );
        }

        let _ = writeln!(&mut markdown);
        let _ = writeln!(&mut markdown, "## Ranked Hotspots");
        let _ = writeln!(&mut markdown);

        for ranking in self.hotspot_rankings() {
            let _ = writeln!(
                &mut markdown,
                "### {} (`{}`, `{}`)",
                ranking.label, ranking.dimension, ranking.unit
            );
            let _ = writeln!(&mut markdown);
            let _ = writeln!(
                &mut markdown,
                "| Rank | Scenario | Workload | Runtime | Mode | Value |"
            );
            let _ = writeln!(&mut markdown, "| ---: | --- | --- | --- | --- | ---: |");

            for scenario in &ranking.ranked_scenarios {
                let _ = writeln!(
                    &mut markdown,
                    "| {} | `{}` | `{}` | `{}` | `{}` | {} |",
                    scenario.rank,
                    scenario.id,
                    scenario.workload,
                    scenario.runtime,
                    scenario.mode,
                    format_hotspot_value(ranking.unit, scenario.value),
                );
            }

            if !ranking.scenarios_without_metric.is_empty() {
                let _ = writeln!(&mut markdown);
                let _ = writeln!(
                    &mut markdown,
                    "Missing metric for: {}",
                    format_string_label_list(&ranking.scenarios_without_metric),
                );
            }

            let _ = writeln!(&mut markdown);
        }

        let _ = writeln!(&mut markdown, "## Hotspot Guidance");
        let _ = writeln!(&mut markdown);

        for line in self.guidance_lines() {
            let _ = writeln!(&mut markdown, "- {line}");
        }

        if let Some(comparison) = comparison {
            let _ = writeln!(&mut markdown);
            let _ = writeln!(&mut markdown, "## Baseline Comparison");
            let _ = writeln!(&mut markdown);
            let _ = writeln!(
                &mut markdown,
                "- Baseline artifact: `{}`",
                comparison.baseline.path.display()
            );
            let _ = writeln!(
                &mut markdown,
                "- Baseline generated at unix ms: `{}`",
                comparison.baseline.generated_at_unix_ms
            );
            let _ = writeln!(
                &mut markdown,
                "- Compared scenarios: `{}`",
                comparison.summary.compared_scenario_count
            );
            if let Some(improvement) = &comparison.summary.largest_wall_improvement {
                let _ = writeln!(
                    &mut markdown,
                    "- Largest wall-time improvement: `{}` at {} ({})",
                    improvement.id,
                    format_delta_ms(improvement.delta_ms),
                    format_delta_pct(improvement.delta_pct),
                );
            }
            if let Some(regression) = &comparison.summary.largest_wall_regression {
                let _ = writeln!(
                    &mut markdown,
                    "- Largest wall-time regression: `{}` at {} ({})",
                    regression.id,
                    format_delta_ms(regression.delta_ms),
                    format_delta_pct(regression.delta_pct),
                );
            }
            if !comparison.scenarios_missing_from_baseline.is_empty() {
                let _ = writeln!(
                    &mut markdown,
                    "- Scenarios missing from baseline: {}",
                    comparison.scenarios_missing_from_baseline.join(", ")
                );
            }
            if !comparison.baseline_only_scenarios.is_empty() {
                let _ = writeln!(
                    &mut markdown,
                    "- Baseline-only scenarios: {}",
                    comparison.baseline_only_scenarios.join(", ")
                );
            }
            let _ = writeln!(&mut markdown);
            let _ = writeln!(
                &mut markdown,
                "| Scenario | Wall delta (ms) | Wall delta % | Import delta (ms) | Startup delta (ms) | Context delta (ms) | Completion delta (ms) |"
            );
            let _ = writeln!(
                &mut markdown,
                "| --- | ---: | ---: | ---: | ---: | ---: | ---: |"
            );

            for scenario in &comparison.scenario_deltas {
                let import_delta = scenario
                    .guest_import_mean_ms
                    .as_ref()
                    .map(|delta| format_delta_ms(delta.delta_ms))
                    .unwrap_or_else(|| String::from("n/a"));
                let startup_delta = scenario
                    .startup_overhead_mean_ms
                    .as_ref()
                    .map(|delta| format_delta_ms(delta.delta_ms))
                    .unwrap_or_else(|| String::from("n/a"));
                let context_delta = scenario
                    .phase_mean_ms
                    .as_ref()
                    .map(|delta| format_delta_ms(delta.context_setup_ms.delta_ms))
                    .unwrap_or_else(|| String::from("n/a"));
                let completion_delta = scenario
                    .phase_mean_ms
                    .as_ref()
                    .map(|delta| format_delta_ms(delta.completion_ms.delta_ms))
                    .unwrap_or_else(|| String::from("n/a"));

                let _ = writeln!(
                    &mut markdown,
                    "| `{}` | {} | {} | {} | {} | {} | {} |",
                    scenario.id,
                    format_delta_ms(scenario.wall_mean_ms.delta_ms),
                    format_delta_pct(scenario.wall_mean_ms.delta_pct),
                    import_delta,
                    startup_delta,
                    context_delta,
                    completion_delta,
                );
            }
        }

        let _ = writeln!(&mut markdown);
        let _ = writeln!(&mut markdown, "## Raw Samples");
        let _ = writeln!(&mut markdown);

        for scenario in &self.scenarios {
            let _ = writeln!(&mut markdown, "### `{}`", scenario.id);
            let _ = writeln!(&mut markdown, "- Workload: `{}`", scenario.workload);
            let _ = writeln!(&mut markdown, "- Runtime: `{}`", scenario.runtime);
            let _ = writeln!(&mut markdown, "- Mode: `{}`", scenario.mode);
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
            let _ = writeln!(
                &mut markdown,
                "- Context setup samples (ms): {}",
                format_sample_list(&scenario.phase_samples_ms.context_setup_ms)
            );
            let _ = writeln!(
                &mut markdown,
                "- Startup samples (ms): {}",
                format_sample_list(&scenario.phase_samples_ms.startup_ms)
            );
            if let Some(samples) = &scenario.phase_samples_ms.guest_execution_ms {
                let _ = writeln!(
                    &mut markdown,
                    "- Guest execution samples (ms): {}",
                    format_sample_list(samples)
                );
            }
            let _ = writeln!(
                &mut markdown,
                "- Completion samples (ms): {}",
                format_sample_list(&scenario.phase_samples_ms.completion_ms)
            );
            if let Some(samples) = &scenario.resource_usage_samples {
                if let Some(rss_samples) = &samples.rss_bytes {
                    let _ = writeln!(
                        &mut markdown,
                        "- RSS samples (MiB): {}",
                        format_scaled_sample_list(rss_samples, bytes_to_mib)
                    );
                }
                if let Some(heap_samples) = &samples.heap_used_bytes {
                    let _ = writeln!(
                        &mut markdown,
                        "- Heap samples (MiB): {}",
                        format_scaled_sample_list(heap_samples, bytes_to_mib)
                    );
                }
                if let Some(cpu_samples) = &samples.cpu_total_us {
                    let _ = writeln!(
                        &mut markdown,
                        "- Total CPU samples (ms): {}",
                        format_scaled_sample_list(cpu_samples, micros_to_ms)
                    );
                }
            }
            let _ = writeln!(&mut markdown);
        }

        markdown
    }

    pub fn render_json(&self) -> Result<String, serde_json::Error> {
        self.render_json_with_comparison(None)
    }

    pub fn render_json_with_comparison(
        &self,
        comparison: Option<&BenchmarkComparison>,
    ) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(&self.json_artifact(comparison))
    }

    pub fn write_artifacts(
        &self,
        output_dir: &Path,
    ) -> Result<JavascriptBenchmarkArtifactPaths, JavascriptBenchmarkError> {
        self.write_artifacts_with_comparison(output_dir, None)
    }

    pub fn write_artifacts_with_comparison(
        &self,
        output_dir: &Path,
        comparison: Option<&BenchmarkComparison>,
    ) -> Result<JavascriptBenchmarkArtifactPaths, JavascriptBenchmarkError> {
        fs::create_dir_all(output_dir)?;

        let markdown_path = output_dir.join("report.md");
        let json_path = output_dir.join("report.json");
        write_string_atomic(
            &markdown_path,
            &self.render_markdown_with_comparison(comparison),
        )?;
        write_string_atomic(&json_path, &self.render_json_with_comparison(comparison)?)?;

        Ok(JavascriptBenchmarkArtifactPaths {
            markdown_path,
            json_path,
        })
    }

    pub fn compare_to_baseline_path(
        &self,
        baseline_path: &Path,
    ) -> Result<BenchmarkComparison, JavascriptBenchmarkError> {
        let baseline = load_benchmark_artifact(baseline_path)?;
        Ok(BenchmarkComparison::from_reports(
            self,
            baseline_path,
            &baseline,
        ))
    }

    fn guidance_lines(&self) -> Vec<String> {
        let isolate = self.scenario("isolate-startup");
        let cold_local = self.scenario("cold-local-import");
        let warm_local = self.scenario("warm-local-import");
        let prewarmed_local = self.scenario("prewarmed-local-import");
        let builtin = self.scenario("builtin-import");
        let large = self.scenario("large-package-import");

        let mut guidance = Vec::new();

        if let (
            Some(cold_import),
            Some(warm_import),
            Some(warm_context),
            Some(warm_startup_phase),
            Some(warm_completion),
            Some(warm_startup_overhead),
            Some(warm_wall),
            Some(isolate_wall),
        ) = (
            cold_local
                .and_then(|scenario| scenario.guest_import_stats.as_ref())
                .map(|stats| stats.mean_ms),
            warm_local
                .and_then(|scenario| scenario.guest_import_stats.as_ref())
                .map(|stats| stats.mean_ms),
            warm_local.map(|scenario| scenario.phase_stats.context_setup_ms.mean_ms),
            warm_local.map(|scenario| scenario.phase_stats.startup_ms.mean_ms),
            warm_local.map(|scenario| scenario.phase_stats.completion_ms.mean_ms),
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
                format_ms(warm_startup_overhead),
                format_ms(isolate_wall),
            ));
            if warm_wall > 0.0 {
                guidance.push(format!(
                    "Warm local imports still spend {:.1}% of wall time in process startup, wrapper evaluation, and stdio handling instead of guest import work. Optimizations that only touch module compilation will not remove that floor.",
                    percentage_share(warm_startup_overhead, warm_wall),
                ));
            }
            let warm_guest = warm_local
                .and_then(|scenario| scenario.phase_stats.guest_execution_ms.as_ref())
                .map(|stats| stats.mean_ms)
                .unwrap_or(0.0);
            guidance.push(format!(
                "The warm path phase split is {} context setup, {} runtime startup, {} guest execution, and {} completion/stdio work. Future attribution can now separate bootstrap wins from pure transport/collection wins instead of treating them as one startup bucket.",
                format_ms(warm_context),
                format_ms(warm_startup_phase),
                format_ms(warm_guest),
                format_ms(warm_completion),
            ));
        }

        if let (Some(warm_startup_overhead), Some(prewarmed_startup_overhead), Some(isolate_wall)) = (
            warm_local
                .and_then(|scenario| scenario.startup_overhead_stats.as_ref())
                .map(|stats| stats.mean_ms),
            prewarmed_local
                .and_then(|scenario| scenario.startup_overhead_stats.as_ref())
                .map(|stats| stats.mean_ms),
            isolate.map(|scenario| scenario.wall_stats.mean_ms),
        ) {
            guidance.push(format!(
                "Keeping the current import-cache materialization and builtin/polyfill prewarm alive inside one execution engine cuts warm local startup overhead from {} to {} ({:.1}% faster). The remaining {} of non-import work is the post-prewarm floor that broader warm-pool/snapshot work would still need to attack above the `{}` empty-isolate baseline.",
                format_ms(warm_startup_overhead),
                format_ms(prewarmed_startup_overhead),
                percentage_reduction(warm_startup_overhead, prewarmed_startup_overhead),
                format_ms(prewarmed_startup_overhead),
                format_ms(isolate_wall),
            ));
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

        if let (Some(smallest), Some(largest)) =
            (self.transport_rtt.first(), self.transport_rtt.last())
        {
            guidance.push(format!(
                "Execution-transport RTT over the stdio bridge rises from {} at {} bytes to {} at {} bytes. That gives later work a direct transport floor to compare against the larger startup and import phases.",
                format_ms(smallest.stats.mean_ms),
                smallest.payload_bytes,
                format_ms(largest.stats.mean_ms),
                largest.payload_bytes,
            ));
        }

        if let Some(noisiest) = self.scenarios.iter().max_by(|lhs, rhs| {
            lhs.wall_stats
                .stddev_ms
                .total_cmp(&rhs.wall_stats.stddev_ms)
        }) {
            guidance.push(format!(
                "Wall-time noise is now surfaced directly in the same artifact set: `{}` currently shows the largest spread at {} stddev over a {}-{} wall range, so future deltas on that path should be judged against stability as well as mean time.",
                noisiest.id,
                format_ms(noisiest.wall_stats.stddev_ms),
                format_ms(noisiest.wall_stats.min_ms),
                format_ms(noisiest.wall_stats.max_ms),
            ));
        }

        if let Some(heaviest) = self.scenarios.iter().max_by(|lhs, rhs| {
            lhs.resource_usage_stats
                .as_ref()
                .and_then(|stats| stats.rss_bytes.as_ref())
                .map(|stats| stats.mean)
                .unwrap_or(f64::NEG_INFINITY)
                .total_cmp(
                    &rhs.resource_usage_stats
                        .as_ref()
                        .and_then(|stats| stats.rss_bytes.as_ref())
                        .map(|stats| stats.mean)
                        .unwrap_or(f64::NEG_INFINITY),
                )
        }) {
            if let Some(rss_mean) = heaviest
                .resource_usage_stats
                .as_ref()
                .and_then(|stats| stats.rss_bytes.as_ref())
            {
                guidance.push(format!(
                    "Per-scenario resource reporting is now attached to the benchmark rows themselves: `{}` currently has the highest mean RSS at {} MiB, so import-path changes can now be judged for memory regressions without a separate memory-only pass.",
                    heaviest.id,
                    format_mib(bytes_to_mib(rss_mean.mean)),
                ));
            }
        }

        guidance.push(String::from(
            "No new PRD stories were added from this run. The measured hotspots already map cleanly onto existing follow-ons: `ARC-021C` for safe resolution and metadata caches, `ARC-021D` for builtin/polyfill prewarm, and `ARC-022` for broader warm-pool and timing-mitigation execution work.",
        ));

        guidance
    }

    fn scenario(&self, id: &str) -> Option<&BenchmarkScenarioReport> {
        self.scenarios.iter().find(|scenario| scenario.id == id)
    }

    fn json_artifact<'a>(
        &'a self,
        comparison: Option<&'a BenchmarkComparison>,
    ) -> JavascriptBenchmarkArtifact<'a> {
        JavascriptBenchmarkArtifact {
            artifact_version: BENCHMARK_ARTIFACT_VERSION,
            generated_at_unix_ms: self.generated_at_unix_ms,
            command: format!(
                "cargo run -p agent-os-execution --bin node-import-bench -- --iterations {} --warmup-iterations {}",
                self.config.iterations, self.config.warmup_iterations
            ),
            config: &self.config,
            host: &self.host,
            repo_root: &self.repo_root,
            summary: self.summary(),
            comparison,
            transport_rtt: self
                .transport_rtt
                .iter()
                .map(|transport| BenchmarkTransportRttArtifact {
                    channel: transport.channel,
                    payload_bytes: transport.payload_bytes,
                    samples_ms: &transport.samples_ms,
                    stats: &transport.stats,
                })
                .collect(),
            scenarios: self
                .scenarios
                .iter()
                .map(|scenario| BenchmarkScenarioArtifact {
                    id: scenario.id,
                    workload: scenario.workload,
                    runtime: scenario.runtime,
                    mode: scenario.mode,
                    description: scenario.description,
                    fixture: scenario.fixture,
                    compile_cache: scenario.compile_cache,
                    wall_samples_ms: &scenario.wall_samples_ms,
                    wall_stats: &scenario.wall_stats,
                    guest_import_samples_ms: scenario.guest_import_samples_ms.as_deref(),
                    guest_import_stats: scenario.guest_import_stats.as_ref(),
                    startup_overhead_samples_ms: scenario.startup_overhead_samples_ms.as_deref(),
                    startup_overhead_stats: scenario.startup_overhead_stats.as_ref(),
                    mean_startup_share_pct: scenario.mean_startup_share_pct(),
                    phase_samples_ms: &scenario.phase_samples_ms,
                    phase_stats: &scenario.phase_stats,
                    resource_usage_samples: scenario.resource_usage_samples.as_ref(),
                    resource_usage_stats: scenario.resource_usage_stats.as_ref(),
                })
                .collect(),
        }
    }

    fn summary(&self) -> BenchmarkSummaryArtifact<'_> {
        BenchmarkSummaryArtifact {
            scenario_count: self.scenarios.len(),
            recorded_samples_per_scenario: self.config.iterations,
            warmup_iterations: self.config.warmup_iterations,
            control_matrix: self.control_matrix(),
            slowest_wall_scenario: self.slowest_scenario_by(|scenario| scenario.wall_stats.mean_ms),
            slowest_guest_import_scenario: self.slowest_scenario_by(|scenario| {
                scenario
                    .guest_import_stats
                    .as_ref()
                    .map(|stats| stats.mean_ms)
                    .unwrap_or(f64::NEG_INFINITY)
            }),
            highest_startup_share_scenario: self.scenarios.iter().max_by(|lhs, rhs| {
                lhs.mean_startup_share_pct()
                    .unwrap_or(f64::NEG_INFINITY)
                    .total_cmp(&rhs.mean_startup_share_pct().unwrap_or(f64::NEG_INFINITY))
            }),
            hotspot_rankings: self.hotspot_rankings(),
            guidance_lines: self.guidance_lines(),
        }
    }

    fn control_matrix(&self) -> Vec<BenchmarkControlMatrixArtifact<'_>> {
        let mut rows = Vec::new();
        let mut row_indexes = BTreeMap::new();

        for scenario in &self.scenarios {
            let row_index = *row_indexes.entry(scenario.workload).or_insert_with(|| {
                rows.push(BenchmarkControlMatrixArtifact {
                    workload: scenario.workload,
                    runtimes: Vec::new(),
                    modes: Vec::new(),
                    scenario_ids: Vec::new(),
                });
                rows.len() - 1
            });
            let row = &mut rows[row_index];
            push_unique_label(&mut row.runtimes, scenario.runtime);
            push_unique_label(&mut row.modes, scenario.mode);
            row.scenario_ids.push(scenario.id);
        }

        rows
    }

    fn slowest_scenario_by(
        &self,
        value: impl Fn(&BenchmarkScenarioReport) -> f64,
    ) -> Option<&BenchmarkScenarioReport> {
        self.scenarios
            .iter()
            .max_by(|lhs, rhs| value(lhs).total_cmp(&value(rhs)))
    }

    fn hotspot_rankings(&self) -> Vec<BenchmarkHotspotRankingArtifact<'_>> {
        HOTSPOT_METRICS
            .iter()
            .map(|metric| {
                let mut ranked_scenarios = self
                    .scenarios
                    .iter()
                    .filter_map(|scenario| {
                        (metric.value)(scenario).map(|value| BenchmarkHotspotScenarioArtifact {
                            rank: 0,
                            id: scenario.id,
                            workload: scenario.workload,
                            runtime: scenario.runtime,
                            mode: scenario.mode,
                            value,
                        })
                    })
                    .collect::<Vec<_>>();
                ranked_scenarios.sort_by(|lhs, rhs| {
                    rhs.value
                        .total_cmp(&lhs.value)
                        .then_with(|| lhs.id.cmp(rhs.id))
                });
                for (index, scenario) in ranked_scenarios.iter_mut().enumerate() {
                    scenario.rank = index + 1;
                }

                BenchmarkHotspotRankingArtifact {
                    metric: metric.metric,
                    label: metric.label,
                    dimension: metric.dimension,
                    unit: metric.unit,
                    ranked_scenarios,
                    scenarios_without_metric: self
                        .scenarios
                        .iter()
                        .filter(|scenario| (metric.value)(scenario).is_none())
                        .map(|scenario| scenario.id)
                        .collect(),
                }
            })
            .collect()
    }
}

impl BenchmarkScenarioReport {
    fn mean_startup_share_pct(&self) -> Option<f64> {
        let startup_mean = self.startup_overhead_stats.as_ref()?.mean_ms;
        let wall_mean = self.wall_stats.mean_ms;
        if wall_mean <= 0.0 {
            Some(0.0)
        } else {
            Some((startup_mean / wall_mean) * 100.0)
        }
    }

    fn wall_range_ms(&self) -> f64 {
        self.wall_stats.max_ms - self.wall_stats.min_ms
    }
}

impl BenchmarkResourceUsage<Vec<f64>> {
    fn push_sample(&mut self, sample: &BenchmarkResourceUsage<f64>) {
        push_optional_sample(&mut self.rss_bytes, sample.rss_bytes);
        push_optional_sample(&mut self.heap_used_bytes, sample.heap_used_bytes);
        push_optional_sample(&mut self.cpu_user_us, sample.cpu_user_us);
        push_optional_sample(&mut self.cpu_system_us, sample.cpu_system_us);
        push_optional_sample(&mut self.cpu_total_us, sample.cpu_total_us);
    }

    fn into_populated(self) -> Option<Self> {
        (!self.is_empty()).then_some(self)
    }
}

impl<T> BenchmarkResourceUsage<T> {
    fn is_empty(&self) -> bool {
        self.rss_bytes.is_none()
            && self.heap_used_bytes.is_none()
            && self.cpu_user_us.is_none()
            && self.cpu_system_us.is_none()
            && self.cpu_total_us.is_none()
    }
}

impl BenchmarkComparison {
    fn from_reports(
        current: &JavascriptBenchmarkReport,
        baseline_path: &Path,
        baseline: &StoredBenchmarkArtifact,
    ) -> Self {
        let baseline_path =
            fs::canonicalize(baseline_path).unwrap_or_else(|_| baseline_path.to_path_buf());
        let baseline_by_id = baseline
            .scenarios
            .iter()
            .map(|scenario| (scenario.id.as_str(), scenario))
            .collect::<BTreeMap<_, _>>();

        let mut scenario_deltas = Vec::new();
        let mut scenarios_missing_from_baseline = Vec::new();

        for scenario in &current.scenarios {
            if let Some(baseline_scenario) = baseline_by_id.get(scenario.id) {
                scenario_deltas.push(BenchmarkScenarioDelta {
                    id: scenario.id.to_owned(),
                    description: scenario.description.to_owned(),
                    wall_mean_ms: BenchmarkMetricDelta::from_means(
                        baseline_scenario.wall_stats.mean_ms,
                        scenario.wall_stats.mean_ms,
                    ),
                    guest_import_mean_ms: match (
                        baseline_scenario.guest_import_stats.as_ref(),
                        scenario.guest_import_stats.as_ref(),
                    ) {
                        (Some(baseline_stats), Some(current_stats)) => {
                            Some(BenchmarkMetricDelta::from_means(
                                baseline_stats.mean_ms,
                                current_stats.mean_ms,
                            ))
                        }
                        _ => None,
                    },
                    startup_overhead_mean_ms: match (
                        baseline_scenario.startup_overhead_stats.as_ref(),
                        scenario.startup_overhead_stats.as_ref(),
                    ) {
                        (Some(baseline_stats), Some(current_stats)) => {
                            Some(BenchmarkMetricDelta::from_means(
                                baseline_stats.mean_ms,
                                current_stats.mean_ms,
                            ))
                        }
                        _ => None,
                    },
                    phase_mean_ms: match (
                        baseline_scenario.phase_stats.as_ref(),
                        Some(&scenario.phase_stats),
                    ) {
                        (Some(baseline_phase), Some(current_phase)) => {
                            Some(BenchmarkScenarioPhases {
                                context_setup_ms: BenchmarkMetricDelta::from_means(
                                    baseline_phase.context_setup_ms.mean_ms,
                                    current_phase.context_setup_ms.mean_ms,
                                ),
                                startup_ms: BenchmarkMetricDelta::from_means(
                                    baseline_phase.startup_ms.mean_ms,
                                    current_phase.startup_ms.mean_ms,
                                ),
                                guest_execution_ms: match (
                                    baseline_phase.guest_execution_ms.as_ref(),
                                    current_phase.guest_execution_ms.as_ref(),
                                ) {
                                    (Some(baseline_stats), Some(current_stats)) => {
                                        Some(BenchmarkMetricDelta::from_means(
                                            baseline_stats.mean_ms,
                                            current_stats.mean_ms,
                                        ))
                                    }
                                    _ => None,
                                },
                                completion_ms: BenchmarkMetricDelta::from_means(
                                    baseline_phase.completion_ms.mean_ms,
                                    current_phase.completion_ms.mean_ms,
                                ),
                            })
                        }
                        _ => None,
                    },
                });
            } else {
                scenarios_missing_from_baseline.push(scenario.id.to_owned());
            }
        }

        let current_ids = current
            .scenarios
            .iter()
            .map(|scenario| (scenario.id, ()))
            .collect::<BTreeMap<_, _>>();
        let baseline_only_scenarios = baseline
            .scenarios
            .iter()
            .filter_map(|scenario| {
                (!current_ids.contains_key(scenario.id.as_str())).then(|| scenario.id.clone())
            })
            .collect::<Vec<_>>();

        let largest_wall_improvement = scenario_deltas
            .iter()
            .filter(|scenario| scenario.wall_mean_ms.delta_ms < 0.0)
            .min_by(|lhs, rhs| {
                lhs.wall_mean_ms
                    .delta_ms
                    .total_cmp(&rhs.wall_mean_ms.delta_ms)
            })
            .map(BenchmarkDeltaHighlight::from_wall_delta);
        let largest_wall_regression = scenario_deltas
            .iter()
            .filter(|scenario| scenario.wall_mean_ms.delta_ms > 0.0)
            .max_by(|lhs, rhs| {
                lhs.wall_mean_ms
                    .delta_ms
                    .total_cmp(&rhs.wall_mean_ms.delta_ms)
            })
            .map(BenchmarkDeltaHighlight::from_wall_delta);

        Self {
            baseline: BenchmarkComparisonBaseline {
                artifact_version: baseline.artifact_version,
                generated_at_unix_ms: baseline.generated_at_unix_ms,
                path: baseline_path,
            },
            summary: BenchmarkComparisonSummary {
                compared_scenario_count: scenario_deltas.len(),
                largest_wall_improvement,
                largest_wall_regression,
            },
            scenario_deltas,
            scenarios_missing_from_baseline,
            baseline_only_scenarios,
        }
    }
}

impl BenchmarkDeltaHighlight {
    fn from_wall_delta(delta: &BenchmarkScenarioDelta) -> Self {
        Self {
            id: delta.id.clone(),
            delta_ms: delta.wall_mean_ms.delta_ms,
            delta_pct: delta.wall_mean_ms.delta_pct,
        }
    }
}

impl BenchmarkMetricDelta {
    fn from_means(baseline_ms: f64, current_ms: f64) -> Self {
        let delta_ms = current_ms - baseline_ms;
        let delta_pct = if baseline_ms <= 0.0 {
            0.0
        } else {
            (delta_ms / baseline_ms) * 100.0
        };

        Self {
            baseline_ms,
            current_ms,
            delta_ms,
            delta_pct,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JavascriptBenchmarkArtifactPaths {
    pub markdown_path: PathBuf,
    pub json_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JavascriptBenchmarkRunOutput {
    pub artifact_paths: JavascriptBenchmarkArtifactPaths,
    pub resumed_stage_count: usize,
}

#[derive(Debug, Serialize)]
struct JavascriptBenchmarkArtifact<'a> {
    artifact_version: u32,
    generated_at_unix_ms: u128,
    command: String,
    config: &'a JavascriptBenchmarkConfig,
    host: &'a BenchmarkHost,
    repo_root: &'a Path,
    summary: BenchmarkSummaryArtifact<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    comparison: Option<&'a BenchmarkComparison>,
    transport_rtt: Vec<BenchmarkTransportRttArtifact<'a>>,
    scenarios: Vec<BenchmarkScenarioArtifact<'a>>,
}

#[derive(Debug, Serialize)]
struct BenchmarkSummaryArtifact<'a> {
    scenario_count: usize,
    recorded_samples_per_scenario: usize,
    warmup_iterations: usize,
    control_matrix: Vec<BenchmarkControlMatrixArtifact<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    slowest_wall_scenario: Option<&'a BenchmarkScenarioReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    slowest_guest_import_scenario: Option<&'a BenchmarkScenarioReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    highest_startup_share_scenario: Option<&'a BenchmarkScenarioReport>,
    hotspot_rankings: Vec<BenchmarkHotspotRankingArtifact<'a>>,
    guidance_lines: Vec<String>,
}

#[derive(Debug, Serialize)]
struct BenchmarkScenarioArtifact<'a> {
    id: &'static str,
    workload: &'static str,
    runtime: &'static str,
    mode: &'static str,
    description: &'static str,
    fixture: &'static str,
    compile_cache: &'static str,
    wall_samples_ms: &'a [f64],
    wall_stats: &'a BenchmarkStats,
    #[serde(skip_serializing_if = "Option::is_none")]
    guest_import_samples_ms: Option<&'a [f64]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    guest_import_stats: Option<&'a BenchmarkStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    startup_overhead_samples_ms: Option<&'a [f64]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    startup_overhead_stats: Option<&'a BenchmarkStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mean_startup_share_pct: Option<f64>,
    phase_samples_ms: &'a BenchmarkScenarioPhases<Vec<f64>>,
    phase_stats: &'a BenchmarkScenarioPhases<BenchmarkStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resource_usage_samples: Option<&'a BenchmarkResourceUsage<Vec<f64>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resource_usage_stats: Option<&'a BenchmarkResourceUsage<BenchmarkDistributionStats>>,
}

#[derive(Debug, Serialize)]
struct BenchmarkControlMatrixArtifact<'a> {
    workload: &'a str,
    runtimes: Vec<&'a str>,
    modes: Vec<&'a str>,
    scenario_ids: Vec<&'a str>,
}

#[derive(Debug, Serialize)]
struct BenchmarkTransportRttArtifact<'a> {
    channel: &'static str,
    payload_bytes: usize,
    samples_ms: &'a [f64],
    stats: &'a BenchmarkStats,
}

#[derive(Debug, Serialize)]
struct BenchmarkHotspotRankingArtifact<'a> {
    metric: &'static str,
    label: &'static str,
    dimension: &'static str,
    unit: &'static str,
    ranked_scenarios: Vec<BenchmarkHotspotScenarioArtifact<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    scenarios_without_metric: Vec<&'a str>,
}

#[derive(Debug, Serialize)]
struct BenchmarkHotspotScenarioArtifact<'a> {
    rank: usize,
    id: &'a str,
    workload: &'a str,
    runtime: &'a str,
    mode: &'a str,
    value: f64,
}

struct HotspotMetricDefinition {
    metric: &'static str,
    label: &'static str,
    dimension: &'static str,
    unit: &'static str,
    value: fn(&BenchmarkScenarioReport) -> Option<f64>,
}

const HOTSPOT_METRICS: [HotspotMetricDefinition; 13] = [
    HotspotMetricDefinition {
        metric: "wall_mean_ms",
        label: "Wall Time",
        dimension: "time",
        unit: "ms",
        value: hotspot_wall_mean_ms,
    },
    HotspotMetricDefinition {
        metric: "wall_stddev_ms",
        label: "Wall Time Stddev",
        dimension: "stability",
        unit: "ms",
        value: hotspot_wall_stddev_ms,
    },
    HotspotMetricDefinition {
        metric: "wall_range_ms",
        label: "Wall Time Range",
        dimension: "stability",
        unit: "ms",
        value: hotspot_wall_range_ms,
    },
    HotspotMetricDefinition {
        metric: "guest_import_mean_ms",
        label: "Guest Import Time",
        dimension: "time",
        unit: "ms",
        value: hotspot_guest_import_mean_ms,
    },
    HotspotMetricDefinition {
        metric: "startup_overhead_mean_ms",
        label: "Startup Overhead",
        dimension: "time",
        unit: "ms",
        value: hotspot_startup_overhead_mean_ms,
    },
    HotspotMetricDefinition {
        metric: "context_setup_mean_ms",
        label: "Context Setup Phase",
        dimension: "time",
        unit: "ms",
        value: hotspot_context_setup_mean_ms,
    },
    HotspotMetricDefinition {
        metric: "startup_phase_mean_ms",
        label: "Runtime Startup Phase",
        dimension: "time",
        unit: "ms",
        value: hotspot_startup_phase_mean_ms,
    },
    HotspotMetricDefinition {
        metric: "guest_execution_mean_ms",
        label: "Guest Execution Phase",
        dimension: "time",
        unit: "ms",
        value: hotspot_guest_execution_mean_ms,
    },
    HotspotMetricDefinition {
        metric: "completion_mean_ms",
        label: "Completion/Stdio Phase",
        dimension: "time",
        unit: "ms",
        value: hotspot_completion_mean_ms,
    },
    HotspotMetricDefinition {
        metric: "startup_share_pct",
        label: "Startup Share Of Wall",
        dimension: "share",
        unit: "pct",
        value: hotspot_startup_share_pct,
    },
    HotspotMetricDefinition {
        metric: "rss_mean_mib",
        label: "RSS",
        dimension: "memory",
        unit: "MiB",
        value: hotspot_rss_mean_mib,
    },
    HotspotMetricDefinition {
        metric: "heap_mean_mib",
        label: "Heap Used",
        dimension: "memory",
        unit: "MiB",
        value: hotspot_heap_mean_mib,
    },
    HotspotMetricDefinition {
        metric: "cpu_total_mean_ms",
        label: "Total CPU",
        dimension: "cpu",
        unit: "ms",
        value: hotspot_total_cpu_mean_ms,
    },
];

#[derive(Debug)]
pub enum JavascriptBenchmarkError {
    InvalidConfig(&'static str),
    InvalidWorkspaceRoot(PathBuf),
    InvalidBaselineReport {
        path: PathBuf,
        message: String,
    },
    Io(std::io::Error),
    Utf8(std::string::FromUtf8Error),
    Execution(JavascriptExecutionError),
    NodeVersion(std::io::Error),
    MissingBenchmarkMetric(&'static str),
    InvalidBenchmarkMetric {
        scenario: &'static str,
        raw_value: String,
    },
    TransportProbeTimeout {
        payload_bytes: usize,
    },
    TransportProbeExited {
        exit_code: i32,
        stderr: String,
    },
    InvalidTransportProbeResponse {
        payload_bytes: usize,
        expected: String,
        actual: String,
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
            Self::InvalidBaselineReport { path, message } => {
                write!(
                    f,
                    "failed to parse benchmark baseline artifact {}: {message}",
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
            Self::TransportProbeTimeout { payload_bytes } => {
                write!(
                    f,
                    "transport probe timed out waiting for {payload_bytes}-byte round-trip"
                )
            }
            Self::TransportProbeExited { exit_code, stderr } => {
                write!(f, "transport probe exited with code {exit_code}: {stderr}")
            }
            Self::InvalidTransportProbeResponse {
                payload_bytes,
                expected,
                actual,
            } => write!(
                f,
                "transport probe returned unexpected payload for {payload_bytes}-byte round-trip: expected {expected:?}, got {actual:?}"
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

impl From<serde_json::Error> for JavascriptBenchmarkError {
    fn from(err: serde_json::Error) -> Self {
        Self::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, err))
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
    let transport_rtt = measure_transport_rtt(&workspace, config)?;

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
        transport_rtt,
        scenarios,
    })
}

fn benchmark_artifact_dir(repo_root: &Path) -> PathBuf {
    repo_root.join(BENCHMARK_ARTIFACT_DIR)
}

fn benchmark_run_state_path(artifact_dir: &Path) -> PathBuf {
    artifact_dir.join(BENCHMARK_RUN_STATE_FILE)
}

fn load_benchmark_run_state(
    state_path: &Path,
    config: &JavascriptBenchmarkConfig,
    host: &BenchmarkHost,
    repo_root: &Path,
    definitions: &[ScenarioDefinition],
) -> Result<StoredBenchmarkRunState, JavascriptBenchmarkError> {
    match fs::read_to_string(state_path) {
        Ok(raw) => match serde_json::from_str::<StoredBenchmarkRunState>(&raw) {
            Ok(state) if state.is_compatible(config, host, repo_root) => {
                Ok(state.sanitized(definitions))
            }
            Ok(_) | Err(_) => Ok(StoredBenchmarkRunState::new(config, host, repo_root)),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok(StoredBenchmarkRunState::new(config, host, repo_root))
        }
        Err(err) => Err(JavascriptBenchmarkError::Io(err)),
    }
}

fn persist_benchmark_run_state(
    state_path: &Path,
    state: &StoredBenchmarkRunState,
) -> Result<(), JavascriptBenchmarkError> {
    write_string_atomic(state_path, &serde_json::to_string_pretty(state)?)
}

fn write_string_atomic(path: &Path, contents: &str) -> Result<(), JavascriptBenchmarkError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let temp_path = path.with_file_name(format!(
        ".{}.tmp-{}-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("artifact"),
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    fs::write(&temp_path, contents)?;
    if let Err(err) = fs::rename(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(JavascriptBenchmarkError::Io(err));
    }

    Ok(())
}

fn remove_file_if_exists(path: &Path) -> Result<(), JavascriptBenchmarkError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(JavascriptBenchmarkError::Io(err)),
    }
}

fn current_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[derive(Debug, Clone, Copy)]
struct ScenarioDefinition {
    id: &'static str,
    workload: &'static str,
    runtime: ScenarioRuntime,
    mode: ScenarioMode,
    description: &'static str,
    fixture: &'static str,
    entrypoint: &'static str,
    compile_cache: CompileCacheStrategy,
    engine_reuse: EngineReuseStrategy,
    expect_import_metric: bool,
    env: ScenarioEnvironment,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EngineReuseStrategy {
    FreshPerSample,
    SharedAcrossScenario,
    SharedContextAcrossScenario,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScenarioEnvironment {
    None,
    ProjectedWorkspaceNodeModules,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScenarioRuntime {
    NativeExecution,
    HostNode,
}

impl ScenarioRuntime {
    fn label(self) -> &'static str {
        match self {
            Self::NativeExecution => "native-execution",
            Self::HostNode => "host-node",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScenarioMode {
    BaselineControl,
    TrueColdStart,
    NewSessionReplay,
    SameSessionReplay,
    SameEngineReplay,
    HostControl,
}

impl ScenarioMode {
    fn label(self) -> &'static str {
        match self {
            Self::BaselineControl => "baseline-control",
            Self::TrueColdStart => "true-cold-start",
            Self::NewSessionReplay => "new-session-replay",
            Self::SameSessionReplay => "same-session-replay",
            Self::SameEngineReplay => "same-engine-replay",
            Self::HostControl => "host-control",
        }
    }
}

#[derive(Debug)]
struct SampleMeasurement {
    wall_ms: f64,
    guest_import_ms: Option<f64>,
    context_setup_ms: f64,
    startup_ms: f64,
    completion_ms: f64,
    resource_usage: Option<BenchmarkResourceUsage<f64>>,
}

#[derive(Debug)]
struct BenchmarkWorkspace {
    root: PathBuf,
    repo_root: PathBuf,
}

#[derive(Debug, Deserialize)]
struct StoredBenchmarkArtifact {
    artifact_version: u32,
    generated_at_unix_ms: u128,
    scenarios: Vec<StoredBenchmarkScenario>,
}

#[derive(Debug, Deserialize)]
struct StoredBenchmarkScenario {
    id: String,
    wall_stats: BenchmarkStats,
    #[serde(default)]
    guest_import_stats: Option<BenchmarkStats>,
    #[serde(default)]
    startup_overhead_stats: Option<BenchmarkStats>,
    #[serde(default)]
    phase_stats: Option<BenchmarkScenarioPhases<BenchmarkStats>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredBenchmarkRunHost {
    node_binary: String,
    node_version: String,
    os: String,
    arch: String,
    logical_cpus: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct StoredBenchmarkRunState {
    artifact_version: u32,
    config: JavascriptBenchmarkConfig,
    host: StoredBenchmarkRunHost,
    repo_root: PathBuf,
    #[serde(default)]
    transport_rtt: Option<Vec<StoredBenchmarkTransportRttReport>>,
    #[serde(default)]
    scenarios: Vec<StoredBenchmarkScenarioReport>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct StoredBenchmarkTransportRttReport {
    payload_bytes: usize,
    samples_ms: Vec<f64>,
    stats: BenchmarkStats,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct StoredBenchmarkScenarioReport {
    id: String,
    wall_samples_ms: Vec<f64>,
    wall_stats: BenchmarkStats,
    #[serde(default)]
    guest_import_samples_ms: Option<Vec<f64>>,
    #[serde(default)]
    guest_import_stats: Option<BenchmarkStats>,
    #[serde(default)]
    startup_overhead_samples_ms: Option<Vec<f64>>,
    #[serde(default)]
    startup_overhead_stats: Option<BenchmarkStats>,
    phase_samples_ms: BenchmarkScenarioPhases<Vec<f64>>,
    phase_stats: BenchmarkScenarioPhases<BenchmarkStats>,
    #[serde(default)]
    resource_usage_samples: Option<BenchmarkResourceUsage<Vec<f64>>>,
    #[serde(default)]
    resource_usage_stats: Option<BenchmarkResourceUsage<BenchmarkDistributionStats>>,
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
        write_benchmark_workspace(&root, repo_root)?;
        Ok(Self {
            root,
            repo_root: repo_root.to_path_buf(),
        })
    }
}

impl Drop for BenchmarkWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl StoredBenchmarkRunHost {
    fn from_host(host: &BenchmarkHost) -> Self {
        Self {
            node_binary: host.node_binary.clone(),
            node_version: host.node_version.clone(),
            os: host.os.to_owned(),
            arch: host.arch.to_owned(),
            logical_cpus: host.logical_cpus,
        }
    }

    fn matches_host(&self, host: &BenchmarkHost) -> bool {
        self.node_binary == host.node_binary
            && self.node_version == host.node_version
            && self.os == host.os
            && self.arch == host.arch
            && self.logical_cpus == host.logical_cpus
    }
}

impl StoredBenchmarkRunState {
    fn new(config: &JavascriptBenchmarkConfig, host: &BenchmarkHost, repo_root: &Path) -> Self {
        Self {
            artifact_version: BENCHMARK_ARTIFACT_VERSION,
            config: config.clone(),
            host: StoredBenchmarkRunHost::from_host(host),
            repo_root: repo_root.to_path_buf(),
            transport_rtt: None,
            scenarios: Vec::new(),
        }
    }

    fn is_compatible(
        &self,
        config: &JavascriptBenchmarkConfig,
        host: &BenchmarkHost,
        repo_root: &Path,
    ) -> bool {
        self.artifact_version == BENCHMARK_ARTIFACT_VERSION
            && self.config == *config
            && self.host.matches_host(host)
            && self.repo_root == repo_root
    }

    fn sanitized(mut self, definitions: &[ScenarioDefinition]) -> Self {
        if let Some(transport_rtt) = &self.transport_rtt {
            let payloads = transport_rtt
                .iter()
                .map(|report| report.payload_bytes)
                .collect::<Vec<_>>();
            if payloads != TRANSPORT_RTT_PAYLOAD_BYTES {
                self.transport_rtt = None;
            }
        }

        let mut scenarios_by_id = self
            .scenarios
            .into_iter()
            .map(|scenario| (scenario.id.clone(), scenario))
            .collect::<BTreeMap<_, _>>();
        self.scenarios = definitions
            .iter()
            .filter_map(|definition| scenarios_by_id.remove(definition.id))
            .collect();
        self
    }

    fn resumed_stage_count(&self, definitions: &[ScenarioDefinition]) -> usize {
        usize::from(self.transport_rtt.is_some())
            + definitions
                .iter()
                .filter(|definition| self.has_scenario(definition.id))
                .count()
    }

    fn has_scenario(&self, id: &str) -> bool {
        self.scenarios.iter().any(|scenario| scenario.id == id)
    }

    fn record_transport_rtt(&mut self, transport_rtt: &[BenchmarkTransportRttReport]) {
        self.transport_rtt = Some(
            transport_rtt
                .iter()
                .map(StoredBenchmarkTransportRttReport::from_report)
                .collect(),
        );
    }

    fn record_scenario(&mut self, scenario: &BenchmarkScenarioReport) {
        self.scenarios.retain(|stored| stored.id != scenario.id);
        self.scenarios
            .push(StoredBenchmarkScenarioReport::from_report(scenario));
    }

    fn to_report(
        &self,
        config: &JavascriptBenchmarkConfig,
        host: &BenchmarkHost,
        repo_root: &Path,
        definitions: &[ScenarioDefinition],
    ) -> JavascriptBenchmarkReport {
        let scenarios_by_id = self
            .scenarios
            .iter()
            .map(|scenario| (scenario.id.as_str(), scenario))
            .collect::<BTreeMap<_, _>>();

        JavascriptBenchmarkReport {
            generated_at_unix_ms: current_unix_ms(),
            config: config.clone(),
            host: host.clone(),
            repo_root: repo_root.to_path_buf(),
            transport_rtt: self
                .transport_rtt
                .clone()
                .unwrap_or_default()
                .into_iter()
                .map(StoredBenchmarkTransportRttReport::into_report)
                .collect(),
            scenarios: definitions
                .iter()
                .filter_map(|definition| {
                    scenarios_by_id
                        .get(definition.id)
                        .map(|scenario| scenario.to_report(*definition))
                })
                .collect(),
        }
    }
}

impl StoredBenchmarkTransportRttReport {
    fn from_report(report: &BenchmarkTransportRttReport) -> Self {
        Self {
            payload_bytes: report.payload_bytes,
            samples_ms: report.samples_ms.clone(),
            stats: report.stats.clone(),
        }
    }

    fn into_report(self) -> BenchmarkTransportRttReport {
        BenchmarkTransportRttReport {
            channel: TRANSPORT_RTT_CHANNEL,
            payload_bytes: self.payload_bytes,
            samples_ms: self.samples_ms,
            stats: self.stats,
        }
    }
}

impl StoredBenchmarkScenarioReport {
    fn from_report(report: &BenchmarkScenarioReport) -> Self {
        Self {
            id: report.id.to_owned(),
            wall_samples_ms: report.wall_samples_ms.clone(),
            wall_stats: report.wall_stats.clone(),
            guest_import_samples_ms: report.guest_import_samples_ms.clone(),
            guest_import_stats: report.guest_import_stats.clone(),
            startup_overhead_samples_ms: report.startup_overhead_samples_ms.clone(),
            startup_overhead_stats: report.startup_overhead_stats.clone(),
            phase_samples_ms: report.phase_samples_ms.clone(),
            phase_stats: report.phase_stats.clone(),
            resource_usage_samples: report.resource_usage_samples.clone(),
            resource_usage_stats: report.resource_usage_stats.clone(),
        }
    }

    fn to_report(&self, definition: ScenarioDefinition) -> BenchmarkScenarioReport {
        BenchmarkScenarioReport {
            id: definition.id,
            workload: definition.workload,
            runtime: definition.runtime.label(),
            mode: definition.mode.label(),
            description: definition.description,
            fixture: definition.fixture,
            compile_cache: definition.compile_cache.label(),
            wall_samples_ms: self.wall_samples_ms.clone(),
            wall_stats: self.wall_stats.clone(),
            guest_import_samples_ms: self.guest_import_samples_ms.clone(),
            guest_import_stats: self.guest_import_stats.clone(),
            startup_overhead_samples_ms: self.startup_overhead_samples_ms.clone(),
            startup_overhead_stats: self.startup_overhead_stats.clone(),
            phase_samples_ms: self.phase_samples_ms.clone(),
            phase_stats: self.phase_stats.clone(),
            resource_usage_samples: self.resource_usage_samples.clone(),
            resource_usage_stats: self.resource_usage_stats.clone(),
        }
    }
}

pub fn run_javascript_benchmarks_with_recovery(
    config: &JavascriptBenchmarkConfig,
    baseline_path: Option<&Path>,
) -> Result<JavascriptBenchmarkRunOutput, JavascriptBenchmarkError> {
    if config.iterations == 0 {
        return Err(JavascriptBenchmarkError::InvalidConfig(
            "iterations must be greater than zero",
        ));
    }

    let repo_root = workspace_root()?;
    let host = benchmark_host()?;
    let artifact_dir = benchmark_artifact_dir(&repo_root);
    let workspace = BenchmarkWorkspace::create(&repo_root)?;
    let (report, resumed_stage_count, state_path) = orchestrate_javascript_benchmark_report(
        config,
        &repo_root,
        &host,
        &artifact_dir,
        || measure_transport_rtt(&workspace, config),
        |scenario| run_scenario(&workspace, config, scenario),
    )?;
    let comparison = baseline_path
        .map(|path| report.compare_to_baseline_path(path))
        .transpose()?;
    let artifact_paths =
        report.write_artifacts_with_comparison(&artifact_dir, comparison.as_ref())?;
    remove_file_if_exists(&state_path)?;

    Ok(JavascriptBenchmarkRunOutput {
        artifact_paths,
        resumed_stage_count,
    })
}

fn orchestrate_javascript_benchmark_report<MeasureTransport, RunScenario>(
    config: &JavascriptBenchmarkConfig,
    repo_root: &Path,
    host: &BenchmarkHost,
    artifact_dir: &Path,
    mut measure_transport: MeasureTransport,
    mut run_scenario: RunScenario,
) -> Result<(JavascriptBenchmarkReport, usize, PathBuf), JavascriptBenchmarkError>
where
    MeasureTransport: FnMut() -> Result<Vec<BenchmarkTransportRttReport>, JavascriptBenchmarkError>,
    RunScenario:
        FnMut(ScenarioDefinition) -> Result<BenchmarkScenarioReport, JavascriptBenchmarkError>,
{
    if config.iterations == 0 {
        return Err(JavascriptBenchmarkError::InvalidConfig(
            "iterations must be greater than zero",
        ));
    }

    fs::create_dir_all(artifact_dir)?;

    let definitions = benchmark_scenarios();
    let state_path = benchmark_run_state_path(artifact_dir);
    let mut state = load_benchmark_run_state(&state_path, config, host, repo_root, &definitions)?;
    let resumed_stage_count = state.resumed_stage_count(&definitions);

    if state.transport_rtt.is_none() {
        let transport_rtt = measure_transport()?;
        state.record_transport_rtt(&transport_rtt);
        persist_benchmark_run_state(&state_path, &state)?;
    }

    for definition in definitions {
        if state.has_scenario(definition.id) {
            continue;
        }

        let scenario = run_scenario(definition)?;
        state.record_scenario(&scenario);
        persist_benchmark_run_state(&state_path, &state)?;
    }

    Ok((
        state.to_report(config, host, repo_root, &benchmark_scenarios()),
        resumed_stage_count,
        state_path,
    ))
}

fn benchmark_scenarios() -> [ScenarioDefinition; 21] {
    [
        ScenarioDefinition {
            id: "isolate-startup",
            workload: "startup-floor",
            runtime: ScenarioRuntime::NativeExecution,
            mode: ScenarioMode::BaselineControl,
            description: "Minimal guest with no extra imports. Measures the current startup floor for create-context plus node process bootstrap.",
            fixture: "empty entrypoint",
            entrypoint: "./bench/isolate-startup.mjs",
            compile_cache: CompileCacheStrategy::Disabled,
            engine_reuse: EngineReuseStrategy::FreshPerSample,
            expect_import_metric: false,
            env: ScenarioEnvironment::None,
        },
        ScenarioDefinition {
            id: "prewarmed-isolate-startup",
            workload: "startup-floor",
            runtime: ScenarioRuntime::NativeExecution,
            mode: ScenarioMode::SameEngineReplay,
            description: "Minimal guest after a priming pass while one execution engine keeps materialized assets and builtin/polyfill prewarm state alive, isolating the hot startup floor from import work.",
            fixture: "empty entrypoint",
            entrypoint: "./bench/isolate-startup.mjs",
            compile_cache: CompileCacheStrategy::Primed,
            engine_reuse: EngineReuseStrategy::SharedAcrossScenario,
            expect_import_metric: false,
            env: ScenarioEnvironment::None,
        },
        ScenarioDefinition {
            id: "cold-local-import",
            workload: "local-import",
            runtime: ScenarioRuntime::NativeExecution,
            mode: ScenarioMode::TrueColdStart,
            description: "Cold import of a repo-local ESM graph that simulates layered application modules without compile-cache reuse.",
            fixture: "24-module local ESM graph",
            entrypoint: "./bench/cold-local-import.mjs",
            compile_cache: CompileCacheStrategy::Disabled,
            engine_reuse: EngineReuseStrategy::FreshPerSample,
            expect_import_metric: true,
            env: ScenarioEnvironment::None,
        },
        ScenarioDefinition {
            id: "warm-local-import",
            workload: "local-import",
            runtime: ScenarioRuntime::NativeExecution,
            mode: ScenarioMode::NewSessionReplay,
            description: "Warm import of the same local ESM graph after a compile-cache priming pass in an earlier isolate.",
            fixture: "24-module local ESM graph",
            entrypoint: "./bench/warm-local-import.mjs",
            compile_cache: CompileCacheStrategy::Primed,
            engine_reuse: EngineReuseStrategy::FreshPerSample,
            expect_import_metric: true,
            env: ScenarioEnvironment::None,
        },
        ScenarioDefinition {
            id: "same-context-local-import",
            workload: "local-import",
            runtime: ScenarioRuntime::NativeExecution,
            mode: ScenarioMode::SameSessionReplay,
            description: "Warm import of the same local ESM graph by replaying executions against one reused JavaScript context after a compile-cache priming pass.",
            fixture: "24-module local ESM graph",
            entrypoint: "./bench/warm-local-import.mjs",
            compile_cache: CompileCacheStrategy::Primed,
            engine_reuse: EngineReuseStrategy::SharedContextAcrossScenario,
            expect_import_metric: true,
            env: ScenarioEnvironment::None,
        },
        ScenarioDefinition {
            id: "prewarmed-local-import",
            workload: "local-import",
            runtime: ScenarioRuntime::NativeExecution,
            mode: ScenarioMode::SameEngineReplay,
            description: "Warm import of the same local ESM graph after compile-cache priming while one execution engine keeps materialized assets and builtin/polyfill prewarm state alive.",
            fixture: "24-module local ESM graph",
            entrypoint: "./bench/warm-local-import.mjs",
            compile_cache: CompileCacheStrategy::Primed,
            engine_reuse: EngineReuseStrategy::SharedAcrossScenario,
            expect_import_metric: true,
            env: ScenarioEnvironment::None,
        },
        ScenarioDefinition {
            id: "host-local-import",
            workload: "local-import",
            runtime: ScenarioRuntime::HostNode,
            mode: ScenarioMode::HostControl,
            description: "Direct host-Node control for the same local ESM graph so later runs can separate native executor overhead from guest import work.",
            fixture: "24-module local ESM graph",
            entrypoint: "./bench/cold-local-import.mjs",
            compile_cache: CompileCacheStrategy::Disabled,
            engine_reuse: EngineReuseStrategy::FreshPerSample,
            expect_import_metric: true,
            env: ScenarioEnvironment::None,
        },
        ScenarioDefinition {
            id: "builtin-import",
            workload: "builtin-import",
            runtime: ScenarioRuntime::NativeExecution,
            mode: ScenarioMode::TrueColdStart,
            description: "Import of the common builtin path used by the wrappers and polyfill-adjacent bootstrap code.",
            fixture: "node:path + node:url + node:fs/promises",
            entrypoint: "./bench/builtin-import.mjs",
            compile_cache: CompileCacheStrategy::Disabled,
            engine_reuse: EngineReuseStrategy::FreshPerSample,
            expect_import_metric: true,
            env: ScenarioEnvironment::None,
        },
        ScenarioDefinition {
            id: "hot-builtin-stream-import",
            workload: "builtin-hot-import",
            runtime: ScenarioRuntime::NativeExecution,
            mode: ScenarioMode::SameEngineReplay,
            description: "Hot single-import microbench for `node:stream` after a priming pass inside one reused execution engine.",
            fixture: "node:stream",
            entrypoint: "./bench/hot-builtin-stream-import.mjs",
            compile_cache: CompileCacheStrategy::Primed,
            engine_reuse: EngineReuseStrategy::SharedAcrossScenario,
            expect_import_metric: true,
            env: ScenarioEnvironment::None,
        },
        ScenarioDefinition {
            id: "hot-builtin-stream-web-import",
            workload: "builtin-hot-import",
            runtime: ScenarioRuntime::NativeExecution,
            mode: ScenarioMode::SameEngineReplay,
            description: "Hot single-import microbench for `node:stream/web` after a priming pass inside one reused execution engine.",
            fixture: "node:stream/web",
            entrypoint: "./bench/hot-builtin-stream-web-import.mjs",
            compile_cache: CompileCacheStrategy::Primed,
            engine_reuse: EngineReuseStrategy::SharedAcrossScenario,
            expect_import_metric: true,
            env: ScenarioEnvironment::None,
        },
        ScenarioDefinition {
            id: "hot-builtin-crypto-import",
            workload: "builtin-hot-import",
            runtime: ScenarioRuntime::NativeExecution,
            mode: ScenarioMode::SameEngineReplay,
            description: "Hot single-import microbench for `node:crypto` after a priming pass inside one reused execution engine.",
            fixture: "node:crypto",
            entrypoint: "./bench/hot-builtin-crypto-import.mjs",
            compile_cache: CompileCacheStrategy::Primed,
            engine_reuse: EngineReuseStrategy::SharedAcrossScenario,
            expect_import_metric: true,
            env: ScenarioEnvironment::None,
        },
        ScenarioDefinition {
            id: "hot-builtin-zlib-import",
            workload: "builtin-hot-import",
            runtime: ScenarioRuntime::NativeExecution,
            mode: ScenarioMode::SameEngineReplay,
            description: "Hot single-import microbench for `node:zlib` after a priming pass inside one reused execution engine.",
            fixture: "node:zlib",
            entrypoint: "./bench/hot-builtin-zlib-import.mjs",
            compile_cache: CompileCacheStrategy::Primed,
            engine_reuse: EngineReuseStrategy::SharedAcrossScenario,
            expect_import_metric: true,
            env: ScenarioEnvironment::None,
        },
        ScenarioDefinition {
            id: "hot-builtin-assert-import",
            workload: "builtin-hot-import",
            runtime: ScenarioRuntime::NativeExecution,
            mode: ScenarioMode::SameEngineReplay,
            description: "Hot single-import microbench for `node:assert/strict` after a priming pass inside one reused execution engine.",
            fixture: "node:assert/strict",
            entrypoint: "./bench/hot-builtin-assert-import.mjs",
            compile_cache: CompileCacheStrategy::Primed,
            engine_reuse: EngineReuseStrategy::SharedAcrossScenario,
            expect_import_metric: true,
            env: ScenarioEnvironment::None,
        },
        ScenarioDefinition {
            id: "hot-builtin-url-import",
            workload: "builtin-hot-import",
            runtime: ScenarioRuntime::NativeExecution,
            mode: ScenarioMode::SameEngineReplay,
            description: "Hot single-import microbench for `node:url` after a priming pass inside one reused execution engine.",
            fixture: "node:url",
            entrypoint: "./bench/hot-builtin-url-import.mjs",
            compile_cache: CompileCacheStrategy::Primed,
            engine_reuse: EngineReuseStrategy::SharedAcrossScenario,
            expect_import_metric: true,
            env: ScenarioEnvironment::None,
        },
        ScenarioDefinition {
            id: "hot-projected-package-file-import",
            workload: "projected-package-hot-import",
            runtime: ScenarioRuntime::HostNode,
            mode: ScenarioMode::SameEngineReplay,
            description: "Hot projected-package single-import microbench for the TypeScript compiler file with compile cache and projected-source manifest reuse enabled across repeated contexts.",
            fixture: "projected TypeScript compiler file",
            entrypoint: "./bench/hot-projected-package-file-import.mjs",
            compile_cache: CompileCacheStrategy::Primed,
            engine_reuse: EngineReuseStrategy::FreshPerSample,
            expect_import_metric: true,
            env: ScenarioEnvironment::ProjectedWorkspaceNodeModules,
        },
        ScenarioDefinition {
            id: "large-package-import",
            workload: "large-package-import",
            runtime: ScenarioRuntime::HostNode,
            mode: ScenarioMode::TrueColdStart,
            description: "Cold import of the real-world `typescript` package from the workspace root `node_modules` tree.",
            fixture: "typescript",
            entrypoint: "./bench/large-package-import.mjs",
            compile_cache: CompileCacheStrategy::Disabled,
            engine_reuse: EngineReuseStrategy::FreshPerSample,
            expect_import_metric: true,
            env: ScenarioEnvironment::None,
        },
        ScenarioDefinition {
            id: "projected-package-import",
            workload: "projected-package-import",
            runtime: ScenarioRuntime::HostNode,
            mode: ScenarioMode::HostControl,
            description: "Projected-package guest-path import of TypeScript with compile cache and projected-source manifest reuse enabled across repeated contexts.",
            fixture: "projected TypeScript guest-path import",
            entrypoint: "./bench/projected-package-import.mjs",
            compile_cache: CompileCacheStrategy::Primed,
            engine_reuse: EngineReuseStrategy::FreshPerSample,
            expect_import_metric: true,
            env: ScenarioEnvironment::ProjectedWorkspaceNodeModules,
        },
        ScenarioDefinition {
            id: "pdf-lib-startup",
            workload: "pdf-lib-startup",
            runtime: ScenarioRuntime::HostNode,
            mode: ScenarioMode::HostControl,
            description: "Cold import of `pdf-lib` plus representative document setup that creates a PDF page and embeds a standard font.",
            fixture: "pdf-lib document creation",
            entrypoint: "./bench/pdf-lib-startup.mjs",
            compile_cache: CompileCacheStrategy::Disabled,
            engine_reuse: EngineReuseStrategy::FreshPerSample,
            expect_import_metric: true,
            env: ScenarioEnvironment::None,
        },
        ScenarioDefinition {
            id: "jszip-startup",
            workload: "jszip-startup",
            runtime: ScenarioRuntime::HostNode,
            mode: ScenarioMode::HostControl,
            description: "Cold import of `jszip` plus representative archive staging that builds a nested archive structure.",
            fixture: "jszip archive staging",
            entrypoint: "./bench/jszip-startup.mjs",
            compile_cache: CompileCacheStrategy::Disabled,
            engine_reuse: EngineReuseStrategy::FreshPerSample,
            expect_import_metric: true,
            env: ScenarioEnvironment::None,
        },
        ScenarioDefinition {
            id: "jszip-end-to-end",
            workload: "jszip-end-to-end",
            runtime: ScenarioRuntime::HostNode,
            mode: ScenarioMode::HostControl,
            description: "Cold import of `jszip` plus a full compressed archive roundtrip that writes, compresses, reloads, and validates nested archive contents.",
            fixture: "jszip end-to-end archive roundtrip",
            entrypoint: "./bench/jszip-end-to-end.mjs",
            compile_cache: CompileCacheStrategy::Disabled,
            engine_reuse: EngineReuseStrategy::FreshPerSample,
            expect_import_metric: true,
            env: ScenarioEnvironment::None,
        },
        ScenarioDefinition {
            id: "jszip-repeated-session-compressed",
            workload: "jszip-repeated-session-compressed",
            runtime: ScenarioRuntime::HostNode,
            mode: ScenarioMode::HostControl,
            description: "Repeated-session `jszip` workload after a compile-cache priming pass that compresses and reloads a nested archive in each fresh isolate.",
            fixture: "jszip compressed archive roundtrip",
            entrypoint: "./bench/jszip-repeated-session-compressed.mjs",
            compile_cache: CompileCacheStrategy::Primed,
            engine_reuse: EngineReuseStrategy::FreshPerSample,
            expect_import_metric: true,
            env: ScenarioEnvironment::None,
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
    let mut shared_engine = match scenario.engine_reuse {
        EngineReuseStrategy::FreshPerSample => None,
        EngineReuseStrategy::SharedAcrossScenario
        | EngineReuseStrategy::SharedContextAcrossScenario => {
            Some(JavascriptExecutionEngine::default())
        }
    };
    let mut shared_context = None;

    if scenario.compile_cache == CompileCacheStrategy::Primed {
        run_sample(
            workspace,
            &scenario,
            Some(compile_cache_root.clone()),
            shared_engine.as_mut(),
            &mut shared_context,
        )?;
    }

    for _ in 0..config.warmup_iterations {
        run_sample(
            workspace,
            &scenario,
            compile_cache_root_for_strategy(scenario.compile_cache, &compile_cache_root),
            shared_engine.as_mut(),
            &mut shared_context,
        )?;
    }

    let mut wall_samples_ms = Vec::with_capacity(config.iterations);
    let mut guest_import_samples_ms = if scenario.expect_import_metric {
        Some(Vec::with_capacity(config.iterations))
    } else {
        None
    };
    let mut context_setup_samples_ms = Vec::with_capacity(config.iterations);
    let mut startup_samples_ms = Vec::with_capacity(config.iterations);
    let mut completion_samples_ms = Vec::with_capacity(config.iterations);
    let mut resource_usage_samples = BenchmarkResourceUsage::<Vec<f64>>::default();

    for _ in 0..config.iterations {
        let sample = run_sample(
            workspace,
            &scenario,
            compile_cache_root_for_strategy(scenario.compile_cache, &compile_cache_root),
            shared_engine.as_mut(),
            &mut shared_context,
        )?;
        wall_samples_ms.push(sample.wall_ms);
        context_setup_samples_ms.push(sample.context_setup_ms);
        startup_samples_ms.push(sample.startup_ms);
        completion_samples_ms.push(sample.completion_ms);

        if let (Some(import_ms), Some(samples)) =
            (sample.guest_import_ms, guest_import_samples_ms.as_mut())
        {
            samples.push(import_ms);
        }
        if let Some(resource_usage) = sample.resource_usage.as_ref() {
            resource_usage_samples.push_sample(resource_usage);
        }
    }

    let startup_overhead_samples_ms = guest_import_samples_ms.as_ref().map(|guest_samples| {
        context_setup_samples_ms
            .iter()
            .zip(startup_samples_ms.iter())
            .zip(completion_samples_ms.iter())
            .zip(guest_samples.iter())
            .map(|(((context_ms, startup_ms), completion_ms), _guest_ms)| {
                context_ms + startup_ms + completion_ms
            })
            .collect::<Vec<_>>()
    });

    let phase_samples_ms = BenchmarkScenarioPhases {
        context_setup_ms: context_setup_samples_ms,
        startup_ms: startup_samples_ms,
        guest_execution_ms: guest_import_samples_ms.clone(),
        completion_ms: completion_samples_ms,
    };
    let resource_usage_samples = resource_usage_samples.into_populated();

    Ok(BenchmarkScenarioReport {
        id: scenario.id,
        workload: scenario.workload,
        runtime: scenario.runtime.label(),
        mode: scenario.mode.label(),
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
        phase_stats: BenchmarkScenarioPhases {
            context_setup_ms: compute_stats(&phase_samples_ms.context_setup_ms),
            startup_ms: compute_stats(&phase_samples_ms.startup_ms),
            guest_execution_ms: phase_samples_ms
                .guest_execution_ms
                .as_ref()
                .map(|samples| compute_stats(samples)),
            completion_ms: compute_stats(&phase_samples_ms.completion_ms),
        },
        resource_usage_stats: resource_usage_samples
            .as_ref()
            .and_then(compute_resource_usage_stats),
        wall_samples_ms,
        guest_import_samples_ms,
        startup_overhead_samples_ms,
        phase_samples_ms,
        resource_usage_samples,
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
    shared_engine: Option<&mut JavascriptExecutionEngine>,
    shared_context: &mut Option<crate::JavascriptContext>,
) -> Result<SampleMeasurement, JavascriptBenchmarkError> {
    match scenario.runtime {
        ScenarioRuntime::NativeExecution => run_native_sample(
            workspace,
            scenario,
            compile_cache_root,
            shared_engine,
            shared_context,
        ),
        ScenarioRuntime::HostNode => run_host_node_sample(workspace, scenario),
    }
}

fn run_native_sample(
    workspace: &BenchmarkWorkspace,
    scenario: &ScenarioDefinition,
    compile_cache_root: Option<PathBuf>,
    shared_engine: Option<&mut JavascriptExecutionEngine>,
    shared_context: &mut Option<crate::JavascriptContext>,
) -> Result<SampleMeasurement, JavascriptBenchmarkError> {
    let mut fresh_engine = JavascriptExecutionEngine::default();
    let engine = shared_engine.unwrap_or(&mut fresh_engine);
    let context_started_at = Instant::now();
    let (context, context_setup_ms) = match scenario.engine_reuse {
        EngineReuseStrategy::SharedContextAcrossScenario => {
            if let Some(context) = shared_context.as_ref() {
                (context.clone(), 0.0)
            } else {
                let context = engine.create_context(CreateJavascriptContextRequest {
                    vm_id: String::from("vm-bench"),
                    bootstrap_module: None,
                    compile_cache_root,
                });
                let context_setup_ms = context_started_at.elapsed().as_secs_f64() * 1000.0;
                *shared_context = Some(context.clone());
                (context, context_setup_ms)
            }
        }
        _ => {
            let context = engine.create_context(CreateJavascriptContextRequest {
                vm_id: String::from("vm-bench"),
                bootstrap_module: None,
                compile_cache_root,
            });
            let context_setup_ms = context_started_at.elapsed().as_secs_f64() * 1000.0;
            (context, context_setup_ms)
        }
    };

    let startup_started_at = Instant::now();
    let execution = engine.start_execution(StartJavascriptExecutionRequest {
        vm_id: String::from("vm-bench"),
        context_id: context.context_id,
        argv: vec![String::from(scenario.entrypoint)],
        env: scenario_env(workspace, scenario),
        cwd: workspace.root.clone(),
        inline_code: None,
    })?;
    let startup_ms = startup_started_at.elapsed().as_secs_f64() * 1000.0;

    let completion_started_at = Instant::now();
    let result = execution.wait()?;
    let completion_total_ms = completion_started_at.elapsed().as_secs_f64() * 1000.0;
    let stdout = String::from_utf8(result.stdout)?;
    let stderr = String::from_utf8(result.stderr)?;

    if result.exit_code != 0 {
        return Err(JavascriptBenchmarkError::NonZeroExit {
            scenario: scenario.id,
            exit_code: result.exit_code,
            stderr,
        });
    }

    let parsed_metrics =
        parse_benchmark_metrics(scenario.id, &stdout, scenario.expect_import_metric)?;
    let guest_import_ms = parsed_metrics.import_ms;
    let completion_ms = guest_import_ms
        .map(|guest_ms| saturating_delta_ms(completion_total_ms, guest_ms))
        .unwrap_or(completion_total_ms);
    let wall_ms = context_setup_ms + startup_ms + completion_total_ms;

    Ok(SampleMeasurement {
        wall_ms,
        guest_import_ms,
        context_setup_ms,
        startup_ms,
        completion_ms,
        resource_usage: parsed_metrics.resource_usage,
    })
}

fn run_host_node_sample(
    workspace: &BenchmarkWorkspace,
    scenario: &ScenarioDefinition,
) -> Result<SampleMeasurement, JavascriptBenchmarkError> {
    let started_at = Instant::now();
    let output = Command::new(crate::host_node::node_binary())
        .arg(scenario.entrypoint)
        .current_dir(&workspace.root)
        .envs(scenario_env(workspace, scenario))
        .output()?;
    let wall_ms = started_at.elapsed().as_secs_f64() * 1000.0;
    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(output.stderr)?;

    if !output.status.success() {
        return Err(JavascriptBenchmarkError::NonZeroExit {
            scenario: scenario.id,
            exit_code: output.status.code().unwrap_or(-1),
            stderr,
        });
    }

    let parsed_metrics =
        parse_benchmark_metrics(scenario.id, &stdout, scenario.expect_import_metric)?;
    let guest_import_ms = parsed_metrics.import_ms;
    let startup_ms = guest_import_ms
        .map(|guest_ms| saturating_delta_ms(wall_ms, guest_ms))
        .unwrap_or(wall_ms);

    Ok(SampleMeasurement {
        wall_ms,
        guest_import_ms,
        context_setup_ms: 0.0,
        startup_ms,
        completion_ms: 0.0,
        resource_usage: parsed_metrics.resource_usage,
    })
}

fn scenario_env(
    workspace: &BenchmarkWorkspace,
    scenario: &ScenarioDefinition,
) -> BTreeMap<String, String> {
    match scenario.env {
        ScenarioEnvironment::None => BTreeMap::new(),
        ScenarioEnvironment::ProjectedWorkspaceNodeModules => {
            let projected_node_modules = workspace.repo_root.join("node_modules");
            let projected_node_modules_json =
                serde_json::to_string(&vec![projected_node_modules.display().to_string()])
                    .expect("serialize projected node_modules read path");
            let guest_path_mappings = serde_json::json!([{
                "guestPath": "/root/node_modules",
                "hostPath": projected_node_modules.display().to_string(),
            }])
            .to_string();

            BTreeMap::from([
                (
                    String::from("AGENT_OS_EXTRA_FS_READ_PATHS"),
                    projected_node_modules_json,
                ),
                (
                    String::from("AGENT_OS_GUEST_PATH_MAPPINGS"),
                    guest_path_mappings,
                ),
            ])
        }
    }
}

fn measure_transport_rtt(
    workspace: &BenchmarkWorkspace,
    config: &JavascriptBenchmarkConfig,
) -> Result<Vec<BenchmarkTransportRttReport>, JavascriptBenchmarkError> {
    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-transport"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let mut execution = engine.start_execution(StartJavascriptExecutionRequest {
        vm_id: String::from("vm-transport"),
        context_id: context.context_id,
        argv: vec![String::from("./bench/transport-echo.mjs")],
        env: BTreeMap::from([(String::from("AGENT_OS_KEEP_STDIN_OPEN"), String::from("1"))]),
        cwd: workspace.root.clone(),
        inline_code: None,
    })?;

    let mut stdout_buffer = String::new();
    let mut stderr_buffer = String::new();
    let mut reports = Vec::with_capacity(TRANSPORT_RTT_PAYLOAD_BYTES.len());

    for payload_bytes in TRANSPORT_RTT_PAYLOAD_BYTES {
        for warmup_index in 0..config.warmup_iterations {
            let label = format!("warmup-{}-{warmup_index}", payload_bytes);
            measure_transport_roundtrip(
                &mut execution,
                payload_bytes,
                &label,
                &mut stdout_buffer,
                &mut stderr_buffer,
            )?;
        }

        let mut samples_ms = Vec::with_capacity(config.iterations);
        for iteration in 0..config.iterations {
            let label = format!("measure-{}-{iteration}", payload_bytes);
            samples_ms.push(measure_transport_roundtrip(
                &mut execution,
                payload_bytes,
                &label,
                &mut stdout_buffer,
                &mut stderr_buffer,
            )?);
        }

        reports.push(BenchmarkTransportRttReport {
            channel: TRANSPORT_RTT_CHANNEL,
            payload_bytes,
            stats: compute_stats(&samples_ms),
            samples_ms,
        });
    }

    execution.close_stdin()?;
    let result = execution.wait()?;
    if result.exit_code != 0 {
        stderr_buffer.push_str(&String::from_utf8(result.stderr)?);
        return Err(JavascriptBenchmarkError::TransportProbeExited {
            exit_code: result.exit_code,
            stderr: stderr_buffer,
        });
    }

    Ok(reports)
}

fn measure_transport_roundtrip(
    execution: &mut crate::JavascriptExecution,
    payload_bytes: usize,
    label: &str,
    stdout_buffer: &mut String,
    stderr_buffer: &mut String,
) -> Result<f64, JavascriptBenchmarkError> {
    let payload = transport_probe_payload(payload_bytes, label);
    let expected_line = format!("{payload}\n");
    let started_at = Instant::now();
    execution.write_stdin(expected_line.as_bytes())?;

    loop {
        if let Some(line) = take_complete_line(stdout_buffer) {
            if line == payload {
                return Ok(started_at.elapsed().as_secs_f64() * 1000.0);
            }
            return Err(JavascriptBenchmarkError::InvalidTransportProbeResponse {
                payload_bytes,
                expected: payload,
                actual: line,
            });
        }

        match execution.poll_event_blocking(TRANSPORT_POLL_TIMEOUT)? {
            Some(crate::JavascriptExecutionEvent::Stdout(chunk)) => {
                stdout_buffer.push_str(&String::from_utf8(chunk)?);
            }
            Some(crate::JavascriptExecutionEvent::Stderr(chunk)) => {
                stderr_buffer.push_str(&String::from_utf8(chunk)?);
            }
            Some(crate::JavascriptExecutionEvent::SyncRpcRequest(request)) => {
                return Err(JavascriptBenchmarkError::Execution(
                    JavascriptExecutionError::PendingSyncRpcRequest(request.id),
                ));
            }
            Some(crate::JavascriptExecutionEvent::SignalState { .. }) => {}
            Some(crate::JavascriptExecutionEvent::Exited(exit_code)) => {
                return Err(JavascriptBenchmarkError::TransportProbeExited {
                    exit_code,
                    stderr: stderr_buffer.clone(),
                });
            }
            None => {
                return Err(JavascriptBenchmarkError::TransportProbeTimeout { payload_bytes });
            }
        }
    }
}

fn transport_probe_payload(payload_bytes: usize, label: &str) -> String {
    if payload_bytes == 0 {
        return format!("transport:{label}:");
    }

    let header = format!("transport:{label}:");
    let fill_len = payload_bytes.saturating_sub(header.len());
    format!("{header}{}", "x".repeat(fill_len))
}

fn take_complete_line(buffer: &mut String) -> Option<String> {
    let newline_index = buffer.find('\n')?;
    let line = buffer[..newline_index].trim_end_matches('\r').to_owned();
    buffer.drain(..=newline_index);
    Some(line)
}

#[derive(Debug, Default, Deserialize)]
struct ParsedBenchmarkMetrics {
    #[serde(default)]
    import_ms: Option<f64>,
    #[serde(default)]
    resource_usage: Option<BenchmarkResourceUsage<f64>>,
}

fn parse_benchmark_metrics(
    scenario_id: &'static str,
    stdout: &str,
    expect_import_metric: bool,
) -> Result<ParsedBenchmarkMetrics, JavascriptBenchmarkError> {
    let raw_value = stdout
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix(BENCHMARK_MARKER_PREFIX))
        .ok_or(JavascriptBenchmarkError::MissingBenchmarkMetric(
            scenario_id,
        ))?
        .trim();

    if let Ok(parsed) = serde_json::from_str::<ParsedBenchmarkMetrics>(raw_value) {
        let has_resource_usage = match parsed.resource_usage.as_ref() {
            Some(resource_usage) => !resource_usage.is_empty(),
            None => false,
        };
        if parsed.import_ms.is_some() || has_resource_usage {
            if expect_import_metric && parsed.import_ms.is_none() {
                return Err(JavascriptBenchmarkError::MissingBenchmarkMetric(
                    scenario_id,
                ));
            }
            return Ok(parsed);
        }
    }

    raw_value
        .parse::<f64>()
        .map(|import_ms| ParsedBenchmarkMetrics {
            import_ms: Some(import_ms),
            resource_usage: None,
        })
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

fn load_benchmark_artifact(
    baseline_path: &Path,
) -> Result<StoredBenchmarkArtifact, JavascriptBenchmarkError> {
    let raw = fs::read_to_string(baseline_path)?;
    serde_json::from_str(&raw).map_err(|err| JavascriptBenchmarkError::InvalidBaselineReport {
        path: baseline_path.to_path_buf(),
        message: err.to_string(),
    })
}

fn benchmark_host() -> Result<BenchmarkHost, JavascriptBenchmarkError> {
    let node_binary = crate::host_node::node_binary();
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

fn write_benchmark_workspace(
    root: &Path,
    repo_root: &Path,
) -> Result<(), JavascriptBenchmarkError> {
    fs::create_dir_all(root.join("bench"))?;
    fs::create_dir_all(root.join("bench/local-graph"))?;
    let host_node_modules = repo_root.join("node_modules");
    let workspace_node_modules = root.join("node_modules");
    if host_node_modules.exists() && !workspace_node_modules.exists() {
        std::os::unix::fs::symlink(&host_node_modules, &workspace_node_modules)?;
    }
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
        root.join("bench/benchmark-metrics.mjs"),
        benchmark_metrics_module_source(),
    )?;

    fs::write(
        root.join("bench/isolate-startup.mjs"),
        resource_only_entrypoint_source("console.log('isolate-ready');"),
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
        timed_entrypoint_source(
            "const [pathMod, fsMod, urlMod] = await Promise.all([\n  import('node:path'),\n  import('node:fs/promises'),\n  import('node:url'),\n]);\nif (typeof pathMod.basename !== 'function' || typeof fsMod.readFile !== 'function' || typeof urlMod.pathToFileURL !== 'function') {\n  throw new Error('builtin import fixture did not load expected exports');\n}",
        ),
    )?;
    fs::write(
        root.join("bench/hot-builtin-stream-import.mjs"),
        single_import_entrypoint_source(
            "node:stream",
            "typeof imported.Readable === 'function'",
            "node:stream import did not expose Readable",
        ),
    )?;
    fs::write(
        root.join("bench/hot-builtin-stream-web-import.mjs"),
        single_import_entrypoint_source(
            "node:stream/web",
            "typeof imported.ReadableStream === 'function'",
            "node:stream/web import did not expose ReadableStream",
        ),
    )?;
    fs::write(
        root.join("bench/hot-builtin-crypto-import.mjs"),
        single_import_entrypoint_source(
            "node:crypto",
            "typeof imported.createHash === 'function'",
            "node:crypto import did not expose createHash",
        ),
    )?;
    fs::write(
        root.join("bench/hot-builtin-zlib-import.mjs"),
        single_import_entrypoint_source(
            "node:zlib",
            "typeof imported.gzipSync === 'function'",
            "node:zlib import did not expose gzipSync",
        ),
    )?;
    fs::write(
        root.join("bench/hot-builtin-assert-import.mjs"),
        single_import_entrypoint_source(
            "node:assert/strict",
            "typeof imported.strictEqual === 'function'",
            "node:assert/strict import did not expose strictEqual",
        ),
    )?;
    fs::write(
        root.join("bench/hot-builtin-url-import.mjs"),
        single_import_entrypoint_source(
            "node:url",
            "typeof imported.pathToFileURL === 'function'",
            "node:url import did not expose pathToFileURL",
        ),
    )?;
    fs::write(
        root.join("bench/large-package-import.mjs"),
        timed_entrypoint_source(
            "const typescript = await import('typescript');\nif (typeof typescript.transpileModule !== 'function') {\n  throw new Error('typescript import did not expose transpileModule');\n}",
        ),
    )?;
    fs::write(
        root.join("bench/hot-projected-package-file-import.mjs"),
        projected_package_file_import_entrypoint_source(),
    )?;
    fs::write(
        root.join("bench/projected-package-import.mjs"),
        projected_package_import_entrypoint_source(),
    )?;
    fs::write(
        root.join("bench/pdf-lib-startup.mjs"),
        pdf_lib_startup_entrypoint_source(),
    )?;
    fs::write(
        root.join("bench/jszip-startup.mjs"),
        jszip_startup_entrypoint_source(),
    )?;
    fs::write(
        root.join("bench/jszip-end-to-end.mjs"),
        jszip_end_to_end_entrypoint_source(),
    )?;
    fs::write(
        root.join("bench/jszip-repeated-session-compressed.mjs"),
        jszip_repeated_session_compressed_entrypoint_source(),
    )?;
    fs::write(
        root.join("bench/transport-echo.mjs"),
        "process.stdin.setEncoding('utf8');\nlet buffered = '';\nconst flushLines = () => {\n  let newlineIndex = buffered.indexOf('\\n');\n  while (newlineIndex >= 0) {\n    const line = buffered.slice(0, newlineIndex).replace(/\\r$/, '');\n    buffered = buffered.slice(newlineIndex + 1);\n    process.stdout.write(line);\n    newlineIndex = buffered.indexOf('\\n');\n  }\n};\nprocess.stdin.on('data', (chunk) => {\n  buffered += chunk;\n  flushLines();\n});\nprocess.stdin.on('end', () => {\n  if (buffered.length > 0) {\n    process.stdout.write(buffered.replace(/\\r$/, ''));\n  }\n});\n",
    )?;

    Ok(())
}

fn local_import_entrypoint_source(final_value: usize) -> String {
    timed_entrypoint_source(&format!(
        "const graph = await import('./local-graph/root.mjs');\nif (graph.value !== {final_value} || graph.expected !== {final_value}) {{\n  throw new Error(`local graph import returned ${{\n    graph.value\n  }} instead of {final_value}`);\n}}"
    ))
}

fn single_import_entrypoint_source(
    specifier: &str,
    validation_expression: &str,
    error_message: &str,
) -> String {
    timed_entrypoint_source(&format!(
        "const imported = await import('{specifier}');\nif (!({validation_expression})) {{\n  throw new Error('{error_message}');\n}}"
    ))
}

fn projected_package_file_import_entrypoint_source() -> String {
    timed_entrypoint_source(
        "const typescriptModule = await import('../node_modules/typescript/lib/typescript.js');\nconst typescript = typescriptModule.default ?? typescriptModule;\nif (typeof typescript.transpileModule !== 'function') {\n  throw new Error('projected package file import did not expose transpileModule');\n}",
    )
}

fn projected_package_import_entrypoint_source() -> String {
    timed_entrypoint_source(
        "const typescriptModule = await import('../node_modules/typescript/lib/typescript.js');\nconst typescript = typescriptModule.default ?? typescriptModule;\nconst sourceFile = typescript.createSourceFile(\n  'bench.ts',\n  'const answer: number = 42;',\n  typescript.ScriptTarget.ES2022,\n  true,\n);\nif (\n  typeof typescript.transpileModule !== 'function' ||\n  typeof typescript.createSourceFile !== 'function' ||\n  !sourceFile ||\n  sourceFile.statements.length !== 1\n) {\n  throw new Error('projected package import did not expose TypeScript compiler APIs');\n}",
    )
}

fn pdf_lib_startup_entrypoint_source() -> String {
    timed_entrypoint_source(
        "const pdfLib = await import('pdf-lib');\nconst pdfDoc = await pdfLib.PDFDocument.create();\nconst page = pdfDoc.addPage([612, 792]);\nconst font = await pdfDoc.embedFont(pdfLib.StandardFonts.Helvetica);\npage.drawText('Agent OS pdf-lib benchmark', {\n  x: 50,\n  y: 750,\n  font,\n  size: 18,\n});\nif (pdfDoc.getPageCount() !== 1 || page.getSize().width !== 612) {\n  throw new Error('pdf-lib fixture did not create the expected document');\n}",
    )
}

fn jszip_startup_entrypoint_source() -> String {
    timed_entrypoint_source(
        "const jszipModule = await import('jszip');\nconst JSZip = jszipModule.default ?? jszipModule;\nconst zip = new JSZip();\nzip.file('README.txt', 'agent-os benchmark archive');\nconst notes = zip.folder('notes');\nif (!notes) {\n  throw new Error('jszip fixture failed to create nested folder');\n}\nnotes.file('todo.txt', 'benchmark staging payload');\nconst fileCount = Object.values(zip.files).filter((entry) => !entry.dir).length;\nif (typeof zip.generateAsync !== 'function' || fileCount !== 2) {\n  throw new Error('jszip fixture did not stage the expected archive');\n}",
    )
}

fn jszip_end_to_end_entrypoint_source() -> String {
    timed_entrypoint_source(
        "const jszipModule = await import('jszip');\nconst JSZip = jszipModule.default ?? jszipModule;\nconst zip = new JSZip();\nconst repeatedPayload = 'agent-os benchmark payload '.repeat(512);\nzip.file('README.txt', repeatedPayload);\nconst notes = zip.folder('notes');\nif (!notes) {\n  throw new Error('jszip end-to-end fixture failed to create notes folder');\n}\nnotes.file('todo.txt', 'complete the archive roundtrip');\nconst data = zip.folder('data');\nif (!data) {\n  throw new Error('jszip end-to-end fixture failed to create data folder');\n}\ndata.file('payload.json', JSON.stringify({\n  repeatedPayloadLength: repeatedPayload.length,\n  mode: 'cold-end-to-end',\n}));\nconst archiveBytes = await zip.generateAsync({\n  type: 'uint8array',\n  compression: 'DEFLATE',\n  compressionOptions: { level: 6 },\n});\nconst restored = await JSZip.loadAsync(archiveBytes);\nconst restoredFileCount = Object.values(restored.files).filter((entry) => !entry.dir).length;\nconst restoredReadme = await restored.file('README.txt')?.async('string');\nconst restoredTodo = await restored.file('notes/todo.txt')?.async('string');\nconst restoredPayload = await restored.file('data/payload.json')?.async('string');\nif (\n  archiveBytes.byteLength >= repeatedPayload.length ||\n  restoredFileCount !== 3 ||\n  restoredReadme !== repeatedPayload ||\n  restoredTodo !== 'complete the archive roundtrip' ||\n  !restoredPayload?.includes('cold-end-to-end')\n) {\n  throw new Error('jszip end-to-end fixture did not complete the compressed archive roundtrip');\n}",
    )
}

fn jszip_repeated_session_compressed_entrypoint_source() -> String {
    timed_entrypoint_source(
        "const jszipModule = await import('jszip');\nconst JSZip = jszipModule.default ?? jszipModule;\nconst zip = new JSZip();\nconst repeatedPayload = 'agent-os benchmark payload '.repeat(512);\nzip.file('README.txt', repeatedPayload);\nconst notes = zip.folder('notes');\nif (!notes) {\n  throw new Error('jszip repeated-session fixture failed to create notes folder');\n}\nnotes.file('todo.txt', 'repeat this session workload');\nconst data = zip.folder('data');\nif (!data) {\n  throw new Error('jszip repeated-session fixture failed to create data folder');\n}\ndata.file('payload.json', JSON.stringify({\n  repeatedPayloadLength: repeatedPayload.length,\n  repeatedSessions: true,\n}));\nconst archiveBytes = await zip.generateAsync({\n  type: 'uint8array',\n  compression: 'DEFLATE',\n  compressionOptions: { level: 6 },\n});\nconst restored = await JSZip.loadAsync(archiveBytes);\nconst restoredFileCount = Object.values(restored.files).filter((entry) => !entry.dir).length;\nconst restoredReadme = await restored.file('README.txt')?.async('string');\nconst restoredTodo = await restored.file('notes/todo.txt')?.async('string');\nif (\n  archiveBytes.byteLength >= repeatedPayload.length ||\n  restoredFileCount !== 3 ||\n  restoredReadme !== repeatedPayload ||\n  restoredTodo !== 'repeat this session workload'\n) {\n  throw new Error('jszip repeated-session fixture did not complete the compressed archive roundtrip');\n}",
    )
}

fn benchmark_metrics_module_source() -> String {
    format!(
        "const BENCHMARK_MARKER_PREFIX = '{BENCHMARK_MARKER_PREFIX}';\n\nexport function emitBenchmarkMetrics(importMs) {{\n  const memoryUsage = process.memoryUsage();\n  const resourceUsage = typeof process.resourceUsage === 'function'\n    ? process.resourceUsage()\n    : null;\n  const payload = {{\n    resource_usage: {{\n      rss_bytes: memoryUsage.rss,\n      heap_used_bytes: memoryUsage.heapUsed,\n      ...(resourceUsage\n        ? {{\n            cpu_user_us: resourceUsage.userCPUTime,\n            cpu_system_us: resourceUsage.systemCPUTime,\n            cpu_total_us: resourceUsage.userCPUTime + resourceUsage.systemCPUTime,\n          }}\n        : {{}}),\n    }},\n  }};\n\n  if (typeof importMs === 'number') {{\n    payload.import_ms = importMs;\n  }}\n\n  console.log(BENCHMARK_MARKER_PREFIX + JSON.stringify(payload));\n}}\n"
    )
}

fn resource_only_entrypoint_source(body: &str) -> String {
    format!(
        "import {{ emitBenchmarkMetrics }} from './benchmark-metrics.mjs';\n{body}\nemitBenchmarkMetrics();\n"
    )
}

fn timed_entrypoint_source(body: &str) -> String {
    format!(
        "import {{ performance }} from 'node:perf_hooks';\nimport {{ emitBenchmarkMetrics }} from './benchmark-metrics.mjs';\nconst started = performance.now();\n{body}\nemitBenchmarkMetrics(performance.now() - started);\n"
    )
}

fn local_graph_terminal_value() -> usize {
    let mut value = 1;

    for index in 1..LOCAL_GRAPH_MODULE_COUNT {
        value += index;
    }

    value
}

fn compute_distribution_stats(samples: &[f64]) -> BenchmarkDistributionStats {
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let mean = sorted.iter().sum::<f64>() / sorted.len() as f64;

    BenchmarkDistributionStats {
        mean,
        p50: percentile(&sorted, 50.0),
        p95: percentile(&sorted, 95.0),
        min: *sorted.first().unwrap_or(&0.0),
        max: *sorted.last().unwrap_or(&0.0),
        stddev: standard_deviation(&sorted, mean),
    }
}

fn compute_stats(samples: &[f64]) -> BenchmarkStats {
    let stats = compute_distribution_stats(samples);

    BenchmarkStats {
        mean_ms: stats.mean,
        p50_ms: stats.p50,
        p95_ms: stats.p95,
        min_ms: stats.min,
        max_ms: stats.max,
        stddev_ms: stats.stddev,
    }
}

fn compute_resource_usage_stats(
    samples: &BenchmarkResourceUsage<Vec<f64>>,
) -> Option<BenchmarkResourceUsage<BenchmarkDistributionStats>> {
    let stats = BenchmarkResourceUsage {
        rss_bytes: samples
            .rss_bytes
            .as_ref()
            .map(|samples| compute_distribution_stats(samples)),
        heap_used_bytes: samples
            .heap_used_bytes
            .as_ref()
            .map(|samples| compute_distribution_stats(samples)),
        cpu_user_us: samples
            .cpu_user_us
            .as_ref()
            .map(|samples| compute_distribution_stats(samples)),
        cpu_system_us: samples
            .cpu_system_us
            .as_ref()
            .map(|samples| compute_distribution_stats(samples)),
        cpu_total_us: samples
            .cpu_total_us
            .as_ref()
            .map(|samples| compute_distribution_stats(samples)),
    };

    (!stats.is_empty()).then_some(stats)
}

fn standard_deviation(samples: &[f64], mean: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }

    let variance = samples
        .iter()
        .map(|sample| {
            let delta = sample - mean;
            delta * delta
        })
        .sum::<f64>()
        / samples.len() as f64;

    variance.sqrt()
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

fn saturating_delta_ms(total_ms: f64, subtracted_ms: f64) -> f64 {
    (total_ms - subtracted_ms).max(0.0)
}

fn format_ms(value: f64) -> String {
    format!("{value:.2}")
}

fn format_hotspot_value(unit: &str, value: f64) -> String {
    match unit {
        "pct" => format!("{value:.1}%"),
        "MiB" => format_mib(value),
        _ => format_ms(value),
    }
}

fn format_sample_list(samples: &[f64]) -> String {
    format_scaled_sample_list(samples, std::convert::identity)
}

fn format_scaled_sample_list(samples: &[f64], scale: impl Fn(f64) -> f64) -> String {
    let mut formatted = String::from("[");

    for (index, sample) in samples.iter().enumerate() {
        if index > 0 {
            formatted.push_str(", ");
        }
        let _ = write!(&mut formatted, "{:.2}", scale(*sample));
    }

    formatted.push(']');
    formatted
}

fn format_mib(value: f64) -> String {
    format!("{value:.2}")
}

fn format_label_list(labels: &[&str]) -> String {
    labels
        .iter()
        .map(|label| format!("`{label}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_string_label_list(labels: &[&str]) -> String {
    labels
        .iter()
        .map(|label| format!("`{label}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn push_unique_label<'a>(labels: &mut Vec<&'a str>, value: &'a str) {
    if !labels.contains(&value) {
        labels.push(value);
    }
}

fn format_delta_ms(value: f64) -> String {
    format!("{value:+.2}")
}

fn format_delta_pct(value: f64) -> String {
    format!("{value:+.1}%")
}

fn push_optional_sample(samples: &mut Option<Vec<f64>>, value: Option<f64>) {
    if let Some(value) = value {
        samples.get_or_insert_with(Vec::new).push(value);
    }
}

fn bytes_to_mib(value: f64) -> f64 {
    value / (1024.0 * 1024.0)
}

fn micros_to_ms(value: f64) -> f64 {
    value / 1000.0
}

fn hotspot_wall_mean_ms(scenario: &BenchmarkScenarioReport) -> Option<f64> {
    Some(scenario.wall_stats.mean_ms)
}

fn hotspot_wall_stddev_ms(scenario: &BenchmarkScenarioReport) -> Option<f64> {
    Some(scenario.wall_stats.stddev_ms)
}

fn hotspot_wall_range_ms(scenario: &BenchmarkScenarioReport) -> Option<f64> {
    Some(scenario.wall_range_ms())
}

fn hotspot_guest_import_mean_ms(scenario: &BenchmarkScenarioReport) -> Option<f64> {
    scenario
        .guest_import_stats
        .as_ref()
        .map(|stats| stats.mean_ms)
}

fn hotspot_startup_overhead_mean_ms(scenario: &BenchmarkScenarioReport) -> Option<f64> {
    scenario
        .startup_overhead_stats
        .as_ref()
        .map(|stats| stats.mean_ms)
}

fn hotspot_context_setup_mean_ms(scenario: &BenchmarkScenarioReport) -> Option<f64> {
    Some(scenario.phase_stats.context_setup_ms.mean_ms)
}

fn hotspot_startup_phase_mean_ms(scenario: &BenchmarkScenarioReport) -> Option<f64> {
    Some(scenario.phase_stats.startup_ms.mean_ms)
}

fn hotspot_guest_execution_mean_ms(scenario: &BenchmarkScenarioReport) -> Option<f64> {
    scenario
        .phase_stats
        .guest_execution_ms
        .as_ref()
        .map(|stats| stats.mean_ms)
}

fn hotspot_completion_mean_ms(scenario: &BenchmarkScenarioReport) -> Option<f64> {
    Some(scenario.phase_stats.completion_ms.mean_ms)
}

fn hotspot_startup_share_pct(scenario: &BenchmarkScenarioReport) -> Option<f64> {
    scenario.mean_startup_share_pct()
}

fn hotspot_rss_mean_mib(scenario: &BenchmarkScenarioReport) -> Option<f64> {
    scenario
        .resource_usage_stats
        .as_ref()?
        .rss_bytes
        .as_ref()
        .map(|stats| bytes_to_mib(stats.mean))
}

fn hotspot_heap_mean_mib(scenario: &BenchmarkScenarioReport) -> Option<f64> {
    scenario
        .resource_usage_stats
        .as_ref()?
        .heap_used_bytes
        .as_ref()
        .map(|stats| bytes_to_mib(stats.mean))
}

fn hotspot_total_cpu_mean_ms(scenario: &BenchmarkScenarioReport) -> Option<f64> {
    scenario
        .resource_usage_stats
        .as_ref()?
        .cpu_total_us
        .as_ref()
        .map(|stats| micros_to_ms(stats.mean))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use tempfile::tempdir;

    fn synthetic_transport_reports() -> Vec<BenchmarkTransportRttReport> {
        TRANSPORT_RTT_PAYLOAD_BYTES
            .iter()
            .enumerate()
            .map(|(index, payload_bytes)| {
                let sample = index as f64 + 1.0;
                BenchmarkTransportRttReport {
                    channel: TRANSPORT_RTT_CHANNEL,
                    payload_bytes: *payload_bytes,
                    samples_ms: vec![sample],
                    stats: compute_stats(&[sample]),
                }
            })
            .collect()
    }

    fn synthetic_scenario_report(
        definition: ScenarioDefinition,
        wall_sample_ms: f64,
    ) -> BenchmarkScenarioReport {
        let context_setup_ms = wall_sample_ms / 5.0;
        let startup_ms = wall_sample_ms / 4.0;
        let guest_execution_ms = definition
            .expect_import_metric
            .then_some(wall_sample_ms / 3.0);
        let completion_ms =
            wall_sample_ms - context_setup_ms - startup_ms - guest_execution_ms.unwrap_or(0.0);
        let startup_overhead_ms = definition
            .expect_import_metric
            .then_some(context_setup_ms + startup_ms + completion_ms);
        let resource_usage_samples = BenchmarkResourceUsage {
            rss_bytes: Some(vec![64.0 * 1024.0 * 1024.0]),
            heap_used_bytes: Some(vec![12.0 * 1024.0 * 1024.0]),
            cpu_user_us: None,
            cpu_system_us: None,
            cpu_total_us: Some(vec![wall_sample_ms * 1000.0]),
        };

        BenchmarkScenarioReport {
            id: definition.id,
            workload: definition.workload,
            runtime: definition.runtime.label(),
            mode: definition.mode.label(),
            description: definition.description,
            fixture: definition.fixture,
            compile_cache: definition.compile_cache.label(),
            wall_samples_ms: vec![wall_sample_ms],
            wall_stats: compute_stats(&[wall_sample_ms]),
            guest_import_samples_ms: guest_execution_ms.map(|sample| vec![sample]),
            guest_import_stats: guest_execution_ms.map(|sample| compute_stats(&[sample])),
            startup_overhead_samples_ms: startup_overhead_ms.map(|sample| vec![sample]),
            startup_overhead_stats: startup_overhead_ms.map(|sample| compute_stats(&[sample])),
            phase_samples_ms: BenchmarkScenarioPhases {
                context_setup_ms: vec![context_setup_ms],
                startup_ms: vec![startup_ms],
                guest_execution_ms: guest_execution_ms.map(|sample| vec![sample]),
                completion_ms: vec![completion_ms],
            },
            phase_stats: BenchmarkScenarioPhases {
                context_setup_ms: compute_stats(&[context_setup_ms]),
                startup_ms: compute_stats(&[startup_ms]),
                guest_execution_ms: guest_execution_ms.map(|sample| compute_stats(&[sample])),
                completion_ms: compute_stats(&[completion_ms]),
            },
            resource_usage_stats: compute_resource_usage_stats(&resource_usage_samples),
            resource_usage_samples: Some(resource_usage_samples),
        }
    }

    fn synthetic_host() -> BenchmarkHost {
        BenchmarkHost {
            node_binary: String::from("node"),
            node_version: String::from("v22.0.0"),
            os: "linux",
            arch: "x86_64",
            logical_cpus: 8,
        }
    }

    #[test]
    fn javascript_benchmark_orchestration_resumes_completed_stages_from_run_state() {
        let tempdir = tempdir().expect("create tempdir");
        let repo_root = tempdir.path().join("repo");
        let artifact_dir = tempdir.path().join("artifacts");
        fs::create_dir_all(&repo_root).expect("create repo root");

        let config = JavascriptBenchmarkConfig {
            iterations: 1,
            warmup_iterations: 0,
        };
        let host = synthetic_host();
        let definitions = benchmark_scenarios();
        let mut state = StoredBenchmarkRunState::new(&config, &host, &repo_root);
        state.record_transport_rtt(&synthetic_transport_reports());
        state.record_scenario(&synthetic_scenario_report(definitions[0], 10.0));
        persist_benchmark_run_state(&benchmark_run_state_path(&artifact_dir), &state)
            .expect("persist initial run state");

        let transport_calls = RefCell::new(0usize);
        let scenario_calls = RefCell::new(Vec::new());
        let (report, resumed_stage_count, _) = orchestrate_javascript_benchmark_report(
            &config,
            &repo_root,
            &host,
            &artifact_dir,
            || {
                *transport_calls.borrow_mut() += 1;
                Ok(synthetic_transport_reports())
            },
            |definition| {
                scenario_calls.borrow_mut().push(definition.id.to_owned());
                Ok(synthetic_scenario_report(definition, 20.0))
            },
        )
        .expect("resume benchmark orchestration");

        assert_eq!(resumed_stage_count, 2);
        assert_eq!(*transport_calls.borrow(), 0);
        assert_eq!(
            scenario_calls.borrow().as_slice(),
            &definitions[1..]
                .iter()
                .map(|definition| definition.id.to_owned())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            report.transport_rtt.len(),
            TRANSPORT_RTT_PAYLOAD_BYTES.len()
        );
        assert_eq!(report.scenarios.len(), definitions.len());
        assert_eq!(report.scenarios[0].id, definitions[0].id);
        assert_eq!(report.scenarios[1].id, definitions[1].id);
    }

    #[test]
    fn javascript_benchmark_orchestration_persists_completed_stages_before_failure() {
        let tempdir = tempdir().expect("create tempdir");
        let repo_root = tempdir.path().join("repo");
        let artifact_dir = tempdir.path().join("artifacts");
        fs::create_dir_all(&repo_root).expect("create repo root");

        let config = JavascriptBenchmarkConfig {
            iterations: 1,
            warmup_iterations: 0,
        };
        let host = synthetic_host();
        let state_path = benchmark_run_state_path(&artifact_dir);
        let failure = orchestrate_javascript_benchmark_report(
            &config,
            &repo_root,
            &host,
            &artifact_dir,
            || Ok(synthetic_transport_reports()),
            |definition| {
                if definition.id == "cold-local-import" {
                    Err(JavascriptBenchmarkError::InvalidConfig("synthetic failure"))
                } else {
                    Ok(synthetic_scenario_report(definition, 15.0))
                }
            },
        )
        .expect_err("expected synthetic orchestration failure");

        assert!(matches!(
            failure,
            JavascriptBenchmarkError::InvalidConfig("synthetic failure")
        ));

        let stored_state = serde_json::from_str::<StoredBenchmarkRunState>(
            &fs::read_to_string(&state_path).expect("read persisted run state"),
        )
        .expect("parse persisted run state");
        assert!(stored_state.transport_rtt.is_some());
        assert_eq!(
            stored_state
                .scenarios
                .iter()
                .map(|scenario| scenario.id.as_str())
                .collect::<Vec<_>>(),
            vec!["isolate-startup", "prewarmed-isolate-startup"]
        );
    }
}
