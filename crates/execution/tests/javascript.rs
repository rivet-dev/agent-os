use agent_os_execution::{
    CreateJavascriptContextRequest, JavascriptExecutionEngine, JavascriptExecutionEvent,
    StartJavascriptExecutionRequest,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tempfile::tempdir;

const NODE_IMPORT_CACHE_METRICS_PREFIX: &str = "__AGENT_OS_NODE_IMPORT_CACHE_METRICS__:";
const NODE_WARMUP_METRICS_PREFIX: &str = "__AGENT_OS_NODE_WARMUP_METRICS__:";

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

#[test]
fn javascript_contexts_preserve_vm_and_bootstrap_configuration() {
    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: Some(String::from("./bootstrap.mjs")),
        compile_cache_root: None,
    });

    assert_eq!(context.context_id, "js-ctx-1");
    assert_eq!(context.vm_id, "vm-js");
    assert_eq!(context.bootstrap_module.as_deref(), Some("./bootstrap.mjs"));
    assert_eq!(context.compile_cache_dir, None);
}

#[test]
fn javascript_execution_runs_bootstrap_and_streams_stdio() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("bootstrap.mjs"),
        r#"
globalThis.__agentOsBootstrapLoaded = true;
console.log("bootstrap:ready");
"#,
    );
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
if (!globalThis.__agentOsBootstrapLoaded) {
  throw new Error("bootstrap missing");
}

let input = "";
process.stdin.setEncoding("utf8");
for await (const chunk of process.stdin) {
  input += chunk;
}

console.log(`stdout:${process.env.VISIBLE_TEST_ENV}:${input}`);
console.error(`stderr:${process.argv.slice(2).join(",")}`);
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: Some(String::from("./bootstrap.mjs")),
        compile_cache_root: None,
    });

    let mut execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![
                String::from("./entry.mjs"),
                String::from("alpha"),
                String::from("beta"),
            ],
            env: BTreeMap::from([(String::from("VISIBLE_TEST_ENV"), String::from("ok"))]),
            cwd: temp.path().to_path_buf(),
        })
        .expect("start JavaScript execution");

    assert_eq!(execution.execution_id(), "exec-1");

    execution
        .write_stdin(b"hello from stdin")
        .expect("write stdin");
    execution.close_stdin().expect("close stdin");

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut exit_code = None;

    while exit_code.is_none() {
        match execution
            .poll_event(Duration::from_secs(5))
            .expect("poll execution event")
        {
            Some(JavascriptExecutionEvent::Stdout(chunk)) => stdout.extend(chunk),
            Some(JavascriptExecutionEvent::Stderr(chunk)) => stderr.extend(chunk),
            Some(JavascriptExecutionEvent::Exited(code)) => exit_code = Some(code),
            None => panic!("timed out waiting for JavaScript execution event"),
        }
    }

    assert_eq!(exit_code, Some(0));

    let stdout = String::from_utf8(stdout).expect("stdout utf8");
    let stderr = String::from_utf8(stderr).expect("stderr utf8");

    assert!(stdout.contains("bootstrap:ready"));
    assert!(stdout.contains("stdout:ok:hello from stdin"));
    assert!(stderr.contains("stderr:alpha,beta"));
}

#[test]
fn javascript_execution_keeps_streaming_stdin_sessions_alive_until_closed() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
let input = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", (chunk) => {
  input += chunk;
});
process.stdin.on("end", () => {
  console.log(`stdin:${input}`);
});
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let mut execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::from([(String::from("AGENT_OS_KEEP_STDIN_OPEN"), String::from("1"))]),
            cwd: temp.path().to_path_buf(),
        })
        .expect("start JavaScript execution");

    assert!(
        execution
            .poll_event(Duration::from_millis(200))
            .expect("poll execution event before stdin write")
            .is_none(),
        "streaming-stdin execution should stay alive until stdin closes"
    );

    execution
        .write_stdin(b"still-open")
        .expect("write stdin after idle period");
    execution.close_stdin().expect("close stdin");

    let mut stdout = Vec::new();
    let mut exit_code = None;
    while exit_code.is_none() {
        match execution
            .poll_event(Duration::from_secs(5))
            .expect("poll execution event")
        {
            Some(JavascriptExecutionEvent::Stdout(chunk)) => stdout.extend(chunk),
            Some(JavascriptExecutionEvent::Stderr(_chunk)) => {}
            Some(JavascriptExecutionEvent::Exited(code)) => exit_code = Some(code),
            None => panic!("timed out waiting for JavaScript execution event"),
        }
    }

    assert_eq!(exit_code, Some(0));
    assert!(String::from_utf8(stdout)
        .expect("stdout utf8")
        .contains("stdin:still-open"));
}

#[test]
fn javascript_execution_ignores_guest_overrides_for_internal_node_env() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
console.log(`entrypoint:${process.argv[1]}`);
console.log(`args:${process.argv.slice(2).join(",")}`);
console.log(`node-options:${process.env.NODE_OPTIONS ?? "missing"}`);
console.log(`loader-path:${process.env.AGENT_OS_NODE_IMPORT_CACHE_LOADER_PATH ?? "missing"}`);
console.log(`loader-visible:${'AGENT_OS_NODE_IMPORT_CACHE_LOADER_PATH' in process.env}`);
console.log(
  `internal-keys:${Object.keys(process.env).filter((key) => key.startsWith("AGENT_OS_")).length}`,
);
"#,
    );
    write_fixture(
        &temp.path().join("evil.mjs"),
        r#"
console.log("evil override executed");
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let (stdout, stderr, exit_code) = run_javascript_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs"), String::from("safe-arg")],
        BTreeMap::from([
            (
                String::from("AGENT_OS_ENTRYPOINT"),
                String::from("./evil.mjs"),
            ),
            (
                String::from("AGENT_OS_NODE_IMPORT_CACHE_LOADER_PATH"),
                String::from("./evil-loader.mjs"),
            ),
            (String::from("NODE_OPTIONS"), String::from("--no-warnings")),
        ]),
    );

    assert_eq!(exit_code, 0, "stderr: {stderr}");
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("entrypoint:") && line.ends_with("entry.mjs")),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("args:safe-arg"), "stdout: {stdout}");
    assert!(stdout.contains("node-options:missing"), "stdout: {stdout}");
    assert!(stdout.contains("loader-path:missing"), "stdout: {stdout}");
    assert!(stdout.contains("loader-visible:false"), "stdout: {stdout}");
    assert!(stdout.contains("internal-keys:0"), "stdout: {stdout}");
    assert!(
        !stdout.contains("evil override executed"),
        "stdout: {stdout}"
    );
    assert!(
        !stdout.contains("loader-path:./evil-loader.mjs"),
        "stdout: {stdout}"
    );
}

#[test]
fn javascript_execution_freezes_guest_time_sources() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
const firstDate = Date.now();
const firstConstructed = new Date().getTime();
const firstPerformance = performance.now();

await new Promise((resolve) => setTimeout(resolve, 25));

const secondDate = Date.now();
const secondConstructed = new Date().getTime();
const secondPerformance = performance.now();

console.log(
  JSON.stringify({
    sameDate: firstDate === secondDate,
    sameConstructed: firstConstructed === secondConstructed,
    samePerformance: firstPerformance === secondPerformance,
    performanceZero: firstPerformance === 0 && secondPerformance === 0,
  }),
);
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let (stdout, stderr, exit_code) = run_javascript_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        BTreeMap::new(),
    );

    assert_eq!(exit_code, 0);
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    assert!(stdout.contains("\"sameDate\":true"), "stdout: {stdout}");
    assert!(
        stdout.contains("\"sameConstructed\":true"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("\"samePerformance\":true"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("\"performanceZero\":true"),
        "stdout: {stdout}"
    );
}

#[test]
fn javascript_date_function_without_new_uses_frozen_time() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
const expected = new Date(Date.now()).toString();
await new Promise((resolve) => setTimeout(resolve, 1200));
const actual = Date();

console.log(
  JSON.stringify({
    actual,
    expected,
    matches: actual === expected,
  }),
);
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let (stdout, stderr, exit_code) = run_javascript_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        BTreeMap::new(),
    );

    assert_eq!(exit_code, 0, "stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    assert!(stdout.contains("\"matches\":true"), "stdout: {stdout}");
}

#[test]
fn javascript_execution_generates_and_reuses_compile_cache_without_leaking_module_state() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let cache_root = temp.path().join("compile-cache");
    write_fixture(
        &temp.path().join("dep.mjs"),
        r#"
globalThis.__agentOsDepInitCount = (globalThis.__agentOsDepInitCount ?? 0) + 1;
console.log(`dep-init:${globalThis.__agentOsDepInitCount}`);
export const answer = 41;
"#,
    );
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import { answer } from "./dep.mjs";
console.log(`entry:${answer + 1}:${globalThis.__agentOsDepInitCount}`);
"#,
    );

    let mut first_engine = JavascriptExecutionEngine::default();
    let first_context = first_engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: Some(cache_root.clone()),
    });
    let first_cache_dir = first_context
        .compile_cache_dir
        .clone()
        .expect("compile cache dir");

    let (first_stdout, first_stderr, first_exit) = run_javascript_execution(
        &mut first_engine,
        first_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        BTreeMap::from([(
            String::from("NODE_DEBUG_NATIVE"),
            String::from("COMPILE_CACHE"),
        )]),
    );

    assert_eq!(first_exit, 0);
    assert!(first_stdout.contains("dep-init:1"));
    assert!(first_stdout.contains("entry:42:1"));
    assert!(first_stderr.contains("was not initialized"));

    let cache_files = collect_files(&first_cache_dir);
    assert!(
        cache_files.len() >= 2,
        "expected cache files in {first_cache_dir:?}, got {cache_files:?}"
    );

    let mut second_engine = JavascriptExecutionEngine::default();
    let second_context = second_engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: Some(cache_root),
    });

    assert_eq!(second_context.compile_cache_dir, Some(first_cache_dir));

    let (second_stdout, second_stderr, second_exit) = run_javascript_execution(
        &mut second_engine,
        second_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        BTreeMap::from([(
            String::from("NODE_DEBUG_NATIVE"),
            String::from("COMPILE_CACHE"),
        )]),
    );

    assert_eq!(second_exit, 0);
    assert!(second_stdout.contains("dep-init:1"));
    assert!(second_stdout.contains("entry:42:1"));
    assert!(second_stderr.contains("was accepted"));
    assert!(second_stderr.contains("skip persisting"));
}

#[test]
fn javascript_execution_invalidates_compile_cache_when_imported_source_changes() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let cache_root = temp.path().join("compile-cache");
    write_fixture(&temp.path().join("dep.mjs"), "export const answer = 41;\n");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import { answer } from "./dep.mjs";
console.log(`entry:${answer}`);
"#,
    );

    let mut first_engine = JavascriptExecutionEngine::default();
    let first_context = first_engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: Some(cache_root.clone()),
    });

    let (first_stdout, first_stderr, first_exit) = run_javascript_execution(
        &mut first_engine,
        first_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        BTreeMap::from([(
            String::from("NODE_DEBUG_NATIVE"),
            String::from("COMPILE_CACHE"),
        )]),
    );

    assert_eq!(first_exit, 0);
    assert!(first_stdout.contains("entry:41"));
    assert!(first_stderr.contains("was not initialized"));

    write_fixture(&temp.path().join("dep.mjs"), "export const answer = 42;\n");

    let mut second_engine = JavascriptExecutionEngine::default();
    let second_context = second_engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: Some(cache_root),
    });

    let (second_stdout, second_stderr, second_exit) = run_javascript_execution(
        &mut second_engine,
        second_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        BTreeMap::from([(
            String::from("NODE_DEBUG_NATIVE"),
            String::from("COMPILE_CACHE"),
        )]),
    );

    assert_eq!(second_exit, 0);
    assert!(second_stdout.contains("entry:42"));
    assert!(second_stderr.contains("code hash mismatch"));
    assert!(second_stderr.contains("was not initialized"));
}

#[test]
fn javascript_execution_prewarms_builtin_wrappers_across_contexts() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let cache_root = temp.path().join("compile-cache");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import pathDefault, {
  basename,
  __agentOsInitCount as pathInit,
} from "agent-os:builtin/path";
import {
  pathToFileURL,
  __agentOsInitCount as urlInit,
} from "agent-os:builtin/url";
import {
  readFile,
  __agentOsInitCount as fsInit,
} from "agent-os:builtin/fs-promises";

console.log(`path:${basename("/tmp/example.txt")}:${pathInit}`);
console.log(`url:${pathToFileURL("/tmp/example.txt").href}:${urlInit}`);
console.log(`fs:${typeof readFile}:${fsInit}`);
console.log(`sep:${pathDefault.sep}`);
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let first_context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: Some(cache_root.clone()),
    });
    let compile_cache_dir = first_context
        .compile_cache_dir
        .clone()
        .expect("compile cache dir");
    let second_context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: Some(cache_root),
    });

    let debug_env = BTreeMap::from([(
        String::from("AGENT_OS_NODE_WARMUP_DEBUG"),
        String::from("1"),
    )]);

    let (first_stdout, first_stderr, first_exit) = run_javascript_execution(
        &mut engine,
        first_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        debug_env.clone(),
    );
    let first_warmup = parse_warmup_metrics(&first_stderr);

    assert_eq!(first_exit, 0);
    assert!(first_stdout.contains("path:example.txt:1"));
    assert!(first_stdout.contains("url:file:///tmp/example.txt:1"));
    assert!(first_stdout.contains("fs:function:1"));
    assert!(first_stdout.contains("sep:/"));
    assert!(first_warmup.executed);
    assert_eq!(first_warmup.reason, "executed");
    assert_eq!(first_warmup.import_count, 4);

    let cache_files = collect_files(&compile_cache_dir);
    assert!(
        !cache_files.is_empty(),
        "expected compile cache files in {compile_cache_dir:?}"
    );

    let (second_stdout, second_stderr, second_exit) = run_javascript_execution(
        &mut engine,
        second_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        debug_env,
    );
    let second_warmup = parse_warmup_metrics(&second_stderr);

    assert_eq!(second_exit, 0);
    assert!(second_stdout.contains("path:example.txt:1"));
    assert!(second_stdout.contains("url:file:///tmp/example.txt:1"));
    assert!(second_stdout.contains("fs:function:1"));
    assert!(second_stdout.contains("sep:/"));
    assert!(!second_warmup.executed);
    assert_eq!(second_warmup.reason, "cached");
}

#[test]
fn javascript_execution_repairs_tampered_polyfill_assets_before_execution() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let cache_root = temp.path().join("compile-cache");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import pathPolyfill, {
  basename,
  join,
  __agentOsInitCount,
} from "agent-os:polyfill/path";

console.log(
  `polyfill:${basename("/tmp/example.txt")}:${join("/tmp", "example.txt")}:${pathPolyfill.sep}:${__agentOsInitCount}`,
);
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let first_context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: Some(cache_root.clone()),
    });
    let second_context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: Some(cache_root),
    });
    let debug_env = BTreeMap::from([(
        String::from("AGENT_OS_NODE_WARMUP_DEBUG"),
        String::from("1"),
    )]);

    let (first_stdout, first_stderr, first_exit) = run_javascript_execution(
        &mut engine,
        first_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        debug_env.clone(),
    );
    let first_warmup = parse_warmup_metrics(&first_stderr);

    assert_eq!(first_exit, 0);
    assert!(first_stdout.contains("polyfill:example.txt:/tmp/example.txt:/:1"));
    assert!(first_warmup.executed);

    let tampered_polyfill = PathBuf::from(&first_warmup.asset_root).join("polyfills/path.mjs");
    write_fixture(
        &tampered_polyfill,
        "throw new Error('tampered polyfill');\n",
    );

    let (second_stdout, second_stderr, second_exit) = run_javascript_execution(
        &mut engine,
        second_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        debug_env,
    );
    let second_warmup = parse_warmup_metrics(&second_stderr);

    assert_eq!(second_exit, 0);
    assert!(second_stdout.contains("polyfill:example.txt:/tmp/example.txt:/:1"));
    assert!(!second_stderr.contains("tampered polyfill"));
    assert!(!second_warmup.executed);
    assert_eq!(second_warmup.reason, "cached");
}

#[test]
fn javascript_execution_reuses_resolution_and_metadata_caches_across_contexts() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("package.json"),
        "{\n  \"name\": \"agent-os-js-cache-test\",\n  \"type\": \"module\"\n}\n",
    );
    write_fixture(&temp.path().join("dep.js"), "export const answer = 41;\n");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
const dep = await import("./dep.js");
console.log(`answer:${dep.answer}`);
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let first_context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let second_context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let debug_env = BTreeMap::from([(
        String::from("AGENT_OS_NODE_IMPORT_CACHE_DEBUG"),
        String::from("1"),
    )]);

    let (first_stdout, first_stderr, first_exit) = run_javascript_execution(
        &mut engine,
        first_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        debug_env.clone(),
    );
    let first_metrics = parse_import_cache_metrics(&first_stderr);

    assert_eq!(first_exit, 0);
    assert!(first_stdout.contains("answer:41"));
    assert_eq!(first_metrics.resolve_hits, 0);
    assert!(first_metrics.resolve_misses >= 1);

    let (second_stdout, second_stderr, second_exit) = run_javascript_execution(
        &mut engine,
        second_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        debug_env,
    );
    let second_metrics = parse_import_cache_metrics(&second_stderr);

    assert_eq!(second_exit, 0);
    assert!(second_stdout.contains("answer:41"));
    assert!(second_metrics.resolve_hits >= 2);
    assert!(second_metrics.package_type_hits >= 1);
    assert!(second_metrics.module_format_hits >= 1);
}

#[test]
fn javascript_execution_invalidates_bare_package_resolution_when_package_metadata_changes() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let package_dir = temp.path().join("node_modules/demo-pkg");
    fs::create_dir_all(&package_dir).expect("create package dir");

    write_fixture(
        &temp.path().join("package.json"),
        "{\n  \"name\": \"agent-os-js-cache-test\",\n  \"type\": \"module\"\n}\n",
    );
    write_fixture(
        &package_dir.join("package.json"),
        "{\n  \"name\": \"demo-pkg\",\n  \"type\": \"module\",\n  \"exports\": \"./entry.js\"\n}\n",
    );
    write_fixture(&package_dir.join("entry.js"), "export const answer = 41;\n");
    write_fixture(
        &package_dir.join("replacement.js"),
        "export const answer = 42;\n",
    );
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
const pkg = await import("demo-pkg");
console.log(`pkg:${pkg.answer}`);
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let first_context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let debug_env = BTreeMap::from([(
        String::from("AGENT_OS_NODE_IMPORT_CACHE_DEBUG"),
        String::from("1"),
    )]);

    let (first_stdout, first_stderr, first_exit) = run_javascript_execution(
        &mut engine,
        first_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        debug_env.clone(),
    );
    let first_metrics = parse_import_cache_metrics(&first_stderr);

    assert_eq!(first_exit, 0);
    assert!(first_stdout.contains("pkg:41"));
    assert!(first_metrics.resolve_misses >= 1);

    write_fixture(
        &package_dir.join("package.json"),
        "{\n  \"name\": \"demo-pkg\",\n  \"type\": \"module\",\n  \"exports\": \"./replacement.js\"\n}\n",
    );

    let second_context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let (second_stdout, second_stderr, second_exit) = run_javascript_execution(
        &mut engine,
        second_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        debug_env,
    );
    let second_metrics = parse_import_cache_metrics(&second_stderr);

    assert_eq!(second_exit, 0);
    assert!(second_stdout.contains("pkg:42"));
    assert!(second_metrics.resolve_misses >= 1);
}

#[test]
fn javascript_execution_invalidates_package_type_and_module_format_caches() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("package.json"),
        "{\n  \"name\": \"agent-os-js-cache-test\",\n  \"type\": \"module\"\n}\n",
    );
    write_fixture(&temp.path().join("dep.js"), "export const answer = 41;\n");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
const dep = await import("./dep.js");
const answer = dep.answer ?? dep.default.answer;
console.log(`answer:${answer}`);
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let first_context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let debug_env = BTreeMap::from([(
        String::from("AGENT_OS_NODE_IMPORT_CACHE_DEBUG"),
        String::from("1"),
    )]);

    let (first_stdout, _, first_exit) = run_javascript_execution(
        &mut engine,
        first_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        debug_env.clone(),
    );

    assert_eq!(first_exit, 0);
    assert!(first_stdout.contains("answer:41"));

    write_fixture(
        &temp.path().join("package.json"),
        "{\n  \"name\": \"agent-os-js-cache-test\",\n  \"type\": \"commonjs\"\n}\n",
    );
    write_fixture(
        &temp.path().join("dep.js"),
        "module.exports = { answer: 42 };\n",
    );

    let second_context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let (second_stdout, second_stderr, second_exit) = run_javascript_execution(
        &mut engine,
        second_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        debug_env,
    );
    let second_metrics = parse_import_cache_metrics(&second_stderr);

    assert_eq!(second_exit, 0);
    assert!(second_stdout.contains("answer:42"));
    assert!(second_metrics.package_type_misses >= 1);
    assert!(second_metrics.module_format_misses >= 1);
}

#[test]
fn javascript_execution_keeps_cjs_fs_requires_extensible_when_loaded_via_esm() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("dep.cjs"),
        r#"
const fs = require("fs");
const marker = Symbol.for("agent-os.fs-marker");
let extensible = Object.isExtensible(fs);
let canDefine = false;

try {
  Object.defineProperty(fs, marker, {
    configurable: true,
    value: true,
  });
  canDefine = fs[marker] === true;
} catch {
  canDefine = false;
}

module.exports = {
  extensible,
  canDefine,
  existsSyncType: typeof fs.existsSync,
};
"#,
    );
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import result from "./dep.cjs";
console.log(JSON.stringify(result));
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let (stdout, _, exit_code) = run_javascript_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        BTreeMap::new(),
    );

    assert_eq!(exit_code, 0);
    assert!(stdout.contains(r#""extensible":true"#), "{stdout}");
    assert!(stdout.contains(r#""canDefine":true"#), "{stdout}");
    assert!(
        stdout.contains(r#""existsSyncType":"function""#),
        "{stdout}"
    );
}

#[test]
fn javascript_execution_preserves_source_changes_with_cached_resolution() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(&temp.path().join("dep.mjs"), "export const answer = 41;\n");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
const dep = await import("./dep.mjs");
console.log(`answer:${dep.answer}`);
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let first_context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let debug_env = BTreeMap::from([(
        String::from("AGENT_OS_NODE_IMPORT_CACHE_DEBUG"),
        String::from("1"),
    )]);

    let (first_stdout, _, first_exit) = run_javascript_execution(
        &mut engine,
        first_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        debug_env.clone(),
    );

    assert_eq!(first_exit, 0);
    assert!(first_stdout.contains("answer:41"));

    write_fixture(&temp.path().join("dep.mjs"), "export const answer = 42;\n");

    let second_context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let (second_stdout, second_stderr, second_exit) = run_javascript_execution(
        &mut engine,
        second_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        debug_env,
    );
    let second_metrics = parse_import_cache_metrics(&second_stderr);

    assert_eq!(second_exit, 0);
    assert!(second_stdout.contains("answer:42"));
    assert!(second_metrics.resolve_hits >= 2);
}

#[test]
fn javascript_execution_reuses_and_invalidates_projected_package_source_cache() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let projected_root = temp.path().join("projected-node-modules");
    let package_dir = projected_root.join("demo-projected");
    fs::create_dir_all(&package_dir).expect("create projected package dir");
    write_fixture(
        &package_dir.join("package.json"),
        "{\n  \"name\": \"demo-projected\",\n  \"type\": \"module\"\n}\n",
    );
    write_fixture(
        &package_dir.join("entry.js"),
        "import { readFileSync } from 'node:fs';\nexport const answer = 41;\nexport const fsReady = typeof readFileSync === 'function';\n",
    );
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
const mod = await import("/root/node_modules/demo-projected/entry.js");
console.log(`answer:${mod.answer}`);
console.log(`fsReady:${mod.fsReady}`);
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let first_context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let projected_root_host_path = projected_root.to_string_lossy().replace('\\', "\\\\");
    let extra_fs_read_paths_json = format!(
        "[\"{}\"]",
        projected_root.to_string_lossy().replace('\\', "\\\\")
    );
    let debug_env = BTreeMap::from([
        (
            String::from("AGENT_OS_EXTRA_FS_READ_PATHS"),
            extra_fs_read_paths_json,
        ),
        (
            String::from("AGENT_OS_GUEST_PATH_MAPPINGS"),
            format!(
                "[{{\"guestPath\":\"/root/node_modules\",\"hostPath\":\"{projected_root_host_path}\"}}]"
            ),
        ),
        (
            String::from("AGENT_OS_NODE_IMPORT_CACHE_DEBUG"),
            String::from("1"),
        ),
    ]);

    let (first_stdout, first_stderr, first_exit) = run_javascript_execution(
        &mut engine,
        first_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        debug_env.clone(),
    );
    let first_metrics = parse_import_cache_metrics(&first_stderr);

    assert_eq!(first_exit, 0, "stderr: {first_stderr}");
    assert!(first_stdout.contains("answer:41"), "stdout: {first_stdout}");
    assert!(
        first_stdout.contains("fsReady:true"),
        "stdout: {first_stdout}"
    );
    assert_eq!(first_metrics.source_hits, 0, "stderr: {first_stderr}");
    assert!(first_metrics.source_misses >= 1, "stderr: {first_stderr}");

    let second_context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let (second_stdout, second_stderr, second_exit) = run_javascript_execution(
        &mut engine,
        second_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        debug_env.clone(),
    );
    let second_metrics = parse_import_cache_metrics(&second_stderr);

    assert_eq!(second_exit, 0, "stderr: {second_stderr}");
    assert!(
        second_stdout.contains("answer:41"),
        "stdout: {second_stdout}"
    );
    assert!(second_metrics.source_hits >= 1, "stderr: {second_stderr}");

    write_fixture(
        &package_dir.join("entry.js"),
        "import { readFileSync } from 'node:fs';\nexport const answer = 42;\nexport const fsReady = typeof readFileSync === 'function';\n",
    );

    let third_context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let (third_stdout, third_stderr, third_exit) = run_javascript_execution(
        &mut engine,
        third_context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        debug_env,
    );
    let third_metrics = parse_import_cache_metrics(&third_stderr);

    assert_eq!(third_exit, 0, "stderr: {third_stderr}");
    assert!(third_stdout.contains("answer:42"), "stdout: {third_stdout}");
    assert!(
        third_stdout.contains("fsReady:true"),
        "stdout: {third_stdout}"
    );
    assert!(third_metrics.source_misses >= 1, "stderr: {third_stderr}");
}

#[test]
fn javascript_execution_redirects_computed_node_fs_imports_through_builtin_assets() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let guest_mount = temp.path().join("guest-mount");
    fs::create_dir_all(&guest_mount).expect("create guest mount");
    write_fixture(&guest_mount.join("flag.txt"), "mapped\n");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
const fs = await import("node:" + "fs");
const text = fs.readFileSync("/guest/flag.txt", "utf8").trim();
const missing = fs.existsSync("/guest/missing.txt");
console.log(`text:${text}`);
console.log(`missing:${missing}`);
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let guest_mount_host_path = guest_mount.to_string_lossy().replace('\\', "\\\\");
    let env = BTreeMap::from([(
        String::from("AGENT_OS_GUEST_PATH_MAPPINGS"),
        format!("[{{\"guestPath\":\"/guest\",\"hostPath\":\"{guest_mount_host_path}\"}}]"),
    )]);

    let (stdout, _stderr, exit_code) = run_javascript_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        env,
    );

    assert_eq!(exit_code, 0);
    assert!(stdout.contains("text:mapped"));
    assert!(stdout.contains("missing:false"));
}

#[test]
fn javascript_execution_virtualizes_process_cwd_and_denies_chdir() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
const result = {
  cwd: process.cwd(),
};

try {
  process.chdir("/other");
  result.chdir = "unexpected";
} catch (error) {
  result.chdir = {
    code: error.code ?? null,
    message: error.message,
  };
}

result.cwdAfter = process.cwd();
console.log(JSON.stringify(result));
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let cwd_host_path = temp.path().to_string_lossy().replace('\\', "\\\\");
    let env = BTreeMap::from([(
        String::from("AGENT_OS_GUEST_PATH_MAPPINGS"),
        format!("[{{\"guestPath\":\"/root\",\"hostPath\":\"{cwd_host_path}\"}}]"),
    )]);

    let (stdout, stderr, exit_code) = run_javascript_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        env,
    );

    assert_eq!(exit_code, 0, "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse cwd JSON");
    assert_eq!(parsed["cwd"], Value::String(String::from("/root")));
    assert_eq!(parsed["cwdAfter"], Value::String(String::from("/root")));
    assert_eq!(
        parsed["chdir"]["code"],
        Value::String(String::from("ERR_ACCESS_DENIED"))
    );
    assert!(parsed["chdir"]["message"]
        .as_str()
        .expect("chdir message")
        .contains("process.chdir"));
}

#[test]
fn javascript_execution_virtualizes_process_identity() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
const result = {
  execPath: process.execPath,
  argv0: process.argv[0],
  pid: process.pid,
  ppid: process.ppid,
  uid: typeof process.getuid === "function" ? process.getuid() : null,
  gid: typeof process.getgid === "function" ? process.getgid() : null,
};

console.log(JSON.stringify(result));
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let cwd_host_path = temp.path().to_string_lossy().replace('\\', "\\\\");
    let env = BTreeMap::from([
        (
            String::from("AGENT_OS_GUEST_PATH_MAPPINGS"),
            format!("[{{\"guestPath\":\"/root\",\"hostPath\":\"{cwd_host_path}\"}}]"),
        ),
        (
            String::from("AGENT_OS_VIRTUAL_PROCESS_EXEC_PATH"),
            String::from("/usr/bin/node"),
        ),
        (
            String::from("AGENT_OS_VIRTUAL_PROCESS_PID"),
            String::from("41"),
        ),
        (
            String::from("AGENT_OS_VIRTUAL_PROCESS_PPID"),
            String::from("7"),
        ),
        (
            String::from("AGENT_OS_VIRTUAL_PROCESS_UID"),
            String::from("0"),
        ),
        (
            String::from("AGENT_OS_VIRTUAL_PROCESS_GID"),
            String::from("0"),
        ),
    ]);

    let (stdout, stderr, exit_code) = run_javascript_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        env,
    );

    assert_eq!(exit_code, 0, "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse process identity JSON");
    assert_eq!(
        parsed["execPath"],
        Value::String(String::from("/usr/bin/node"))
    );
    assert_eq!(
        parsed["argv0"],
        Value::String(String::from("/usr/bin/node"))
    );
    assert_eq!(parsed["pid"], Value::from(41));
    assert_eq!(parsed["ppid"], Value::from(7));
    assert_eq!(parsed["uid"], Value::from(0));
    assert_eq!(parsed["gid"], Value::from(0));
}

#[test]
fn javascript_execution_denies_process_signal_handlers_and_native_addons() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(&temp.path().join("addon.node"), "not-a-real-native-addon\n");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import { fileURLToPath } from 'node:url';

const addonPath = fileURLToPath(new URL('./addon.node', import.meta.url));
const result = {};

try {
  const returned = process.on('beforeExit', () => {});
  result.nonSignalReturnedSelf = returned === process;
  process.removeAllListeners('beforeExit');
} catch (error) {
  result.nonSignal = { code: error.code ?? null, message: error.message };
}

try {
  process.on('SIGTERM', () => {});
  result.signalOn = 'unexpected';
} catch (error) {
  result.signalOn = { code: error.code ?? null, message: error.message };
}

try {
  process.once('SIGINT', () => {});
  result.signalOnce = 'unexpected';
} catch (error) {
  result.signalOnce = { code: error.code ?? null, message: error.message };
}

try {
  process.dlopen({}, addonPath);
  result.dlopen = 'unexpected';
} catch (error) {
  result.dlopen = { code: error.code ?? null, message: error.message };
}

try {
  require(addonPath);
  result.nativeAddon = 'unexpected';
} catch (error) {
  result.nativeAddon = { code: error.code ?? null, message: error.message };
}

console.log(JSON.stringify(result));
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let (stdout, stderr, exit_code) = run_javascript_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        BTreeMap::new(),
    );

    assert_eq!(exit_code, 0, "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse hardening JSON");
    assert_eq!(parsed["nonSignalReturnedSelf"], Value::Bool(true));
    assert_eq!(
        parsed["signalOn"]["code"],
        Value::String(String::from("ERR_ACCESS_DENIED"))
    );
    assert!(parsed["signalOn"]["message"]
        .as_str()
        .expect("signal on message")
        .contains("process.on(SIGTERM)"));
    assert_eq!(
        parsed["signalOnce"]["code"],
        Value::String(String::from("ERR_ACCESS_DENIED"))
    );
    assert!(parsed["signalOnce"]["message"]
        .as_str()
        .expect("signal once message")
        .contains("process.once(SIGINT)"));
    assert_eq!(
        parsed["dlopen"]["code"],
        Value::String(String::from("ERR_ACCESS_DENIED"))
    );
    assert!(parsed["dlopen"]["message"]
        .as_str()
        .expect("dlopen message")
        .contains("process.dlopen"));
    assert_eq!(
        parsed["nativeAddon"]["code"],
        Value::String(String::from("ERR_ACCESS_DENIED"))
    );
    assert!(parsed["nativeAddon"]["message"]
        .as_str()
        .expect("native addon message")
        .contains("native addon loading"));
}

#[test]
fn javascript_execution_hardens_exec_and_execsync_child_process_calls() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("child.mjs"),
        r#"
import fs from 'node:fs';

const result = {
  marker: process.argv[2] ?? null,
};

try {
  result.secret = fs.readFileSync('/etc/passwd', 'utf8').slice(0, 16);
} catch (error) {
  result.readError = {
    code: error.code ?? null,
    message: error.message,
  };
}

console.log(JSON.stringify(result));
"#,
    );
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
const { exec, execSync } = require('node:child_process');
const execAsync = (command) =>
  new Promise((resolve, reject) => {
    exec(command, (error, stdout, stderr) => {
      if (error) {
        error.stdout = stdout;
        error.stderr = stderr;
        reject(error);
        return;
      }

      resolve({ stdout, stderr });
    });
  });
const result = {};

result.execSync = JSON.parse(
  execSync('node ./child.mjs sync', { encoding: 'utf8' }).trim(),
);
result.exec = JSON.parse((await execAsync('node ./child.mjs async')).stdout.trim());

try {
  execSync('cat /etc/passwd', { encoding: 'utf8' });
  result.hostExecSync = 'unexpected';
} catch (error) {
  result.hostExecSync = {
    code: error.code ?? null,
    message: error.message,
  };
}

try {
  await execAsync('cat /etc/passwd');
  result.hostExec = 'unexpected';
} catch (error) {
  result.hostExec = {
    code: error.code ?? null,
    message: error.message,
  };
}

console.log(JSON.stringify(result));
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let cwd_host_path = temp.path().to_string_lossy().replace('\\', "\\\\");
    let env = BTreeMap::from([
        (
            String::from("AGENT_OS_GUEST_PATH_MAPPINGS"),
            format!("[{{\"guestPath\":\"/root\",\"hostPath\":\"{cwd_host_path}\"}}]"),
        ),
        (
            String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
            String::from(
                "[\"assert\",\"buffer\",\"console\",\"child_process\",\"crypto\",\"events\",\"fs\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
            ),
        ),
    ]);

    let (stdout, stderr, exit_code) = run_javascript_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        env,
    );

    assert_eq!(exit_code, 0, "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse exec hardening JSON");

    assert_eq!(
        parsed["execSync"]["marker"],
        Value::String(String::from("sync"))
    );
    assert_eq!(
        parsed["exec"]["marker"],
        Value::String(String::from("async"))
    );
    assert!(
        parsed["execSync"]["secret"].is_null(),
        "execSync should not expose host file contents: {stdout}"
    );
    assert!(
        parsed["exec"]["secret"].is_null(),
        "exec should not expose host file contents: {stdout}"
    );
    assert_eq!(
        parsed["hostExecSync"]["code"],
        Value::String(String::from("ERR_ACCESS_DENIED"))
    );
    assert!(parsed["hostExecSync"]["message"]
        .as_str()
        .expect("execSync denial message")
        .contains("child_process.execSync"));
    assert_eq!(
        parsed["hostExec"]["code"],
        Value::String(String::from("ERR_ACCESS_DENIED"))
    );
    assert!(parsed["hostExec"]["message"]
        .as_str()
        .expect("exec denial message")
        .contains("child_process.exec"));
}
