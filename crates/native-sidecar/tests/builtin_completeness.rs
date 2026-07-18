mod support;

use agentos_native_sidecar::wire::{EventPayload, GuestRuntimeKind, StreamChannel};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};
use support::{
    authenticate_wire, create_vm_wire_with_metadata, execute_wire, new_sidecar, open_session_wire,
    temp_dir, wire_session, wire_vm, write_fixture,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BuiltinStatus {
    Polyfilled,
    KernelBacked,
    Denied,
    StubOk,
}

#[derive(Clone, Copy, Debug)]
struct BuiltinExpectation {
    name: &'static str,
    status: BuiltinStatus,
}

const BUILTIN_EXPECTATIONS: &[BuiltinExpectation] = &[
    BuiltinExpectation {
        name: "fs",
        status: BuiltinStatus::KernelBacked,
    },
    BuiltinExpectation {
        name: "fs/promises",
        status: BuiltinStatus::KernelBacked,
    },
    BuiltinExpectation {
        name: "path",
        status: BuiltinStatus::Polyfilled,
    },
    BuiltinExpectation {
        name: "path/posix",
        status: BuiltinStatus::Polyfilled,
    },
    BuiltinExpectation {
        name: "path/win32",
        status: BuiltinStatus::Polyfilled,
    },
    BuiltinExpectation {
        name: "os",
        status: BuiltinStatus::KernelBacked,
    },
    BuiltinExpectation {
        name: "crypto",
        status: BuiltinStatus::KernelBacked,
    },
    BuiltinExpectation {
        name: "http",
        status: BuiltinStatus::KernelBacked,
    },
    BuiltinExpectation {
        name: "https",
        status: BuiltinStatus::KernelBacked,
    },
    BuiltinExpectation {
        name: "http2",
        status: BuiltinStatus::KernelBacked,
    },
    BuiltinExpectation {
        name: "net",
        status: BuiltinStatus::KernelBacked,
    },
    BuiltinExpectation {
        name: "dgram",
        status: BuiltinStatus::KernelBacked,
    },
    BuiltinExpectation {
        name: "dns",
        status: BuiltinStatus::KernelBacked,
    },
    BuiltinExpectation {
        name: "dns/promises",
        status: BuiltinStatus::KernelBacked,
    },
    BuiltinExpectation {
        name: "tls",
        status: BuiltinStatus::KernelBacked,
    },
    BuiltinExpectation {
        name: "child_process",
        status: BuiltinStatus::KernelBacked,
    },
    BuiltinExpectation {
        name: "stream",
        status: BuiltinStatus::Polyfilled,
    },
    BuiltinExpectation {
        name: "events",
        status: BuiltinStatus::Polyfilled,
    },
    BuiltinExpectation {
        name: "buffer",
        status: BuiltinStatus::Polyfilled,
    },
    BuiltinExpectation {
        name: "util",
        status: BuiltinStatus::Polyfilled,
    },
    BuiltinExpectation {
        name: "util/types",
        status: BuiltinStatus::Polyfilled,
    },
    BuiltinExpectation {
        name: "url",
        status: BuiltinStatus::Polyfilled,
    },
    BuiltinExpectation {
        name: "querystring",
        status: BuiltinStatus::Polyfilled,
    },
    BuiltinExpectation {
        name: "sqlite",
        status: BuiltinStatus::KernelBacked,
    },
    BuiltinExpectation {
        name: "string_decoder",
        status: BuiltinStatus::Polyfilled,
    },
    BuiltinExpectation {
        name: "punycode",
        status: BuiltinStatus::Polyfilled,
    },
    BuiltinExpectation {
        name: "zlib",
        status: BuiltinStatus::Polyfilled,
    },
    BuiltinExpectation {
        name: "assert",
        status: BuiltinStatus::Polyfilled,
    },
    BuiltinExpectation {
        name: "assert/strict",
        status: BuiltinStatus::Polyfilled,
    },
    BuiltinExpectation {
        name: "constants",
        status: BuiltinStatus::Polyfilled,
    },
    BuiltinExpectation {
        name: "console",
        status: BuiltinStatus::Polyfilled,
    },
    BuiltinExpectation {
        name: "readline",
        status: BuiltinStatus::KernelBacked,
    },
    BuiltinExpectation {
        name: "tty",
        status: BuiltinStatus::KernelBacked,
    },
    BuiltinExpectation {
        name: "perf_hooks",
        status: BuiltinStatus::KernelBacked,
    },
    BuiltinExpectation {
        name: "timers",
        status: BuiltinStatus::Polyfilled,
    },
    BuiltinExpectation {
        name: "timers/promises",
        status: BuiltinStatus::Polyfilled,
    },
    BuiltinExpectation {
        name: "stream/promises",
        status: BuiltinStatus::Polyfilled,
    },
    BuiltinExpectation {
        name: "stream/consumers",
        status: BuiltinStatus::Polyfilled,
    },
    BuiltinExpectation {
        name: "stream/web",
        status: BuiltinStatus::Polyfilled,
    },
    BuiltinExpectation {
        name: "module",
        status: BuiltinStatus::KernelBacked,
    },
    BuiltinExpectation {
        name: "process",
        status: BuiltinStatus::KernelBacked,
    },
    BuiltinExpectation {
        name: "vm",
        status: BuiltinStatus::Polyfilled,
    },
    BuiltinExpectation {
        name: "worker_threads",
        status: BuiltinStatus::StubOk,
    },
    BuiltinExpectation {
        name: "inspector",
        status: BuiltinStatus::StubOk,
    },
    BuiltinExpectation {
        name: "v8",
        status: BuiltinStatus::Polyfilled,
    },
    BuiltinExpectation {
        name: "cluster",
        status: BuiltinStatus::Denied,
    },
    BuiltinExpectation {
        name: "async_hooks",
        status: BuiltinStatus::StubOk,
    },
    BuiltinExpectation {
        name: "diagnostics_channel",
        status: BuiltinStatus::StubOk,
    },
    BuiltinExpectation {
        name: "wasi",
        status: BuiltinStatus::Denied,
    },
    BuiltinExpectation {
        name: "trace_events",
        status: BuiltinStatus::Denied,
    },
    BuiltinExpectation {
        name: "repl",
        status: BuiltinStatus::Denied,
    },
    BuiltinExpectation {
        name: "domain",
        status: BuiltinStatus::Denied,
    },
    BuiltinExpectation {
        name: "sys",
        status: BuiltinStatus::Polyfilled,
    },
];

const EXPECTED_RUNTIME_BUILTINS: &[&str] = &[
    "assert",
    "assert/strict",
    "async_hooks",
    "buffer",
    "child_process",
    "cluster",
    "console",
    "constants",
    "crypto",
    "dgram",
    "diagnostics_channel",
    "dns",
    "dns/promises",
    "domain",
    "events",
    "fs",
    "fs/promises",
    "http",
    "http2",
    "https",
    "inspector",
    "module",
    "net",
    "os",
    "path",
    "path/posix",
    "path/win32",
    "perf_hooks",
    "process",
    "punycode",
    "querystring",
    "readline",
    "repl",
    "sqlite",
    "stream",
    "stream/consumers",
    "stream/promises",
    "stream/web",
    "string_decoder",
    "sys",
    "timers",
    "timers/promises",
    "tls",
    "trace_events",
    "tty",
    "url",
    "util",
    "util/types",
    "v8",
    "vm",
    "wasi",
    "worker_threads",
    "zlib",
];

const COMPLETENESS_SCRIPT: &str = r#"
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const target = process.argv[2];
if (target === "--inventory") {
  const builtinModules = require("module").builtinModules.slice().sort();
  console.log(JSON.stringify({ builtinModules }));
  process.exit(0);
}

try {
  const mod = require(target);
  const modType = typeof mod;
  const ownKeys =
    mod != null && (modType === "object" || modType === "function")
      ? Array.from(
          new Set([
            ...Object.keys(mod),
            ...Object.getOwnPropertyNames(mod),
          ]),
        ).sort()
      : [];

  console.log(
    JSON.stringify({
      target,
      ok: true,
      type: modType,
      isNull: mod === null,
      ownKeyCount: ownKeys.length,
      emptyObject: modType === "object" && mod !== null && ownKeys.length === 0,
    }),
  );
} catch (error) {
  console.log(
    JSON.stringify({
      target,
      ok: false,
      code: error?.code ?? null,
      name: error?.name ?? null,
      message: String(error?.message ?? error),
    }),
  );
}
"#;

const PROBE_OUTPUT_BYTE_LIMIT: usize = 1024 * 1024;

fn allowed_builtins_json() -> String {
    let allowed = BUILTIN_EXPECTATIONS
        .iter()
        .filter(|builtin| builtin.status != BuiltinStatus::Denied)
        .map(|builtin| builtin.name)
        .collect::<Vec<_>>();
    serde_json::to_string(&allowed).expect("serialize allowed builtin list")
}

fn run_guest_probe(entrypoint: &Path, arg: &str) -> Value {
    let mut sidecar = new_sidecar(&format!(
        "builtin-completeness-{}",
        arg.chars()
            .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
            .collect::<String>()
    ));
    let connection_id = authenticate_wire(&mut sidecar, &format!("conn-{arg}"));
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let cwd = entrypoint.parent().expect("entrypoint parent");
    let (vm_id, _) = create_vm_wire_with_metadata(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        cwd,
        HashMap::from([(
            String::from("env.AGENTOS_ALLOWED_NODE_BUILTINS"),
            allowed_builtins_json(),
        )]),
    );

    let process_id = format!(
        "probe-{}",
        arg.chars()
            .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
            .collect::<String>()
    );

    execute_wire(
        &mut sidecar,
        100,
        &connection_id,
        &session_id,
        &vm_id,
        &process_id,
        GuestRuntimeKind::JavaScript,
        entrypoint,
        vec![arg.to_owned()],
    );

    let ownership = wire_session(&connection_id, &session_id);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut exit = None;

    loop {
        let event = sidecar
            .poll_event_wire_blocking(&ownership, Duration::from_millis(100))
            .expect("poll sidecar event");
        if let Some(event) = event {
            assert_eq!(
                event.ownership,
                wire_vm(&connection_id, &session_id, &vm_id)
            );

            match event.payload {
                EventPayload::ProcessOutputEvent(output) if output.process_id == process_id => {
                    match output.channel {
                        StreamChannel::Stdout => {
                            append_probe_output(&mut stdout, &output.chunk, arg, "stdout")
                        }
                        StreamChannel::Stderr => {
                            append_probe_output(&mut stderr, &output.chunk, arg, "stderr")
                        }
                    }
                }
                EventPayload::ProcessExitedEvent(exited) if exited.process_id == process_id => {
                    exit = Some((exited.exit_code, Instant::now()));
                }
                _ => {}
            }
        }

        if let Some((exit_code, seen_at)) = exit {
            if Instant::now().duration_since(seen_at) >= Duration::from_millis(200) {
                assert_eq!(
                    exit_code, 0,
                    "guest probe failed for {arg}\nstdout:\n{stdout}\nstderr:\n{stderr}"
                );
                assert!(
                    stderr.trim().is_empty(),
                    "guest probe stderr for {arg}:\n{stderr}"
                );
                return serde_json::from_str(stdout.trim()).expect("parse builtin probe JSON");
            }
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for process events for builtin {arg}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}

fn append_probe_output(buffer: &mut String, chunk: &[u8], arg: &str, channel: &str) {
    let text = String::from_utf8_lossy(chunk);
    assert!(
        buffer.len().saturating_add(text.len()) <= PROBE_OUTPUT_BYTE_LIMIT,
        "builtin probe {arg} exceeded {PROBE_OUTPUT_BYTE_LIMIT} bytes on {channel}"
    );
    buffer.push_str(&text);
}

#[test]
fn every_guest_builtin_is_classified_and_never_silently_missing() {
    let cwd = temp_dir("builtin-completeness");
    let entrypoint = cwd.join("entry.mjs");
    write_fixture(&entrypoint, COMPLETENESS_SCRIPT);

    let inventory = run_guest_probe(&entrypoint, "--inventory");
    let actual_inventory = inventory["builtinModules"]
        .as_array()
        .expect("builtinModules array")
        .iter()
        .map(|value| value.as_str().expect("builtin module string"))
        .collect::<Vec<_>>();
    assert_eq!(
        actual_inventory, EXPECTED_RUNTIME_BUILTINS,
        "guest builtin inventory changed; classify the added/removed modules in builtin_completeness.rs"
    );

    let mut failures = Vec::new();

    for builtin in BUILTIN_EXPECTATIONS {
        let result = run_guest_probe(&entrypoint, builtin.name);

        match builtin.status {
            BuiltinStatus::Denied => {
                let code = result["code"].as_str();
                if result["ok"].as_bool() != Some(false) || code != Some("ERR_ACCESS_DENIED") {
                    failures.push(format!(
                        "{name}: expected ERR_ACCESS_DENIED, got {result}",
                        name = builtin.name
                    ));
                }
            }
            BuiltinStatus::Polyfilled | BuiltinStatus::KernelBacked | BuiltinStatus::StubOk => {
                let ok = result["ok"].as_bool() == Some(true);
                let type_ok = matches!(result["type"].as_str(), Some("object" | "function"));
                let is_null = result["isNull"].as_bool() == Some(true);
                let empty_object = result["emptyObject"].as_bool() == Some(true);
                if !ok {
                    failures.push(format!(
                        "{name}: expected loaded module, got {result}",
                        name = builtin.name
                    ));
                } else if !type_ok {
                    failures.push(format!(
                        "{name}: expected typeof object/function, got {result}",
                        name = builtin.name
                    ));
                } else if is_null {
                    failures.push(format!(
                        "{name}: module resolved to null, got {result}",
                        name = builtin.name
                    ));
                } else if empty_object {
                    failures.push(format!(
                        "{name}: module resolved to an empty object stub, got {result}",
                        name = builtin.name
                    ));
                }
            }
        }
    }

    if !failures.is_empty() {
        let mut message = String::from("builtin completeness failures:\n");
        for failure in failures {
            let _ = writeln!(&mut message, "- {failure}");
        }
        panic!("{message}");
    }
}
