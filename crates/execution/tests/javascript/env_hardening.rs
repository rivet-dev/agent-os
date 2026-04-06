use super::*;

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

    let mut engine = new_test_engine();
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

    let mut engine = new_test_engine();
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

    let mut engine = new_test_engine();
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

    let mut engine = new_test_engine();
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
fn javascript_execution_accepts_host_absolute_entrypoints_under_guest_path_mappings() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
console.log(JSON.stringify({
  argv1: process.argv[1],
  cwd: process.cwd(),
  loaded: true,
}));
"#,
    );

    let mut engine = new_test_engine();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let cwd_host_path = temp.path().to_string_lossy().replace('\\', "\\\\");
    let entrypoint_host_path = temp
        .path()
        .join("entry.mjs")
        .to_string_lossy()
        .replace('\\', "\\\\");
    let env = BTreeMap::from([
        (
            String::from("AGENT_OS_GUEST_PATH_MAPPINGS"),
            format!("[{{\"guestPath\":\"/root\",\"hostPath\":\"{cwd_host_path}\"}}]"),
        ),
        (
            String::from("AGENT_OS_GUEST_ENTRYPOINT"),
            String::from("/root/entry.mjs"),
        ),
    ]);

    let (stdout, stderr, exit_code) = run_javascript_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        vec![entrypoint_host_path],
        env,
    );

    assert_eq!(exit_code, 0, "stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");

    let parsed: Value =
        serde_json::from_str(stdout.trim()).expect("parse absolute entrypoint JSON");
    assert_eq!(parsed["argv1"], Value::String(String::from("/root/entry.mjs")));
    assert_eq!(parsed["cwd"], Value::String(String::from("/root")));
    assert_eq!(parsed["loaded"], Value::Bool(true));
}

#[test]
fn javascript_execution_uses_virtual_root_when_no_guest_path_mapping_exists() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("dep.cjs"),
        "module.exports = { answer: 42 };\n",
    );
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
const result = {
  cwd: process.cwd(),
  resolved: require.resolve('./dep.cjs'),
};

try {
  require.resolve('./missing.cjs');
  result.resolveMissing = 'unexpected';
} catch (error) {
  result.resolveMissing = {
    message: error.message,
    stack: error.stack ?? null,
  };
}

console.log(JSON.stringify(result));
"#,
    );

    let mut engine = new_test_engine();
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
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse cwd fallback JSON");
    let host_path = temp.path().to_string_lossy();

    assert_eq!(parsed["cwd"], Value::String(String::from("/root")));
    assert_eq!(
        parsed["resolved"],
        Value::String(String::from("/root/dep.cjs"))
    );
    let message = parsed["resolveMissing"]["message"]
        .as_str()
        .expect("missing resolve message");
    let stack = parsed["resolveMissing"]["stack"]
        .as_str()
        .expect("missing resolve stack");
    assert!(
        message.contains("/root/missing.cjs"),
        "message should use virtual cwd fallback: {message}"
    );
    assert!(
        stack.contains("/root/entry.mjs"),
        "stack should use virtual cwd fallback: {stack}"
    );
    assert!(
        !message.contains(host_path.as_ref()),
        "message leaked host path: {message}"
    );
    assert!(
        !stack.contains(host_path.as_ref()),
        "stack leaked host path: {stack}"
    );
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

    let mut engine = new_test_engine();
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
fn javascript_execution_blocks_remaining_process_property_leaks() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
function summarize(mod) {
  return {
    platform: mod.platform,
    arch: mod.arch,
    version: mod.version,
    release: mod.release,
    config: mod.config,
    versions: mod.versions,
    memoryUsage: typeof mod.memoryUsage === "function" ? mod.memoryUsage() : null,
    memoryUsageRss:
      typeof mod.memoryUsage === "function" && typeof mod.memoryUsage.rss === "function"
        ? mod.memoryUsage.rss()
        : null,
    uptime: typeof mod.uptime === "function" ? mod.uptime() : null,
  };
}

const result = {
  globalProcess: summarize(process),
  requireProcess: summarize(require("node:process")),
  builtinProcess: summarize(process.getBuiltinModule("node:process")),
};

console.log(JSON.stringify(result));
"#,
    );

    let mut engine = new_test_engine();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let env = BTreeMap::from([
        (
            String::from("AGENT_OS_VIRTUAL_OS_ARCH"),
            String::from("arm64"),
        ),
        (
            String::from("AGENT_OS_VIRTUAL_PROCESS_VERSION"),
            String::from("v24.0.0"),
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
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse process leak JSON");
    for key in ["globalProcess", "requireProcess", "builtinProcess"] {
        let summary = &parsed[key];
        assert_eq!(summary["platform"], Value::String(String::from("linux")));
        assert_eq!(summary["arch"], Value::String(String::from("arm64")));
        assert_eq!(summary["version"], Value::String(String::from("v24.0.0")));
        assert_eq!(
            summary["release"]["name"],
            Value::String(String::from("node"))
        );
        assert_eq!(
            summary["release"]["lts"],
            Value::String(String::from("Agent OS"))
        );
        assert!(summary["release"]["sourceUrl"].is_null());
        assert!(summary["release"]["headersUrl"].is_null());
        assert_eq!(
            summary["config"]["variables"]["host_arch"],
            Value::String(String::from("arm64"))
        );
        assert_eq!(
            summary["config"]["variables"]["node_shared"],
            Value::Bool(false)
        );
        assert_eq!(
            summary["config"]["variables"]["node_use_openssl"],
            Value::Bool(false)
        );
        assert_eq!(
            summary["versions"]["node"],
            Value::String(String::from("24.0.0"))
        );
        assert_eq!(
            summary["versions"]["openssl"],
            Value::String(String::from("0.0.0"))
        );
        assert_eq!(
            summary["versions"]["v8"],
            Value::String(String::from("0.0"))
        );
        assert_eq!(
            summary["versions"]["zlib"],
            Value::String(String::from("0.0.0"))
        );

        let memory_usage = summary["memoryUsage"]
            .as_object()
            .expect("memory usage object");
        for field in ["rss", "heapTotal", "heapUsed", "external", "arrayBuffers"] {
            assert!(
                memory_usage[field].as_u64().unwrap_or_default() > 0
                    || field == "external"
                    || field == "arrayBuffers"
            );
        }
        assert_eq!(
            summary["memoryUsageRss"], summary["memoryUsage"]["rss"],
            "memoryUsage.rss() should match memoryUsage().rss for {key}"
        );
        let uptime = summary["uptime"].as_f64().expect("uptime number");
        assert!(uptime >= 0.0, "uptime should not be negative for {key}");
        assert!(
            uptime < 5.0,
            "uptime should be VM-scoped for {key}, got {uptime}"
        );
    }
}

#[test]
fn javascript_execution_virtualizes_os_module() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import os from "node:os";

function summarize(mod) {
  return {
    hostname: mod.hostname(),
    cpus: mod.cpus(),
    totalmem: mod.totalmem(),
    freemem: mod.freemem(),
    homedir: mod.homedir(),
    tmpdir: mod.tmpdir(),
    platform: mod.platform(),
    type: mod.type(),
    release: mod.release(),
    version: typeof mod.version === "function" ? mod.version() : null,
    arch: typeof mod.arch === "function" ? mod.arch() : null,
    machine: typeof mod.machine === "function" ? mod.machine() : null,
    availableParallelism:
      typeof mod.availableParallelism === "function"
        ? mod.availableParallelism()
        : null,
    loadavg: typeof mod.loadavg === "function" ? mod.loadavg() : null,
    uptime: typeof mod.uptime === "function" ? mod.uptime() : null,
    networkInterfaces: mod.networkInterfaces(),
    userInfo: mod.userInfo(),
    userInfoBuffer: mod.userInfo({ encoding: "buffer" }),
    getPriority: typeof mod.getPriority === "function" ? mod.getPriority(0) : null,
  };
}

const result = {
  importOs: summarize(os),
  requireOs: summarize(require("node:os")),
  builtinOs: summarize(process.getBuiltinModule("node:os")),
};

try {
  os.setPriority(0, 0);
  result.setPriority = "unexpected";
} catch (error) {
  result.setPriority = {
    code: error.code ?? null,
    message: error.message,
  };
}

console.log(JSON.stringify(result));
"#,
    );

    let mut engine = new_test_engine();
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
        (String::from("HOME"), String::from("/root")),
        (String::from("SHELL"), String::from("/bin/bash")),
        (String::from("AGENT_OS_VIRTUAL_PROCESS_UID"), String::from("0")),
        (String::from("AGENT_OS_VIRTUAL_PROCESS_GID"), String::from("0")),
        (
            String::from("AGENT_OS_VIRTUAL_OS_HOSTNAME"),
            String::from("agent-os-test"),
        ),
        (
            String::from("AGENT_OS_VIRTUAL_OS_CPU_COUNT"),
            String::from("4"),
        ),
        (
            String::from("AGENT_OS_VIRTUAL_OS_CPU_MODEL"),
            String::from("Agent OS Test CPU"),
        ),
        (
            String::from("AGENT_OS_VIRTUAL_OS_TOTALMEM"),
            String::from("2147483648"),
        ),
        (
            String::from("AGENT_OS_VIRTUAL_OS_FREEMEM"),
            String::from("1073741824"),
        ),
        (
            String::from("AGENT_OS_VIRTUAL_OS_RELEASE"),
            String::from("6.8.0-agent-os-test"),
        ),
        (
            String::from("AGENT_OS_VIRTUAL_OS_VERSION"),
            String::from("#1 SMP PREEMPT_DYNAMIC Agent OS Test"),
        ),
        (
            String::from("AGENT_OS_VIRTUAL_OS_ARCH"),
            String::from("x64"),
        ),
        (
            String::from("AGENT_OS_VIRTUAL_OS_MACHINE"),
            String::from("x86_64"),
        ),
        (
            String::from("AGENT_OS_VIRTUAL_OS_USER"),
            String::from("agent"),
        ),
        (
            String::from("AGENT_OS_VIRTUAL_OS_SHELL"),
            String::from("/bin/bash"),
        ),
        (
            String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
            String::from(
                "[\"assert\",\"buffer\",\"console\",\"child_process\",\"crypto\",\"events\",\"fs\",\"os\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
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
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse os JSON");

    for surface in ["importOs", "requireOs", "builtinOs"] {
        assert_eq!(
            parsed[surface]["hostname"],
            Value::String(String::from("agent-os-test"))
        );
        assert_eq!(
            parsed[surface]["homedir"],
            Value::String(String::from("/root"))
        );
        assert_eq!(
            parsed[surface]["tmpdir"],
            Value::String(String::from("/tmp"))
        );
        assert_eq!(
            parsed[surface]["platform"],
            Value::String(String::from("linux"))
        );
        assert_eq!(
            parsed[surface]["type"],
            Value::String(String::from("Linux"))
        );
        assert_eq!(
            parsed[surface]["release"],
            Value::String(String::from("6.8.0-agent-os-test"))
        );
        assert_eq!(
            parsed[surface]["version"],
            Value::String(String::from("#1 SMP PREEMPT_DYNAMIC Agent OS Test"))
        );
        assert_eq!(parsed[surface]["arch"], Value::String(String::from("x64")));
        assert_eq!(
            parsed[surface]["machine"],
            Value::String(String::from("x86_64"))
        );
        assert_eq!(parsed[surface]["availableParallelism"], Value::from(4));
        assert_eq!(parsed[surface]["totalmem"], Value::from(2_147_483_648_u64));
        assert_eq!(parsed[surface]["freemem"], Value::from(1_073_741_824_u64));
        assert_eq!(parsed[surface]["loadavg"], json!([0, 0, 0]));
        assert_eq!(parsed[surface]["uptime"], Value::from(0));
        assert_eq!(parsed[surface]["getPriority"], Value::from(0));
        assert_eq!(parsed[surface]["cpus"].as_array().map(Vec::len), Some(4));
        assert_eq!(
            parsed[surface]["cpus"][0]["model"],
            Value::String(String::from("Agent OS Test CPU"))
        );
        assert_eq!(
            parsed[surface]["userInfo"]["username"],
            Value::String(String::from("agent"))
        );
        assert_eq!(parsed[surface]["userInfo"]["uid"], Value::from(0));
        assert_eq!(parsed[surface]["userInfo"]["gid"], Value::from(0));
        assert_eq!(
            parsed[surface]["userInfo"]["shell"],
            Value::String(String::from("/bin/bash"))
        );
        assert_eq!(
            parsed[surface]["userInfo"]["homedir"],
            Value::String(String::from("/root"))
        );
        assert_eq!(
            parsed[surface]["userInfoBuffer"]["username"]["type"],
            Value::String(String::from("Buffer"))
        );
        assert_eq!(
            parsed[surface]["userInfoBuffer"]["shell"]["type"],
            Value::String(String::from("Buffer"))
        );

        let interfaces = parsed[surface]["networkInterfaces"]
            .as_object()
            .expect("network interfaces object");
        assert_eq!(interfaces.len(), 1);
        assert!(interfaces.contains_key("lo"));
        let loopback = interfaces["lo"].as_array().expect("loopback interfaces");
        assert_eq!(loopback.len(), 2);
        assert_eq!(
            loopback[0]["address"],
            Value::String(String::from("127.0.0.1"))
        );
        assert_eq!(loopback[0]["internal"], Value::Bool(true));
        assert_eq!(loopback[1]["address"], Value::String(String::from("::1")));
    }

    assert_eq!(
        parsed["setPriority"]["code"],
        Value::String(String::from("ERR_ACCESS_DENIED"))
    );
    assert!(parsed["setPriority"]["message"]
        .as_str()
        .expect("setPriority message")
        .contains("os.setPriority"));
}

#[test]
fn javascript_execution_os_module_safe_defaults_ignore_host_env() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import os from "node:os";

console.log(JSON.stringify({
  hostname: os.hostname(),
  homedir: os.homedir(),
  tmpdir: os.tmpdir(),
  userInfo: os.userInfo(),
}));
"#,
    );

    let mut engine = new_test_engine();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let env = BTreeMap::from([
        (
            String::from("HOME"),
            String::from("/Users/host-user/should-not-leak"),
        ),
        (
            String::from("USER"),
            String::from("host-user-should-not-leak"),
        ),
        (
            String::from("LOGNAME"),
            String::from("host-logname-should-not-leak"),
        ),
        (
            String::from("TMPDIR"),
            String::from("/var/folders/host-tmp-should-not-leak"),
        ),
        (
            String::from("TEMP"),
            String::from("/tmp/host-temp-should-not-leak"),
        ),
        (
            String::from("TMP"),
            String::from("/tmp/host-tmp-should-not-leak"),
        ),
        (
            String::from("HOSTNAME"),
            String::from("host-machine-should-not-leak"),
        ),
        (String::from("SHELL"), String::from("/bin/zsh")),
        (
            String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
            String::from(
                "[\"assert\",\"buffer\",\"console\",\"child_process\",\"crypto\",\"events\",\"fs\",\"os\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
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
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse os defaults JSON");

    assert_eq!(parsed["hostname"], Value::String(String::from("agent-os")));
    assert_eq!(parsed["homedir"], Value::String(String::from("/root")));
    assert_eq!(parsed["tmpdir"], Value::String(String::from("/tmp")));
    assert_eq!(
        parsed["userInfo"]["username"],
        Value::String(String::from("root"))
    );
    assert_eq!(
        parsed["userInfo"]["shell"],
        Value::String(String::from("/bin/sh"))
    );
    assert_eq!(
        parsed["userInfo"]["homedir"],
        Value::String(String::from("/root"))
    );
}

#[test]
fn javascript_execution_allows_supported_process_signal_handlers_and_denies_native_addons() {
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
  const returned = process.on('SIGTERM', () => {});
  result.signalOnReturnedSelf = returned === process;
  process.removeAllListeners('SIGTERM');
} catch (error) {
  result.signalOn = { code: error.code ?? null, message: error.message };
}

try {
  const returned = process.once('SIGINT', () => {});
  result.signalOnceReturnedSelf = returned === process;
  process.removeAllListeners('SIGINT');
} catch (error) {
  result.signalOnce = { code: error.code ?? null, message: error.message };
}

try {
  const returned = process.on('SIGCHLD', () => {});
  result.sigchldReturnedSelf = returned === process;
  process.removeAllListeners('SIGCHLD');
} catch (error) {
  result.sigchld = { code: error.code ?? null, message: error.message };
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

    let mut engine = new_test_engine();
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
    assert_eq!(parsed["signalOnReturnedSelf"], Value::Bool(true));
    assert_eq!(parsed.get("signalOn"), None);
    assert_eq!(parsed["signalOnceReturnedSelf"], Value::Bool(true));
    assert_eq!(parsed.get("signalOnce"), None);
    assert_eq!(parsed.get("sigchld"), None);
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
fn javascript_execution_process_get_builtin_module_returns_undefined_for_denied_probes() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
const result = {};

for (const specifier of ["node:v8", "node:vm", "node:worker_threads", "node:inspector"]) {
  try {
    result[specifier] = process.getBuiltinModule?.(specifier) === undefined;
  } catch (error) {
    result[specifier] = {
      code: error.code ?? null,
      message: error.message,
    };
  }
}

console.log(JSON.stringify(result));
"#,
    );

    let mut engine = new_test_engine();
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
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse builtin probe JSON");
    for specifier in ["node:v8", "node:vm", "node:worker_threads", "node:inspector"] {
        assert_eq!(
            parsed[specifier],
            Value::Bool(true),
            "expected process.getBuiltinModule({specifier}) to return undefined"
        );
    }
}

#[test]
fn javascript_execution_still_starts_with_fail_closed_property_hardening() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
console.log(JSON.stringify({
  envType: typeof process.env,
  cwdType: typeof process.cwd,
  execPathType: typeof process.execPath,
}));
"#,
    );

    let mut engine = new_test_engine();
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
    assert_eq!(parsed["envType"], Value::String(String::from("object")));
    assert_eq!(parsed["cwdType"], Value::String(String::from("function")));
    assert_eq!(
        parsed["execPathType"],
        Value::String(String::from("string"))
    );
}

#[test]
fn javascript_execution_hardens_exec_and_execsync_child_process_calls() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
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

console.log(JSON.stringify({
  execSync: JSON.parse(execSync('node ./child.mjs sync', { encoding: 'utf8' }).trim()),
  exec: JSON.parse((await execAsync('node ./child.mjs async')).stdout.trim()),
}));
"#,
    );

    let mut engine = new_test_engine();
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
    let mut execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env,
            cwd: temp.path().to_path_buf(),
        })
        .expect("start JavaScript execution");

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut exit_code = None;
    let mut next_child_pid = 40_u64;
    let mut child_events = BTreeMap::<String, Vec<Value>>::new();
    let mut methods = Vec::new();

    while exit_code.is_none() {
        match execution
            .poll_event(Duration::from_secs(5))
            .expect("poll execution event")
        {
            Some(JavascriptExecutionEvent::Stdout(chunk)) => stdout.extend(chunk),
            Some(JavascriptExecutionEvent::Stderr(chunk)) => stderr.extend(chunk),
            Some(JavascriptExecutionEvent::SignalState { .. }) => {}
            Some(JavascriptExecutionEvent::Exited(code)) => exit_code = Some(code),
            Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => {
                methods.push(request.method.clone());
                match request.method.as_str() {
                    "child_process.spawn" => {
                        let payload = request.args[0].as_object().expect("spawn payload");
                        let command = payload["command"].as_str().expect("spawn command");
                        let args = payload["args"]
                            .as_array()
                            .expect("spawn args")
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect::<Vec<_>>();
                        let shell = payload["options"]["shell"].as_bool().unwrap_or(false);
                        let marker = if shell {
                            command
                                .split_whitespace()
                                .last()
                                .expect("shell marker")
                                .to_owned()
                        } else {
                            args.last().expect("spawn marker").clone()
                        };
                        let child_id = format!("child-{next_child_pid}");
                        let stdout_payload = format!("{{\"marker\":\"{marker}\"}}\n");
                        child_events.insert(
                            child_id.clone(),
                            vec![
                                json!({
                                    "type": "stdout",
                                    "data": stdout_payload,
                                }),
                                json!({
                                    "type": "exit",
                                    "exitCode": 0,
                                }),
                            ],
                        );
                        execution
                            .respond_sync_rpc_success(
                                request.id,
                                json!({
                                    "childId": child_id,
                                    "pid": next_child_pid,
                                    "command": command,
                                    "args": args,
                                }),
                            )
                            .expect("respond to child_process.spawn");
                        next_child_pid += 1;
                    }
                    "child_process.poll" => {
                        let child_id = request.args[0].as_str().expect("poll child id");
                        let next = child_events
                            .get_mut(child_id)
                            .and_then(|events| {
                                if events.is_empty() {
                                    None
                                } else {
                                    Some(events.remove(0))
                                }
                            })
                            .unwrap_or(Value::Null);
                        execution
                            .respond_sync_rpc_success(request.id, next)
                            .expect("respond to child_process.poll");
                    }
                    other => panic!("unexpected child_process sync RPC method: {other}"),
                }
            }
            None => panic!("timed out waiting for JavaScript execution event"),
        }
    }

    let stdout = String::from_utf8(stdout).expect("stdout utf8");
    let stderr = String::from_utf8(stderr).expect("stderr utf8");
    assert_eq!(exit_code, Some(0), "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse child_process JSON");
    assert_eq!(
        parsed["execSync"]["marker"],
        Value::String(String::from("sync"))
    );
    assert_eq!(
        parsed["exec"]["marker"],
        Value::String(String::from("async"))
    );
    assert!(methods.iter().any(|method| method == "child_process.spawn"));
    assert!(methods.iter().any(|method| method == "child_process.poll"));
}

#[test]
fn javascript_execution_strips_internal_env_from_child_process_rpc_payloads() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
const { spawnSync } = require('node:child_process');

spawnSync('node', ['./child.mjs'], {
  env: {
    VISIBLE_MARKER: 'child-visible',
    AGENT_OS_GUEST_PATH_MAPPINGS: 'user-override',
    AGENT_OS_VIRTUAL_PROCESS_UID: '999',
    AGENT_OS_VIRTUAL_OS_HOSTNAME: 'leak-attempt',
  },
});
"#,
    );

    let mut engine = new_test_engine();
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
        (String::from("VISIBLE_MARKER"), String::from("parent-visible")),
        (String::from("AGENT_OS_VIRTUAL_PROCESS_UID"), String::from("0")),
        (
            String::from("AGENT_OS_VIRTUAL_OS_HOSTNAME"),
            String::from("agent-os-test"),
        ),
        (
            String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
            String::from(
                "[\"assert\",\"buffer\",\"console\",\"child_process\",\"crypto\",\"events\",\"fs\",\"path\",\"querystring\",\"stream\",\"string_decoder\",\"timers\",\"url\",\"util\",\"zlib\"]",
            ),
        ),
    ]);
    let mut execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env,
            cwd: temp.path().to_path_buf(),
        })
        .expect("start JavaScript execution");

    let mut stderr = Vec::new();
    let mut exit_code = None;
    let mut observed_env = None;

    while exit_code.is_none() {
        match execution
            .poll_event(Duration::from_secs(5))
            .expect("poll execution event")
        {
            Some(JavascriptExecutionEvent::Stdout(_chunk)) => {}
            Some(JavascriptExecutionEvent::Stderr(chunk)) => stderr.extend(chunk),
            Some(JavascriptExecutionEvent::SignalState { .. }) => {}
            Some(JavascriptExecutionEvent::Exited(code)) => exit_code = Some(code),
            Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => {
                match request.method.as_str() {
                    "child_process.spawn" => {
                        let payload = request.args[0].as_object().expect("spawn payload");
                        observed_env = Some(
                            payload["options"]["env"]
                                .as_object()
                                .expect("spawn env")
                                .clone(),
                        );
                        execution
                            .respond_sync_rpc_success(
                                request.id,
                                json!({
                                    "childId": "child-1",
                                    "pid": 41,
                                    "command": payload["command"],
                                    "args": payload["args"],
                                }),
                            )
                            .expect("respond to child_process.spawn");
                    }
                    "child_process.poll" => {
                        execution
                            .respond_sync_rpc_success(
                                request.id,
                                json!({
                                    "type": "exit",
                                    "exitCode": 0,
                                }),
                            )
                            .expect("respond to child_process.poll");
                    }
                    other => panic!("unexpected child_process sync RPC method: {other}"),
                }
            }
            None => panic!("timed out waiting for JavaScript execution event"),
        }
    }

    let stderr = String::from_utf8(stderr).expect("stderr utf8");
    assert_eq!(exit_code, Some(0), "stderr: {stderr}");
    let env = observed_env.expect("observed child env");
    assert_eq!(
        env.get("VISIBLE_MARKER"),
        Some(&Value::String(String::from("child-visible")))
    );
    assert!(!env.contains_key("AGENT_OS_GUEST_PATH_MAPPINGS"));
    assert!(!env.contains_key("AGENT_OS_VIRTUAL_PROCESS_UID"));
    assert!(!env.contains_key("AGENT_OS_VIRTUAL_OS_HOSTNAME"));
}
