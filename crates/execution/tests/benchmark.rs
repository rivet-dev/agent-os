use agent_os_execution::benchmark::{
    run_javascript_benchmarks, BenchmarkDistributionStats, BenchmarkHost, BenchmarkResourceUsage,
    BenchmarkScenarioPhases, BenchmarkScenarioReport, BenchmarkStats, BenchmarkTransportRttReport,
    JavascriptBenchmarkConfig, JavascriptBenchmarkReport,
};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use tempfile::tempdir;

fn stats(
    mean_ms: f64,
    p50_ms: f64,
    p95_ms: f64,
    min_ms: f64,
    max_ms: f64,
    stddev_ms: f64,
) -> BenchmarkStats {
    BenchmarkStats {
        mean_ms,
        p50_ms,
        p95_ms,
        min_ms,
        max_ms,
        stddev_ms,
    }
}

fn phase_samples(
    context_setup_ms: Vec<f64>,
    startup_ms: Vec<f64>,
    guest_execution_ms: Option<Vec<f64>>,
    completion_ms: Vec<f64>,
) -> BenchmarkScenarioPhases<Vec<f64>> {
    BenchmarkScenarioPhases {
        context_setup_ms,
        startup_ms,
        guest_execution_ms,
        completion_ms,
    }
}

fn phase_stats(
    context_setup_ms: BenchmarkStats,
    startup_ms: BenchmarkStats,
    guest_execution_ms: Option<BenchmarkStats>,
    completion_ms: BenchmarkStats,
) -> BenchmarkScenarioPhases<BenchmarkStats> {
    BenchmarkScenarioPhases {
        context_setup_ms,
        startup_ms,
        guest_execution_ms,
        completion_ms,
    }
}

fn transport_rtt(
    payload_bytes: usize,
    samples_ms: Vec<f64>,
    stats: BenchmarkStats,
) -> BenchmarkTransportRttReport {
    BenchmarkTransportRttReport {
        channel: "execution-stdio-echo",
        payload_bytes,
        samples_ms,
        stats,
    }
}

fn distribution_stats(
    mean: f64,
    p50: f64,
    p95: f64,
    min: f64,
    max: f64,
    stddev: f64,
) -> BenchmarkDistributionStats {
    BenchmarkDistributionStats {
        mean,
        p50,
        p95,
        min,
        max,
        stddev,
    }
}

fn resource_samples(
    rss_bytes: Option<Vec<f64>>,
    heap_used_bytes: Option<Vec<f64>>,
    cpu_total_us: Option<Vec<f64>>,
) -> BenchmarkResourceUsage<Vec<f64>> {
    BenchmarkResourceUsage {
        rss_bytes,
        heap_used_bytes,
        cpu_user_us: None,
        cpu_system_us: None,
        cpu_total_us,
    }
}

fn resource_stats(
    rss_bytes: Option<BenchmarkDistributionStats>,
    heap_used_bytes: Option<BenchmarkDistributionStats>,
    cpu_total_us: Option<BenchmarkDistributionStats>,
) -> BenchmarkResourceUsage<BenchmarkDistributionStats> {
    BenchmarkResourceUsage {
        rss_bytes,
        heap_used_bytes,
        cpu_user_us: None,
        cpu_system_us: None,
        cpu_total_us,
    }
}

#[test]
fn javascript_benchmark_harness_covers_required_startup_and_import_scenarios() {
    let report = run_javascript_benchmarks(&JavascriptBenchmarkConfig {
        iterations: 1,
        warmup_iterations: 0,
    })
    .expect("run execution benchmark harness");

    let scenario_ids = report
        .scenarios
        .iter()
        .map(|scenario| scenario.id)
        .collect::<Vec<_>>();
    assert_eq!(
        scenario_ids,
        vec![
            "isolate-startup",
            "prewarmed-isolate-startup",
            "cold-local-import",
            "warm-local-import",
            "same-context-local-import",
            "prewarmed-local-import",
            "host-local-import",
            "builtin-import",
            "hot-builtin-stream-import",
            "hot-builtin-stream-web-import",
            "hot-builtin-crypto-import",
            "hot-builtin-zlib-import",
            "hot-builtin-assert-import",
            "hot-builtin-url-import",
            "hot-projected-package-file-import",
            "large-package-import",
            "projected-package-import",
            "pdf-lib-startup",
            "jszip-startup",
            "jszip-end-to-end",
            "jszip-repeated-session-compressed",
        ]
    );

    for scenario in &report.scenarios {
        assert_eq!(scenario.wall_samples_ms.len(), 1);
        assert!(scenario.wall_stats.mean_ms >= 0.0);
    }

    let warm = report
        .scenarios
        .iter()
        .find(|scenario| scenario.id == "warm-local-import")
        .expect("warm-local-import scenario");
    assert_eq!(warm.compile_cache, "primed");
    assert_eq!(
        warm.guest_import_samples_ms
            .as_ref()
            .expect("warm import samples")
            .len(),
        1
    );
    assert_eq!(
        warm.startup_overhead_samples_ms
            .as_ref()
            .expect("warm startup samples")
            .len(),
        1
    );
    assert_eq!(warm.workload, "local-import");
    assert_eq!(warm.runtime, "native-execution");
    assert_eq!(warm.mode, "new-session-replay");

    let same_context = report
        .scenarios
        .iter()
        .find(|scenario| scenario.id == "same-context-local-import")
        .expect("same-context-local-import scenario");
    assert_eq!(same_context.compile_cache, "primed");
    assert_eq!(same_context.workload, "local-import");
    assert_eq!(same_context.runtime, "native-execution");
    assert_eq!(same_context.mode, "same-session-replay");
    assert_eq!(same_context.wall_samples_ms.len(), 1);

    let prewarmed = report
        .scenarios
        .iter()
        .find(|scenario| scenario.id == "prewarmed-local-import")
        .expect("prewarmed-local-import scenario");
    assert_eq!(prewarmed.compile_cache, "primed");
    assert_eq!(
        prewarmed
            .guest_import_samples_ms
            .as_ref()
            .expect("prewarmed import samples")
            .len(),
        1
    );
    assert_eq!(
        prewarmed
            .startup_overhead_samples_ms
            .as_ref()
            .expect("prewarmed startup samples")
            .len(),
        1
    );
    assert_eq!(prewarmed.mode, "same-engine-replay");

    let host = report
        .scenarios
        .iter()
        .find(|scenario| scenario.id == "host-local-import")
        .expect("host-local-import scenario");
    assert_eq!(host.workload, "local-import");
    assert_eq!(host.runtime, "host-node");
    assert_eq!(host.mode, "host-control");
    assert_eq!(
        host.guest_import_samples_ms
            .as_ref()
            .expect("host import samples")
            .len(),
        1
    );

    let prewarmed_isolate = report
        .scenarios
        .iter()
        .find(|scenario| scenario.id == "prewarmed-isolate-startup")
        .expect("prewarmed-isolate-startup scenario");
    assert_eq!(prewarmed_isolate.workload, "startup-floor");
    assert_eq!(prewarmed_isolate.mode, "same-engine-replay");
    assert_eq!(prewarmed_isolate.compile_cache, "primed");
    assert!(prewarmed_isolate.guest_import_samples_ms.is_none());

    let hot_builtin = report
        .scenarios
        .iter()
        .find(|scenario| scenario.id == "hot-builtin-crypto-import")
        .expect("hot-builtin-crypto-import scenario");
    assert_eq!(hot_builtin.workload, "builtin-hot-import");
    assert_eq!(hot_builtin.mode, "same-engine-replay");
    assert_eq!(hot_builtin.compile_cache, "primed");
    assert_eq!(
        hot_builtin
            .guest_import_samples_ms
            .as_ref()
            .expect("hot builtin import samples")
            .len(),
        1
    );

    let hot_projected = report
        .scenarios
        .iter()
        .find(|scenario| scenario.id == "hot-projected-package-file-import")
        .expect("hot-projected-package-file-import scenario");
    assert_eq!(hot_projected.workload, "projected-package-hot-import");
    assert_eq!(hot_projected.mode, "same-engine-replay");
    assert_eq!(hot_projected.compile_cache, "primed");
    assert_eq!(
        hot_projected
            .guest_import_samples_ms
            .as_ref()
            .expect("hot projected import samples")
            .len(),
        1
    );

    let rendered = report.render_markdown();
    assert!(rendered.contains("ARC-021C"));
    assert!(rendered.contains("ARC-021D"));
    assert!(rendered.contains("ARC-022"));
    assert!(rendered.contains("current import-cache materialization and builtin/polyfill prewarm"));
    assert!(rendered.contains("typescript"));
    assert!(rendered.contains("projected TypeScript guest-path import"));
    assert!(rendered.contains("projected-package-import"));
    assert!(rendered.contains("pdf-lib document creation"));
    assert!(rendered.contains("jszip archive staging"));
    assert!(rendered.contains("jszip end-to-end archive roundtrip"));
    assert!(rendered.contains("jszip compressed archive roundtrip"));
    assert!(rendered.contains("prewarmed-isolate-startup"));
    assert!(rendered.contains("prewarmed-local-import"));
    assert!(rendered.contains("same-context-local-import"));
    assert!(rendered.contains("host-local-import"));
    assert!(rendered.contains("node:path + node:url + node:fs/promises"));
    assert!(rendered.contains("node:stream/web"));
    assert!(rendered.contains("node:crypto"));
    assert!(rendered.contains("projected TypeScript compiler file"));
    assert!(rendered.contains("hot-projected-package-file-import"));
    assert!(rendered.contains("## Transport RTT"));
    assert!(rendered.contains("## Control Matrix"));
    assert!(rendered.contains("## Ranked Hotspots"));
    assert!(rendered.contains("### Wall Time (`time`, `ms`)"));
    assert!(rendered.contains("### Startup Share Of Wall (`share`, `pct`)"));
    assert!(rendered.contains("Mean context (ms)"));
    assert!(rendered.contains("same-session-replay"));
    assert!(rendered.contains("host-control"));

    let json = report.render_json().expect("render benchmark json");
    let parsed: Value = serde_json::from_str(&json).expect("parse benchmark json");
    assert_eq!(parsed["artifact_version"], 5);
    assert_eq!(parsed["summary"]["scenario_count"], 21);
    assert_eq!(parsed["summary"]["recorded_samples_per_scenario"], 1);
    assert_eq!(
        parsed["transport_rtt"]
            .as_array()
            .expect("transport rtt array")
            .len(),
        3
    );
    let scenarios = parsed["scenarios"]
        .as_array()
        .expect("json scenarios array");
    assert_eq!(scenarios.len(), 21);
    assert!(
        parsed["summary"]["slowest_wall_scenario"]["id"].is_string(),
        "expected a summarized slowest wall scenario: {json}"
    );
    let startup_floor_matrix = parsed["summary"]["control_matrix"]
        .as_array()
        .expect("control matrix array")
        .iter()
        .find(|row| row["workload"] == "startup-floor")
        .expect("startup-floor control matrix row");
    assert_eq!(
        startup_floor_matrix["modes"].as_array().map(Vec::len),
        Some(2)
    );
    let local_import_matrix = parsed["summary"]["control_matrix"]
        .as_array()
        .expect("control matrix array")
        .iter()
        .find(|row| row["workload"] == "local-import")
        .expect("local-import control matrix row");
    assert_eq!(
        local_import_matrix["modes"].as_array().map(Vec::len),
        Some(5)
    );
    assert_eq!(
        local_import_matrix["runtimes"].as_array().map(Vec::len),
        Some(2)
    );
    let builtin_hot_matrix = parsed["summary"]["control_matrix"]
        .as_array()
        .expect("control matrix array")
        .iter()
        .find(|row| row["workload"] == "builtin-hot-import")
        .expect("builtin-hot-import control matrix row");
    assert_eq!(
        builtin_hot_matrix["scenario_ids"].as_array().map(Vec::len),
        Some(6)
    );
    let hotspot_rankings = parsed["summary"]["hotspot_rankings"]
        .as_array()
        .expect("hotspot rankings array");
    assert_eq!(hotspot_rankings.len(), 13);
    assert_eq!(hotspot_rankings[0]["metric"], "wall_mean_ms");
    assert_eq!(hotspot_rankings[1]["metric"], "wall_stddev_ms");
    assert_eq!(hotspot_rankings[1]["dimension"], "stability");
    assert_eq!(hotspot_rankings[0]["unit"], "ms");
    assert!(scenarios
        .iter()
        .all(|scenario| scenario["wall_stats"]["stddev_ms"].is_number()));
    assert!(scenarios.iter().any(|scenario| {
        scenario["id"] == "prewarmed-isolate-startup"
            && scenario["workload"] == "startup-floor"
            && scenario["mode"] == "same-engine-replay"
            && scenario["compile_cache"] == "primed"
            && scenario["guest_import_stats"].is_null()
    }));
    assert!(scenarios.iter().any(|scenario| {
        scenario["id"] == "same-context-local-import"
            && scenario["workload"] == "local-import"
            && scenario["runtime"] == "native-execution"
            && scenario["mode"] == "same-session-replay"
            && scenario["compile_cache"] == "primed"
    }));
    assert!(scenarios.iter().any(|scenario| {
        scenario["id"] == "host-local-import"
            && scenario["workload"] == "local-import"
            && scenario["runtime"] == "host-node"
            && scenario["mode"] == "host-control"
            && scenario["guest_import_stats"]["mean_ms"].is_number()
    }));
    assert!(scenarios.iter().any(|scenario| {
        scenario["id"] == "hot-builtin-stream-web-import"
            && scenario["fixture"] == "node:stream/web"
            && scenario["compile_cache"] == "primed"
            && scenario["guest_import_stats"]["mean_ms"].is_number()
    }));
    assert!(scenarios.iter().any(|scenario| {
        scenario["id"] == "hot-builtin-crypto-import"
            && scenario["fixture"] == "node:crypto"
            && scenario["compile_cache"] == "primed"
            && scenario["guest_import_stats"]["mean_ms"].is_number()
    }));
    assert!(scenarios.iter().any(|scenario| {
        scenario["id"] == "hot-projected-package-file-import"
            && scenario["fixture"] == "projected TypeScript compiler file"
            && scenario["compile_cache"] == "primed"
            && scenario["guest_import_stats"]["mean_ms"].is_number()
    }));
    assert!(scenarios.iter().any(|scenario| {
        scenario["id"] == "pdf-lib-startup"
            && scenario["fixture"] == "pdf-lib document creation"
            && scenario["guest_import_stats"]["mean_ms"].is_number()
    }));
    assert!(scenarios.iter().any(|scenario| {
        scenario["id"] == "large-package-import"
            && scenario["fixture"] == "typescript"
            && scenario["compile_cache"] == "disabled"
            && scenario["guest_import_stats"]["mean_ms"].is_number()
    }));
    assert!(scenarios.iter().any(|scenario| {
        scenario["id"] == "jszip-startup"
            && scenario["fixture"] == "jszip archive staging"
            && scenario["guest_import_stats"]["mean_ms"].is_number()
    }));
    assert!(scenarios.iter().any(|scenario| {
        scenario["id"] == "jszip-end-to-end"
            && scenario["fixture"] == "jszip end-to-end archive roundtrip"
            && scenario["compile_cache"] == "disabled"
            && scenario["guest_import_stats"]["mean_ms"].is_number()
    }));
    assert!(scenarios.iter().any(|scenario| {
        scenario["id"] == "jszip-repeated-session-compressed"
            && scenario["fixture"] == "jszip compressed archive roundtrip"
            && scenario["compile_cache"] == "primed"
            && scenario["guest_import_stats"]["mean_ms"].is_number()
    }));
    assert!(scenarios.iter().any(|scenario| {
        scenario["id"] == "prewarmed-local-import"
            && scenario["fixture"] == "24-module local ESM graph"
            && scenario["compile_cache"] == "primed"
            && scenario["guest_import_stats"]["mean_ms"].is_number()
    }));
    assert!(scenarios.iter().any(|scenario| {
        scenario["id"] == "projected-package-import"
            && scenario["fixture"] == "projected TypeScript guest-path import"
            && scenario["compile_cache"] == "primed"
            && scenario["guest_import_stats"]["mean_ms"].is_number()
    }));
    assert!(scenarios.iter().any(|scenario| {
        scenario["guest_import_samples_ms"].is_array()
            && scenario["startup_overhead_samples_ms"].is_array()
            && scenario["mean_startup_share_pct"].is_number()
            && scenario["phase_stats"]["startup_ms"]["mean_ms"].is_number()
            && scenario["phase_samples_ms"]["completion_ms"].is_array()
            && scenario["resource_usage_stats"]["rss_bytes"]["mean"].is_number()
            && scenario["resource_usage_stats"]["cpu_total_us"]["mean"].is_number()
            && scenario["resource_usage_samples"]["heap_used_bytes"].is_array()
    }));
}

#[test]
fn javascript_benchmark_json_artifact_stays_stable_for_summary_and_samples() {
    let report = JavascriptBenchmarkReport {
        generated_at_unix_ms: 42,
        config: JavascriptBenchmarkConfig {
            iterations: 2,
            warmup_iterations: 1,
        },
        host: BenchmarkHost {
            node_binary: String::from("node"),
            node_version: String::from("v22.0.0"),
            os: "linux",
            arch: "x86_64",
            logical_cpus: 8,
        },
        repo_root: PathBuf::from("/repo"),
        transport_rtt: vec![
            transport_rtt(32, vec![0.4, 0.6], stats(0.5, 0.4, 0.6, 0.4, 0.6, 0.1)),
            transport_rtt(4096, vec![0.9, 1.1], stats(1.0, 0.9, 1.1, 0.9, 1.1, 0.1)),
            transport_rtt(65536, vec![2.6, 3.0], stats(2.8, 2.6, 3.0, 2.6, 3.0, 0.2)),
        ],
        scenarios: vec![
            BenchmarkScenarioReport {
                id: "fast-scenario",
                workload: "fixture-a",
                runtime: "native-execution",
                mode: "true-cold-start",
                description: "Faster benchmark path",
                fixture: "fixture-a",
                compile_cache: "disabled",
                wall_samples_ms: vec![10.0, 14.0],
                wall_stats: stats(12.0, 10.0, 14.0, 10.0, 14.0, 2.0),
                guest_import_samples_ms: Some(vec![4.0, 6.0]),
                guest_import_stats: Some(stats(5.0, 4.0, 6.0, 4.0, 6.0, 1.0)),
                startup_overhead_samples_ms: Some(vec![6.0, 8.0]),
                startup_overhead_stats: Some(stats(7.0, 6.0, 8.0, 6.0, 8.0, 1.0)),
                phase_samples_ms: phase_samples(
                    vec![1.0, 2.0],
                    vec![2.0, 3.0],
                    Some(vec![4.0, 6.0]),
                    vec![3.0, 3.0],
                ),
                phase_stats: phase_stats(
                    stats(1.5, 1.0, 2.0, 1.0, 2.0, 0.5),
                    stats(2.5, 2.0, 3.0, 2.0, 3.0, 0.5),
                    Some(stats(5.0, 4.0, 6.0, 4.0, 6.0, 1.0)),
                    stats(3.0, 3.0, 3.0, 3.0, 3.0, 0.0),
                ),
                resource_usage_samples: Some(resource_samples(
                    Some(vec![32.0 * 1024.0 * 1024.0, 36.0 * 1024.0 * 1024.0]),
                    Some(vec![8.0 * 1024.0 * 1024.0, 10.0 * 1024.0 * 1024.0]),
                    Some(vec![4000.0, 6000.0]),
                )),
                resource_usage_stats: Some(resource_stats(
                    Some(distribution_stats(
                        34.0 * 1024.0 * 1024.0,
                        32.0 * 1024.0 * 1024.0,
                        36.0 * 1024.0 * 1024.0,
                        32.0 * 1024.0 * 1024.0,
                        36.0 * 1024.0 * 1024.0,
                        2.0 * 1024.0 * 1024.0,
                    )),
                    Some(distribution_stats(
                        9.0 * 1024.0 * 1024.0,
                        8.0 * 1024.0 * 1024.0,
                        10.0 * 1024.0 * 1024.0,
                        8.0 * 1024.0 * 1024.0,
                        10.0 * 1024.0 * 1024.0,
                        1.0 * 1024.0 * 1024.0,
                    )),
                    Some(distribution_stats(
                        5000.0, 4000.0, 6000.0, 4000.0, 6000.0, 1000.0,
                    )),
                )),
            },
            BenchmarkScenarioReport {
                id: "slow-scenario",
                workload: "fixture-b",
                runtime: "host-node",
                mode: "host-control",
                description: "Slower benchmark path",
                fixture: "fixture-b",
                compile_cache: "primed",
                wall_samples_ms: vec![30.0, 34.0],
                wall_stats: stats(32.0, 30.0, 34.0, 30.0, 34.0, 2.0),
                guest_import_samples_ms: Some(vec![12.0, 14.0]),
                guest_import_stats: Some(stats(13.0, 12.0, 14.0, 12.0, 14.0, 1.0)),
                startup_overhead_samples_ms: Some(vec![18.0, 20.0]),
                startup_overhead_stats: Some(stats(19.0, 18.0, 20.0, 18.0, 20.0, 1.0)),
                phase_samples_ms: phase_samples(
                    vec![4.0, 4.0],
                    vec![5.0, 6.0],
                    Some(vec![12.0, 14.0]),
                    vec![9.0, 10.0],
                ),
                phase_stats: phase_stats(
                    stats(4.0, 4.0, 4.0, 4.0, 4.0, 0.0),
                    stats(5.5, 5.0, 6.0, 5.0, 6.0, 0.5),
                    Some(stats(13.0, 12.0, 14.0, 12.0, 14.0, 1.0)),
                    stats(9.5, 9.0, 10.0, 9.0, 10.0, 0.5),
                ),
                resource_usage_samples: Some(resource_samples(
                    Some(vec![64.0 * 1024.0 * 1024.0, 72.0 * 1024.0 * 1024.0]),
                    Some(vec![14.0 * 1024.0 * 1024.0, 18.0 * 1024.0 * 1024.0]),
                    Some(vec![9000.0, 11000.0]),
                )),
                resource_usage_stats: Some(resource_stats(
                    Some(distribution_stats(
                        68.0 * 1024.0 * 1024.0,
                        64.0 * 1024.0 * 1024.0,
                        72.0 * 1024.0 * 1024.0,
                        64.0 * 1024.0 * 1024.0,
                        72.0 * 1024.0 * 1024.0,
                        4.0 * 1024.0 * 1024.0,
                    )),
                    Some(distribution_stats(
                        16.0 * 1024.0 * 1024.0,
                        14.0 * 1024.0 * 1024.0,
                        18.0 * 1024.0 * 1024.0,
                        14.0 * 1024.0 * 1024.0,
                        18.0 * 1024.0 * 1024.0,
                        2.0 * 1024.0 * 1024.0,
                    )),
                    Some(distribution_stats(
                        10000.0, 9000.0, 11000.0, 9000.0, 11000.0, 1000.0,
                    )),
                )),
            },
        ],
    };

    let json = report.render_json().expect("render json");
    let parsed: Value = serde_json::from_str(&json).expect("parse json");

    assert_eq!(parsed["artifact_version"], 5);
    assert_eq!(parsed["generated_at_unix_ms"], 42);
    assert_eq!(
        parsed["command"].as_str(),
        Some(
            "cargo run -p agent-os-execution --bin node-import-bench -- --iterations 2 --warmup-iterations 1"
        )
    );
    assert_eq!(parsed["summary"]["scenario_count"], 2);
    assert_eq!(parsed["summary"]["recorded_samples_per_scenario"], 2);
    assert_eq!(
        parsed["summary"]["control_matrix"][0]["workload"].as_str(),
        Some("fixture-a")
    );
    assert_eq!(
        parsed["summary"]["control_matrix"][1]["runtimes"][0].as_str(),
        Some("host-node")
    );
    assert_eq!(
        parsed["transport_rtt"][2]["payload_bytes"].as_u64(),
        Some(65536)
    );
    assert_eq!(parsed["transport_rtt"][2]["stats"]["mean_ms"], 2.8);
    assert_eq!(
        parsed["summary"]["slowest_wall_scenario"]["id"].as_str(),
        Some("slow-scenario")
    );
    assert_eq!(
        parsed["summary"]["slowest_guest_import_scenario"]["id"].as_str(),
        Some("slow-scenario")
    );
    assert_eq!(
        parsed["summary"]["highest_startup_share_scenario"]["id"].as_str(),
        Some("slow-scenario")
    );
    let hotspot_rankings = parsed["summary"]["hotspot_rankings"]
        .as_array()
        .expect("hotspot rankings array");
    assert_eq!(hotspot_rankings.len(), 13);
    assert_eq!(hotspot_rankings[0]["metric"], "wall_mean_ms");
    assert_eq!(hotspot_rankings[0]["label"], "Wall Time");
    assert_eq!(
        hotspot_rankings[0]["ranked_scenarios"][0]["id"].as_str(),
        Some("slow-scenario")
    );
    assert_eq!(hotspot_rankings[0]["ranked_scenarios"][0]["rank"], 1);
    assert_eq!(hotspot_rankings[3]["metric"], "guest_import_mean_ms");
    assert_eq!(
        hotspot_rankings[3]["ranked_scenarios"][0]["value"].as_f64(),
        Some(13.0)
    );
    assert_eq!(hotspot_rankings[9]["metric"], "startup_share_pct");
    assert_eq!(hotspot_rankings[9]["unit"], "pct");
    assert_eq!(hotspot_rankings[10]["metric"], "rss_mean_mib");
    assert_eq!(hotspot_rankings[12]["metric"], "cpu_total_mean_ms");

    let scenarios = parsed["scenarios"].as_array().expect("scenario array");
    assert_eq!(scenarios.len(), 2);
    assert_eq!(scenarios[0]["workload"], "fixture-a");
    assert_eq!(scenarios[0]["runtime"], "native-execution");
    assert_eq!(scenarios[0]["mode"], "true-cold-start");
    assert_eq!(scenarios[0]["wall_stats"]["stddev_ms"], 2.0);
    assert_eq!(scenarios[0]["mean_startup_share_pct"], 58.333333333333336);
    assert_eq!(
        scenarios[0]["resource_usage_stats"]["rss_bytes"]["mean"],
        35651584.0
    );
    assert_eq!(
        scenarios[0]["resource_usage_stats"]["cpu_total_us"]["mean"],
        5000.0
    );
    assert_eq!(
        scenarios[0]["phase_stats"]["context_setup_ms"]["mean_ms"],
        1.5
    );
    assert_eq!(scenarios[0]["phase_stats"]["completion_ms"]["mean_ms"], 3.0);
    assert_eq!(scenarios[1]["mean_startup_share_pct"], 59.375);
    assert_eq!(scenarios[1]["phase_stats"]["startup_ms"]["mean_ms"], 5.5);
    assert_eq!(
        scenarios[1]["resource_usage_stats"]["heap_used_bytes"]["mean"],
        16777216.0
    );
}

#[test]
fn javascript_benchmark_hotspot_rankings_handle_missing_metrics() {
    let report = JavascriptBenchmarkReport {
        generated_at_unix_ms: 42,
        config: JavascriptBenchmarkConfig {
            iterations: 2,
            warmup_iterations: 1,
        },
        host: BenchmarkHost {
            node_binary: String::from("node"),
            node_version: String::from("v22.0.0"),
            os: "linux",
            arch: "x86_64",
            logical_cpus: 8,
        },
        repo_root: PathBuf::from("/repo"),
        transport_rtt: vec![],
        scenarios: vec![
            BenchmarkScenarioReport {
                id: "alpha",
                workload: "fixture-a",
                runtime: "native-execution",
                mode: "true-cold-start",
                description: "Alpha path",
                fixture: "fixture-a",
                compile_cache: "disabled",
                wall_samples_ms: vec![15.0, 17.0],
                wall_stats: stats(16.0, 15.0, 17.0, 15.0, 17.0, 1.0),
                guest_import_samples_ms: Some(vec![7.0, 9.0]),
                guest_import_stats: Some(stats(8.0, 7.0, 9.0, 7.0, 9.0, 1.0)),
                startup_overhead_samples_ms: Some(vec![8.0, 8.0]),
                startup_overhead_stats: Some(stats(8.0, 8.0, 8.0, 8.0, 8.0, 0.0)),
                phase_samples_ms: phase_samples(
                    vec![2.0, 2.0],
                    vec![3.0, 3.0],
                    Some(vec![7.0, 9.0]),
                    vec![3.0, 3.0],
                ),
                phase_stats: phase_stats(
                    stats(2.0, 2.0, 2.0, 2.0, 2.0, 0.0),
                    stats(3.0, 3.0, 3.0, 3.0, 3.0, 0.0),
                    Some(stats(8.0, 7.0, 9.0, 7.0, 9.0, 1.0)),
                    stats(3.0, 3.0, 3.0, 3.0, 3.0, 0.0),
                ),
                resource_usage_samples: Some(resource_samples(
                    Some(vec![40.0 * 1024.0 * 1024.0, 44.0 * 1024.0 * 1024.0]),
                    None,
                    Some(vec![6000.0, 8000.0]),
                )),
                resource_usage_stats: Some(resource_stats(
                    Some(distribution_stats(
                        42.0 * 1024.0 * 1024.0,
                        40.0 * 1024.0 * 1024.0,
                        44.0 * 1024.0 * 1024.0,
                        40.0 * 1024.0 * 1024.0,
                        44.0 * 1024.0 * 1024.0,
                        2.0 * 1024.0 * 1024.0,
                    )),
                    None,
                    Some(distribution_stats(
                        7000.0, 6000.0, 8000.0, 6000.0, 8000.0, 1000.0,
                    )),
                )),
            },
            BenchmarkScenarioReport {
                id: "beta",
                workload: "fixture-b",
                runtime: "host-node",
                mode: "host-control",
                description: "Beta path",
                fixture: "fixture-b",
                compile_cache: "primed",
                wall_samples_ms: vec![20.0, 24.0],
                wall_stats: stats(22.0, 20.0, 24.0, 20.0, 24.0, 2.0),
                guest_import_samples_ms: Some(vec![10.0, 12.0]),
                guest_import_stats: Some(stats(11.0, 10.0, 12.0, 10.0, 12.0, 1.0)),
                startup_overhead_samples_ms: Some(vec![9.0, 11.0]),
                startup_overhead_stats: Some(stats(10.0, 9.0, 11.0, 9.0, 11.0, 1.0)),
                phase_samples_ms: phase_samples(
                    vec![3.0, 3.0],
                    vec![4.0, 4.0],
                    Some(vec![10.0, 12.0]),
                    vec![5.0, 5.0],
                ),
                phase_stats: phase_stats(
                    stats(3.0, 3.0, 3.0, 3.0, 3.0, 0.0),
                    stats(4.0, 4.0, 4.0, 4.0, 4.0, 0.0),
                    Some(stats(11.0, 10.0, 12.0, 10.0, 12.0, 1.0)),
                    stats(5.0, 5.0, 5.0, 5.0, 5.0, 0.0),
                ),
                resource_usage_samples: Some(resource_samples(
                    Some(vec![60.0 * 1024.0 * 1024.0, 68.0 * 1024.0 * 1024.0]),
                    Some(vec![12.0 * 1024.0 * 1024.0, 14.0 * 1024.0 * 1024.0]),
                    Some(vec![9000.0, 12000.0]),
                )),
                resource_usage_stats: Some(resource_stats(
                    Some(distribution_stats(
                        64.0 * 1024.0 * 1024.0,
                        60.0 * 1024.0 * 1024.0,
                        68.0 * 1024.0 * 1024.0,
                        60.0 * 1024.0 * 1024.0,
                        68.0 * 1024.0 * 1024.0,
                        4.0 * 1024.0 * 1024.0,
                    )),
                    Some(distribution_stats(
                        13.0 * 1024.0 * 1024.0,
                        12.0 * 1024.0 * 1024.0,
                        14.0 * 1024.0 * 1024.0,
                        12.0 * 1024.0 * 1024.0,
                        14.0 * 1024.0 * 1024.0,
                        1.0 * 1024.0 * 1024.0,
                    )),
                    Some(distribution_stats(
                        10500.0, 9000.0, 12000.0, 9000.0, 12000.0, 1500.0,
                    )),
                )),
            },
            BenchmarkScenarioReport {
                id: "gamma",
                workload: "fixture-c",
                runtime: "native-execution",
                mode: "baseline-control",
                description: "Gamma path",
                fixture: "fixture-c",
                compile_cache: "disabled",
                wall_samples_ms: vec![12.0, 14.0],
                wall_stats: stats(13.0, 12.0, 14.0, 12.0, 14.0, 1.0),
                guest_import_samples_ms: None,
                guest_import_stats: None,
                startup_overhead_samples_ms: None,
                startup_overhead_stats: None,
                phase_samples_ms: phase_samples(
                    vec![1.0, 1.0],
                    vec![2.0, 2.0],
                    None,
                    vec![4.0, 4.0],
                ),
                phase_stats: phase_stats(
                    stats(1.0, 1.0, 1.0, 1.0, 1.0, 0.0),
                    stats(2.0, 2.0, 2.0, 2.0, 2.0, 0.0),
                    None,
                    stats(4.0, 4.0, 4.0, 4.0, 4.0, 0.0),
                ),
                resource_usage_samples: Some(resource_samples(
                    Some(vec![24.0 * 1024.0 * 1024.0, 28.0 * 1024.0 * 1024.0]),
                    None,
                    None,
                )),
                resource_usage_stats: Some(resource_stats(
                    Some(distribution_stats(
                        26.0 * 1024.0 * 1024.0,
                        24.0 * 1024.0 * 1024.0,
                        28.0 * 1024.0 * 1024.0,
                        24.0 * 1024.0 * 1024.0,
                        28.0 * 1024.0 * 1024.0,
                        2.0 * 1024.0 * 1024.0,
                    )),
                    None,
                    None,
                )),
            },
        ],
    };

    let json = report.render_json().expect("render json");
    let parsed: Value = serde_json::from_str(&json).expect("parse json");
    let hotspot_rankings = parsed["summary"]["hotspot_rankings"]
        .as_array()
        .expect("hotspot rankings array");
    let wall_ranking = hotspot_rankings
        .iter()
        .find(|ranking| ranking["metric"] == "wall_mean_ms")
        .expect("wall ranking");
    assert_eq!(wall_ranking["ranked_scenarios"][0]["id"], "beta");
    assert_eq!(wall_ranking["ranked_scenarios"][1]["id"], "alpha");
    assert_eq!(wall_ranking["ranked_scenarios"][2]["id"], "gamma");

    let guest_execution_ranking = hotspot_rankings
        .iter()
        .find(|ranking| ranking["metric"] == "guest_execution_mean_ms")
        .expect("guest execution ranking");
    assert_eq!(guest_execution_ranking["ranked_scenarios"][0]["id"], "beta");
    assert_eq!(
        guest_execution_ranking["ranked_scenarios"][1]["id"],
        "alpha"
    );
    assert_eq!(
        guest_execution_ranking["scenarios_without_metric"][0].as_str(),
        Some("gamma")
    );
    let rss_ranking = hotspot_rankings
        .iter()
        .find(|ranking| ranking["metric"] == "rss_mean_mib")
        .expect("rss ranking");
    assert_eq!(rss_ranking["ranked_scenarios"][0]["id"], "beta");
    let cpu_ranking = hotspot_rankings
        .iter()
        .find(|ranking| ranking["metric"] == "cpu_total_mean_ms")
        .expect("cpu ranking");
    assert_eq!(cpu_ranking["scenarios_without_metric"][0], "gamma");

    let markdown = report.render_markdown();
    assert!(markdown.contains("## Ranked Hotspots"));
    assert!(markdown.contains("## Stability And Resource Summary"));
    assert!(markdown.contains("### Guest Execution Phase (`time`, `ms`)"));
    assert!(markdown.contains("### RSS (`memory`, `MiB`)"));
    assert!(markdown.contains("Missing metric for: `gamma`"));
}

#[test]
fn javascript_benchmark_comparison_artifact_stays_stable_for_deltas() {
    let report = JavascriptBenchmarkReport {
        generated_at_unix_ms: 42,
        config: JavascriptBenchmarkConfig {
            iterations: 2,
            warmup_iterations: 1,
        },
        host: BenchmarkHost {
            node_binary: String::from("node"),
            node_version: String::from("v22.0.0"),
            os: "linux",
            arch: "x86_64",
            logical_cpus: 8,
        },
        repo_root: PathBuf::from("/repo"),
        transport_rtt: vec![
            transport_rtt(32, vec![0.4, 0.6], stats(0.5, 0.4, 0.6, 0.4, 0.6, 0.1)),
            transport_rtt(4096, vec![0.9, 1.1], stats(1.0, 0.9, 1.1, 0.9, 1.1, 0.1)),
            transport_rtt(65536, vec![2.6, 3.0], stats(2.8, 2.6, 3.0, 2.6, 3.0, 0.2)),
        ],
        scenarios: vec![
            BenchmarkScenarioReport {
                id: "fast-scenario",
                workload: "fixture-a",
                runtime: "native-execution",
                mode: "true-cold-start",
                description: "Faster benchmark path",
                fixture: "fixture-a",
                compile_cache: "disabled",
                wall_samples_ms: vec![10.0, 14.0],
                wall_stats: stats(12.0, 10.0, 14.0, 10.0, 14.0, 2.0),
                guest_import_samples_ms: Some(vec![4.0, 6.0]),
                guest_import_stats: Some(stats(5.0, 4.0, 6.0, 4.0, 6.0, 1.0)),
                startup_overhead_samples_ms: Some(vec![6.0, 8.0]),
                startup_overhead_stats: Some(stats(7.0, 6.0, 8.0, 6.0, 8.0, 1.0)),
                phase_samples_ms: phase_samples(
                    vec![1.0, 2.0],
                    vec![2.0, 3.0],
                    Some(vec![4.0, 6.0]),
                    vec![3.0, 3.0],
                ),
                phase_stats: phase_stats(
                    stats(1.5, 1.0, 2.0, 1.0, 2.0, 0.5),
                    stats(2.5, 2.0, 3.0, 2.0, 3.0, 0.5),
                    Some(stats(5.0, 4.0, 6.0, 4.0, 6.0, 1.0)),
                    stats(3.0, 3.0, 3.0, 3.0, 3.0, 0.0),
                ),
                resource_usage_samples: None,
                resource_usage_stats: None,
            },
            BenchmarkScenarioReport {
                id: "slow-scenario",
                workload: "fixture-b",
                runtime: "native-execution",
                mode: "new-session-replay",
                description: "Slower benchmark path",
                fixture: "fixture-b",
                compile_cache: "primed",
                wall_samples_ms: vec![30.0, 34.0],
                wall_stats: stats(32.0, 30.0, 34.0, 30.0, 34.0, 2.0),
                guest_import_samples_ms: Some(vec![12.0, 14.0]),
                guest_import_stats: Some(stats(13.0, 12.0, 14.0, 12.0, 14.0, 1.0)),
                startup_overhead_samples_ms: Some(vec![18.0, 20.0]),
                startup_overhead_stats: Some(stats(19.0, 18.0, 20.0, 18.0, 20.0, 1.0)),
                phase_samples_ms: phase_samples(
                    vec![4.0, 4.0],
                    vec![5.0, 6.0],
                    Some(vec![12.0, 14.0]),
                    vec![9.0, 10.0],
                ),
                phase_stats: phase_stats(
                    stats(4.0, 4.0, 4.0, 4.0, 4.0, 0.0),
                    stats(5.5, 5.0, 6.0, 5.0, 6.0, 0.5),
                    Some(stats(13.0, 12.0, 14.0, 12.0, 14.0, 1.0)),
                    stats(9.5, 9.0, 10.0, 9.0, 10.0, 0.5),
                ),
                resource_usage_samples: None,
                resource_usage_stats: None,
            },
            BenchmarkScenarioReport {
                id: "current-only",
                workload: "fixture-c",
                runtime: "host-node",
                mode: "host-control",
                description: "Current-only scenario",
                fixture: "fixture-c",
                compile_cache: "disabled",
                wall_samples_ms: vec![8.0, 10.0],
                wall_stats: stats(9.0, 8.0, 10.0, 8.0, 10.0, 1.0),
                guest_import_samples_ms: None,
                guest_import_stats: None,
                startup_overhead_samples_ms: None,
                startup_overhead_stats: None,
                phase_samples_ms: phase_samples(
                    vec![1.0, 1.0],
                    vec![2.0, 3.0],
                    None,
                    vec![5.0, 6.0],
                ),
                phase_stats: phase_stats(
                    stats(1.0, 1.0, 1.0, 1.0, 1.0, 0.0),
                    stats(2.5, 2.0, 3.0, 2.0, 3.0, 0.5),
                    None,
                    stats(5.5, 5.0, 6.0, 5.0, 6.0, 0.5),
                ),
                resource_usage_samples: None,
                resource_usage_stats: None,
            },
        ],
    };

    let tempdir = tempdir().expect("create tempdir");
    let baseline_path = tempdir.path().join("baseline.json");
    fs::write(
        &baseline_path,
        r#"{
  "artifact_version": 1,
  "generated_at_unix_ms": 24,
  "scenarios": [
    {
      "id": "fast-scenario",
      "wall_stats": {
        "mean_ms": 15.0,
        "p50_ms": 15.0,
        "p95_ms": 15.0,
        "min_ms": 15.0,
        "max_ms": 15.0,
        "stddev_ms": 0.0
      },
      "guest_import_stats": {
        "mean_ms": 6.0,
        "p50_ms": 6.0,
        "p95_ms": 6.0,
        "min_ms": 6.0,
        "max_ms": 6.0,
        "stddev_ms": 0.0
      },
      "startup_overhead_stats": {
        "mean_ms": 9.0,
        "p50_ms": 9.0,
        "p95_ms": 9.0,
        "min_ms": 9.0,
        "max_ms": 9.0,
        "stddev_ms": 0.0
      }
    },
    {
      "id": "slow-scenario",
      "wall_stats": {
        "mean_ms": 28.0,
        "p50_ms": 28.0,
        "p95_ms": 28.0,
        "min_ms": 28.0,
        "max_ms": 28.0,
        "stddev_ms": 0.0
      },
      "guest_import_stats": {
        "mean_ms": 11.0,
        "p50_ms": 11.0,
        "p95_ms": 11.0,
        "min_ms": 11.0,
        "max_ms": 11.0,
        "stddev_ms": 0.0
      },
      "startup_overhead_stats": {
        "mean_ms": 17.0,
        "p50_ms": 17.0,
        "p95_ms": 17.0,
        "min_ms": 17.0,
        "max_ms": 17.0,
        "stddev_ms": 0.0
      }
    },
    {
      "id": "baseline-only",
      "wall_stats": {
        "mean_ms": 5.0,
        "p50_ms": 5.0,
        "p95_ms": 5.0,
        "min_ms": 5.0,
        "max_ms": 5.0,
        "stddev_ms": 0.0
      }
    }
  ]
}"#,
    )
    .expect("write baseline report");

    let comparison = report
        .compare_to_baseline_path(&baseline_path)
        .expect("load comparison");
    let json = report
        .render_json_with_comparison(Some(&comparison))
        .expect("render comparison json");
    let parsed: Value = serde_json::from_str(&json).expect("parse comparison json");

    assert_eq!(
        parsed["comparison"]["summary"]["compared_scenario_count"],
        2
    );
    assert_eq!(
        parsed["comparison"]["summary"]["largest_wall_improvement"]["id"].as_str(),
        Some("fast-scenario")
    );
    assert_eq!(
        parsed["comparison"]["summary"]["largest_wall_regression"]["id"].as_str(),
        Some("slow-scenario")
    );
    assert_eq!(
        parsed["comparison"]["scenario_deltas"][0]["wall_mean_ms"]["delta_ms"],
        -3.0
    );
    assert_eq!(
        parsed["comparison"]["scenario_deltas"][1]["wall_mean_ms"]["delta_ms"],
        4.0
    );
    assert!(
        parsed["comparison"]["scenario_deltas"][0]["phase_mean_ms"].is_null(),
        "phase deltas should stay absent when the baseline artifact has no phase data"
    );
    assert_eq!(
        parsed["comparison"]["scenarios_missing_from_baseline"][0].as_str(),
        Some("current-only")
    );
    assert_eq!(
        parsed["comparison"]["baseline_only_scenarios"][0].as_str(),
        Some("baseline-only")
    );

    let markdown = report.render_markdown_with_comparison(Some(&comparison));
    assert!(markdown.contains("## Baseline Comparison"));
    assert!(markdown.contains("Context delta (ms)"));
    assert!(markdown.contains("Largest wall-time improvement: `fast-scenario`"));
    assert!(markdown.contains("Largest wall-time regression: `slow-scenario`"));
    assert!(markdown.contains("Scenarios missing from baseline: current-only"));
    assert!(markdown.contains("Baseline-only scenarios: baseline-only"));
}
