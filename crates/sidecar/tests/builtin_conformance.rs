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
    "constants",
    "crypto",
    "events",
    "fs",
    "module",
    "path",
    "perf_hooks",
    "punycode",
    "querystring",
    "stream",
    "string_decoder",
    "timers",
    "tty",
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

fn run_guest_script(case_name: &str, script: &str) -> Value {
    assert_node_available();

    let cwd = temp_dir(&format!("builtin-guest-{case_name}"));
    let entrypoint = cwd.join("entry.mjs");
    write_fixture(&entrypoint, script);

    run_guest_probe(case_name, &cwd, &entrypoint)
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
fn console_conformance_matches_host_node() {
    assert_conformance(
        "console",
        r#"
import * as consoleModule from "node:console";
const consoleInstance = new consoleModule.Console(process.stdout, process.stderr);
const task = consoleModule.createTask("demo-task");

console.log(JSON.stringify({
  types: {
    Console: typeof consoleModule.Console,
    context: typeof consoleModule.context,
    createTask: typeof consoleModule.createTask,
    log: typeof consoleModule.log,
    table: typeof consoleModule.table,
  },
  taskRunType: typeof task.run,
  consoleMethods: {
    assert: typeof consoleInstance.assert,
    clear: typeof consoleInstance.clear,
    count: typeof consoleInstance.count,
    countReset: typeof consoleInstance.countReset,
    debug: typeof consoleInstance.debug,
    dir: typeof consoleInstance.dir,
    dirxml: typeof consoleInstance.dirxml,
    error: typeof consoleInstance.error,
    group: typeof consoleInstance.group,
    groupCollapsed: typeof consoleInstance.groupCollapsed,
    groupEnd: typeof consoleInstance.groupEnd,
    info: typeof consoleInstance.info,
    log: typeof consoleInstance.log,
    profile: typeof consoleInstance.profile,
    profileEnd: typeof consoleInstance.profileEnd,
    table: typeof consoleInstance.table,
    time: typeof consoleInstance.time,
    timeEnd: typeof consoleInstance.timeEnd,
    timeLog: typeof consoleInstance.timeLog,
    timeStamp: typeof consoleInstance.timeStamp,
    trace: typeof consoleInstance.trace,
    warn: typeof consoleInstance.warn,
  },
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
  hashesIncludeSha256: crypto.getHashes().includes("sha256"),
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

#[test]
fn stdlib_polyfill_conformance_matches_host_node() {
    assert_conformance(
        "stdlib-polyfills",
        r#"
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const assert = require("node:assert");
const constants = require("node:constants");
const path = require("node:path");
const punycode = require("node:punycode");
const querystring = require("node:querystring");
const stringDecoder = require("node:string_decoder");
const util = require("node:util");
const utilTypes = require("node:util/types");
const zlib = require("node:zlib");

assert.deepStrictEqual(path.normalize?.("/alpha/../beta"), "/beta");
assert.notStrictEqual(1, 2);
assert.strictEqual(typeof assert.fail, "function");

let throwsCode = null;
assert.throws(
  () => {
    const error = new TypeError("boom");
    error.code = "ERR_BOOM";
    throw error;
  },
  (error) => {
    throwsCode = error?.code ?? null;
    return true;
  },
);

let rejectsCode = null;
await assert.rejects(
  Promise.reject(Object.assign(new Error("reject"), { code: "ERR_REJECT" })),
  (error) => {
    rejectsCode = error?.code ?? null;
    return true;
  },
);

const decoder = new stringDecoder.StringDecoder("utf8");
const textBytes = Buffer.from("Grüße", "utf8");
const decoded =
  decoder.write(textBytes.subarray(0, 4)) +
  decoder.end(textBytes.subarray(4));

const formatted = util.format("value:%s count:%d json:%j", "alpha", 7, { ok: true });
const promisified = await util.promisify((value, callback) => callback(null, value.toUpperCase()))("beta");
const encodedLength = new util.TextEncoder().encode("Grüße").length;
const decodedText = new util.TextDecoder().decode(textBytes);

const deflated = zlib.deflateSync(Buffer.from("agent-os", "utf8"));
const inflated = zlib.inflateSync(deflated).toString("utf8");

console.log(JSON.stringify({
  constants: {
    fOk: constants.F_OK ?? null,
    oRdOnly: constants.O_RDONLY ?? null,
    rOk: constants.R_OK ?? null,
  },
  decoded,
  decodedText,
  deflatedBase64: deflated.toString("base64"),
  encodedLength,
  formatted,
  inflated,
  isArrayBufferView: util.types.isArrayBufferView(textBytes),
  isDateViaUtilTypes: utilTypes.isDate(new Date("2024-01-01T00:00:00Z")),
  isMapViaUtilTypes: utilTypes.isMap(new Map([["alpha", 1]])),
  isUint8ArrayViaUtilTypes: utilTypes.isUint8Array(textBytes),
  promisified,
  punycodeAscii: punycode.toASCII("mañana.com"),
  punycodeUnicode: punycode.toUnicode("xn--maana-pta.com"),
  querystringParsed: querystring.parse("a=1&b=x&b=y"),
  querystringStringified: querystring.stringify({ a: 1, b: ["x", "y"] }),
  rejectsCode,
  throwsCode,
}));
"#,
    );
}

#[test]
fn extended_builtin_polyfills_work_in_guest_v8() {
    let result = run_guest_script(
        "extended-builtins",
        r#"
import os from "node:os";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const moduleBuiltin = require("node:module");
const perfHooks = require("node:perf_hooks");
const streamConsumers = require("node:stream/consumers");
const streamPromises = require("node:stream/promises");
const timersPromises = require("node:timers/promises");
const tty = require("node:tty");
const zlib = require("node:zlib");

perfHooks.performance.clearMarks?.();
perfHooks.performance.clearMeasures?.();
perfHooks.performance.mark("start");
await timersPromises.setTimeout(5);
perfHooks.performance.mark("end");
const measure = perfHooks.performance.measure("delta", "start", "end");

const immediateValue = await timersPromises.setImmediate("tick");
const timeoutValue = await timersPromises.setTimeout(1, "done");
const intervalValues = [];
const interval = timersPromises.setInterval(1, "pulse");
intervalValues.push((await interval.next()).value);
intervalValues.push((await interval.next()).value);
await interval.return();

function createSink() {
  const listeners = new Map();
  return {
    chunks: [],
    write(chunk, callback) {
      this.chunks.push(Buffer.from(chunk).toString("utf8"));
      callback?.(null);
    },
    end(callback) {
      queueMicrotask(() => {
        for (const handler of listeners.get("finish") ?? []) handler();
        for (const handler of listeners.get("close") ?? []) handler();
        callback?.(null);
      });
    },
    once(event, handler) {
      const entries = listeners.get(event) ?? [];
      listeners.set(event, [...entries, handler]);
      return this;
    },
    off(event, handler) {
      const entries = listeners.get(event) ?? [];
      listeners.set(
        event,
        entries.filter((candidate) => candidate !== handler),
      );
      return this;
    },
  };
}

const pipelineWritable = createSink();
await streamPromises.pipeline(
  (async function* () {
    yield Buffer.from("left");
    yield Buffer.from("+");
    yield Buffer.from("right");
  })(),
  pipelineWritable,
);

const finishedWritable = createSink();
const finishedResult = streamPromises.finished(finishedWritable).then(() => "resolved");
finishedWritable.end();

function makeAsyncStream(chunks) {
  return (async function* () {
    for (const chunk of chunks) {
      yield chunk;
    }
  })();
}

const textValue = await streamConsumers.text(
  makeAsyncStream([
    Buffer.from("he"),
    Buffer.from("llo"),
  ]),
);
const jsonValue = await streamConsumers.json(
  makeAsyncStream([Buffer.from('{"ok":true,"count":2}')]),
);
const arrayBufferValue = await streamConsumers.arrayBuffer(
  makeAsyncStream([Buffer.from("AB")]),
);
const blobValue = await streamConsumers.blob(
  makeAsyncStream([Buffer.from("blob")]),
);
const bufferValue = await streamConsumers.buffer(
  makeAsyncStream([Buffer.from("buf")]),
);

const deflated = zlib.deflateSync(Buffer.from("agent-os", "utf8"));
const inflated = zlib.inflateSync(deflated).toString("utf8");

process.stdout.write(`${JSON.stringify({
  moduleBuiltinHasCreateRequire:
    typeof moduleBuiltin.createRequire === "function",
  moduleBuiltinHasBuiltinModules:
    Array.isArray(moduleBuiltin.builtinModules),
  moduleBuiltinHasStreamPromises:
    moduleBuiltin.builtinModules.includes("stream/promises"),
  os: {
    arch: os.arch(),
    availableParallelism: os.availableParallelism(),
    cpusLength: os.cpus().length,
    eol: os.EOL,
    freemem: os.freemem(),
    hasSignals: typeof os.constants?.signals?.SIGTERM === "number",
    homedir: os.homedir(),
    hostname: os.hostname(),
    networkInterfaceKeys: Object.keys(os.networkInterfaces()),
    platform: os.platform(),
    release: os.release(),
    tmpdir: os.tmpdir(),
    totalmem: os.totalmem(),
    type: os.type(),
    userInfoHomedir: os.userInfo().homedir,
  },
  perf: {
    entriesByName: perfHooks.performance.getEntriesByName?.("delta", "measure")?.length ?? 0,
    hasNow: typeof perfHooks.performance.now === "function",
    hasObserver: typeof perfHooks.PerformanceObserver === "function",
    measureDurationFinite: Number.isFinite(measure.duration),
  },
  streamConsumers: {
    arrayBufferLength: arrayBufferValue.byteLength,
    blobText: await blobValue.text(),
    bufferText: bufferValue.toString("utf8"),
    jsonCount: jsonValue.count,
    jsonOk: jsonValue.ok,
    textValue,
  },
  streamPromises: {
    finishedResult: await finishedResult,
    pipelineText: pipelineWritable.chunks.join(""),
  },
  timersPromises: {
    immediateValue,
    intervalValues,
    timeoutValue,
  },
  tty: {
    isatty0: tty.isatty(0),
    isatty1: tty.isatty(1),
    isatty2: tty.isatty(2),
    readStreamType: typeof tty.ReadStream,
    writeStreamType: typeof tty.WriteStream,
  },
  zlib: {
    createDeflateType: typeof zlib.createDeflate,
    createInflateType: typeof zlib.createInflate,
    inflated,
  },
})}\n`);
process.exit(0);
"#,
    );

    assert_eq!(result["moduleBuiltinHasCreateRequire"], true);
    assert_eq!(result["moduleBuiltinHasBuiltinModules"], true);
    assert_eq!(result["moduleBuiltinHasStreamPromises"], true);
    assert_eq!(result["os"]["platform"], "linux");
    assert_eq!(result["os"]["arch"], "x64");
    assert_eq!(result["os"]["type"], "Linux");
    assert!(result["os"]["homedir"]
        .as_str()
        .expect("os.homedir string")
        .starts_with('/'));
    assert_eq!(result["os"]["tmpdir"], "/tmp");
    assert_eq!(result["os"]["userInfoHomedir"], result["os"]["homedir"]);
    assert_eq!(result["os"]["eol"], "\n");
    assert_eq!(result["os"]["availableParallelism"], 1);
    assert_eq!(result["os"]["cpusLength"], 1);
    assert_eq!(result["os"]["totalmem"], 1_073_741_824u64);
    assert_eq!(result["os"]["freemem"], 536_870_912u64);
    assert_eq!(result["os"]["hasSignals"], true);
    assert!(result["os"]["networkInterfaceKeys"]
        .as_array()
        .expect("network interfaces array")
        .is_empty());
    assert_eq!(result["perf"]["hasNow"], true);
    assert_eq!(result["perf"]["hasObserver"], true);
    assert_eq!(result["perf"]["measureDurationFinite"], true);
    assert_eq!(result["perf"]["entriesByName"], 1);
    assert_eq!(result["timersPromises"]["immediateValue"], "tick");
    assert_eq!(result["timersPromises"]["timeoutValue"], "done");
    assert_eq!(
        result["timersPromises"]["intervalValues"]
            .as_array()
            .expect("interval values"),
        &vec![Value::from("pulse"), Value::from("pulse")]
    );
    assert_eq!(result["streamPromises"]["pipelineText"], "left+right");
    assert_eq!(result["streamPromises"]["finishedResult"], "resolved");
    assert_eq!(result["streamConsumers"]["textValue"], "hello");
    assert_eq!(result["streamConsumers"]["jsonOk"], true);
    assert_eq!(result["streamConsumers"]["jsonCount"], 2);
    assert_eq!(result["streamConsumers"]["arrayBufferLength"], 2);
    assert_eq!(result["streamConsumers"]["blobText"], "blob");
    assert_eq!(result["streamConsumers"]["bufferText"], "buf");
    assert_eq!(result["tty"]["readStreamType"], "function");
    assert_eq!(result["tty"]["writeStreamType"], "function");
    assert_eq!(result["tty"]["isatty0"], false);
    assert_eq!(result["tty"]["isatty1"], false);
    assert_eq!(result["tty"]["isatty2"], false);
    assert_eq!(result["zlib"]["createDeflateType"], "function");
    assert_eq!(result["zlib"]["createInflateType"], "function");
    assert_eq!(result["zlib"]["inflated"], "agent-os");
}
