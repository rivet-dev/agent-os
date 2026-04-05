use agent_os_execution::benchmark::{run_javascript_benchmarks, JavascriptBenchmarkConfig};

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
            "cold-local-import",
            "warm-local-import",
            "builtin-import",
            "large-package-import",
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

    let rendered = report.render_markdown();
    assert!(rendered.contains("ARC-021C"));
    assert!(rendered.contains("ARC-021D"));
    assert!(rendered.contains("ARC-022"));
    assert!(rendered.contains("typescript"));
    assert!(rendered.contains("node:path + node:url + node:fs/promises"));
}
