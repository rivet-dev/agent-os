use super::*;
#[cfg(unix)]
use std::os::unix::fs::symlink;

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

    let mut first_engine = new_test_engine();
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

    let mut second_engine = new_test_engine();
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

    let mut first_engine = new_test_engine();
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

    let mut second_engine = new_test_engine();
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

    let mut engine = new_test_engine();
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

    let mut engine = new_test_engine();
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

    let mut engine = new_test_engine();
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

    let mut engine = new_test_engine();
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

    let mut engine = new_test_engine();
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

    let mut engine = new_test_engine();
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

#[cfg(unix)]
#[test]
fn javascript_execution_resolves_projected_pnpm_dependencies_in_guest_path_space() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let workspace_root = temp.path().join("workspace");
    let projected_root = workspace_root.join("packages/core/node_modules");
    let pnpm_root = workspace_root.join("node_modules/.pnpm");
    let package_root = pnpm_root.join("demo-projected@1.0.0/node_modules/demo-projected");
    let dependency_root = pnpm_root.join("demo-projected@1.0.0/node_modules/demo-dependency");

    fs::create_dir_all(package_root.join("dist")).expect("create projected package dir");
    fs::create_dir_all(&dependency_root).expect("create projected dependency dir");
    fs::create_dir_all(&projected_root).expect("create projected node_modules dir");

    write_fixture(
        &package_root.join("package.json"),
        "{\n  \"name\": \"demo-projected\",\n  \"type\": \"module\"\n}\n",
    );
    write_fixture(
        &package_root.join("dist/entry.js"),
        "import { answer, resolved } from 'demo-dependency';\nexport { answer, resolved };\n",
    );
    write_fixture(
        &dependency_root.join("package.json"),
        "{\n  \"name\": \"demo-dependency\",\n  \"type\": \"module\",\n  \"exports\": \"./index.js\"\n}\n",
    );
    write_fixture(
        &dependency_root.join("index.js"),
        "export const answer = 42;\nexport const resolved = import.meta.url;\n",
    );
    symlink(&package_root, projected_root.join("demo-projected"))
        .expect("symlink projected package into workspace node_modules");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
const mod = await import("/root/node_modules/demo-projected/dist/entry.js");
console.log(JSON.stringify(mod));
"#,
    );

    let mut engine = new_test_engine();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let projected_root_host_path = projected_root.to_string_lossy().replace('\\', "\\\\");
    let env = BTreeMap::from([(
        String::from("AGENT_OS_GUEST_PATH_MAPPINGS"),
        format!(
            "[{{\"guestPath\":\"/root/node_modules\",\"hostPath\":\"{projected_root_host_path}\"}}]"
        ),
    )]);

    let (stdout, stderr, exit_code) = run_javascript_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        env,
    );

    assert_eq!(exit_code, 0, "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse module json");
    assert_eq!(parsed["answer"], Value::from(42));
    let resolved = parsed["resolved"]
        .as_str()
        .expect("resolved dependency guest url");
    assert!(
        resolved.contains("/root/node_modules/.pnpm/demo-projected@1.0.0/node_modules/demo-dependency/index.js"),
        "resolved dependency should stay in guest path space: {resolved}"
    );
    assert!(
        !resolved.contains(workspace_root.to_string_lossy().as_ref()),
        "resolved dependency leaked host path: {resolved}"
    );
}

#[cfg(unix)]
#[test]
fn javascript_execution_resolves_projected_pnpm_cjs_dependencies_in_guest_path_space() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let workspace_root = temp.path().join("workspace");
    let projected_root = workspace_root.join("packages/core/node_modules");
    let pnpm_root = workspace_root.join("node_modules/.pnpm");
    let package_root = pnpm_root.join("demo-projected@1.0.0/node_modules/demo-projected");
    let dependency_root = pnpm_root.join("demo-projected@1.0.0/node_modules/demo-dependency");

    fs::create_dir_all(package_root.join("dist")).expect("create projected package dir");
    fs::create_dir_all(&dependency_root).expect("create projected dependency dir");
    fs::create_dir_all(&projected_root).expect("create projected node_modules dir");

    write_fixture(
        &package_root.join("package.json"),
        "{\n  \"name\": \"demo-projected\",\n  \"type\": \"commonjs\"\n}\n",
    );
    write_fixture(
        &package_root.join("dist/entry.cjs"),
        "const dep = require('demo-dependency');\nmodule.exports = { answer: dep.answer, resolved: require.resolve('demo-dependency') };\n",
    );
    write_fixture(
        &dependency_root.join("package.json"),
        "{\n  \"name\": \"demo-dependency\",\n  \"type\": \"commonjs\",\n  \"exports\": \"./index.cjs\"\n}\n",
    );
    write_fixture(
        &dependency_root.join("index.cjs"),
        "module.exports = { answer: 42 };\n",
    );
    symlink(&package_root, projected_root.join("demo-projected"))
        .expect("symlink projected package into workspace node_modules");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
const mod = await import("/root/node_modules/demo-projected/dist/entry.cjs");
console.log(JSON.stringify(mod.default));
"#,
    );

    let mut engine = new_test_engine();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let projected_root_host_path = projected_root.to_string_lossy().replace('\\', "\\\\");
    let env = BTreeMap::from([(
        String::from("AGENT_OS_GUEST_PATH_MAPPINGS"),
        format!(
            "[{{\"guestPath\":\"/root/node_modules\",\"hostPath\":\"{projected_root_host_path}\"}}]"
        ),
    )]);

    let (stdout, stderr, exit_code) = run_javascript_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        vec![String::from("./entry.mjs")],
        env,
    );

    assert_eq!(exit_code, 0, "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse module json");
    assert_eq!(parsed["answer"], Value::from(42));
    let resolved = parsed["resolved"]
        .as_str()
        .expect("resolved dependency guest path");
    assert!(
        resolved.contains("/root/node_modules/.pnpm/demo-projected@1.0.0/node_modules/demo-dependency/index.cjs"),
        "resolved dependency should stay in guest path space: {resolved}"
    );
    assert!(
        !resolved.contains(workspace_root.to_string_lossy().as_ref()),
        "resolved dependency leaked host path: {resolved}"
    );
}

#[test]
fn javascript_execution_translates_require_resolve_and_cjs_errors_to_guest_paths() {
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
  resolved: require.resolve('./dep.cjs'),
};

try {
  require.resolve('/root/missing.cjs');
  result.resolveMissing = 'unexpected';
} catch (error) {
  result.resolveMissing = {
    code: error.code ?? null,
    message: error.message,
    stack: error.stack ?? null,
    requireStack: error.requireStack ?? [],
  };
}

try {
  require('/root/missing.cjs');
  result.requireMissing = 'unexpected';
} catch (error) {
  result.requireMissing = {
    code: error.code ?? null,
    message: error.message,
    stack: error.stack ?? null,
    requireStack: error.requireStack ?? [],
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
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse require JSON");
    let host_path = temp.path().to_string_lossy();

    assert_eq!(
        parsed["resolved"],
        Value::String(String::from("/root/dep.cjs"))
    );

    for field in ["resolveMissing", "requireMissing"] {
        assert_eq!(
            parsed[field]["code"],
            Value::String(String::from("MODULE_NOT_FOUND"))
        );
        let message = parsed[field]["message"].as_str().expect("missing message");
        let stack = parsed[field]["stack"].as_str().expect("missing stack");
        assert!(message.contains("/root/missing.cjs"), "message: {message}");
        assert!(
            !message.contains(host_path.as_ref()),
            "message leaked host path: {message}"
        );
        assert!(
            !stack.contains(host_path.as_ref()),
            "stack leaked host path: {stack}"
        );

        let require_stack = parsed[field]["requireStack"]
            .as_array()
            .expect("require stack array");
        let mut saw_guest_path = false;
        for entry in require_stack {
            let entry = entry.as_str().expect("require stack entry");
            saw_guest_path |= entry.starts_with("/root/");
            assert!(
                !entry.contains(host_path.as_ref()),
                "requireStack leaked host path: {entry}"
            );
        }
        assert!(
            saw_guest_path,
            "requireStack should contain guest-visible paths"
        );
    }
}

#[test]
fn javascript_execution_blocks_cjs_require_from_hidden_parent_node_modules() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let guest_root = temp.path().join("guest-root");
    let guest_package_dir = guest_root.join("node_modules/visible-pkg");
    let hidden_parent_package_dir = temp.path().join("node_modules/host-only-pkg");
    fs::create_dir_all(&guest_package_dir).expect("create guest package dir");
    fs::create_dir_all(&hidden_parent_package_dir).expect("create hidden parent package dir");

    write_fixture(
        &guest_root.join("dep.cjs"),
        "module.exports = { answer: 41 };\n",
    );
    write_fixture(
        &guest_package_dir.join("package.json"),
        "{\n  \"name\": \"visible-pkg\",\n  \"main\": \"./index.js\"\n}\n",
    );
    write_fixture(
        &guest_package_dir.join("index.js"),
        "module.exports = { answer: 42 };\n",
    );
    write_fixture(
        &hidden_parent_package_dir.join("package.json"),
        "{\n  \"name\": \"host-only-pkg\",\n  \"main\": \"./index.js\"\n}\n",
    );
    write_fixture(
        &hidden_parent_package_dir.join("index.js"),
        "module.exports = { compromised: true };\n",
    );
    write_fixture(
        &guest_root.join("consumer.cjs"),
        r#"
const dep = require("./dep.cjs");
const visible = require("visible-pkg");

let hidden;
try {
  hidden = require("host-only-pkg");
} catch (error) {
  hidden = {
    code: error.code ?? null,
    message: error.message,
  };
}

module.exports = {
  dep: dep.answer,
  visible: visible.answer,
  hidden,
};
"#,
    );
    write_fixture(
        &guest_root.join("entry.mjs"),
        r#"
import result from "./consumer.cjs";
result.cacheKeys = Object.keys(require.cache)
  .filter((key) =>
    key.includes("consumer.cjs") ||
    key.includes("dep.cjs") ||
    key.includes("visible-pkg"),
  )
  .sort();
console.log(JSON.stringify(result));
"#,
    );

    let mut engine = new_test_engine();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let guest_root_host_path = guest_root.to_string_lossy().replace('\\', "\\\\");
    let env = BTreeMap::from([(
        String::from("AGENT_OS_GUEST_PATH_MAPPINGS"),
        format!("[{{\"guestPath\":\"/root\",\"hostPath\":\"{guest_root_host_path}\"}}]"),
    )]);

    let (stdout, stderr, exit_code) = run_javascript_execution(
        &mut engine,
        context.context_id,
        &guest_root,
        vec![String::from("./entry.mjs")],
        env,
    );

    assert_eq!(exit_code, 0, "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse CJS JSON");

    assert_eq!(parsed["dep"], Value::from(41));
    assert_eq!(parsed["visible"], Value::from(42));
    assert_eq!(
        parsed["hidden"]["code"],
        Value::String(String::from("MODULE_NOT_FOUND"))
    );
    let hidden_message = parsed["hidden"]["message"]
        .as_str()
        .expect("hidden module missing message");
    assert!(
        hidden_message.contains("host-only-pkg"),
        "message should mention blocked package: {hidden_message}"
    );

    let cache_keys = parsed["cacheKeys"].as_array().expect("cache keys array");
    let cache_key_values: Vec<&str> = cache_keys
        .iter()
        .map(|entry| entry.as_str().expect("cache key"))
        .collect();
    assert!(
        cache_key_values.contains(&"/root/consumer.cjs"),
        "consumer cache key should use guest path: {cache_key_values:?}"
    );
    assert!(
        cache_key_values.contains(&"/root/dep.cjs"),
        "dep cache key should use guest path: {cache_key_values:?}"
    );
    assert!(
        cache_key_values
            .iter()
            .any(|entry| entry.starts_with("/root/node_modules/visible-pkg/")),
        "package cache key should stay in guest path space: {cache_key_values:?}"
    );
}

#[test]
fn javascript_execution_translates_top_level_loader_stacks_to_guest_paths() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
export const broken = ;
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

    assert_eq!(stdout.trim(), "");
    assert_eq!(exit_code, 1, "stderr: {stderr}");
    let host_path = temp.path().to_string_lossy();
    assert!(
        stderr.contains("/root/entry.mjs"),
        "stderr should use guest path: {stderr}"
    );
    assert!(
        stderr.contains("SyntaxError"),
        "stderr should contain the parse failure: {stderr}"
    );
    assert!(
        !stderr.contains(host_path.as_ref()),
        "stderr leaked host path: {stderr}"
    );
}

#[test]
fn javascript_execution_scrubs_unmapped_host_paths_to_unknown() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let outside = tempdir().expect("create outside temp dir");
    let outside_path = outside
        .path()
        .join("outside-only.mjs")
        .to_string_lossy()
        .replace('\\', "\\\\");
    write_fixture(
        &temp.path().join("entry.mjs"),
        &format!(
            r#"
const hostOnlyPath = "{outside_path}";
const error = new Error(`boom at ${{hostOnlyPath}}`);
error.path = hostOnlyPath;
error.filename = hostOnlyPath;
throw error;
"#
        ),
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

    assert_eq!(stdout.trim(), "");
    assert_eq!(exit_code, 1, "stderr: {stderr}");
    assert!(
        stderr.contains("/unknown"),
        "stderr should redact unmapped host paths: {stderr}"
    );
    assert!(
        !stderr.contains(outside.path().to_string_lossy().as_ref()),
        "stderr leaked unmapped host path: {stderr}"
    );
}
