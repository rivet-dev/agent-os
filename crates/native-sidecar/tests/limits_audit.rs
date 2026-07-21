//! Audit test: every limit-shaped constant in the scanned source roots must be classified in
//! `fixtures/limits-inventory.json` as `policy`, `policy-deferred`, or `invariant`. A new
//! `MAX_*` / `*_LIMIT` / capacity / retention constant that is not classified fails this test
//! with instructions, so operator-tunable bounds cannot silently accumulate as hardcoded values.
//!
//! This is a pure filesystem test: no VM, no V8, no new dependencies (`serde_json` is already a
//! native-sidecar dependency). The match rule is hand-rolled string checks, asserted by its own unit
//! cases below.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ScannedConst {
    name: String,
    path: String,
}

/// Resolve the workspace root from `CARGO_MANIFEST_DIR` (which points at `crates/native-sidecar`).
fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .expect("crates/native-sidecar has a workspace root two levels up")
        .to_path_buf()
}

const SKIP_DIRS: &[&str] = &["target", "node_modules", "dist", "tests", "fixtures"];

/// Decide whether a constant name is a limit-shaped bound that must be classified. Mirrors the
/// rule documented in `limits-config.md`. Env-var-name constants (`*_ENV`) and error-code string
/// constants (`*_ERROR_CODE`) are excluded because they name a knob or code, not a bound.
fn name_qualifies(name: &str) -> bool {
    if name.ends_with("_ENV") || name.ends_with("_ERROR_CODE") {
        return false;
    }
    if name.contains("MAX_")
        || name.contains("_MAX")
        || name.contains("_LIMIT")
        || name.contains("LIMIT_")
        || name.contains("_CAPACITY")
        || name.contains("RETENTION")
    {
        return true;
    }
    if name.ends_with("_CAP") || name.contains("_CAP_") {
        return true;
    }
    if let Some(rest) = name.strip_prefix("DEFAULT_") {
        if rest.contains("BYTES") || rest.contains("TIMEOUT") || rest.contains("ENTRIES") {
            return true;
        }
    }
    false
}

/// Extract a constant name from a Rust `const` declaration line, if present.
/// Matches `^\s*(pub(\(crate\))?\s+)?const\s+([A-Z][A-Z0-9_]*)\s*:`.
fn rust_const_name(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let after_vis = trimmed
        .strip_prefix("pub(crate) ")
        .or_else(|| trimmed.strip_prefix("pub "))
        .or_else(|| {
            trimmed
                .strip_prefix("pub(in ")
                .and_then(|rest| rest.split_once(") ").map(|(_, declaration)| declaration))
        })
        .unwrap_or(trimmed);
    let after_const = after_vis.strip_prefix("const ")?;
    let name = identifier_prefix(after_const);
    if name.is_empty() {
        return None;
    }
    let rest = after_const[name.len()..].trim_start();
    if !rest.starts_with(':') {
        return None;
    }
    if is_screaming_snake(name) {
        Some(name)
    } else {
        None
    }
}

/// Extract a constant name from a TS/JS `const` declaration line, if present.
/// Matches `^\s*(export\s+)?const\s+([A-Z][A-Z0-9_]*)\s*=`.
fn ts_const_name(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let after_export = trimmed.strip_prefix("export ").unwrap_or(trimmed);
    let after_const = after_export.strip_prefix("const ")?;
    let name = identifier_prefix(after_const);
    if name.is_empty() {
        return None;
    }
    let rest = after_const[name.len()..].trim_start();
    if !rest.starts_with('=') {
        return None;
    }
    if is_screaming_snake(name) {
        Some(name)
    } else {
        None
    }
}

fn identifier_prefix(input: &str) -> &str {
    let end = input
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '_'))
        .map(|(idx, _)| idx)
        .unwrap_or(input.len());
    &input[..end]
}

/// SCREAMING_SNAKE_CASE: starts with an uppercase ASCII letter, contains only uppercase letters,
/// digits, and underscores.
fn is_screaming_snake(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() => {}
        _ => return false,
    }
    name.chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn scan_file(full: &Path, rel: &str, is_ts: bool, found: &mut Vec<ScannedConst>) {
    let contents = fs::read_to_string(full)
        .unwrap_or_else(|error| panic!("failed to read scanned file {rel}: {error}"));
    for line in contents.lines() {
        let name = if is_ts {
            ts_const_name(line)
        } else {
            rust_const_name(line)
        };
        if let Some(name) = name {
            if name_qualifies(name) {
                found.push(ScannedConst {
                    name: name.to_string(),
                    path: rel.to_string(),
                });
            }
        }
    }
}

fn scan_dir(root: &Path, dir: &Path, extension: &str, is_ts: bool, found: &mut Vec<ScannedConst>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries {
        let entry = entry.expect("readable directory entry");
        let path = entry.path();
        let file_type = entry.file_type().expect("readable file type");
        if file_type.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            scan_dir(root, &path, extension, is_ts, found);
        } else if file_type.is_file()
            && path
                .extension()
                .map(|ext| ext == extension)
                .unwrap_or(false)
        {
            let rel = path
                .strip_prefix(root)
                .expect("scanned path under workspace root")
                .to_string_lossy()
                .replace('\\', "/");
            scan_file(&path, &rel, is_ts, found);
        }
    }
}

fn scan_workspace() -> Vec<ScannedConst> {
    let root = workspace_root();
    let mut found = Vec::new();

    // crates/*/src/**/*.rs
    let crates_dir = root.join("crates");
    let mut crate_names: Vec<_> = fs::read_dir(&crates_dir)
        .expect("crates directory exists")
        .map(|entry| entry.expect("readable crate entry").path())
        .collect();
    crate_names.sort();
    for crate_path in crate_names {
        let src = crate_path.join("src");
        if src.is_dir() {
            scan_dir(&root, &src, "rs", false, &mut found);
        }
    }

    // packages/core/src/**/*.ts
    let core_src = root.join("packages/core/src");
    if core_src.is_dir() {
        scan_dir(&root, &core_src, "ts", true, &mut found);
    }

    // packages/build-tools/bridge-src/**/*.ts
    let bridge_src = root.join("packages/build-tools/bridge-src");
    if bridge_src.is_dir() {
        scan_dir(&root, &bridge_src, "ts", true, &mut found);
    }

    found.sort();
    found.dedup();
    found
}

#[derive(Debug, Clone)]
struct InventoryEntry {
    name: String,
    path: String,
    class: String,
    wired: Option<String>,
}

fn load_inventory() -> Vec<InventoryEntry> {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/limits-inventory.json");
    let raw = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read limits inventory {path:?}: {error}"));
    let value: Value = serde_json::from_str(&raw).expect("inventory is valid JSON");
    let array = value.as_array().expect("inventory is a JSON array");
    array
        .iter()
        .map(|entry| {
            let name = entry["name"].as_str().expect("entry has name").to_string();
            let path = entry["path"].as_str().expect("entry has path").to_string();
            let class = entry["class"]
                .as_str()
                .expect("entry has class")
                .to_string();
            assert!(
                matches!(class.as_str(), "policy" | "policy-deferred" | "invariant"),
                "inventory entry {name} ({path}) has invalid class {class}"
            );
            let wired = entry
                .get("wired")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            InventoryEntry {
                name,
                path,
                class,
                wired,
            }
        })
        .collect()
}

#[test]
fn limit_constants_are_classified() {
    let scanned = scan_workspace();
    let inventory = load_inventory();

    let mut failures: Vec<String> = Vec::new();

    // Duplicate (name, path) inventory entries are rejected.
    let mut inventory_keys: BTreeSet<(String, String)> = BTreeSet::new();
    for entry in &inventory {
        let key = (entry.name.clone(), entry.path.clone());
        if !inventory_keys.insert(key.clone()) {
            failures.push(format!(
                "duplicate inventory entry for {} in {}",
                entry.name, entry.path
            ));
        }
    }

    let scanned_keys: BTreeSet<(String, String)> = scanned
        .iter()
        .map(|c| (c.name.clone(), c.path.clone()))
        .collect();

    // Every scanned constant must have an inventory entry.
    for c in &scanned {
        let key = (c.name.clone(), c.path.clone());
        if !inventory_keys.contains(&key) {
            failures.push(format!(
                "unclassified limit constant {} in {}: wire it through a typed configuration field and mark it \
                 \"policy\", or add an \"invariant\"/\"policy-deferred\" entry to \
                 crates/native-sidecar/tests/fixtures/limits-inventory.json with a one-line rationale",
                c.name, c.path
            ));
        }
    }

    // Every inventory entry must still exist in the scanned source (no stale entries).
    for entry in &inventory {
        let key = (entry.name.clone(), entry.path.clone());
        if !scanned_keys.contains(&key) {
            failures.push(format!(
                "stale inventory entry {} in {}: the constant no longer exists in source; \
                 remove or update the entry in limits-inventory.json (renames must update both)",
                entry.name, entry.path
            ));
        }
    }

    // Every policy entry names the typed VM- or process-scoped configuration field it is wired
    // through. Process-wide reactor and transport limits intentionally live in RuntimeConfig,
    // rather than being duplicated into every VM's VmLimits.
    for entry in &inventory {
        if entry.class == "policy" {
            let wired_ok = entry
                .wired
                .as_deref()
                .map(|w| !w.is_empty())
                .unwrap_or(false);
            if !wired_ok {
                failures.push(format!(
                    "policy inventory entry {} in {} must set a non-empty \"wired\" field naming \
                     the config field it flows from",
                    entry.name, entry.path
                ));
            }
        }
    }

    if !failures.is_empty() {
        panic!(
            "limits inventory audit failed with {} issue(s):\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}

#[test]
fn match_rule_unit_assertions() {
    // Qualifying names.
    assert!(name_qualifies("MAX_BINDING_SCHEMA_BYTES"));
    assert!(name_qualifies("VM_FETCH_BUFFER_LIMIT_BYTES"));
    assert!(name_qualifies("SESSION_OUTPUT_CHANNEL_CAPACITY"));
    assert!(name_qualifies("ACP_SESSION_EVENT_RETENTION_LIMIT"));
    assert!(name_qualifies("DEFAULT_COMPLETED_RESPONSE_CAP"));
    assert!(name_qualifies("DEFAULT_TIMEOUT_MS"));
    assert!(name_qualifies("DEFAULT_MAX_PREAD_BYTES"));
    assert!(name_qualifies("MAX_MODULE_RESOLVE_CACHE_ENTRIES"));

    // Non-qualifying names.
    assert!(!name_qualifies("PROTOCOL_VERSION"));
    assert!(!name_qualifies("EXECUTION_DRIVER_NAME"));
    assert!(!name_qualifies("DEFAULT_VIRTUAL_CPU_COUNT"));
    // Exclusions.
    assert!(!name_qualifies("ERR_SESSION_DEFERRED_COMMAND_ERROR_CODE"));

    // Declaration extraction.
    assert_eq!(
        rust_const_name("pub(crate) const MAX_BINDING_TIMEOUT_MS: u64 = 300_000;"),
        Some("MAX_BINDING_TIMEOUT_MS")
    );
    assert_eq!(
        rust_const_name("    const MAX_FRAME_SIZE: usize = 64 * 1024 * 1024;"),
        Some("MAX_FRAME_SIZE")
    );
    assert_eq!(
        rust_const_name("pub const DEFAULT_MAX_PROCESSES: usize = 256;"),
        Some("DEFAULT_MAX_PROCESSES")
    );
    assert_eq!(
        rust_const_name("pub(in crate::execution) const UDP_MAX_DATAGRAM_BYTES: usize = 65_536;"),
        Some("UDP_MAX_DATAGRAM_BYTES")
    );
    // Lowercase const is not a screaming-snake limit constant.
    assert_eq!(rust_const_name("const max_value: usize = 1;"), None);
    // A function, not a const.
    assert_eq!(rust_const_name("fn parse_resource_limits() {}"), None);

    assert_eq!(
        ts_const_name("export const ACP_SESSION_EVENT_RETENTION_LIMIT = 1024;"),
        Some("ACP_SESSION_EVENT_RETENTION_LIMIT")
    );
    assert_eq!(
        ts_const_name("const MAX_SYMLINK_DEPTH = 40;"),
        Some("MAX_SYMLINK_DEPTH")
    );
    // camelCase identifiers are not constants for this rule.
    assert_eq!(ts_const_name("const maxRetries = 3;"), None);
}
