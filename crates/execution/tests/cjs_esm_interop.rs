use agent_os_execution::{
    javascript::ModuleResolutionTestHarness, CreateJavascriptContextRequest,
    JavascriptExecutionEngine, JavascriptExecutionResult, StartJavascriptExecutionRequest,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

struct Fixture {
    temp: TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            temp: TempDir::new().expect("create temp dir"),
        }
    }

    fn root(&self) -> &Path {
        self.temp.path()
    }

    fn host_path(&self, relative: &str) -> PathBuf {
        self.root().join(relative)
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.host_path(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        fs::write(path, contents).expect("write fixture");
    }

    fn write_json(&self, relative: &str, value: Value) {
        self.write(
            relative,
            &serde_json::to_string_pretty(&value).expect("serialize JSON"),
        );
    }

    fn resolver(&self) -> ModuleResolutionTestHarness {
        ModuleResolutionTestHarness::new(self.root())
    }
}

fn assert_import(fixture: &Fixture, specifier: &str, from_path: &str, expected: &str) {
    let mut resolver = fixture.resolver();
    assert_eq!(
        resolver.resolve_import(specifier, from_path),
        Some(String::from(expected))
    );
}

fn assert_require(fixture: &Fixture, specifier: &str, from_path: &str, expected: &str) {
    let mut resolver = fixture.resolver();
    assert_eq!(
        resolver.resolve_require(specifier, from_path),
        Some(String::from(expected))
    );
}

fn run_guest_result(
    fixture: &Fixture,
    entrypoint: &str,
    env: BTreeMap<String, String>,
) -> JavascriptExecutionResult {
    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from(entrypoint)],
            env,
            cwd: fixture.root().to_path_buf(),
            inline_code: None,
        })
        .expect("start JavaScript execution");

    execution.wait().expect("wait for JavaScript execution")
}

fn assert_guest_success(result: &JavascriptExecutionResult) {
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert_eq!(
        result.exit_code, 0,
        "guest exited with {}\nstdout:\n{}\nstderr:\n{}",
        result.exit_code, stdout, stderr
    );
    assert!(result.stderr.is_empty(), "unexpected stderr: {}", stderr);
}

fn run_guest_json(fixture: &Fixture, entrypoint: &str) -> Value {
    let result = run_guest_result(fixture, entrypoint, BTreeMap::new());
    assert_guest_success(&result);
    serde_json::from_slice(&result.stdout).expect("parse guest stdout as JSON")
}

#[test]
fn resolution_nested_exports_conditions_recurse_three_levels() {
    let fixture = Fixture::new();
    fixture.write_json(
        "node_modules/pkg/package.json",
        json!({
            "exports": {
                ".": {
                    "import": {
                        "node": {
                            "default": "./dist/node-default.mjs"
                        },
                        "default": "./dist/import-default.mjs"
                    },
                    "default": "./dist/fallback.mjs"
                }
            }
        }),
    );
    fixture.write(
        "node_modules/pkg/dist/node-default.mjs",
        "export default 'node';",
    );
    fixture.write(
        "node_modules/pkg/dist/import-default.mjs",
        "export default 'import-default';",
    );
    fixture.write(
        "node_modules/pkg/dist/fallback.mjs",
        "export default 'fallback';",
    );

    assert_import(
        &fixture,
        "pkg",
        "/root/project/index.mjs",
        "/root/node_modules/pkg/dist/node-default.mjs",
    );
}

#[test]
fn resolution_exports_array_and_condition_nesting_uses_first_valid_target() {
    let fixture = Fixture::new();
    fixture.write_json(
        "node_modules/pkg/package.json",
        json!({
            "exports": {
                ".": [
                    { "browser": "./dist/browser.mjs" },
                    {
                        "import": {
                            "node": {
                                "default": "./dist/node.mjs"
                            }
                        }
                    },
                    "./dist/fallback.mjs"
                ]
            }
        }),
    );
    fixture.write("node_modules/pkg/dist/node.mjs", "export default 'node';");
    fixture.write(
        "node_modules/pkg/dist/fallback.mjs",
        "export default 'fallback';",
    );

    assert_import(
        &fixture,
        "pkg",
        "/root/project/index.mjs",
        "/root/node_modules/pkg/dist/node.mjs",
    );
}

#[test]
fn resolution_require_prefers_cjs_entry_for_dual_packages() {
    let fixture = Fixture::new();
    fixture.write_json(
        "node_modules/pkg/package.json",
        json!({
            "exports": {
                ".": {
                    "import": "./dist/index.mjs",
                    "require": "./dist/index.cjs",
                    "default": "./dist/index.mjs"
                }
            }
        }),
    );
    fixture.write("node_modules/pkg/dist/index.mjs", "export default 'esm';");
    fixture.write("node_modules/pkg/dist/index.cjs", "module.exports = 'cjs';");

    assert_require(
        &fixture,
        "pkg",
        "/root/project/index.cjs",
        "/root/node_modules/pkg/dist/index.cjs",
    );
}

#[test]
fn runtime_exports_dot_named_exports_are_available_to_esm_imports() {
    let fixture = Fixture::new();
    fixture.write(
        "dep.cjs",
        "exports.answer = 42;\nexports.label = 'ok';\nmodule.exports.extra = true;\n",
    );
    fixture.write(
        "entry.mjs",
        r#"
import dep, { answer, label, extra } from "./dep.cjs";
console.log(JSON.stringify({ answer, label, extra, defaultAnswer: dep.answer }));
"#,
    );

    let output = run_guest_json(&fixture, "./entry.mjs");
    assert_eq!(
        output,
        json!({
            "answer": 42,
            "label": "ok",
            "extra": true,
            "defaultAnswer": 42
        })
    );
}

#[test]
fn runtime_object_define_property_exports_are_available_to_esm_imports() {
    let fixture = Fixture::new();
    fixture.write(
        "dep.cjs",
        r#"
Object.defineProperty(exports, "answer", { enumerable: true, value: 42 });
Object.defineProperty(exports, "label", { enumerable: true, value: "ok" });
"#,
    );
    fixture.write(
        "entry.mjs",
        r#"
import dep, { answer, label } from "./dep.cjs";
console.log(JSON.stringify({ answer, label, defaultLabel: dep.label }));
"#,
    );

    let output = run_guest_json(&fixture, "./entry.mjs");
    assert_eq!(
        output,
        json!({
            "answer": 42,
            "label": "ok",
            "defaultLabel": "ok"
        })
    );
}

#[test]
fn runtime_computed_property_cjs_modules_still_work_via_default_import() {
    let fixture = Fixture::new();
    fixture.write(
        "dep.cjs",
        r#"
const key = "dynamic";
module.exports = { [key]: 7, plain: 1 };
"#,
    );
    fixture.write(
        "entry.mjs",
        r#"
import dep from "./dep.cjs";
console.log(JSON.stringify(dep));
"#,
    );

    let output = run_guest_json(&fixture, "./entry.mjs");
    assert_eq!(output, json!({ "dynamic": 7, "plain": 1 }));
}

#[test]
fn runtime_exports_bracket_assignment_preserves_default_export_shape() {
    let fixture = Fixture::new();
    fixture.write(
        "dep.cjs",
        r#"
const name = "alpha";
exports[name] = 1;
module.exports.beta = 2;
"#,
    );
    fixture.write(
        "entry.mjs",
        r#"
import dep, { beta } from "./dep.cjs";
console.log(JSON.stringify({ alpha: dep.alpha, beta, defaultBeta: dep.beta }));
"#,
    );

    let output = run_guest_json(&fixture, "./entry.mjs");
    assert_eq!(
        output,
        json!({
            "alpha": 1,
            "beta": 2,
            "defaultBeta": 2
        })
    );
}

#[test]
fn runtime_object_assign_module_exports_still_exposes_the_default_export_shape() {
    let fixture = Fixture::new();
    fixture.write(
        "dep.cjs",
        r#"
Object.assign(module.exports, { answer: 42, label: "ok" });
"#,
    );
    fixture.write(
        "entry.mjs",
        r#"
import dep from "./dep.cjs";
console.log(JSON.stringify(dep));
"#,
    );

    let output = run_guest_json(&fixture, "./entry.mjs");
    assert_eq!(output, json!({ "answer": 42, "label": "ok" }));
}

#[test]
fn runtime_spread_based_module_exports_still_exposes_the_default_export_shape() {
    let fixture = Fixture::new();
    fixture.write(
        "dep.cjs",
        r#"
const shared = { alpha: 1 };
module.exports = { ...shared, beta: 2 };
"#,
    );
    fixture.write(
        "entry.mjs",
        r#"
import dep from "./dep.cjs";
console.log(JSON.stringify(dep));
"#,
    );

    let output = run_guest_json(&fixture, "./entry.mjs");
    assert_eq!(output, json!({ "alpha": 1, "beta": 2 }));
}

#[test]
fn runtime_require_of_esm_only_packages_either_loads_or_throws_clearly() {
    let fixture = Fixture::new();
    fixture.write_json(
        "node_modules/pkg/package.json",
        json!({
            "type": "module",
            "exports": "./index.mjs"
        }),
    );
    fixture.write(
        "node_modules/pkg/index.mjs",
        "export default { value: 42 };",
    );
    fixture.write(
        "entry.cjs",
        r#"
try {
  const value = require("pkg");
  console.log(JSON.stringify({
    mode: "loaded",
    value: value && value.default ? value.default.value : value.value
  }));
} catch (error) {
  console.log(JSON.stringify({
    mode: "error",
    code: error && error.code ? error.code : null,
    message: String(error && error.message ? error.message : error)
  }));
}
"#,
    );

    let output = run_guest_json(&fixture, "./entry.cjs");
    match output.get("mode").and_then(Value::as_str) {
        Some("loaded") => {
            assert_eq!(output.get("value"), Some(&json!(42)));
        }
        Some("error") => {
            let message = output
                .get("message")
                .and_then(Value::as_str)
                .expect("error message");
            assert!(!message.is_empty(), "expected a non-empty error message");
        }
        other => panic!("unexpected require(pkg) mode: {other:?}"),
    }
}

#[test]
fn runtime_require_of_dual_packages_uses_the_cjs_entrypoint() {
    let fixture = Fixture::new();
    fixture.write_json(
        "node_modules/pkg/package.json",
        json!({
            "exports": {
                ".": {
                    "import": "./dist/index.mjs",
                    "require": "./dist/index.cjs",
                    "default": "./dist/index.mjs"
                }
            }
        }),
    );
    fixture.write(
        "node_modules/pkg/dist/index.mjs",
        "export default { kind: 'esm' };",
    );
    fixture.write(
        "node_modules/pkg/dist/index.cjs",
        "module.exports = { kind: 'cjs' };",
    );
    fixture.write(
        "entry.cjs",
        r#"console.log(JSON.stringify(require("pkg")));"#,
    );

    let output = run_guest_json(&fixture, "./entry.cjs");
    assert_eq!(output, json!({ "kind": "cjs" }));
}

#[test]
fn runtime_two_module_circular_require_exposes_partial_exports() {
    let fixture = Fixture::new();
    fixture.write(
        "a.cjs",
        r#"
exports.name = "a";
const b = require("./b.cjs");
exports.fromB = b.name;
exports.seesBReady = Boolean(b.ready);
exports.ready = true;
"#,
    );
    fixture.write(
        "b.cjs",
        r#"
exports.name = "b";
const a = require("./a.cjs");
exports.fromA = a.name;
exports.seesAReady = Boolean(a.ready);
exports.ready = true;
"#,
    );
    fixture.write(
        "entry.cjs",
        r#"
const a = require("./a.cjs");
const b = require("./b.cjs");
console.log(JSON.stringify({ a, b }));
"#,
    );

    let output = run_guest_json(&fixture, "./entry.cjs");
    assert_eq!(
        output,
        json!({
            "a": {
                "name": "a",
                "fromB": "b",
                "seesBReady": true,
                "ready": true
            },
            "b": {
                "name": "b",
                "fromA": "a",
                "seesAReady": false,
                "ready": true
            }
        })
    );
}

#[test]
fn runtime_three_module_circular_chains_complete_without_hanging() {
    let fixture = Fixture::new();
    fixture.write(
        "a.cjs",
        r#"
exports.name = "a";
const b = require("./b.cjs");
exports.chain = (b.chain || []).concat("a");
"#,
    );
    fixture.write(
        "b.cjs",
        r#"
exports.name = "b";
const c = require("./c.cjs");
exports.chain = (c.chain || []).concat("b");
"#,
    );
    fixture.write(
        "c.cjs",
        r#"
exports.name = "c";
const a = require("./a.cjs");
exports.chain = [a.name || "missing", "c"];
"#,
    );
    fixture.write(
        "entry.cjs",
        r#"
const a = require("./a.cjs");
const b = require("./b.cjs");
const c = require("./c.cjs");
console.log(JSON.stringify({ a: a.chain, b: b.chain, c: c.chain }));
"#,
    );

    let output = run_guest_json(&fixture, "./entry.cjs");
    assert_eq!(
        output,
        json!({
            "a": ["a", "c", "b", "a"],
            "b": ["a", "c", "b"],
            "c": ["a", "c"]
        })
    );
}

#[test]
fn runtime_circular_requires_use_cache_instead_of_re_evaluating_modules() {
    let fixture = Fixture::new();
    fixture.write(
        "a.cjs",
        r#"
globalThis.__aLoads = (globalThis.__aLoads || 0) + 1;
exports.name = "a";
exports.fromB = require("./b.cjs").name;
"#,
    );
    fixture.write(
        "b.cjs",
        r#"
globalThis.__bLoads = (globalThis.__bLoads || 0) + 1;
exports.name = "b";
exports.fromA = require("./a.cjs").name;
"#,
    );
    fixture.write(
        "entry.cjs",
        r#"
const first = require("./a.cjs");
const second = require("./a.cjs");
console.log(JSON.stringify({
  sameInstance: first === second,
  aLoads: globalThis.__aLoads,
  bLoads: globalThis.__bLoads,
  first,
  second
}));
"#,
    );

    let output = run_guest_json(&fixture, "./entry.cjs");
    assert_eq!(output.get("sameInstance"), Some(&json!(true)));
    assert_eq!(output.get("aLoads"), Some(&json!(1)));
    assert_eq!(output.get("bLoads"), Some(&json!(1)));
    assert_eq!(
        output.get("first"),
        Some(&json!({ "name": "a", "fromB": "b" }))
    );
    assert_eq!(
        output.get("second"),
        Some(&json!({ "name": "a", "fromB": "b" }))
    );
}

#[test]
fn runtime_require_json_returns_the_parsed_object() {
    let fixture = Fixture::new();
    fixture.write("data.json", r#"{ "name": "agent-os", "ok": true }"#);
    fixture.write(
        "entry.cjs",
        r#"console.log(JSON.stringify(require("./data.json")));"#,
    );

    let output = run_guest_json(&fixture, "./entry.cjs");
    assert_eq!(output, json!({ "name": "agent-os", "ok": true }));
}

#[test]
fn runtime_require_invalid_json_surfaces_a_parse_error() {
    let fixture = Fixture::new();
    fixture.write(
        "data.json",
        "{\n  // comments are not valid JSON\n  \"ok\": true,\n}\n",
    );
    fixture.write(
        "entry.cjs",
        r#"
try {
  require("./data.json");
  throw new Error("require should have failed");
} catch (error) {
  console.log(JSON.stringify({
    message: String(error && error.message ? error.message : error)
  }));
}
"#,
    );

    let output = run_guest_json(&fixture, "./entry.cjs");
    let message = output
        .get("message")
        .and_then(Value::as_str)
        .expect("error message");
    assert!(
        message.contains("Unexpected") || message.contains("JSON"),
        "unexpected invalid JSON error: {message}"
    );
}

#[test]
fn runtime_esm_entrypoints_can_use_require_via_the_runtime_prelude() {
    let fixture = Fixture::new();
    fixture.write("dep.cjs", "module.exports = { answer: 42 };");
    fixture.write(
        "entry.mjs",
        r#"
const dep = require("./dep.cjs");
console.log(JSON.stringify(dep));
"#,
    );

    let output = run_guest_json(&fixture, "./entry.mjs");
    assert_eq!(output, json!({ "answer": 42 }));
}

#[test]
fn runtime_esm_default_import_of_cjs_uses_module_exports_value() {
    let fixture = Fixture::new();
    fixture.write(
        "dep.cjs",
        r#"
module.exports = function greet(name) {
  return `hello ${name}`;
};
"#,
    );
    fixture.write(
        "entry.mjs",
        r#"
import greet from "./dep.cjs";
console.log(JSON.stringify({ greeting: greet("agent") }));
"#,
    );

    let output = run_guest_json(&fixture, "./entry.mjs");
    assert_eq!(output, json!({ "greeting": "hello agent" }));
}

#[test]
fn runtime_esm_named_imports_of_cjs_use_the_extracted_names() {
    let fixture = Fixture::new();
    fixture.write(
        "dep.cjs",
        r#"
exports.answer = 42;
exports.label = "ok";
"#,
    );
    fixture.write(
        "entry.mjs",
        r#"
import { answer, label } from "./dep.cjs";
console.log(JSON.stringify({ answer, label }));
"#,
    );

    let output = run_guest_json(&fixture, "./entry.mjs");
    assert_eq!(output, json!({ "answer": 42, "label": "ok" }));
}

#[test]
fn runtime_builtin_assert_exposes_deep_strict_equal() {
    let fixture = Fixture::new();
    fixture.write(
        "entry.cjs",
        r#"
const assert = require("node:assert");
assert.deepStrictEqual({ nested: ["ok"] }, { nested: ["ok"] });
console.log(JSON.stringify({
  deepStrictEqual: typeof assert.deepStrictEqual
}));
"#,
    );

    let output = run_guest_json(&fixture, "./entry.cjs");
    assert_eq!(output, json!({ "deepStrictEqual": "function" }));
}

#[test]
fn runtime_builtin_assert_exposes_throws() {
    let fixture = Fixture::new();
    fixture.write(
        "entry.cjs",
        r#"
const assert = require("node:assert");
assert.throws(() => {
  throw new Error("boom");
}, /boom/);
console.log(JSON.stringify({ throws: typeof assert.throws }));
"#,
    );

    let output = run_guest_json(&fixture, "./entry.cjs");
    assert_eq!(output, json!({ "throws": "function" }));
}

#[test]
fn runtime_builtin_path_normalize_matches_expected_edge_cases() {
    let fixture = Fixture::new();
    fixture.write(
        "entry.cjs",
        r#"
const path = require("node:path");
console.log(JSON.stringify({
  dot: path.normalize("."),
  dotDot: path.normalize("foo/../bar"),
  trailing: path.normalize("/tmp/demo/"),
  repeated: path.normalize("/tmp//demo//../file")
}));
"#,
    );

    let output = run_guest_json(&fixture, "./entry.cjs");
    assert_eq!(
        output,
        json!({
            "dot": ".",
            "dotDot": "bar",
            "trailing": "/tmp/demo",
            "repeated": "/tmp/file"
        })
    );
}

#[test]
fn runtime_builtin_path_resolve_and_relative_match_expected_values() {
    let fixture = Fixture::new();
    fixture.write(
        "entry.cjs",
        r#"
const path = require("node:path");
console.log(JSON.stringify({
  resolve: path.resolve("alpha", "..", "beta", "file.txt"),
  relative: path.relative("/root/project/src", "/root/project/tests/spec"),
  same: path.relative("/root/project", "/root/project")
}));
"#,
    );

    let output = run_guest_json(&fixture, "./entry.cjs");
    assert_eq!(
        output,
        json!({
            "resolve": "/root/beta/file.txt",
            "relative": "../tests/spec",
            "same": ""
        })
    );
}

#[test]
fn runtime_object_assign_module_exports_named_exports_are_visible_to_esm_imports() {
    let fixture = Fixture::new();
    fixture.write(
        "dep.cjs",
        r#"
Object.assign(module.exports, { answer: 42, label: "ok" });
"#,
    );
    fixture.write(
        "entry.mjs",
        r#"
import { answer, label } from "./dep.cjs";
console.log(JSON.stringify({ answer, label }));
"#,
    );

    let output = run_guest_json(&fixture, "./entry.mjs");
    assert_eq!(output, json!({ "answer": 42, "label": "ok" }));
}

#[test]
fn runtime_spread_based_module_exports_named_exports_are_visible_to_esm_imports() {
    let fixture = Fixture::new();
    fixture.write(
        "dep.cjs",
        r#"
const shared = { alpha: 1 };
module.exports = { ...shared, beta: 2 };
"#,
    );
    fixture.write(
        "entry.mjs",
        r#"
import { alpha, beta } from "./dep.cjs";
console.log(JSON.stringify({ alpha, beta }));
"#,
    );

    let output = run_guest_json(&fixture, "./entry.mjs");
    assert_eq!(output, json!({ "alpha": 1, "beta": 2 }));
}

#[test]
fn runtime_object_define_properties_reexports_are_visible_to_esm_imports() {
    let fixture = Fixture::new();
    fixture.write(
        "dep.cjs",
        r#"
Object.defineProperties(module.exports, {
  answer: { enumerable: true, value: 42 },
  label: { enumerable: true, value: "ok" }
});
"#,
    );
    fixture.write(
        "entry.mjs",
        r#"
import { answer, label } from "./dep.cjs";
console.log(JSON.stringify({ answer, label }));
"#,
    );

    let output = run_guest_json(&fixture, "./entry.mjs");
    assert_eq!(output, json!({ "answer": 42, "label": "ok" }));
}

#[test]
fn runtime_esm_json_imports_return_the_parsed_object() {
    let fixture = Fixture::new();
    fixture.write("data.json", r#"{ "name": "agent-os", "ok": true }"#);
    fixture.write(
        "entry.mjs",
        r#"
import data from "./data.json";
console.log(JSON.stringify(data));
"#,
    );

    let output = run_guest_json(&fixture, "./entry.mjs");
    assert_eq!(output, json!({ "name": "agent-os", "ok": true }));
}
