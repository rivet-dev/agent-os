use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// VM-wide default engine for standalone WebAssembly process images.
///
/// This does not affect JavaScript's `WebAssembly.*` APIs, which always run in
/// the owning V8 isolate. Individual process launches may override this value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "kebab-case")]
#[ts(export, export_to = "../../../packages/runtime-core/src/generated/")]
pub enum StandaloneWasmBackend {
    #[default]
    V8,
    Wasmtime,
    WasmtimeThreads,
}

/// Canonical Rust-side VM config. Unknown fields must stay rejected here and in
/// the TS preflight schema at
/// `packages/core/src/node-runtime-options-schema.ts`; update both when a
/// public `NodeRuntime.create(...)` option changes the generated VM config.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "../../../packages/runtime-core/src/generated/")]
#[derive(Default)]
pub struct CreateVmConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[ts(type = "Record<string, string>")]
    pub env: BTreeMap<String, String>,
    #[serde(
        default,
        rename = "wasmBackend",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional)]
    pub wasm_backend: Option<StandaloneWasmBackend>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub database: Option<VmSqliteDescriptor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub user: Option<VmUserConfig>,
    #[serde(default, rename = "rootFilesystem")]
    pub root_filesystem: RootFilesystemConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub permissions: Option<PermissionsPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub limits: Option<VmLimitsConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub dns: Option<VmDnsConfig>,
    #[serde(
        default,
        rename = "nativeRoot",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional)]
    pub native_root: Option<NativeRootFilesystemConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub listen: Option<VmListenPolicyConfig>,
    #[serde(
        default,
        rename = "loopbackExemptPorts",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub loopback_exempt_ports: Vec<u16>,
    #[serde(default, rename = "jsRuntime", skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub js_runtime: Option<JsRuntimeConfig>,
    #[serde(
        default,
        rename = "bootstrapCommands",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional)]
    pub bootstrap_commands: Option<Vec<String>>,
}

impl CreateVmConfig {
    pub fn validate(&self, max_frame_bytes: usize) -> Result<(), VmConfigError> {
        if let Some(cwd) = self.cwd.as_deref() {
            validate_guest_path("cwd", cwd)?;
        }
        if let Some(database) = &self.database {
            database.validate()?;
        }
        if let Some(user) = &self.user {
            user.validate()?;
        }
        self.root_filesystem.validate()?;
        if let Some(native_root) = &self.native_root {
            native_root.validate()?;
        }
        if self.native_root.is_some() && !self.root_filesystem.bootstrap_entries.is_empty() {
            return Err(VmConfigError::new(
                "nativeRoot does not support rootFilesystem.bootstrapEntries",
            ));
        }
        if let Some(dns) = &self.dns {
            dns.validate()?;
        }
        if let Some(listen) = &self.listen {
            listen.validate()?;
        }
        if let Some(limits) = &self.limits {
            limits.validate(max_frame_bytes)?;
        }
        if let Some(js_runtime) = &self.js_runtime {
            js_runtime.validate()?;
        }
        if let Some(bootstrap_commands) = &self.bootstrap_commands {
            validate_command_names("bootstrapCommands", bootstrap_commands)?;
        }
        Ok(())
    }
}

/// Transport used by the VM-scoped SQLite substrate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
#[ts(tag = "type", rename_all = "snake_case")]
#[ts(export, export_to = "../../../packages/runtime-core/src/generated/")]
pub enum VmSqliteDescriptor {
    /// Rivet actor SQLite reached through the actor's local runtime socket.
    ActorUds { path: String },
    /// A SQLite database file owned by the native sidecar host.
    SqliteFile { path: String },
}

impl VmSqliteDescriptor {
    fn validate(&self) -> Result<(), VmConfigError> {
        match self {
            Self::ActorUds { path } => {
                validate_absolute_host_path("database.path", path)?;
            }
            Self::SqliteFile { path } => validate_absolute_host_path("database.path", path)?,
        }
        Ok(())
    }
}

fn validate_absolute_host_path(field: &str, path: &str) -> Result<(), VmConfigError> {
    if path.is_empty() || !path.starts_with('/') || path.as_bytes().contains(&0) {
        return Err(VmConfigError::new(format!(
            "{field} must be a non-empty absolute path without NUL bytes"
        )));
    }
    Ok(())
}

/// Initial Linux-style credentials and account record for processes in a VM.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "../../../packages/runtime-core/src/generated/")]
pub struct VmUserConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub uid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub gid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub euid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub egid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub homedir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub shell: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub gecos: Option<String>,
    #[serde(default, rename = "groupName", skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub group_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    /// Initial supplementary process credentials. An explicit group record is
    /// authoritative and is not given extra members from this list.
    pub supplementary_gids: Option<Vec<u32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub accounts: Option<Vec<VmUserAccountConfig>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub groups: Option<Vec<VmGroupConfig>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "../../../packages/runtime-core/src/generated/")]
pub struct VmUserAccountConfig {
    pub uid: u32,
    pub gid: u32,
    pub username: String,
    pub homedir: String,
    pub shell: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub gecos: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    /// Initial process credentials only. These gids do not add the account to
    /// an explicit `/etc/group` record's member list.
    pub supplementary_gids: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "../../../packages/runtime-core/src/generated/")]
pub struct VmGroupConfig {
    pub gid: u32,
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    /// Authoritative `/etc/group` membership. Process supplementary gids are
    /// intentionally not merged into this list.
    pub members: Vec<String>,
}

// The libc account ABI uses a 4096-byte text buffer and reserves one byte for
// the terminating NUL. Keep configuration-derived records representable by
// every executor adapter before they reach the kernel account database.
const MAX_ACCOUNT_RECORD_BYTES: usize = 4095;
const MAX_GROUP_MEMBERS: usize = 256;

impl VmUserConfig {
    fn validate(&self) -> Result<(), VmConfigError> {
        const MAX_SUPPLEMENTARY_GIDS: usize = 64;
        const MAX_ACCOUNTS: usize = 64;
        const MAX_GROUPS: usize = 128;
        if self
            .supplementary_gids
            .as_ref()
            .is_some_and(|groups| groups.len() > MAX_SUPPLEMENTARY_GIDS)
        {
            return Err(VmConfigError::new(format!(
                "user.supplementaryGids exceeds limit of {MAX_SUPPLEMENTARY_GIDS}"
            )));
        }
        if let Some(username) = self.username.as_deref() {
            validate_account_name("user.username", username)?;
        }
        if let Some(group_name) = self.group_name.as_deref() {
            validate_account_name("user.groupName", group_name)?;
        }
        if let Some(gecos) = self.gecos.as_deref() {
            validate_account_record_field("user.gecos", gecos)?;
        }
        let accounts = self.accounts.as_deref().unwrap_or_default();
        let groups = self.groups.as_deref().unwrap_or_default();
        if accounts.len() > MAX_ACCOUNTS {
            return Err(VmConfigError::new(format!(
                "user.accounts exceeds limit of {MAX_ACCOUNTS}"
            )));
        }
        if groups.len() > MAX_GROUPS {
            return Err(VmConfigError::new(format!(
                "user.groups exceeds limit of {MAX_GROUPS}"
            )));
        }
        let mut account_uids = std::collections::BTreeSet::new();
        let mut account_names = std::collections::BTreeSet::new();
        for account in accounts {
            validate_account_name("user.accounts[].username", &account.username)?;
            validate_account_path("user.accounts[].homedir", &account.homedir)?;
            validate_account_path("user.accounts[].shell", &account.shell)?;
            if let Some(gecos) = account.gecos.as_deref() {
                validate_account_record_field("user.accounts[].gecos", gecos)?;
            }
            if account.supplementary_gids.len() > MAX_SUPPLEMENTARY_GIDS {
                return Err(VmConfigError::new(format!(
                    "user.accounts[].supplementaryGids exceeds limit of {MAX_SUPPLEMENTARY_GIDS}"
                )));
            }
            if !account_uids.insert(account.uid) {
                return Err(VmConfigError::new(format!(
                    "duplicate user account uid {}",
                    account.uid
                )));
            }
            if !account_names.insert(account.username.as_str()) {
                return Err(VmConfigError::new(format!(
                    "duplicate user account name {}",
                    account.username
                )));
            }
            validate_passwd_record(
                "user.accounts[]",
                &account.username,
                account.uid,
                account.gid,
                account.gecos.as_deref().unwrap_or_default(),
                &account.homedir,
                &account.shell,
            )?;
        }
        let mut group_gids = std::collections::BTreeSet::new();
        let mut group_names = std::collections::BTreeSet::new();
        for group in groups {
            validate_account_name("user.groups[].name", &group.name)?;
            if group.members.len() > MAX_GROUP_MEMBERS {
                return Err(VmConfigError::new(format!(
                    "user.groups[].members exceeds limit of {MAX_GROUP_MEMBERS}"
                )));
            }
            for member in &group.members {
                validate_account_name("user.groups[].members[]", member)?;
            }
            if !group_gids.insert(group.gid) {
                return Err(VmConfigError::new(format!(
                    "duplicate user group gid {}",
                    group.gid
                )));
            }
            if !group_names.insert(group.name.as_str()) {
                return Err(VmConfigError::new(format!(
                    "duplicate user group name {}",
                    group.name
                )));
            }
            validate_group_record("user.groups[]", &group.name, group.gid, &group.members)?;
        }

        let username = self.username.as_deref().unwrap_or("agentos");
        let homedir = self.homedir.as_deref().unwrap_or("/home/agentos");
        let shell = self.shell.as_deref().unwrap_or("/bin/sh");
        let gecos = self.gecos.as_deref().unwrap_or_default();
        validate_account_name("user.username", username)?;
        validate_account_path("user.homedir", homedir)?;
        validate_account_path("user.shell", shell)?;
        validate_passwd_record(
            "user",
            username,
            self.uid.unwrap_or(1000),
            self.gid.unwrap_or(1000),
            gecos,
            homedir,
            shell,
        )?;
        validate_materialized_groups(self, accounts, groups, username)?;
        Ok(())
    }
}

fn validate_account_name(label: &str, value: &str) -> Result<(), VmConfigError> {
    if value.is_empty()
        || value.contains([':', ',', '\n', '\r', '\0'])
        || value.chars().any(char::is_whitespace)
    {
        return Err(VmConfigError::new(format!("{label} is invalid")));
    }
    validate_account_text_bound(label, value)
}

fn validate_account_record_field(label: &str, value: &str) -> Result<(), VmConfigError> {
    if value.contains([':', '\n', '\r', '\0']) {
        return Err(VmConfigError::new(format!("{label} is invalid")));
    }
    validate_account_text_bound(label, value)
}

fn validate_account_path(label: &str, value: &str) -> Result<(), VmConfigError> {
    validate_guest_path(label, value)?;
    validate_account_record_field(label, value)
}

fn validate_account_text_bound(label: &str, value: &str) -> Result<(), VmConfigError> {
    if value.len() > MAX_ACCOUNT_RECORD_BYTES {
        return Err(VmConfigError::new(format!(
            "{label} exceeds limit of {MAX_ACCOUNT_RECORD_BYTES} UTF-8 bytes"
        )));
    }
    Ok(())
}

fn validate_passwd_record(
    label: &str,
    username: &str,
    uid: u32,
    gid: u32,
    gecos: &str,
    homedir: &str,
    shell: &str,
) -> Result<(), VmConfigError> {
    let field_bytes = [
        username.len(),
        uid.to_string().len(),
        gid.to_string().len(),
        gecos.len(),
        homedir.len(),
        shell.len(),
    ];
    validate_account_record_size(label, field_bytes.into_iter(), 7)
}

fn validate_group_record(
    label: &str,
    name: &str,
    gid: u32,
    members: &[String],
) -> Result<(), VmConfigError> {
    if members.len() > MAX_GROUP_MEMBERS {
        return Err(VmConfigError::new(format!(
            "{label}.members exceeds limit of {MAX_GROUP_MEMBERS}"
        )));
    }
    let member_separators = members.len().saturating_sub(1);
    validate_account_record_size(
        label,
        std::iter::once(name.len())
            .chain(std::iter::once(gid.to_string().len()))
            .chain(members.iter().map(|member| member.len())),
        4 + member_separators,
    )
}

fn validate_account_record_size(
    label: &str,
    mut field_bytes: impl Iterator<Item = usize>,
    syntax_bytes: usize,
) -> Result<(), VmConfigError> {
    let record_bytes = field_bytes.try_fold(syntax_bytes, usize::checked_add);
    if record_bytes.is_none_or(|bytes| bytes > MAX_ACCOUNT_RECORD_BYTES) {
        return Err(VmConfigError::new(format!(
            "{label} rendered account record exceeds {MAX_ACCOUNT_RECORD_BYTES} bytes (the 4096-byte ABI buffer includes its terminating NUL)"
        )));
    }
    Ok(())
}

fn validate_materialized_groups(
    config: &VmUserConfig,
    accounts: &[VmUserAccountConfig],
    groups: &[VmGroupConfig],
    primary_username: &str,
) -> Result<(), VmConfigError> {
    let primary_gid = config.gid.unwrap_or(1000);
    let primary_group_name = config.group_name.as_deref().unwrap_or(primary_username);
    let mut materialized = groups
        .iter()
        .map(|group| (group.gid, (group.name.clone(), group.members.clone())))
        .collect::<BTreeMap<_, _>>();
    materialized.entry(primary_gid).or_insert_with(|| {
        (
            primary_group_name.to_owned(),
            vec![primary_username.to_owned()],
        )
    });
    let authoritative_group_gids = materialized
        .keys()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();

    let mut effective_accounts = accounts
        .iter()
        .map(|account| {
            (
                account.uid,
                (
                    account.username.as_str(),
                    account.gid,
                    account.supplementary_gids.as_slice(),
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let primary_supplementary_gids = config.supplementary_gids.as_deref().unwrap_or_default();
    effective_accounts.insert(
        config.uid.unwrap_or(1000),
        (primary_username, primary_gid, primary_supplementary_gids),
    );

    for (_, (username, account_gid, supplementary_gids)) in effective_accounts {
        for group_gid in std::iter::once(account_gid).chain(supplementary_gids.iter().copied()) {
            // Credentials and the account database are separate Linux state:
            // an explicit group record is authoritative and is never mutated
            // merely because a process carries its gid.
            if authoritative_group_gids.contains(&group_gid) {
                continue;
            }
            let (_, members) = materialized
                .entry(group_gid)
                .or_insert_with(|| (format!("group{group_gid}"), Vec::new()));
            if !members.iter().any(|member| member == username) {
                members.push(username.to_owned());
            }
        }
    }

    let mut gids_by_name = BTreeMap::<&str, u32>::new();
    for (gid, (name, members)) in &materialized {
        if let Some(previous_gid) = gids_by_name.insert(name, *gid) {
            return Err(VmConfigError::new(format!(
                "materialized user group name {name:?} maps to both gid {previous_gid} and gid {gid}; synthesized group names must not collide"
            )));
        }
        validate_group_record("materialized user group", name, *gid, members)?;
    }
    Ok(())
}

/// Guest JavaScript host-environment configuration.
///
/// Selects which globals/builtins/module-resolution surface guest JS sees,
/// modeled on esbuild's `platform`. Omitting this preserves full Node.js
/// emulation (`platform = node`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "../../../packages/runtime-core/src/generated/")]
pub struct JsRuntimeConfig {
    /// Which host environment to emulate for guest JS. Default `node`.
    #[serde(default)]
    pub platform: JsRuntimePlatform,
    /// How bare import specifiers resolve. Independent of `platform`.
    /// Default `node`.
    #[serde(default, rename = "moduleResolution")]
    pub module_resolution: JsModuleResolution,
    /// Node builtin-module allow-list. Only valid when `platform = node`.
    /// `None` => engine default allow-list. `Some([])` => deny all builtins.
    /// `Some([..])` => exactly those.
    #[serde(
        default,
        rename = "allowedBuiltins",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional)]
    pub allowed_builtins: Option<Vec<String>>,
    /// Opt in to a high-resolution monotonic guest clock. Default false keeps
    /// the security-oriented 1ms timer resolution.
    #[serde(
        default,
        rename = "highResolutionTime",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional)]
    pub high_resolution_time: Option<bool>,
}

impl JsRuntimeConfig {
    fn validate(&self) -> Result<(), VmConfigError> {
        if let Some(allowed) = &self.allowed_builtins {
            if self.platform != JsRuntimePlatform::Node {
                return Err(VmConfigError::new(
                    "jsRuntime.allowedBuiltins is only valid when jsRuntime.platform is \"node\"",
                ));
            }
            for name in allowed {
                if !is_known_node_builtin(name) {
                    return Err(VmConfigError::new(format!(
                        "jsRuntime.allowedBuiltins contains unknown builtin {name:?}"
                    )));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../packages/runtime-core/src/generated/")]
#[derive(Default)]
pub enum JsRuntimePlatform {
    /// Full Node.js host surface (process/Buffer/require, `node:*`, npm
    /// resolution, virtual Node identity). Default.
    #[default]
    Node,
    /// Web-platform globals (fetch/URL/WebCrypto/...), no Node surface.
    Browser,
    /// Universal primitives only (console, timers, queueMicrotask) — no web
    /// platform, no Node surface.
    Neutral,
    /// Language-only: ECMAScript spec globals + WebAssembly. Nothing host-provided.
    Bare,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../packages/runtime-core/src/generated/")]
#[derive(Default)]
pub enum JsModuleResolution {
    /// node_modules ancestor-walk + exports/imports/conditions + realpath. Default.
    #[default]
    Node,
    /// Relative/absolute ESM from the VFS only; bare specifiers do not resolve.
    Relative,
    /// No resolution: any import/require (even relative) fails.
    None,
}

/// Canonical set of recognized Node builtin module names (without the `node:`
/// prefix), kept in sync with `normalize_builtin_specifier` in
/// `crates/v8-runtime/src/javascript.rs`. Used to validate
/// `jsRuntime.allowedBuiltins` entries.
const KNOWN_NODE_BUILTINS: &[&str] = &[
    "assert",
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

fn is_known_node_builtin(name: &str) -> bool {
    let bare = name.strip_prefix("node:").unwrap_or(name);
    KNOWN_NODE_BUILTINS.contains(&bare)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "../../../packages/runtime-core/src/generated/")]
pub struct RootFilesystemConfig {
    #[serde(default)]
    pub mode: RootFilesystemMode,
    #[serde(default, rename = "disableDefaultBaseLayer")]
    pub disable_default_base_layer: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lowers: Vec<RootFilesystemLowerDescriptor>,
    #[serde(
        default,
        rename = "bootstrapEntries",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub bootstrap_entries: Vec<RootFilesystemEntry>,
}

impl Default for RootFilesystemConfig {
    fn default() -> Self {
        Self {
            mode: RootFilesystemMode::Ephemeral,
            disable_default_base_layer: false,
            lowers: Vec::new(),
            bootstrap_entries: Vec::new(),
        }
    }
}

impl RootFilesystemConfig {
    fn validate(&self) -> Result<(), VmConfigError> {
        for lower in &self.lowers {
            if let RootFilesystemLowerDescriptor::Snapshot { entries } = lower {
                for entry in entries {
                    entry.validate()?;
                }
            }
        }
        for entry in &self.bootstrap_entries {
            entry.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export, export_to = "../../../packages/runtime-core/src/generated/")]
#[derive(Default)]
pub enum RootFilesystemMode {
    #[default]
    Ephemeral,
    ReadOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export, export_to = "../../../packages/runtime-core/src/generated/")]
pub enum RootFilesystemLowerDescriptor {
    Snapshot {
        #[serde(default)]
        entries: Vec<RootFilesystemEntry>,
    },
    BundledBaseFilesystem,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "../../../packages/runtime-core/src/generated/")]
pub struct RootFilesystemEntry {
    pub path: String,
    pub kind: RootFilesystemEntryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mode: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub uid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub gid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub encoding: Option<RootFilesystemEntryEncoding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub target: Option<String>,
    #[serde(default)]
    pub executable: bool,
}

impl RootFilesystemEntry {
    fn validate(&self) -> Result<(), VmConfigError> {
        validate_guest_path("root filesystem entry path", &self.path)?;
        match self.kind {
            RootFilesystemEntryKind::File => {
                if self.target.is_some() {
                    return Err(VmConfigError::new(format!(
                        "file entry {} must not include target",
                        self.path
                    )));
                }
            }
            RootFilesystemEntryKind::Directory => {
                if self.content.is_some() || self.encoding.is_some() || self.target.is_some() {
                    return Err(VmConfigError::new(format!(
                        "directory entry {} must not include content, encoding, or target",
                        self.path
                    )));
                }
            }
            RootFilesystemEntryKind::Symlink => {
                if self.target.as_deref().unwrap_or("").is_empty() {
                    return Err(VmConfigError::new(format!(
                        "symlink entry {} requires target",
                        self.path
                    )));
                }
                if self.content.is_some() || self.encoding.is_some() {
                    return Err(VmConfigError::new(format!(
                        "symlink entry {} must not include content or encoding",
                        self.path
                    )));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../packages/runtime-core/src/generated/")]
pub enum RootFilesystemEntryKind {
    File,
    Directory,
    Symlink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../packages/runtime-core/src/generated/")]
pub enum RootFilesystemEntryEncoding {
    Utf8,
    Base64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "../../../packages/runtime-core/src/generated/")]
pub struct NativeRootFilesystemConfig {
    pub plugin: MountPluginDescriptor,
    #[serde(default, rename = "readOnly")]
    pub read_only: bool,
}

impl NativeRootFilesystemConfig {
    fn validate(&self) -> Result<(), VmConfigError> {
        if self.plugin.id.trim().is_empty() {
            return Err(VmConfigError::new("nativeRoot.plugin.id is required"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "../../../packages/runtime-core/src/generated/")]
pub struct MountPluginDescriptor {
    pub id: String,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    #[ts(type = "import(\"@rivet-dev/agentos-runtime-core/descriptors\").MountConfigJsonValue")]
    pub config: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../packages/runtime-core/src/generated/")]
pub enum PermissionMode {
    Allow,
    Ask,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(untagged)]
#[ts(export, export_to = "../../../packages/runtime-core/src/generated/")]
pub enum FsPermissionScope {
    Mode(PermissionMode),
    Rules(FsPermissionRuleSet),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(untagged)]
#[ts(export, export_to = "../../../packages/runtime-core/src/generated/")]
pub enum PatternPermissionScope {
    Mode(PermissionMode),
    Rules(PatternPermissionRuleSet),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "../../../packages/runtime-core/src/generated/")]
pub struct FsPermissionRuleSet {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub default: Option<PermissionMode>,
    #[serde(default)]
    pub rules: Vec<FsPermissionRule>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "../../../packages/runtime-core/src/generated/")]
pub struct PatternPermissionRuleSet {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub default: Option<PermissionMode>,
    #[serde(default)]
    pub rules: Vec<PatternPermissionRule>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "../../../packages/runtime-core/src/generated/")]
pub struct FsPermissionRule {
    pub mode: PermissionMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "../../../packages/runtime-core/src/generated/")]
pub struct PatternPermissionRule {
    pub mode: PermissionMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patterns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "../../../packages/runtime-core/src/generated/")]
pub struct PermissionsPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fs: Option<FsPermissionScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub network: Option<PatternPermissionScope>,
    #[serde(
        default,
        rename = "childProcess",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional)]
    pub child_process: Option<PatternPermissionScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub process: Option<PatternPermissionScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub env: Option<PatternPermissionScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub binding: Option<PatternPermissionScope>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "../../../packages/runtime-core/src/generated/")]
pub struct VmLimitsConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reactor: Option<ReactorLimitsConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub resources: Option<ResourceLimitsConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub http: Option<HttpLimitsConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub udp: Option<UdpLimitsConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tls: Option<TlsLimitsConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub http2: Option<Http2LimitsConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub bindings: Option<BindingLimitsConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plugins: Option<PluginLimitsConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub acp: Option<AcpLimitsConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub sqlite: Option<SqliteLimitsConfig>,
    #[serde(default, rename = "jsRuntime", skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub js_runtime: Option<JsRuntimeLimitsConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub python: Option<PythonLimitsConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub wasm: Option<WasmLimitsConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub execution: Option<ExecutionLimitsConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub process: Option<ProcessLimitsConfig>,
}

impl VmLimitsConfig {
    fn validate(&self, max_frame_bytes: usize) -> Result<(), VmConfigError> {
        if let Some(reactor) = &self.reactor {
            validate_nonzero_options([
                ("limits.reactor.maxCapabilities", reactor.max_capabilities),
                ("limits.reactor.maxReadyHandles", reactor.max_ready_handles),
                ("limits.reactor.maxTasks", reactor.max_tasks),
                ("limits.reactor.workQuantum", reactor.work_quantum),
                ("limits.reactor.byteQuantum", reactor.byte_quantum),
                (
                    "limits.reactor.maxHandleCommands",
                    reactor.max_handle_commands,
                ),
                (
                    "limits.reactor.maxHandleCommandBytes",
                    reactor.max_handle_command_bytes,
                ),
                ("limits.reactor.maxBridgeCalls", reactor.max_bridge_calls),
                (
                    "limits.reactor.maxBridgeRequestBytes",
                    reactor.max_bridge_request_bytes,
                ),
                (
                    "limits.reactor.maxBridgeResponseBytes",
                    reactor.max_bridge_response_bytes,
                ),
                (
                    "limits.reactor.maxAsyncCompletions",
                    reactor.max_async_completions,
                ),
                (
                    "limits.reactor.maxAsyncCompletionBytes",
                    reactor.max_async_completion_bytes,
                ),
                ("limits.reactor.maxBlockingJobs", reactor.max_blocking_jobs),
                (
                    "limits.reactor.maxBlockingBytes",
                    reactor.max_blocking_bytes,
                ),
                (
                    "limits.reactor.perHandleOperationQuantum",
                    reactor.per_handle_operation_quantum,
                ),
                ("limits.reactor.acceptQuantum", reactor.accept_quantum),
                ("limits.reactor.datagramQuantum", reactor.datagram_quantum),
                (
                    "limits.reactor.completionQuantum",
                    reactor.completion_quantum,
                ),
                ("limits.reactor.signalQuantum", reactor.signal_quantum),
                (
                    "limits.reactor.shutdownDeadlineMs",
                    reactor.shutdown_deadline_ms,
                ),
                (
                    "limits.reactor.operationDeadlineMs",
                    reactor.operation_deadline_ms,
                ),
            ])?;
            validate_optional_parent(
                "limits.reactor.maxCapabilities",
                reactor.max_capabilities,
                "limits.reactor.maxReadyHandles",
                reactor.max_ready_handles,
            )?;
            validate_optional_parent(
                "limits.reactor.perHandleOperationQuantum",
                reactor.per_handle_operation_quantum,
                "limits.reactor.maxHandleCommands",
                reactor.max_handle_commands,
            )?;
            validate_optional_parent(
                "limits.reactor.acceptQuantum",
                reactor.accept_quantum,
                "limits.reactor.maxCapabilities",
                reactor.max_capabilities,
            )?;
            validate_optional_parent(
                "limits.reactor.completionQuantum",
                reactor.completion_quantum,
                "limits.reactor.maxAsyncCompletions",
                reactor.max_async_completions,
            )?;
            if let Some(max_bridge_request_bytes) = reactor.max_bridge_request_bytes {
                if max_bridge_request_bytes > max_frame_bytes as u64 {
                    return Err(VmConfigError::new(format!(
                        "limits.reactor.maxBridgeRequestBytes ({max_bridge_request_bytes}) must \
                         be <= the sidecar wire frame cap ({max_frame_bytes})"
                    )));
                }
            }
            if let Some(max_bridge_response_bytes) = reactor.max_bridge_response_bytes {
                if max_bridge_response_bytes > max_frame_bytes as u64 {
                    return Err(VmConfigError::new(format!(
                        "limits.reactor.maxBridgeResponseBytes ({max_bridge_response_bytes}) must \
                         be <= the sidecar wire frame cap ({max_frame_bytes})"
                    )));
                }
            }
        }
        if let Some(http) = &self.http {
            if let Some(max_fetch_response_bytes) = http.max_fetch_response_bytes {
                if max_fetch_response_bytes == 0 {
                    return Err(VmConfigError::new(
                        "limits.http.maxFetchResponseBytes must be greater than zero",
                    ));
                }
                if max_fetch_response_bytes as usize > max_frame_bytes {
                    return Err(VmConfigError::new(format!(
                        "limits.http.maxFetchResponseBytes ({max_fetch_response_bytes}) must be <= the sidecar wire frame cap ({max_frame_bytes})"
                    )));
                }
            }
        }
        if let Some(udp) = &self.udp {
            validate_nonzero_options([
                (
                    "limits.udp.maxBufferedDatagrams",
                    udp.max_buffered_datagrams,
                ),
                ("limits.udp.maxBufferedBytes", udp.max_buffered_bytes),
            ])?;
        }
        if let Some(tls) = &self.tls {
            validate_nonzero_options([("limits.tls.maxBufferedBytes", tls.max_buffered_bytes)])?;
        }
        if let Some(http2) = &self.http2 {
            validate_nonzero_options([
                ("limits.http2.maxConnections", http2.max_connections),
                ("limits.http2.maxStreams", http2.max_streams),
                (
                    "limits.http2.maxStreamsPerConnection",
                    http2.max_streams_per_connection,
                ),
                ("limits.http2.maxBufferedBytes", http2.max_buffered_bytes),
                ("limits.http2.maxHeaderBytes", http2.max_header_bytes),
                ("limits.http2.maxDataBytes", http2.max_data_bytes),
                (
                    "limits.http2.maxPendingCommands",
                    http2.max_pending_commands,
                ),
                (
                    "limits.http2.maxPendingCommandBytes",
                    http2.max_pending_command_bytes,
                ),
                ("limits.http2.maxPendingEvents", http2.max_pending_events),
                (
                    "limits.http2.maxPendingEventBytes",
                    http2.max_pending_event_bytes,
                ),
            ])?;
            validate_optional_parent(
                "limits.http2.maxStreamsPerConnection",
                http2.max_streams_per_connection,
                "limits.http2.maxStreams",
                http2.max_streams,
            )?;
            for (path, value) in [
                ("limits.http2.maxHeaderBytes", http2.max_header_bytes),
                ("limits.http2.maxDataBytes", http2.max_data_bytes),
                (
                    "limits.http2.maxPendingCommandBytes",
                    http2.max_pending_command_bytes,
                ),
                (
                    "limits.http2.maxPendingEventBytes",
                    http2.max_pending_event_bytes,
                ),
            ] {
                validate_optional_parent(
                    path,
                    value,
                    "limits.http2.maxBufferedBytes",
                    http2.max_buffered_bytes,
                )?;
            }
        }
        if let Some(resources) = &self.resources {
            let aggregate_socket_bytes = resources.max_socket_buffered_bytes;
            for (path, value) in [
                (
                    "limits.reactor.maxHandleCommandBytes",
                    self.reactor
                        .as_ref()
                        .and_then(|limits| limits.max_handle_command_bytes),
                ),
                (
                    "limits.http.maxFetchResponseBytes",
                    self.http
                        .as_ref()
                        .and_then(|limits| limits.max_fetch_response_bytes),
                ),
                (
                    "limits.udp.maxBufferedBytes",
                    self.udp
                        .as_ref()
                        .and_then(|limits| limits.max_buffered_bytes),
                ),
                (
                    "limits.tls.maxBufferedBytes",
                    self.tls
                        .as_ref()
                        .and_then(|limits| limits.max_buffered_bytes),
                ),
                (
                    "limits.http2.maxBufferedBytes",
                    self.http2
                        .as_ref()
                        .and_then(|limits| limits.max_buffered_bytes),
                ),
            ] {
                validate_optional_parent(
                    path,
                    value,
                    "limits.resources.maxSocketBufferedBytes",
                    aggregate_socket_bytes,
                )?;
            }
            validate_optional_parent(
                "limits.udp.maxBufferedDatagrams",
                self.udp
                    .as_ref()
                    .and_then(|limits| limits.max_buffered_datagrams),
                "limits.resources.maxSocketDatagramQueueLen",
                resources.max_socket_datagram_queue_len,
            )?;
            validate_optional_parent(
                "limits.http2.maxConnections",
                self.http2
                    .as_ref()
                    .and_then(|limits| limits.max_connections),
                "limits.resources.maxConnections",
                resources.max_connections,
            )?;
        }
        if let (Some(reactor), Some(udp)) = (&self.reactor, &self.udp) {
            validate_optional_parent(
                "limits.reactor.datagramQuantum",
                reactor.datagram_quantum,
                "limits.udp.maxBufferedDatagrams",
                udp.max_buffered_datagrams,
            )?;
        }
        if let Some(bindings) = &self.bindings {
            if let (Some(default), Some(max)) = (
                bindings.default_binding_timeout_ms,
                bindings.max_binding_timeout_ms,
            ) {
                if default > max {
                    return Err(VmConfigError::new(
                        "limits.bindings.defaultBindingTimeoutMs must be <= limits.bindings.maxBindingTimeoutMs",
                    ));
                }
            }
        }
        if let Some(js_runtime) = &self.js_runtime {
            validate_nonzero_options([("limits.jsRuntime.maxTimers", js_runtime.max_timers)])?;
        }
        if let Some(execution) = &self.execution {
            validate_nonzero_options([
                (
                    "limits.execution.completedTtlMs",
                    execution.completed_ttl_ms,
                ),
                (
                    "limits.execution.maxCompletedExecutions",
                    execution.max_completed_executions,
                ),
                (
                    "limits.execution.liveExecutionWarningThreshold",
                    execution.live_execution_warning_threshold,
                ),
            ])?;
            const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
            for (path, value) in [
                (
                    "limits.execution.completedTtlMs",
                    execution.completed_ttl_ms,
                ),
                (
                    "limits.execution.maxCompletedExecutions",
                    execution.max_completed_executions,
                ),
                (
                    "limits.execution.liveExecutionWarningThreshold",
                    execution.live_execution_warning_threshold,
                ),
            ] {
                if value.is_some_and(|value| value > MAX_SAFE_INTEGER) {
                    return Err(VmConfigError::new(format!("{path} must be a safe integer")));
                }
            }
        }
        Ok(())
    }
}

fn validate_nonzero_options<const N: usize>(
    values: [(&str, Option<u64>); N],
) -> Result<(), VmConfigError> {
    for (path, value) in values {
        if value == Some(0) {
            return Err(VmConfigError::new(format!(
                "{path} must be greater than zero"
            )));
        }
    }
    Ok(())
}

fn validate_optional_parent(
    child_path: &str,
    child: Option<u64>,
    parent_path: &str,
    parent: Option<u64>,
) -> Result<(), VmConfigError> {
    if let (Some(child), Some(parent)) = (child, parent) {
        if child > parent {
            return Err(VmConfigError::new(format!(
                "{child_path} ({child}) must be <= {parent_path} ({parent})"
            )));
        }
    }
    Ok(())
}

macro_rules! limits_struct {
    ($name:ident { $($field:ident),* $(,)? }) => {
        #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        #[ts(export, export_to = "../../../packages/runtime-core/src/generated/")]
        pub struct $name {
            $(
                #[serde(default, skip_serializing_if = "Option::is_none")]
                #[ts(optional)]
                #[ts(type = "number")]
                pub $field: Option<u64>,
            )*
        }
    };
}

limits_struct!(ResourceLimitsConfig {
    cpu_count,
    max_processes,
    max_open_fds,
    max_pipes,
    max_ptys,
    max_sockets,
    max_connections,
    max_socket_buffered_bytes,
    max_socket_datagram_queue_len,
    max_filesystem_bytes,
    max_inode_count,
    max_blocking_read_ms,
    max_pread_bytes,
    max_fd_write_bytes,
    max_process_argv_bytes,
    max_process_env_bytes,
    max_readdir_entries,
    max_recursive_fs_depth,
    max_recursive_fs_entries,
    max_wasm_memory_bytes,
    max_wasm_stack_bytes,
});

limits_struct!(ReactorLimitsConfig {
    max_capabilities,
    max_ready_handles,
    max_tasks,
    work_quantum,
    byte_quantum,
    max_handle_commands,
    max_handle_command_bytes,
    max_bridge_calls,
    max_bridge_request_bytes,
    max_bridge_response_bytes,
    max_async_completions,
    max_async_completion_bytes,
    max_blocking_jobs,
    max_blocking_bytes,
    per_handle_operation_quantum,
    accept_quantum,
    datagram_quantum,
    completion_quantum,
    signal_quantum,
    shutdown_deadline_ms,
    operation_deadline_ms,
});

limits_struct!(HttpLimitsConfig {
    max_fetch_response_bytes,
});

limits_struct!(UdpLimitsConfig {
    max_buffered_datagrams,
    max_buffered_bytes,
});

limits_struct!(TlsLimitsConfig { max_buffered_bytes });

limits_struct!(Http2LimitsConfig {
    max_connections,
    max_streams,
    max_streams_per_connection,
    max_buffered_bytes,
    max_header_bytes,
    max_data_bytes,
    max_pending_commands,
    max_pending_command_bytes,
    max_pending_events,
    max_pending_event_bytes,
});

limits_struct!(BindingLimitsConfig {
    default_binding_timeout_ms,
    max_binding_timeout_ms,
    max_registered_collections,
    max_registered_bindings_per_vm,
    max_bindings_per_collection,
    max_binding_schema_bytes,
    max_examples_per_binding,
    max_binding_example_input_bytes,
});

limits_struct!(PluginLimitsConfig {
    max_persisted_manifest_bytes,
    max_persisted_manifest_file_bytes,
});

limits_struct!(AcpLimitsConfig {
    max_read_line_bytes,
    stdout_buffer_byte_limit,
    max_completed_message_bytes,
    max_turn_output_bytes,
    max_prompt_bytes,
    max_prompt_blocks,
    max_fallback_continuation_bytes,
    max_session_history_bytes,
    max_session_history_events,
    max_history_page_entries,
    max_session_list_entries,
    max_sessions_per_vm,
    max_prompts_per_session,
    max_prompts_per_vm,
    max_pending_permissions_per_session,
    max_pending_permissions_per_vm,
    max_permission_outcomes_per_session,
    max_permission_outcomes_per_vm,
});

limits_struct!(SqliteLimitsConfig { max_result_bytes });

limits_struct!(JsRuntimeLimitsConfig {
    v8_heap_limit_mb,
    sync_rpc_wait_timeout_ms,
    cpu_time_limit_ms,
    wall_clock_limit_ms,
    import_cache_materialize_timeout_ms,
    captured_output_limit_bytes,
    stdin_buffer_limit_bytes,
    event_payload_limit_bytes,
    max_timers,
    v8_ipc_max_frame_bytes,
});

limits_struct!(PythonLimitsConfig {
    output_buffer_max_bytes,
    execution_timeout_ms,
    max_old_space_mb,
    vfs_rpc_timeout_ms,
});

limits_struct!(WasmLimitsConfig {
    max_module_file_bytes,
    captured_output_limit_bytes,
    sync_read_limit_bytes,
    prewarm_timeout_ms,
    runner_heap_limit_mb,
    active_cpu_time_limit_ms,
    wall_clock_limit_ms,
    deterministic_fuel,
    max_threads,
    max_concurrent_threads,
});

limits_struct!(ExecutionLimitsConfig {
    completed_ttl_ms,
    max_completed_executions,
    live_execution_warning_threshold,
});

limits_struct!(ProcessLimitsConfig {
    max_spawn_file_actions,
    max_spawn_file_action_bytes,
    pending_stdin_bytes,
    pending_event_count,
    pending_event_bytes,
    max_pending_child_sync_count,
    max_pending_child_sync_bytes,
});

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "../../../packages/runtime-core/src/generated/")]
pub struct VmDnsConfig {
    #[serde(default, rename = "nameServers", skip_serializing_if = "Vec::is_empty")]
    pub name_servers: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub overrides: BTreeMap<String, Vec<String>>,
}

impl VmDnsConfig {
    fn validate(&self) -> Result<(), VmConfigError> {
        for entry in &self.name_servers {
            if entry.trim().is_empty() {
                return Err(VmConfigError::new(
                    "dns.nameServers entries must not be empty",
                ));
            }
        }
        for (host, addresses) in &self.overrides {
            if host.trim().is_empty() {
                return Err(VmConfigError::new("dns.overrides keys must not be empty"));
            }
            if addresses.is_empty() {
                return Err(VmConfigError::new(format!(
                    "dns.overrides.{host} must contain at least one address"
                )));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "../../../packages/runtime-core/src/generated/")]
pub struct VmListenPolicyConfig {
    #[serde(default, rename = "portMin", skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub port_min: Option<u16>,
    #[serde(default, rename = "portMax", skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub port_max: Option<u16>,
    #[serde(
        default,
        rename = "allowPrivileged",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional)]
    pub allow_privileged: Option<bool>,
}

impl VmListenPolicyConfig {
    fn validate(&self) -> Result<(), VmConfigError> {
        if self.port_min == Some(0) {
            return Err(VmConfigError::new(
                "listen.portMin must be between 1 and 65535",
            ));
        }
        if self.port_max == Some(0) {
            return Err(VmConfigError::new(
                "listen.portMax must be between 1 and 65535",
            ));
        }
        if let (Some(min), Some(max)) = (self.port_min, self.port_max) {
            if min > max {
                return Err(VmConfigError::new(
                    "listen.portMin must be <= listen.portMax",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmConfigError {
    message: String,
}

impl VmConfigError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for VmConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for VmConfigError {}

fn validate_guest_path(label: &str, path: &str) -> Result<(), VmConfigError> {
    if !path.starts_with('/') {
        return Err(VmConfigError::new(format!("{label} must be absolute")));
    }
    if path.split('/').any(|part| part == "..") {
        return Err(VmConfigError::new(format!("{label} must not contain '..'")));
    }
    Ok(())
}

fn validate_command_names(label: &str, commands: &[String]) -> Result<(), VmConfigError> {
    for command in commands {
        if command.is_empty()
            || command == "."
            || command == ".."
            || command.contains('/')
            || command.contains('\0')
        {
            return Err(VmConfigError::new(format!(
                "{label} contains invalid command name {command:?}"
            )));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_round_trips() {
        let config = CreateVmConfig::default();
        let json = serde_json::to_string(&config).expect("serialize config");
        let decoded: CreateVmConfig = serde_json::from_str(&json).expect("decode config");
        assert_eq!(decoded, config);
    }

    #[test]
    fn standalone_wasm_backend_round_trips_and_defaults_to_v8() {
        let omitted: CreateVmConfig =
            serde_json::from_str(r#"{"env":{},"rootFilesystem":{},"loopbackExemptPorts":[]}"#)
                .expect("decode omitted backend");
        assert_eq!(
            omitted.wasm_backend.unwrap_or_default(),
            StandaloneWasmBackend::V8
        );

        for backend in [
            StandaloneWasmBackend::V8,
            StandaloneWasmBackend::Wasmtime,
            StandaloneWasmBackend::WasmtimeThreads,
        ] {
            let config = CreateVmConfig {
                wasm_backend: Some(backend),
                ..CreateVmConfig::default()
            };
            let json = serde_json::to_string(&config).expect("serialize backend");
            let decoded: CreateVmConfig = serde_json::from_str(&json).expect("decode backend");
            assert_eq!(decoded.wasm_backend, Some(backend));
        }
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let error =
            serde_json::from_str::<CreateVmConfig>(r#"{"rootFilesystem":{},"surprise":true}"#)
                .expect_err("unknown fields should fail");
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn root_user_config_round_trips_and_validates() {
        let config = CreateVmConfig {
            user: Some(VmUserConfig {
                uid: Some(0),
                gid: Some(0),
                username: Some(String::from("root")),
                homedir: Some(String::from("/root")),
                shell: Some(String::from("/bin/sh")),
                supplementary_gids: Some(vec![0, 10]),
                ..VmUserConfig::default()
            }),
            ..CreateVmConfig::default()
        };
        config.validate(usize::MAX).expect("valid root account");
        let json = serde_json::to_string(&config).expect("serialize root user");
        let decoded: CreateVmConfig = serde_json::from_str(&json).expect("decode root user");
        assert_eq!(decoded, config);
    }

    #[test]
    fn user_config_rejects_unbounded_groups_and_invalid_records() {
        let too_many_groups = CreateVmConfig {
            user: Some(VmUserConfig {
                supplementary_gids: Some((0..65).collect()),
                ..VmUserConfig::default()
            }),
            ..CreateVmConfig::default()
        };
        assert!(too_many_groups.validate(usize::MAX).is_err());

        let invalid_name = CreateVmConfig {
            user: Some(VmUserConfig {
                username: Some(String::from("bad:name")),
                ..VmUserConfig::default()
            }),
            ..CreateVmConfig::default()
        };
        assert!(invalid_name.validate(usize::MAX).is_err());
    }

    #[test]
    fn user_config_rejects_materialized_group_name_collisions() {
        let config = CreateVmConfig {
            user: Some(VmUserConfig {
                uid: Some(0),
                gid: Some(0),
                username: Some(String::from("root")),
                supplementary_gids: Some(vec![44]),
                groups: Some(vec![VmGroupConfig {
                    gid: 99,
                    name: String::from("group44"),
                    members: Vec::new(),
                }]),
                ..VmUserConfig::default()
            }),
            ..CreateVmConfig::default()
        };

        let error = config
            .validate(usize::MAX)
            .expect_err("synthesized group name collision must fail");
        assert!(error
            .to_string()
            .contains("synthesized group names must not collide"));
    }

    #[test]
    fn user_config_bounds_rendered_account_records_and_group_members() {
        let valid_maximum = CreateVmConfig {
            user: Some(VmUserConfig {
                uid: Some(0),
                gid: Some(0),
                username: Some(String::from("u")),
                homedir: Some(String::from("/")),
                shell: Some(String::from("/")),
                // `u:x:0:0:<gecos>:/:/` is exactly 4095 bytes.
                gecos: Some("x".repeat(4083)),
                groups: Some(vec![VmGroupConfig {
                    gid: 7,
                    name: String::from("g"),
                    members: vec![String::from("m"); MAX_GROUP_MEMBERS],
                }]),
                ..VmUserConfig::default()
            }),
            ..CreateVmConfig::default()
        };
        valid_maximum
            .validate(usize::MAX)
            .expect("4095-byte record and 256 group members must fit");

        let oversized_passwd = CreateVmConfig {
            user: Some(VmUserConfig {
                uid: Some(0),
                gid: Some(0),
                username: Some(String::from("u")),
                homedir: Some(String::from("/")),
                shell: Some(String::from("/")),
                gecos: Some("x".repeat(4084)),
                ..VmUserConfig::default()
            }),
            ..CreateVmConfig::default()
        };
        assert!(oversized_passwd.validate(usize::MAX).is_err());

        let oversized_group_record = CreateVmConfig {
            user: Some(VmUserConfig {
                groups: Some(vec![VmGroupConfig {
                    gid: 7,
                    name: String::from("g"),
                    members: vec!["a".repeat(2045), "b".repeat(2045)],
                }]),
                ..VmUserConfig::default()
            }),
            ..CreateVmConfig::default()
        };
        assert!(oversized_group_record.validate(usize::MAX).is_err());

        let too_many_members = CreateVmConfig {
            user: Some(VmUserConfig {
                groups: Some(vec![VmGroupConfig {
                    gid: 7,
                    name: String::from("g"),
                    members: (0..=MAX_GROUP_MEMBERS)
                        .map(|index| format!("m{index}"))
                        .collect(),
                }]),
                ..VmUserConfig::default()
            }),
            ..CreateVmConfig::default()
        };
        assert!(too_many_members.validate(usize::MAX).is_err());
    }

    #[test]
    fn validate_rejects_fetch_limit_above_frame_cap() {
        let config = CreateVmConfig {
            limits: Some(VmLimitsConfig {
                http: Some(HttpLimitsConfig {
                    max_fetch_response_bytes: Some(2048),
                }),
                ..VmLimitsConfig::default()
            }),
            ..CreateVmConfig::default()
        };
        assert!(config.validate(1024).is_err());
    }

    #[test]
    fn canonical_reactor_and_protocol_limits_round_trip() {
        let config: CreateVmConfig = serde_json::from_value(serde_json::json!({
            "limits": {
                "resources": {
                    "maxConnections": 16,
                    "maxSocketBufferedBytes": 8192,
                    "maxSocketDatagramQueueLen": 128
                },
                "reactor": {
                    "maxCapabilities": 128,
                    "maxReadyHandles": 256,
                    "maxTasks": 512,
                    "workQuantum": 32,
                    "byteQuantum": 4096,
                    "maxHandleCommands": 64,
                    "maxHandleCommandBytes": 1024,
                    "maxBridgeCalls": 32,
                    "maxBridgeResponseBytes": 4096,
                    "maxAsyncCompletions": 64,
                    "maxAsyncCompletionBytes": 2048,
                    "maxBlockingJobs": 32,
                    "maxBlockingBytes": 4096,
                    "perHandleOperationQuantum": 8,
                    "acceptQuantum": 16,
                    "datagramQuantum": 8,
                    "completionQuantum": 16,
                    "signalQuantum": 16,
                    "shutdownDeadlineMs": 5000,
                    "operationDeadlineMs": 30000
                },
                "http": { "maxFetchResponseBytes": 4096 },
                "udp": {
                    "maxBufferedDatagrams": 64,
                    "maxBufferedBytes": 4096
                },
                "tls": { "maxBufferedBytes": 2048 },
                "http2": {
                    "maxConnections": 8,
                    "maxStreams": 64,
                    "maxStreamsPerConnection": 16,
                    "maxBufferedBytes": 8192,
                    "maxHeaderBytes": 1024,
                    "maxDataBytes": 4096,
                    "maxPendingCommands": 32,
                    "maxPendingCommandBytes": 1024,
                    "maxPendingEvents": 32,
                    "maxPendingEventBytes": 2048
                }
            }
        }))
        .expect("decode canonical limits");
        config.validate(16 * 1024).expect("valid relationships");

        let json = serde_json::to_string(&config).expect("serialize canonical limits");
        let decoded: CreateVmConfig = serde_json::from_str(&json).expect("decode round trip");
        assert_eq!(decoded, config);
        assert!(json.contains("maxHandleCommandBytes"));
        assert!(json.contains("maxBufferedDatagrams"));
        assert!(json.contains("maxStreamsPerConnection"));
        assert!(json.contains("shutdownDeadlineMs"));
    }

    #[test]
    fn canonical_limits_reject_zero_and_invalid_parent_relationships() {
        let cases = [
            (
                serde_json::json!({
                    "reactor": { "maxHandleCommands": 0 }
                }),
                "limits.reactor.maxHandleCommands",
            ),
            (
                serde_json::json!({
                    "reactor": { "maxBlockingJobs": 0 }
                }),
                "limits.reactor.maxBlockingJobs",
            ),
            (
                serde_json::json!({
                    "udp": { "maxBufferedBytes": 0 }
                }),
                "limits.udp.maxBufferedBytes",
            ),
            (
                serde_json::json!({
                    "reactor": { "maxCapabilities": 8, "maxReadyHandles": 4 }
                }),
                "limits.reactor.maxCapabilities",
            ),
            (
                serde_json::json!({
                    "resources": { "maxSocketBufferedBytes": 1024 },
                    "tls": { "maxBufferedBytes": 2048 }
                }),
                "limits.tls.maxBufferedBytes",
            ),
            (
                serde_json::json!({
                    "http2": {
                        "maxBufferedBytes": 1024,
                        "maxPendingEventBytes": 2048
                    }
                }),
                "limits.http2.maxPendingEventBytes",
            ),
            (
                serde_json::json!({
                    "resources": { "maxSocketDatagramQueueLen": 16 },
                    "udp": { "maxBufferedDatagrams": 32 }
                }),
                "limits.udp.maxBufferedDatagrams",
            ),
        ];

        for (limits, expected_path) in cases {
            let config: CreateVmConfig = serde_json::from_value(serde_json::json!({
                "limits": limits
            }))
            .expect("decode invalid relationship fixture");
            let error = config
                .validate(16 * 1024)
                .expect_err("invalid relationship must fail");
            assert!(
                error.to_string().contains(expected_path),
                "expected {expected_path} in {error}"
            );
        }
    }

    #[test]
    fn wasm_cpu_fields_round_trip_without_legacy_aliases() {
        let config: CreateVmConfig = serde_json::from_value(serde_json::json!({
            "limits": {
                "wasm": {
                    "activeCpuTimeLimitMs": 30_000,
                    "wallClockLimitMs": 45_000,
                    "deterministicFuel": 1_000_000
                }
            }
        }))
        .expect("decode WASM CPU fields");
        let wasm = config
            .limits
            .as_ref()
            .and_then(|limits| limits.wasm.as_ref())
            .expect("WASM limits");
        assert_eq!(wasm.active_cpu_time_limit_ms, Some(30_000));
        assert_eq!(wasm.wall_clock_limit_ms, Some(45_000));
        assert_eq!(wasm.deterministic_fuel, Some(1_000_000));

        let json = serde_json::to_string(&config).expect("serialize WASM CPU fields");
        assert!(json.contains("activeCpuTimeLimitMs"));
        assert!(json.contains("wallClockLimitMs"));
        assert!(json.contains("deterministicFuel"));

        let removed_fuel_name = ["maxWasm", "Fuel"].concat();
        let removed_runner_cpu_name = ["runnerCpu", "TimeLimitMs"].concat();
        for legacy_limits in [
            serde_json::json!({ "resources": { (removed_fuel_name): 1 } }),
            serde_json::json!({ "wasm": { (removed_runner_cpu_name): 1 } }),
        ] {
            let error = serde_json::from_value::<CreateVmConfig>(serde_json::json!({
                "limits": legacy_limits
            }))
            .expect_err("removed WASM CPU field must be rejected");
            assert!(error.to_string().contains("unknown field"));
        }
    }

    fn js_runtime_config(value: serde_json::Value) -> Result<CreateVmConfig, serde_json::Error> {
        serde_json::from_value(serde_json::json!({ "jsRuntime": value }))
    }

    #[test]
    fn js_runtime_defaults_to_node() {
        let config: CreateVmConfig =
            serde_json::from_value(serde_json::json!({ "jsRuntime": {} })).expect("decode");
        let js = config.js_runtime.expect("jsRuntime present");
        assert_eq!(js.platform, JsRuntimePlatform::Node);
        assert_eq!(js.module_resolution, JsModuleResolution::Node);
        assert!(js.allowed_builtins.is_none());
        assert!(js.high_resolution_time.is_none());
    }

    #[test]
    fn js_runtime_high_resolution_time_defaults_off_and_round_trips() {
        let defaulted = js_runtime_config(serde_json::json!({})).unwrap();
        assert!(defaulted.js_runtime.unwrap().high_resolution_time.is_none());

        let enabled = js_runtime_config(serde_json::json!({
            "highResolutionTime": true,
        }))
        .unwrap();
        assert_eq!(
            enabled.js_runtime.as_ref().unwrap().high_resolution_time,
            Some(true)
        );
        let json = serde_json::to_string(&enabled).expect("serialize");
        assert!(json.contains("highResolutionTime"));
        let decoded: CreateVmConfig = serde_json::from_str(&json).expect("re-decode");
        assert_eq!(decoded, enabled);
    }

    #[test]
    fn js_runtime_all_platform_resolution_combos_round_trip() {
        for platform in ["node", "browser", "neutral", "bare"] {
            for resolution in ["node", "relative", "none"] {
                let config = js_runtime_config(serde_json::json!({
                    "platform": platform,
                    "moduleResolution": resolution,
                }))
                .unwrap_or_else(|err| panic!("decode {platform}/{resolution}: {err}"));
                let json = serde_json::to_string(&config).expect("serialize");
                let decoded: CreateVmConfig = serde_json::from_str(&json).expect("re-decode");
                assert_eq!(decoded, config);
                assert!(config.validate(usize::MAX).is_ok());
            }
        }
    }

    #[test]
    fn js_runtime_allowed_builtins_tri_state() {
        // None => omitted.
        let none = js_runtime_config(serde_json::json!({ "platform": "node" })).unwrap();
        assert!(none.js_runtime.unwrap().allowed_builtins.is_none());
        // Some([]) => deny all (representable, distinct from None).
        let empty = js_runtime_config(serde_json::json!({ "allowedBuiltins": [] })).unwrap();
        assert_eq!(empty.js_runtime.unwrap().allowed_builtins, Some(Vec::new()));
        // Some([..]) => explicit.
        let some = js_runtime_config(serde_json::json!({ "allowedBuiltins": ["path", "node:fs"] }))
            .unwrap();
        assert_eq!(
            some.js_runtime.unwrap().allowed_builtins,
            Some(vec!["path".to_owned(), "node:fs".to_owned()])
        );
    }

    #[test]
    fn js_runtime_rejects_allowed_builtins_under_non_node_platform() {
        for platform in ["browser", "neutral", "bare"] {
            let config = js_runtime_config(serde_json::json!({
                "platform": platform,
                "allowedBuiltins": ["path"],
            }))
            .unwrap();
            let error = config
                .validate(usize::MAX)
                .expect_err("allowedBuiltins under non-node must reject");
            assert!(error.to_string().contains("allowedBuiltins"));
        }
    }

    #[test]
    fn js_runtime_rejects_unknown_builtin_names() {
        let config = js_runtime_config(serde_json::json!({
            "platform": "node",
            "allowedBuiltins": ["path", "totally_not_a_builtin"],
        }))
        .unwrap();
        let error = config
            .validate(usize::MAX)
            .expect_err("unknown builtin must reject");
        assert!(error.to_string().contains("unknown builtin"));
    }

    #[test]
    fn js_runtime_accepts_empty_allow_list_under_node() {
        let config =
            js_runtime_config(serde_json::json!({ "platform": "node", "allowedBuiltins": [] }))
                .unwrap();
        assert!(config.validate(usize::MAX).is_ok());
    }

    #[test]
    fn js_runtime_rejects_unknown_fields() {
        let error = js_runtime_config(serde_json::json!({ "surprise": true }))
            .expect_err("unknown jsRuntime field should fail");
        assert!(error.to_string().contains("unknown field"));
    }
}
