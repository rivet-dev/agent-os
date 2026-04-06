use agent_os_execution::{
    CreateJavascriptContextRequest, JavascriptExecutionEngine, JavascriptExecutionEvent,
    StartJavascriptExecutionRequest,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tempfile::tempdir;

const NODE_IMPORT_CACHE_METRICS_PREFIX: &str = "__AGENT_OS_NODE_IMPORT_CACHE_METRICS__:";
const NODE_WARMUP_METRICS_PREFIX: &str = "__AGENT_OS_NODE_WARMUP_METRICS__:";
const JAVASCRIPT_TEST_VM_ID: &str = "vm-js";

static NEXT_TEST_IMPORT_CACHE_ID: AtomicUsize = AtomicUsize::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NodeImportCacheMetrics {
    resolve_hits: usize,
    resolve_misses: usize,
    package_type_hits: usize,
    package_type_misses: usize,
    module_format_hits: usize,
    module_format_misses: usize,
    source_hits: usize,
    source_misses: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NodeWarmupMetrics {
    executed: bool,
    reason: String,
    import_count: usize,
    asset_root: String,
}

fn assert_node_available() {
    let binary = std::env::var("AGENT_OS_NODE_BINARY").unwrap_or_else(|_| String::from("node"));
    let output = Command::new(binary)
        .arg("--version")
        .output()
        .expect("spawn node --version");
    assert!(output.status.success(), "node --version failed");
}

fn write_fixture(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write fixture");
}

fn collect_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();

    if !root.exists() {
        return files;
    }

    for entry in fs::read_dir(root).expect("read cache dir") {
        let entry = entry.expect("cache entry");
        let path = entry.path();
        let metadata = entry.metadata().expect("cache metadata");

        if metadata.is_dir() {
            files.extend(collect_files(&path));
        } else if metadata.is_file() {
            files.push(path);
        }
    }

    files.sort();
    files
}

fn parse_import_cache_metrics(stderr: &str) -> NodeImportCacheMetrics {
    let metrics_line = stderr
        .lines()
        .filter_map(|line| line.strip_prefix(NODE_IMPORT_CACHE_METRICS_PREFIX))
        .last()
        .expect("import cache metrics line");

    NodeImportCacheMetrics {
        resolve_hits: parse_metric_value(metrics_line, "resolveHits"),
        resolve_misses: parse_metric_value(metrics_line, "resolveMisses"),
        package_type_hits: parse_metric_value(metrics_line, "packageTypeHits"),
        package_type_misses: parse_metric_value(metrics_line, "packageTypeMisses"),
        module_format_hits: parse_metric_value(metrics_line, "moduleFormatHits"),
        module_format_misses: parse_metric_value(metrics_line, "moduleFormatMisses"),
        source_hits: parse_metric_value(metrics_line, "sourceHits"),
        source_misses: parse_metric_value(metrics_line, "sourceMisses"),
    }
}

fn parse_warmup_metrics(stderr: &str) -> NodeWarmupMetrics {
    let metrics_line = stderr
        .lines()
        .filter_map(|line| line.strip_prefix(NODE_WARMUP_METRICS_PREFIX))
        .last()
        .expect("warmup metrics line");

    NodeWarmupMetrics {
        executed: parse_boolean_metric(metrics_line, "executed"),
        reason: parse_string_metric(metrics_line, "reason"),
        import_count: parse_metric_value(metrics_line, "importCount"),
        asset_root: parse_string_metric(metrics_line, "assetRoot"),
    }
}

fn parse_metric_value(metrics_line: &str, key: &str) -> usize {
    let marker = format!("\"{key}\":");
    let start = metrics_line.find(&marker).expect("metric key") + marker.len();
    let digits: String = metrics_line[start..]
        .chars()
        .skip_while(|ch| !ch.is_ascii_digit())
        .take_while(|ch| ch.is_ascii_digit())
        .collect();

    digits.parse().expect("metric value")
}

fn parse_boolean_metric(metrics_line: &str, key: &str) -> bool {
    let marker = format!("\"{key}\":");
    let start = metrics_line.find(&marker).expect("metric key") + marker.len();
    let remaining = &metrics_line[start..];

    if remaining.starts_with("true") {
        true
    } else if remaining.starts_with("false") {
        false
    } else {
        panic!("invalid boolean metric for {key}: {metrics_line}");
    }
}

fn parse_string_metric(metrics_line: &str, key: &str) -> String {
    let marker = format!("\"{key}\":\"");
    let start = metrics_line.find(&marker).expect("metric key") + marker.len();
    let mut value = String::new();
    let mut escaped = false;

    for ch in metrics_line[start..].chars() {
        if escaped {
            value.push(match ch {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                '"' => '"',
                '\\' => '\\',
                other => other,
            });
            escaped = false;
            continue;
        }

        match ch {
            '\\' => escaped = true,
            '"' => return value,
            other => value.push(other),
        }
    }

    panic!("unterminated string metric for {key}: {metrics_line}");
}

fn run_javascript_execution(
    engine: &mut JavascriptExecutionEngine,
    context_id: String,
    cwd: &Path,
    argv: Vec<String>,
    env: BTreeMap<String, String>,
) -> (String, String, i32) {
    let execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id,
            argv,
            env,
            cwd: cwd.to_path_buf(),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stdout = String::from_utf8(result.stdout).expect("stdout utf8");
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");

    (stdout, stderr, result.exit_code)
}

fn new_test_engine() -> JavascriptExecutionEngine {
    let mut engine = JavascriptExecutionEngine::default();
    let cache_id = NEXT_TEST_IMPORT_CACHE_ID.fetch_add(1, Ordering::Relaxed);
    let base_dir = std::env::temp_dir().join(format!(
        "agent-os-node-import-cache-test-{}-{cache_id}",
        std::process::id()
    ));
    engine.set_import_cache_base_dir(JAVASCRIPT_TEST_VM_ID, base_dir);
    engine
}

mod builtin_interception;
mod env_hardening;
mod module_resolution;
mod sync_rpc;
