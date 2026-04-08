mod support;

use agent_os_sidecar::protocol::GuestRuntimeKind;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use support::{
    assert_node_available, authenticate, collect_process_output, create_vm_with_metadata, execute,
    new_sidecar, open_session, temp_dir, write_fixture,
};

const ALLOWED_NODE_BUILTINS: &[&str] = &[
    "assert",
    "buffer",
    "child_process",
    "console",
    "crypto",
    "events",
    "fs",
    "path",
    "querystring",
    "stream",
    "string_decoder",
    "timers",
    "url",
    "util",
    "zlib",
];

fn run_host_probe(cwd: &Path, entrypoint: &Path) -> Value {
    let output = Command::new("node")
        .arg(entrypoint)
        .current_dir(cwd)
        .output()
        .expect("run host node probe");

    assert!(
        output.status.success(),
        "host probe failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    serde_json::from_slice(&output.stdout).expect("parse host probe JSON")
}

fn run_guest_probe(case_name: &str, cwd: &Path, entrypoint: &Path) -> Value {
    let mut sidecar = new_sidecar(case_name);
    let connection_id = authenticate(&mut sidecar, &format!("conn-{case_name}"));
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let allowed_builtins =
        serde_json::to_string(ALLOWED_NODE_BUILTINS).expect("serialize builtin allowlist");
    let (vm_id, _) = create_vm_with_metadata(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        cwd,
        BTreeMap::from([(
            String::from("env.AGENT_OS_ALLOWED_NODE_BUILTINS"),
            allowed_builtins,
        )]),
    );

    execute(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        &format!("proc-{case_name}"),
        GuestRuntimeKind::JavaScript,
        entrypoint,
        Vec::new(),
    );

    let (stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        &format!("proc-{case_name}"),
    );

    assert_eq!(
        exit_code, 0,
        "guest probe failed for {case_name}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stderr.trim().is_empty(),
        "guest probe stderr for {case_name}:\n{stderr}"
    );

    serde_json::from_str(stdout.trim()).expect("parse guest probe JSON")
}

fn assert_conformance(case_name: &str, script: &str) {
    assert_node_available();

    let cwd = temp_dir(&format!("builtin-conformance-{case_name}"));
    let entrypoint = cwd.join("entry.mjs");
    write_fixture(&entrypoint, script);

    let host = run_host_probe(&cwd, &entrypoint);
    let guest = run_guest_probe(case_name, &cwd, &entrypoint);

    assert_eq!(
        guest,
        host,
        "guest V8 result diverged from host Node for {case_name}\nhost: {}\nguest: {}",
        serde_json::to_string_pretty(&host).expect("pretty host JSON"),
        serde_json::to_string_pretty(&guest).expect("pretty guest JSON")
    );
}

#[test]
fn fs_conformance_matches_host_node() {
    assert_conformance(
        "fs",
        r#"
import fs from "node:fs";

fs.mkdirSync("workspace");
fs.mkdirSync("workspace/nested");
fs.writeFileSync("workspace/nested/alpha.txt", Buffer.from("alpha-sync", "utf8"));
await new Promise((resolve, reject) => {
  fs.writeFile("workspace/beta.txt", Buffer.from("beta-async", "utf8"), (error) => {
    if (error) {
      reject(error);
      return;
    }
    resolve();
  });
});

let missingStatCode = null;
try {
  fs.statSync("workspace/missing.txt");
} catch (error) {
  missingStatCode = error?.code ?? null;
}

let missingReadCode = null;
try {
  await new Promise((resolve, reject) => {
    fs.readFile("workspace/missing.txt", "utf8", (error, value) => {
      if (error) {
        reject(error);
        return;
      }
      resolve(value);
    });
  });
} catch (error) {
  missingReadCode = error?.code ?? null;
}

const asyncRead = await new Promise((resolve, reject) => {
  fs.readFile("workspace/beta.txt", "utf8", (error, value) => {
    if (error) {
      reject(error);
      return;
    }
    resolve(value);
  });
});

console.log(JSON.stringify({
  syncRead: fs.readFileSync("workspace/nested/alpha.txt", "utf8"),
  asyncRead,
  entries: fs.readdirSync("workspace").sort(),
  statSize: fs.statSync("workspace/nested/alpha.txt").size,
  existsAlpha: fs.existsSync("workspace/nested/alpha.txt"),
  existsBeta: fs.existsSync("workspace/beta.txt"),
  missingStatCode,
  missingReadCode,
}));
"#,
    );
}

#[test]
fn child_process_conformance_matches_host_node() {
    assert_conformance(
        "child-process",
        r#"
import childProcess from "node:child_process";
const syncStdout = childProcess.spawnSync(
  "node",
  ["-e", "process.stdout.write(process.argv[1] ?? '')", "alpha-sync"],
);
const syncError = childProcess.spawnSync(
  "node",
  ["-e", "process.stderr.write('sync-error'); throw new Error('sync-fail');"],
);

const asyncEchoResult = await new Promise((resolve, reject) => {
  const child = childProcess.spawn(
    "node",
    [
      "-e",
      "let data=''; let settled = false; const fallback = setTimeout(() => { if (!settled) process.exit(19); }, 50); process.stdin.on('data', (chunk) => { data += chunk; }); process.stdin.on('end', () => { settled = true; clearTimeout(fallback); process.exit(data === 'beta-async' ? 0 : 17); });",
    ],
  );
  const timer = setTimeout(() => {
    reject(new Error("spawn(node async echo) did not close within 2s"));
  }, 2000);
  const stdout = [];
  const stderr = [];
  child.stdout.on("data", (chunk) => {
    stdout.push(Buffer.from(chunk));
  });
  child.stderr.on("data", (chunk) => {
    stderr.push(Buffer.from(chunk));
  });
  child.stdin.write(Buffer.from("beta-async"));
  child.stdin.end();
  child.on("error", reject);
  child.on("close", (code, signal) => {
    clearTimeout(timer);
    resolve({
      code,
      signal,
      stdoutBase64: Buffer.concat(stdout).toString("base64"),
      stderrBase64: Buffer.concat(stderr).toString("base64"),
    });
  });
});

const asyncErrorResult = await new Promise((resolve, reject) => {
  const child = childProcess.spawn(
    "node",
    [
      "-e",
      "setTimeout(() => { process.stderr.write('async-error'); throw new Error('async-fail'); }, 10);",
    ],
  );
  const timer = setTimeout(() => {
    reject(new Error("spawn(node async failure) did not close within 2s"));
  }, 2000);
  const stdout = [];
  const stderr = [];
  child.stdout.on("data", (chunk) => {
    stdout.push(Buffer.from(chunk));
  });
  child.stderr.on("data", (chunk) => {
    stderr.push(Buffer.from(chunk));
  });
  child.on("error", reject);
  child.on("close", (code, signal) => {
    clearTimeout(timer);
    resolve({
      code,
      signal,
      stdoutBase64: Buffer.concat(stdout).toString("base64"),
      stderrBase64: Buffer.concat(stderr).toString("base64"),
    });
  });
});

console.log(JSON.stringify({
  syncStdoutStatus: syncStdout.status,
  syncStdoutTrimmed: Buffer.from(syncStdout.stdout ?? []).toString("utf8").trim(),
  syncStdoutStderrBase64: Buffer.from(syncStdout.stderr ?? []).toString("base64"),
  syncErrorStatus: syncError.status,
  syncErrorStdoutBase64: Buffer.from(syncError.stdout ?? []).toString("base64"),
  syncErrorHasMarker: Buffer.from(syncError.stderr ?? []).toString("utf8").includes("sync-error"),
  syncErrorHasNonZeroStatus: (syncError.status ?? 0) !== 0,
  asyncEchoCode: asyncEchoResult.code,
  asyncEchoSignal: asyncEchoResult.signal,
  asyncEchoStdoutBase64: asyncEchoResult.stdoutBase64,
  asyncEchoStderrBase64: asyncEchoResult.stderrBase64,
  asyncErrorCode: asyncErrorResult.code,
  asyncErrorSignal: asyncErrorResult.signal,
  asyncErrorStdoutBase64: asyncErrorResult.stdoutBase64,
  asyncErrorHasNonZeroStatus: (asyncErrorResult.code ?? 0) !== 0,
}));
"#,
    );
}

#[test]
fn path_conformance_matches_host_node() {
    assert_conformance(
        "path",
        r#"
import * as pathNs from "node:path";

const path = pathNs.default ?? pathNs;

console.log(JSON.stringify({
  join: path.join("/virtual", "project", "file.txt"),
  resolve: path.resolve("/virtual/root", "alpha", "..", "beta", "file.txt"),
  dirname: path.dirname("/virtual/root/beta/file.txt"),
  basename: path.basename("/virtual/root/beta/file.txt"),
  extname: path.extname("/virtual/root/beta/file.txt"),
  isAbsoluteFile: path.isAbsolute("/virtual/root/beta/file.txt"),
  isAbsoluteRelative: path.isAbsolute("virtual/root/beta/file.txt"),
  relative: path.relative("/virtual/root/alpha", "/virtual/root/beta/file.txt"),
  normalize: path.normalize("/virtual//root/alpha/../beta//file.txt"),
}));
"#,
    );
}

#[test]
fn crypto_conformance_matches_host_node() {
    assert_conformance(
        "crypto",
        r#"
import crypto from "node:crypto";

const random = crypto.randomBytes(16);
const uuid = crypto.randomUUID();

console.log(JSON.stringify({
  sha256: crypto.createHash("sha256").update("agent-os").digest("hex"),
  hmacSha256: crypto.createHmac("sha256", "shared-secret").update("agent-os").digest("hex"),
  randomBytesLength: random.length,
  randomBytesHexLength: random.toString("hex").length,
  randomBytesAllZero: Array.from(random).every((value) => value === 0),
  randomUuidValid: /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(uuid),
}));
"#,
    );
}

#[test]
fn events_conformance_matches_host_node() {
    assert_conformance(
        "events",
        r#"
import { EventEmitter } from "node:events";

const emitter = new EventEmitter();
const seen = [];

function persistent(value) {
  seen.push(`on:${value}`);
}

emitter.on("tick", persistent);
emitter.once("tick", (value) => {
  seen.push(`once:${value}`);
});
emitter.emit("tick", "alpha");
emitter.removeListener("tick", persistent);
emitter.emit("tick", "beta");

console.log(JSON.stringify({
  seen,
  listenerCount: emitter.listenerCount("tick"),
}));
"#,
    );
}

#[test]
fn stream_conformance_matches_host_node() {
    assert_conformance(
        "stream",
        r#"
import * as streamNs from "node:stream";

const stream = streamNs.default ?? streamNs;

class Source extends stream.Readable {
  constructor() {
    super();
    this.sent = false;
  }

  _read() {
    if (this.sent) {
      return;
    }
    this.sent = true;
    this.push("alpha");
    this.push("beta");
    this.push(null);
  }
}

class Sink extends stream.Writable {
  constructor(chunks) {
    super();
    this.chunks = chunks;
  }

  _write(chunk, _encoding, callback) {
    this.chunks.push(Buffer.from(chunk).toString("utf8"));
    callback();
  }
}

class Upper extends stream.Transform {
  _transform(chunk, _encoding, callback) {
    callback(null, Buffer.from(chunk).toString("utf8").toUpperCase());
  }
}

const chunks = [];
const source = new Source();
const sink = new Sink(chunks);
const upper = new Upper();

let pipelineError = null;
const pipelineResult = stream.pipeline(source, upper, sink, (error) => {
  pipelineError = error ? String(error.message || error) : null;
});
source._read();
await new Promise((resolve) => setTimeout(resolve, 0));

console.log(JSON.stringify({
  output: chunks.join("|"),
  pipelineReturnedSink: pipelineResult === sink,
  pipelineError,
  readableIsFunction: typeof stream.Readable === "function",
  writableIsFunction: typeof stream.Writable === "function",
  transformIsFunction: typeof stream.Transform === "function",
}));
"#,
    );
}

#[test]
fn buffer_conformance_matches_host_node() {
    assert_conformance(
        "buffer",
        r#"
const text = Buffer.from("hello", "utf8");
const filled = Buffer.alloc(4, 0x61);
const combined = Buffer.concat([text, Buffer.from("-world", "utf8")]);

console.log(JSON.stringify({
  fromHex: text.toString("hex"),
  allocUtf8: filled.toString("utf8"),
  concatUtf8: combined.toString("utf8"),
  sliceUtf8: combined.slice(3, 8).toString("utf8"),
}));
"#,
    );
}

#[test]
fn url_conformance_matches_host_node() {
    assert_conformance(
        "url",
        r#"
import * as urlNs from "node:url";

const urlModule = urlNs.default ?? urlNs;
const url = new urlModule.URL("https://example.com/a/b?x=1&y=two#frag");
url.searchParams.append("z", "3");

const parsed = urlModule.parse("https://example.com/a/b?x=1&y=two#frag", true);

console.log(JSON.stringify({
  href: url.href,
  searchParams: Array.from(url.searchParams.entries()),
  formatted: urlModule.format(parsed),
  parsedPathname: parsed.pathname,
  parsedQuery: parsed.query,
}));
"#,
    );
}
