#![cfg(unix)]

use agent_os_execution::{
    CreateJavascriptContextRequest, CreateWasmContextRequest, JavascriptExecutionEngine,
    StartJavascriptExecutionRequest, StartWasmExecutionRequest, WasmExecutionEngine,
};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

const ARG_PREFIX: &str = "ARG=";
const INVOCATION_BREAK: &str = "--END--";
const NODE_ALLOW_FS_READ_FLAG: &str = "--allow-fs-read=";
const NODE_ALLOW_FS_WRITE_FLAG: &str = "--allow-fs-write=";

struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &Path) -> Self {
        let previous = std::env::var(key).ok();
        // SAFETY: This test binary controls its own process environment and uses a
        // single test to avoid concurrent environment mutation within the process.
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => {
                // SAFETY: See EnvVarGuard::set; restoring the test process env is
                // limited to this single-threaded test scope.
                unsafe {
                    std::env::set_var(self.key, value);
                }
            }
            None => {
                // SAFETY: See EnvVarGuard::set; restoring the test process env is
                // limited to this single-threaded test scope.
                unsafe {
                    std::env::remove_var(self.key);
                }
            }
        }
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")))
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|error| panic!("canonicalize {}: {error}", path.display()))
}

fn write_fake_node_binary(path: &Path, log_path: &Path) {
    let script = format!(
        "#!/bin/sh\nset -eu\nlog=\"{}\"\nfor arg in \"$@\"; do\n  printf 'ARG=%s\\n' \"$arg\" >> \"$log\"\ndone\nprintf '%s\\n' '{}' >> \"$log\"\nexit 0\n",
        log_path.display(),
        INVOCATION_BREAK,
    );
    fs::write(path, script).expect("write fake node binary");
    let mut permissions = fs::metadata(path)
        .expect("fake node metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod fake node binary");
}

fn parse_invocations(log_path: &Path) -> Vec<Vec<String>> {
    let contents = fs::read_to_string(log_path).expect("read invocation log");
    let separator = format!("{INVOCATION_BREAK}\n");
    contents
        .split(&separator)
        .filter(|block| !block.trim().is_empty())
        .map(|block| {
            block
                .lines()
                .filter_map(|line| line.strip_prefix(ARG_PREFIX))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn read_flags(args: &[String]) -> Vec<&str> {
    args.iter()
        .filter_map(|arg| arg.strip_prefix(NODE_ALLOW_FS_READ_FLAG))
        .collect()
}

fn write_flags(args: &[String]) -> Vec<&str> {
    args.iter()
        .filter_map(|arg| arg.strip_prefix(NODE_ALLOW_FS_WRITE_FLAG))
        .collect()
}

#[test]
fn node_permission_flags_do_not_expose_workspace_root_or_entrypoint_parent_writes() {
    let temp = tempdir().expect("create temp dir");
    let fake_node_path = temp.path().join("fake-node.sh");
    let log_path = temp.path().join("node-args.log");
    write_fake_node_binary(&fake_node_path, &log_path);
    let _node_binary = EnvVarGuard::set("AGENT_OS_NODE_BINARY", &fake_node_path);

    let js_cwd = temp.path().join("js-project");
    let js_entry_dir = js_cwd.join("nested");
    fs::create_dir_all(&js_entry_dir).expect("create js entry dir");
    fs::write(js_entry_dir.join("entry.mjs"), "console.log('ignored');").expect("write js entry");

    let mut js_engine = JavascriptExecutionEngine::default();
    let js_context = js_engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let js_result = js_engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: js_context.context_id,
            argv: vec![String::from("./nested/entry.mjs")],
            env: BTreeMap::new(),
            cwd: js_cwd.clone(),
        })
        .expect("start javascript execution")
        .wait()
        .expect("wait for javascript execution");
    assert_eq!(js_result.exit_code, 0);

    let wasm_cwd = temp.path().join("wasm-project");
    let wasm_module_dir = wasm_cwd.join("modules");
    fs::create_dir_all(&wasm_module_dir).expect("create wasm module dir");
    fs::write(wasm_module_dir.join("guest.wasm"), []).expect("write wasm module");

    let mut wasm_engine = WasmExecutionEngine::default();
    let wasm_context = wasm_engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm"),
        module_path: Some(String::from("./modules/guest.wasm")),
    });
    let wasm_result = wasm_engine
        .start_execution(StartWasmExecutionRequest {
            vm_id: String::from("vm-wasm"),
            context_id: wasm_context.context_id,
            argv: vec![String::from("./modules/guest.wasm")],
            env: BTreeMap::new(),
            cwd: wasm_cwd.clone(),
        })
        .expect("start wasm execution")
        .wait()
        .expect("wait for wasm execution");
    assert_eq!(wasm_result.exit_code, 0);

    let invocations = parse_invocations(&log_path);
    assert_eq!(
        invocations.len(),
        3,
        "expected javascript exec plus wasm prewarm and exec"
    );

    let workspace_root = canonical(&workspace_root()).display().to_string();
    let js_entry_parent = canonical(&js_entry_dir).display().to_string();
    let wasm_module_parent = canonical(&wasm_module_dir).display().to_string();

    let javascript_args = &invocations[0];
    let javascript_reads = read_flags(javascript_args);
    let javascript_writes = write_flags(javascript_args);
    assert!(
        !javascript_reads
            .iter()
            .any(|path| *path == workspace_root.as_str()),
        "javascript read flags should not include workspace root: {javascript_args:?}"
    );
    assert!(
        javascript_reads
            .iter()
            .any(|path| *path == js_entry_parent.as_str()),
        "javascript read flags should include the entrypoint parent: {javascript_args:?}"
    );
    assert!(
        !javascript_writes
            .iter()
            .any(|path| *path == js_entry_parent.as_str()),
        "javascript write flags should not include the entrypoint parent: {javascript_args:?}"
    );

    for wasm_args in &invocations[1..] {
        let wasm_reads = read_flags(wasm_args);
        let wasm_writes = write_flags(wasm_args);
        assert!(
            !wasm_reads
                .iter()
                .any(|path| *path == workspace_root.as_str()),
            "wasm read flags should not include workspace root: {wasm_args:?}"
        );
        assert!(
            wasm_reads
                .iter()
                .any(|path| *path == wasm_module_parent.as_str()),
            "wasm read flags should include the module parent: {wasm_args:?}"
        );
        assert!(
            !wasm_writes
                .iter()
                .any(|path| *path == wasm_module_parent.as_str()),
            "wasm write flags should not include the module parent: {wasm_args:?}"
        );
    }
}
