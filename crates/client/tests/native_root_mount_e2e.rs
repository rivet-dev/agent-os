//! Regression repro for "guest WASM commands cannot read mount-backed root files".
//!
//! When the VM root filesystem is a `js_bridge` mount (the `@rivet-dev/agentos`
//! actor plugin default: `RootFilesystemKind::Native` + `MountPlugin { id:
//! "js_bridge" }`), host `writeFile`/`readFile` round-trip through the bridge,
//! but guest WASM commands used to see broken views: `cat file` exited 0 with
//! empty output, `wc -c` reported 0 bytes, and `sh -c 'cd /workspace'` failed
//! with "not a directory".
//!
//! This suite reproduces the exact contract with an in-memory js_bridge backend
//! standing in for actor durable storage: host writeFile -> guest `cat` must
//! print the written bytes.

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use agentos_client::config::{
    AgentOsConfig, MountPlugin, PackageRef, RootFilesystemConfig, RootFilesystemKind,
    SidecarJsBridgeCall, SidecarJsBridgeCallback,
};
use agentos_client::fs::FileContent;
use agentos_client::{AgentOs, ExecOptions};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde_json::{json, Value};

const DEFAULT_FILE_MODE: u32 = 0o644;
const DEFAULT_DIR_MODE: u32 = 0o755;

#[derive(Clone, Debug)]
struct MemEntry {
    is_directory: bool,
    content: Vec<u8>,
    mode: u32,
    uid: u32,
    gid: u32,
    symlink_target: Option<String>,
}

/// Minimal in-memory implementation of the `js_bridge` mount operation
/// contract (the same op set used by sidecar-native root plugins
/// services from actor SQLite).
#[derive(Default)]
struct MemBridgeFs {
    entries: Mutex<BTreeMap<String, MemEntry>>,
}

impl MemBridgeFs {
    fn new() -> Self {
        let fs = Self::default();
        let mut entries = fs.entries.lock().unwrap();
        entries.insert(
            "/".to_string(),
            MemEntry {
                is_directory: true,
                content: Vec::new(),
                mode: DEFAULT_DIR_MODE,
                uid: 0,
                gid: 0,
                symlink_target: None,
            },
        );
        // Model the actor VM's persisted working directory. Host filesystem
        // APIs are trusted and create root-owned entries, while guest commands
        // run as the default uid/gid 1000 and need an owned directory in which
        // to create files.
        entries.insert(
            "/workspace".to_string(),
            MemEntry {
                is_directory: true,
                content: Vec::new(),
                mode: DEFAULT_DIR_MODE,
                uid: 1000,
                gid: 1000,
                symlink_target: None,
            },
        );
        drop(entries);
        fs
    }

    fn normalize(path: &str) -> String {
        let mut segments: Vec<&str> = Vec::new();
        for segment in path.split('/') {
            match segment {
                "" | "." => {}
                ".." => {
                    segments.pop();
                }
                other => segments.push(other),
            }
        }
        if segments.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", segments.join("/"))
        }
    }

    fn stat_json(path: &str, entry: &MemEntry) -> Value {
        let size = entry.content.len() as u64;
        // Permission-only `mode` (no S_IFMT file-type bits) with the entry
        // type carried by the `isDirectory` / `isSymbolicLink` booleans — the
        // exact stat shape the sidecar-native mount contract
        // `stat_json` produces. The sidecar must normalize this, not require
        // every bridge backend to encode type bits into `mode`.
        json!({
            "dev": 0,
            "ino": ino_for(path),
            "mode": entry.mode,
            "nlink": 1,
            "uid": entry.uid,
            "gid": entry.gid,
            "rdev": 0,
            "size": size,
            "blocks": size.div_ceil(512),
            "atimeMs": 0,
            "mtimeMs": 0,
            "ctimeMs": 0,
            "birthtimeMs": 0,
            "isDirectory": entry.is_directory,
            "isSymbolicLink": entry.symlink_target.is_some(),
        })
    }

    fn handle(&self, operation: &str, args: &Value) -> Result<Option<Value>, String> {
        let path = || -> Result<String, String> {
            args.get("path")
                .and_then(Value::as_str)
                .map(Self::normalize)
                .ok_or_else(|| format!("EINVAL missing path for {operation}"))
        };
        let mut entries = self.entries.lock().unwrap();
        match operation {
            "readFile" => {
                let path = path()?;
                let entry = entries
                    .get(&path)
                    .ok_or_else(|| format!("ENOENT no such file: {path}"))?;
                if entry.is_directory {
                    return Err(format!("EISDIR is a directory: {path}"));
                }
                Ok(Some(json!(BASE64.encode(&entry.content))))
            }
            "pread" => {
                let path = path()?;
                let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
                let len = args
                    .get("len")
                    .or_else(|| args.get("length"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
                let entry = entries
                    .get(&path)
                    .ok_or_else(|| format!("ENOENT no such file: {path}"))?;
                if entry.is_directory {
                    return Err(format!("EISDIR is a directory: {path}"));
                }
                let start = offset.min(entry.content.len());
                let end = start.saturating_add(len).min(entry.content.len());
                Ok(Some(json!(BASE64.encode(&entry.content[start..end]))))
            }
            "pwrite" => {
                let path = path()?;
                let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
                let content = args
                    .get("content")
                    .and_then(Value::as_str)
                    .map(|encoded| BASE64.decode(encoded))
                    .transpose()
                    .map_err(|error| format!("EINVAL bad base64 content: {error}"))?
                    .unwrap_or_default();
                let entry = entries
                    .get_mut(&path)
                    .ok_or_else(|| format!("ENOENT no such file: {path}"))?;
                if entry.is_directory {
                    return Err(format!("EISDIR is a directory: {path}"));
                }
                let end = offset
                    .checked_add(content.len())
                    .ok_or_else(|| "EFBIG write offset overflow".to_string())?;
                if entry.content.len() < end {
                    entry.content.resize(end, 0);
                }
                entry.content[offset..end].copy_from_slice(&content);
                Ok(None)
            }
            "writeFile" | "createFileExclusive" => {
                let path = path()?;
                if operation == "createFileExclusive" && entries.contains_key(&path) {
                    return Err(format!("EEXIST file exists: {path}"));
                }
                let content = args
                    .get("content")
                    .and_then(Value::as_str)
                    .map(|encoded| BASE64.decode(encoded))
                    .transpose()
                    .map_err(|error| format!("EINVAL bad base64 content: {error}"))?
                    .unwrap_or_default();
                let mode = args
                    .get("mode")
                    .and_then(Value::as_u64)
                    .map(|mode| mode as u32)
                    .unwrap_or(DEFAULT_FILE_MODE);
                entries.insert(
                    path,
                    MemEntry {
                        is_directory: false,
                        content,
                        mode,
                        uid: 0,
                        gid: 0,
                        symlink_target: None,
                    },
                );
                Ok(None)
            }
            "readDir" | "readDirWithTypes" => {
                let path = path()?;
                let entry = entries
                    .get(&path)
                    .ok_or_else(|| format!("ENOENT no such directory: {path}"))?;
                if !entry.is_directory {
                    return Err(format!("ENOTDIR not a directory: {path}"));
                }
                let prefix = if path == "/" {
                    "/".to_string()
                } else {
                    format!("{path}/")
                };
                let mut children = Vec::new();
                for (child_path, child) in entries.iter() {
                    let Some(rest) = child_path.strip_prefix(&prefix) else {
                        continue;
                    };
                    if rest.is_empty() || rest.contains('/') {
                        continue;
                    }
                    if operation == "readDir" {
                        children.push(json!(rest));
                    } else {
                        children.push(json!({
                            "name": rest,
                            "isDirectory": child.is_directory,
                            "isSymbolicLink": child.symlink_target.is_some(),
                        }));
                    }
                }
                Ok(Some(Value::Array(children)))
            }
            "createDir" | "mkdir" => {
                let path = path()?;
                let recursive = operation == "mkdir"
                    && args
                        .get("recursive")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                let mode = args
                    .get("mode")
                    .and_then(Value::as_u64)
                    .map(|mode| mode as u32)
                    .unwrap_or(DEFAULT_DIR_MODE);
                if entries.get(&path).is_some_and(|entry| entry.is_directory) {
                    if recursive {
                        return Ok(None);
                    }
                    return Err(format!("EEXIST directory exists: {path}"));
                }
                let mut ancestors = Vec::new();
                let mut current = path.clone();
                while current != "/" {
                    ancestors.push(current.clone());
                    match current.rfind('/') {
                        Some(0) => current = "/".to_string(),
                        Some(index) => current.truncate(index),
                        None => break,
                    }
                }
                if !recursive && ancestors.len() > 1 {
                    let parent = &ancestors[1];
                    if !entries.get(parent).is_some_and(|entry| entry.is_directory) {
                        return Err(format!("ENOENT missing parent for: {path}"));
                    }
                }
                for ancestor in ancestors.into_iter().rev() {
                    entries.entry(ancestor).or_insert(MemEntry {
                        is_directory: true,
                        content: Vec::new(),
                        mode,
                        uid: 0,
                        gid: 0,
                        symlink_target: None,
                    });
                }
                Ok(None)
            }
            "exists" => Ok(Some(json!(entries.contains_key(&path()?)))),
            "stat" | "lstat" => {
                let path = path()?;
                let entry = entries
                    .get(&path)
                    .ok_or_else(|| format!("ENOENT no such entry: {path}"))?;
                Ok(Some(Self::stat_json(&path, entry)))
            }
            "realpath" => Ok(Some(json!(path()?))),
            "removeFile" => {
                let path = path()?;
                match entries.get(&path) {
                    Some(entry) if entry.is_directory => {
                        Err(format!("EISDIR is a directory: {path}"))
                    }
                    Some(_) => {
                        entries.remove(&path);
                        Ok(None)
                    }
                    None => Err(format!("ENOENT no such file: {path}")),
                }
            }
            "removeDir" => {
                let path = path()?;
                match entries.get(&path) {
                    Some(entry) if !entry.is_directory => {
                        Err(format!("ENOTDIR not a directory: {path}"))
                    }
                    Some(_) => {
                        let prefix = format!("{path}/");
                        if entries.keys().any(|child| child.starts_with(&prefix)) {
                            return Err(format!("ENOTEMPTY directory not empty: {path}"));
                        }
                        entries.remove(&path);
                        Ok(None)
                    }
                    None => Err(format!("ENOENT no such directory: {path}")),
                }
            }
            "rename" => {
                let old_path = args
                    .get("oldPath")
                    .and_then(Value::as_str)
                    .map(Self::normalize)
                    .ok_or_else(|| "EINVAL missing oldPath".to_string())?;
                let new_path = args
                    .get("newPath")
                    .and_then(Value::as_str)
                    .map(Self::normalize)
                    .ok_or_else(|| "EINVAL missing newPath".to_string())?;
                let moved: Vec<(String, MemEntry)> = entries
                    .iter()
                    .filter(|(path, _)| {
                        **path == old_path || path.starts_with(&format!("{old_path}/"))
                    })
                    .map(|(path, entry)| {
                        let suffix = &path[old_path.len()..];
                        (format!("{new_path}{suffix}"), entry.clone())
                    })
                    .collect();
                if moved.is_empty() {
                    return Err(format!("ENOENT no such entry: {old_path}"));
                }
                entries.retain(|path, _| {
                    path != &old_path && !path.starts_with(&format!("{old_path}/"))
                });
                entries.extend(moved);
                Ok(None)
            }
            "symlink" => {
                let target = args
                    .get("target")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "EINVAL missing target".to_string())?
                    .to_string();
                let link_path = args
                    .get("linkPath")
                    .and_then(Value::as_str)
                    .map(Self::normalize)
                    .ok_or_else(|| "EINVAL missing linkPath".to_string())?;
                entries.insert(
                    link_path,
                    MemEntry {
                        is_directory: false,
                        content: Vec::new(),
                        mode: 0o777,
                        uid: 0,
                        gid: 0,
                        symlink_target: Some(target),
                    },
                );
                Ok(None)
            }
            "readLink" => {
                let path = path()?;
                let entry = entries
                    .get(&path)
                    .ok_or_else(|| format!("ENOENT no such entry: {path}"))?;
                match &entry.symlink_target {
                    Some(target) => Ok(Some(json!(target))),
                    None => Err(format!("EINVAL not a symlink: {path}")),
                }
            }
            "link" => {
                let old_path = args
                    .get("oldPath")
                    .and_then(Value::as_str)
                    .map(Self::normalize)
                    .ok_or_else(|| "EINVAL missing oldPath".to_string())?;
                let new_path = args
                    .get("newPath")
                    .and_then(Value::as_str)
                    .map(Self::normalize)
                    .ok_or_else(|| "EINVAL missing newPath".to_string())?;
                let entry = entries
                    .get(&old_path)
                    .ok_or_else(|| format!("ENOENT no such entry: {old_path}"))?
                    .clone();
                entries.insert(new_path, entry);
                Ok(None)
            }
            "chmod" => {
                let path = path()?;
                let mode = args
                    .get("mode")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| "EINVAL missing mode".to_string())?
                    as u32;
                let entry = entries
                    .get_mut(&path)
                    .ok_or_else(|| format!("ENOENT no such entry: {path}"))?;
                entry.mode = mode & 0o7777;
                Ok(None)
            }
            "chown" => {
                let path = path()?;
                let entry = entries
                    .get_mut(&path)
                    .ok_or_else(|| format!("ENOENT no such entry: {path}"))?;
                entry.uid = args
                    .get("uid")
                    .and_then(Value::as_u64)
                    .map(|uid| uid as u32)
                    .unwrap_or(entry.uid);
                entry.gid = args
                    .get("gid")
                    .and_then(Value::as_u64)
                    .map(|gid| gid as u32)
                    .unwrap_or(entry.gid);
                Ok(None)
            }
            "utimes" => {
                let path = path()?;
                if !entries.contains_key(&path) {
                    return Err(format!("ENOENT no such entry: {path}"));
                }
                Ok(None)
            }
            "truncate" => {
                let path = path()?;
                let len = args
                    .get("len")
                    .or_else(|| args.get("length"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
                let entry = entries
                    .get_mut(&path)
                    .ok_or_else(|| format!("ENOENT no such file: {path}"))?;
                entry.content.resize(len, 0);
                Ok(None)
            }
            operation => Err(format!("ENOSYS unsupported operation {operation}")),
        }
    }
}

fn ino_for(path: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    hasher.finish() | 1
}

fn mem_bridge_callback(fs: Arc<MemBridgeFs>) -> SidecarJsBridgeCallback {
    Arc::new(move |call: SidecarJsBridgeCall| {
        let fs = fs.clone();
        Box::pin(async move { fs.handle(&call.operation, &call.args) })
    })
}

/// Boot a VM whose ROOT filesystem is a js_bridge mount (the actor-plugin
/// shape) plus the coreutils command package, mirroring
/// the native root VM configuration path.
async fn new_bridge_root_vm() -> Option<AgentOs> {
    common::ensure_sidecar_env();
    let package_dir = common::coreutils_package_dir()?;
    let backend = Arc::new(MemBridgeFs::new());
    let config = AgentOsConfig {
        packages: vec![PackageRef {
            path: package_dir.to_string_lossy().into_owned(),
        }],
        root_filesystem: RootFilesystemConfig {
            kind: RootFilesystemKind::Native,
            native_plugin: Some(MountPlugin {
                id: "js_bridge".to_owned(),
                config: Some(json!({ "mountId": "native-root-e2e" })),
            }),
            ..RootFilesystemConfig::default()
        },
        sidecar_js_bridge_callback: Some(mem_bridge_callback(backend)),
        ..Default::default()
    };
    Some(
        AgentOs::create(config)
            .await
            .expect("create VM with js_bridge native root"),
    )
}

#[tokio::test]
async fn native_root_mount_files_visible_to_wasm_commands() {
    if !common::require_sidecar("native_root_mount_files_visible_to_wasm_commands") {
        return;
    }
    let Some(os) = new_bridge_root_vm().await else {
        eprintln!(
            "skipping native_root_mount_files_visible_to_wasm_commands: coreutils package artifacts absent"
        );
        return;
    };

    // Bootstrap may have created /workspace already; tolerate both.
    if let Err(error) = os
        .mkdir("/workspace", agentos_client::fs::MkdirOptions::default())
        .await
    {
        assert!(
            error.to_string().contains("EEXIST"),
            "unexpected mkdir error: {error}"
        );
    }
    os.write_file(
        "/workspace/data.txt",
        FileContent::Text("hello-bridge-root".to_string()),
    )
    .await
    .expect("write /workspace/data.txt");

    // Host round-trip must work (it always did).
    assert_eq!(
        os.read_file("/workspace/data.txt")
            .await
            .expect("host read"),
        b"hello-bridge-root"
    );

    // The regression: guest WASM commands must observe the mount-backed root
    // content, not an empty/broken view.
    let cat = os
        .exec_process("cat /workspace/data.txt", ExecOptions::default())
        .await
        .expect("exec cat");
    assert_eq!(
        cat.exit_code, 0,
        "cat should exit 0 (stderr: {:?})",
        cat.stderr
    );
    assert_eq!(
        cat.stdout.trim_end(),
        "hello-bridge-root",
        "guest cat must print the host-written bytes (stderr: {:?})",
        cat.stderr
    );

    let wc = os
        .exec_process("wc -c /workspace/data.txt", ExecOptions::default())
        .await
        .expect("exec wc");
    assert_eq!(
        wc.exit_code, 0,
        "wc should exit 0 (stderr: {:?})",
        wc.stderr
    );
    assert_eq!(
        wc.stdout.trim_start().split(' ').next().unwrap_or_default(),
        "17",
        "wc must count the real byte length (stdout: {:?})",
        wc.stdout
    );

    // A command spawned by the guest shell must retain the same mounted root.
    let sh_absolute = os
        .exec_process("sh -c 'cat /workspace/data.txt'", ExecOptions::default())
        .await
        .expect("exec sh absolute cat");
    assert_eq!(
        sh_absolute.exit_code, 0,
        "sh absolute cat should exit 0 (stderr: {:?})",
        sh_absolute.stderr
    );
    assert_eq!(sh_absolute.stdout.trim_end(), "hello-bridge-root");

    // Shell traversal into the mounted root must work too.
    let sh = os
        .exec_process(
            "sh -c 'cd /workspace && cat data.txt'",
            ExecOptions::default(),
        )
        .await
        .expect("exec sh cd");
    assert_eq!(
        sh.exit_code, 0,
        "sh cd should exit 0 (stderr: {:?})",
        sh.stderr
    );
    assert_eq!(sh.stdout.trim_end(), "hello-bridge-root");

    let relative_write = os
        .exec_process(
            "sh -c 'cd /workspace && echo relative-write > relative.txt'",
            ExecOptions::default(),
        )
        .await
        .expect("exec relative guest write");
    assert_eq!(
        relative_write.exit_code, 0,
        "relative guest write should exit 0 (stderr: {:?})",
        relative_write.stderr
    );
    assert_eq!(
        os.read_file("/workspace/relative.txt")
            .await
            .expect("host read of relative guest write"),
        b"relative-write\n"
    );

    // Guest writes must round-trip back to the host view as well.
    let write = os
        .exec_process(
            "sh -c 'echo guest-write > /workspace/out.txt'",
            ExecOptions::default(),
        )
        .await
        .expect("exec guest write");
    assert_eq!(
        write.exit_code, 0,
        "guest write should exit 0 (stderr: {:?})",
        write.stderr
    );
    assert_eq!(
        os.read_file("/workspace/out.txt")
            .await
            .expect("host read of guest write"),
        b"guest-write\n"
    );

    os.shutdown().await.expect("shutdown");
}
