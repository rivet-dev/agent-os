use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use agentos_runtime::RuntimeContext;
use tokio::sync::Mutex as AsyncMutex;
use tokio::time;

pub(crate) const NODE_IMPORT_CACHE_DEBUG_ENV: &str = "AGENTOS_NODE_IMPORT_CACHE_DEBUG";
pub(crate) const NODE_IMPORT_CACHE_METRICS_PREFIX: &str = "__AGENTOS_NODE_IMPORT_CACHE_METRICS__:";
pub(crate) const NODE_IMPORT_CACHE_ASSET_ROOT_ENV: &str = "AGENTOS_NODE_IMPORT_CACHE_ASSET_ROOT";

const NODE_IMPORT_CACHE_PATH_ENV: &str = "AGENTOS_NODE_IMPORT_CACHE_PATH";
const NODE_IMPORT_CACHE_LOADER_PATH_ENV: &str = "AGENTOS_NODE_IMPORT_CACHE_LOADER_PATH";
const NODE_IMPORT_CACHE_SCHEMA_VERSION: &str = "1";
const NODE_IMPORT_CACHE_LOADER_VERSION: &str = "8";
// Upstream reached 104 while the reactor branch independently changed bundled
// assets; use a new generation so no stale materialization survives the merge.
const NODE_IMPORT_CACHE_ASSET_VERSION: &str = "106";
const NODE_IMPORT_CACHE_DIR_PREFIX: &str = "agentos-node-import-cache";
const DEFAULT_NODE_IMPORT_CACHE_MATERIALIZE_TIMEOUT: Duration = Duration::from_secs(30);
const NODE_IMPORT_CACHE_BLOCKING_JOB_RESERVATION_BYTES: usize = 64 * 1024;
const PYODIDE_DIST_DIR: &str = "pyodide-dist";
const AGENTOS_BUILTIN_SPECIFIER_PREFIX: &str = "agentos:builtin/";
const AGENTOS_POLYFILL_SPECIFIER_PREFIX: &str = "agentos:polyfill/";
const BUNDLED_PYODIDE_MJS: &[u8] = include_bytes!("../assets/pyodide/pyodide.mjs");
// Large Pyodide assets are excluded from the published crate and staged into
// OUT_DIR by build.rs (copied from `assets/pyodide/` in-tree, or downloaded
// from the release CDN when building the published crate).
const BUNDLED_PYODIDE_ASM_JS: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/pyodide/pyodide.asm.js"));
const BUNDLED_PYODIDE_ASM_WASM: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/pyodide/pyodide.asm.wasm"));
const BUNDLED_PYODIDE_LOCK: &[u8] = include_bytes!("../assets/pyodide/pyodide-lock.json");
const BUNDLED_PYTHON_STDLIB_ZIP: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/pyodide/python_stdlib.zip"));
const BUNDLED_NUMPY_WHL: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/pyodide/numpy-2.2.5-cp313-cp313-pyodide_2025_0_wasm32.whl"
));
const BUNDLED_PANDAS_WHL: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/pyodide/pandas-2.3.3-cp313-cp313-pyodide_2025_0_wasm32.whl"
));
const BUNDLED_PYTHON_DATEUTIL_WHL: &[u8] =
    include_bytes!("../assets/pyodide/python_dateutil-2.9.0.post0-py2.py3-none-any.whl");
const BUNDLED_PYTZ_WHL: &[u8] =
    include_bytes!("../assets/pyodide/pytz-2025.2-py2.py3-none-any.whl");
const BUNDLED_SIX_WHL: &[u8] = include_bytes!("../assets/pyodide/six-1.17.0-py2.py3-none-any.whl");
const BUNDLED_MICROPIP_WHL: &[u8] =
    include_bytes!("../assets/pyodide/micropip-0.11.0-py3-none-any.whl");
const BUNDLED_CLICK_WHL: &[u8] = include_bytes!("../assets/pyodide/click-8.3.1-py3-none-any.whl");
const NODE_PYTHON_RUNNER_SOURCE: &str = include_str!("../assets/runners/python-runner.mjs");

const BUNDLED_TYPESCRIPT_ASSETS: &[(&str, &[u8])] =
    include!(concat!(env!("OUT_DIR"), "/typescript_assets.rs"));

#[doc(hidden)]
pub fn bundled_typescript_assets() -> &'static [(&'static str, &'static [u8])] {
    BUNDLED_TYPESCRIPT_ASSETS
}

static CLEANED_NODE_IMPORT_CACHE_ROOTS: OnceLock<Mutex<BTreeSet<PathBuf>>> = OnceLock::new();
#[cfg(test)]
static NODE_IMPORT_CACHE_TEST_MATERIALIZE_DELAY_MS: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
static NODE_IMPORT_CACHE_TEST_MATERIALIZE_DELAY_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy)]
struct BundledPyodidePackageAsset {
    file_name: &'static str,
    bytes: &'static [u8],
}

const BUNDLED_PYODIDE_PACKAGE_ASSETS: &[BundledPyodidePackageAsset] = &[
    BundledPyodidePackageAsset {
        file_name: "numpy-2.2.5-cp313-cp313-pyodide_2025_0_wasm32.whl",
        bytes: BUNDLED_NUMPY_WHL,
    },
    BundledPyodidePackageAsset {
        file_name: "pandas-2.3.3-cp313-cp313-pyodide_2025_0_wasm32.whl",
        bytes: BUNDLED_PANDAS_WHL,
    },
    BundledPyodidePackageAsset {
        file_name: "python_dateutil-2.9.0.post0-py2.py3-none-any.whl",
        bytes: BUNDLED_PYTHON_DATEUTIL_WHL,
    },
    BundledPyodidePackageAsset {
        file_name: "pytz-2025.2-py2.py3-none-any.whl",
        bytes: BUNDLED_PYTZ_WHL,
    },
    BundledPyodidePackageAsset {
        file_name: "six-1.17.0-py2.py3-none-any.whl",
        bytes: BUNDLED_SIX_WHL,
    },
    BundledPyodidePackageAsset {
        file_name: "micropip-0.11.0-py3-none-any.whl",
        bytes: BUNDLED_MICROPIP_WHL,
    },
    BundledPyodidePackageAsset {
        file_name: "click-8.3.1-py3-none-any.whl",
        bytes: BUNDLED_CLICK_WHL,
    },
];
const NODE_IMPORT_CACHE_LOADER_TEMPLATE: &str = r#"
import crypto from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const GUEST_PATH_MAPPINGS = parseGuestPathMappings(process.env.AGENTOS_GUEST_PATH_MAPPINGS);
const ALLOWED_BUILTINS = new Set(parseJsonArray(process.env.AGENTOS_ALLOWED_NODE_BUILTINS));
const CACHE_PATH = process.env.__NODE_IMPORT_CACHE_PATH_ENV__;
const CACHE_ROOT = CACHE_PATH ? path.dirname(CACHE_PATH) : null;
const GUEST_INTERNAL_CACHE_ROOT = '/.agentos/node-import-cache';
const HOST_CWD = process.cwd();
const DEFAULT_GUEST_CWD =
  typeof process.env.PWD === 'string' &&
  process.env.PWD.startsWith('/')
    ? path.posix.normalize(process.env.PWD)
    : typeof (globalThis.__agentOSVirtualOs||{}).homedir === 'string' &&
        (globalThis.__agentOSVirtualOs||{}).homedir.startsWith('/')
      ? path.posix.normalize((globalThis.__agentOSVirtualOs||{}).homedir)
    : '/root';
const UNMAPPED_GUEST_PATH = '/unknown';
const PROJECTED_SOURCE_CACHE_ROOT = CACHE_PATH
  ? path.join(path.dirname(CACHE_PATH), 'projected-sources')
  : null;
const ASSET_ROOT = process.env.__NODE_IMPORT_CACHE_ASSET_ROOT_ENV__;
const DEBUG_ENABLED = process.env.__NODE_IMPORT_CACHE_DEBUG_ENV__ === '1';
const CONTROL_PIPE_FD = parseControlPipeFd(process.env.AGENTOS_CONTROL_PIPE_FD);
const SCHEMA_VERSION = '__NODE_IMPORT_CACHE_SCHEMA_VERSION__';
const LOADER_VERSION = '__NODE_IMPORT_CACHE_LOADER_VERSION__';
const ASSET_VERSION = '__NODE_IMPORT_CACHE_ASSET_VERSION__';
const MAX_CACHE_RECORD_ENTRIES = 512;
const MAX_CACHE_KEY_BYTES = 4096;
const MAX_CACHE_VALUE_BYTES = 16 * 1024;
const MAX_CACHE_STATE_BYTES = 4 * 1024 * 1024;
const BUILTIN_PREFIX = '__AGENTOS_BUILTIN_SPECIFIER_PREFIX__';
const POLYFILL_PREFIX = '__AGENTOS_POLYFILL_SPECIFIER_PREFIX__';
const FS_ASSET_SPECIFIER = `${BUILTIN_PREFIX}fs`;
const FS_PROMISES_ASSET_SPECIFIER = `${BUILTIN_PREFIX}fs-promises`;
const CHILD_PROCESS_ASSET_SPECIFIER = `${BUILTIN_PREFIX}child-process`;
const NET_ASSET_SPECIFIER = `${BUILTIN_PREFIX}net`;
const DGRAM_ASSET_SPECIFIER = `${BUILTIN_PREFIX}dgram`;
const DNS_ASSET_SPECIFIER = `${BUILTIN_PREFIX}dns`;
const DNS_PROMISES_ASSET_SPECIFIER = `${BUILTIN_PREFIX}dns-promises`;
const HTTP_ASSET_SPECIFIER = `${BUILTIN_PREFIX}http`;
const HTTP2_ASSET_SPECIFIER = `${BUILTIN_PREFIX}http2`;
const HTTPS_ASSET_SPECIFIER = `${BUILTIN_PREFIX}https`;
const TLS_ASSET_SPECIFIER = `${BUILTIN_PREFIX}tls`;
const OS_ASSET_SPECIFIER = `${BUILTIN_PREFIX}os`;
const DENIED_BUILTINS = new Set([
  'child_process',
  'cluster',
  'dgram',
  'dns',
  'dns/promises',
  'http',
  'http2',
  'https',
  'inspector',
  'module',
  'net',
  'tls',
  'trace_events',
  'v8',
  'vm',
  'worker_threads',
].filter((name) => !ALLOWED_BUILTINS.has(name)));

let cacheState = loadCacheState();
let dirty = false;
let cacheWriteError = null;
const metrics = {
  resolveHits: 0,
  resolveMisses: 0,
  packageTypeHits: 0,
  packageTypeMisses: 0,
  moduleFormatHits: 0,
  moduleFormatMisses: 0,
  sourceHits: 0,
  sourceMisses: 0,
};

export async function resolve(specifier, context, nextResolve) {
  const guestResolvedPath = resolveGuestSpecifier(specifier, context);
  if (guestResolvedPath) {
    const guestUrl = pathToFileURL(guestResolvedPath).href;
    const format = lookupModuleFormat(guestUrl);
    flushCacheState();
    emitMetrics();
    return {
      shortCircuit: true,
      url: guestUrl,
      ...(format && format !== 'builtin' ? { format } : {}),
    };
  }

  const key = createResolutionKey(specifier, context);
  const cached = cacheState.resolutions[key];

  if (cached && validateResolutionEntry(cached)) {
    metrics.resolveHits += 1;
    const response = {
      shortCircuit: true,
      url: cached.resolvedUrl,
    };

    if (cached.format) {
      response.format = cached.format;
    }

    flushCacheState();
    emitMetrics();
    return response;
  }

  metrics.resolveMisses += 1;

  const asset = resolveAgentOsAsset(specifier);
  if (asset) {
    cacheState.resolutions[key] = {
      kind: 'explicit-file',
      resolvedUrl: asset.url,
      format: 'module',
      resolvedFilePath: asset.filePath,
    };
    dirty = true;
    flushCacheState();
    emitMetrics();
    return {
      shortCircuit: true,
      url: asset.url,
      format: 'module',
    };
  }

  const builtinAsset = resolveBuiltinAsset(specifier, context);
  if (builtinAsset) {
    cacheState.resolutions[key] = {
      kind: 'explicit-file',
      resolvedUrl: builtinAsset.url,
      format: 'module',
      resolvedFilePath: builtinAsset.filePath,
    };
    dirty = true;
    flushCacheState();
    emitMetrics();
    return {
      shortCircuit: true,
      url: builtinAsset.url,
      format: 'module',
    };
  }

  const deniedBuiltin = resolveDeniedBuiltin(specifier);
  if (deniedBuiltin) {
    cacheState.resolutions[key] = {
      kind: 'explicit-file',
      resolvedUrl: deniedBuiltin.url,
      format: 'module',
      resolvedFilePath: deniedBuiltin.filePath,
    };
    dirty = true;
    flushCacheState();
    emitMetrics();
    return {
      shortCircuit: true,
      url: deniedBuiltin.url,
      format: 'module',
    };
  }

  const translatedContext = translateContextParentUrl(context);
  let resolved;
  try {
    resolved = await nextResolve(specifier, translatedContext);
  } catch (error) {
    flushCacheState();
    emitMetrics();
    throw translateErrorToGuest(error);
  }
  const translatedUrl = translateResolvedUrlToGuest(resolved.url);
  const translatedResolved =
    translatedUrl === resolved.url ? resolved : { ...resolved, url: translatedUrl };
  const entry = buildResolutionEntry(specifier, context, translatedResolved);
  if (entry) {
    cacheState.resolutions[key] = entry;
    dirty = true;
  }

  if (entry && entry.format && resolved.format == null) {
    flushCacheState();
    emitMetrics();
    return {
      ...translatedResolved,
      format: entry.format,
    };
  }

  flushCacheState();
  emitMetrics();
  return translatedResolved;
}

export async function load(url, context, nextLoad) {
  try {
    const filePath = filePathFromUrl(url);
    const format = lookupModuleFormat(url) ?? context.format;

    if (!filePath || !format || format === 'builtin') {
      return await nextLoad(url, context);
    }

    const projectedPackageSource = loadProjectedPackageSource(url, filePath, format);
    if (projectedPackageSource != null) {
      flushCacheState();
      emitMetrics();
      return {
        shortCircuit: true,
        format,
        source: projectedPackageSource,
      };
    }

    const source =
      format === 'wasm'
        ? fs.readFileSync(filePath)
        : rewriteBuiltinImports(fs.readFileSync(filePath, 'utf8'), filePath);

    return {
      shortCircuit: true,
      format,
      source,
    };
  } catch (error) {
    flushCacheState();
    emitMetrics();
    throw translateErrorToGuest(error);
  }
}

function loadCacheState() {
  if (!CACHE_PATH) {
    return emptyCacheState();
  }

  try {
    const stat = fs.statSync(CACHE_PATH);
    if (!stat.isFile() || stat.size > MAX_CACHE_STATE_BYTES) {
      return emptyCacheState();
    }
    const parsed = JSON.parse(fs.readFileSync(CACHE_PATH, 'utf8'));
    if (!isCompatibleCacheState(parsed)) {
      return emptyCacheState();
    }

    return normalizeCacheState(parsed);
  } catch {
    return emptyCacheState();
  }
}

function flushCacheState() {
  if (!CACHE_PATH || !dirty) {
    return;
  }

  try {
    fs.mkdirSync(path.dirname(CACHE_PATH), { recursive: true });

    let merged = cacheState;
    try {
      const existingStat = fs.statSync(CACHE_PATH);
      if (existingStat.isFile() && existingStat.size <= MAX_CACHE_STATE_BYTES) {
        const existing = JSON.parse(fs.readFileSync(CACHE_PATH, 'utf8'));
        if (isCompatibleCacheState(existing)) {
          merged = mergeCacheStates(normalizeCacheState(existing), cacheState);
        }
      }
    } catch {
      // Ignore missing or unreadable prior state and replace it with the in-memory view.
    }

    merged = pruneCacheState(merged);
    let serialized = JSON.stringify(merged);
    if (byteLengthUtf8(serialized) > MAX_CACHE_STATE_BYTES) {
      merged = pruneCacheState(merged, Math.floor(MAX_CACHE_RECORD_ENTRIES / 4));
      serialized = JSON.stringify(merged);
    }
    if (byteLengthUtf8(serialized) > MAX_CACHE_STATE_BYTES) {
      merged = emptyCacheState();
      serialized = JSON.stringify(merged);
    }

    const tempPath = `${CACHE_PATH}.${process.pid}.${Date.now()}.tmp`;
    fs.writeFileSync(tempPath, serialized);
    fs.renameSync(tempPath, CACHE_PATH);
    cacheState = merged;
    pruneProjectedSourceFiles();
    dirty = false;
  } catch (error) {
    cacheWriteError = error instanceof Error ? error.message : String(error);
  }
}

function emitMetrics() {
  if (!DEBUG_ENABLED) {
    return;
  }

  const payload = cacheWriteError
    ? { ...metrics, cacheWriteError }
    : metrics;

  emitControlMessage({ type: 'node_import_cache_metrics', metrics: payload });
}

function parseControlPipeFd(value) {
  if (typeof value !== 'string' || value.trim() === '') {
    return null;
  }

  const parsed = Number.parseInt(value, 10);
  return Number.isInteger(parsed) && parsed >= 3 ? parsed : null;
}

function emitControlMessage(message) {
  if (CONTROL_PIPE_FD == null) {
    if (
      message?.type === 'signal_state' &&
      typeof process?.stdout?.write === 'function'
    ) {
      try {
        process.stdout.write(`__AGENTOS_WASM_SIGNAL_STATE__:${JSON.stringify(message)}\n`);
      } catch {
        // Ignore control-channel fallback failures during teardown.
      }
    }
    return;
  }

  try {
    fs.writeSync(CONTROL_PIPE_FD, `${JSON.stringify(message)}\n`);
  } catch {
    if (
      message?.type === 'signal_state' &&
      typeof process?.stdout?.write === 'function'
    ) {
      try {
        process.stdout.write(`__AGENTOS_WASM_SIGNAL_STATE__:${JSON.stringify(message)}\n`);
      } catch {
        // Ignore control-channel fallback failures during teardown.
      }
    }
  }
}

function emptyCacheState() {
  return {
    schemaVersion: SCHEMA_VERSION,
    loaderVersion: LOADER_VERSION,
    assetVersion: ASSET_VERSION,
    nodeVersion: process.version,
    resolutions: {},
    packageTypes: {},
    moduleFormats: {},
    projectedSources: {},
  };
}

function isCompatibleCacheState(value) {
  return (
    isRecord(value) &&
    value.schemaVersion === SCHEMA_VERSION &&
    value.loaderVersion === LOADER_VERSION &&
    value.assetVersion === ASSET_VERSION &&
    value.nodeVersion === process.version
  );
}

function normalizeCacheState(value) {
  return pruneCacheState({
    ...emptyCacheState(),
    ...value,
    resolutions: isRecord(value.resolutions) ? value.resolutions : {},
    packageTypes: isRecord(value.packageTypes) ? value.packageTypes : {},
    moduleFormats: isRecord(value.moduleFormats) ? value.moduleFormats : {},
    projectedSources: isRecord(value.projectedSources) ? value.projectedSources : {},
  });
}

function mergeCacheStates(base, current) {
  return pruneCacheState({
    ...emptyCacheState(),
    resolutions: {
      ...base.resolutions,
      ...current.resolutions,
    },
    packageTypes: {
      ...base.packageTypes,
      ...current.packageTypes,
    },
    moduleFormats: {
      ...base.moduleFormats,
      ...current.moduleFormats,
    },
    projectedSources: {
      ...base.projectedSources,
      ...current.projectedSources,
    },
  });
}

function pruneCacheState(state, maxEntries = MAX_CACHE_RECORD_ENTRIES) {
  return {
    ...emptyCacheState(),
    ...state,
    resolutions: pruneCacheRecord(state.resolutions, maxEntries),
    packageTypes: pruneCacheRecord(state.packageTypes, maxEntries),
    moduleFormats: pruneCacheRecord(state.moduleFormats, maxEntries),
    projectedSources: pruneCacheRecord(state.projectedSources, maxEntries),
  };
}

function pruneCacheRecord(record, maxEntries) {
  if (!isRecord(record)) {
    return {};
  }

  const entries = [];
  for (const [key, value] of Object.entries(record)) {
    if (
      byteLengthUtf8(key) <= MAX_CACHE_KEY_BYTES &&
      cacheValueLength(value) <= MAX_CACHE_VALUE_BYTES
    ) {
      entries.push([key, value]);
    }
  }

  return Object.fromEntries(entries.slice(-maxEntries));
}

function cacheValueLength(value) {
  try {
    return byteLengthUtf8(JSON.stringify(value));
  } catch {
    return MAX_CACHE_VALUE_BYTES + 1;
  }
}

function byteLengthUtf8(value) {
  return Buffer.byteLength(String(value), 'utf8');
}

function pruneProjectedSourceFiles() {
  if (!PROJECTED_SOURCE_CACHE_ROOT) {
    return;
  }

  const retained = new Set();
  for (const entry of Object.values(cacheState.projectedSources)) {
    if (
      isRecord(entry) &&
      typeof entry.cachedPath === 'string' &&
      path.dirname(entry.cachedPath) === PROJECTED_SOURCE_CACHE_ROOT
    ) {
      retained.add(path.resolve(entry.cachedPath));
    }
  }

  let entries;
  try {
    entries = fs.readdirSync(PROJECTED_SOURCE_CACHE_ROOT, { withFileTypes: true });
  } catch {
    return;
  }

  for (const entry of entries) {
    if (!entry.isFile()) {
      continue;
    }
    const filePath = path.resolve(PROJECTED_SOURCE_CACHE_ROOT, entry.name);
    if (!retained.has(filePath)) {
      try {
        fs.unlinkSync(filePath);
      } catch {
        // Best-effort cleanup. A failed unlink should not break module loading.
      }
    }
  }
}

function loadProjectedPackageSource(url, filePath, format) {
  if (
    format === 'wasm' ||
    !isProjectedPackageSource(filePath) ||
    !PROJECTED_SOURCE_CACHE_ROOT
  ) {
    return null;
  }

  const cached = cacheState.projectedSources[url];
  if (cached && validateProjectedSourceEntry(cached, filePath, format)) {
    metrics.sourceHits += 1;
    return fs.readFileSync(cached.cachedPath, 'utf8');
  }

  metrics.sourceMisses += 1;

  const stat = statForPath(filePath);
  if (!stat) {
    return null;
  }

  const source = rewriteBuiltinImports(fs.readFileSync(filePath, 'utf8'), filePath);
  const cacheKey = hashString(
    JSON.stringify({
      url,
      format,
      size: stat.size,
      mtimeMs: stat.mtimeMs,
    }),
  );
  const extension = path.extname(filePath) || '.js';
  const cachedPath = path.join(
    PROJECTED_SOURCE_CACHE_ROOT,
    `${cacheKey}${extension}.cached`,
  );
  fs.mkdirSync(path.dirname(cachedPath), { recursive: true });
  fs.writeFileSync(cachedPath, source);

  cacheState.projectedSources[url] = {
    kind: 'text',
    filePath,
    format,
    cachedPath,
    size: stat.size,
    mtimeMs: stat.mtimeMs,
  };
  dirty = true;
  return source;
}

function resolveAgentOsAsset(specifier) {
  if (typeof specifier !== 'string' || !ASSET_ROOT) {
    return null;
  }

  if (specifier.startsWith(BUILTIN_PREFIX)) {
    return assetModuleDescriptor(
      path.join(
        ASSET_ROOT,
        'builtins',
        `${sanitizeAssetName(specifier.slice(BUILTIN_PREFIX.length))}.mjs`,
      ),
    );
  }

  if (specifier.startsWith(POLYFILL_PREFIX)) {
    return assetModuleDescriptor(
      path.join(
        ASSET_ROOT,
        'polyfills',
        `${sanitizeAssetName(specifier.slice(POLYFILL_PREFIX.length))}.mjs`,
      ),
    );
  }

  return null;
}

function rewriteBuiltinImports(source, filePath) {
  if (typeof source !== 'string' || isAssetPath(filePath)) {
    return source;
  }

  let rewritten = source;

  for (const specifier of ['node:fs/promises', 'fs/promises']) {
    rewritten = replaceBuiltinImportSpecifier(
      rewritten,
      specifier,
      FS_PROMISES_ASSET_SPECIFIER,
    );
    rewritten = replaceBuiltinDynamicImportSpecifier(
      rewritten,
      specifier,
      FS_PROMISES_ASSET_SPECIFIER,
    );
  }

  for (const specifier of ['node:fs', 'fs']) {
    rewritten = replaceBuiltinImportSpecifier(
      rewritten,
      specifier,
      FS_ASSET_SPECIFIER,
    );
    rewritten = replaceBuiltinDynamicImportSpecifier(
      rewritten,
      specifier,
      FS_ASSET_SPECIFIER,
    );
  }

  if (ALLOWED_BUILTINS.has('child_process')) {
    for (const specifier of ['node:child_process', 'child_process']) {
      rewritten = replaceBuiltinImportSpecifier(
        rewritten,
        specifier,
        CHILD_PROCESS_ASSET_SPECIFIER,
      );
      rewritten = replaceBuiltinDynamicImportSpecifier(
        rewritten,
        specifier,
        CHILD_PROCESS_ASSET_SPECIFIER,
      );
    }
  }

  if (ALLOWED_BUILTINS.has('net')) {
    for (const specifier of ['node:net', 'net']) {
      rewritten = replaceBuiltinImportSpecifier(
        rewritten,
        specifier,
        NET_ASSET_SPECIFIER,
      );
      rewritten = replaceBuiltinDynamicImportSpecifier(
        rewritten,
        specifier,
        NET_ASSET_SPECIFIER,
      );
    }
  }

  if (ALLOWED_BUILTINS.has('dgram')) {
    for (const specifier of ['node:dgram', 'dgram']) {
      rewritten = replaceBuiltinImportSpecifier(
        rewritten,
        specifier,
        DGRAM_ASSET_SPECIFIER,
      );
      rewritten = replaceBuiltinDynamicImportSpecifier(
        rewritten,
        specifier,
        DGRAM_ASSET_SPECIFIER,
      );
    }
  }

  if (ALLOWED_BUILTINS.has('dns')) {
    for (const specifier of ['node:dns/promises', 'dns/promises']) {
      rewritten = replaceBuiltinImportSpecifier(
        rewritten,
        specifier,
        DNS_PROMISES_ASSET_SPECIFIER,
      );
      rewritten = replaceBuiltinDynamicImportSpecifier(
        rewritten,
        specifier,
        DNS_PROMISES_ASSET_SPECIFIER,
      );
    }
    for (const specifier of ['node:dns', 'dns']) {
      rewritten = replaceBuiltinImportSpecifier(
        rewritten,
        specifier,
        DNS_ASSET_SPECIFIER,
      );
      rewritten = replaceBuiltinDynamicImportSpecifier(
        rewritten,
        specifier,
        DNS_ASSET_SPECIFIER,
      );
    }
  }

  if (ALLOWED_BUILTINS.has('http')) {
    for (const specifier of ['node:http', 'http']) {
      rewritten = replaceBuiltinImportSpecifier(
        rewritten,
        specifier,
        HTTP_ASSET_SPECIFIER,
      );
      rewritten = replaceBuiltinDynamicImportSpecifier(
        rewritten,
        specifier,
        HTTP_ASSET_SPECIFIER,
      );
    }
  }

  if (ALLOWED_BUILTINS.has('http2')) {
    for (const specifier of ['node:http2', 'http2']) {
      rewritten = replaceBuiltinImportSpecifier(
        rewritten,
        specifier,
        HTTP2_ASSET_SPECIFIER,
      );
      rewritten = replaceBuiltinDynamicImportSpecifier(
        rewritten,
        specifier,
        HTTP2_ASSET_SPECIFIER,
      );
    }
  }

  if (ALLOWED_BUILTINS.has('https')) {
    for (const specifier of ['node:https', 'https']) {
      rewritten = replaceBuiltinImportSpecifier(
        rewritten,
        specifier,
        HTTPS_ASSET_SPECIFIER,
      );
      rewritten = replaceBuiltinDynamicImportSpecifier(
        rewritten,
        specifier,
        HTTPS_ASSET_SPECIFIER,
      );
    }
  }

  if (ALLOWED_BUILTINS.has('tls')) {
    for (const specifier of ['node:tls', 'tls']) {
      rewritten = replaceBuiltinImportSpecifier(
        rewritten,
        specifier,
        TLS_ASSET_SPECIFIER,
      );
      rewritten = replaceBuiltinDynamicImportSpecifier(
        rewritten,
        specifier,
        TLS_ASSET_SPECIFIER,
      );
    }
  }

  if (ALLOWED_BUILTINS.has('os')) {
    for (const specifier of ['node:os', 'os']) {
      rewritten = replaceBuiltinImportSpecifier(
        rewritten,
        specifier,
        OS_ASSET_SPECIFIER,
      );
      rewritten = replaceBuiltinDynamicImportSpecifier(
        rewritten,
        specifier,
        OS_ASSET_SPECIFIER,
      );
    }
  }

  return rewritten;
}

function replaceBuiltinImportSpecifier(source, specifier, replacement) {
  const pattern = new RegExp(
    `(\\bfrom\\s*)(['"])${escapeRegExp(specifier)}\\2`,
    'g',
  );
  return source.replace(pattern, `$1$2${replacement}$2`);
}

function replaceBuiltinDynamicImportSpecifier(source, specifier, replacement) {
  const pattern = new RegExp(
    `(\\bimport\\s*\\(\\s*)(['"])${escapeRegExp(specifier)}\\2(\\s*\\))`,
    'g',
  );
  return source.replace(pattern, `$1$2${replacement}$2$3`);
}

function isAssetPath(filePath) {
  return (
    typeof filePath === 'string' &&
    typeof ASSET_ROOT === 'string' &&
    (filePath === ASSET_ROOT || filePath.startsWith(`${ASSET_ROOT}${path.sep}`))
  );
}

function resolveDeniedBuiltin(specifier) {
  if (typeof specifier !== 'string' || !ASSET_ROOT) {
    return null;
  }

  const normalized =
    specifier.startsWith('node:') ? specifier.slice('node:'.length) : specifier;
  if (!DENIED_BUILTINS.has(normalized)) {
    return null;
  }

  return assetModuleDescriptor(
    path.join(ASSET_ROOT, 'denied', `${sanitizeAssetName(normalized)}.mjs`),
  );
}

function resolveBuiltinAsset(specifier, context) {
  if (
    typeof specifier !== 'string' ||
    !ASSET_ROOT ||
    !specifier.startsWith('node:')
  ) {
    return null;
  }

  if (
    typeof context?.parentURL === 'string' &&
    (context.parentURL.startsWith(BUILTIN_PREFIX) ||
      context.parentURL.startsWith(POLYFILL_PREFIX))
  ) {
    return null;
  }

  const parentPath = filePathFromUrl(context?.parentURL);
  if (parentPath && isAssetPath(parentPath)) {
    return null;
  }

  const normalized = specifier.slice('node:'.length);
  switch (normalized) {
    case 'fs':
      return assetModuleDescriptor(path.join(ASSET_ROOT, 'builtins', 'fs.mjs'));
    case 'fs/promises':
      return assetModuleDescriptor(
        path.join(ASSET_ROOT, 'builtins', 'fs-promises.mjs'),
      );
    case 'async_hooks':
      return assetModuleDescriptor(
        path.join(ASSET_ROOT, 'builtins', 'async-hooks.mjs'),
      );
    case 'child_process':
      return ALLOWED_BUILTINS.has('child_process')
        ? assetModuleDescriptor(path.join(ASSET_ROOT, 'builtins', 'child-process.mjs'))
        : null;
    case 'diagnostics_channel':
      return assetModuleDescriptor(
        path.join(ASSET_ROOT, 'builtins', 'diagnostics-channel.mjs'),
      );
    case 'net':
      return ALLOWED_BUILTINS.has('net')
        ? assetModuleDescriptor(path.join(ASSET_ROOT, 'builtins', 'net.mjs'))
        : null;
    case 'dgram':
      return ALLOWED_BUILTINS.has('dgram')
        ? assetModuleDescriptor(path.join(ASSET_ROOT, 'builtins', 'dgram.mjs'))
        : null;
    case 'dns':
      return ALLOWED_BUILTINS.has('dns')
        ? assetModuleDescriptor(path.join(ASSET_ROOT, 'builtins', 'dns.mjs'))
        : null;
    case 'dns/promises':
      return ALLOWED_BUILTINS.has('dns')
        ? assetModuleDescriptor(path.join(ASSET_ROOT, 'builtins', 'dns-promises.mjs'))
        : null;
    case 'http':
      return ALLOWED_BUILTINS.has('http')
        ? assetModuleDescriptor(path.join(ASSET_ROOT, 'builtins', 'http.mjs'))
        : null;
    case 'http2':
      return ALLOWED_BUILTINS.has('http2')
        ? assetModuleDescriptor(path.join(ASSET_ROOT, 'builtins', 'http2.mjs'))
        : null;
    case 'https':
      return ALLOWED_BUILTINS.has('https')
        ? assetModuleDescriptor(path.join(ASSET_ROOT, 'builtins', 'https.mjs'))
        : null;
    case 'tls':
      return ALLOWED_BUILTINS.has('tls')
        ? assetModuleDescriptor(path.join(ASSET_ROOT, 'builtins', 'tls.mjs'))
        : null;
    case 'os':
      return ALLOWED_BUILTINS.has('os')
        ? assetModuleDescriptor(path.join(ASSET_ROOT, 'builtins', 'os.mjs'))
        : null;
    default:
      return null;
  }
}

function assetModuleDescriptor(filePath) {
  if (!statForPath(filePath)) {
    return null;
  }

  return {
    filePath,
    url: pathToFileURL(filePath).href,
  };
}

function sanitizeAssetName(name) {
  return String(name).replace(/[^A-Za-z0-9_.-]+/g, '-');
}

function escapeRegExp(value) {
  return String(value).replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

function buildResolutionEntry(specifier, context, resolved) {
  const format = lookupModuleFormat(resolved.url) ?? resolved.format;

  if (resolved.url.startsWith('node:')) {
    return {
      kind: 'builtin',
      resolvedUrl: resolved.url,
      format,
    };
  }

  if (isBareSpecifier(specifier)) {
    const packageName = barePackageName(specifier);
    if (!packageName) {
      return null;
    }

    const candidatePackageJsonPaths = barePackageJsonCandidates(
      context.parentURL,
      packageName,
    );
    const selectedPackageJsonPath = firstExistingPath(candidatePackageJsonPaths);
    return {
      kind: 'bare',
      resolvedUrl: resolved.url,
      format,
      candidatePackageJsonPaths,
      selectedPackageJsonPath,
      selectedPackageJsonFingerprint: selectedPackageJsonPath
        ? fileFingerprint(selectedPackageJsonPath)
        : null,
    };
  }

  if (isExplicitFileLikeSpecifier(specifier)) {
    return {
      kind: 'explicit-file',
      resolvedUrl: resolved.url,
      format,
      resolvedFilePath: filePathFromUrl(resolved.url),
    };
  }

  return null;
}

function isProjectedPackageSource(filePath) {
  if (typeof filePath !== 'string' || isAssetPath(filePath)) {
    return false;
  }

  const guestPath = guestPathFromHostPath(filePath);
  return typeof guestPath === 'string' && guestPath.includes('/node_modules/');
}

function validateResolutionEntry(entry) {
  if (!isRecord(entry) || typeof entry.kind !== 'string') {
    return false;
  }

  switch (entry.kind) {
    case 'builtin':
      return true;
    case 'bare': {
      if (!Array.isArray(entry.candidatePackageJsonPaths)) {
        return false;
      }

      const currentPackageJsonPath = firstExistingPath(
        entry.candidatePackageJsonPaths,
      );
      if (currentPackageJsonPath !== entry.selectedPackageJsonPath) {
        return false;
      }

      if (
        currentPackageJsonPath &&
        !fingerprintMatches(
          currentPackageJsonPath,
          entry.selectedPackageJsonFingerprint,
        )
      ) {
        return false;
      }

      return formatMatches(entry.resolvedUrl, entry.format);
    }
    case 'explicit-file':
      if (
        typeof entry.resolvedFilePath !== 'string' ||
        !fs.existsSync(entry.resolvedFilePath)
      ) {
        return false;
      }

      return formatMatches(entry.resolvedUrl, entry.format);
    default:
      return false;
  }
}

function formatMatches(url, expectedFormat) {
  if (expectedFormat == null) {
    return true;
  }

  return lookupModuleFormat(url) === expectedFormat;
}

function lookupModuleFormat(url) {
  const cached = cacheState.moduleFormats[url];
  if (cached && validateModuleFormatEntry(cached)) {
    metrics.moduleFormatHits += 1;
    return cached.format;
  }

  metrics.moduleFormatMisses += 1;
  const entry = buildModuleFormatEntry(url);
  if (!entry) {
    return null;
  }

  cacheState.moduleFormats[url] = entry;
  dirty = true;
  return entry.format;
}

function buildModuleFormatEntry(url) {
  if (url.startsWith('node:')) {
    return {
      kind: 'builtin',
      url,
      format: 'builtin',
    };
  }

  const filePath = filePathFromUrl(url);
  if (!filePath) {
    return null;
  }

  const stat = statForPath(filePath);
  if (!stat) {
    return null;
  }

  const extension = path.extname(filePath);
  if (extension === '.mjs') {
    return createFileFormatEntry(url, filePath, stat, 'module', false);
  }
  if (extension === '.cjs') {
    return createFileFormatEntry(url, filePath, stat, 'commonjs', false);
  }
  if (extension === '.json') {
    return createFileFormatEntry(url, filePath, stat, 'json', false);
  }
  if (extension === '.wasm') {
    return createFileFormatEntry(url, filePath, stat, 'wasm', false);
  }
  if (extension === '.js' || extension === '') {
    const packageType = lookupPackageType(filePath);
    return createFileFormatEntry(
      url,
      filePath,
      stat,
      packageType === 'module' ? 'module' : 'commonjs',
      true,
    );
  }

  return null;
}

function createFileFormatEntry(url, filePath, stat, format, usesPackageType) {
  return {
    kind: 'file',
    url,
    filePath,
    format,
    usesPackageType,
    size: stat.size,
    mtimeMs: stat.mtimeMs,
  };
}

function validateModuleFormatEntry(entry) {
  if (!isRecord(entry) || typeof entry.kind !== 'string') {
    return false;
  }

  if (entry.kind === 'builtin') {
    return true;
  }

  if (entry.kind !== 'file' || typeof entry.filePath !== 'string') {
    return false;
  }

  const stat = statForPath(entry.filePath);
  if (!stat || stat.size !== entry.size || stat.mtimeMs !== entry.mtimeMs) {
    return false;
  }

  if (entry.usesPackageType) {
    const packageType = lookupPackageType(entry.filePath);
    const expectedFormat = packageType === 'module' ? 'module' : 'commonjs';
    return entry.format === expectedFormat;
  }

  return true;
}

function validateProjectedSourceEntry(entry, filePath, format) {
  if (
    !isRecord(entry) ||
    entry.kind !== 'text' ||
    typeof entry.filePath !== 'string' ||
    typeof entry.cachedPath !== 'string' ||
    typeof entry.format !== 'string'
  ) {
    return false;
  }

  if (entry.filePath !== filePath || entry.format !== format) {
    return false;
  }

  const stat = statForPath(filePath);
  if (!stat || stat.size !== entry.size || stat.mtimeMs !== entry.mtimeMs) {
    return false;
  }

  return statForPath(entry.cachedPath)?.isFile() ?? false;
}

function lookupPackageType(filePath) {
  let directory = path.dirname(filePath);

  while (true) {
    const packageJsonPath = path.join(directory, 'package.json');
    const cached = cacheState.packageTypes[packageJsonPath];
    if (cached && validatePackageTypeEntry(cached)) {
      metrics.packageTypeHits += 1;
      if (cached.kind === 'present') {
        return cached.packageType;
      }
    } else {
      metrics.packageTypeMisses += 1;
      const entry = buildPackageTypeEntry(packageJsonPath);
      cacheState.packageTypes[packageJsonPath] = entry;
      dirty = true;
      if (entry.kind === 'present') {
        return entry.packageType;
      }
    }

    const parent = path.dirname(directory);
    if (parent === directory) {
      break;
    }
    directory = parent;
  }

  return 'commonjs';
}

function buildPackageTypeEntry(packageJsonPath) {
  const stat = statForPath(packageJsonPath);
  if (!stat) {
    return {
      kind: 'missing',
      packageJsonPath,
    };
  }

  const contents = fs.readFileSync(packageJsonPath, 'utf8');
  let packageType = 'commonjs';
  try {
    const parsed = JSON.parse(contents);
    if (parsed && parsed.type === 'module') {
      packageType = 'module';
    }
  } catch {
    packageType = 'commonjs';
  }

  return {
    kind: 'present',
    packageJsonPath,
    packageType,
    size: stat.size,
    mtimeMs: stat.mtimeMs,
    hash: hashString(contents),
  };
}

function validatePackageTypeEntry(entry) {
  if (!isRecord(entry) || typeof entry.kind !== 'string') {
    return false;
  }

  if (entry.kind === 'missing') {
    return statForPath(entry.packageJsonPath) == null;
  }

  if (entry.kind !== 'present') {
    return false;
  }

  const stat = statForPath(entry.packageJsonPath);
  if (!stat) {
    return false;
  }

  if (stat.size !== entry.size || stat.mtimeMs !== entry.mtimeMs) {
    return false;
  }

  const contents = fs.readFileSync(entry.packageJsonPath, 'utf8');
  return hashString(contents) === entry.hash;
}

function fileFingerprint(filePath) {
  const stat = statForPath(filePath);
  if (!stat) {
    return null;
  }

  const contents = fs.readFileSync(filePath, 'utf8');
  return {
    size: stat.size,
    mtimeMs: stat.mtimeMs,
    hash: hashString(contents),
  };
}

function fingerprintMatches(filePath, expectedFingerprint) {
  if (!isRecord(expectedFingerprint)) {
    return false;
  }

  const stat = statForPath(filePath);
  if (!stat) {
    return false;
  }

  if (
    stat.size !== expectedFingerprint.size ||
    stat.mtimeMs !== expectedFingerprint.mtimeMs
  ) {
    return false;
  }

  const contents = fs.readFileSync(filePath, 'utf8');
  return hashString(contents) === expectedFingerprint.hash;
}

function barePackageJsonCandidates(parentURL, packageName) {
  const parentPath = filePathFromUrl(parentURL);
  if (!parentPath) {
    return [];
  }

  let directory = path.dirname(parentPath);
  const candidates = [];

  while (true) {
    candidates.push(path.join(directory, 'node_modules', packageName, 'package.json'));
    const parent = path.dirname(directory);
    if (parent === directory) {
      break;
    }
    directory = parent;
  }

  return candidates;
}

function firstExistingPath(paths) {
  for (const candidate of paths) {
    if (statForPath(candidate)) {
      return candidate;
    }
  }

  return null;
}

function statForPath(filePath) {
  try {
    return fs.statSync(filePath);
  } catch {
    return null;
  }
}

function createResolutionKey(specifier, context) {
  return JSON.stringify({
    specifier,
    parentURL: context.parentURL ?? null,
    conditions: Array.isArray(context.conditions)
      ? [...context.conditions].sort()
      : [],
    importAttributes: sortObject(context.importAttributes ?? {}),
  });
}

function sortObject(value) {
  if (Array.isArray(value)) {
    return value.map((item) => sortObject(item));
  }

  if (isRecord(value)) {
    return Object.fromEntries(
      Object.keys(value)
        .sort()
        .map((key) => [key, sortObject(value[key])]),
    );
  }

  return value;
}

function isExplicitFileLikeSpecifier(specifier) {
  if (typeof specifier !== 'string') {
    return false;
  }

  if (specifier.startsWith('file:')) {
    const filePath = filePathFromUrl(specifier);
    return Boolean(filePath && path.extname(filePath));
  }

  if (
    specifier.startsWith('./') ||
    specifier.startsWith('../') ||
    specifier.startsWith('/')
  ) {
    return Boolean(path.extname(specifier));
  }

  return false;
}

function isBareSpecifier(specifier) {
  if (typeof specifier !== 'string') {
    return false;
  }

  if (
    specifier.startsWith('./') ||
    specifier.startsWith('../') ||
    specifier.startsWith('/') ||
    specifier.startsWith('file:') ||
    specifier.startsWith('node:')
  ) {
    return false;
  }

  return !/^[A-Za-z][A-Za-z0-9+.-]*:/.test(specifier);
}

function barePackageName(specifier) {
  if (!isBareSpecifier(specifier)) {
    return null;
  }

  const parts = specifier.split('/');
  if (specifier.startsWith('@')) {
    return parts.length >= 2 ? `${parts[0]}/${parts[1]}` : null;
  }

  return parts[0] ?? null;
}

function resolveGuestSpecifier(specifier, context) {
  if (typeof specifier !== 'string') {
    return null;
  }

  if (specifier.startsWith('file:')) {
    const filePath = guestFilePathFromUrl(specifier);
    if (!filePath) {
      return null;
    }
    if (isInternalImportCachePath(filePath)) {
      return null;
    }
    if (pathExists(filePath) && !guestPathFromHostPath(filePath)) {
      return null;
    }
    return filePath;
  }

  if (specifier.startsWith('/')) {
    if (isInternalImportCachePath(specifier)) {
      return null;
    }
    if (pathExists(specifier)) {
      return null;
    }
    return path.posix.normalize(specifier);
  }

  if (!specifier.startsWith('./') && !specifier.startsWith('../')) {
    return null;
  }

  const parentPath = guestFilePathFromUrl(context.parentURL);
  if (!parentPath) {
    return null;
  }

  return path.posix.normalize(
    path.posix.join(path.posix.dirname(parentPath), specifier),
  );
}

function translateContextParentUrl(context) {
  if (!context || typeof context.parentURL !== 'string') {
    return context;
  }

  const hostParentUrl = translateResolvedUrlToHost(context.parentURL);
  const hostParentPath = guestFilePathFromUrl(hostParentUrl);
  const realParentPath =
    hostParentPath && pathExists(hostParentPath) ? safeRealpath(hostParentPath) : null;
  const normalizedParentUrl = realParentPath
    ? pathToFileURL(realParentPath).href
    : hostParentUrl;

  if (normalizedParentUrl === context.parentURL) {
    return context;
  }

  return {
    ...context,
    parentURL: normalizedParentUrl,
  };
}

function translateResolvedUrlToGuest(url) {
  const hostPath = guestFilePathFromUrl(url);
  if (!hostPath) {
    return url;
  }

  return pathToFileURL(guestVisiblePathFromHostPath(hostPath)).href;
}

function translateResolvedUrlToHost(url) {
  const guestPath = guestFilePathFromUrl(url);
  if (!guestPath) {
    return url;
  }

  if (pathExists(guestPath) && !guestPathFromHostPath(guestPath)) {
    return url;
  }

  const hostPath = hostPathFromGuestPath(guestPath);
  return hostPath ? pathToFileURL(hostPath).href : url;
}

function filePathFromUrl(url) {
  const guestPath = guestFilePathFromUrl(url);
  if (!guestPath) {
    return null;
  }

  if (pathExists(guestPath)) {
    return guestPath;
  }

  return hostPathFromGuestPath(guestPath) ?? guestPath;
}

function guestFilePathFromUrl(url) {
  if (typeof url !== 'string' || !url.startsWith('file:')) {
    return null;
  }

  try {
    return fileURLToPath(url);
  } catch {
    return null;
  }
}

function hostPathFromGuestPath(guestPath) {
  if (typeof guestPath !== 'string') {
    return null;
  }

  const normalized = path.posix.normalize(guestPath);
  if (
    CACHE_ROOT &&
    (normalized === GUEST_INTERNAL_CACHE_ROOT ||
      normalized.startsWith(`${GUEST_INTERNAL_CACHE_ROOT}/`))
  ) {
    const suffix =
      normalized === GUEST_INTERNAL_CACHE_ROOT
        ? ''
        : normalized.slice(GUEST_INTERNAL_CACHE_ROOT.length + 1);
    return suffix ? path.join(CACHE_ROOT, ...suffix.split('/')) : CACHE_ROOT;
  }

  for (const mapping of GUEST_PATH_MAPPINGS) {
    if (mapping.guestPath === '/') {
      const suffix = normalized.replace(/^\/+/, '');
      return suffix ? path.join(mapping.hostPath, suffix) : mapping.hostPath;
    }

    if (
      normalized !== mapping.guestPath &&
      !normalized.startsWith(`${mapping.guestPath}/`)
    ) {
      continue;
    }

    const suffix =
      normalized === mapping.guestPath
        ? ''
        : normalized.slice(mapping.guestPath.length + 1);
    return suffix ? path.join(mapping.hostPath, suffix) : mapping.hostPath;
  }

  if (
    normalized === DEFAULT_GUEST_CWD ||
    normalized.startsWith(`${DEFAULT_GUEST_CWD}/`)
  ) {
    const suffix =
      normalized === DEFAULT_GUEST_CWD
        ? ''
        : normalized.slice(DEFAULT_GUEST_CWD.length + 1);
    return suffix ? path.join(HOST_CWD, ...suffix.split('/')) : HOST_CWD;
  }

  return null;
}

function guestPathFromHostPath(hostPath) {
  if (typeof hostPath !== 'string') {
    return null;
  }

  const normalized = path.resolve(hostPath);
  if (isInternalImportCachePath(normalized)) {
    return null;
  }
  for (const mapping of GUEST_PATH_MAPPINGS) {
    const hostRoot = path.resolve(mapping.hostPath);
    if (
      normalized !== hostRoot &&
      !normalized.startsWith(`${hostRoot}${path.sep}`)
    ) {
      continue;
    }

    const suffix =
      normalized === hostRoot
        ? ''
        : normalized.slice(hostRoot.length + path.sep.length);
    return suffix
      ? path.posix.join(mapping.guestPath, suffix.split(path.sep).join('/'))
      : mapping.guestPath;
  }

  return null;
}

function guestCwdPathFromHostPath(hostPath) {
  if (typeof hostPath !== 'string') {
    return null;
  }

  const normalized = path.resolve(hostPath);
  const hostRoot = path.resolve(HOST_CWD);
  if (
    normalized !== hostRoot &&
    !normalized.startsWith(`${hostRoot}${path.sep}`)
  ) {
    return null;
  }

  const suffix =
    normalized === hostRoot
      ? ''
      : normalized.slice(hostRoot.length + path.sep.length);
  return suffix
    ? path.posix.join(DEFAULT_GUEST_CWD, suffix.split(path.sep).join('/'))
    : DEFAULT_GUEST_CWD;
}

function guestInternalPathFromHostPath(hostPath) {
  if (typeof hostPath !== 'string' || !CACHE_ROOT) {
    return null;
  }

  const normalized = path.resolve(hostPath);
  const hostRoot = path.resolve(CACHE_ROOT);
  if (
    normalized !== hostRoot &&
    !normalized.startsWith(`${hostRoot}${path.sep}`)
  ) {
    return null;
  }

  const suffix =
    normalized === hostRoot
      ? ''
      : normalized.slice(hostRoot.length + path.sep.length);
  return suffix
    ? path.posix.join(GUEST_INTERNAL_CACHE_ROOT, suffix.split(path.sep).join('/'))
    : GUEST_INTERNAL_CACHE_ROOT;
}

function guestVisiblePathFromHostPath(hostPath) {
  return (
    guestPathFromHostPath(hostPath) ??
    guestInternalPathFromHostPath(hostPath) ??
    guestCwdPathFromHostPath(hostPath) ??
    UNMAPPED_GUEST_PATH
  );
}

function isGuestVisiblePath(value) {
  if (typeof value !== 'string' || !path.posix.isAbsolute(value)) {
    return false;
  }

  const normalized = path.posix.normalize(value);
  return (
    normalized === UNMAPPED_GUEST_PATH ||
    normalized === GUEST_INTERNAL_CACHE_ROOT ||
    normalized.startsWith(`${GUEST_INTERNAL_CACHE_ROOT}/`) ||
    normalized === DEFAULT_GUEST_CWD ||
    normalized.startsWith(`${DEFAULT_GUEST_CWD}/`) ||
    hostPathFromGuestPath(normalized) != null
  );
}

function translatePathStringToGuest(value) {
  if (typeof value !== 'string') {
    return value;
  }

  if (value.startsWith('file:')) {
    const hostPath = guestFilePathFromUrl(value);
    if (!hostPath) {
      return value;
    }

    const guestPath = isGuestVisiblePath(hostPath)
      ? path.posix.normalize(hostPath)
      : guestVisiblePathFromHostPath(hostPath);
    return pathToFileURL(guestPath).href;
  }

  if (!path.isAbsolute(value)) {
    return value;
  }

  return isGuestVisiblePath(value)
    ? path.posix.normalize(value)
    : guestVisiblePathFromHostPath(value);
}

function buildHostToGuestTextReplacements() {
  const replacements = new Map();
  const addReplacement = (hostValue, guestValue) => {
    if (
      typeof hostValue !== 'string' ||
      hostValue.length === 0 ||
      typeof guestValue !== 'string' ||
      guestValue.length === 0
    ) {
      return;
    }

    replacements.set(hostValue, guestValue);
  };

  for (const mapping of GUEST_PATH_MAPPINGS) {
    const hostRoot = path.resolve(mapping.hostPath);
    addReplacement(hostRoot, mapping.guestPath);
    addReplacement(pathToFileURL(hostRoot).href, pathToFileURL(mapping.guestPath).href);
    const forwardSlashHostRoot = hostRoot.split(path.sep).join('/');
    if (forwardSlashHostRoot !== hostRoot) {
      addReplacement(forwardSlashHostRoot, mapping.guestPath);
    }
  }

  if (CACHE_ROOT) {
    const hostRoot = path.resolve(CACHE_ROOT);
    addReplacement(hostRoot, GUEST_INTERNAL_CACHE_ROOT);
    addReplacement(
      pathToFileURL(hostRoot).href,
      pathToFileURL(GUEST_INTERNAL_CACHE_ROOT).href,
    );
    const forwardSlashHostRoot = hostRoot.split(path.sep).join('/');
    if (forwardSlashHostRoot !== hostRoot) {
      addReplacement(forwardSlashHostRoot, GUEST_INTERNAL_CACHE_ROOT);
    }
  }

  if (!guestPathFromHostPath(HOST_CWD)) {
    const hostRoot = path.resolve(HOST_CWD);
    addReplacement(hostRoot, DEFAULT_GUEST_CWD);
    addReplacement(pathToFileURL(hostRoot).href, pathToFileURL(DEFAULT_GUEST_CWD).href);
    const forwardSlashHostRoot = hostRoot.split(path.sep).join('/');
    if (forwardSlashHostRoot !== hostRoot) {
      addReplacement(forwardSlashHostRoot, DEFAULT_GUEST_CWD);
    }
  }

  return [...replacements.entries()].sort((left, right) => right[0].length - left[0].length);
}

function splitPathLocationSuffix(value) {
  if (typeof value !== 'string') {
    return { pathLike: value, suffix: '' };
  }

  const match = /^(.*?)(:\d+(?::\d+)?)$/.exec(value);
  return match
    ? { pathLike: match[1], suffix: match[2] }
    : { pathLike: value, suffix: '' };
}

function translateTextTokenToGuest(token) {
  if (typeof token !== 'string' || token.length === 0) {
    return token;
  }

  const leading = token.match(/^[("'`[{<]+/)?.[0] ?? '';
  const trailing = token.match(/[)"'`\]}>.,;!?]+$/)?.[0] ?? '';
  const coreEnd = token.length - trailing.length;
  const core = token.slice(leading.length, coreEnd);
  if (core.length === 0) {
    return token;
  }

  const { pathLike, suffix } = splitPathLocationSuffix(core);
  if (
    typeof pathLike !== 'string' ||
    (!pathLike.startsWith('file:') && !path.isAbsolute(pathLike))
  ) {
    return token;
  }

  return `${leading}${translatePathStringToGuest(pathLike)}${suffix}${trailing}`;
}

function translateTextToGuest(value) {
  if (typeof value !== 'string' || value.length === 0) {
    return value;
  }

  let translated = value;
  for (const [hostValue, guestValue] of buildHostToGuestTextReplacements()) {
    translated = translated.split(hostValue).join(guestValue);
  }

  return translated
    .split(/(\s+)/)
    .map((token) => (/^\s+$/.test(token) ? token : translateTextTokenToGuest(token)))
    .join('');
}

function translateErrorToGuest(error) {
  if (error == null || typeof error !== 'object') {
    return error;
  }

  if (typeof error.message === 'string') {
    try {
      error.message = translateTextToGuest(error.message);
    } catch {
      // Ignore readonly message bindings.
    }
  }

  if (typeof error.stack === 'string') {
    try {
      error.stack = translateTextToGuest(error.stack);
    } catch {
      // Ignore readonly stack bindings.
    }
  }

  if (typeof error.path === 'string') {
    try {
      error.path = translatePathStringToGuest(error.path);
    } catch {
      // Ignore readonly path bindings.
    }
  }

  if (typeof error.filename === 'string') {
    try {
      error.filename = translatePathStringToGuest(error.filename);
    } catch {
      // Ignore readonly filename bindings.
    }
  }

  if (typeof error.url === 'string') {
    try {
      error.url = translatePathStringToGuest(error.url);
    } catch {
      // Ignore readonly url bindings.
    }
  }

  if (Array.isArray(error.requireStack)) {
    try {
      error.requireStack = error.requireStack.map((entry) => translatePathStringToGuest(entry));
    } catch {
      // Ignore readonly requireStack bindings.
    }
  }

  return error;
}

function pathExists(targetPath) {
  try {
    return fs.existsSync(targetPath);
  } catch {
    return false;
  }
}

function safeRealpath(targetPath) {
  try {
    return fs.realpathSync.native(targetPath);
  } catch {
    return null;
  }
}

function parseJsonArray(value) {
  if (!value) {
    return [];
  }

  try {
    const parsed = JSON.parse(value);
    return Array.isArray(parsed) ? parsed.filter((entry) => typeof entry === 'string') : [];
  } catch {
    return [];
  }
}

function isInternalImportCachePath(filePath) {
  return typeof filePath === 'string' && filePath.includes(`${path.sep}agentos-node-import-cache-`);
}

function parseGuestPathMappings(value) {
  const parsed = parseJsonArrayLikeObjects(value);
  return parsed
    .map((entry) => {
      const guestPath =
        typeof entry.guestPath === 'string'
          ? path.posix.normalize(entry.guestPath)
          : null;
      const hostPath =
        typeof entry.hostPath === 'string' ? path.resolve(entry.hostPath) : null;
      return guestPath && hostPath ? { guestPath, hostPath } : null;
    })
    .filter(Boolean)
    .sort((left, right) => {
      if (right.guestPath.length !== left.guestPath.length) {
        return right.guestPath.length - left.guestPath.length;
      }
      return right.hostPath.length - left.hostPath.length;
    });
}

function parseJsonArrayLikeObjects(value) {
  if (!value) {
    return [];
  }

  try {
    const parsed = JSON.parse(value);
    return Array.isArray(parsed) ? parsed.filter(isRecord) : [];
  } catch {
    return [];
  }
}

function hashString(contents) {
  return crypto.createHash('sha256').update(contents).digest('hex');
}

function isRecord(value) {
  return value != null && typeof value === 'object' && !Array.isArray(value);
}
"#;

const NODE_IMPORT_CACHE_REGISTER_SOURCE: &str = r#"
import { register } from 'node:module';

const loaderPath = process.env.__NODE_IMPORT_CACHE_LOADER_PATH_ENV__;

if (!loaderPath) {
  throw new Error('__NODE_IMPORT_CACHE_LOADER_PATH_ENV__ is required');
}

register(loaderPath, import.meta.url);
"#;

const NODE_TIMING_BOOTSTRAP_SOURCE: &str = r#"
const frozenTimeValue = Number(process.env.AGENTOS_FROZEN_TIME_MS);
const frozenTimeMs = Number.isFinite(frozenTimeValue) ? Math.trunc(frozenTimeValue) : Date.now();
const frozenDateNow = () => frozenTimeMs;
const OriginalDate = Date;

function FrozenDate(...args) {
  if (new.target) {
    if (args.length === 0) {
      return new OriginalDate(frozenTimeMs);
    }
    return new OriginalDate(...args);
  }
  return new OriginalDate(frozenTimeMs).toString();
}

Object.setPrototypeOf(FrozenDate, OriginalDate);
Object.defineProperty(FrozenDate, 'prototype', {
  value: OriginalDate.prototype,
  writable: false,
  configurable: false,
});
FrozenDate.parse = OriginalDate.parse;
FrozenDate.UTC = OriginalDate.UTC;
Object.defineProperty(FrozenDate, 'now', {
  value: frozenDateNow,
  writable: false,
  configurable: false,
});

try {
  Object.defineProperty(globalThis, 'Date', {
    value: FrozenDate,
    writable: false,
    configurable: false,
  });
} catch {
  globalThis.Date = FrozenDate;
}

const originalPerformance = globalThis.performance;
const frozenPerformance = Object.create(null);
if (typeof originalPerformance !== 'undefined' && originalPerformance !== null) {
  const performanceSource =
    Object.getPrototypeOf(originalPerformance) ?? originalPerformance;
  for (const key of Object.getOwnPropertyNames(performanceSource)) {
    if (key === 'now') {
      continue;
    }
    try {
      const value = originalPerformance[key];
      frozenPerformance[key] =
        typeof value === 'function' ? value.bind(originalPerformance) : value;
    } catch {
      // Ignore properties that throw during access.
    }
  }
}
Object.defineProperty(frozenPerformance, 'now', {
  value: () => 0,
  writable: false,
  configurable: false,
});
Object.freeze(frozenPerformance);

try {
  Object.defineProperty(globalThis, 'performance', {
    value: frozenPerformance,
    writable: false,
    configurable: false,
  });
} catch {
  globalThis.performance = frozenPerformance;
}

const frozenHrtimeBigint = BigInt(frozenTimeMs) * 1000000n;
const frozenHrtime = (previous) => {
  const seconds = Math.trunc(frozenTimeMs / 1000);
  const nanoseconds = Math.trunc((frozenTimeMs % 1000) * 1000000);

  if (!Array.isArray(previous) || previous.length < 2) {
    return [seconds, nanoseconds];
  }

  let deltaSeconds = seconds - Number(previous[0]);
  let deltaNanoseconds = nanoseconds - Number(previous[1]);
  if (deltaNanoseconds < 0) {
    deltaSeconds -= 1;
    deltaNanoseconds += 1000000000;
  }
  return [deltaSeconds, deltaNanoseconds];
};
frozenHrtime.bigint = () => frozenHrtimeBigint;

try {
  process.hrtime = frozenHrtime;
} catch {
  // Ignore runtimes that expose a non-writable process.hrtime binding.
}
"#;

const NODE_PREWARM_SOURCE: &str = r#"
import path from 'node:path';
import { pathToFileURL } from 'node:url';

function isPathLike(specifier) {
  return specifier.startsWith('.') || specifier.startsWith('/') || specifier.startsWith('file:');
}

function toImportSpecifier(specifier) {
  if (specifier.startsWith('file:')) {
    return specifier;
  }
  if (isPathLike(specifier)) {
    return pathToFileURL(path.resolve(process.cwd(), specifier)).href;
  }
  return specifier;
}

const imports = JSON.parse(process.env.AGENTOS_NODE_PREWARM_IMPORTS ?? '[]');
for (const specifier of imports) {
  await import(toImportSpecifier(specifier));
}
"#;

const NODE_WASM_RUNNER_SOURCE: &str = include_str!("../assets/runners/wasm-runner.mjs");

static NEXT_NODE_IMPORT_CACHE_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy)]
struct BuiltinAsset {
    name: &'static str,
    module_specifier: &'static str,
    init_counter_key: &'static str,
}

#[derive(Clone, Copy)]
struct DeniedBuiltinAsset {
    name: &'static str,
    module_specifier: &'static str,
}

const BUILTIN_ASSETS: &[BuiltinAsset] = &[
    BuiltinAsset {
        name: "async-hooks",
        module_specifier: "node:async_hooks",
        init_counter_key: "__agentOSBuiltinAsyncHooksInitCount",
    },
    BuiltinAsset {
        name: "assert",
        module_specifier: "node:assert",
        init_counter_key: "__agentOSBuiltinAssertInitCount",
    },
    BuiltinAsset {
        name: "buffer",
        module_specifier: "node:buffer",
        init_counter_key: "__agentOSBuiltinBufferInitCount",
    },
    BuiltinAsset {
        name: "constants",
        module_specifier: "node:constants",
        init_counter_key: "__agentOSBuiltinConstantsInitCount",
    },
    BuiltinAsset {
        name: "events",
        module_specifier: "node:events",
        init_counter_key: "__agentOSBuiltinEventsInitCount",
    },
    BuiltinAsset {
        name: "fs",
        module_specifier: "node:fs",
        init_counter_key: "__agentOSBuiltinFsInitCount",
    },
    BuiltinAsset {
        name: "path",
        module_specifier: "node:path",
        init_counter_key: "__agentOSBuiltinPathInitCount",
    },
    BuiltinAsset {
        name: "url",
        module_specifier: "node:url",
        init_counter_key: "__agentOSBuiltinUrlInitCount",
    },
    BuiltinAsset {
        name: "fs-promises",
        module_specifier: "node:fs/promises",
        init_counter_key: "__agentOSBuiltinFsPromisesInitCount",
    },
    BuiltinAsset {
        name: "child-process",
        module_specifier: "node:child_process",
        init_counter_key: "__agentOSBuiltinChildProcessInitCount",
    },
    BuiltinAsset {
        name: "net",
        module_specifier: "node:net",
        init_counter_key: "__agentOSBuiltinNetInitCount",
    },
    BuiltinAsset {
        name: "dgram",
        module_specifier: "node:dgram",
        init_counter_key: "__agentOSBuiltinDgramInitCount",
    },
    BuiltinAsset {
        name: "diagnostics-channel",
        module_specifier: "node:diagnostics_channel",
        init_counter_key: "__agentOSBuiltinDiagnosticsChannelInitCount",
    },
    BuiltinAsset {
        name: "dns",
        module_specifier: "node:dns",
        init_counter_key: "__agentOSBuiltinDnsInitCount",
    },
    BuiltinAsset {
        name: "dns-promises",
        module_specifier: "node:dns/promises",
        init_counter_key: "__agentOSBuiltinDnsPromisesInitCount",
    },
    BuiltinAsset {
        name: "http",
        module_specifier: "node:http",
        init_counter_key: "__agentOSBuiltinHttpInitCount",
    },
    BuiltinAsset {
        name: "http2",
        module_specifier: "node:http2",
        init_counter_key: "__agentOSBuiltinHttp2InitCount",
    },
    BuiltinAsset {
        name: "https",
        module_specifier: "node:https",
        init_counter_key: "__agentOSBuiltinHttpsInitCount",
    },
    BuiltinAsset {
        name: "tls",
        module_specifier: "node:tls",
        init_counter_key: "__agentOSBuiltinTlsInitCount",
    },
    BuiltinAsset {
        name: "os",
        module_specifier: "node:os",
        init_counter_key: "__agentOSBuiltinOsInitCount",
    },
    BuiltinAsset {
        name: "punycode",
        module_specifier: "node:punycode",
        init_counter_key: "__agentOSBuiltinPunycodeInitCount",
    },
    BuiltinAsset {
        name: "querystring",
        module_specifier: "node:querystring",
        init_counter_key: "__agentOSBuiltinQuerystringInitCount",
    },
    BuiltinAsset {
        name: "stream",
        module_specifier: "node:stream",
        init_counter_key: "__agentOSBuiltinStreamInitCount",
    },
    BuiltinAsset {
        name: "string-decoder",
        module_specifier: "node:string_decoder",
        init_counter_key: "__agentOSBuiltinStringDecoderInitCount",
    },
    BuiltinAsset {
        name: "util",
        module_specifier: "node:util",
        init_counter_key: "__agentOSBuiltinUtilInitCount",
    },
    BuiltinAsset {
        name: "v8",
        module_specifier: "node:v8",
        init_counter_key: "__agentOSBuiltinV8InitCount",
    },
    BuiltinAsset {
        name: "vm",
        module_specifier: "node:vm",
        init_counter_key: "__agentOSBuiltinVmInitCount",
    },
    BuiltinAsset {
        name: "worker-threads",
        module_specifier: "node:worker_threads",
        init_counter_key: "__agentOSBuiltinWorkerThreadsInitCount",
    },
    BuiltinAsset {
        name: "zlib",
        module_specifier: "node:zlib",
        init_counter_key: "__agentOSBuiltinZlibInitCount",
    },
];

const DENIED_BUILTIN_ASSETS: &[DeniedBuiltinAsset] = &[
    DeniedBuiltinAsset {
        name: "child_process",
        module_specifier: "node:child_process",
    },
    DeniedBuiltinAsset {
        name: "cluster",
        module_specifier: "node:cluster",
    },
    DeniedBuiltinAsset {
        name: "dgram",
        module_specifier: "node:dgram",
    },
    DeniedBuiltinAsset {
        name: "http",
        module_specifier: "node:http",
    },
    DeniedBuiltinAsset {
        name: "http2",
        module_specifier: "node:http2",
    },
    DeniedBuiltinAsset {
        name: "https",
        module_specifier: "node:https",
    },
    DeniedBuiltinAsset {
        name: "inspector",
        module_specifier: "node:inspector",
    },
    DeniedBuiltinAsset {
        name: "module",
        module_specifier: "node:module",
    },
    DeniedBuiltinAsset {
        name: "net",
        module_specifier: "node:net",
    },
    DeniedBuiltinAsset {
        name: "trace_events",
        module_specifier: "node:trace_events",
    },
];

const PATH_POLYFILL_ASSET_NAME: &str = "path";
const PATH_POLYFILL_INIT_COUNTER_KEY: &str = "__agentOSPolyfillPathInitCount";

#[derive(Debug)]
pub(crate) struct NodeImportCache {
    root_dir: PathBuf,
    cleanup: Arc<NodeImportCacheCleanup>,
    materialized: AtomicBool,
    materialization_lock: Mutex<()>,
    async_materialization_lock: AsyncMutex<()>,
    materialization_marker_path: PathBuf,
    cache_path: PathBuf,
    loader_path: PathBuf,
    register_path: PathBuf,
    python_runner_path: PathBuf,
    timing_bootstrap_path: PathBuf,
    prewarm_path: PathBuf,
    wasm_runner_path: PathBuf,
    asset_root: PathBuf,
    pyodide_dist_path: PathBuf,
    prewarm_marker_dir: PathBuf,
}

#[derive(Debug)]
pub(crate) struct NodeImportCacheCleanup {
    root_dir: PathBuf,
}

#[derive(Debug, Clone)]
struct NodeImportCacheMaterialization {
    root_dir: PathBuf,
    materialization_marker_path: PathBuf,
    loader_path: PathBuf,
    register_path: PathBuf,
    python_runner_path: PathBuf,
    timing_bootstrap_path: PathBuf,
    prewarm_path: PathBuf,
    wasm_runner_path: PathBuf,
    asset_root: PathBuf,
    pyodide_dist_path: PathBuf,
    prewarm_marker_dir: PathBuf,
}

impl Default for NodeImportCache {
    fn default() -> Self {
        Self::new_in(default_node_import_cache_base_dir())
    }
}

fn default_node_import_cache_base_dir() -> PathBuf {
    env::temp_dir().join(format!(
        "{NODE_IMPORT_CACHE_DIR_PREFIX}-roots-{}",
        std::process::id()
    ))
}

fn cleanup_stale_node_import_caches_once(base_dir: &Path) {
    let cleaned_roots = CLEANED_NODE_IMPORT_CACHE_ROOTS.get_or_init(|| Mutex::new(BTreeSet::new()));
    let should_cleanup = cleaned_roots
        .lock()
        .map(|mut roots| roots.insert(base_dir.to_path_buf()))
        .unwrap_or(true);

    if should_cleanup {
        cleanup_stale_node_import_caches(base_dir);
    }
}

fn cleanup_stale_node_import_caches(base_dir: &Path) {
    let entries = match fs::read_dir(base_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return,
        Err(error) => {
            eprintln!(
                "agentos: failed to scan node import cache root {}: {error}",
                base_dir.display()
            );
            return;
        }
    };

    for entry in entries.flatten() {
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        if !file_type.is_dir() {
            continue;
        }

        let name = entry.file_name();
        if !name
            .to_str()
            .is_some_and(|name| name.starts_with(NODE_IMPORT_CACHE_DIR_PREFIX))
        {
            continue;
        }

        let path = entry.path();
        if let Err(error) = fs::remove_dir_all(&path) {
            if error.kind() != io::ErrorKind::NotFound {
                eprintln!(
                    "agentos: failed to clean up stale node import cache {}: {error}",
                    path.display()
                );
            }
        }
    }
}

impl NodeImportCache {
    pub(crate) fn new_in(base_dir: PathBuf) -> Self {
        cleanup_stale_node_import_caches_once(&base_dir);
        let cache_id = NEXT_NODE_IMPORT_CACHE_ID.fetch_add(1, Ordering::Relaxed);
        let root_dir = base_dir.join(format!(
            "{NODE_IMPORT_CACHE_DIR_PREFIX}-{}-{cache_id}",
            std::process::id()
        ));

        Self {
            root_dir: root_dir.clone(),
            cleanup: Arc::new(NodeImportCacheCleanup {
                root_dir: root_dir.clone(),
            }),
            materialized: AtomicBool::new(false),
            materialization_lock: Mutex::new(()),
            async_materialization_lock: AsyncMutex::new(()),
            materialization_marker_path: root_dir.join(".materialized"),
            cache_path: root_dir.join("state.json"),
            loader_path: root_dir.join("loader.mjs"),
            register_path: root_dir.join("register.mjs"),
            python_runner_path: root_dir.join("python-runner.mjs"),
            timing_bootstrap_path: root_dir.join("timing-bootstrap.mjs"),
            prewarm_path: root_dir.join("prewarm.mjs"),
            wasm_runner_path: root_dir.join("wasm-runner.mjs"),
            asset_root: root_dir.join("assets"),
            pyodide_dist_path: root_dir.join("assets").join(PYODIDE_DIST_DIR),
            prewarm_marker_dir: root_dir.join("warmup"),
        }
    }
}

impl Drop for NodeImportCacheCleanup {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.root_dir) {
            if error.kind() != io::ErrorKind::NotFound {
                eprintln!(
                    "agentos: failed to clean up node import cache {}: {error}",
                    self.root_dir.display()
                );
            }
        }
    }
}

impl NodeImportCache {
    pub(crate) fn cache_path(&self) -> &Path {
        &self.cache_path
    }

    pub(crate) fn cleanup_guard(&self) -> Arc<NodeImportCacheCleanup> {
        Arc::clone(&self.cleanup)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn python_runner_path(&self) -> &Path {
        &self.python_runner_path
    }

    #[cfg(test)]
    pub(crate) fn timing_bootstrap_path(&self) -> &Path {
        &self.timing_bootstrap_path
    }

    pub(crate) fn wasm_runner_path(&self) -> &Path {
        &self.wasm_runner_path
    }

    pub(crate) fn asset_root(&self) -> &Path {
        &self.asset_root
    }

    pub(crate) fn pyodide_dist_path(&self) -> &Path {
        &self.pyodide_dist_path
    }

    pub(crate) fn prewarm_marker_dir(&self) -> &Path {
        &self.prewarm_marker_dir
    }

    pub(crate) fn shared_compile_cache_dir(&self) -> PathBuf {
        self.root_dir.join("compile-cache")
    }

    pub(crate) fn ensure_materialized_with_runtime(
        &self,
        runtime: &RuntimeContext,
    ) -> Result<(), io::Error> {
        self.ensure_materialized_with_timeout_and_runtime(
            runtime,
            DEFAULT_NODE_IMPORT_CACHE_MATERIALIZE_TIMEOUT,
        )
    }

    pub(crate) fn ensure_materialized_with_timeout_and_runtime(
        &self,
        runtime: &RuntimeContext,
        timeout: Duration,
    ) -> Result<(), io::Error> {
        if self.is_materialized() {
            return Ok(());
        }

        let _materialization_guard = self
            .materialization_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.is_materialized() {
            return Ok(());
        }
        self.materialized.store(false, Ordering::Release);

        let materialization = NodeImportCacheMaterialization::from(self);
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let result = runtime.blocking().run_sync(
            NODE_IMPORT_CACHE_BLOCKING_JOB_RESERVATION_BYTES,
            timeout,
            move || materialization.materialize(worker_cancelled),
        );
        let result = result.map_err(|error| {
            cancelled.store(true, Ordering::Release);
            match error {
                agentos_runtime::BlockingJobError::TimedOut { .. } => io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "timed out materializing node import cache after {} ms",
                        timeout.as_millis()
                    ),
                ),
                other => io::Error::other(other.to_string()),
            }
        })?;
        result?;
        self.materialized.store(true, Ordering::Release);
        Ok(())
    }

    /// Materialize from an async sidecar path without blocking a Tokio worker.
    /// The fixed blocking executor owns the filesystem work; this future only
    /// holds an async single-flight guard and awaits its bounded completion.
    pub(crate) async fn ensure_materialized_with_timeout_and_runtime_async(
        &self,
        runtime: &RuntimeContext,
        timeout: Duration,
    ) -> Result<(), io::Error> {
        if self.is_materialized() {
            return Ok(());
        }

        let _materialization_guard = self.async_materialization_lock.lock().await;
        if self.is_materialized() {
            return Ok(());
        }
        self.materialized.store(false, Ordering::Release);

        let materialization = NodeImportCacheMaterialization::from(self);
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let job = runtime.blocking().run(
            NODE_IMPORT_CACHE_BLOCKING_JOB_RESERVATION_BYTES,
            move || materialization.materialize(worker_cancelled),
        );
        let result = match time::timeout(timeout, job).await {
            Ok(result) => result.map_err(|error| io::Error::other(error.to_string()))?,
            Err(_) => {
                cancelled.store(true, Ordering::Release);
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "timed out materializing node import cache after {} ms",
                        timeout.as_millis()
                    ),
                ));
            }
        };
        result?;
        self.materialized.store(true, Ordering::Release);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn ensure_materialized(&self) -> Result<(), io::Error> {
        let runtime =
            agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
                .map(agentos_runtime::SidecarRuntime::context)
                .map_err(|error| io::Error::other(error.to_string()))?;
        self.ensure_materialized_with_runtime(&runtime)
    }

    #[cfg(test)]
    pub(crate) fn ensure_materialized_with_timeout(
        &self,
        timeout: Duration,
    ) -> Result<(), io::Error> {
        let runtime =
            agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
                .map(agentos_runtime::SidecarRuntime::context)
                .map_err(|error| io::Error::other(error.to_string()))?;
        self.ensure_materialized_with_timeout_and_runtime(&runtime, timeout)
    }

    fn is_materialized(&self) -> bool {
        self.materialized.load(Ordering::Acquire) && self.materialization_marker_path.is_file()
    }
}

impl From<&NodeImportCache> for NodeImportCacheMaterialization {
    fn from(cache: &NodeImportCache) -> Self {
        Self {
            root_dir: cache.root_dir.clone(),
            materialization_marker_path: cache.materialization_marker_path.clone(),
            loader_path: cache.loader_path.clone(),
            register_path: cache.register_path.clone(),
            python_runner_path: cache.python_runner_path.clone(),
            timing_bootstrap_path: cache.timing_bootstrap_path.clone(),
            prewarm_path: cache.prewarm_path.clone(),
            wasm_runner_path: cache.wasm_runner_path.clone(),
            asset_root: cache.asset_root.clone(),
            pyodide_dist_path: cache.pyodide_dist_path.clone(),
            prewarm_marker_dir: cache.prewarm_marker_dir.clone(),
        }
    }
}

impl NodeImportCacheMaterialization {
    fn materialize(self, cancelled: Arc<AtomicBool>) -> Result<(), io::Error> {
        #[cfg(test)]
        {
            let delay_ms = NODE_IMPORT_CACHE_TEST_MATERIALIZE_DELAY_MS.load(Ordering::Relaxed);
            if delay_ms > 0 {
                std::thread::sleep(Duration::from_millis(delay_ms));
            }
        }

        check_materialization_cancelled(&cancelled)?;
        fs::create_dir_all(&self.root_dir)?;
        fs::create_dir_all(self.asset_root.join("builtins"))?;
        fs::create_dir_all(self.asset_root.join("denied"))?;
        fs::create_dir_all(self.asset_root.join("polyfills"))?;
        fs::create_dir_all(&self.pyodide_dist_path)?;
        fs::create_dir_all(&self.prewarm_marker_dir)?;

        write_file_if_changed(&self.loader_path, &render_loader_source())?;
        write_file_if_changed(&self.register_path, &render_register_source())?;
        write_file_if_changed(&self.python_runner_path, NODE_PYTHON_RUNNER_SOURCE)?;
        write_file_if_changed(&self.timing_bootstrap_path, NODE_TIMING_BOOTSTRAP_SOURCE)?;
        write_file_if_changed(&self.prewarm_path, NODE_PREWARM_SOURCE)?;
        write_file_if_changed(&self.wasm_runner_path, NODE_WASM_RUNNER_SOURCE)?;

        for asset in BUILTIN_ASSETS {
            write_file_if_changed(
                &self
                    .asset_root
                    .join("builtins")
                    .join(format!("{}.mjs", asset.name)),
                &render_builtin_asset_source(asset),
            )?;
        }

        for asset in DENIED_BUILTIN_ASSETS {
            write_file_if_changed(
                &self
                    .asset_root
                    .join("denied")
                    .join(format!("{}.mjs", asset.name)),
                &render_denied_asset_source(asset.module_specifier),
            )?;
        }

        write_file_if_changed(
            &self
                .asset_root
                .join("polyfills")
                .join(format!("{PATH_POLYFILL_ASSET_NAME}.mjs")),
            &render_path_polyfill_source(),
        )?;
        write_file_if_changed(
            &self.pyodide_dist_path.join("pyodide.mjs"),
            &render_patched_pyodide_mjs(),
        )?;
        write_bytes_if_changed(
            &self.pyodide_dist_path.join("pyodide.asm.js"),
            BUNDLED_PYODIDE_ASM_JS,
        )?;
        write_bytes_if_changed(
            &self.pyodide_dist_path.join("pyodide.asm.wasm"),
            BUNDLED_PYODIDE_ASM_WASM,
        )?;
        write_bytes_if_changed(
            &self.pyodide_dist_path.join("pyodide-lock.json"),
            BUNDLED_PYODIDE_LOCK,
        )?;
        write_bytes_if_changed(
            &self.pyodide_dist_path.join("python_stdlib.zip"),
            BUNDLED_PYTHON_STDLIB_ZIP,
        )?;
        for asset in BUNDLED_PYODIDE_PACKAGE_ASSETS {
            check_materialization_cancelled(&cancelled)?;
            write_bytes_if_changed(&self.pyodide_dist_path.join(asset.file_name), asset.bytes)?;
        }
        check_materialization_cancelled(&cancelled)?;
        write_file_if_changed(
            &self.materialization_marker_path,
            NODE_IMPORT_CACHE_ASSET_VERSION,
        )?;
        Ok(())
    }
}

fn check_materialization_cancelled(cancelled: &AtomicBool) -> Result<(), io::Error> {
    if cancelled.load(Ordering::Acquire) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "node import cache materialization cancelled",
        ));
    }
    Ok(())
}

fn render_loader_source() -> String {
    NODE_IMPORT_CACHE_LOADER_TEMPLATE
        .replace("__NODE_IMPORT_CACHE_PATH_ENV__", NODE_IMPORT_CACHE_PATH_ENV)
        .replace(
            "__NODE_IMPORT_CACHE_ASSET_ROOT_ENV__",
            NODE_IMPORT_CACHE_ASSET_ROOT_ENV,
        )
        .replace(
            "__NODE_IMPORT_CACHE_DEBUG_ENV__",
            NODE_IMPORT_CACHE_DEBUG_ENV,
        )
        .replace(
            "__NODE_IMPORT_CACHE_METRICS_PREFIX__",
            NODE_IMPORT_CACHE_METRICS_PREFIX,
        )
        .replace(
            "__NODE_IMPORT_CACHE_SCHEMA_VERSION__",
            NODE_IMPORT_CACHE_SCHEMA_VERSION,
        )
        .replace(
            "__NODE_IMPORT_CACHE_LOADER_VERSION__",
            NODE_IMPORT_CACHE_LOADER_VERSION,
        )
        .replace(
            "__NODE_IMPORT_CACHE_ASSET_VERSION__",
            NODE_IMPORT_CACHE_ASSET_VERSION,
        )
        .replace(
            "__AGENTOS_BUILTIN_SPECIFIER_PREFIX__",
            AGENTOS_BUILTIN_SPECIFIER_PREFIX,
        )
        .replace(
            "__AGENTOS_POLYFILL_SPECIFIER_PREFIX__",
            AGENTOS_POLYFILL_SPECIFIER_PREFIX,
        )
}

fn render_patched_pyodide_mjs() -> String {
    let source = String::from_utf8_lossy(BUNDLED_PYODIDE_MJS);
    source
        .replace(
            r#"H=(await import("node:vm")).default,"#,
            "",
        )
        .replace(
            r#"async function fe(e){e.startsWith("file://")&&(e=e.slice(7)),e.includes("://")?H.runInThisContext(await(await fetch(e)).text()):await import(e.startsWith("/" )?e:$.pathToFileURL(e).href)}o(fe,"nodeLoadScript");"#,
            r#"async function fe(e){if(e.startsWith("file://")&&(e=e.slice(7)),e.includes("://")){let t=await(await fetch(e)).text();await import(`data:text/javascript;base64,${$e(t)}`);return}await import(e.startsWith("/")?e:$.pathToFileURL(e).href)}o(fe,"nodeLoadScript");"#,
        )
        .replace(
            r#"function Ne(e){if(typeof WasmOffsetConverter<"u")return;let{binary:t,response:n}=R(e+"pyodide.asm.wasm"),i=K();return function(s,r){return async function(){s.sentinel=await i;try{let a;if(n){a=await WebAssembly.instantiateStreaming(n,s);}else{let l=await t;a=await WebAssembly.instantiate(l,s);}let{instance:l,module:c}=a;r(l,c);}catch(a){console.warn("wasm instantiation failed!"),console.warn(a)}}(),{}}}o(Ne,"getInstantiateWasmFunc");"#,
            r#"function Ne(e){if(typeof WasmOffsetConverter<"u")return;let{binary:t,response:n}=R(e+"pyodide.asm.wasm"),i=K();return function(s,r){return async function(){s.sentinel=await i;try{let a;if(n){a=await WebAssembly.instantiateStreaming(n,s);}else{let l=await t;a=await WebAssembly.instantiate(l,s);}let{instance:l,module:c}=a;r(l,c);}catch(a){console.warn("wasm instantiation failed!"),console.warn(a);throw a}}(),{}}}o(Ne,"getInstantiateWasmFunc");"#,
        )
}

fn render_register_source() -> String {
    NODE_IMPORT_CACHE_REGISTER_SOURCE.replace(
        "__NODE_IMPORT_CACHE_LOADER_PATH_ENV__",
        NODE_IMPORT_CACHE_LOADER_PATH_ENV,
    )
}

fn render_builtin_asset_source(asset: &BuiltinAsset) -> String {
    match asset.name {
        "async-hooks" => render_async_hooks_builtin_asset_source(asset.init_counter_key),
        "fs" => render_fs_builtin_asset_source(asset.init_counter_key),
        "fs-promises" => render_fs_promises_builtin_asset_source(asset.init_counter_key),
        "child-process" => render_child_process_builtin_asset_source(asset.init_counter_key),
        "net" => render_net_builtin_asset_source(asset.init_counter_key),
        "dgram" => render_dgram_builtin_asset_source(asset.init_counter_key),
        "diagnostics-channel" => {
            render_diagnostics_channel_builtin_asset_source(asset.init_counter_key)
        }
        "dns" => render_dns_builtin_asset_source(asset.init_counter_key),
        "dns-promises" => render_dns_promises_builtin_asset_source(asset.init_counter_key),
        "http" => render_http_builtin_asset_source(asset.init_counter_key),
        "http2" => render_http2_builtin_asset_source(asset.init_counter_key),
        "https" => render_https_builtin_asset_source(asset.init_counter_key),
        "tls" => render_tls_builtin_asset_source(asset.init_counter_key),
        "os" => render_os_builtin_asset_source(asset.init_counter_key),
        "util" => render_util_builtin_asset_source(asset.init_counter_key),
        "v8" => render_v8_builtin_asset_source(asset.init_counter_key),
        "vm" => render_vm_builtin_asset_source(asset.init_counter_key),
        "worker-threads" => render_worker_threads_builtin_asset_source(asset.init_counter_key),
        _ => {
            render_passthrough_builtin_asset_source(asset.module_specifier, asset.init_counter_key)
        }
    }
}

fn render_passthrough_builtin_asset_source(
    module_specifier: &str,
    init_counter_key: &str,
) -> String {
    let module_specifier = format!("{module_specifier:?}");
    let init_counter_key = format!("{init_counter_key:?}");

    format!(
        "import * as namespace from {module_specifier};\n\n\
const initCount = (globalThis[{init_counter_key}] ?? 0) + 1;\n\
globalThis[{init_counter_key}] = initCount;\n\
const builtin = namespace.default ?? namespace;\n\n\
export const __agentOSInitCount = initCount;\n\
export default builtin;\n\
export * from {module_specifier};\n"
    )
}

fn render_util_builtin_asset_source(init_counter_key: &str) -> String {
    let init_counter_key = format!("{init_counter_key:?}");

    format!(
        "import * as namespace from \"node:util\";\n\n\
const initCount = (globalThis[{init_counter_key}] ?? 0) + 1;\n\
globalThis[{init_counter_key}] = initCount;\n\
const builtin = namespace.default ?? namespace;\n\n\
export const __agentOSInitCount = initCount;\n\
export default builtin;\n\
export const formatWithOptions = builtin.formatWithOptions;\n\
export * from \"node:util\";\n"
    )
}

fn render_fs_builtin_asset_source(init_counter_key: &str) -> String {
    let init_counter_key = format!("{init_counter_key:?}");

    format!(
        "const initCount = (globalThis[{init_counter_key}] ?? 0) + 1;\n\
globalThis[{init_counter_key}] = initCount;\n\
const mod = globalThis.__agentOSBuiltinFs ?? globalThis.__agentOSGuestFs ?? process.getBuiltinModule?.(\"node:fs\");\n\
if (!mod) {{\n\
  throw new Error('agentos guest fs polyfill was not initialized');\n\
}}\n\n\
export const __agentOSInitCount = initCount;\n\
export default mod;\n\
export const Dir = mod.Dir;\n\
export const Dirent = mod.Dirent;\n\
export const ReadStream = mod.ReadStream;\n\
export const Stats = mod.Stats;\n\
export const WriteStream = mod.WriteStream;\n\
export const constants = mod.constants;\n\
export const promises = mod.promises;\n\
export const access = mod.access;\n\
export const accessSync = mod.accessSync;\n\
export const appendFile = mod.appendFile;\n\
export const appendFileSync = mod.appendFileSync;\n\
export const chmod = mod.chmod;\n\
export const chmodSync = mod.chmodSync;\n\
export const chown = mod.chown;\n\
export const chownSync = mod.chownSync;\n\
export const close = mod.close;\n\
export const closeSync = mod.closeSync;\n\
export const copyFile = mod.copyFile;\n\
export const copyFileSync = mod.copyFileSync;\n\
export const cp = mod.cp;\n\
export const cpSync = mod.cpSync;\n\
export const createReadStream = mod.createReadStream;\n\
export const createWriteStream = mod.createWriteStream;\n\
export const exists = mod.exists;\n\
export const existsSync = mod.existsSync;\n\
export const lchmod = mod.lchmod;\n\
export const lchmodSync = mod.lchmodSync;\n\
export const lchown = mod.lchown;\n\
export const lchownSync = mod.lchownSync;\n\
export const link = mod.link;\n\
export const linkSync = mod.linkSync;\n\
export const lstat = mod.lstat;\n\
export const lstatSync = mod.lstatSync;\n\
export const lutimes = mod.lutimes;\n\
export const lutimesSync = mod.lutimesSync;\n\
export const mkdir = mod.mkdir;\n\
export const mkdirSync = mod.mkdirSync;\n\
export const mkdtemp = mod.mkdtemp;\n\
export const mkdtempSync = mod.mkdtempSync;\n\
export const open = mod.open;\n\
export const openSync = mod.openSync;\n\
export const opendir = mod.opendir;\n\
export const opendirSync = mod.opendirSync;\n\
export const read = mod.read;\n\
export const readFile = mod.readFile;\n\
export const readFileSync = mod.readFileSync;\n\
export const readSync = mod.readSync;\n\
export const readdir = mod.readdir;\n\
export const readdirSync = mod.readdirSync;\n\
export const readlink = mod.readlink;\n\
export const readlinkSync = mod.readlinkSync;\n\
export const realpath = mod.realpath;\n\
export const realpathSync = mod.realpathSync;\n\
export const rename = mod.rename;\n\
export const renameSync = mod.renameSync;\n\
export const rm = mod.rm;\n\
export const rmSync = mod.rmSync;\n\
export const rmdir = mod.rmdir;\n\
export const rmdirSync = mod.rmdirSync;\n\
export const stat = mod.stat;\n\
export const statSync = mod.statSync;\n\
export const statfs = mod.statfs;\n\
export const statfsSync = mod.statfsSync;\n\
export const symlink = mod.symlink;\n\
export const symlinkSync = mod.symlinkSync;\n\
export const truncate = mod.truncate;\n\
export const truncateSync = mod.truncateSync;\n\
export const unlink = mod.unlink;\n\
export const unlinkSync = mod.unlinkSync;\n\
export const unwatchFile = mod.unwatchFile;\n\
export const utimes = mod.utimes;\n\
export const utimesSync = mod.utimesSync;\n\
export const watch = mod.watch;\n\
export const watchFile = mod.watchFile;\n\
export const write = mod.write;\n\
export const writeFile = mod.writeFile;\n\
export const writeFileSync = mod.writeFileSync;\n\
export const writeSync = mod.writeSync;\n"
    )
}

fn render_fs_promises_builtin_asset_source(init_counter_key: &str) -> String {
    let init_counter_key = format!("{init_counter_key:?}");

    format!(
        "import fsModule from \"agentos:builtin/fs\";\n\n\
const initCount = (globalThis[{init_counter_key}] ?? 0) + 1;\n\
globalThis[{init_counter_key}] = initCount;\n\
const mod = fsModule.promises;\n\n\
export const __agentOSInitCount = initCount;\n\
export default mod;\n\
export const constants = fsModule.constants;\n\
export const FileHandle = mod.FileHandle;\n\
export const access = mod.access;\n\
export const appendFile = mod.appendFile;\n\
export const chmod = mod.chmod;\n\
export const chown = mod.chown;\n\
export const copyFile = mod.copyFile;\n\
export const cp = mod.cp;\n\
export const lchmod = mod.lchmod;\n\
export const lchown = mod.lchown;\n\
export const link = mod.link;\n\
export const lstat = mod.lstat;\n\
export const lutimes = mod.lutimes;\n\
export const mkdir = mod.mkdir;\n\
export const mkdtemp = mod.mkdtemp;\n\
export const open = mod.open;\n\
export const opendir = mod.opendir;\n\
export const readFile = mod.readFile;\n\
export const readdir = mod.readdir;\n\
export const readlink = mod.readlink;\n\
export const realpath = mod.realpath;\n\
export const rename = mod.rename;\n\
export const rm = mod.rm;\n\
export const rmdir = mod.rmdir;\n\
export const stat = mod.stat;\n\
export const statfs = mod.statfs;\n\
export const symlink = mod.symlink;\n\
export const truncate = mod.truncate;\n\
export const unlink = mod.unlink;\n\
export const utimes = mod.utimes;\n\
export const watch = mod.watch;\n\
export const writeFile = mod.writeFile;\n"
    )
}

fn render_async_hooks_builtin_asset_source(init_counter_key: &str) -> String {
    let init_counter_key = format!("{init_counter_key:?}");

    format!(
        "const initCount = (globalThis[{init_counter_key}] ?? 0) + 1;\n\
globalThis[{init_counter_key}] = initCount;\n\
\n\
class AsyncLocalStorage {{\n\
  constructor() {{\n\
    this._store = undefined;\n\
  }}\n\
  disable() {{\n\
    this._store = undefined;\n\
  }}\n\
  enterWith(store) {{\n\
    this._store = store;\n\
  }}\n\
  exit(callback, ...args) {{\n\
    return callback(...args);\n\
  }}\n\
  getStore() {{\n\
    return this._store;\n\
  }}\n\
  run(store, callback, ...args) {{\n\
    const previous = this._store;\n\
    this._store = store;\n\
    try {{\n\
      return callback(...args);\n\
    }} finally {{\n\
      this._store = previous;\n\
    }}\n\
  }}\n\
}}\n\
\n\
class AsyncResource {{\n\
  constructor(type = 'AgentOsAsyncResource') {{\n\
    this.type = type;\n\
  }}\n\
  emitBefore() {{}}\n\
  emitAfter() {{}}\n\
  emitDestroy() {{}}\n\
  asyncId() {{\n\
    return 0;\n\
  }}\n\
  triggerAsyncId() {{\n\
    return 0;\n\
  }}\n\
  runInAsyncScope(callback, thisArg, ...args) {{\n\
    return callback.apply(thisArg, args);\n\
  }}\n\
}}\n\
\n\
function createHook() {{\n\
  return {{\n\
    enable() {{\n\
      return this;\n\
    }},\n\
    disable() {{\n\
      return this;\n\
    }},\n\
  }};\n\
}}\n\
\n\
function executionAsyncId() {{\n\
  return 0;\n\
}}\n\
\n\
function triggerAsyncId() {{\n\
  return 0;\n\
}}\n\
\n\
const mod = {{\n\
  AsyncLocalStorage,\n\
  AsyncResource,\n\
  createHook,\n\
  executionAsyncId,\n\
  triggerAsyncId,\n\
}};\n\
\n\
export const __agentOSInitCount = initCount;\n\
export default mod;\n\
export {{ AsyncLocalStorage, AsyncResource, createHook, executionAsyncId, triggerAsyncId }};\n"
    )
}

fn render_child_process_builtin_asset_source(init_counter_key: &str) -> String {
    let init_counter_key = format!("{init_counter_key:?}");

    format!(
        "const ACCESS_DENIED_CODE = \"ERR_ACCESS_DENIED\";\n\
const initCount = (globalThis[{init_counter_key}] ?? 0) + 1;\n\
globalThis[{init_counter_key}] = initCount;\n\
if (!globalThis.__agentOSBuiltinChildProcess) {{\n\
  const error = new Error(\"node:child_process is not available in the agentos guest runtime\");\n\
  error.code = ACCESS_DENIED_CODE;\n\
  throw error;\n\
}}\n\n\
const mod = globalThis.__agentOSBuiltinChildProcess;\n\n\
export const __agentOSInitCount = initCount;\n\
export default mod;\n\
export const ChildProcess = mod.ChildProcess;\n\
export const _forkChild = mod._forkChild;\n\
export const exec = mod.exec;\n\
export const execFile = mod.execFile;\n\
export const execFileSync = mod.execFileSync;\n\
export const execSync = mod.execSync;\n\
export const fork = mod.fork;\n\
export const spawn = mod.spawn;\n\
export const spawnSync = mod.spawnSync;\n"
    )
}

fn render_net_builtin_asset_source(init_counter_key: &str) -> String {
    let init_counter_key = format!("{init_counter_key:?}");

    format!(
        "const ACCESS_DENIED_CODE = \"ERR_ACCESS_DENIED\";\n\
const initCount = (globalThis[{init_counter_key}] ?? 0) + 1;\n\
globalThis[{init_counter_key}] = initCount;\n\
if (!globalThis.__agentOSBuiltinNet) {{\n\
  const error = new Error(\"node:net is not available in the agentos guest runtime\");\n\
  error.code = ACCESS_DENIED_CODE;\n\
  throw error;\n\
}}\n\n\
const mod = globalThis.__agentOSBuiltinNet;\n\n\
export const __agentOSInitCount = initCount;\n\
export default mod;\n\
export const BlockList = mod.BlockList;\n\
export const Server = mod.Server;\n\
export const Socket = mod.Socket;\n\
export const SocketAddress = mod.SocketAddress;\n\
export const Stream = mod.Stream;\n\
export const connect = mod.connect;\n\
export const createConnection = mod.createConnection;\n\
export const createServer = mod.createServer;\n\
export const getDefaultAutoSelectFamily = mod.getDefaultAutoSelectFamily;\n\
export const getDefaultAutoSelectFamilyAttemptTimeout = mod.getDefaultAutoSelectFamilyAttemptTimeout;\n\
export const isIP = mod.isIP;\n\
export const isIPv4 = mod.isIPv4;\n\
export const isIPv6 = mod.isIPv6;\n\
export const setDefaultAutoSelectFamily = mod.setDefaultAutoSelectFamily;\n\
export const setDefaultAutoSelectFamilyAttemptTimeout = mod.setDefaultAutoSelectFamilyAttemptTimeout;\n"
    )
}

fn render_dgram_builtin_asset_source(init_counter_key: &str) -> String {
    let init_counter_key = format!("{init_counter_key:?}");

    format!(
        "const ACCESS_DENIED_CODE = \"ERR_ACCESS_DENIED\";\n\
const initCount = (globalThis[{init_counter_key}] ?? 0) + 1;\n\
globalThis[{init_counter_key}] = initCount;\n\
if (!globalThis.__agentOSBuiltinDgram) {{\n\
  const error = new Error(\"node:dgram is not available in the agentos guest runtime\");\n\
  error.code = ACCESS_DENIED_CODE;\n\
  throw error;\n\
}}\n\n\
const mod = globalThis.__agentOSBuiltinDgram;\n\n\
export const __agentOSInitCount = initCount;\n\
export default mod;\n\
export const Socket = mod.Socket;\n\
export const createSocket = mod.createSocket;\n"
    )
}

fn render_diagnostics_channel_builtin_asset_source(init_counter_key: &str) -> String {
    let init_counter_key = format!("{init_counter_key:?}");

    format!(
        r#"const initCount = (globalThis[{init_counter_key}] ?? 0) + 1;
globalThis[{init_counter_key}] = initCount;

class Channel {{
  constructor(name = '') {{
    this.name = String(name);
    this._subscribers = new Set();
  }}

  get hasSubscribers() {{
    return this._subscribers.size > 0;
  }}

  publish(message) {{
    for (const subscriber of Array.from(this._subscribers)) {{
      subscriber(message, this.name);
    }}
  }}

  subscribe(subscriber) {{
    if (typeof subscriber === 'function') {{
      this._subscribers.add(subscriber);
    }}
  }}

  unsubscribe(subscriber) {{
    return this._subscribers.delete(subscriber);
  }}

  runStores(context, callback, thisArg, ...args) {{
    if (typeof callback !== 'function') {{
      return callback;
    }}
    return callback.apply(thisArg, args);
  }}
}}

const channelCache = new Map();

function channel(name = '') {{
  const channelName = String(name);
  let existing = channelCache.get(channelName);
  if (!existing) {{
    existing = new Channel(channelName);
    channelCache.set(channelName, existing);
  }}
  return existing;
}}

function hasSubscribers(name = '') {{
  return channel(name).hasSubscribers;
}}

function subscribe(name = '', subscriber) {{
  return channel(name).subscribe(subscriber);
}}

function unsubscribe(name = '', subscriber) {{
  return channel(name).unsubscribe(subscriber);
}}

function tracingChannel(name = '') {{
  const channelName = String(name);
  const tracing = {{
    start: channel(`tracing:${{channelName}}:start`),
    end: channel(`tracing:${{channelName}}:end`),
    asyncStart: channel(`tracing:${{channelName}}:asyncStart`),
    asyncEnd: channel(`tracing:${{channelName}}:asyncEnd`),
    error: channel(`tracing:${{channelName}}:error`),
    subscribe() {{}},
    unsubscribe() {{
      return true;
    }},
    traceSync(fn, context, thisArg, ...args) {{
      if (typeof fn !== 'function') {{
        return fn;
      }}
      return fn.apply(thisArg, args);
    }},
    tracePromise(fn, context, thisArg, ...args) {{
      if (typeof fn !== 'function') {{
        return Promise.resolve(fn);
      }}
      return Promise.resolve(fn.apply(thisArg, args));
    }},
    traceCallback(fn, position, context, thisArg, ...args) {{
      if (typeof fn !== 'function') {{
        return fn;
      }}
      return fn.apply(thisArg, args);
    }},
  }};
  Object.defineProperty(tracing, 'hasSubscribers', {{
    get() {{
      return (
        tracing.start.hasSubscribers ||
        tracing.end.hasSubscribers ||
        tracing.asyncStart.hasSubscribers ||
        tracing.asyncEnd.hasSubscribers ||
        tracing.error.hasSubscribers
      );
    }},
    enumerable: false,
    configurable: true,
  }});
  return tracing;
}}

const mod = {{ Channel, channel, hasSubscribers, subscribe, tracingChannel, unsubscribe }};

export const __agentOSInitCount = initCount;
export default mod;
export {{ Channel, channel, hasSubscribers, subscribe, tracingChannel, unsubscribe }};
"#
    )
}

fn render_dns_builtin_asset_source(init_counter_key: &str) -> String {
    let init_counter_key = format!("{init_counter_key:?}");

    format!(
        "const ACCESS_DENIED_CODE = \"ERR_ACCESS_DENIED\";\n\
const initCount = (globalThis[{init_counter_key}] ?? 0) + 1;\n\
globalThis[{init_counter_key}] = initCount;\n\
if (!globalThis.__agentOSBuiltinDns) {{\n\
  const error = new Error(\"node:dns is not available in the agentos guest runtime\");\n\
  error.code = ACCESS_DENIED_CODE;\n\
  throw error;\n\
}}\n\n\
const mod = globalThis.__agentOSBuiltinDns;\n\n\
export const __agentOSInitCount = initCount;\n\
export default mod;\n\
export const ADDRCONFIG = mod.ADDRCONFIG;\n\
export const ALL = mod.ALL;\n\
export const Resolver = mod.Resolver;\n\
export const V4MAPPED = mod.V4MAPPED;\n\
export const constants = mod.constants;\n\
export const getDefaultResultOrder = mod.getDefaultResultOrder;\n\
export const getServers = mod.getServers;\n\
export const lookup = mod.lookup;\n\
export const lookupService = mod.lookupService;\n\
export const promises = mod.promises;\n\
export const resolve = mod.resolve;\n\
export const resolve4 = mod.resolve4;\n\
export const resolve6 = mod.resolve6;\n\
export const reverse = mod.reverse;\n\
export const setDefaultResultOrder = mod.setDefaultResultOrder;\n\
export const setServers = mod.setServers;\n"
    )
}

fn render_dns_promises_builtin_asset_source(init_counter_key: &str) -> String {
    let init_counter_key = format!("{init_counter_key:?}");

    format!(
        "const ACCESS_DENIED_CODE = \"ERR_ACCESS_DENIED\";\n\
const initCount = (globalThis[{init_counter_key}] ?? 0) + 1;\n\
globalThis[{init_counter_key}] = initCount;\n\
if (!globalThis.__agentOSBuiltinDns) {{\n\
  const error = new Error(\"node:dns/promises is not available in the agentos guest runtime\");\n\
  error.code = ACCESS_DENIED_CODE;\n\
  throw error;\n\
}}\n\n\
const mod = globalThis.__agentOSBuiltinDns.promises;\n\n\
export const __agentOSInitCount = initCount;\n\
export default mod;\n\
export const Resolver = mod.Resolver;\n\
export const lookup = mod.lookup;\n\
export const resolve = mod.resolve;\n\
export const resolve4 = mod.resolve4;\n\
export const resolve6 = mod.resolve6;\n\
export const resolveAny = mod.resolveAny;\n\
export const resolveMx = mod.resolveMx;\n\
export const resolveTxt = mod.resolveTxt;\n\
export const resolveSrv = mod.resolveSrv;\n\
export const resolveCname = mod.resolveCname;\n\
export const resolvePtr = mod.resolvePtr;\n\
export const resolveNs = mod.resolveNs;\n\
export const resolveSoa = mod.resolveSoa;\n\
export const resolveNaptr = mod.resolveNaptr;\n\
export const resolveCaa = mod.resolveCaa;\n"
    )
}

fn render_http_builtin_asset_source(init_counter_key: &str) -> String {
    let init_counter_key = format!("{init_counter_key:?}");

    format!(
        "const ACCESS_DENIED_CODE = \"ERR_ACCESS_DENIED\";\n\
const initCount = (globalThis[{init_counter_key}] ?? 0) + 1;\n\
globalThis[{init_counter_key}] = initCount;\n\
if (!globalThis.__agentOSBuiltinHttp) {{\n\
  const error = new Error(\"node:http is not available in the agentos guest runtime\");\n\
  error.code = ACCESS_DENIED_CODE;\n\
  throw error;\n\
}}\n\n\
const mod = globalThis.__agentOSBuiltinHttp;\n\n\
export const __agentOSInitCount = initCount;\n\
export default mod;\n\
export const Agent = mod.Agent;\n\
export const ClientRequest = mod.ClientRequest;\n\
export const IncomingMessage = mod.IncomingMessage;\n\
export const METHODS = mod.METHODS;\n\
export const OutgoingMessage = mod.OutgoingMessage;\n\
export const STATUS_CODES = mod.STATUS_CODES;\n\
export const Server = mod.Server;\n\
export const ServerResponse = mod.ServerResponse;\n\
export const createServer = mod.createServer;\n\
export const get = mod.get;\n\
export const globalAgent = mod.globalAgent;\n\
export const maxHeaderSize = mod.maxHeaderSize;\n\
export const request = mod.request;\n\
export const setMaxIdleHTTPParsers = mod.setMaxIdleHTTPParsers;\n\
export const validateHeaderName = mod.validateHeaderName;\n\
export const validateHeaderValue = mod.validateHeaderValue;\n"
    )
}

fn render_http2_builtin_asset_source(init_counter_key: &str) -> String {
    let init_counter_key = format!("{init_counter_key:?}");

    format!(
        "const ACCESS_DENIED_CODE = \"ERR_ACCESS_DENIED\";\n\
const initCount = (globalThis[{init_counter_key}] ?? 0) + 1;\n\
globalThis[{init_counter_key}] = initCount;\n\
if (!globalThis.__agentOSBuiltinHttp2) {{\n\
  const error = new Error(\"node:http2 is not available in the agentos guest runtime\");\n\
  error.code = ACCESS_DENIED_CODE;\n\
  throw error;\n\
}}\n\n\
const mod = globalThis.__agentOSBuiltinHttp2;\n\n\
export const __agentOSInitCount = initCount;\n\
export default mod;\n\
export const Http2ServerRequest = mod.Http2ServerRequest;\n\
export const Http2ServerResponse = mod.Http2ServerResponse;\n\
export const Http2Session = mod.Http2Session;\n\
export const Http2Stream = mod.Http2Stream;\n\
export const constants = mod.constants;\n\
export const connect = mod.connect;\n\
export const createServer = mod.createServer;\n\
export const createSecureServer = mod.createSecureServer;\n\
export const getDefaultSettings = mod.getDefaultSettings;\n\
export const getPackedSettings = mod.getPackedSettings;\n\
export const getUnpackedSettings = mod.getUnpackedSettings;\n\
export const sensitiveHeaders = mod.sensitiveHeaders;\n"
    )
}

fn render_https_builtin_asset_source(init_counter_key: &str) -> String {
    let init_counter_key = format!("{init_counter_key:?}");

    format!(
        "const ACCESS_DENIED_CODE = \"ERR_ACCESS_DENIED\";\n\
const initCount = (globalThis[{init_counter_key}] ?? 0) + 1;\n\
globalThis[{init_counter_key}] = initCount;\n\
if (!globalThis.__agentOSBuiltinHttps) {{\n\
  const error = new Error(\"node:https is not available in the agentos guest runtime\");\n\
  error.code = ACCESS_DENIED_CODE;\n\
  throw error;\n\
}}\n\n\
const mod = globalThis.__agentOSBuiltinHttps;\n\n\
export const __agentOSInitCount = initCount;\n\
export default mod;\n\
export const Agent = mod.Agent;\n\
export const Server = mod.Server;\n\
export const createServer = mod.createServer;\n\
export const get = mod.get;\n\
export const globalAgent = mod.globalAgent;\n\
export const request = mod.request;\n"
    )
}

fn render_tls_builtin_asset_source(init_counter_key: &str) -> String {
    let init_counter_key = format!("{init_counter_key:?}");

    format!(
        "const ACCESS_DENIED_CODE = \"ERR_ACCESS_DENIED\";\n\
const initCount = (globalThis[{init_counter_key}] ?? 0) + 1;\n\
globalThis[{init_counter_key}] = initCount;\n\
if (!globalThis.__agentOSBuiltinTls) {{\n\
  const error = new Error(\"node:tls is not available in the agentos guest runtime\");\n\
  error.code = ACCESS_DENIED_CODE;\n\
  throw error;\n\
}}\n\n\
const mod = globalThis.__agentOSBuiltinTls;\n\n\
export const __agentOSInitCount = initCount;\n\
export default mod;\n\
export const CLIENT_RENEG_LIMIT = mod.CLIENT_RENEG_LIMIT;\n\
export const CLIENT_RENEG_WINDOW = mod.CLIENT_RENEG_WINDOW;\n\
export const DEFAULT_CIPHERS = mod.DEFAULT_CIPHERS;\n\
export const DEFAULT_ECDH_CURVE = mod.DEFAULT_ECDH_CURVE;\n\
export const DEFAULT_MAX_VERSION = mod.DEFAULT_MAX_VERSION;\n\
export const DEFAULT_MIN_VERSION = mod.DEFAULT_MIN_VERSION;\n\
export const SecureContext = mod.SecureContext;\n\
export const Server = mod.Server;\n\
export const TLSSocket = mod.TLSSocket;\n\
export const checkServerIdentity = mod.checkServerIdentity;\n\
export const connect = mod.connect;\n\
export const createConnection = mod.createConnection;\n\
export const createSecureContext = mod.createSecureContext;\n\
export const createSecurePair = mod.createSecurePair;\n\
export const createServer = mod.createServer;\n\
export const getCiphers = mod.getCiphers;\n\
export const rootCertificates = mod.rootCertificates;\n"
    )
}

fn render_os_builtin_asset_source(init_counter_key: &str) -> String {
    let init_counter_key = format!("{init_counter_key:?}");

    format!(
        "const ACCESS_DENIED_CODE = \"ERR_ACCESS_DENIED\";\n\
const initCount = (globalThis[{init_counter_key}] ?? 0) + 1;\n\
globalThis[{init_counter_key}] = initCount;\n\
if (!globalThis.__agentOSBuiltinOs) {{\n\
  const error = new Error(\"node:os is not available in the agentos guest runtime\");\n\
  error.code = ACCESS_DENIED_CODE;\n\
  throw error;\n\
}}\n\n\
const mod = globalThis.__agentOSBuiltinOs;\n\n\
export const __agentOSInitCount = initCount;\n\
export default mod;\n\
export const EOL = mod.EOL;\n\
export const arch = mod.arch;\n\
export const availableParallelism = mod.availableParallelism;\n\
export const constants = mod.constants;\n\
export const cpus = mod.cpus;\n\
export const devNull = mod.devNull;\n\
export const endianness = mod.endianness;\n\
export const freemem = mod.freemem;\n\
export const getPriority = mod.getPriority;\n\
export const homedir = mod.homedir;\n\
export const hostname = mod.hostname;\n\
export const loadavg = mod.loadavg;\n\
export const machine = mod.machine;\n\
export const networkInterfaces = mod.networkInterfaces;\n\
export const platform = mod.platform;\n\
export const release = mod.release;\n\
export const setPriority = mod.setPriority;\n\
export const tmpdir = mod.tmpdir;\n\
export const totalmem = mod.totalmem;\n\
export const type = mod.type;\n\
export const uptime = mod.uptime;\n\
export const userInfo = mod.userInfo;\n\
export const version = mod.version;\n"
    )
}

fn render_v8_builtin_asset_source(init_counter_key: &str) -> String {
    let init_counter_key = format!("{init_counter_key:?}");

    format!(
        "const initCount = (globalThis[{init_counter_key}] ?? 0) + 1;\n\
globalThis[{init_counter_key}] = initCount;\n\
const mod = process.getBuiltinModule?.(\"node:v8\");\n\
if (!mod) {{\n\
  throw new Error(\"agentos guest v8 compatibility module was not initialized\");\n\
}}\n\n\
export const __agentOSInitCount = initCount;\n\
export default mod;\n\
export const GCProfiler = mod.GCProfiler;\n\
export const Deserializer = mod.Deserializer;\n\
export const Serializer = mod.Serializer;\n\
export const cachedDataVersionTag = mod.cachedDataVersionTag;\n\
export const deserialize = mod.deserialize;\n\
export const getCppHeapStatistics = mod.getCppHeapStatistics;\n\
export const getHeapCodeStatistics = mod.getHeapCodeStatistics;\n\
export const getHeapSnapshot = mod.getHeapSnapshot;\n\
export const getHeapSpaceStatistics = mod.getHeapSpaceStatistics;\n\
export const getHeapStatistics = mod.getHeapStatistics;\n\
export const isStringOneByteRepresentation = mod.isStringOneByteRepresentation;\n\
export const promiseHooks = mod.promiseHooks;\n\
export const queryObjects = mod.queryObjects;\n\
export const serialize = mod.serialize;\n\
export const setFlagsFromString = mod.setFlagsFromString;\n\
export const setHeapSnapshotNearHeapLimit = mod.setHeapSnapshotNearHeapLimit;\n\
export const startCpuProfile = mod.startCpuProfile;\n\
export const startupSnapshot = mod.startupSnapshot;\n\
export const stopCoverage = mod.stopCoverage;\n\
export const takeCoverage = mod.takeCoverage;\n\
export const writeHeapSnapshot = mod.writeHeapSnapshot;\n"
    )
}

fn render_vm_builtin_asset_source(init_counter_key: &str) -> String {
    let init_counter_key = format!("{init_counter_key:?}");

    format!(
        "const initCount = (globalThis[{init_counter_key}] ?? 0) + 1;\n\
globalThis[{init_counter_key}] = initCount;\n\
const mod = process.getBuiltinModule?.(\"node:vm\");\n\
if (!mod) {{\n\
  throw new Error(\"agentos guest vm compatibility module was not initialized\");\n\
}}\n\n\
export const __agentOSInitCount = initCount;\n\
export default mod;\n\
export const Script = mod.Script;\n\
export const createContext = mod.createContext;\n\
export const isContext = mod.isContext;\n\
export const runInNewContext = mod.runInNewContext;\n\
export const runInThisContext = mod.runInThisContext;\n"
    )
}

fn render_worker_threads_builtin_asset_source(init_counter_key: &str) -> String {
    let init_counter_key = format!("{init_counter_key:?}");

    format!(
        "const initCount = (globalThis[{init_counter_key}] ?? 0) + 1;\n\
globalThis[{init_counter_key}] = initCount;\n\
\n\
function createNotImplementedError(feature) {{\n\
  const error = new Error(`node:worker_threads ${{feature}} is not available in the agentos guest runtime`);\n\
  error.code = \"ERR_NOT_IMPLEMENTED\";\n\
  return error;\n\
}}\n\
\n\
class MessagePort {{\n\
  postMessage() {{}}\n\
  start() {{}}\n\
  close() {{}}\n\
  unref() {{\n\
    return this;\n\
  }}\n\
  ref() {{\n\
    return this;\n\
  }}\n\
}}\n\
\n\
class MessageChannel {{\n\
  constructor() {{\n\
    this.port1 = new MessagePort();\n\
    this.port2 = new MessagePort();\n\
  }}\n\
}}\n\
\n\
class Worker {{\n\
  constructor() {{\n\
    throw createNotImplementedError(\"Worker\");\n\
  }}\n\
}}\n\
\n\
function getEnvironmentData() {{\n\
  return undefined;\n\
}}\n\
\n\
function markAsUncloneable() {{}}\n\
\n\
function markAsUntransferable() {{}}\n\
\n\
function moveMessagePortToContext() {{\n\
  throw createNotImplementedError(\"moveMessagePortToContext\");\n\
}}\n\
\n\
function postMessageToThread() {{\n\
  throw createNotImplementedError(\"postMessageToThread\");\n\
}}\n\
\n\
function receiveMessageOnPort() {{\n\
  return undefined;\n\
}}\n\
\n\
function setEnvironmentData() {{}}\n\
\n\
const mod = {{\n\
  BroadcastChannel: globalThis.BroadcastChannel,\n\
  MessageChannel,\n\
  MessagePort,\n\
  SHARE_ENV: Symbol.for(\"agentos.worker_threads.SHARE_ENV\"),\n\
  Worker,\n\
  getEnvironmentData,\n\
  isMainThread: true,\n\
  markAsUncloneable,\n\
  markAsUntransferable,\n\
  moveMessagePortToContext,\n\
  parentPort: null,\n\
  postMessageToThread,\n\
  receiveMessageOnPort,\n\
  resourceLimits: {{}},\n\
  setEnvironmentData,\n\
  threadId: 0,\n\
  workerData: null,\n\
}};\n\
\n\
export const __agentOSInitCount = initCount;\n\
export default mod;\n\
export const BroadcastChannel = mod.BroadcastChannel;\n\
export const MessageChannel = mod.MessageChannel;\n\
export const MessagePort = mod.MessagePort;\n\
export const SHARE_ENV = mod.SHARE_ENV;\n\
export const Worker = mod.Worker;\n\
export const getEnvironmentData = mod.getEnvironmentData;\n\
export const isMainThread = mod.isMainThread;\n\
export const markAsUncloneable = mod.markAsUncloneable;\n\
export const markAsUntransferable = mod.markAsUntransferable;\n\
export const moveMessagePortToContext = mod.moveMessagePortToContext;\n\
export const parentPort = mod.parentPort;\n\
export const postMessageToThread = mod.postMessageToThread;\n\
export const receiveMessageOnPort = mod.receiveMessageOnPort;\n\
export const resourceLimits = mod.resourceLimits;\n\
export const setEnvironmentData = mod.setEnvironmentData;\n\
export const threadId = mod.threadId;\n\
export const workerData = mod.workerData;\n"
    )
}

fn render_denied_asset_source(module_specifier: &str) -> String {
    let message = format!("{module_specifier} is not available in the agentos guest runtime");
    format!(
        "const error = new Error({message:?});\nerror.code = \"ERR_ACCESS_DENIED\";\nthrow error;\n"
    )
}

fn render_path_polyfill_source() -> String {
    let init_counter_key = format!("{PATH_POLYFILL_INIT_COUNTER_KEY:?}");

    format!(
        "import path from \"node:path\";\n\n\
const initCount = (globalThis[{init_counter_key}] ?? 0) + 1;\n\
globalThis[{init_counter_key}] = initCount;\n\n\
export const __agentOSInitCount = initCount;\n\
export const basename = (...args) => path.basename(...args);\n\
export const dirname = (...args) => path.dirname(...args);\n\
export const join = (...args) => path.join(...args);\n\
export const resolve = (...args) => path.resolve(...args);\n\
export const sep = path.sep;\n\
export default path;\n"
    )
}

fn write_bytes_if_changed(path: &Path, contents: &[u8]) -> Result<(), io::Error> {
    match fs::read(path) {
        Ok(existing) if existing == contents => return Ok(()),
        Ok(_) | Err(_) => {}
    }

    fs::write(path, contents)
}

fn write_file_if_changed(path: &Path, contents: &str) -> Result<(), io::Error> {
    write_bytes_if_changed(path, contents.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::{
        NodeImportCache, NODE_IMPORT_CACHE_TEST_MATERIALIZE_DELAY_LOCK,
        NODE_IMPORT_CACHE_TEST_MATERIALIZE_DELAY_MS, NODE_WASM_RUNNER_SOURCE,
    };
    use crate::host_node::node_binary;
    use serde_json::Value;
    use std::collections::BTreeSet;
    use std::fs;
    use std::io::Write;
    use std::path::Path;
    use std::process::{Command, Output, Stdio};
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use tempfile::tempdir;

    fn assert_node_available() {
        let output = Command::new(node_binary())
            .arg("--version")
            .output()
            .expect("spawn node --version");
        assert!(output.status.success(), "node --version failed");
    }

    fn write_fixture(path: &Path, contents: &str) {
        fs::write(path, contents).expect("write fixture");
    }

    fn run_python_runner(
        import_cache: &NodeImportCache,
        pyodide_index_url: &Path,
        code: &str,
    ) -> Output {
        run_python_runner_with_env(import_cache, pyodide_index_url, code, &[])
    }

    fn run_python_runner_with_env(
        import_cache: &NodeImportCache,
        pyodide_index_url: &Path,
        code: &str,
        env: &[(&str, &str)],
    ) -> Output {
        let mut command = Command::new(node_binary());
        command
            .arg("--import")
            .arg(import_cache.timing_bootstrap_path())
            .arg(import_cache.python_runner_path())
            .env("AGENTOS_PYODIDE_INDEX_URL", pyodide_index_url)
            .env(
                "AGENTOS_PYODIDE_PACKAGE_CACHE_DIR",
                pyodide_index_url.join("pyodide-package-cache"),
            )
            .env("AGENTOS_PYTHON_CODE", code);

        for (key, value) in env {
            command.env(key, value);
        }

        command.output().expect("run python runner")
    }

    fn run_python_runner_prewarm(
        import_cache: &NodeImportCache,
        pyodide_index_url: &Path,
        env: &[(&str, &str)],
    ) -> Output {
        let mut command = Command::new(node_binary());
        command
            .arg("--import")
            .arg(import_cache.timing_bootstrap_path())
            .arg(import_cache.python_runner_path())
            .env("AGENTOS_PYODIDE_INDEX_URL", pyodide_index_url)
            .env(
                "AGENTOS_PYODIDE_PACKAGE_CACHE_DIR",
                pyodide_index_url.join("pyodide-package-cache"),
            )
            .env("AGENTOS_PYTHON_PREWARM_ONLY", "1");

        for (key, value) in env {
            command.env(key, value);
        }

        command.output().expect("run python runner prewarm")
    }

    fn run_python_runner_with_env_and_stdin(
        import_cache: &NodeImportCache,
        pyodide_index_url: &Path,
        code: &str,
        env: &[(&str, &str)],
        stdin_chunks: &[&[u8]],
    ) -> Output {
        let mut command = Command::new(node_binary());
        command
            .arg("--import")
            .arg(import_cache.timing_bootstrap_path())
            .arg(import_cache.python_runner_path())
            .env("AGENTOS_PYODIDE_INDEX_URL", pyodide_index_url)
            .env(
                "AGENTOS_PYODIDE_PACKAGE_CACHE_DIR",
                pyodide_index_url.join("pyodide-package-cache"),
            )
            .env("AGENTOS_PYTHON_CODE", code)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        for (key, value) in env {
            command.env(key, value);
        }

        let mut child = command.spawn().expect("spawn python runner");
        {
            let mut stdin = child.stdin.take().expect("python runner stdin");
            for chunk in stdin_chunks {
                stdin
                    .write_all(chunk)
                    .expect("write python runner stdin chunk");
            }
        }

        child.wait_with_output().expect("wait for python runner")
    }

    #[test]
    fn materialized_python_runner_hardens_builtin_access_before_load_pyodide() {
        assert_node_available();

        let import_cache = NodeImportCache::default();
        import_cache
            .ensure_materialized()
            .expect("materialize node import cache");

        let pyodide_dir = tempdir().expect("create pyodide fixture dir");
        write_fixture(
            &pyodide_dir.path().join("pyodide.mjs"),
            r#"
export async function loadPyodide(options) {
  const capturedFetch = globalThis.fetch;
  return {
    setStdin(_stdin) {},
    async runPythonAsync() {
      try {
        await capturedFetch('http://127.0.0.1:1/');
        options.stdout('unexpected');
      } catch (error) {
        options.stdout(JSON.stringify({
          code: error.code ?? null,
          message: error.message,
        }));
      }
    },
  };
}
"#,
        );
        write_fixture(
            &pyodide_dir.path().join("pyodide-lock.json"),
            "{\"packages\":[]}\n",
        );

        let output = run_python_runner(&import_cache, pyodide_dir.path(), "print('hello')");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse hardening JSON");

        assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
        assert_eq!(
            parsed["code"],
            Value::String(String::from("ERR_ACCESS_DENIED"))
        );
        assert!(
            parsed["message"]
                .as_str()
                .expect("fetch denial message")
                .contains("network access"),
            "unexpected stdout: {stdout}"
        );
    }

    #[test]
    fn materialized_python_runner_executes_python_code_via_pyodide_callbacks() {
        assert_node_available();

        let import_cache = NodeImportCache::default();
        import_cache
            .ensure_materialized()
            .expect("materialize node import cache");

        let pyodide_dir = tempdir().expect("create pyodide fixture dir");
        write_fixture(
            &pyodide_dir.path().join("pyodide.mjs"),
            r#"
export async function loadPyodide(options) {
  return {
    setStdin(_stdin) {},
    async runPythonAsync(code) {
      options.stdout(`stdout:${code}`);
      options.stderr(`stderr:${options.indexURL}:${options.lockFileContents}`);
    },
  };
}
"#,
        );
        write_fixture(
            &pyodide_dir.path().join("pyodide-lock.json"),
            "{\"packages\":[]}\n",
        );

        let output = run_python_runner(&import_cache, pyodide_dir.path(), "print('hello')");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let expected_index_path = format!(
            "stderr:{}{}",
            pyodide_dir.path().display(),
            std::path::MAIN_SEPARATOR
        );

        assert_eq!(output.status.code(), Some(0));
        assert_eq!(stdout, "stdout:print('hello')\n");
        assert!(
            stderr.starts_with(&expected_index_path),
            "unexpected stderr: {stderr}"
        );
        assert!(
            stderr.contains("{\"packages\":[]}"),
            "lock file contents should be passed to loadPyodide: {stderr}"
        );
    }

    #[test]
    fn materialized_python_runner_prefers_python_file_over_inline_code() {
        assert_node_available();

        let import_cache = NodeImportCache::default();
        import_cache
            .ensure_materialized()
            .expect("materialize node import cache");

        let pyodide_dir = tempdir().expect("create pyodide fixture dir");
        write_fixture(
            &pyodide_dir.path().join("pyodide.mjs"),
            r#"
export async function loadPyodide(options) {
  return {
    FS: {
      readFile(path, config = {}) {
        options.stderr(`file:${path}:${config.encoding ?? 'binary'}`);
        return "print('from file')";
      },
    },
    setStdin(_stdin) {},
    async runPythonAsync(code) {
      options.stdout(`stdout:${code}`);
    },
  };
}
"#,
        );
        write_fixture(
            &pyodide_dir.path().join("pyodide-lock.json"),
            "{\"packages\":[]}\n",
        );

        let output = run_python_runner_with_env(
            &import_cache,
            pyodide_dir.path(),
            "print('ignored')",
            &[("AGENTOS_PYTHON_FILE", "/workspace/script.py")],
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
        assert_eq!(stdout, "stdout:print('from file')\n");
        assert!(
            stderr.contains("file:/workspace/script.py:utf8"),
            "unexpected stderr: {stderr}"
        );
    }

    #[test]
    fn materialized_python_runner_prewarm_validates_assets_without_running_guest_code() {
        assert_node_available();

        let import_cache = NodeImportCache::default();
        import_cache
            .ensure_materialized()
            .expect("materialize node import cache");

        let pyodide_dir = tempdir().expect("create pyodide fixture dir");
        write_fixture(
            &pyodide_dir.path().join("pyodide.mjs"),
            r#"
export async function loadPyodide(options) {
  options.stderr(`prewarm:${options.indexURL}`);
  return {
    setStdin() {
      throw new Error('setStdin should not run during prewarm');
    },
    async runPythonAsync() {
      throw new Error('runPythonAsync should not run during prewarm');
    },
  };
}
"#,
        );
        write_fixture(
            &pyodide_dir.path().join("pyodide-lock.json"),
            "{\"packages\":[]}\n",
        );
        fs::write(pyodide_dir.path().join("python_stdlib.zip"), b"stub-stdlib")
            .expect("write stdlib fixture");
        fs::write(pyodide_dir.path().join("pyodide.asm.wasm"), b"stub-wasm")
            .expect("write wasm fixture");

        let output = run_python_runner_prewarm(
            &import_cache,
            pyodide_dir.path(),
            &[("AGENTOS_PYTHON_CODE", "print('ignored')")],
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
        assert!(stdout.is_empty(), "unexpected stdout: {stdout}");
        assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
        assert!(
            !stderr.contains("setStdin should not run during prewarm"),
            "unexpected stderr: {stderr}"
        );
        assert!(
            !stderr.contains("runPythonAsync should not run during prewarm"),
            "unexpected stderr: {stderr}"
        );
    }

    #[test]
    fn materialized_python_runner_reports_syntax_errors_to_stderr_and_exits_nonzero() {
        assert_node_available();

        let import_cache = NodeImportCache::default();
        import_cache
            .ensure_materialized()
            .expect("materialize node import cache");

        let pyodide_dir = tempdir().expect("create pyodide fixture dir");
        write_fixture(
            &pyodide_dir.path().join("pyodide.mjs"),
            r#"
export async function loadPyodide() {
  return {
    setStdin(_stdin) {},
    async runPythonAsync(code) {
      throw new Error(`SyntaxError: invalid syntax near ${code}`);
    },
  };
}
"#,
        );
        write_fixture(
            &pyodide_dir.path().join("pyodide-lock.json"),
            "{\"packages\":[]}\n",
        );

        let output = run_python_runner(&import_cache, pyodide_dir.path(), "print(");
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert_eq!(output.status.code(), Some(1));
        assert!(
            stderr.contains("SyntaxError: invalid syntax near print("),
            "unexpected stderr: {stderr}"
        );
    }

    #[test]
    fn materialized_python_runner_blocks_pyodide_js_escape_modules() {
        assert_node_available();

        let import_cache = NodeImportCache::default();
        import_cache
            .ensure_materialized()
            .expect("materialize node import cache");

        let output = run_python_runner(
            &import_cache,
            import_cache.pyodide_dist_path(),
            r#"
import json
import js
import pyodide_js

def capture(action):
    try:
        action()
        return {"ok": True}
    except Exception as error:
        return {
            "ok": False,
            "type": type(error).__name__,
            "message": str(error),
        }

print(json.dumps({
    "js_process_env": capture(lambda: js.process.env),
    "js_require": capture(lambda: js.require),
    "js_process_exit": capture(lambda: js.process.exit),
    "js_process_kill": capture(lambda: js.process.kill),
    "js_child_process_builtin": capture(
        lambda: js.process.getBuiltinModule("node:child_process")
    ),
    "js_vm_builtin": capture(
        lambda: js.process.getBuiltinModule("node:vm")
    ),
    "pyodide_js_eval_code": capture(lambda: pyodide_js.eval_code),
}))
"#,
        );

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let parsed: Value =
            serde_json::from_str(stdout.trim()).expect("parse Python hardening JSON");

        assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");

        for key in [
            "js_process_env",
            "js_require",
            "js_process_exit",
            "js_process_kill",
            "js_child_process_builtin",
            "js_vm_builtin",
        ] {
            assert_eq!(parsed[key]["ok"], Value::Bool(false), "stdout: {stdout}");
            assert_eq!(
                parsed[key]["type"],
                Value::String(String::from("RuntimeError"))
            );
            assert!(
                parsed[key]["message"]
                    .as_str()
                    .expect("js hardening message")
                    .contains("js is not available"),
                "stdout: {stdout}"
            );
        }

        assert_eq!(
            parsed["pyodide_js_eval_code"]["ok"],
            Value::Bool(false),
            "stdout: {stdout}"
        );
        assert_eq!(
            parsed["pyodide_js_eval_code"]["type"],
            Value::String(String::from("RuntimeError"))
        );
        assert!(
            parsed["pyodide_js_eval_code"]["message"]
                .as_str()
                .expect("pyodide_js hardening message")
                .contains("pyodide_js is not available"),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn materialized_python_runner_exposes_frozen_time_to_python() {
        assert_node_available();

        let import_cache = NodeImportCache::default();
        import_cache
            .ensure_materialized()
            .expect("materialize node import cache");

        let frozen_time_ms = 1_704_067_200_123_u64;
        let output = run_python_runner_with_env(
            &import_cache,
            import_cache.pyodide_dist_path(),
            r#"
import datetime
import json
import time

first_ns = time.time_ns()
second_ns = time.time_ns()
utc_now = datetime.datetime.now(datetime.timezone.utc)

print(json.dumps({
    "first_ns": first_ns,
    "second_ns": second_ns,
    "iso": utc_now.isoformat(timespec="milliseconds"),
}))
"#,
            &[("AGENTOS_FROZEN_TIME_MS", "1704067200123")],
        );

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse frozen-time JSON");

        assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
        assert_eq!(parsed["first_ns"], parsed["second_ns"], "stdout: {stdout}");
        let first_ns = parsed["first_ns"]
            .as_u64()
            .expect("frozen time.time_ns() value");
        assert_eq!(first_ns / 1_000_000, frozen_time_ms, "stdout: {stdout}");
        assert_eq!(
            parsed["iso"],
            Value::String(String::from("2024-01-01T00:00:00.123+00:00")),
            "stdout: {stdout}"
        );
    }

    #[test]
    fn materialized_python_runner_preloads_bundled_packages_from_local_disk() {
        assert_node_available();

        let import_cache = NodeImportCache::default();
        import_cache
            .ensure_materialized()
            .expect("materialize node import cache");

        let pyodide_dir = tempdir().expect("create pyodide fixture dir");
        write_fixture(
            &pyodide_dir.path().join("pyodide.mjs"),
            r#"
export async function loadPyodide(options) {
  return {
    setStdin(_stdin) {},
    async loadPackage(packages) {
      options.stdout(`packages:${packages.join(',')}`);
      options.stderr(`base:${options.packageBaseUrl}`);
    },
    async runPythonAsync(code) {
      options.stdout(`code:${code}`);
    },
  };
}
"#,
        );
        write_fixture(
            &pyodide_dir.path().join("pyodide-lock.json"),
            "{\"packages\":[]}\n",
        );

        let output = run_python_runner_with_env(
            &import_cache,
            pyodide_dir.path(),
            "print('hello')",
            &[("AGENTOS_PYTHON_PRELOAD_PACKAGES", "[\"numpy\",\"pandas\"]")],
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let expected_package_base = format!(
            "base:{}{}",
            pyodide_dir.path().display(),
            std::path::MAIN_SEPARATOR
        );

        assert_eq!(output.status.code(), Some(0));
        assert_eq!(
            stdout,
            "packages:micropip\npackages:numpy,pandas\ncode:print('hello')\n"
        );
        assert!(
            stderr.contains(&expected_package_base),
            "expected local package base path in stderr, got: {stderr}"
        );
    }

    #[test]
    fn materialized_python_runner_rejects_unknown_preload_packages() {
        assert_node_available();

        let import_cache = NodeImportCache::default();
        import_cache
            .ensure_materialized()
            .expect("materialize node import cache");

        let pyodide_dir = tempdir().expect("create pyodide fixture dir");
        write_fixture(
            &pyodide_dir.path().join("pyodide.mjs"),
            r#"
export async function loadPyodide() {
  return {
    setStdin(_stdin) {},
    async loadPackage() {
      throw new Error('loadPackage should not be called');
    },
    async runPythonAsync(_code) {},
  };
}
"#,
        );
        write_fixture(
            &pyodide_dir.path().join("pyodide-lock.json"),
            "{\"packages\":[]}\n",
        );

        let output = run_python_runner_with_env(
            &import_cache,
            pyodide_dir.path(),
            "print('hello')",
            &[("AGENTOS_PYTHON_PRELOAD_PACKAGES", "[\"requests\"]")],
        );
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert_eq!(output.status.code(), Some(1));
        assert!(
            stderr.contains("Unsupported bundled Python package \"requests\""),
            "unexpected stderr: {stderr}"
        );
        assert!(
            stderr.contains("Available packages: numpy, pandas"),
            "unexpected stderr: {stderr}"
        );
        assert!(
            !stderr.contains("loadPackage should not be called"),
            "runner should validate packages before calling loadPackage: {stderr}"
        );
    }

    #[test]
    fn materialized_python_runner_streams_multiple_stdin_reads_through_pyodide() {
        assert_node_available();

        let import_cache = NodeImportCache::default();
        import_cache
            .ensure_materialized()
            .expect("materialize node import cache");

        let pyodide_dir = tempdir().expect("create pyodide fixture dir");
        write_fixture(
            &pyodide_dir.path().join("pyodide.mjs"),
            r#"
const decoder = new TextDecoder();

export async function loadPyodide(options) {
  let stdin = null;

  function createInputReader() {
    let buffered = '';

    return () => {
      while (true) {
        const newlineIndex = buffered.indexOf('\n');
        if (newlineIndex >= 0) {
          const line = buffered.slice(0, newlineIndex);
          buffered = buffered.slice(newlineIndex + 1);
          return line;
        }

        const chunk = new Uint8Array(64);
        const bytesRead = stdin.read(chunk);
        if (bytesRead === 0) {
          const tail = buffered;
          buffered = '';
          return tail;
        }

        buffered += decoder.decode(chunk.subarray(0, bytesRead));
      }
    };
  }

  return {
    setStdin(config) {
      stdin = config;
    },
    async runPythonAsync(code) {
      const input = createInputReader();
      options.stdout(`first:${input()}`);
      options.stdout(`second:${input()}`);
      options.stdout(`tail:${JSON.stringify(input())}`);
      options.stdout(`code:${code}`);
    },
  };
}
"#,
        );
        write_fixture(
            &pyodide_dir.path().join("pyodide-lock.json"),
            "{\"packages\":[]}\n",
        );

        let output = run_python_runner_with_env_and_stdin(
            &import_cache,
            pyodide_dir.path(),
            "print('interactive')",
            &[],
            &[b"first line\n", b"second line\n"],
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
        assert!(
            stdout.contains("first:first line\n"),
            "unexpected stdout: {stdout}"
        );
        assert!(
            stdout.contains("second:second line\n"),
            "unexpected stdout: {stdout}"
        );
        assert!(stdout.contains("tail:\"\""), "unexpected stdout: {stdout}");
        assert!(
            stdout.contains("code:print('interactive')"),
            "unexpected stdout: {stdout}"
        );
    }

    #[test]
    fn ensure_materialized_writes_bundled_pyodide_distribution_assets() {
        let import_cache = NodeImportCache::default();
        import_cache
            .ensure_materialized()
            .expect("materialize node import cache");

        for file_name in [
            "pyodide.mjs",
            "pyodide.asm.js",
            "pyodide.asm.wasm",
            "pyodide-lock.json",
            "python_stdlib.zip",
            "numpy-2.2.5-cp313-cp313-pyodide_2025_0_wasm32.whl",
            "pandas-2.3.3-cp313-cp313-pyodide_2025_0_wasm32.whl",
            "python_dateutil-2.9.0.post0-py2.py3-none-any.whl",
            "pytz-2025.2-py2.py3-none-any.whl",
            "six-1.17.0-py2.py3-none-any.whl",
        ] {
            assert!(
                import_cache.pyodide_dist_path().join(file_name).is_file(),
                "expected bundled Pyodide asset {file_name} to be materialized"
            );
        }
    }

    #[test]
    fn ensure_materialized_honors_configured_timeout() {
        let _delay_guard = NODE_IMPORT_CACHE_TEST_MATERIALIZE_DELAY_LOCK
            .lock()
            .expect("materialization delay test lock");
        let temp_root = tempdir().expect("create node import cache temp root");
        let import_cache = NodeImportCache::new_in(temp_root.path().to_path_buf());

        NODE_IMPORT_CACHE_TEST_MATERIALIZE_DELAY_MS.store(50, Ordering::Relaxed);
        let error = import_cache
            .ensure_materialized_with_timeout(Duration::from_millis(5))
            .expect_err("materialization should time out");
        NODE_IMPORT_CACHE_TEST_MATERIALIZE_DELAY_MS.store(0, Ordering::Relaxed);

        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(
            error
                .to_string()
                .contains("timed out materializing node import cache"),
            "unexpected error: {error}"
        );

        std::thread::sleep(Duration::from_millis(75));
    }

    #[test]
    fn timed_out_materialization_cannot_recreate_dropped_cache_root() {
        let _delay_guard = NODE_IMPORT_CACHE_TEST_MATERIALIZE_DELAY_LOCK
            .lock()
            .expect("materialization delay test lock");
        let temp_root = tempdir().expect("create node import cache temp root");
        let import_cache = NodeImportCache::new_in(temp_root.path().to_path_buf());
        let cache_root = import_cache.root_dir.clone();

        NODE_IMPORT_CACHE_TEST_MATERIALIZE_DELAY_MS.store(50, Ordering::Relaxed);
        let error = import_cache
            .ensure_materialized_with_timeout(Duration::from_millis(5))
            .expect_err("materialization should time out");
        NODE_IMPORT_CACHE_TEST_MATERIALIZE_DELAY_MS.store(0, Ordering::Relaxed);
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);

        drop(import_cache);
        std::thread::sleep(Duration::from_millis(75));
        assert!(
            !cache_root.exists(),
            "timed-out materializer recreated the cache root after teardown: {}",
            cache_root.display()
        );
    }

    #[test]
    fn ensure_materialized_skips_repeated_materialization_after_success() {
        let _delay_guard = NODE_IMPORT_CACHE_TEST_MATERIALIZE_DELAY_LOCK
            .lock()
            .expect("materialization delay test lock");
        let temp_root = tempdir().expect("create node import cache temp root");
        let import_cache = NodeImportCache::new_in(temp_root.path().to_path_buf());

        import_cache
            .ensure_materialized()
            .expect("initial materialization should succeed");

        NODE_IMPORT_CACHE_TEST_MATERIALIZE_DELAY_MS.store(50, Ordering::Relaxed);
        let result = import_cache.ensure_materialized_with_timeout(Duration::from_millis(5));
        NODE_IMPORT_CACHE_TEST_MATERIALIZE_DELAY_MS.store(0, Ordering::Relaxed);
        result.expect("second materialization should use memoized success");
    }

    #[test]
    fn ensure_materialized_repairs_an_externally_removed_cache_root() {
        let temp_root = tempdir().expect("create node import cache temp root");
        let import_cache = NodeImportCache::new_in(temp_root.path().to_path_buf());

        import_cache
            .ensure_materialized()
            .expect("initial materialization should succeed");
        assert!(import_cache.wasm_runner_path().is_file());

        fs::remove_dir_all(&import_cache.root_dir).expect("remove materialized cache root");
        import_cache
            .ensure_materialized()
            .expect("missing materialized cache should be repaired");

        assert!(import_cache.wasm_runner_path().is_file());
        assert!(import_cache.materialization_marker_path.is_file());
    }

    #[test]
    fn new_in_cleans_stale_temp_roots_without_touching_unrelated_entries() {
        let temp_root = tempdir().expect("create node import cache temp root");
        let stale_cache_dir = temp_root
            .path()
            .join("agentos-node-import-cache-stale-test");
        let unrelated_dir = temp_root.path().join("keep-me");
        fs::create_dir_all(&stale_cache_dir).expect("create stale cache dir");
        fs::create_dir_all(&unrelated_dir).expect("create unrelated dir");
        fs::write(stale_cache_dir.join("state.json"), b"stale").expect("seed stale cache");

        let import_cache = NodeImportCache::new_in(temp_root.path().to_path_buf());

        assert!(
            !stale_cache_dir.exists(),
            "expected stale cache dir to be removed"
        );
        assert!(unrelated_dir.exists(), "expected unrelated dir to remain");
        assert!(
            import_cache.root_dir.starts_with(temp_root.path()),
            "expected import cache root to stay inside the configured temp root"
        );
    }

    #[test]
    fn materialized_loader_prunes_persisted_resolution_cache_state() {
        assert_node_available();

        let temp_root = tempdir().expect("create node import cache temp root");
        let workspace = tempdir().expect("create loader test workspace");
        let import_cache = NodeImportCache::new_in(temp_root.path().to_path_buf());
        import_cache
            .ensure_materialized()
            .expect("materialize node import cache");

        let driver_path = workspace.path().join("drive-loader-cache.mjs");
        write_fixture(
            &driver_path,
            r#"
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const [loaderPath, workspaceRoot] = process.argv.slice(2);
const loader = await import(`${pathToFileURL(loaderPath).href}?case=${process.pid}-${Date.now()}`);
const parentURL = pathToFileURL(path.join(workspaceRoot, 'entry.mjs')).href;

for (let index = 0; index < 600; index += 1) {
  const specifier = `pkg-${index}`;
  const resolvedPath = path.join(workspaceRoot, 'node_modules', specifier, 'index.mjs');
  await loader.resolve(specifier, { parentURL }, async () => ({
    url: pathToFileURL(resolvedPath).href,
    format: 'module',
  }));
}
"#,
        );

        let output = Command::new(node_binary())
            .arg(&driver_path)
            .arg(&import_cache.loader_path)
            .arg(workspace.path())
            .env("AGENTOS_NODE_IMPORT_CACHE_PATH", import_cache.cache_path())
            .env(
                "AGENTOS_NODE_IMPORT_CACHE_ASSET_ROOT",
                import_cache.asset_root(),
            )
            .output()
            .expect("run loader cache driver");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");

        let state: Value = serde_json::from_str(
            &fs::read_to_string(import_cache.cache_path()).expect("read cache state"),
        )
        .expect("parse cache state");
        let resolutions = state["resolutions"]
            .as_object()
            .expect("resolution cache object");

        assert_eq!(resolutions.len(), 512);
        assert!(
            resolutions.keys().any(|key| key.contains("pkg-599")),
            "newest resolution should be retained"
        );
        assert!(
            !resolutions.keys().any(|key| key.contains("pkg-0\"")),
            "oldest resolution should be pruned"
        );
    }

    #[test]
    fn materialized_loader_ignores_oversized_state_during_flush_merge() {
        assert_node_available();

        let temp_root = tempdir().expect("create node import cache temp root");
        let workspace = tempdir().expect("create loader test workspace");
        let import_cache = NodeImportCache::new_in(temp_root.path().to_path_buf());
        import_cache
            .ensure_materialized()
            .expect("materialize node import cache");
        fs::create_dir_all(import_cache.cache_path().parent().expect("cache parent"))
            .expect("create cache parent");
        fs::write(import_cache.cache_path(), vec![b' '; 5 * 1024 * 1024])
            .expect("seed oversized cache state");

        let driver_path = workspace.path().join("drive-oversized-state.mjs");
        write_fixture(
            &driver_path,
            r#"
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const [loaderPath, workspaceRoot] = process.argv.slice(2);
const loader = await import(`${pathToFileURL(loaderPath).href}?case=oversized-${process.pid}-${Date.now()}`);
const parentURL = pathToFileURL(path.join(workspaceRoot, 'entry.mjs')).href;
await loader.resolve('pkg-fresh', { parentURL }, async () => ({
  url: pathToFileURL(path.join(workspaceRoot, 'node_modules/pkg-fresh/index.mjs')).href,
  format: 'module',
}));
"#,
        );

        let output = Command::new(node_binary())
            .arg(&driver_path)
            .arg(&import_cache.loader_path)
            .arg(workspace.path())
            .env("AGENTOS_NODE_IMPORT_CACHE_PATH", import_cache.cache_path())
            .env(
                "AGENTOS_NODE_IMPORT_CACHE_ASSET_ROOT",
                import_cache.asset_root(),
            )
            .output()
            .expect("run oversized state driver");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");

        let state_contents =
            fs::read_to_string(import_cache.cache_path()).expect("read rewritten cache state");
        assert!(
            state_contents.len() < 4 * 1024 * 1024,
            "cache state should be rewritten below the hard limit"
        );
        let state: Value = serde_json::from_str(&state_contents).expect("parse cache state");
        assert_eq!(
            state["resolutions"]
                .as_object()
                .expect("resolution cache object")
                .len(),
            1
        );
    }

    #[test]
    fn materialized_loader_prunes_unreferenced_projected_source_files() {
        assert_node_available();

        let temp_root = tempdir().expect("create node import cache temp root");
        let workspace = tempdir().expect("create loader test workspace");
        let import_cache = NodeImportCache::new_in(temp_root.path().to_path_buf());
        import_cache
            .ensure_materialized()
            .expect("materialize node import cache");
        let node_modules = workspace.path().join("node_modules");
        fs::create_dir_all(&node_modules).expect("create node_modules");
        for index in 0..520 {
            let package_dir = node_modules.join(format!("pkg-{index}"));
            fs::create_dir_all(&package_dir).expect("create package dir");
            fs::write(
                package_dir.join("index.mjs"),
                format!("import fs from 'node:fs';\nexport const value = {index};\n"),
            )
            .expect("write package source");
        }

        let driver_path = workspace.path().join("drive-projected-source-cache.mjs");
        write_fixture(
            &driver_path,
            r#"
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const [loaderPath, workspaceRoot] = process.argv.slice(2);
const loader = await import(`${pathToFileURL(loaderPath).href}?case=projected-${process.pid}-${Date.now()}`);

for (let index = 0; index < 520; index += 1) {
  const filePath = path.join(workspaceRoot, 'node_modules', `pkg-${index}`, 'index.mjs');
  await loader.load(pathToFileURL(filePath).href, { format: 'module' }, async () => {
    throw new Error('nextLoad should not run for projected package sources');
  });
}
"#,
        );

        let guest_path_mappings = format!(
            r#"[{{"guestPath":"/root/node_modules","hostPath":"{}"}}]"#,
            node_modules.display()
        );
        let output = Command::new(node_binary())
            .arg(&driver_path)
            .arg(&import_cache.loader_path)
            .arg(workspace.path())
            .env("AGENTOS_NODE_IMPORT_CACHE_PATH", import_cache.cache_path())
            .env(
                "AGENTOS_NODE_IMPORT_CACHE_ASSET_ROOT",
                import_cache.asset_root(),
            )
            .env("AGENTOS_GUEST_PATH_MAPPINGS", guest_path_mappings)
            .output()
            .expect("run projected source cache driver");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");

        let projected_source_root = import_cache
            .cache_path()
            .parent()
            .expect("cache parent")
            .join("projected-sources");
        let cached_file_count = fs::read_dir(&projected_source_root)
            .expect("read projected source cache")
            .count();
        assert_eq!(cached_file_count, 512);
    }

    #[test]
    fn ensure_materialized_writes_denied_builtin_assets_for_hardened_modules() {
        let import_cache = NodeImportCache::default();
        import_cache
            .ensure_materialized()
            .expect("materialize node import cache");

        let denied_root = import_cache.asset_root().join("denied");
        let actual = fs::read_dir(&denied_root)
            .expect("read denied builtin assets")
            .map(|entry| {
                entry
                    .expect("denied builtin asset entry")
                    .path()
                    .file_stem()
                    .expect("denied builtin asset file stem")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<BTreeSet<_>>();
        let expected = BTreeSet::from([
            String::from("child_process"),
            String::from("cluster"),
            String::from("dgram"),
            String::from("http"),
            String::from("http2"),
            String::from("https"),
            String::from("inspector"),
            String::from("module"),
            String::from("net"),
            String::from("trace_events"),
        ]);

        assert_eq!(actual, expected);

        let module_asset =
            fs::read_to_string(denied_root.join("module.mjs")).expect("read module denied asset");
        let trace_events_asset = fs::read_to_string(denied_root.join("trace_events.mjs"))
            .expect("read trace_events denied asset");

        assert!(module_asset.contains("node:module is not available"));
        assert!(trace_events_asset.contains("ERR_ACCESS_DENIED"));
    }

    #[test]
    fn ensure_materialized_writes_v8_vm_and_worker_threads_builtin_assets() {
        let import_cache = NodeImportCache::default();
        import_cache
            .ensure_materialized()
            .expect("materialize node import cache");

        let builtins_root = import_cache.asset_root().join("builtins");
        let v8_asset =
            fs::read_to_string(builtins_root.join("v8.mjs")).expect("read v8 builtin asset");
        let vm_asset =
            fs::read_to_string(builtins_root.join("vm.mjs")).expect("read vm builtin asset");
        let worker_threads_asset = fs::read_to_string(builtins_root.join("worker-threads.mjs"))
            .expect("read worker_threads builtin asset");

        assert!(v8_asset.contains("process.getBuiltinModule?.(\"node:v8\")"));
        assert!(v8_asset.contains("export const cachedDataVersionTag = mod.cachedDataVersionTag;"));
        assert!(vm_asset.contains("process.getBuiltinModule?.(\"node:vm\")"));
        assert!(vm_asset.contains("export const runInThisContext = mod.runInThisContext;"));
        assert!(worker_threads_asset.contains("class Worker"));
        assert!(worker_threads_asset.contains("export const isMainThread = mod.isMainThread;"));
    }

    #[test]
    fn ensure_materialized_writes_async_and_diagnostics_builtin_assets() {
        let import_cache = NodeImportCache::default();
        import_cache
            .ensure_materialized()
            .expect("materialize node import cache");

        let builtins_root = import_cache.asset_root().join("builtins");
        let async_hooks_asset = fs::read_to_string(builtins_root.join("async-hooks.mjs"))
            .expect("read async_hooks builtin asset");
        let diagnostics_asset = fs::read_to_string(builtins_root.join("diagnostics-channel.mjs"))
            .expect("read diagnostics_channel builtin asset");

        assert!(async_hooks_asset.contains("class AsyncLocalStorage"));
        assert!(async_hooks_asset.contains("function createHook()"));
        assert!(diagnostics_asset.contains("function channel(name = '')"));
        assert!(diagnostics_asset.contains("class Channel"));
        assert!(diagnostics_asset.contains("function tracingChannel(name = '')"));
    }

    #[test]
    fn ensure_materialized_writes_os_builtin_asset() {
        let import_cache = NodeImportCache::default();
        import_cache
            .ensure_materialized()
            .expect("materialize node import cache");

        let os_asset =
            fs::read_to_string(import_cache.asset_root().join("builtins").join("os.mjs"))
                .expect("read os builtin asset");

        assert!(os_asset.contains("__agentOSBuiltinOs"));
        assert!(os_asset.contains("export const hostname = mod.hostname"));
        assert!(os_asset.contains("export const userInfo = mod.userInfo"));
    }

    #[test]
    fn ensure_materialized_writes_http_builtin_assets() {
        let import_cache = NodeImportCache::default();
        import_cache
            .ensure_materialized()
            .expect("materialize node import cache");

        let builtins_root = import_cache.asset_root().join("builtins");
        let http_asset =
            fs::read_to_string(builtins_root.join("http.mjs")).expect("read http builtin asset");
        let http2_asset =
            fs::read_to_string(builtins_root.join("http2.mjs")).expect("read http2 builtin asset");
        let https_asset =
            fs::read_to_string(builtins_root.join("https.mjs")).expect("read https builtin asset");

        assert!(http_asset.contains("__agentOSBuiltinHttp"));
        assert!(http_asset.contains("export const request = mod.request"));
        assert!(http2_asset.contains("__agentOSBuiltinHttp2"));
        assert!(http2_asset.contains("export const connect = mod.connect"));
        assert!(https_asset.contains("__agentOSBuiltinHttps"));
        assert!(https_asset.contains("export const createServer = mod.createServer"));
    }

    #[test]
    fn ensure_materialized_writes_net_builtin_asset() {
        let import_cache = NodeImportCache::default();
        import_cache
            .ensure_materialized()
            .expect("materialize node import cache");

        let net_asset =
            fs::read_to_string(import_cache.asset_root().join("builtins").join("net.mjs"))
                .expect("read net builtin asset");

        assert!(net_asset.contains("__agentOSBuiltinNet"));
        assert!(net_asset.contains("export const connect = mod.connect"));
        assert!(net_asset.contains("export const createServer = mod.createServer"));
    }

    #[test]
    fn ensure_materialized_writes_dgram_builtin_asset() {
        let import_cache = NodeImportCache::default();
        import_cache
            .ensure_materialized()
            .expect("materialize node import cache");

        let dgram_asset =
            fs::read_to_string(import_cache.asset_root().join("builtins").join("dgram.mjs"))
                .expect("read dgram builtin asset");

        assert!(dgram_asset.contains("__agentOSBuiltinDgram"));
        assert!(dgram_asset.contains("export const Socket = mod.Socket"));
        assert!(dgram_asset.contains("export const createSocket = mod.createSocket"));
    }

    #[test]
    fn ensure_materialized_writes_dns_builtin_asset() {
        let import_cache = NodeImportCache::default();
        import_cache
            .ensure_materialized()
            .expect("materialize node import cache");

        let dns_asset =
            fs::read_to_string(import_cache.asset_root().join("builtins").join("dns.mjs"))
                .expect("read dns builtin asset");

        assert!(dns_asset.contains("__agentOSBuiltinDns"));
        assert!(dns_asset.contains("export const Resolver = mod.Resolver"));
        assert!(dns_asset.contains("export const lookup = mod.lookup"));
        assert!(dns_asset.contains("export const resolve4 = mod.resolve4"));
    }

    #[test]
    fn ensure_materialized_writes_dns_promises_builtin_asset() {
        let import_cache = NodeImportCache::default();
        import_cache
            .ensure_materialized()
            .expect("materialize node import cache");

        let dns_promises_asset = fs::read_to_string(
            import_cache
                .asset_root()
                .join("builtins")
                .join("dns-promises.mjs"),
        )
        .expect("read dns promises builtin asset");

        assert!(dns_promises_asset.contains("__agentOSBuiltinDns.promises"));
        assert!(dns_promises_asset.contains("export const Resolver = mod.Resolver"));
        assert!(dns_promises_asset.contains("export const resolve4 = mod.resolve4"));
    }

    #[test]
    fn wasm_runner_preopens_guest_cwd_before_root() {
        let cwd_index = NODE_WASM_RUNNER_SOURCE
            .find("preopens[cwdMount] = createPreopen(HOST_CWD, cwdReadOnly);")
            .expect("runner should preopen the guest cwd");
        let root_index = NODE_WASM_RUNNER_SOURCE
            .find("preopens['/'] = createPreopen(rootMapping.hostPath, rootMapping.readOnly);")
            .expect("runner should preopen the guest root");

        assert!(cwd_index < root_index);
    }

    #[test]
    fn wasm_runner_preserves_read_only_mappings_in_preopens() {
        assert!(NODE_WASM_RUNNER_SOURCE
            .contains("? { guestPath, hostPath, readOnly: entry.readOnly === true }"));
        assert!(NODE_WASM_RUNNER_SOURCE.contains("readOnly: readOnly === true,"));
        assert!(NODE_WASM_RUNNER_SOURCE.contains("resolveModuleGuestPathToHostMapping"));
        assert!(NODE_WASM_RUNNER_SOURCE.contains("rightsBase: READ_ONLY_PREOPEN_RIGHTS_BASE,"));
        assert!(NODE_WASM_RUNNER_SOURCE
            .contains("preopens[guestPath] = createPreopen(mapping.hostPath, mapping.readOnly);"));
        assert!(NODE_WASM_RUNNER_SOURCE.contains("const cwdReadOnly = readOnlyForCwd(guestCwd);"));
        assert!(NODE_WASM_RUNNER_SOURCE
            .contains("preopens[cwdMount] = createPreopen(HOST_CWD, cwdReadOnly);"));
        assert!(NODE_WASM_RUNNER_SOURCE
            .contains("if (mapping.readOnly) {\n        return WASI_ERRNO_ROFS;\n      }"));
        assert!(NODE_WASM_RUNNER_SOURCE.contains("readOnly: preopenSpec?.readOnly === true,"));
        assert!(NODE_WASM_RUNNER_SOURCE
            .contains("resolveModuleGuestPathToHostMapping(guestPath)?.readOnly === true"));
        assert!(NODE_WASM_RUNNER_SOURCE
            .contains("if (handle?.readOnly === true) {\n        return 1;\n      }"));
    }

    #[test]
    fn ensure_materialized_writes_tls_builtin_asset() {
        let import_cache = NodeImportCache::default();
        import_cache
            .ensure_materialized()
            .expect("materialize node import cache");

        let tls_asset =
            fs::read_to_string(import_cache.asset_root().join("builtins").join("tls.mjs"))
                .expect("read tls builtin asset");

        assert!(tls_asset.contains("__agentOSBuiltinTls"));
        assert!(tls_asset.contains("export const connect = mod.connect"));
        assert!(tls_asset.contains("export const createServer = mod.createServer"));
    }
}
