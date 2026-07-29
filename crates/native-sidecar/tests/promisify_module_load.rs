//! Regression guard for issue #11: `util.promisify()` throws at module load.
//!
//! Original failure mode: a hand-rolled legacy `util` polyfill exposed many
//! builtin functions as `undefined`. Adapter dependencies (extract-zip, get-stream) called
//! `promisify(undefined)` at module-load time, and the polyfill's `promisify` threw a
//! `TypeError` synchronously, crashing the whole module load before any application code ran.
//!
//! The downstream fix removed the eager throw (returning a function that rejects lazily). The
//! architecture has since changed: `node:util` now comes from the real upstream `util@0.12.5`
//! bundle (which still throws eagerly on non-functions, matching real Node), and the builtin
//! surface (`node:fs`, etc.) is now backed by real complete modules rather than `undefined`
//! stubs.
//!
//! This test encodes the CORRECT expected behavior in the redesigned world:
//!   1. `util.promisify(undefined)` throwing a `TypeError` must be *containable* by user code
//!      (a try/catch) and must NOT crash the guest at module load (process still exits 0).
//!   2. The real-world root cause must stay fixed: `promisify` applied to a genuine builtin
//!      function that the adapters use (`fs.readFile`) must return a function, proving the
//!      builtin surface is complete enough that adapter deps never receive `undefined`.

mod support;

use std::collections::HashMap;
use std::time::Duration;

use serde_json::Value;
use support::{
    assert_node_available, authenticate_wire, collect_process_output_wire_with_timeout,
    create_vm_wire_with_metadata, dispose_vm_and_close_session_wire, execute_wire, new_sidecar,
    open_session_wire, temp_dir, write_fixture,
};

const ALLOWED_NODE_BUILTINS: &[&str] = &["fs", "util"];

const GUEST_SCRIPT: &str = r#"
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);

// `node:util` and `node:fs` are resolved at module load. If `util.promisify`
// throws synchronously on the eager TypeError path *outside* of a guard, this
// whole module would fail to load and the process would crash (non-zero exit),
// reproducing issue #11.
const util = require("node:util");
const fs = require("node:fs");

// (1) The eager TypeError must be containable by user code rather than tearing
//     down the module/process. This mirrors `promisify(undefined)` which is what
//     the old incomplete polyfill handed to extract-zip / get-stream at load time.
let undefinedThrewTypeError = false;
try {
  util.promisify(undefined);
} catch (error) {
  undefinedThrewTypeError = error != null && error.name === "TypeError";
}

// (2) Root-cause regression guard: a genuine builtin function that adapters
//     actually promisify must be a real function (not an `undefined` stub), so
//     `promisify` yields a usable function.
const readFileIsFunction = typeof fs.readFile === "function";
const promisifiedReadFileIsFunction =
  typeof util.promisify(fs.readFile) === "function";

console.log(
  JSON.stringify({
    undefinedThrewTypeError,
    readFileIsFunction,
    promisifiedReadFileIsFunction,
  }),
);
"#;

fn run_guest(case_name: &str, script: &str, allowed_builtins: &[&str]) -> (Value, String, i32) {
    assert_node_available();

    let cwd = temp_dir(&format!("promisify-{case_name}"));
    let entrypoint = cwd.join("entry.mjs");
    write_fixture(&entrypoint, script);

    let mut sidecar = new_sidecar(case_name);
    let connection_id = authenticate_wire(&mut sidecar, &format!("conn-{case_name}"));
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);

    let mut metadata = HashMap::new();
    metadata.insert(
        String::from("env.AGENTOS_ALLOWED_NODE_BUILTINS"),
        serde_json::to_string(allowed_builtins).expect("serialize builtin allowlist"),
    );

    let (vm_id, _create) = create_vm_wire_with_metadata(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        agentos_native_sidecar::wire::GuestRuntimeKind::JavaScript,
        &cwd,
        metadata,
    );

    let process_id = format!("proc-{case_name}");
    execute_wire(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        &process_id,
        agentos_native_sidecar::wire::GuestRuntimeKind::JavaScript,
        &entrypoint,
        Vec::new(),
    );

    let (stdout, stderr, exit_code) = collect_process_output_wire_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        &process_id,
        Duration::from_secs(30),
    );

    dispose_vm_and_close_session_wire(&mut sidecar, &connection_id, &session_id, &vm_id);

    let parsed = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("parse guest JSON\nstdout:\n{stdout}\nstderr:\n{stderr}"));
    (parsed, stderr, exit_code)
}

#[test]
fn promisify_undefined_does_not_crash_module_load() {
    let (parsed, stderr, exit_code) =
        run_guest("promisify-module-load", GUEST_SCRIPT, ALLOWED_NODE_BUILTINS);

    // The module must have loaded and the process must have exited cleanly: the
    // eager TypeError from `promisify(undefined)` must be containable, not fatal.
    assert_eq!(
        exit_code, 0,
        "guest module load crashed (issue #11)\nstderr:\n{stderr}\nparsed:\n{parsed}"
    );

    assert_eq!(
        parsed["undefinedThrewTypeError"],
        Value::Bool(true),
        "promisify(undefined) should raise a containable TypeError, parsed={parsed}"
    );

    // Root-cause guard: the builtin surface must be complete, so adapter deps
    // never receive `undefined` to promisify.
    assert_eq!(
        parsed["readFileIsFunction"],
        Value::Bool(true),
        "fs.readFile must be a real function, parsed={parsed}"
    );
    assert_eq!(
        parsed["promisifiedReadFileIsFunction"],
        Value::Bool(true),
        "promisify(fs.readFile) must return a function, parsed={parsed}"
    );
}
