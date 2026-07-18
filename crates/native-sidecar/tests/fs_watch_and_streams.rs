mod support;

use agentos_native_sidecar::wire::{
    ConfigureVmRequest, CreateVmRequest, GuestRuntimeKind, MountDescriptor, MountPluginDescriptor,
    RequestPayload, ResponsePayload, RootFilesystemDescriptor, RootFilesystemEntry,
    RootFilesystemEntryEncoding, RootFilesystemEntryKind, RootFilesystemMode,
};
use serde_json::json;
use std::collections::HashMap;
use std::time::Duration;
use support::{
    assert_node_available, authenticate_wire, collect_process_output_wire_with_timeout,
    execute_wire, new_sidecar, open_session_wire, temp_dir, wire_permissions_allow_all,
    wire_request, wire_session, write_fixture,
};

fn root_dir(path: &str, mode: u32) -> RootFilesystemEntry {
    RootFilesystemEntry {
        path: path.to_owned(),
        kind: RootFilesystemEntryKind::Directory,
        mode: Some(mode),
        uid: None,
        gid: None,
        content: None,
        encoding: None,
        target: None,
        executable: false,
    }
}

fn root_file(path: &str, content: &str) -> RootFilesystemEntry {
    RootFilesystemEntry {
        path: path.to_owned(),
        kind: RootFilesystemEntryKind::File,
        mode: None,
        uid: Some(1000),
        gid: Some(1000),
        content: Some(content.to_owned()),
        encoding: Some(RootFilesystemEntryEncoding::Utf8),
        target: None,
        executable: false,
    }
}

#[test]
fn javascript_fs_watch_and_streams_work_against_the_vm_kernel_filesystem() {
    assert_node_available();

    let mut sidecar = new_sidecar("fs-watch-and-streams");
    let cwd = temp_dir("fs-watch-and-streams-cwd");
    let entry = cwd.join("fs-watch-and-streams.mjs");

    write_fixture(
        &entry,
        r#"
import fs from "node:fs";
import { once } from "node:events";

const readChunks = [];
const reader = fs.createReadStream("/rpc/input.txt", {
  encoding: "utf8",
  start: 1,
  end: 5,
  highWaterMark: 2,
});
reader.on("data", (chunk) => readChunks.push(chunk));
await once(reader, "close");

const writer = fs.createWriteStream("/rpc/output.txt", {
  start: 2,
  highWaterMark: 2,
});
writer.write("XY");
writer.end("Z");
await once(writer, "close");

const watchEvents = [];
const watchFileEvents = [];
const watcher = fs.watch("/rpc/watch.txt", (eventType, filename) => {
  watchEvents.push({
    eventType,
    filename: Buffer.isBuffer(filename) ? filename.toString("utf8") : filename,
  });
});
fs.watchFile("/rpc/watch.txt", { interval: 20 }, (curr, prev) => {
  watchFileEvents.push({
    currSize: curr.size,
    prevSize: prev.size,
  });
});

setTimeout(() => {
  fs.writeFileSync("/rpc/watch.txt", "after!!");
}, 60);

const deadline = Date.now() + 3000;
while (watchEvents.length === 0 || watchFileEvents.length === 0) {
  if (Date.now() > deadline) {
    watcher.close();
    fs.unwatchFile("/rpc/watch.txt");
    throw new Error(
      `timed out waiting for watch events: ${JSON.stringify({
        watchEvents,
        watchFileEvents,
      })}`,
    );
  }
  await new Promise((resolve) => setTimeout(resolve, 20));
}

watcher.close();
fs.unwatchFile("/rpc/watch.txt");

console.log(
  JSON.stringify({
    readChunks,
    output: fs.readFileSync("/rpc/output.txt", "utf8"),
    watchEvents,
    watchFileEvents,
  }),
);
"#,
    );

    let connection_id = authenticate_wire(&mut sidecar, "conn-fs-watch-and-streams");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let create = sidecar
        .dispatch_wire_blocking(wire_request(
            3,
            wire_session(&connection_id, &session_id),
            RequestPayload::CreateVmRequest(CreateVmRequest::legacy_test_config(
                GuestRuntimeKind::JavaScript,
                HashMap::from([(String::from("cwd"), cwd.to_string_lossy().into_owned())]),
                RootFilesystemDescriptor {
                    mode: RootFilesystemMode::Ephemeral,
                    disable_default_base_layer: false,
                    lowers: Vec::new(),
                    bootstrap_entries: vec![
                        root_dir("/rpc", 0o755),
                        root_file("/rpc/input.txt", "abcdefg"),
                        root_file("/rpc/output.txt", "hello"),
                        root_file("/rpc/watch.txt", "before"),
                    ],
                },
                Some(wire_permissions_allow_all()),
            )),
        ))
        .expect("create sidecar vm");
    let vm_id = match create.response.payload {
        ResponsePayload::VmCreatedResponse(response) => response.vm_id,
        other => panic!("unexpected create vm response: {other:?}"),
    };

    execute_wire(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "fs-watch-and-streams",
        GuestRuntimeKind::JavaScript,
        &entry,
        Vec::new(),
    );

    let (stdout, stderr, exit_code) = collect_process_output_wire_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "fs-watch-and-streams",
        Duration::from_secs(10),
    );

    assert_eq!(exit_code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(stderr.trim().is_empty(), "unexpected stderr:\n{stderr}");

    let json_line = stdout
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .expect("stdout json line");
    let payload: serde_json::Value =
        serde_json::from_str(json_line).expect("parse fs watch and streams result");

    assert_eq!(payload["readChunks"], serde_json::json!(["bc", "de", "f"]));
    assert_eq!(payload["output"], "\u{0}\u{0}XYZ");
    assert_eq!(payload["watchEvents"][0]["eventType"], "change");
    assert_eq!(payload["watchEvents"][0]["filename"], "watch.txt");
    assert_eq!(payload["watchFileEvents"][0]["prevSize"], 6);
    assert_eq!(payload["watchFileEvents"][0]["currSize"], 7);
}

#[test]
fn javascript_host_fs_watch_and_glob_metadata_work_is_bounded() {
    assert_node_available();

    let mut sidecar = new_sidecar("fs-watch-poll-backoff");
    let cwd = temp_dir("fs-watch-poll-backoff-cwd");
    let entry = cwd.join("fs-watch-poll-backoff.mjs");
    let watched_host_dir = temp_dir("fs-watch-poll-backoff-host");
    let watched_host_parent = watched_host_dir.join("skills/use-railway/references");
    std::fs::create_dir_all(&watched_host_parent).expect("create watched host directory");
    std::fs::write(watched_host_parent.join("watch.txt"), "unchanged")
        .expect("seed watched host file");
    std::fs::create_dir_all(watched_host_dir.join("included/nested"))
        .expect("create included glob directory");
    std::fs::create_dir_all(watched_host_dir.join("excluded/nested"))
        .expect("create excluded glob directory");
    std::fs::write(watched_host_dir.join("included/nested/keep.ts"), "keep")
        .expect("seed included glob file");
    std::fs::write(watched_host_dir.join("excluded/nested/skip.ts"), "skip")
        .expect("seed excluded glob file");

    write_fixture(
        &entry,
        r#"
import fs from "node:fs";

const watchedPath = "/watched/skills/use-railway/references/watch.txt";
const watcherCount = 32;
const originalStatSync = fs.statSync;
let watchedStatCalls = 0;
fs.statSync = function(path, ...args) {
  if (path === watchedPath) watchedStatCalls += 1;
  return originalStatSync.call(this, path, ...args);
};

const watchers = Array.from(
  { length: watcherCount },
  () => fs.watch(watchedPath),
);
await new Promise((resolve) => setTimeout(resolve, 325));
for (const watcher of watchers) watcher.close();
fs.statSync = originalStatSync;

let globStatCalls = 0;
const globReadDirs = [];
const originalReaddirSync = fs.readdirSync;
fs.statSync = function(path, ...args) {
  globStatCalls += 1;
  return originalStatSync.call(this, path, ...args);
};
fs.readdirSync = function(path, ...args) {
  globReadDirs.push(String(path));
  return originalReaddirSync.call(this, path, ...args);
};
const globMatches = fs.globSync("**/*.ts", {
  cwd: "/watched",
  exclude: ["excluded/**"],
});
const globDirectoryMatches = fs.globSync("**/*", {
  cwd: "/watched",
  exclude: ["excluded/**"],
});
fs.statSync = originalStatSync;
fs.readdirSync = originalReaddirSync;

console.log(JSON.stringify({
  watcherCount,
  watchedStatCalls,
  globMatches,
  globDirectoryMatches,
  globReadDirs,
  globStatCalls,
}));
"#,
    );

    let connection_id = authenticate_wire(&mut sidecar, "conn-fs-watch-poll-backoff");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let create = sidecar
        .dispatch_wire_blocking(wire_request(
            3,
            wire_session(&connection_id, &session_id),
            RequestPayload::CreateVmRequest(CreateVmRequest::legacy_test_config(
                GuestRuntimeKind::JavaScript,
                HashMap::from([(String::from("cwd"), cwd.to_string_lossy().into_owned())]),
                RootFilesystemDescriptor {
                    mode: RootFilesystemMode::Ephemeral,
                    disable_default_base_layer: false,
                    lowers: Vec::new(),
                    bootstrap_entries: Vec::new(),
                },
                Some(wire_permissions_allow_all()),
            )),
        ))
        .expect("create sidecar vm");
    let vm_id = match create.response.payload {
        ResponsePayload::VmCreatedResponse(response) => response.vm_id,
        other => panic!("unexpected create vm response: {other:?}"),
    };

    sidecar
        .dispatch_wire_blocking(wire_request(
            4,
            support::wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ConfigureVmRequest(ConfigureVmRequest {
                mounts: vec![MountDescriptor {
                    guest_path: String::from("/watched"),
                    guest_source: String::from("host_dir"),
                    guest_fstype: String::from("host_dir"),
                    read_only: true,
                    plugin: MountPluginDescriptor {
                        id: String::from("host_dir"),
                        config: serde_json::to_string(&json!({
                            "hostPath": watched_host_dir,
                            "readOnly": true,
                        }))
                        .expect("serialize watched host mount"),
                    },
                }],
                software: Vec::new(),
                permissions: None,
                module_access_cwd: None,
                instructions: Vec::new(),
                projected_modules: Vec::new(),
                command_permissions: HashMap::new(),
                loopback_exempt_ports: Vec::new(),
                packages: Vec::new(),
                packages_mount_at: String::new(),
                bootstrap_commands: Vec::new(),
                binding_shim_commands: Vec::new(),
            }),
        ))
        .expect("configure watched host mount");

    execute_wire(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "fs-watch-poll-backoff",
        GuestRuntimeKind::JavaScript,
        &entry,
        Vec::new(),
    );

    let (stdout, stderr, exit_code) = collect_process_output_wire_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "fs-watch-poll-backoff",
        Duration::from_secs(10),
    );

    assert_eq!(exit_code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(stderr.trim().is_empty(), "unexpected stderr:\n{stderr}");

    let json_line = stdout
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .expect("stdout json line");
    let payload: serde_json::Value =
        serde_json::from_str(json_line).expect("parse fs watch poll backoff result");
    let watcher_count = payload["watcherCount"].as_u64().expect("watcher count");
    let watched_stat_calls = payload["watchedStatCalls"]
        .as_u64()
        .expect("watched stat calls");

    assert!(
        watched_stat_calls >= watcher_count * 2,
        "watchers should poll at least once after their initial snapshot: {payload}"
    );
    assert!(
        watched_stat_calls <= watcher_count * 4,
        "idle watchers must back off instead of polling every 50 ms: {payload}"
    );
    assert_eq!(
        payload["globMatches"],
        json!(["included/nested/keep.ts"]),
        "glob must honor cwd and exclude"
    );
    assert_eq!(
        payload["globStatCalls"], 0,
        "glob should use typed directory entries instead of statting every child"
    );
    assert!(
        payload["globDirectoryMatches"]
            .as_array()
            .expect("glob directory matches")
            .contains(&json!("excluded")),
        "an exclude ending in /** prunes children without hiding the directory itself"
    );
    assert!(
        !payload["globDirectoryMatches"]
            .as_array()
            .expect("glob directory matches")
            .contains(&json!("excluded/nested/skip.ts")),
        "excluded descendants must not be returned"
    );
    assert!(
        payload["globReadDirs"]
            .as_array()
            .expect("glob read directories")
            .iter()
            .all(|path| !path.as_str().unwrap_or_default().contains("/excluded")),
        "glob must prune excluded directory trees: {payload}"
    );
}
