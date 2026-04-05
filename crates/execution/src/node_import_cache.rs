use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) const NODE_IMPORT_CACHE_DEBUG_ENV: &str = "AGENT_OS_NODE_IMPORT_CACHE_DEBUG";
pub(crate) const NODE_IMPORT_CACHE_METRICS_PREFIX: &str = "__AGENT_OS_NODE_IMPORT_CACHE_METRICS__:";
pub(crate) const NODE_IMPORT_CACHE_ASSET_ROOT_ENV: &str = "AGENT_OS_NODE_IMPORT_CACHE_ASSET_ROOT";

const NODE_IMPORT_CACHE_PATH_ENV: &str = "AGENT_OS_NODE_IMPORT_CACHE_PATH";
const NODE_IMPORT_CACHE_LOADER_PATH_ENV: &str = "AGENT_OS_NODE_IMPORT_CACHE_LOADER_PATH";
const NODE_IMPORT_CACHE_SCHEMA_VERSION: &str = "1";
const NODE_IMPORT_CACHE_LOADER_VERSION: &str = "6";
const NODE_IMPORT_CACHE_ASSET_VERSION: &str = "3";
const PYODIDE_DIST_DIR: &str = "pyodide-dist";
const AGENT_OS_BUILTIN_SPECIFIER_PREFIX: &str = "agent-os:builtin/";
const AGENT_OS_POLYFILL_SPECIFIER_PREFIX: &str = "agent-os:polyfill/";
const BUNDLED_PYODIDE_MJS: &[u8] = include_bytes!("../assets/pyodide/pyodide.mjs");
const BUNDLED_PYODIDE_ASM_JS: &[u8] = include_bytes!("../assets/pyodide/pyodide.asm.js");
const BUNDLED_PYODIDE_ASM_WASM: &[u8] = include_bytes!("../assets/pyodide/pyodide.asm.wasm");
const BUNDLED_PYODIDE_LOCK: &[u8] = include_bytes!("../assets/pyodide/pyodide-lock.json");
const BUNDLED_PYTHON_STDLIB_ZIP: &[u8] = include_bytes!("../assets/pyodide/python_stdlib.zip");
const BUNDLED_NUMPY_WHL: &[u8] =
    include_bytes!("../assets/pyodide/numpy-2.2.5-cp313-cp313-pyodide_2025_0_wasm32.whl");
const BUNDLED_PANDAS_WHL: &[u8] =
    include_bytes!("../assets/pyodide/pandas-2.3.3-cp313-cp313-pyodide_2025_0_wasm32.whl");
const BUNDLED_PYTHON_DATEUTIL_WHL: &[u8] =
    include_bytes!("../assets/pyodide/python_dateutil-2.9.0.post0-py2.py3-none-any.whl");
const BUNDLED_PYTZ_WHL: &[u8] =
    include_bytes!("../assets/pyodide/pytz-2025.2-py2.py3-none-any.whl");
const BUNDLED_SIX_WHL: &[u8] = include_bytes!("../assets/pyodide/six-1.17.0-py2.py3-none-any.whl");

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
];
const NODE_IMPORT_CACHE_LOADER_TEMPLATE: &str = r#"
import crypto from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const GUEST_PATH_MAPPINGS = parseGuestPathMappings(process.env.AGENT_OS_GUEST_PATH_MAPPINGS);
const ALLOWED_BUILTINS = new Set(parseJsonArray(process.env.AGENT_OS_ALLOWED_NODE_BUILTINS));
const CACHE_PATH = process.env.__NODE_IMPORT_CACHE_PATH_ENV__;
const CACHE_ROOT = CACHE_PATH ? path.dirname(CACHE_PATH) : null;
const GUEST_INTERNAL_CACHE_ROOT = '/.agent-os/node-import-cache';
const PROJECTED_SOURCE_CACHE_ROOT = CACHE_PATH
  ? path.join(path.dirname(CACHE_PATH), 'projected-sources')
  : null;
const ASSET_ROOT = process.env.__NODE_IMPORT_CACHE_ASSET_ROOT_ENV__;
const DEBUG_ENABLED = process.env.__NODE_IMPORT_CACHE_DEBUG_ENV__ === '1';
const CONTROL_PIPE_FD = parseControlPipeFd(process.env.AGENT_OS_CONTROL_PIPE_FD);
const SCHEMA_VERSION = '__NODE_IMPORT_CACHE_SCHEMA_VERSION__';
const LOADER_VERSION = '__NODE_IMPORT_CACHE_LOADER_VERSION__';
const ASSET_VERSION = '__NODE_IMPORT_CACHE_ASSET_VERSION__';
const BUILTIN_PREFIX = '__AGENT_OS_BUILTIN_SPECIFIER_PREFIX__';
const POLYFILL_PREFIX = '__AGENT_OS_POLYFILL_SPECIFIER_PREFIX__';
const FS_ASSET_SPECIFIER = `${BUILTIN_PREFIX}fs`;
const FS_PROMISES_ASSET_SPECIFIER = `${BUILTIN_PREFIX}fs-promises`;
const CHILD_PROCESS_ASSET_SPECIFIER = `${BUILTIN_PREFIX}child-process`;
const NET_ASSET_SPECIFIER = `${BUILTIN_PREFIX}net`;
const DGRAM_ASSET_SPECIFIER = `${BUILTIN_PREFIX}dgram`;
const DNS_ASSET_SPECIFIER = `${BUILTIN_PREFIX}dns`;
const TLS_ASSET_SPECIFIER = `${BUILTIN_PREFIX}tls`;
const OS_ASSET_SPECIFIER = `${BUILTIN_PREFIX}os`;
const DENIED_BUILTINS = new Set([
  'child_process',
  'cluster',
  'dgram',
  'diagnostics_channel',
  'dns',
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
      const existing = JSON.parse(fs.readFileSync(CACHE_PATH, 'utf8'));
      if (isCompatibleCacheState(existing)) {
        merged = mergeCacheStates(normalizeCacheState(existing), cacheState);
      }
    } catch {
      // Ignore missing or unreadable prior state and replace it with the in-memory view.
    }

    const tempPath = `${CACHE_PATH}.${process.pid}.${Date.now()}.tmp`;
    fs.writeFileSync(tempPath, JSON.stringify(merged));
    fs.renameSync(tempPath, CACHE_PATH);
    cacheState = merged;
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
  return Number.isInteger(parsed) && parsed >= 0 ? parsed : null;
}

function emitControlMessage(message) {
  if (CONTROL_PIPE_FD == null) {
    return;
  }

  try {
    fs.writeSync(CONTROL_PIPE_FD, `${JSON.stringify(message)}\n`);
  } catch {
    // Ignore control-channel write failures during teardown.
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
  return {
    ...emptyCacheState(),
    ...value,
    resolutions: isRecord(value.resolutions) ? value.resolutions : {},
    packageTypes: isRecord(value.packageTypes) ? value.packageTypes : {},
    moduleFormats: isRecord(value.moduleFormats) ? value.moduleFormats : {},
    projectedSources: isRecord(value.projectedSources) ? value.projectedSources : {},
  };
}

function mergeCacheStates(base, current) {
  return {
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
  };
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
    case 'child_process':
      return ALLOWED_BUILTINS.has('child_process')
        ? assetModuleDescriptor(path.join(ASSET_ROOT, 'builtins', 'child-process.mjs'))
        : null;
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

  const guestPath = guestPathFromHostPath(hostPath);
  return guestPath ? pathToFileURL(guestPath).href : url;
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
  return guestPathFromHostPath(hostPath) ?? guestInternalPathFromHostPath(hostPath);
}

function translatePathStringToGuest(value) {
  if (typeof value !== 'string') {
    return value;
  }

  if (value.startsWith('file:')) {
    const hostPath = guestFilePathFromUrl(value);
    const guestPath = hostPath ? guestVisiblePathFromHostPath(hostPath) : null;
    return guestPath ? pathToFileURL(guestPath).href : value;
  }

  if (!path.isAbsolute(value)) {
    return value;
  }

  return guestVisiblePathFromHostPath(value) ?? value;
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

  return [...replacements.entries()].sort((left, right) => right[0].length - left[0].length);
}

function translateTextToGuest(value) {
  if (typeof value !== 'string' || value.length === 0) {
    return value;
  }

  let translated = value;
  for (const [hostValue, guestValue] of buildHostToGuestTextReplacements()) {
    translated = translated.split(hostValue).join(guestValue);
  }
  return translated;
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
  return typeof filePath === 'string' && filePath.includes(`${path.sep}agent-os-node-import-cache-`);
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

const NODE_EXECUTION_RUNNER_SOURCE: &str = r#"
const fs = process.getBuiltinModule?.('node:fs');
const path = process.getBuiltinModule?.('node:path');
const { pathToFileURL } = process.getBuiltinModule?.('node:url') ?? {};

if (!fs || !path || typeof pathToFileURL !== 'function') {
  throw new Error('node builtin access is required for the Agent OS guest runtime');
}

const HOST_PROCESS_ENV = { ...process.env };
const Module =
  typeof process.getBuiltinModule === 'function'
    ? process.getBuiltinModule('node:module')
    : null;
const syncBuiltinESMExports =
  typeof Module?.syncBuiltinESMExports === 'function'
    ? Module.syncBuiltinESMExports.bind(Module)
    : () => {};
const GUEST_PATH_MAPPINGS = parseGuestPathMappings(HOST_PROCESS_ENV.AGENT_OS_GUEST_PATH_MAPPINGS);
const ALLOWED_BUILTINS = new Set(parseJsonArray(HOST_PROCESS_ENV.AGENT_OS_ALLOWED_NODE_BUILTINS));
const LOOPBACK_EXEMPT_PORTS = new Set(parseJsonArray(HOST_PROCESS_ENV.AGENT_OS_LOOPBACK_EXEMPT_PORTS));
const DENIED_BUILTINS = new Set([
  'child_process',
  'cluster',
  'dgram',
  'diagnostics_channel',
  'dns',
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
const originalGetBuiltinModule =
  typeof process.getBuiltinModule === 'function'
    ? process.getBuiltinModule.bind(process)
    : null;
const originalModuleLoad =
  typeof Module?._load === 'function' ? Module._load.bind(Module) : null;
const originalFetch =
  typeof globalThis.fetch === 'function'
    ? globalThis.fetch.bind(globalThis)
    : null;
const HOST_CWD = process.cwd();
const HOST_EXEC_PATH = process.execPath;
const HOST_EXEC_DIR = path.dirname(HOST_EXEC_PATH);
if (!Module || typeof Module.createRequire !== 'function') {
  throw new Error('node:module builtin access is required for the Agent OS guest runtime');
}
const hostRequire = Module.createRequire(import.meta.url);
const hostOs = hostRequire('node:os');
const hostNet = hostRequire('node:net');
const hostDgram = hostRequire('node:dgram');
const hostDns = hostRequire('node:dns');
const hostTls = hostRequire('node:tls');
const { EventEmitter } = hostRequire('node:events');
const { Duplex, Readable, Writable } = hostRequire('node:stream');
const NODE_SYNC_RPC_ENABLE = HOST_PROCESS_ENV.AGENT_OS_NODE_SYNC_RPC_ENABLE === '1';
const hostWorkerThreads = NODE_SYNC_RPC_ENABLE ? hostRequire('node:worker_threads') : null;
const SIGNAL_EVENTS = new Set(
  Object.keys(hostOs.constants?.signals ?? {}).filter((name) =>
    name.startsWith('SIG'),
  ),
);
const guestEntryPoint =
  HOST_PROCESS_ENV.AGENT_OS_GUEST_ENTRYPOINT ?? HOST_PROCESS_ENV.AGENT_OS_ENTRYPOINT;
const DEFAULT_VIRTUAL_EXEC_PATH = '/usr/bin/node';
const DEFAULT_VIRTUAL_PID = 1;
const DEFAULT_VIRTUAL_PPID = 0;
const DEFAULT_VIRTUAL_UID = 0;
const DEFAULT_VIRTUAL_GID = 0;
const DEFAULT_VIRTUAL_OS_HOSTNAME = 'agent-os';
const DEFAULT_VIRTUAL_OS_TYPE = 'Linux';
const DEFAULT_VIRTUAL_OS_PLATFORM = 'linux';
const DEFAULT_VIRTUAL_OS_RELEASE = '6.8.0-agent-os';
const DEFAULT_VIRTUAL_OS_VERSION = '#1 SMP PREEMPT_DYNAMIC Agent OS';
const DEFAULT_VIRTUAL_OS_ARCH = 'x64';
const DEFAULT_VIRTUAL_OS_MACHINE = 'x86_64';
const DEFAULT_VIRTUAL_OS_CPU_MODEL = 'Agent OS Virtual CPU';
const DEFAULT_VIRTUAL_OS_CPU_COUNT = 1;
const DEFAULT_VIRTUAL_OS_TOTALMEM = 1024 * 1024 * 1024;
const DEFAULT_VIRTUAL_OS_FREEMEM = 768 * 1024 * 1024;
const DEFAULT_VIRTUAL_OS_USER = 'user';
const DEFAULT_VIRTUAL_OS_SHELL = '/bin/sh';
const DEFAULT_VIRTUAL_OS_TMPDIR = '/tmp';
const NODE_SYNC_RPC_REQUEST_FD = parseOptionalFd(HOST_PROCESS_ENV.AGENT_OS_NODE_SYNC_RPC_REQUEST_FD);
const NODE_SYNC_RPC_RESPONSE_FD = parseOptionalFd(HOST_PROCESS_ENV.AGENT_OS_NODE_SYNC_RPC_RESPONSE_FD);
const NODE_SYNC_RPC_DATA_BYTES = parsePositiveInt(
  HOST_PROCESS_ENV.AGENT_OS_NODE_SYNC_RPC_DATA_BYTES,
  4 * 1024 * 1024,
);
const NODE_SYNC_RPC_WAIT_TIMEOUT_MS = parsePositiveInt(
  HOST_PROCESS_ENV.AGENT_OS_NODE_SYNC_RPC_WAIT_TIMEOUT_MS,
  30_000,
);
const NODE_IMPORT_CACHE_PATH = HOST_PROCESS_ENV.AGENT_OS_NODE_IMPORT_CACHE_PATH ?? null;
const NODE_IMPORT_CACHE_ROOT =
  typeof NODE_IMPORT_CACHE_PATH === 'string' && NODE_IMPORT_CACHE_PATH.length > 0
    ? path.dirname(NODE_IMPORT_CACHE_PATH)
    : null;
const GUEST_INTERNAL_NODE_IMPORT_CACHE_ROOT = '/.agent-os/node-import-cache';
const VIRTUAL_EXEC_PATH = parseVirtualProcessString(
  HOST_PROCESS_ENV.AGENT_OS_VIRTUAL_PROCESS_EXEC_PATH,
  DEFAULT_VIRTUAL_EXEC_PATH,
);
const VIRTUAL_PID = parseVirtualProcessNumber(
  HOST_PROCESS_ENV.AGENT_OS_VIRTUAL_PROCESS_PID,
  DEFAULT_VIRTUAL_PID,
);
const VIRTUAL_PPID = parseVirtualProcessNumber(
  HOST_PROCESS_ENV.AGENT_OS_VIRTUAL_PROCESS_PPID,
  DEFAULT_VIRTUAL_PPID,
);
const VIRTUAL_UID = parseVirtualProcessNumber(
  HOST_PROCESS_ENV.AGENT_OS_VIRTUAL_PROCESS_UID,
  DEFAULT_VIRTUAL_UID,
);
const VIRTUAL_GID = parseVirtualProcessNumber(
  HOST_PROCESS_ENV.AGENT_OS_VIRTUAL_PROCESS_GID,
  DEFAULT_VIRTUAL_GID,
);

function isPathLike(specifier) {
  return specifier.startsWith('.') || specifier.startsWith('/') || specifier.startsWith('file:');
}

function toImportSpecifier(specifier) {
  if (specifier.startsWith('file:')) {
    return translatePathStringToGuest(specifier);
  }
  if (isPathLike(specifier)) {
    if (specifier.startsWith('/')) {
      return pathToFileURL(
        translatePathStringToGuest(
          pathExists(specifier) ? path.resolve(specifier) : path.posix.normalize(specifier),
        ),
      ).href;
    }
    return pathToFileURL(translatePathStringToGuest(path.resolve(HOST_CWD, specifier))).href;
  }
  return specifier;
}

function accessDenied(subject) {
  const error = new Error(`${subject} is not available in the Agent OS guest runtime`);
  error.code = 'ERR_ACCESS_DENIED';
  return error;
}

function normalizeBuiltin(specifier) {
  return specifier.startsWith('node:') ? specifier.slice('node:'.length) : specifier;
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

function pathExists(targetPath) {
  try {
    return fs.existsSync(targetPath);
  } catch {
    return false;
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

function parseOptionalFd(value) {
  if (value == null || value === '') {
    return null;
  }

  const parsed = Number.parseInt(value, 10);
  return Number.isInteger(parsed) && parsed >= 0 ? parsed : null;
}

function parsePositiveInt(value, fallback) {
  if (value == null || value === '') {
    return fallback;
  }

  const parsed = Number(value);
  return Number.isInteger(parsed) && parsed > 0 ? parsed : fallback;
}

function parseVirtualProcessNumber(value, fallback) {
  if (value == null || value === '') {
    return fallback;
  }

  const parsed = Number(value);
  return Number.isInteger(parsed) && parsed >= 0 ? parsed : fallback;
}

function parseVirtualProcessString(value, fallback) {
  return typeof value === 'string' && value.length > 0 ? value : fallback;
}

function isInternalProcessEnvKey(key) {
  return typeof key === 'string' && key.startsWith('AGENT_OS_');
}

function createGuestProcessEnv(env) {
  const guestEnv = {};

  for (const [key, value] of Object.entries(env ?? {})) {
    if (typeof value !== 'string' || isInternalProcessEnvKey(key)) {
      continue;
    }
    guestEnv[key] = value;
  }

  return new Proxy(guestEnv, {
    defineProperty(target, key, descriptor) {
      if (typeof key === 'string' && isInternalProcessEnvKey(key)) {
        return true;
      }

      const normalized = { ...descriptor };
      if ('value' in normalized) {
        normalized.value = String(normalized.value);
      }
      return Reflect.defineProperty(target, key, normalized);
    },
    deleteProperty(target, key) {
      if (typeof key === 'string' && isInternalProcessEnvKey(key)) {
        return true;
      }
      return Reflect.deleteProperty(target, key);
    },
    get(target, key, receiver) {
      if (typeof key === 'string' && isInternalProcessEnvKey(key)) {
        return undefined;
      }
      return Reflect.get(target, key, receiver);
    },
    getOwnPropertyDescriptor(target, key) {
      if (typeof key === 'string' && isInternalProcessEnvKey(key)) {
        return undefined;
      }
      return Reflect.getOwnPropertyDescriptor(target, key);
    },
    has(target, key) {
      if (typeof key === 'string' && isInternalProcessEnvKey(key)) {
        return false;
      }
      return Reflect.has(target, key);
    },
    ownKeys(target) {
      return Reflect.ownKeys(target).filter(
        (key) => typeof key !== 'string' || !isInternalProcessEnvKey(key),
      );
    },
    set(target, key, value, receiver) {
      if (typeof key === 'string' && isInternalProcessEnvKey(key)) {
        return true;
      }
      return Reflect.set(target, key, String(value), receiver);
    },
  });
}

function parseGuestPathMappings(value) {
  if (!value) {
    return [];
  }

  try {
    const parsed = JSON.parse(value);
    if (!Array.isArray(parsed)) {
      return [];
    }

    return parsed
      .map((entry) => {
        const guestPath =
          entry && typeof entry.guestPath === 'string'
            ? path.posix.normalize(entry.guestPath)
            : null;
        const hostPath =
          entry && typeof entry.hostPath === 'string'
            ? path.resolve(entry.hostPath)
            : null;
        return guestPath && hostPath ? { guestPath, hostPath } : null;
      })
      .filter(Boolean)
      .sort((left, right) => right.guestPath.length - left.guestPath.length);
  } catch {
    return [];
  }
}

function hostPathFromGuestPath(guestPath) {
  if (typeof guestPath !== 'string') {
    return null;
  }

  const normalized = path.posix.normalize(guestPath);
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

  return null;
}

function guestPathFromHostPath(hostPath) {
  if (typeof hostPath !== 'string') {
    return null;
  }

  const normalized = path.resolve(hostPath);
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

function guestInternalPathFromHostPath(hostPath) {
  if (typeof hostPath !== 'string' || !NODE_IMPORT_CACHE_ROOT) {
    return null;
  }

  const normalized = path.resolve(hostPath);
  const hostRoot = path.resolve(NODE_IMPORT_CACHE_ROOT);
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
    ? path.posix.join(
        GUEST_INTERNAL_NODE_IMPORT_CACHE_ROOT,
        suffix.split(path.sep).join('/'),
      )
    : GUEST_INTERNAL_NODE_IMPORT_CACHE_ROOT;
}

function guestVisiblePathFromHostPath(hostPath) {
  return guestPathFromHostPath(hostPath) ?? guestInternalPathFromHostPath(hostPath);
}

function translatePathStringToGuest(value) {
  if (typeof value !== 'string') {
    return value;
  }

  if (value.startsWith('file:')) {
    try {
      const hostPath = new URL(value).pathname;
      const guestPath = guestVisiblePathFromHostPath(hostPath);
      return guestPath ? pathToFileURL(guestPath).href : value;
    } catch {
      return value;
    }
  }

  if (!path.isAbsolute(value)) {
    return value;
  }

  return guestVisiblePathFromHostPath(value) ?? value;
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

  if (NODE_IMPORT_CACHE_ROOT) {
    const hostRoot = path.resolve(NODE_IMPORT_CACHE_ROOT);
    addReplacement(hostRoot, GUEST_INTERNAL_NODE_IMPORT_CACHE_ROOT);
    addReplacement(
      pathToFileURL(hostRoot).href,
      pathToFileURL(GUEST_INTERNAL_NODE_IMPORT_CACHE_ROOT).href,
    );
    const forwardSlashHostRoot = hostRoot.split(path.sep).join('/');
    if (forwardSlashHostRoot !== hostRoot) {
      addReplacement(forwardSlashHostRoot, GUEST_INTERNAL_NODE_IMPORT_CACHE_ROOT);
    }
  }

  return [...replacements.entries()].sort((left, right) => right[0].length - left[0].length);
}

function translateTextToGuest(value) {
  if (typeof value !== 'string' || value.length === 0) {
    return value;
  }

  let translated = value;
  for (const [hostValue, guestValue] of buildHostToGuestTextReplacements()) {
    translated = translated.split(hostValue).join(guestValue);
  }
  return translated;
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

function hostPathForSpecifier(specifier, fromGuestDir) {
  if (typeof specifier !== 'string') {
    return null;
  }

  if (specifier.startsWith('file:')) {
    try {
      return hostPathFromGuestPath(new URL(specifier).pathname);
    } catch {
      return null;
    }
  }

  if (specifier.startsWith('/')) {
    return hostPathFromGuestPath(specifier);
  }

  if (specifier.startsWith('./') || specifier.startsWith('../')) {
    return hostPathFromGuestPath(
      path.posix.normalize(path.posix.join(fromGuestDir, specifier)),
    );
  }

  return null;
}

function translateGuestPath(value, fromGuestDir = '/') {
  if (typeof value !== 'string') {
    return value;
  }

  const translated = hostPathForSpecifier(value, fromGuestDir);
  return translated ?? value;
}

function resolveGuestFsPath(value, fromGuestDir = '/') {
  if (typeof value !== 'string') {
    return value;
  }

  if (value.startsWith('file:')) {
    try {
      return path.posix.normalize(new URL(value).pathname);
    } catch {
      return value;
    }
  }

  if (value.startsWith('/')) {
    return path.posix.normalize(value);
  }

  if (value.startsWith('./') || value.startsWith('../')) {
    return path.posix.normalize(path.posix.join(fromGuestDir, value));
  }

  return value;
}

function normalizeFsReadOptions(options) {
  return typeof options === 'string' ? { encoding: options } : options;
}

function normalizeFsWriteContents(contents, options) {
  if (typeof contents !== 'string') {
    return contents;
  }

  const encoding =
    typeof options === 'string'
      ? options
      : options && typeof options === 'object'
        ? options.encoding
        : undefined;
  if (typeof encoding === 'string' && encoding !== 'utf8' && encoding !== 'utf-8') {
    return Buffer.from(contents, encoding);
  }

  return contents;
}

function normalizeFsTimeValue(value) {
  if (value instanceof Date) {
    return value.getTime();
  }

  return value;
}

function createGuestFsStats(stat) {
  if (stat == null || typeof stat !== 'object') {
    return stat;
  }

  const flags = {
    isDirectory: Boolean(stat.isDirectory),
    isSymbolicLink: Boolean(stat.isSymbolicLink),
  };
  const target = { ...stat };

  return new Proxy(target, {
    get(source, key, receiver) {
      switch (key) {
        case 'isBlockDevice':
        case 'isCharacterDevice':
        case 'isFIFO':
        case 'isSocket':
          return () => false;
        case 'isDirectory':
          return () => flags.isDirectory;
        case 'isFile':
          return () => !flags.isDirectory && !flags.isSymbolicLink;
        case 'isSymbolicLink':
          return () => flags.isSymbolicLink;
        case 'toJSON':
          return () => ({ ...source, ...flags });
        default:
          return Reflect.get(source, key, receiver);
      }
    },
  });
}

function requireAgentOsSyncRpcBridge() {
  const bridge = globalThis.__agentOsSyncRpc;
  if (
    bridge &&
    typeof bridge.call === 'function' &&
    typeof bridge.callSync === 'function'
  ) {
    return bridge;
  }

  const error = new Error('Agent OS sync RPC bridge is unavailable');
  error.code = 'ERR_AGENT_OS_NODE_SYNC_RPC_UNAVAILABLE';
  throw error;
}

function requireFsSyncRpcBridge() {
  return requireAgentOsSyncRpcBridge();
}

function createRpcBackedFsPromises(fromGuestDir = '/') {
  const call = (method, args = []) => requireFsSyncRpcBridge().call(method, args);

  return {
    access: async (target, mode) => {
      await call('fs.promises.access', [
        resolveGuestFsPath(target, fromGuestDir),
        mode,
      ]);
    },
    chmod: async (target, mode) =>
      call('fs.promises.chmod', [
        resolveGuestFsPath(target, fromGuestDir),
        mode,
      ]),
    chown: async (target, uid, gid) =>
      call('fs.promises.chown', [
        resolveGuestFsPath(target, fromGuestDir),
        uid,
        gid,
      ]),
    copyFile: async (source, destination, mode) =>
      call('fs.promises.copyFile', [
        resolveGuestFsPath(source, fromGuestDir),
        resolveGuestFsPath(destination, fromGuestDir),
        mode,
      ]),
    lstat: async (target) =>
      createGuestFsStats(
        await call('fs.promises.lstat', [resolveGuestFsPath(target, fromGuestDir)]),
      ),
    mkdir: async (target, options) =>
      call('fs.promises.mkdir', [
        resolveGuestFsPath(target, fromGuestDir),
        options,
      ]),
    readFile: async (target, options) =>
      call('fs.promises.readFile', [
        resolveGuestFsPath(target, fromGuestDir),
        normalizeFsReadOptions(options),
      ]),
    readdir: async (target, options) =>
      call('fs.promises.readdir', [
        resolveGuestFsPath(target, fromGuestDir),
        options,
      ]),
    rename: async (source, destination) =>
      call('fs.promises.rename', [
        resolveGuestFsPath(source, fromGuestDir),
        resolveGuestFsPath(destination, fromGuestDir),
      ]),
    rmdir: async (target, options) =>
      call('fs.promises.rmdir', [
        resolveGuestFsPath(target, fromGuestDir),
        options,
      ]),
    stat: async (target) =>
      createGuestFsStats(
        await call('fs.promises.stat', [resolveGuestFsPath(target, fromGuestDir)]),
      ),
    unlink: async (target) =>
      call('fs.promises.unlink', [resolveGuestFsPath(target, fromGuestDir)]),
    utimes: async (target, atime, mtime) =>
      call('fs.promises.utimes', [
        resolveGuestFsPath(target, fromGuestDir),
        normalizeFsTimeValue(atime),
        normalizeFsTimeValue(mtime),
      ]),
    writeFile: async (target, contents, options) =>
      call('fs.promises.writeFile', [
        resolveGuestFsPath(target, fromGuestDir),
        normalizeFsWriteContents(contents, options),
        normalizeFsReadOptions(options),
      ]),
  };
}

function resolveGuestSymlinkTarget(value, fromGuestDir = '/') {
  if (typeof value !== 'string') {
    return value;
  }

  if (value.startsWith('file:') || value.startsWith('/')) {
    return resolveGuestFsPath(value, fromGuestDir);
  }

  return value;
}

const INITIAL_GUEST_CWD = guestPathFromHostPath(HOST_CWD) ?? HOST_CWD;

function guestMappedChildNames(guestDir) {
  if (typeof guestDir !== 'string') {
    return [];
  }

  const normalized = path.posix.normalize(guestDir);
  const prefix = normalized === '/' ? '/' : `${normalized}/`;
  const children = new Set();

  for (const mapping of GUEST_PATH_MAPPINGS) {
    if (!mapping.guestPath.startsWith(prefix)) {
      continue;
    }
    const remainder = mapping.guestPath.slice(prefix.length);
    const childName = remainder.split('/')[0];
    if (childName) {
      children.add(childName);
    }
  }

  return [...children].sort();
}

function createSyntheticDirent(name) {
  return {
    name,
    isBlockDevice: () => false,
    isCharacterDevice: () => false,
    isDirectory: () => true,
    isFIFO: () => false,
    isFile: () => false,
    isSocket: () => false,
    isSymbolicLink: () => false,
  };
}

function createGuestDirent(name, stat) {
  return {
    name,
    isBlockDevice: stat.isBlockDevice,
    isCharacterDevice: stat.isCharacterDevice,
    isDirectory: stat.isDirectory,
    isFIFO: stat.isFIFO,
    isFile: stat.isFile,
    isSocket: stat.isSocket,
    isSymbolicLink: stat.isSymbolicLink,
  };
}

const GUEST_FS_O_RDONLY = 0;
const GUEST_FS_O_WRONLY = 1;
const GUEST_FS_O_RDWR = 2;
const GUEST_FS_O_CREAT = 0o100;
const GUEST_FS_O_EXCL = 0o200;
const GUEST_FS_O_TRUNC = 0o1000;
const GUEST_FS_O_APPEND = 0o2000;
const GUEST_FS_DEFAULT_STREAM_HWM = 64 * 1024;

function normalizeFsInteger(value, label) {
  const numeric =
    typeof value === 'number'
      ? value
      : typeof value === 'bigint'
        ? Number(value)
        : Number.NaN;
  if (!Number.isFinite(numeric) || !Number.isInteger(numeric) || numeric < 0) {
    throw new TypeError(`Agent OS ${label} must be a non-negative integer`);
  }
  return numeric;
}

function normalizeFsFd(value) {
  return normalizeFsInteger(value, 'fd');
}

function normalizeFsMode(mode) {
  if (mode == null) {
    return null;
  }
  if (typeof mode === 'string') {
    const parsed = Number.parseInt(mode, 8);
    if (!Number.isNaN(parsed)) {
      return parsed;
    }
  }
  return normalizeFsInteger(mode, 'mode');
}

function normalizeFsPosition(position) {
  if (position == null) {
    return null;
  }
  return normalizeFsInteger(position, 'position');
}

function normalizeFsOpenFlags(flags = 'r') {
  if (typeof flags === 'number') {
    return flags;
  }

  switch (flags) {
    case 'r':
    case 'rs':
    case 'sr':
      return GUEST_FS_O_RDONLY;
    case 'r+':
    case 'rs+':
    case 'sr+':
      return GUEST_FS_O_RDWR;
    case 'w':
      return GUEST_FS_O_WRONLY | GUEST_FS_O_CREAT | GUEST_FS_O_TRUNC;
    case 'wx':
    case 'xw':
      return GUEST_FS_O_WRONLY | GUEST_FS_O_CREAT | GUEST_FS_O_TRUNC | GUEST_FS_O_EXCL;
    case 'w+':
      return GUEST_FS_O_RDWR | GUEST_FS_O_CREAT | GUEST_FS_O_TRUNC;
    case 'wx+':
    case 'xw+':
      return GUEST_FS_O_RDWR | GUEST_FS_O_CREAT | GUEST_FS_O_TRUNC | GUEST_FS_O_EXCL;
    case 'a':
      return GUEST_FS_O_WRONLY | GUEST_FS_O_CREAT | GUEST_FS_O_APPEND;
    case 'ax':
    case 'xa':
      return GUEST_FS_O_WRONLY | GUEST_FS_O_CREAT | GUEST_FS_O_APPEND | GUEST_FS_O_EXCL;
    case 'a+':
      return GUEST_FS_O_RDWR | GUEST_FS_O_CREAT | GUEST_FS_O_APPEND;
    case 'ax+':
    case 'xa+':
      return GUEST_FS_O_RDWR | GUEST_FS_O_CREAT | GUEST_FS_O_APPEND | GUEST_FS_O_EXCL;
    default:
      throw new TypeError(`Agent OS does not support fs open flag ${String(flags)}`);
  }
}

function toGuestBufferView(value, label) {
  if (Buffer.isBuffer(value)) {
    return value;
  }
  if (ArrayBuffer.isView(value)) {
    return Buffer.from(value.buffer, value.byteOffset, value.byteLength);
  }
  throw new TypeError(`Agent OS ${label} must be a Buffer, TypedArray, or DataView`);
}

function decodeFsBytesPayload(value, label) {
  if (Buffer.isBuffer(value)) {
    return value;
  }
  if (ArrayBuffer.isView(value)) {
    return Buffer.from(value.buffer, value.byteOffset, value.byteLength);
  }
  if (typeof value === 'string') {
    return Buffer.from(value);
  }

  const base64Value =
    value &&
    typeof value === 'object' &&
    value.__agentOsType === 'bytes' &&
    typeof value.base64 === 'string'
      ? value.base64
      : null;
  if (base64Value == null) {
    throw new TypeError(`Agent OS ${label} must be an encoded bytes payload`);
  }
  return Buffer.from(base64Value, 'base64');
}

function normalizeFsReadTarget(buffer, offset, length) {
  const target = toGuestBufferView(buffer, 'read buffer');
  const normalizedOffset = offset == null ? 0 : normalizeFsInteger(offset, 'read offset');
  const available = target.byteLength - normalizedOffset;
  if (normalizedOffset > target.byteLength) {
    throw new RangeError('Agent OS read offset is out of range');
  }
  const normalizedLength =
    length == null ? available : normalizeFsInteger(length, 'read length');
  if (normalizedLength > available) {
    throw new RangeError('Agent OS read length is out of range');
  }
  return { target, offset: normalizedOffset, length: normalizedLength };
}

function normalizeFsWriteOperation(value, offsetOrPosition, lengthOrEncoding, position) {
  if (typeof value === 'string') {
    const normalizedPosition = normalizeFsPosition(offsetOrPosition);
    const encoding =
      typeof lengthOrEncoding === 'string' ? lengthOrEncoding : 'utf8';
    return {
      payload: normalizeFsWriteContents(value, { encoding }),
      position: normalizedPosition,
      result: value,
    };
  }

  const source = toGuestBufferView(value, 'write buffer');
  const normalizedOffset =
    offsetOrPosition == null ? 0 : normalizeFsInteger(offsetOrPosition, 'write offset');
  const available = source.byteLength - normalizedOffset;
  if (normalizedOffset > source.byteLength) {
    throw new RangeError('Agent OS write offset is out of range');
  }
  const normalizedLength =
    lengthOrEncoding == null
      ? available
      : normalizeFsInteger(lengthOrEncoding, 'write length');
  if (normalizedLength > available) {
    throw new RangeError('Agent OS write length is out of range');
  }

  return {
    payload: source.subarray(normalizedOffset, normalizedOffset + normalizedLength),
    position: normalizeFsPosition(position),
    result: value,
  };
}

function normalizeFsBytesResult(value, label) {
  const numeric =
    typeof value === 'number'
      ? value
      : typeof value === 'bigint'
        ? Number(value)
        : Number.NaN;
  if (!Number.isFinite(numeric) || numeric < 0) {
    throw new TypeError(`Agent OS ${label} must be numeric`);
  }
  return Math.trunc(numeric);
}

function requireFsCallback(callback, methodName) {
  if (typeof callback !== 'function') {
    throw new TypeError(`Agent OS ${methodName} requires a callback`);
  }
  return callback;
}

function invokeFsCallback(callback, error, ...results) {
  queueMicrotask(() => callback(error, ...results));
}

function createFsWatchUnavailableError(methodName) {
  const error = new Error(
    `Agent OS ${methodName} is unavailable because the kernel has no file-watching API`,
  );
  error.code = 'ERR_AGENT_OS_FS_WATCH_UNAVAILABLE';
  return error;
}

function createRpcBackedFsCallbacks(fromGuestDir = '/') {
  const call = (method, args = []) => requireFsSyncRpcBridge().call(method, args);

  return {
    close: (fd, callback) => {
      const done = requireFsCallback(callback, 'fs.close');
      call('fs.close', [normalizeFsFd(fd)]).then(
        () => invokeFsCallback(done, null),
        (error) => invokeFsCallback(done, error),
      );
    },
    fstat: (fd, options, callback) => {
      const done = requireFsCallback(
        typeof options === 'function' ? options : callback,
        'fs.fstat',
      );
      call('fs.fstat', [normalizeFsFd(fd)]).then(
        (stat) => invokeFsCallback(done, null, createGuestFsStats(stat)),
        (error) => invokeFsCallback(done, error),
      );
    },
    open: (target, flags, mode, callback) => {
      if (typeof flags === 'function') {
        callback = flags;
        flags = undefined;
        mode = undefined;
      } else if (typeof mode === 'function') {
        callback = mode;
        mode = undefined;
      }

      const done = requireFsCallback(callback, 'fs.open');
      call('fs.open', [
        resolveGuestFsPath(target, fromGuestDir),
        normalizeFsOpenFlags(flags ?? 'r'),
        normalizeFsMode(mode),
      ]).then(
        (fd) => invokeFsCallback(done, null, normalizeFsFd(fd)),
        (error) => invokeFsCallback(done, error),
      );
    },
    read: (fd, buffer, offset, length, position, callback) => {
      if (typeof offset === 'function') {
        callback = offset;
        offset = undefined;
        length = undefined;
        position = undefined;
      } else if (typeof length === 'function') {
        callback = length;
        length = undefined;
        position = undefined;
      } else if (typeof position === 'function') {
        callback = position;
        position = undefined;
      }

      const done = requireFsCallback(callback, 'fs.read');
      const target = normalizeFsReadTarget(buffer, offset, length);
      call('fs.read', [
        normalizeFsFd(fd),
        target.length,
        normalizeFsPosition(position),
      ]).then(
        (payload) => {
          const chunk = decodeFsBytesPayload(payload, 'fs.read result');
          const bytesRead = Math.min(target.length, chunk.byteLength);
          chunk.copy(target.target, target.offset, 0, bytesRead);
          invokeFsCallback(done, null, bytesRead, buffer);
        },
        (error) => invokeFsCallback(done, error),
      );
    },
    write: (fd, value, offsetOrPosition, lengthOrEncoding, position, callback) => {
      if (typeof offsetOrPosition === 'function') {
        callback = offsetOrPosition;
        offsetOrPosition = undefined;
        lengthOrEncoding = undefined;
        position = undefined;
      } else if (typeof lengthOrEncoding === 'function') {
        callback = lengthOrEncoding;
        lengthOrEncoding = undefined;
        position = undefined;
      } else if (typeof position === 'function') {
        callback = position;
        position = undefined;
      }

      const done = requireFsCallback(callback, 'fs.write');
      const write = normalizeFsWriteOperation(
        value,
        offsetOrPosition,
        lengthOrEncoding,
        position,
      );
      call('fs.write', [normalizeFsFd(fd), write.payload, write.position]).then(
        (bytesWritten) =>
          invokeFsCallback(
            done,
            null,
            normalizeFsBytesResult(bytesWritten, 'fs.write result'),
            write.result,
          ),
        (error) => invokeFsCallback(done, error),
      );
    },
  };
}

function createRpcBackedFsSync(fromGuestDir = '/') {
  const callSync = (method, args = []) => requireFsSyncRpcBridge().callSync(method, args);

  return {
    accessSync: (target, mode) =>
      callSync('fs.accessSync', [resolveGuestFsPath(target, fromGuestDir), mode]),
    chmodSync: (target, mode) =>
      callSync('fs.chmodSync', [resolveGuestFsPath(target, fromGuestDir), mode]),
    chownSync: (target, uid, gid) =>
      callSync('fs.chownSync', [resolveGuestFsPath(target, fromGuestDir), uid, gid]),
    closeSync: (fd) => callSync('fs.closeSync', [normalizeFsFd(fd)]),
    copyFileSync: (source, destination, mode) =>
      callSync('fs.copyFileSync', [
        resolveGuestFsPath(source, fromGuestDir),
        resolveGuestFsPath(destination, fromGuestDir),
        mode,
      ]),
    existsSync: (target) => {
      try {
        return Boolean(callSync('fs.existsSync', [resolveGuestFsPath(target, fromGuestDir)]));
      } catch {
        return false;
      }
    },
    fstatSync: (fd) =>
      createGuestFsStats(callSync('fs.fstatSync', [normalizeFsFd(fd)])),
    linkSync: (existingPath, newPath) =>
      callSync('fs.linkSync', [
        resolveGuestFsPath(existingPath, fromGuestDir),
        resolveGuestFsPath(newPath, fromGuestDir),
      ]),
    lstatSync: (target) =>
      createGuestFsStats(callSync('fs.lstatSync', [resolveGuestFsPath(target, fromGuestDir)])),
    mkdirSync: (target, options) =>
      callSync('fs.mkdirSync', [resolveGuestFsPath(target, fromGuestDir), options]),
    openSync: (target, flags, mode) =>
      normalizeFsFd(
        callSync('fs.openSync', [
          resolveGuestFsPath(target, fromGuestDir),
          normalizeFsOpenFlags(flags ?? 'r'),
          normalizeFsMode(mode),
        ]),
      ),
    readFileSync: (target, options) =>
      callSync('fs.readFileSync', [
        resolveGuestFsPath(target, fromGuestDir),
        normalizeFsReadOptions(options),
      ]),
    readSync: (fd, buffer, offset, length, position) => {
      const target = normalizeFsReadTarget(buffer, offset, length);
      const chunk = decodeFsBytesPayload(
        callSync('fs.readSync', [
          normalizeFsFd(fd),
          target.length,
          normalizeFsPosition(position),
        ]),
        'fs.readSync result',
      );
      const bytesRead = Math.min(target.length, chunk.byteLength);
      chunk.copy(target.target, target.offset, 0, bytesRead);
      return bytesRead;
    },
    readdirSync: (target, options) => {
      const guestPath = resolveGuestFsPath(target, fromGuestDir);
      const entries = callSync('fs.readdirSync', [guestPath, options]);
      if (!options || typeof options !== 'object' || !options.withFileTypes) {
        return entries;
      }

      return entries.map((name) =>
        createGuestDirent(
          name,
          createGuestFsStats(callSync('fs.lstatSync', [path.posix.join(guestPath, name)])),
        ),
      );
    },
    readlinkSync: (target) =>
      callSync('fs.readlinkSync', [resolveGuestFsPath(target, fromGuestDir)]),
    renameSync: (source, destination) =>
      callSync('fs.renameSync', [
        resolveGuestFsPath(source, fromGuestDir),
        resolveGuestFsPath(destination, fromGuestDir),
      ]),
    rmdirSync: (target, options) =>
      callSync('fs.rmdirSync', [resolveGuestFsPath(target, fromGuestDir), options]),
    statSync: (target) =>
      createGuestFsStats(callSync('fs.statSync', [resolveGuestFsPath(target, fromGuestDir)])),
    symlinkSync: (target, linkPath, type) =>
      callSync('fs.symlinkSync', [
        resolveGuestSymlinkTarget(target, fromGuestDir),
        resolveGuestFsPath(linkPath, fromGuestDir),
        type,
      ]),
    unlinkSync: (target) =>
      callSync('fs.unlinkSync', [resolveGuestFsPath(target, fromGuestDir)]),
    utimesSync: (target, atime, mtime) =>
      callSync('fs.utimesSync', [
        resolveGuestFsPath(target, fromGuestDir),
        normalizeFsTimeValue(atime),
        normalizeFsTimeValue(mtime),
      ]),
    writeSync: (fd, value, offsetOrPosition, lengthOrEncoding, position) => {
      const write = normalizeFsWriteOperation(
        value,
        offsetOrPosition,
        lengthOrEncoding,
        position,
      );
      return normalizeFsBytesResult(
        callSync('fs.writeSync', [normalizeFsFd(fd), write.payload, write.position]),
        'fs.writeSync result',
      );
    },
    writeFileSync: (target, contents, options) =>
      callSync('fs.writeFileSync', [
        resolveGuestFsPath(target, fromGuestDir),
        normalizeFsWriteContents(contents, options),
        normalizeFsReadOptions(options),
      ]),
  };
}

function createGuestReadStreamClass(fromGuestDir = '/') {
  const call = (method, args = []) => requireFsSyncRpcBridge().call(method, args);

  return class AgentOsReadStream extends Readable {
    constructor(target, options = {}) {
      super({
        autoDestroy: options.autoClose !== false,
        emitClose: options.emitClose !== false,
        highWaterMark: options.highWaterMark,
      });

      this.path = target;
      this.fd = typeof options.fd === 'number' ? options.fd : null;
      this.flags = options.flags ?? 'r';
      this.mode = options.mode;
      this.autoClose = options.autoClose !== false;
      this.start = options.start;
      this.end = options.end;
      this.bytesRead = 0;
      this.pending = false;
      this.position =
        options.start == null ? null : normalizeFsInteger(options.start, 'stream start');
      this.guestDir = fromGuestDir;

      if (options.end != null) {
        this.end = normalizeFsInteger(options.end, 'stream end');
        if (this.position != null && this.end < this.position) {
          throw new RangeError('Agent OS read stream end must be >= start');
        }
      }

      if (options.encoding) {
        this.setEncoding(options.encoding);
      }
    }

    _construct(callback) {
      if (typeof this.fd === 'number') {
        this.emit('open', this.fd);
        this.emit('ready');
        callback();
        return;
      }

      call('fs.open', [
        resolveGuestFsPath(this.path, this.guestDir),
        normalizeFsOpenFlags(this.flags),
        normalizeFsMode(this.mode),
      ]).then(
        (fd) => {
          this.fd = normalizeFsFd(fd);
          this.emit('open', this.fd);
          this.emit('ready');
          callback();
        },
        (error) => callback(error),
      );
    }

    _read(size) {
      if (this.pending || typeof this.fd !== 'number') {
        return;
      }

      let length = size > 0 ? size : this.readableHighWaterMark ?? GUEST_FS_DEFAULT_STREAM_HWM;
      if (this.position != null && this.end != null) {
        const remaining = this.end - this.position + 1;
        if (remaining <= 0) {
          this.push(null);
          return;
        }
        length = Math.min(length, remaining);
      }

      this.pending = true;
      call('fs.read', [this.fd, length, this.position]).then(
        (payload) => {
          this.pending = false;
          const chunk = decodeFsBytesPayload(payload, 'fs.createReadStream chunk');
          if (this.position != null) {
            this.position += chunk.byteLength;
          }
          this.bytesRead += chunk.byteLength;
          if (chunk.byteLength === 0) {
            this.push(null);
            return;
          }
          this.push(chunk);
        },
        (error) => {
          this.pending = false;
          this.destroy(error);
        },
      );
    }

    _destroy(error, callback) {
      if (!this.autoClose || typeof this.fd !== 'number') {
        callback(error);
        return;
      }

      const fd = this.fd;
      this.fd = null;
      call('fs.close', [fd]).then(
        () => callback(error),
        (closeError) => callback(error ?? closeError),
      );
    }
  };
}

function createGuestWriteStreamClass(fromGuestDir = '/') {
  const call = (method, args = []) => requireFsSyncRpcBridge().call(method, args);

  return class AgentOsWriteStream extends Writable {
    constructor(target, options = {}) {
      super({
        autoDestroy: options.autoClose !== false,
        defaultEncoding: options.defaultEncoding,
        decodeStrings: options.decodeStrings !== false,
        emitClose: options.emitClose !== false,
        highWaterMark: options.highWaterMark,
      });

      this.path = target;
      this.fd = typeof options.fd === 'number' ? options.fd : null;
      this.flags = options.flags ?? 'w';
      this.mode = options.mode;
      this.autoClose = options.autoClose !== false;
      this.bytesWritten = 0;
      this.position =
        options.start == null ? null : normalizeFsInteger(options.start, 'stream start');
      this.guestDir = fromGuestDir;
    }

    _construct(callback) {
      if (typeof this.fd === 'number') {
        this.emit('open', this.fd);
        this.emit('ready');
        callback();
        return;
      }

      call('fs.open', [
        resolveGuestFsPath(this.path, this.guestDir),
        normalizeFsOpenFlags(this.flags),
        normalizeFsMode(this.mode),
      ]).then(
        (fd) => {
          this.fd = normalizeFsFd(fd);
          this.emit('open', this.fd);
          this.emit('ready');
          callback();
        },
        (error) => callback(error),
      );
    }

    _write(chunk, encoding, callback) {
      const write = normalizeFsWriteOperation(chunk, 0, chunk.length, this.position);
      call('fs.write', [normalizeFsFd(this.fd), write.payload, write.position]).then(
        (bytesWritten) => {
          const normalized = normalizeFsBytesResult(
            bytesWritten,
            'fs.createWriteStream result',
          );
          this.bytesWritten += normalized;
          if (this.position != null) {
            this.position += normalized;
          }
          callback();
        },
        (error) => callback(error),
      );
    }

    _destroy(error, callback) {
      if (!this.autoClose || typeof this.fd !== 'number') {
        callback(error);
        return;
      }

      const fd = this.fd;
      this.fd = null;
      call('fs.close', [fd]).then(
        () => callback(error),
        (closeError) => callback(error ?? closeError),
      );
    }
  };
}

function wrapFsModule(fsModule, fromGuestDir = '/') {
  const wrapPathFirst = (methodName) => {
    const fn = fsModule[methodName];
    return (...args) =>
      fn(translateGuestPath(args[0], fromGuestDir), ...args.slice(1));
  };
  const wrapRenameLike = (methodName) => {
    const fn = fsModule[methodName];
    return (...args) =>
      fn(
        translateGuestPath(args[0], fromGuestDir),
        translateGuestPath(args[1], fromGuestDir),
        ...args.slice(2),
      );
  };
  const existsSync = fsModule.existsSync.bind(fsModule);
  const readdirSync = fsModule.readdirSync.bind(fsModule);
  const ReadStream = createGuestReadStreamClass(fromGuestDir);
  const WriteStream = createGuestWriteStreamClass(fromGuestDir);

  const wrapped = {
    ...fsModule,
    ReadStream,
    WriteStream,
    accessSync: wrapPathFirst('accessSync'),
    appendFileSync: wrapPathFirst('appendFileSync'),
    chmodSync: wrapPathFirst('chmodSync'),
    chownSync: wrapPathFirst('chownSync'),
    createReadStream: (target, options) => new ReadStream(target, options),
    createWriteStream: (target, options) => new WriteStream(target, options),
    existsSync: (target) => {
      const translated = translateGuestPath(target, fromGuestDir);
      return existsSync(translated) || guestMappedChildNames(target).length > 0;
    },
    lstatSync: wrapPathFirst('lstatSync'),
    mkdirSync: wrapPathFirst('mkdirSync'),
    readFileSync: wrapPathFirst('readFileSync'),
    readdirSync: (target, options) => {
      const translated = translateGuestPath(target, fromGuestDir);
      if (existsSync(translated)) {
        return readdirSync(translated, options);
      }

      const synthetic = guestMappedChildNames(target);
      if (synthetic.length > 0) {
        return options && typeof options === 'object' && options.withFileTypes
          ? synthetic.map((name) => createSyntheticDirent(name))
          : synthetic;
      }

      return readdirSync(translated, options);
    },
    readlinkSync: wrapPathFirst('readlinkSync'),
    realpathSync: wrapPathFirst('realpathSync'),
    renameSync: wrapRenameLike('renameSync'),
    rmSync: wrapPathFirst('rmSync'),
    rmdirSync: wrapPathFirst('rmdirSync'),
    statSync: wrapPathFirst('statSync'),
    symlinkSync: wrapRenameLike('symlinkSync'),
    unlinkSync: wrapPathFirst('unlinkSync'),
    unwatchFile: () => {},
    utimesSync: wrapPathFirst('utimesSync'),
    watch: () => {
      throw createFsWatchUnavailableError('fs.watch');
    },
    watchFile: () => {
      throw createFsWatchUnavailableError('fs.watchFile');
    },
    writeFileSync: wrapPathFirst('writeFileSync'),
  };

  if (fsModule.promises) {
    wrapped.promises = {
      ...fsModule.promises,
      access: wrapPathFirstAsync(fsModule.promises.access, fromGuestDir),
      appendFile: wrapPathFirstAsync(fsModule.promises.appendFile, fromGuestDir),
      chmod: wrapPathFirstAsync(fsModule.promises.chmod, fromGuestDir),
      chown: wrapPathFirstAsync(fsModule.promises.chown, fromGuestDir),
      lstat: wrapPathFirstAsync(fsModule.promises.lstat, fromGuestDir),
      mkdir: wrapPathFirstAsync(fsModule.promises.mkdir, fromGuestDir),
      open: wrapPathFirstAsync(fsModule.promises.open, fromGuestDir),
      readFile: wrapPathFirstAsync(fsModule.promises.readFile, fromGuestDir),
      readdir: wrapPathFirstAsync(fsModule.promises.readdir, fromGuestDir),
      readlink: wrapPathFirstAsync(fsModule.promises.readlink, fromGuestDir),
      realpath: wrapPathFirstAsync(fsModule.promises.realpath, fromGuestDir),
      rename: wrapRenameLikeAsync(fsModule.promises.rename, fromGuestDir),
      rm: wrapPathFirstAsync(fsModule.promises.rm, fromGuestDir),
      rmdir: wrapPathFirstAsync(fsModule.promises.rmdir, fromGuestDir),
      stat: wrapPathFirstAsync(fsModule.promises.stat, fromGuestDir),
      symlink: wrapRenameLikeAsync(fsModule.promises.symlink, fromGuestDir),
      unlink: wrapPathFirstAsync(fsModule.promises.unlink, fromGuestDir),
      utimes: wrapPathFirstAsync(fsModule.promises.utimes, fromGuestDir),
      writeFile: wrapPathFirstAsync(fsModule.promises.writeFile, fromGuestDir),
    };
    Object.assign(wrapped.promises, createRpcBackedFsPromises(fromGuestDir));
  }

  Object.assign(wrapped, createRpcBackedFsCallbacks(fromGuestDir));
  Object.assign(wrapped, createRpcBackedFsSync(fromGuestDir));

  return wrapped;
}

function wrapPathFirstAsync(fn, fromGuestDir) {
  return (...args) =>
    fn(translateGuestPath(args[0], fromGuestDir), ...args.slice(1));
}

function wrapRenameLikeAsync(fn, fromGuestDir) {
  return (...args) =>
    fn(
      translateGuestPath(args[0], fromGuestDir),
      translateGuestPath(args[1], fromGuestDir),
      ...args.slice(2),
    );
}

function createRpcBackedChildProcessModule(fromGuestDir = '/') {
  const RPC_POLL_WAIT_MS = 50;
  const RPC_IDLE_POLL_DELAY_MS = 10;
  const INTERNAL_ENV_KEYS = [
    'AGENT_OS_ALLOWED_NODE_BUILTINS',
    'AGENT_OS_GUEST_PATH_MAPPINGS',
    'AGENT_OS_LOOPBACK_EXEMPT_PORTS',
    'AGENT_OS_VIRTUAL_PROCESS_EXEC_PATH',
    'AGENT_OS_VIRTUAL_PROCESS_UID',
    'AGENT_OS_VIRTUAL_PROCESS_GID',
  ];

  const bridge = () => requireAgentOsSyncRpcBridge();
  const createUnsupportedChildProcessError = (subject) => {
    const error = new Error(`${subject} is not supported by the Agent OS child_process polyfill`);
    error.code = 'ERR_AGENT_OS_CHILD_PROCESS_UNSUPPORTED';
    return error;
  };
  const normalizeSpawnInvocation = (args, options) => {
    if (!Array.isArray(args)) {
      return {
        args: [],
        options: args && typeof args === 'object' ? args : options,
      };
    }

    return {
      args,
      options,
    };
  };
  const normalizeExecInvocation = (options, callback) =>
    typeof options === 'function'
      ? { options: undefined, callback: options }
      : { options, callback };
  const normalizeExecFileInvocation = (args, options, callback) => {
    if (typeof args === 'function') {
      return { args: [], options: undefined, callback: args };
    }
    if (!Array.isArray(args)) {
      return {
        args: [],
        options: args,
        callback: typeof options === 'function' ? options : callback,
      };
    }
    if (typeof options === 'function') {
      return { args, options: undefined, callback: options };
    }
    return { args, options, callback };
  };
  const normalizeChildProcessSignal = (value) =>
    typeof value === 'string' && value.length > 0 ? value : 'SIGTERM';
  const normalizeChildProcessEncoding = (options) =>
    typeof options?.encoding === 'string' ? options.encoding : null;
  const normalizeChildProcessTimeout = (options) =>
    Number.isInteger(options?.timeout) && options.timeout > 0 ? options.timeout : null;
  const normalizeChildProcessEnv = (env) => {
    const source = env && typeof env === 'object' ? env : {};
    const merged = {
      ...Object.fromEntries(
        Object.entries(process.env).filter(([, value]) => typeof value === 'string'),
      ),
      ...Object.fromEntries(
        Object.entries(source).filter(([, value]) => value != null),
      ),
    };
    delete merged.NODE_OPTIONS;

    for (const key of INTERNAL_ENV_KEYS) {
      if (typeof HOST_PROCESS_ENV[key] === 'string') {
        merged[key] = HOST_PROCESS_ENV[key];
      }
    }
    for (const [key, value] of Object.entries(HOST_PROCESS_ENV)) {
      if (key.startsWith('AGENT_OS_VIRTUAL_OS_') && typeof value === 'string') {
        merged[key] = value;
      }
    }

    return Object.fromEntries(
      Object.entries(merged).map(([key, value]) => [key, String(value)]),
    );
  };
  const normalizeChildProcessStdioEntry = (value, index) => {
    if (value == null) {
      return 'pipe';
    }
    if (value === 'pipe' || value === 'ignore' || value === 'inherit') {
      return value;
    }
    if (value === 'ipc') {
      throw createUnsupportedChildProcessError('child_process IPC stdio');
    }
    if (value === null && index === 0) {
      return 'pipe';
    }
    throw createUnsupportedChildProcessError(`child_process stdio=${String(value)}`);
  };
  const normalizeChildProcessStdio = (stdio) => {
    if (stdio == null) {
      return ['pipe', 'pipe', 'pipe'];
    }
    if (typeof stdio === 'string') {
      return [
        normalizeChildProcessStdioEntry(stdio, 0),
        normalizeChildProcessStdioEntry(stdio, 1),
        normalizeChildProcessStdioEntry(stdio, 2),
      ];
    }
    if (!Array.isArray(stdio)) {
      throw createUnsupportedChildProcessError('child_process stdio configuration');
    }
    return [0, 1, 2].map((index) =>
      normalizeChildProcessStdioEntry(stdio[index], index),
    );
  };
  const normalizeChildProcessOptions = (options, shell = false) => {
    if (options != null && typeof options !== 'object') {
      throw new TypeError('child_process options must be an object');
    }
    if (options?.detached) {
      throw createUnsupportedChildProcessError('child_process detached');
    }

    return {
      cwd:
        typeof options?.cwd === 'string'
          ? resolveGuestFsPath(options.cwd, fromGuestDir)
          : fromGuestDir,
      env: normalizeChildProcessEnv(options?.env),
      shell: shell || options?.shell === true,
      stdio: normalizeChildProcessStdio(options?.stdio),
      timeout: normalizeChildProcessTimeout(options),
      killSignal: normalizeChildProcessSignal(options?.killSignal),
    };
  };
  const createRpcSpawnRequest = (command, args, options, shell = false) => ({
    command: String(command),
    args: Array.isArray(args) ? args.map((arg) => String(arg)) : [],
    options: normalizeChildProcessOptions(options, shell),
  });
  const callSpawn = (command, args, options, shell = false) =>
    bridge().callSync('child_process.spawn', [
      createRpcSpawnRequest(command, args, options, shell),
    ]);
  const callPoll = (childId, waitMs = 0) =>
    bridge().callSync('child_process.poll', [childId, waitMs]);
  const callKill = (childId, signal) =>
    bridge().callSync('child_process.kill', [childId, normalizeChildProcessSignal(signal)]);
  const callWriteStdin = (childId, chunk) =>
    bridge().call('child_process.write_stdin', [childId, toGuestBufferView(chunk, 'stdin chunk')]);
  const callCloseStdin = (childId) =>
    bridge().call('child_process.close_stdin', [childId]);
  const encodeChildProcessOutput = (buffer, encoding) =>
    encoding ? buffer.toString(encoding) : buffer;
  const createChildProcessExecError = (subject, exitCode, signal, stdout, stderr) => {
    const error = new Error(
      signal == null
        ? `${subject} exited with code ${exitCode ?? 'unknown'}`
        : `${subject} terminated by signal ${signal}`,
    );
    error.code = signal == null ? 'ERR_AGENT_OS_CHILD_PROCESS_EXIT' : signal;
    error.killed = signal != null;
    error.signal = signal;
    error.stdout = stdout;
    error.stderr = stderr;
    if (typeof exitCode === 'number') {
      error.status = exitCode;
    }
    return error;
  };
  const createSpawnSyncResult = (pid, stdout, stderr, exitCode, signal, error, encoding) => {
    const encodedStdout = encodeChildProcessOutput(stdout, encoding);
    const encodedStderr = encodeChildProcessOutput(stderr, encoding);
    return {
      pid,
      output: [null, encodedStdout, encodedStderr],
      stdout: encodedStdout,
      stderr: encodedStderr,
      status: typeof exitCode === 'number' ? exitCode : null,
      signal: signal ?? null,
      error,
    };
  };
  const runChildProcessSync = (command, args, options, shell = false) => {
    const normalizedOptions = normalizeChildProcessOptions(options, shell);
    const encoding = normalizeChildProcessEncoding(options);
    const stdout = [];
    const stderr = [];
    let child;
    try {
      child = callSpawn(command, args, options, shell);
    } catch (error) {
      return createSpawnSyncResult(
        0,
        Buffer.alloc(0),
        Buffer.from(error instanceof Error ? error.message : String(error)),
        null,
        null,
        error,
        encoding,
      );
    }

    const startedAt = Date.now();
    let exitCode = null;
    let signal = null;
    while (exitCode == null && signal == null) {
      if (
        normalizedOptions.timeout != null &&
        Date.now() - startedAt > normalizedOptions.timeout
      ) {
        callKill(child.childId, normalizedOptions.killSignal);
      }

      const event = callPoll(child.childId, RPC_POLL_WAIT_MS);
      if (!event) {
        continue;
      }

      if (event.type === 'stdout') {
        stdout.push(decodeFsBytesPayload(event.data, 'child_process.spawnSync stdout'));
      } else if (event.type === 'stderr') {
        stderr.push(decodeFsBytesPayload(event.data, 'child_process.spawnSync stderr'));
      } else if (event.type === 'exit') {
        exitCode =
          typeof event.exitCode === 'number' ? Math.trunc(event.exitCode) : null;
        signal = typeof event.signal === 'string' ? event.signal : null;
      }
    }

    const stdoutBuffer = Buffer.concat(stdout);
    const stderrBuffer = Buffer.concat(stderr);
    return createSpawnSyncResult(
      Number(child.pid) || 0,
      stdoutBuffer,
      stderrBuffer,
      exitCode,
      signal,
      null,
      encoding,
    );
  };

  class AgentOsChildReadable extends Readable {
    _read() {}
  }

  class AgentOsChildWritable extends Writable {
    constructor(childId) {
      super();
      this.childId = childId;
    }

    _write(chunk, encoding, callback) {
      callWriteStdin(this.childId, chunk).then(
        () => callback(),
        (error) => callback(error),
      );
    }

    _final(callback) {
      callCloseStdin(this.childId).then(
        () => callback(),
        (error) => callback(error),
      );
    }
  }

  const finalizeChildStream = (stream) => {
    if (!stream || stream.destroyed) {
      return;
    }
    stream.push(null);
  };
  const emitChildLifecycleEvents = (child) => {
    queueMicrotask(() => {
      child.emit('exit', child.exitCode, child.signalCode);
      child.emit('close', child.exitCode, child.signalCode);
    });
  };
  const deliverChildOutput = (child, channel, payload) => {
    const chunk = decodeFsBytesPayload(payload, `child_process.${channel}`);
    const mode = channel === 'stdout' ? child._stdio[1] : child._stdio[2];
    if (mode === 'ignore') {
      return;
    }
    if (mode === 'inherit') {
      (channel === 'stdout' ? process.stdout : process.stderr).write(chunk);
      return;
    }

    const stream = channel === 'stdout' ? child.stdout : child.stderr;
    stream?.push(chunk);
  };
  const closeSyntheticChild = (child, exitCode, signalCode) => {
    if (child._closed) {
      return;
    }
    child._closed = true;
    child.exitCode = exitCode;
    child.signalCode = signalCode;
    finalizeChildStream(child.stdout);
    finalizeChildStream(child.stderr);
    if (child.stdin && !child.stdin.destroyed) {
      child.stdin.destroy();
    }
    emitChildLifecycleEvents(child);
  };
  const scheduleSyntheticChildPoll = (child, delayMs) => {
    if (child._closed || child._pollTimer != null) {
      return;
    }
    child._pollTimer = setTimeout(() => {
      child._pollTimer = null;
      if (child._closed) {
        return;
      }

      let event;
      try {
        event = callPoll(child._childId, RPC_POLL_WAIT_MS);
      } catch (error) {
        child._closed = true;
        finalizeChildStream(child.stdout);
        finalizeChildStream(child.stderr);
        queueMicrotask(() => child.emit('error', error));
        return;
      }

      if (!event) {
        scheduleSyntheticChildPoll(child, RPC_IDLE_POLL_DELAY_MS);
        return;
      }

      if (event.type === 'stdout' || event.type === 'stderr') {
        deliverChildOutput(child, event.type, event.data);
        scheduleSyntheticChildPoll(child, 0);
        return;
      }

      if (event.type === 'exit') {
        closeSyntheticChild(
          child,
          typeof event.exitCode === 'number' ? Math.trunc(event.exitCode) : null,
          typeof event.signal === 'string' ? event.signal : null,
        );
        return;
      }

      scheduleSyntheticChildPoll(child, 0);
    }, delayMs);
    if (!child._refed) {
      child._pollTimer.unref?.();
    }
  };
  const createSyntheticChildProcess = (spawnResult, options) => {
    const child = Object.create(EventEmitter.prototype);
    EventEmitter.call(child);
    child._childId = spawnResult.childId;
    child._closed = false;
    child._pollTimer = null;
    child._refed = true;
    child._stdio = options.stdio;
    child.pid = Math.trunc(Number(spawnResult.pid) || 0);
    child.exitCode = null;
    child.signalCode = null;
    child.spawnfile = String(spawnResult.command ?? '');
    child.spawnargs = [
      child.spawnfile,
      ...(Array.isArray(spawnResult.args) ? spawnResult.args.map(String) : []),
    ];
    child.stdin = options.stdio[0] === 'pipe' ? new AgentOsChildWritable(child._childId) : null;
    child.stdout = options.stdio[1] === 'pipe' ? new AgentOsChildReadable() : null;
    child.stderr = options.stdio[2] === 'pipe' ? new AgentOsChildReadable() : null;
    child.killed = false;
    child.connected = false;
    child.kill = (signal = 'SIGTERM') => {
      try {
        callKill(child._childId, signal);
        child.killed = true;
        return true;
      } catch (error) {
        if (error && typeof error === 'object' && error.code === 'ESRCH') {
          return false;
        }
        throw error;
      }
    };
    child.ref = () => {
      child._refed = true;
      child._pollTimer?.ref?.();
      return child;
    };
    child.unref = () => {
      child._refed = false;
      child._pollTimer?.unref?.();
      return child;
    };
    child.disconnect = () => {
      throw createUnsupportedChildProcessError('child_process.disconnect');
    };
    child.send = () => {
      throw createUnsupportedChildProcessError('child_process.send');
    };
    queueMicrotask(() => child.emit('spawn'));
    scheduleSyntheticChildPoll(child, 0);
    return child;
  };
  const collectSyntheticChildOutput = (child, options, callback) => {
    const encoding = normalizeChildProcessEncoding(options) ?? 'utf8';
    const stdoutChunks = [];
    const stderrChunks = [];
    const timeout = normalizeChildProcessTimeout(options);
    const killSignal = normalizeChildProcessSignal(options?.killSignal);
    let timer = null;

    if (child.stdout) {
      child.stdout.on('data', (chunk) => {
        stdoutChunks.push(Buffer.from(chunk));
      });
    }
    if (child.stderr) {
      child.stderr.on('data', (chunk) => {
        stderrChunks.push(Buffer.from(chunk));
      });
    }

    const promise = new Promise((resolve, reject) => {
      if (timeout != null) {
        timer = setTimeout(() => {
          try {
            child.kill(killSignal);
          } catch {}
        }, timeout);
        timer.unref?.();
      }

      child.once('error', reject);
      child.once('close', (exitCode, signalCode) => {
        if (timer) {
          clearTimeout(timer);
        }
        const stdout = encodeChildProcessOutput(Buffer.concat(stdoutChunks), encoding);
        const stderr = encodeChildProcessOutput(Buffer.concat(stderrChunks), encoding);
        if (exitCode === 0 && signalCode == null) {
          resolve({ stdout, stderr, exitCode, signalCode });
          return;
        }
        reject(createChildProcessExecError('child_process', exitCode, signalCode, stdout, stderr));
      });
    });

    if (typeof callback === 'function') {
      promise.then(
        ({ stdout, stderr }) => callback(null, stdout, stderr),
        (error) => callback(error, error.stdout, error.stderr),
      );
    }

    return promise;
  };

  const module = {
    ChildProcess: EventEmitter,
    spawn(command, args, options) {
      const invocation = normalizeSpawnInvocation(args, options);
      const normalizedOptions = normalizeChildProcessOptions(invocation.options);
      const child = createSyntheticChildProcess(
        callSpawn(command, invocation.args, invocation.options),
        normalizedOptions,
      );
      return child;
    },
    spawnSync(command, args, options) {
      const invocation = normalizeSpawnInvocation(args, options);
      return runChildProcessSync(command, invocation.args, invocation.options);
    },
    exec(command, options, callback) {
      const invocation = normalizeExecInvocation(options, callback);
      const child = module.spawn(command, [], {
        ...invocation.options,
        stdio: ['pipe', 'pipe', 'pipe'],
        shell: true,
      });
      collectSyntheticChildOutput(child, invocation.options, invocation.callback);
      return child;
    },
    execSync(command, options) {
      const result = runChildProcessSync(command, [], {
        ...options,
        stdio: ['pipe', 'pipe', 'pipe'],
      }, true);
      if (result.error) {
        throw result.error;
      }
      if (result.status !== 0 || result.signal != null) {
        throw createChildProcessExecError(
          'child_process.execSync',
          result.status,
          result.signal,
          result.stdout,
          result.stderr,
        );
      }
      return result.stdout;
    },
    execFile(file, args, options, callback) {
      const invocation = normalizeExecFileInvocation(args, options, callback);
      const child = module.spawn(file, invocation.args, {
        ...invocation.options,
        stdio: ['pipe', 'pipe', 'pipe'],
      });
      collectSyntheticChildOutput(child, invocation.options, invocation.callback);
      return child;
    },
    execFileSync(file, args, options) {
      const invocation = normalizeExecFileInvocation(args, options);
      const result = runChildProcessSync(file, invocation.args, {
        ...invocation.options,
        stdio: ['pipe', 'pipe', 'pipe'],
      });
      if (result.error) {
        throw result.error;
      }
      if (result.status !== 0 || result.signal != null) {
        throw createChildProcessExecError(
          'child_process.execFileSync',
          result.status,
          result.signal,
          result.stdout,
          result.stderr,
        );
      }
      return result.stdout;
    },
    fork(modulePath, args, options) {
      const invocation = normalizeSpawnInvocation(args, options);
      return module.spawn('node', [modulePath, ...invocation.args], {
        ...invocation.options,
        stdio: invocation.options?.stdio ?? ['pipe', 'pipe', 'pipe'],
      });
    },
  };

  return module;
}

function createRpcBackedNetModule(netModule, fromGuestDir = '/') {
  const RPC_POLL_WAIT_MS = 50;
  const RPC_IDLE_POLL_DELAY_MS = 10;
  const bridge = () => requireAgentOsSyncRpcBridge();
  const createUnsupportedNetError = (subject) => {
    const error = new Error(`${subject} is not supported by the Agent OS net polyfill yet`);
    error.code = 'ERR_AGENT_OS_NET_UNSUPPORTED';
    return error;
  };
  const normalizeNetPort = (value) => {
    const numeric =
      typeof value === 'number'
        ? value
        : typeof value === 'string' && value.length > 0
          ? Number(value)
          : Number.NaN;
    if (!Number.isInteger(numeric) || numeric < 0 || numeric > 65535) {
      throw new RangeError(`Agent OS net port must be an integer between 0 and 65535`);
    }
    return numeric;
  };
  const normalizeNetBacklog = (value) => {
    const numeric =
      typeof value === 'number'
        ? value
        : typeof value === 'string' && value.length > 0
          ? Number(value)
          : Number.NaN;
    if (!Number.isInteger(numeric) || numeric < 0) {
      throw new RangeError(`Agent OS net backlog must be a non-negative integer`);
    }
    return numeric;
  };
  const normalizeNetConnectInvocation = (args) => {
    const values = [...args];
    const callback =
      typeof values[values.length - 1] === 'function' ? values.pop() : undefined;

    let options;
    if (values[0] != null && typeof values[0] === 'object') {
      options = { ...values[0] };
    } else {
      options = { port: values[0] };
      if (typeof values[1] === 'string') {
        options.host = values[1];
      }
    }

    if (typeof options?.path === 'string') {
      throw createUnsupportedNetError('net.connect({ path })');
    }
    if (options?.lookup != null) {
      throw createUnsupportedNetError('net.connect({ lookup })');
    }

    return {
      callback,
      options: {
        allowHalfOpen: options?.allowHalfOpen === true,
        host:
          typeof options?.host === 'string' && options.host.length > 0
            ? options.host
            : 'localhost',
        port: normalizeNetPort(options?.port),
      },
    };
  };
  const normalizeNetServerCreation = (args) => {
    let options = {};
    let connectionListener;

    if (typeof args[0] === 'function') {
      connectionListener = args[0];
    } else {
      if (args[0] != null) {
        if (typeof args[0] !== 'object') {
          throw new TypeError('net.createServer options must be an object');
        }
        options = { ...args[0] };
      }
      if (typeof args[1] === 'function') {
        connectionListener = args[1];
      }
    }

    return {
      connectionListener,
      options: {
        allowHalfOpen: options.allowHalfOpen === true,
        pauseOnConnect: options.pauseOnConnect === true,
      },
    };
  };
  const normalizeNetListenInvocation = (args) => {
    const values = [...args];
    const callback =
      typeof values[values.length - 1] === 'function' ? values.pop() : undefined;

    let backlog;
    if (typeof values[values.length - 1] === 'number') {
      backlog = normalizeNetBacklog(values.pop());
    }

    let options;
    if (values[0] != null && typeof values[0] === 'object') {
      options = { ...values[0] };
    } else {
      options = { port: values[0] };
      if (typeof values[1] === 'string') {
        options.host = values[1];
      }
    }

    if (typeof options?.path === 'string') {
      throw createUnsupportedNetError('net.Server.listen({ path })');
    }
    if (options?.signal != null) {
      throw createUnsupportedNetError('net.Server.listen({ signal })');
    }

    return {
      callback,
      options: {
        backlog:
          options?.backlog != null
            ? normalizeNetBacklog(options.backlog)
            : backlog,
        host:
          typeof options?.host === 'string' && options.host.length > 0
            ? options.host
            : '0.0.0.0',
        port: normalizeNetPort(options?.port ?? 0),
      },
    };
  };
  const socketFamilyForAddress = (value) => {
    if (typeof value !== 'string') {
      return undefined;
    }
    return value.includes(':') ? 'IPv6' : 'IPv4';
  };
  const callConnect = (options) => bridge().callSync('net.connect', [options]);
  const callListen = (options) => bridge().callSync('net.listen', [options]);
  const callPoll = (socketId, waitMs = 0) => bridge().callSync('net.poll', [socketId, waitMs]);
  const callServerPoll = (serverId, waitMs = 0) =>
    bridge().callSync('net.server_poll', [serverId, waitMs]);
  const callWrite = (socketId, chunk) =>
    bridge().call('net.write', [socketId, toGuestBufferView(chunk, 'net.write chunk')]);
  const callShutdown = (socketId) => bridge().call('net.shutdown', [socketId]);
  const callDestroy = (socketId) => bridge().call('net.destroy', [socketId]);
  const callServerClose = (serverId) => bridge().call('net.server_close', [serverId]);

  const finalizeSocketClose = (socket, hadError = false) => {
    if (socket._agentOsClosed) {
      return;
    }
    socket._agentOsClosed = true;
    socket.connecting = false;
    socket.pending = false;
    socket._pollTimer && clearTimeout(socket._pollTimer);
    socket._pollTimer = null;
    if (!socket.readableEnded) {
      socket.push(null);
    }
    queueMicrotask(() => socket.emit('close', hadError));
  };

  const scheduleSocketPoll = (socket, delayMs) => {
    if (socket._agentOsClosed || socket._agentOsSocketId == null || socket._pollTimer != null) {
      return;
    }

    socket._pollTimer = setTimeout(() => {
      socket._pollTimer = null;
      if (socket._agentOsClosed || socket._agentOsSocketId == null) {
        return;
      }

      let event;
      try {
        event = callPoll(socket._agentOsSocketId, RPC_POLL_WAIT_MS);
      } catch (error) {
        socket.destroy(error);
        return;
      }

      if (!event) {
        scheduleSocketPoll(socket, RPC_IDLE_POLL_DELAY_MS);
        return;
      }

      if (event.type === 'data') {
        const chunk = decodeFsBytesPayload(event.data, 'net.data');
        socket.bytesRead += chunk.length;
        socket.push(chunk);
        scheduleSocketPoll(socket, 0);
        return;
      }

      if (event.type === 'end') {
        socket.push(null);
        if (!socket._agentOsAllowHalfOpen && !socket.writableEnded) {
          socket.end();
        }
        scheduleSocketPoll(socket, 0);
        return;
      }

      if (event.type === 'error') {
        const error = new Error(
          typeof event.message === 'string' ? event.message : 'Agent OS net socket error',
        );
        if (typeof event.code === 'string' && event.code.length > 0) {
          error.code = event.code;
        }
        socket.emit('error', error);
        scheduleSocketPoll(socket, 0);
        return;
      }

      if (event.type === 'close') {
        finalizeSocketClose(socket, event.hadError === true);
        return;
      }

      scheduleSocketPoll(socket, 0);
    }, delayMs);

    if (!socket._agentOsRefed) {
      socket._pollTimer.unref?.();
    }
  };
  const attachSocketState = (socket, result, options = {}, emitConnect = false) => {
    socket._agentOsAllowHalfOpen = options.allowHalfOpen === true;
    socket._agentOsSocketId = String(result.socketId);
    socket.localAddress = result.localAddress;
    socket.localPort = result.localPort;
    socket.remoteAddress = result.remoteAddress;
    socket.remotePort = result.remotePort;
    socket.remoteFamily =
      result.remoteFamily ?? socketFamilyForAddress(socket.remoteAddress);
    socket.connecting = false;
    socket.pending = false;
    socket._agentOsClosed = false;
    if (emitConnect) {
      queueMicrotask(() => {
        if (socket._agentOsClosed) {
          return;
        }
        socket.emit('connect');
        socket.emit('ready');
      });
    }
    scheduleSocketPoll(socket, 0);
  };

  class AgentOsSocket extends Duplex {
    constructor(options = undefined) {
      super(options);
      this._agentOsAllowHalfOpen = options?.allowHalfOpen === true;
      this._agentOsClosed = false;
      this._agentOsRefed = true;
      this._agentOsSocketId = null;
      this._pollTimer = null;
      this.bytesRead = 0;
      this.bytesWritten = 0;
      this.connecting = false;
      this.pending = false;
      this.localAddress = undefined;
      this.localPort = undefined;
      this.remoteAddress = undefined;
      this.remoteFamily = undefined;
      this.remotePort = undefined;
    }

    _read() {}

    _write(chunk, encoding, callback) {
      if (this._agentOsSocketId == null) {
        callback(new Error('Agent OS net socket is not connected'));
        return;
      }
      const payload =
        typeof chunk === 'string' ? Buffer.from(chunk, encoding) : Buffer.from(chunk);
      callWrite(this._agentOsSocketId, payload).then(
        (written) => {
          if (typeof written === 'number') {
            this.bytesWritten += written;
          } else {
            this.bytesWritten += payload.length;
          }
          callback();
        },
        (error) => callback(error),
      );
    }

    _final(callback) {
      if (this._agentOsSocketId == null || this._agentOsClosed) {
        callback();
        return;
      }
      callShutdown(this._agentOsSocketId).then(
        () => callback(),
        (error) => callback(error),
      );
    }

    _destroy(error, callback) {
      const socketId = this._agentOsSocketId;
      this._agentOsSocketId = null;
      const finishDestroy = () => {
        finalizeSocketClose(this, Boolean(error));
        callback(error);
      };
      if (socketId == null) {
        finishDestroy();
        return;
      }
      callDestroy(socketId).then(finishDestroy, () => finishDestroy());
    }

    address() {
      if (typeof this.localAddress !== 'string' || typeof this.localPort !== 'number') {
        return null;
      }
      return {
        address: this.localAddress,
        family: socketFamilyForAddress(this.localAddress),
        port: this.localPort,
      };
    }

    connect(...args) {
      const { callback, options } = normalizeNetConnectInvocation(args);
      if (typeof callback === 'function') {
        this.once('connect', callback);
      }
      if (this._agentOsSocketId != null || this.connecting) {
        throw new Error('Agent OS net socket is already connected');
      }

      this._agentOsAllowHalfOpen = options.allowHalfOpen;
      this.connecting = true;
      this.pending = true;

      try {
        const result = callConnect(options);
        attachSocketState(
          this,
          {
            ...result,
            remoteAddress: result.remoteAddress ?? options.host,
            remotePort: result.remotePort ?? options.port,
          },
          options,
          true,
        );
      } catch (error) {
        this.connecting = false;
        this.pending = false;
        this.destroy(error);
      }

      return this;
    }

    ref() {
      this._agentOsRefed = true;
      this._pollTimer?.ref?.();
      return this;
    }

    unref() {
      this._agentOsRefed = false;
      this._pollTimer?.unref?.();
      return this;
    }

    setKeepAlive() {
      return this;
    }

    setNoDelay() {
      return this;
    }

    setTimeout(timeout, callback) {
      if (typeof callback === 'function') {
        if (Number(timeout) > 0) {
          setTimeout(() => {
            if (!this._agentOsClosed) {
              this.emit('timeout');
              callback();
            }
          }, Number(timeout)).unref?.();
        } else {
          queueMicrotask(() => callback());
        }
      }
      return this;
    }
  }

  const finalizeServerClose = (server) => {
    if (server._agentOsClosed) {
      return;
    }
    server._agentOsClosed = true;
    server.listening = false;
    server._agentOsServerId = null;
    server._pollTimer && clearTimeout(server._pollTimer);
    server._pollTimer = null;
    queueMicrotask(() => server.emit('close'));
  };
  const scheduleServerPoll = (server, delayMs) => {
    if (server._agentOsClosed || server._agentOsServerId == null || server._pollTimer != null) {
      return;
    }

    server._pollTimer = setTimeout(() => {
      server._pollTimer = null;
      if (server._agentOsClosed || server._agentOsServerId == null) {
        return;
      }

      let event;
      try {
        event = callServerPoll(server._agentOsServerId, RPC_POLL_WAIT_MS);
      } catch (error) {
        server.emit('error', error);
        finalizeServerClose(server);
        return;
      }

      if (!event) {
        scheduleServerPoll(server, RPC_IDLE_POLL_DELAY_MS);
        return;
      }

      if (event.type === 'connection') {
        const socket = new AgentOsSocket({ allowHalfOpen: server.allowHalfOpen });
        attachSocketState(socket, event, { allowHalfOpen: server.allowHalfOpen });
        if (server.pauseOnConnect) {
          socket.pause();
        }
        server.emit('connection', socket);
        scheduleServerPoll(server, 0);
        return;
      }

      if (event.type === 'error') {
        const error = new Error(
          typeof event.message === 'string' ? event.message : 'Agent OS net server error',
        );
        if (typeof event.code === 'string' && event.code.length > 0) {
          error.code = event.code;
        }
        server.emit('error', error);
        scheduleServerPoll(server, 0);
        return;
      }

      if (event.type === 'close') {
        finalizeServerClose(server);
        return;
      }

      scheduleServerPoll(server, 0);
    }, delayMs);

    if (!server._agentOsRefed) {
      server._pollTimer.unref?.();
    }
  };

  class AgentOsServer extends EventEmitter {
    constructor(options = {}, connectionListener = undefined) {
      super();
      this.allowHalfOpen = options.allowHalfOpen === true;
      this.pauseOnConnect = options.pauseOnConnect === true;
      this.listening = false;
      this.maxConnections = undefined;
      this._agentOsClosed = false;
      this._agentOsRefed = true;
      this._agentOsServerId = null;
      this._pollTimer = null;
      this._address = null;
      if (typeof connectionListener === 'function') {
        this.on('connection', connectionListener);
      }
    }

    address() {
      return this._address;
    }

    close(callback) {
      if (this._agentOsServerId == null || this._agentOsClosed) {
        const error = new Error('Agent OS net server is not running');
        error.code = 'ERR_SERVER_NOT_RUNNING';
        if (typeof callback === 'function') {
          queueMicrotask(() => callback(error));
          return this;
        }
        throw error;
      }

      if (typeof callback === 'function') {
        this.once('close', callback);
      }
      const serverId = this._agentOsServerId;
      callServerClose(serverId).then(
        () => finalizeServerClose(this),
        (error) => this.emit('error', error),
      );
      return this;
    }

    getConnections(callback) {
      if (typeof callback === 'function') {
        queueMicrotask(() => callback(null, 0));
      }
      return Promise.resolve(0);
    }

    listen(...args) {
      const { callback, options } = normalizeNetListenInvocation(args);
      if (typeof callback === 'function') {
        this.once('listening', callback);
      }
      if (this._agentOsServerId != null || this.listening) {
        throw new Error('Agent OS net server is already listening');
      }

      this._agentOsClosed = false;
      try {
        const result = callListen(options);
        this._agentOsServerId = String(result.serverId);
        this._address = {
          address: result.localAddress,
          family: result.family ?? socketFamilyForAddress(result.localAddress),
          port: result.localPort,
        };
        this.listening = true;
        queueMicrotask(() => {
          if (this._agentOsClosed) {
            return;
          }
          this.emit('listening');
        });
        scheduleServerPoll(this, 0);
      } catch (error) {
        this._agentOsServerId = null;
        this._address = null;
        this.listening = false;
        throw error;
      }

      return this;
    }

    ref() {
      this._agentOsRefed = true;
      this._pollTimer?.ref?.();
      return this;
    }

    unref() {
      this._agentOsRefed = false;
      this._pollTimer?.unref?.();
      return this;
    }
  }

  const connect = (...args) => new AgentOsSocket().connect(...args);
  const createServer = (...args) => {
    const { connectionListener, options } = normalizeNetServerCreation(args);
    return new AgentOsServer(options, connectionListener);
  };
  const module = Object.assign(Object.create(netModule ?? null), {
    Server: AgentOsServer,
    Socket: AgentOsSocket,
    Stream: AgentOsSocket,
    connect,
    createConnection: connect,
    createServer,
  });

  return module;
}

function createRpcBackedTlsModule(tlsModule, netModule) {
  const createUnsupportedTlsError = (subject) => {
    const error = new Error(`${subject} is not supported by the Agent OS tls polyfill yet`);
    error.code = 'ERR_AGENT_OS_TLS_UNSUPPORTED';
    return error;
  };
  const defineSocketMetadataPassthrough = (tlsSocket, rawSocket) => {
    for (const key of ['localAddress', 'localPort', 'remoteAddress', 'remotePort', 'remoteFamily']) {
      try {
        Object.defineProperty(tlsSocket, key, {
          configurable: true,
          enumerable: true,
          get() {
            return rawSocket[key];
          },
          set(value) {
            rawSocket[key] = value;
          },
        });
      } catch {
        // Ignore non-configurable host properties.
      }
    }
  };
  const normalizeTlsPort = (value) => {
    const numeric =
      typeof value === 'number'
        ? value
        : typeof value === 'string' && value.length > 0
          ? Number(value)
          : Number.NaN;
    if (!Number.isInteger(numeric) || numeric < 0 || numeric > 65535) {
      throw new RangeError('Agent OS tls port must be between 0 and 65535');
    }
    return numeric;
  };
  const normalizeTlsConnectInvocation = (args) => {
    const values = [...args];
    const callback =
      typeof values[values.length - 1] === 'function' ? values.pop() : undefined;

    let options;
    if (values[0] != null && typeof values[0] === 'object') {
      options = { ...values[0] };
    } else {
      const positional = {};
      if (values.length > 0) {
        positional.port = values.shift();
      }
      if (typeof values[0] === 'string') {
        positional.host = values.shift();
      }
      const providedOptions =
        values[0] != null && typeof values[0] === 'object' ? { ...values[0] } : {};
      options = { ...providedOptions, ...positional };
    }

    if (typeof options?.path === 'string') {
      throw createUnsupportedTlsError('tls.connect({ path })');
    }
    if (options?.lookup != null) {
      throw createUnsupportedTlsError('tls.connect({ lookup })');
    }

    const transportSocket = options?.socket ?? null;
    const host =
      typeof options?.host === 'string' && options.host.length > 0
        ? options.host
        : 'localhost';
    const tlsOptions = { ...options };
    delete tlsOptions.allowHalfOpen;
    delete tlsOptions.host;
    delete tlsOptions.lookup;
    delete tlsOptions.path;
    delete tlsOptions.port;
    delete tlsOptions.socket;
    if (
      typeof tlsOptions.servername !== 'string' &&
      typeof host === 'string' &&
      host.length > 0 &&
      hostNet.isIP(host) === 0
    ) {
      tlsOptions.servername = host;
    }

    return {
      callback,
      transportOptions:
        transportSocket == null
          ? {
              allowHalfOpen: options?.allowHalfOpen === true,
              host,
              port: normalizeTlsPort(options?.port),
            }
          : null,
      transportSocket,
      tlsOptions,
    };
  };
  const normalizeTlsServerCreation = (args) => {
    let options = {};
    let secureConnectionListener;

    if (typeof args[0] === 'function') {
      secureConnectionListener = args[0];
    } else {
      if (args[0] != null) {
        if (typeof args[0] !== 'object') {
          throw new TypeError('tls.createServer options must be an object');
        }
        options = { ...args[0] };
      }
      if (typeof args[1] === 'function') {
        secureConnectionListener = args[1];
      }
    }

    return {
      secureConnectionListener,
      options,
    };
  };
  const createServerSecureContext = (options) =>
    options?.secureContext ?? tlsModule.createSecureContext(options ?? {});
  const createClientTlsSocket = (rawSocket, tlsOptions) => {
    const tlsSocket = tlsModule.connect({
      ...tlsOptions,
      socket: rawSocket,
    });
    defineSocketMetadataPassthrough(tlsSocket, rawSocket);
    return tlsSocket;
  };
  const createServerTlsSocket = (rawSocket, options, secureContext) => {
    const tlsSocket = new tlsModule.TLSSocket(rawSocket, {
      ...options,
      isServer: true,
      secureContext,
    });
    defineSocketMetadataPassthrough(tlsSocket, rawSocket);
    return tlsSocket;
  };

  class AgentOsTlsServer extends EventEmitter {
    constructor(options = {}, secureConnectionListener = undefined) {
      super();
      this._tlsOptions = { ...options };
      this._secureContext = createServerSecureContext(this._tlsOptions);
      this._netServer = netModule.createServer(
        {
          allowHalfOpen: options.allowHalfOpen === true,
          pauseOnConnect: options.pauseOnConnect === true,
        },
        (socket) => {
          const tlsSocket = createServerTlsSocket(socket, this._tlsOptions, this._secureContext);
          tlsSocket.on('secure', () => {
            this.emit('secureConnection', tlsSocket);
          });
          tlsSocket.on('error', (error) => {
            this.emit('tlsClientError', error, tlsSocket);
          });
        },
      );
      if (typeof secureConnectionListener === 'function') {
        this.on('secureConnection', secureConnectionListener);
      }
      this._netServer.on('close', () => this.emit('close'));
      this._netServer.on('error', (error) => this.emit('error', error));
      this._netServer.on('listening', () => this.emit('listening'));

      Object.defineProperties(this, {
        listening: {
          enumerable: true,
          get: () => this._netServer.listening,
        },
        maxConnections: {
          enumerable: true,
          get: () => this._netServer.maxConnections,
          set: (value) => {
            this._netServer.maxConnections = value;
          },
        },
      });
    }

    address() {
      return this._netServer.address();
    }

    close(callback) {
      this._netServer.close(callback);
      return this;
    }

    getConnections(callback) {
      return this._netServer.getConnections(callback);
    }

    listen(...args) {
      this._netServer.listen(...args);
      return this;
    }

    ref() {
      this._netServer.ref();
      return this;
    }

    setSecureContext(options) {
      if (options == null || typeof options !== 'object') {
        throw new TypeError('tls.Server.setSecureContext options must be an object');
      }
      this._tlsOptions = { ...options };
      this._secureContext = createServerSecureContext(this._tlsOptions);
      return this;
    }

    unref() {
      this._netServer.unref();
      return this;
    }
  }

  const connect = (...args) => {
    const { callback, transportOptions, transportSocket, tlsOptions } =
      normalizeTlsConnectInvocation(args);
    const rawSocket =
      transportSocket ??
      netModule.connect({
        allowHalfOpen: transportOptions.allowHalfOpen,
        host: transportOptions.host,
        port: transportOptions.port,
      });
    const tlsSocket = createClientTlsSocket(rawSocket, tlsOptions);
    if (typeof callback === 'function') {
      tlsSocket.once('secureConnect', callback);
    }
    return tlsSocket;
  };
  const createServer = (...args) => {
    const { options, secureConnectionListener } = normalizeTlsServerCreation(args);
    return new AgentOsTlsServer(options, secureConnectionListener);
  };
  const module = Object.assign(Object.create(tlsModule ?? null), {
    Server: AgentOsTlsServer,
    TLSSocket: tlsModule.TLSSocket,
    connect,
    createConnection: connect,
    createServer,
  });

  return module;
}

function createRpcBackedDgramModule(dgramModule, fromGuestDir = '/') {
  const RPC_POLL_WAIT_MS = 50;
  const RPC_IDLE_POLL_DELAY_MS = 10;
  const bridge = () => requireAgentOsSyncRpcBridge();
  const createUnsupportedDgramError = (subject) => {
    const error = new Error(`${subject} is not supported by the Agent OS dgram polyfill yet`);
    error.code = 'ERR_AGENT_OS_DGRAM_UNSUPPORTED';
    return error;
  };
  const normalizeDgramInteger = (value, label) => {
    const numeric =
      typeof value === 'number'
        ? value
        : typeof value === 'string' && value.length > 0
          ? Number(value)
          : Number.NaN;
    if (!Number.isInteger(numeric) || numeric < 0) {
      throw new RangeError(`Agent OS ${label} must be a non-negative integer`);
    }
    return numeric;
  };
  const normalizeDgramPort = (value) => {
    const numeric = normalizeDgramInteger(value, 'dgram port');
    if (numeric > 65535) {
      throw new RangeError(`Agent OS dgram port must be between 0 and 65535`);
    }
    return numeric;
  };
  const socketFamilyForAddress = (value) => {
    if (typeof value !== 'string') {
      return undefined;
    }
    return value.includes(':') ? 'IPv6' : 'IPv4';
  };
  const normalizeDgramType = (value) => {
    if (value === 'udp4' || value === 'udp6') {
      return value;
    }
    throw new TypeError(`Agent OS dgram socket type must be udp4 or udp6`);
  };
  const normalizeDgramCreateSocketInvocation = (args) => {
    const values = [...args];
    const callback =
      typeof values[values.length - 1] === 'function' ? values.pop() : undefined;

    let options;
    if (typeof values[0] === 'string') {
      options = { type: values[0] };
    } else if (values[0] != null && typeof values[0] === 'object') {
      options = { ...values[0] };
    } else {
      throw new TypeError('dgram.createSocket requires a socket type or options object');
    }

    if (options?.recvBufferSize != null || options?.sendBufferSize != null) {
      throw createUnsupportedDgramError('dgram.createSocket({ recvBufferSize/sendBufferSize })');
    }

    return {
      callback,
      options: {
        type: normalizeDgramType(options.type),
      },
    };
  };
  const normalizeDgramBindInvocation = (args, socketType) => {
    const values = [...args];
    const callback =
      typeof values[values.length - 1] === 'function' ? values.pop() : undefined;

    let options;
    if (values[0] != null && typeof values[0] === 'object') {
      options = { ...values[0] };
    } else {
      options = { port: values[0] };
      if (typeof values[1] === 'string') {
        options.address = values[1];
      }
    }

    if (options?.exclusive != null || options?.fd != null || options?.signal != null) {
      throw createUnsupportedDgramError('dgram.Socket.bind advanced options');
    }

    return {
      callback,
      options: {
        port: normalizeDgramPort(options?.port ?? 0),
        address:
          typeof options?.address === 'string' && options.address.length > 0
            ? options.address
            : socketType === 'udp6'
              ? '::'
              : '0.0.0.0',
      },
    };
  };
  const normalizeDgramMessageBuffer = (value) => {
    if (typeof value === 'string') {
      return Buffer.from(value);
    }
    if (Array.isArray(value)) {
      return Buffer.concat(value.map((entry) => normalizeDgramMessageBuffer(entry)));
    }
    return Buffer.from(toGuestBufferView(value, 'dgram payload'));
  };
  const normalizeDgramSendInvocation = (args) => {
    const values = [...args];
    const callback =
      typeof values[values.length - 1] === 'function' ? values.pop() : undefined;
    if (values.length === 0) {
      throw new TypeError('dgram.Socket.send requires a payload');
    }

    let payload = normalizeDgramMessageBuffer(values.shift());
    let port;
    let address;

    if (
      values.length >= 3 &&
      typeof values[0] === 'number' &&
      typeof values[1] === 'number'
    ) {
      const offset = normalizeDgramInteger(values.shift(), 'dgram send offset');
      const length = normalizeDgramInteger(values.shift(), 'dgram send length');
      if (offset > payload.length || offset + length > payload.length) {
        throw new RangeError('Agent OS dgram send offset/length is out of range');
      }
      payload = payload.subarray(offset, offset + length);
      port = normalizeDgramPort(values.shift());
      if (typeof values[0] === 'string') {
        address = values.shift();
      }
    } else if (values[0] != null && typeof values[0] === 'object') {
      const options = { ...values.shift() };
      port = normalizeDgramPort(options.port);
      address = options.address;
    } else {
      port = normalizeDgramPort(values.shift());
      if (typeof values[0] === 'string') {
        address = values.shift();
      }
    }

    return {
      callback,
      options: {
        port,
        address: typeof address === 'string' && address.length > 0 ? address : 'localhost',
      },
      payload,
    };
  };
  const callCreateSocket = (options) => bridge().callSync('dgram.createSocket', [options]);
  const callBind = (socketId, options) => bridge().callSync('dgram.bind', [socketId, options]);
  const callSend = (socketId, payload, options) =>
    bridge().call('dgram.send', [socketId, toGuestBufferView(payload, 'dgram.send payload'), options]);
  const callPoll = (socketId, waitMs = 0) => bridge().callSync('dgram.poll', [socketId, waitMs]);
  const callClose = (socketId) => bridge().call('dgram.close', [socketId]);

  const finalizeDatagramClose = (socket) => {
    if (socket._agentOsClosed) {
      return;
    }
    socket._agentOsClosed = true;
    socket._agentOsBound = false;
    socket._agentOsPollTimer && clearTimeout(socket._agentOsPollTimer);
    socket._agentOsPollTimer = null;
    queueMicrotask(() => socket.emit('close'));
  };
  const attachDatagramBindState = (socket, result, emitListening = false) => {
    const alreadyBound = socket._agentOsBound;
    socket._agentOsBound = true;
    socket._address = {
      address: result.localAddress,
      family: result.family ?? socketFamilyForAddress(result.localAddress),
      port: result.localPort,
    };
    if (emitListening && !alreadyBound) {
      queueMicrotask(() => {
        if (!socket._agentOsClosed) {
          socket.emit('listening');
        }
      });
    }
    scheduleDatagramPoll(socket, 0);
  };
  const scheduleDatagramPoll = (socket, delayMs) => {
    if (
      socket._agentOsClosed ||
      socket._agentOsSocketId == null ||
      !socket._agentOsBound ||
      socket._agentOsPollTimer != null
    ) {
      return;
    }

    socket._agentOsPollTimer = setTimeout(() => {
      socket._agentOsPollTimer = null;
      if (
        socket._agentOsClosed ||
        socket._agentOsSocketId == null ||
        !socket._agentOsBound
      ) {
        return;
      }

      let event;
      try {
        event = callPoll(socket._agentOsSocketId, RPC_POLL_WAIT_MS);
      } catch (error) {
        socket.emit('error', error);
        scheduleDatagramPoll(socket, 0);
        return;
      }

      if (!event) {
        scheduleDatagramPoll(socket, RPC_IDLE_POLL_DELAY_MS);
        return;
      }

      if (event.type === 'message') {
        socket.emit(
          'message',
          decodeFsBytesPayload(event.data, 'dgram.message'),
          {
            address: event.remoteAddress,
            family: event.remoteFamily ?? socketFamilyForAddress(event.remoteAddress),
            port: event.remotePort,
            size: decodeFsBytesPayload(event.data, 'dgram.message').length,
          },
        );
        scheduleDatagramPoll(socket, 0);
        return;
      }

      if (event.type === 'error') {
        const error = new Error(
          typeof event.message === 'string' ? event.message : 'Agent OS dgram socket error',
        );
        if (typeof event.code === 'string' && event.code.length > 0) {
          error.code = event.code;
        }
        socket.emit('error', error);
        scheduleDatagramPoll(socket, 0);
        return;
      }

      scheduleDatagramPoll(socket, 0);
    }, delayMs);

    if (!socket._agentOsRefed) {
      socket._agentOsPollTimer.unref?.();
    }
  };

  class AgentOsDatagramSocket extends EventEmitter {
    constructor(options = {}, messageListener = undefined) {
      super();
      this.type = options.type;
      this._agentOsClosed = false;
      this._agentOsRefed = true;
      this._agentOsBound = false;
      this._agentOsSocketId = null;
      this._agentOsPollTimer = null;
      this._address = null;
      if (typeof messageListener === 'function') {
        this.on('message', messageListener);
      }
      const result = callCreateSocket(options);
      this._agentOsSocketId = String(result.socketId);
    }

    address() {
      return this._address;
    }

    bind(...args) {
      const { callback, options } = normalizeDgramBindInvocation(args, this.type);
      if (typeof callback === 'function') {
        this.once('listening', callback);
      }
      if (this._agentOsClosed) {
        throw new Error('Agent OS dgram socket is closed');
      }
      attachDatagramBindState(this, callBind(this._agentOsSocketId, options), true);
      return this;
    }

    close(callback) {
      if (typeof callback === 'function') {
        this.once('close', callback);
      }
      if (this._agentOsClosed || this._agentOsSocketId == null) {
        queueMicrotask(() => finalizeDatagramClose(this));
        return this;
      }
      this._agentOsBound = false;
      this._agentOsPollTimer && clearTimeout(this._agentOsPollTimer);
      this._agentOsPollTimer = null;
      const socketId = this._agentOsSocketId;
      this._agentOsSocketId = null;
      callClose(socketId).then(
        () => finalizeDatagramClose(this),
        (error) => this.emit('error', error),
      );
      return this;
    }

    send(...args) {
      if (this._agentOsClosed || this._agentOsSocketId == null) {
        const error = new Error('Agent OS dgram socket is closed');
        const callback =
          typeof args[args.length - 1] === 'function' ? args[args.length - 1] : null;
        if (callback) {
          queueMicrotask(() => callback(error));
          return;
        }
        throw error;
      }

      const { callback, options, payload } = normalizeDgramSendInvocation(args);
      callSend(this._agentOsSocketId, payload, options).then(
        (result) => {
          attachDatagramBindState(this, result, true);
          if (typeof callback === 'function') {
            callback(null, typeof result?.bytes === 'number' ? result.bytes : payload.length);
          }
        },
        (error) => {
          if (typeof callback === 'function') {
            callback(error);
            return;
          }
          this.emit('error', error);
        },
      );
    }

    ref() {
      this._agentOsRefed = true;
      this._agentOsPollTimer?.ref?.();
      return this;
    }

    unref() {
      this._agentOsRefed = false;
      this._agentOsPollTimer?.unref?.();
      return this;
    }

    setBroadcast() {
      return this;
    }

    setMulticastInterface() {
      return this;
    }

    setMulticastLoopback() {
      return this;
    }

    setMulticastTTL() {
      return this;
    }

    setRecvBufferSize() {
      return this;
    }

    setSendBufferSize() {
      return this;
    }

    setTTL() {
      return this;
    }

    addMembership() {
      throw createUnsupportedDgramError('dgram.Socket.addMembership');
    }

    connect() {
      throw createUnsupportedDgramError('dgram.Socket.connect');
    }

    disconnect() {
      throw createUnsupportedDgramError('dgram.Socket.disconnect');
    }

    dropMembership() {
      throw createUnsupportedDgramError('dgram.Socket.dropMembership');
    }

    getRecvBufferSize() {
      return 0;
    }

    getSendBufferSize() {
      return 0;
    }

    remoteAddress() {
      throw createUnsupportedDgramError('dgram.Socket.remoteAddress');
    }
  }

  const createSocket = (...args) => {
    const { callback, options } = normalizeDgramCreateSocketInvocation(args);
    return new AgentOsDatagramSocket(options, callback);
  };
  const module = Object.assign(Object.create(dgramModule ?? null), {
    Socket: AgentOsDatagramSocket,
    createSocket,
  });

  return module;
}

function createRpcBackedDnsModule(dnsModule) {
  const bridge = () => requireAgentOsSyncRpcBridge();
  const dnsConstants = Object.freeze({ ...(dnsModule?.constants ?? {}) });
  let defaultResultOrder = 'verbatim';

  const createUnsupportedDnsError = (subject) => {
    const error = new Error(`${subject} is not supported by the Agent OS dns polyfill yet`);
    error.code = 'ERR_AGENT_OS_DNS_UNSUPPORTED';
    return error;
  };

  const normalizeDnsHostname = (hostname, methodName) => {
    if (typeof hostname !== 'string' || hostname.length === 0) {
      throw new TypeError(`Agent OS ${methodName} hostname must be a non-empty string`);
    }
    return hostname;
  };

  const normalizeDnsFamily = (value, label, allowAny = true) => {
    if (value == null) {
      return allowAny ? 0 : 4;
    }
    const numeric =
      typeof value === 'number'
        ? value
        : typeof value === 'string' && value.length > 0
          ? Number(value)
          : Number.NaN;
    if (
      !Number.isInteger(numeric) ||
      (!allowAny && numeric !== 4 && numeric !== 6) ||
      (allowAny && numeric !== 0 && numeric !== 4 && numeric !== 6)
    ) {
      throw new TypeError(
        `Agent OS ${label} must be ${allowAny ? '0, 4, or 6' : '4 or 6'}`,
      );
    }
    return numeric;
  };

  const normalizeDnsResultOrder = (value) => {
    const normalized = value == null ? defaultResultOrder : String(value);
    if (
      normalized !== 'verbatim' &&
      normalized !== 'ipv4first' &&
      normalized !== 'ipv6first'
    ) {
      throw new TypeError(
        'Agent OS dns result order must be one of verbatim, ipv4first, or ipv6first',
      );
    }
    return normalized;
  };

  const sortLookupAddresses = (records, order) => {
    if (!Array.isArray(records) || order === 'verbatim') {
      return [...records];
    }
    const rankFamily = (family) => {
      if (order === 'ipv4first') {
        return family === 4 ? 0 : family === 6 ? 1 : 2;
      }
      return family === 6 ? 0 : family === 4 ? 1 : 2;
    };
    return [...records].sort((left, right) => rankFamily(left.family) - rankFamily(right.family));
  };

  const normalizeLookupInvocation = (hostname, options, callback) => {
    let normalizedOptions = {};
    let done = callback;

    if (typeof options === 'function') {
      done = options;
    } else if (typeof options === 'number') {
      normalizedOptions = { family: options };
    } else if (options == null) {
      normalizedOptions = {};
    } else if (typeof options === 'object') {
      normalizedOptions = { ...options };
    } else {
      throw new TypeError('Agent OS dns.lookup options must be a number, object, or callback');
    }

    return {
      callback: done,
      options: {
        hostname: normalizeDnsHostname(hostname, 'dns.lookup'),
        family: normalizeDnsFamily(normalizedOptions.family, 'dns.lookup family'),
        all: normalizedOptions.all === true,
        order: normalizeDnsResultOrder(
          normalizedOptions.order ??
            (normalizedOptions.verbatim === false ? 'ipv4first' : undefined),
        ),
      },
    };
  };

  const normalizeResolveInvocation = (methodName, hostname, rrtype, callback) => {
    let type = rrtype;
    let done = callback;
    if (typeof rrtype === 'function') {
      done = rrtype;
      type = undefined;
    }
    if (type == null) {
      type = 'A';
    }
    const normalizedType = String(type).toUpperCase();
    if (normalizedType !== 'A' && normalizedType !== 'AAAA') {
      throw createUnsupportedDnsError(`${methodName}(${normalizedType})`);
    }
    return {
      callback: done,
      options: {
        hostname: normalizeDnsHostname(hostname, methodName),
        rrtype: normalizedType,
      },
    };
  };

  const resolveRecords = (method, options) => bridge().callSync(method, [options]);
  const lookupRecords = (options) => bridge().callSync('dns.lookup', [options]);

  const lookup = (hostname, options, callback) => {
    const invocation = normalizeLookupInvocation(hostname, options, callback);
    const records = sortLookupAddresses(lookupRecords(invocation.options), invocation.options.order);
    if (typeof invocation.callback === 'function') {
      queueMicrotask(() => {
        if (invocation.options.all) {
          invocation.callback(null, records);
        } else {
          const first = records[0] ?? { address: null, family: invocation.options.family || 0 };
          invocation.callback(null, first.address, first.family);
        }
      });
    }
    return invocation.options.all
      ? records
      : {
          address: records[0]?.address ?? null,
          family: records[0]?.family ?? (invocation.options.family || 0),
        };
  };

  const resolve = (hostname, rrtype, callback) => {
    const invocation = normalizeResolveInvocation('dns.resolve', hostname, rrtype, callback);
    const records = resolveRecords('dns.resolve', invocation.options);
    if (typeof invocation.callback === 'function') {
      queueMicrotask(() => invocation.callback(null, records));
    }
    return records;
  };

  const resolve4 = (hostname, callback) => {
    const invocation = normalizeResolveInvocation('dns.resolve4', hostname, 'A', callback);
    const records = resolveRecords('dns.resolve4', invocation.options);
    if (typeof invocation.callback === 'function') {
      queueMicrotask(() => invocation.callback(null, records));
    }
    return records;
  };

  const resolve6 = (hostname, callback) => {
    const invocation = normalizeResolveInvocation('dns.resolve6', hostname, 'AAAA', callback);
    const records = resolveRecords('dns.resolve6', invocation.options);
    if (typeof invocation.callback === 'function') {
      queueMicrotask(() => invocation.callback(null, records));
    }
    return records;
  };

  class AgentOsResolver {
    cancel() {}

    getServers() {
      return [];
    }

    lookup(hostname, options, callback) {
      return lookup(hostname, options, callback);
    }

    resolve(hostname, rrtype, callback) {
      return resolve(hostname, rrtype, callback);
    }

    resolve4(hostname, callback) {
      return resolve4(hostname, callback);
    }

    resolve6(hostname, callback) {
      return resolve6(hostname, callback);
    }

    setServers() {
      throw createUnsupportedDnsError('dns.Resolver.setServers');
    }
  }

  class AgentOsPromisesResolver {
    cancel() {}

    getServers() {
      return [];
    }

    lookup(hostname, options) {
      return Promise.resolve(lookup(hostname, options));
    }

    resolve(hostname, rrtype) {
      return Promise.resolve(resolve(hostname, rrtype));
    }

    resolve4(hostname) {
      return Promise.resolve(resolve4(hostname));
    }

    resolve6(hostname) {
      return Promise.resolve(resolve6(hostname));
    }

    setServers() {
      throw createUnsupportedDnsError('dns.promises.Resolver.setServers');
    }
  }

  const promises = Object.freeze({
    Resolver: AgentOsPromisesResolver,
    lookup(hostname, options) {
      return Promise.resolve(lookup(hostname, options));
    },
    resolve(hostname, rrtype) {
      return Promise.resolve(resolve(hostname, rrtype));
    },
    resolve4(hostname) {
      return Promise.resolve(resolve4(hostname));
    },
    resolve6(hostname) {
      return Promise.resolve(resolve6(hostname));
    },
  });

  const module = {
    ADDRCONFIG: dnsConstants.ADDRCONFIG,
    ALL: dnsConstants.ALL,
    V4MAPPED: dnsConstants.V4MAPPED,
    Resolver: AgentOsResolver,
    constants: dnsConstants,
    getDefaultResultOrder() {
      return defaultResultOrder;
    },
    getServers() {
      return [];
    },
    lookup,
    lookupService() {
      throw createUnsupportedDnsError('dns.lookupService');
    },
    promises,
    resolve,
    resolve4,
    resolve6,
    reverse() {
      throw createUnsupportedDnsError('dns.reverse');
    },
    setDefaultResultOrder(order) {
      defaultResultOrder = normalizeDnsResultOrder(order);
    },
    setServers() {
      throw createUnsupportedDnsError('dns.setServers');
    },
  };

  return module;
}

const guestRequireCache = new Map();
let rootGuestRequire = null;
const hostFs = fs;
const hostFsPromises = fs.promises;
const hostFsWriteSync = fs.writeSync.bind(fs);
const hostFsCloseSync = fs.closeSync.bind(fs);
const guestFs = wrapFsModule(hostFs);
globalThis.__agentOsGuestFs = guestFs;
const guestChildProcess = createRpcBackedChildProcessModule(INITIAL_GUEST_CWD);
const guestNet = createRpcBackedNetModule(hostNet, INITIAL_GUEST_CWD);
const guestDgram = createRpcBackedDgramModule(hostDgram, INITIAL_GUEST_CWD);
const guestDns = createRpcBackedDnsModule(hostDns);
const guestTls = createRpcBackedTlsModule(hostTls, guestNet);
const guestGetUid = () => VIRTUAL_UID;
const guestGetGid = () => VIRTUAL_GID;
const VIRTUAL_OS_HOSTNAME = parseVirtualProcessString(
  HOST_PROCESS_ENV.AGENT_OS_VIRTUAL_OS_HOSTNAME ?? HOST_PROCESS_ENV.HOSTNAME,
  DEFAULT_VIRTUAL_OS_HOSTNAME,
);
const VIRTUAL_OS_TYPE = parseVirtualProcessString(
  HOST_PROCESS_ENV.AGENT_OS_VIRTUAL_OS_TYPE,
  DEFAULT_VIRTUAL_OS_TYPE,
);
const VIRTUAL_OS_PLATFORM = parseVirtualProcessString(
  HOST_PROCESS_ENV.AGENT_OS_VIRTUAL_OS_PLATFORM,
  DEFAULT_VIRTUAL_OS_PLATFORM,
);
const VIRTUAL_OS_RELEASE = parseVirtualProcessString(
  HOST_PROCESS_ENV.AGENT_OS_VIRTUAL_OS_RELEASE,
  DEFAULT_VIRTUAL_OS_RELEASE,
);
const VIRTUAL_OS_VERSION = parseVirtualProcessString(
  HOST_PROCESS_ENV.AGENT_OS_VIRTUAL_OS_VERSION,
  DEFAULT_VIRTUAL_OS_VERSION,
);
const VIRTUAL_OS_ARCH = parseVirtualProcessString(
  HOST_PROCESS_ENV.AGENT_OS_VIRTUAL_OS_ARCH,
  DEFAULT_VIRTUAL_OS_ARCH,
);
const VIRTUAL_OS_MACHINE = parseVirtualProcessString(
  HOST_PROCESS_ENV.AGENT_OS_VIRTUAL_OS_MACHINE,
  DEFAULT_VIRTUAL_OS_MACHINE,
);
const VIRTUAL_OS_CPU_MODEL = parseVirtualProcessString(
  HOST_PROCESS_ENV.AGENT_OS_VIRTUAL_OS_CPU_MODEL,
  DEFAULT_VIRTUAL_OS_CPU_MODEL,
);
const VIRTUAL_OS_CPU_COUNT = parsePositiveInt(
  HOST_PROCESS_ENV.AGENT_OS_VIRTUAL_OS_CPU_COUNT,
  DEFAULT_VIRTUAL_OS_CPU_COUNT,
);
const VIRTUAL_OS_TOTALMEM = parsePositiveInt(
  HOST_PROCESS_ENV.AGENT_OS_VIRTUAL_OS_TOTALMEM,
  DEFAULT_VIRTUAL_OS_TOTALMEM,
);
const VIRTUAL_OS_FREEMEM = Math.min(
  parsePositiveInt(
    HOST_PROCESS_ENV.AGENT_OS_VIRTUAL_OS_FREEMEM,
    DEFAULT_VIRTUAL_OS_FREEMEM,
  ),
  VIRTUAL_OS_TOTALMEM,
);
let guestProcess = process;

function syncBuiltinModuleExports(hostModule, wrappedModule) {
  if (
    hostModule == null ||
    wrappedModule == null ||
    typeof hostModule !== 'object' ||
    typeof wrappedModule !== 'object'
  ) {
    return;
  }

  for (const [key, value] of Object.entries(wrappedModule)) {
    try {
      hostModule[key] = value;
    } catch {
      // Ignore immutable bindings and keep the original builtin export.
    }
  }
}

function cloneFsModule(fsModule) {
  if (fsModule == null || typeof fsModule !== 'object') {
    return fsModule;
  }

  const cloned = { ...fsModule };
  if (fsModule.promises && typeof fsModule.promises === 'object') {
    cloned.promises = { ...fsModule.promises };
  }
  return cloned;
}

function resolveVirtualPath(value, fallback) {
  if (typeof value !== 'string' || value.length === 0) {
    return fallback;
  }

  return translatePathStringToGuest(value);
}

function cloneVirtualCpuInfo(cpu) {
  return {
    ...cpu,
    times: { ...cpu.times },
  };
}

function cloneVirtualNetworkInterfaces(networkInterfaces) {
  return Object.fromEntries(
    Object.entries(networkInterfaces).map(([name, entries]) => [
      name,
      entries.map((entry) => ({ ...entry })),
    ]),
  );
}

function encodeUserInfoValue(value, encoding) {
  return encoding === 'buffer' ? Buffer.from(String(value)) : String(value);
}

function createGuestOsModule(osModule) {
  const virtualHomeDir = resolveVirtualPath(
    HOST_PROCESS_ENV.AGENT_OS_VIRTUAL_OS_HOMEDIR ?? HOST_PROCESS_ENV.HOME,
    INITIAL_GUEST_CWD,
  );
  const virtualTmpDir = resolveVirtualPath(
    HOST_PROCESS_ENV.AGENT_OS_VIRTUAL_OS_TMPDIR ??
      HOST_PROCESS_ENV.TMPDIR ??
      HOST_PROCESS_ENV.TEMP ??
      HOST_PROCESS_ENV.TMP,
    DEFAULT_VIRTUAL_OS_TMPDIR,
  );
  const virtualUserName = parseVirtualProcessString(
    HOST_PROCESS_ENV.AGENT_OS_VIRTUAL_OS_USER ??
      HOST_PROCESS_ENV.USER ??
      HOST_PROCESS_ENV.LOGNAME,
    DEFAULT_VIRTUAL_OS_USER,
  );
  const virtualShell = resolveVirtualPath(
    HOST_PROCESS_ENV.AGENT_OS_VIRTUAL_OS_SHELL ?? HOST_PROCESS_ENV.SHELL,
    DEFAULT_VIRTUAL_OS_SHELL,
  );
  const virtualCpuInfo = Object.freeze(
    Array.from({ length: VIRTUAL_OS_CPU_COUNT }, () =>
      Object.freeze({
        model: VIRTUAL_OS_CPU_MODEL,
        speed: 0,
        times: Object.freeze({
          user: 0,
          nice: 0,
          sys: 0,
          idle: 0,
          irq: 0,
        }),
      }),
    ),
  );
  const virtualNetworkInterfaces = Object.freeze({
    lo: Object.freeze([
      Object.freeze({
        address: '127.0.0.1',
        netmask: '255.0.0.0',
        family: 'IPv4',
        mac: '00:00:00:00:00:00',
        internal: true,
        cidr: '127.0.0.1/8',
      }),
      Object.freeze({
        address: '::1',
        netmask: 'ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff',
        family: 'IPv6',
        mac: '00:00:00:00:00:00',
        internal: true,
        cidr: '::1/128',
        scopeid: 0,
      }),
    ]),
  });

  return Object.assign(Object.create(osModule ?? null), {
    arch: () => VIRTUAL_OS_ARCH,
    availableParallelism: () => VIRTUAL_OS_CPU_COUNT,
    cpus: () => virtualCpuInfo.map((cpu) => cloneVirtualCpuInfo(cpu)),
    freemem: () => VIRTUAL_OS_FREEMEM,
    getPriority: () => 0,
    homedir: () => virtualHomeDir,
    hostname: () => VIRTUAL_OS_HOSTNAME,
    loadavg: () => [0, 0, 0],
    machine: () => VIRTUAL_OS_MACHINE,
    networkInterfaces: () => cloneVirtualNetworkInterfaces(virtualNetworkInterfaces),
    platform: () => VIRTUAL_OS_PLATFORM,
    release: () => VIRTUAL_OS_RELEASE,
    setPriority: () => {
      throw accessDenied('os.setPriority');
    },
    tmpdir: () => virtualTmpDir,
    totalmem: () => VIRTUAL_OS_TOTALMEM,
    type: () => VIRTUAL_OS_TYPE,
    uptime: () => 0,
    userInfo: (options = undefined) => {
      const encoding =
        options && typeof options === 'object' ? options.encoding : undefined;
      return {
        username: encodeUserInfoValue(virtualUserName, encoding),
        uid: VIRTUAL_UID,
        gid: VIRTUAL_GID,
        shell: encodeUserInfoValue(virtualShell, encoding),
        homedir: encodeUserInfoValue(virtualHomeDir, encoding),
      };
    },
    version: () => VIRTUAL_OS_VERSION,
  });
}

const guestOs = createGuestOsModule(hostOs);

function isProcessSignalEventName(eventName) {
  return typeof eventName === 'string' && SIGNAL_EVENTS.has(eventName);
}

function createBlockedProcessSignalMethod(methodName) {
  const target = process;
  const method =
    typeof target[methodName] === 'function' ? target[methodName].bind(target) : null;
  if (!method) {
    return null;
  }

  return (...args) => {
    const [eventName] = args;
    if (isProcessSignalEventName(eventName)) {
      throw accessDenied(`process.${methodName}(${eventName})`);
    }

    const result = method(...args);
    return result === target ? guestProcess : result;
  };
}

function createGuestProcessProxy(target) {
  return new Proxy(target, {
    get(source, key) {
      switch (key) {
        case 'execPath':
          return VIRTUAL_EXEC_PATH;
        case 'pid':
          return VIRTUAL_PID;
        case 'ppid':
          return VIRTUAL_PPID;
        case 'getuid':
          return guestGetUid;
        case 'getgid':
          return guestGetGid;
        default:
          return Reflect.get(source, key, source);
      }
    },
    getOwnPropertyDescriptor(source, key) {
      switch (key) {
        case 'execPath':
          return { value: VIRTUAL_EXEC_PATH, writable: false, enumerable: true, configurable: true };
        case 'pid':
          return { value: VIRTUAL_PID, writable: false, enumerable: true, configurable: true };
        case 'ppid':
          return { value: VIRTUAL_PPID, writable: false, enumerable: true, configurable: true };
        case 'getuid':
          return { value: guestGetUid, writable: false, enumerable: true, configurable: true };
        case 'getgid':
          return { value: guestGetGid, writable: false, enumerable: true, configurable: true };
        default:
          return Reflect.getOwnPropertyDescriptor(source, key);
      }
    },
    has(source, key) {
      switch (key) {
        case 'execPath':
        case 'pid':
        case 'ppid':
        case 'getuid':
        case 'getgid':
          return true;
        default:
          return Reflect.has(source, key);
      }
    },
  });
}

function createGuestRequire(fromGuestDir) {
  const normalizedGuestDir = path.posix.normalize(fromGuestDir || '/');
  const cached = guestRequireCache.get(normalizedGuestDir);
  if (cached) {
    return cached;
  }

  const hostDir = hostPathFromGuestPath(normalizedGuestDir) ?? HOST_CWD;
  const baseRequire = Module.createRequire(
    pathToFileURL(path.join(hostDir, '__agent_os_require__.cjs')),
  );

  const guestRequire = function(specifier) {
    const translated = hostPathForSpecifier(specifier, normalizedGuestDir);
    try {
      if (translated) {
        return baseRequire(translated);
      }

      return baseRequire(specifier);
    } catch (error) {
      if (rootGuestRequire && rootGuestRequire !== guestRequire && isBareSpecifier(specifier)) {
        return rootGuestRequire(specifier);
      }
      throw translateErrorToGuest(error);
    }
  };

  guestRequire.resolve = (specifier, options) => {
    const translated = hostPathForSpecifier(specifier, normalizedGuestDir);
    try {
      if (translated) {
        return translatePathStringToGuest(baseRequire.resolve(translated, options));
      }

      return translatePathStringToGuest(baseRequire.resolve(specifier, options));
    } catch (error) {
      if (rootGuestRequire && rootGuestRequire !== guestRequire && isBareSpecifier(specifier)) {
        return rootGuestRequire.resolve(specifier, options);
      }
      throw translateErrorToGuest(error);
    }
  };

  guestRequireCache.set(normalizedGuestDir, guestRequire);
  return guestRequire;
}

function hardenProperty(target, key, value) {
  try {
    Object.defineProperty(target, key, {
      value,
      writable: false,
      configurable: false,
    });
    return;
  } catch {
    // Fall back to assignment below.
  }

  try {
    target[key] = value;
  } catch {
    // Ignore immutable properties; the Node permission model still applies.
  }
}

function encodeSyncRpcValue(value) {
  if (value == null || typeof value === 'string' || typeof value === 'number' || typeof value === 'boolean') {
    return value;
  }

  if (typeof Buffer === 'function' && Buffer.isBuffer(value)) {
    return {
      __agentOsType: 'bytes',
      base64: value.toString('base64'),
    };
  }

  if (ArrayBuffer.isView(value)) {
    return {
      __agentOsType: 'bytes',
      base64: Buffer.from(value.buffer, value.byteOffset, value.byteLength).toString('base64'),
    };
  }

  if (value instanceof ArrayBuffer) {
    return {
      __agentOsType: 'bytes',
      base64: Buffer.from(value).toString('base64'),
    };
  }

  if (Array.isArray(value)) {
    return value.map((entry) => encodeSyncRpcValue(entry));
  }

  if (typeof value === 'object') {
    return Object.fromEntries(
      Object.entries(value).map(([key, entry]) => [key, encodeSyncRpcValue(entry)]),
    );
  }

  return String(value);
}

function decodeSyncRpcValue(value) {
  if (Array.isArray(value)) {
    return value.map((entry) => decodeSyncRpcValue(entry));
  }

  if (value && typeof value === 'object') {
    if (value.__agentOsType === 'bytes' && typeof value.base64 === 'string') {
      return Buffer.from(value.base64, 'base64');
    }

    return Object.fromEntries(
      Object.entries(value).map(([key, entry]) => [key, decodeSyncRpcValue(entry)]),
    );
  }

  return value;
}

function formatSyncRpcError(error) {
  if (error instanceof Error) {
    return {
      message: error.message,
      code: typeof error.code === 'string' ? error.code : undefined,
    };
  }

  return {
    message: String(error),
  };
}

function createNodeSyncRpcBridge() {
  if (!NODE_SYNC_RPC_ENABLE) {
    return null;
  }

  if (NODE_SYNC_RPC_REQUEST_FD == null || NODE_SYNC_RPC_RESPONSE_FD == null) {
    throw new Error('Agent OS Node sync RPC requires request and response file descriptors');
  }

  const Worker = hostWorkerThreads?.Worker;
  if (typeof Worker !== 'function') {
    throw new Error('Agent OS Node sync RPC requires node:worker_threads support');
  }

  const STATE_INDEX = 0;
  const STATUS_INDEX = 1;
  const KIND_INDEX = 2;
  const REQUEST_LENGTH_INDEX = 3;
  const RESPONSE_LENGTH_INDEX = 4;
  const STATE_IDLE = 0;
  const STATE_REQUEST_READY = 1;
  const STATE_RESPONSE_READY = 2;
  const STATE_SHUTDOWN = 3;
  const STATUS_OK = 0;
  const STATUS_ERROR = 1;
  const KIND_JSON = 3;
  const signalBuffer = new SharedArrayBuffer(5 * Int32Array.BYTES_PER_ELEMENT);
  const dataBuffer = new SharedArrayBuffer(NODE_SYNC_RPC_DATA_BYTES);
  const signal = new Int32Array(signalBuffer);
  const data = new Uint8Array(dataBuffer);
  const encoder = new TextEncoder();
  const decoder = new TextDecoder();
  let nextRequestId = 1;
  let disposed = false;

  const workerSource = `
    const { parentPort, workerData } = require('node:worker_threads');
    const { readSync, writeSync, closeSync } = require('node:fs');
    const STATE_INDEX = 0;
    const STATUS_INDEX = 1;
    const KIND_INDEX = 2;
    const REQUEST_LENGTH_INDEX = 3;
    const RESPONSE_LENGTH_INDEX = 4;
    const STATE_IDLE = 0;
    const STATE_REQUEST_READY = 1;
    const STATE_RESPONSE_READY = 2;
    const STATE_SHUTDOWN = 3;
    const STATUS_OK = 0;
    const STATUS_ERROR = 1;
    const KIND_JSON = 3;
    const signal = new Int32Array(workerData.signalBuffer);
    const data = new Uint8Array(workerData.dataBuffer);
    const responseFd = workerData.responseFd;
    const encoder = new TextEncoder();
    const decoder = new TextDecoder();
    let responseBuffer = '';

    function setResponse(status, bytes) {
      let payload = bytes;
      let nextStatus = status;
      if (payload.byteLength > data.byteLength) {
        payload = encoder.encode(JSON.stringify({
          message: 'Agent OS Node sync RPC payload exceeded shared buffer capacity',
          code: 'ERR_AGENT_OS_NODE_SYNC_RPC_PAYLOAD_TOO_LARGE',
        }));
        nextStatus = STATUS_ERROR;
      }

      data.fill(0);
      data.set(payload, 0);
      Atomics.store(signal, STATUS_INDEX, nextStatus);
      Atomics.store(signal, KIND_INDEX, KIND_JSON);
      Atomics.store(signal, RESPONSE_LENGTH_INDEX, payload.byteLength);
      Atomics.store(signal, STATE_INDEX, STATE_RESPONSE_READY);
      Atomics.notify(signal, STATE_INDEX, 1);
    }

    function readResponseLineSync() {
      while (true) {
        const newlineIndex = responseBuffer.indexOf('\\n');
        if (newlineIndex >= 0) {
          const line = responseBuffer.slice(0, newlineIndex);
          responseBuffer = responseBuffer.slice(newlineIndex + 1);
          return line;
        }

        const chunk = Buffer.alloc(4096);
        const bytesRead = readSync(responseFd, chunk, 0, chunk.length, null);
        if (bytesRead === 0) {
          throw new Error('Agent OS Node sync RPC response channel closed unexpectedly');
        }
        responseBuffer += chunk.subarray(0, bytesRead).toString('utf8');
      }
    }

    function waitForRequest() {
      while (true) {
        const state = Atomics.load(signal, STATE_INDEX);
        if (state === STATE_REQUEST_READY || state === STATE_SHUTDOWN) {
          return state;
        }

        Atomics.wait(signal, STATE_INDEX, state);
      }
    }

    try {
      while (true) {
        const state = waitForRequest();
        if (state === STATE_SHUTDOWN) {
          break;
        }

        try {
          const responseLine = readResponseLineSync();
          setResponse(STATUS_OK, encoder.encode(responseLine));
        } catch (error) {
          setResponse(
            STATUS_ERROR,
            encoder.encode(JSON.stringify({
              message: error instanceof Error ? error.message : String(error),
              code: typeof error?.code === 'string' ? error.code : 'ERR_AGENT_OS_NODE_SYNC_RPC',
            })),
          );
        }
      }
    } finally {
      try {
        closeSync(responseFd);
      } catch {}
    }
  `;

  const worker = new Worker(workerSource, {
    eval: true,
    workerData: {
      signalBuffer,
      dataBuffer,
      responseFd: NODE_SYNC_RPC_RESPONSE_FD,
    },
  });
  worker.unref?.();

  const readBytes = (length) => {
    if (length <= 0) {
      return new Uint8Array(0);
    }
    return data.slice(0, length);
  };

  const resetSignal = () => {
    Atomics.store(signal, STATUS_INDEX, STATUS_OK);
    Atomics.store(signal, KIND_INDEX, KIND_JSON);
    Atomics.store(signal, REQUEST_LENGTH_INDEX, 0);
    Atomics.store(signal, RESPONSE_LENGTH_INDEX, 0);
    Atomics.store(signal, STATE_INDEX, STATE_IDLE);
    Atomics.notify(signal, STATE_INDEX, 1);
  };

  const requestRaw = (method, args = []) => {
    if (disposed) {
      throw new Error('Agent OS Node sync RPC bridge is already disposed');
    }

    const payload = encoder.encode(
      JSON.stringify({
        id: nextRequestId++,
        method,
        args: encodeSyncRpcValue(args),
      }),
    );
    if (payload.byteLength > data.byteLength) {
      const error = new Error('Agent OS Node sync RPC request exceeded shared buffer capacity');
      error.code = 'ERR_AGENT_OS_NODE_SYNC_RPC_PAYLOAD_TOO_LARGE';
      throw error;
    }

    data.fill(0);
    data.set(payload, 0);
    hostFsWriteSync(
      NODE_SYNC_RPC_REQUEST_FD,
      `${decoder.decode(data.subarray(0, payload.byteLength))}\n`,
    );
    Atomics.store(signal, STATUS_INDEX, STATUS_OK);
    Atomics.store(signal, KIND_INDEX, KIND_JSON);
    Atomics.store(signal, REQUEST_LENGTH_INDEX, payload.byteLength);
    Atomics.store(signal, RESPONSE_LENGTH_INDEX, 0);
    Atomics.store(signal, STATE_INDEX, STATE_REQUEST_READY);
    Atomics.notify(signal, STATE_INDEX, 1);

    while (true) {
      const result = Atomics.wait(
        signal,
        STATE_INDEX,
        STATE_REQUEST_READY,
        NODE_SYNC_RPC_WAIT_TIMEOUT_MS,
      );
      if (result !== 'timed-out') {
        break;
      }
      throw new Error(`Agent OS Node sync RPC timed out while handling ${method}`);
    }

    const status = Atomics.load(signal, STATUS_INDEX);
    const kind = Atomics.load(signal, KIND_INDEX);
    const length = Atomics.load(signal, RESPONSE_LENGTH_INDEX);
    const bytes = readBytes(length);
    resetSignal();

    if (kind !== KIND_JSON) {
      throw new Error(`Agent OS Node sync RPC returned unsupported payload kind ${kind}`);
    }

    if (status === STATUS_ERROR) {
      const payload = JSON.parse(decoder.decode(bytes));
      const error = new Error(payload?.message || `Agent OS Node sync RPC ${method} failed`);
      if (typeof payload?.code === 'string') {
        error.code = payload.code;
      }
      throw error;
    }

    return JSON.parse(decoder.decode(bytes));
  };

  return {
    callSync(method, args = []) {
      const response = requestRaw(method, args);
      if (response?.ok) {
        return decodeSyncRpcValue(response.result);
      }

      const error = new Error(
        response?.error?.message || `Agent OS Node sync RPC ${method} failed`,
      );
      if (typeof response?.error?.code === 'string') {
        error.code = response.error.code;
      }
      throw error;
    },
    async call(method, args = []) {
      return this.callSync(method, args);
    },
    dispose() {
      if (disposed) {
        return;
      }
      disposed = true;
      Atomics.store(signal, STATE_INDEX, STATE_SHUTDOWN);
      Atomics.notify(signal, STATE_INDEX, 1);
      try {
        hostFsCloseSync(NODE_SYNC_RPC_REQUEST_FD);
      } catch {}
      worker.terminate().catch(() => {});
    },
  };
}

function installGuestHardening() {
  hardenProperty(process, 'env', createGuestProcessEnv(HOST_PROCESS_ENV));
  hardenProperty(process, 'cwd', () => INITIAL_GUEST_CWD);
  hardenProperty(process, 'chdir', () => {
    throw accessDenied('process.chdir');
  });
  syncBuiltinModuleExports(hostFs, guestFs);
  syncBuiltinModuleExports(hostFsPromises, guestFs.promises);
  try {
    syncBuiltinESMExports();
  } catch {
    // Ignore runtimes that reject syncing builtin ESM exports.
  }

  hardenProperty(process, 'execPath', VIRTUAL_EXEC_PATH);
  hardenProperty(process, 'pid', VIRTUAL_PID);
  hardenProperty(process, 'ppid', VIRTUAL_PPID);
  hardenProperty(process, 'getuid', guestGetUid);
  hardenProperty(process, 'getgid', guestGetGid);

  hardenProperty(process, 'binding', () => {
    throw accessDenied('process.binding');
  });
  hardenProperty(process, '_linkedBinding', () => {
    throw accessDenied('process._linkedBinding');
  });
  hardenProperty(process, 'dlopen', () => {
    throw accessDenied('process.dlopen');
  });
  for (const methodName of [
    'addListener',
    'on',
    'once',
    'prependListener',
    'prependOnceListener',
  ]) {
    const blockedMethod = createBlockedProcessSignalMethod(methodName);
    if (blockedMethod) {
      hardenProperty(process, methodName, blockedMethod);
    }
  }
  if (Module?._extensions && typeof Module._extensions === 'object') {
    hardenProperty(Module._extensions, '.node', () => {
      throw accessDenied('native addon loading');
    });
  }
  if (originalGetBuiltinModule) {
    hardenProperty(process, 'getBuiltinModule', (specifier) => {
      const normalized =
        typeof specifier === 'string' ? normalizeBuiltin(specifier) : null;
      if (normalized === 'process') {
        return guestProcess;
      }
      if (normalized === 'fs') {
        return cloneFsModule(guestFs);
      }
      if (normalized === 'os' && ALLOWED_BUILTINS.has('os')) {
        return guestOs;
      }
      if (normalized === 'net' && ALLOWED_BUILTINS.has('net')) {
        return guestNet;
      }
      if (normalized === 'dgram' && ALLOWED_BUILTINS.has('dgram')) {
        return guestDgram;
      }
      if (normalized === 'dns' && ALLOWED_BUILTINS.has('dns')) {
        return guestDns;
      }
      if (normalized === 'tls' && ALLOWED_BUILTINS.has('tls')) {
        return guestTls;
      }
      if (normalized === 'child_process' && ALLOWED_BUILTINS.has('child_process')) {
        return guestChildProcess;
      }
      if (normalized && DENIED_BUILTINS.has(normalized)) {
        throw accessDenied(`node:${normalized}`);
      }
      return originalGetBuiltinModule(specifier);
    });
  }

  if (originalModuleLoad) {
    Module._load = function(request, parent, isMain) {
      const normalized =
        typeof request === 'string' ? normalizeBuiltin(request) : null;
      if (normalized === 'process') {
        return guestProcess;
      }
      if (normalized === 'fs') {
        return cloneFsModule(guestFs);
      }
      if (normalized === 'os' && ALLOWED_BUILTINS.has('os')) {
        return guestOs;
      }
      if (normalized === 'net' && ALLOWED_BUILTINS.has('net')) {
        return guestNet;
      }
      if (normalized === 'dgram' && ALLOWED_BUILTINS.has('dgram')) {
        return guestDgram;
      }
      if (normalized === 'dns' && ALLOWED_BUILTINS.has('dns')) {
        return guestDns;
      }
      if (normalized === 'tls' && ALLOWED_BUILTINS.has('tls')) {
        return guestTls;
      }
      if (normalized === 'child_process' && ALLOWED_BUILTINS.has('child_process')) {
        return guestChildProcess;
      }
      if (normalized && DENIED_BUILTINS.has(normalized)) {
        throw accessDenied(`node:${normalized}`);
      }

      return originalModuleLoad(request, parent, isMain);
    };
  }

  if (originalFetch) {
    const restrictedFetch = async (resource, init) => {
      const candidate =
        typeof resource === 'string'
          ? resource
          : resource instanceof URL
            ? resource.href
            : resource?.url;

      let url;
      try {
        url = new URL(String(candidate ?? ''));
      } catch {
        throw accessDenied('network access');
      }

      if (url.protocol !== 'data:') {
        const normalizedPort =
          url.port || (url.protocol === 'https:' ? '443' : url.protocol === 'http:' ? '80' : '');
        const loopbackHost =
          url.hostname === '127.0.0.1' ||
          url.hostname === 'localhost' ||
          url.hostname === '::1' ||
          url.hostname === '[::1]';
        const loopbackAllowed =
          loopbackHost &&
          (url.protocol === 'http:' || url.protocol === 'https:') &&
          LOOPBACK_EXEMPT_PORTS.has(normalizedPort);

        if (!loopbackAllowed) {
          throw accessDenied(`network access to ${url.protocol}`);
        }
      }

      return originalFetch(resource, init);
    };

    hardenProperty(globalThis, 'fetch', restrictedFetch);
  }
}

const entrypoint = HOST_PROCESS_ENV.AGENT_OS_ENTRYPOINT;
if (!entrypoint) {
  throw new Error('AGENT_OS_ENTRYPOINT is required');
}

const guestSyncRpc = createNodeSyncRpcBridge();
installGuestHardening();
rootGuestRequire = createGuestRequire('/root/node_modules');
if (ALLOWED_BUILTINS.has('child_process')) {
  hardenProperty(globalThis, '__agentOsBuiltinChildProcess', guestChildProcess);
}
hardenProperty(globalThis, '__agentOsBuiltinFs', guestFs);
if (ALLOWED_BUILTINS.has('net')) {
  hardenProperty(globalThis, '__agentOsBuiltinNet', guestNet);
}
if (ALLOWED_BUILTINS.has('dgram')) {
  hardenProperty(globalThis, '__agentOsBuiltinDgram', guestDgram);
}
if (ALLOWED_BUILTINS.has('dns')) {
  hardenProperty(globalThis, '__agentOsBuiltinDns', guestDns);
}
if (ALLOWED_BUILTINS.has('tls')) {
  hardenProperty(globalThis, '__agentOsBuiltinTls', guestTls);
}
if (ALLOWED_BUILTINS.has('os')) {
  hardenProperty(globalThis, '__agentOsBuiltinOs', guestOs);
}
if (guestSyncRpc) {
  hardenProperty(globalThis, '__agentOsSyncRpc', guestSyncRpc);
}
hardenProperty(globalThis, '_requireFrom', (specifier, fromDir = '/') =>
  createGuestRequire(fromDir)(specifier),
);
hardenProperty(
  globalThis,
  'require',
  createGuestRequire(path.posix.dirname(guestEntryPoint ?? entrypoint)),
);

if (HOST_PROCESS_ENV.AGENT_OS_KEEP_STDIN_OPEN === '1') {
  let stdinKeepalive = setInterval(() => {}, 1_000_000);
  const releaseStdinKeepalive = () => {
    if (stdinKeepalive !== null) {
      clearInterval(stdinKeepalive);
      stdinKeepalive = null;
    }
  };

  process.stdin.resume();
  process.stdin.once('end', releaseStdinKeepalive);
  process.stdin.once('close', releaseStdinKeepalive);
  process.stdin.once('error', releaseStdinKeepalive);
}

const guestArgv = JSON.parse(HOST_PROCESS_ENV.AGENT_OS_GUEST_ARGV ?? '[]');
const bootstrapModule = HOST_PROCESS_ENV.AGENT_OS_BOOTSTRAP_MODULE;
const entrypointPath = isPathLike(entrypoint)
  ? path.resolve(process.cwd(), entrypoint)
  : entrypoint;

process.argv = [VIRTUAL_EXEC_PATH, guestEntryPoint ?? entrypointPath, ...guestArgv];
guestProcess = createGuestProcessProxy(process);
hardenProperty(globalThis, 'process', guestProcess);

try {
  if (bootstrapModule) {
    await import(toImportSpecifier(bootstrapModule));
  }

  await import(toImportSpecifier(entrypoint));
} catch (error) {
  throw translateErrorToGuest(error);
} finally {
  guestSyncRpc?.dispose?.();
}
"#;

const NODE_TIMING_BOOTSTRAP_SOURCE: &str = r#"
const frozenTimeValue = Number(process.env.AGENT_OS_FROZEN_TIME_MS);
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

const imports = JSON.parse(process.env.AGENT_OS_NODE_PREWARM_IMPORTS ?? '[]');
for (const specifier of imports) {
  await import(toImportSpecifier(specifier));
}
"#;

const NODE_WASM_RUNNER_SOURCE: &str = r#"
import fs from 'node:fs/promises';
import { writeSync } from 'node:fs';
import path from 'node:path';
import { WASI } from 'node:wasi';

const WASI_ERRNO_SUCCESS = 0;
const WASI_ERRNO_FAULT = 21;

function isPathLike(specifier) {
  return specifier.startsWith('.') || specifier.startsWith('/') || specifier.startsWith('file:');
}

function resolveModulePath(specifier) {
  if (specifier.startsWith('file:')) {
    return new URL(specifier);
  }
  if (isPathLike(specifier)) {
    return path.resolve(process.cwd(), specifier);
  }
  return specifier;
}

const modulePath = process.env.AGENT_OS_WASM_MODULE_PATH;
if (!modulePath) {
  throw new Error('AGENT_OS_WASM_MODULE_PATH is required');
}

const guestArgv = JSON.parse(process.env.AGENT_OS_GUEST_ARGV ?? '[]');
const guestEnv = JSON.parse(process.env.AGENT_OS_GUEST_ENV ?? '{}');
const prewarmOnly = process.env.AGENT_OS_WASM_PREWARM_ONLY === '1';
const frozenTimeValue = Number(process.env.AGENT_OS_FROZEN_TIME_MS);
const frozenTimeMs = Number.isFinite(frozenTimeValue) ? Math.trunc(frozenTimeValue) : Date.now();
const frozenTimeNs = BigInt(frozenTimeMs) * 1000000n;
const CONTROL_PIPE_FD = parseControlPipeFd(process.env.AGENT_OS_CONTROL_PIPE_FD);

const moduleBytes = await fs.readFile(resolveModulePath(modulePath));
const module = await WebAssembly.compile(moduleBytes);

if (prewarmOnly) {
  process.exit(0);
}

const wasi = new WASI({
  version: 'preview1',
  args: guestArgv,
  env: guestEnv,
  preopens: {
    '/workspace': process.cwd(),
  },
  returnOnExit: true,
});

let instanceMemory = null;
const wasiImport = { ...wasi.wasiImport };
const delegateClockTimeGet =
  typeof wasi.wasiImport.clock_time_get === 'function'
    ? wasi.wasiImport.clock_time_get.bind(wasi.wasiImport)
    : null;
const delegateClockResGet =
  typeof wasi.wasiImport.clock_res_get === 'function'
    ? wasi.wasiImport.clock_res_get.bind(wasi.wasiImport)
    : null;

function decodeSignalMask(maskLo, maskHi) {
  const values = [];
  const lo = Number(maskLo) >>> 0;
  const hi = Number(maskHi) >>> 0;
  for (let bit = 0; bit < 32; bit += 1) {
    if (((lo >>> bit) & 1) === 1) {
      values.push(bit + 1);
    }
  }
  for (let bit = 0; bit < 32; bit += 1) {
    if (((hi >>> bit) & 1) === 1) {
      values.push(bit + 33);
    }
  }
  return values;
}

function parseControlPipeFd(value) {
  if (typeof value !== 'string' || value.trim() === '') {
    return null;
  }

  const parsed = Number.parseInt(value, 10);
  return Number.isInteger(parsed) && parsed >= 0 ? parsed : null;
}

function emitControlMessage(message) {
  if (CONTROL_PIPE_FD == null) {
    return;
  }

  try {
    writeSync(CONTROL_PIPE_FD, `${JSON.stringify(message)}\n`);
  } catch {
    // Ignore control-channel write failures during teardown.
  }
}

const hostProcessImport = {
  proc_sigaction(signal, action, maskLo, maskHi, flags) {
    try {
      const registration = {
        action: action === 0 ? 'default' : action === 1 ? 'ignore' : 'user',
        mask: decodeSignalMask(maskLo, maskHi),
        flags: Number(flags) >>> 0,
      };
      emitControlMessage({
        type: 'signal_state',
        signal: Number(signal) >>> 0,
        registration,
      });
      return WASI_ERRNO_SUCCESS;
    } catch {
      return WASI_ERRNO_FAULT;
    }
  },
};

wasiImport.clock_time_get = (clockId, precision, resultPtr) => {
  if (!(instanceMemory instanceof WebAssembly.Memory)) {
    return delegateClockTimeGet
      ? delegateClockTimeGet(clockId, precision, resultPtr)
      : WASI_ERRNO_FAULT;
  }

  try {
    const view = new DataView(instanceMemory.buffer);
    view.setBigUint64(Number(resultPtr), frozenTimeNs, true);
    return WASI_ERRNO_SUCCESS;
  } catch {
    return WASI_ERRNO_FAULT;
  }
};

wasiImport.clock_res_get = (clockId, resultPtr) => {
  if (!(instanceMemory instanceof WebAssembly.Memory)) {
    return delegateClockResGet
      ? delegateClockResGet(clockId, resultPtr)
      : WASI_ERRNO_FAULT;
  }

  try {
    const view = new DataView(instanceMemory.buffer);
    view.setBigUint64(Number(resultPtr), 1000000n, true);
    return WASI_ERRNO_SUCCESS;
  } catch {
    return WASI_ERRNO_FAULT;
  }
};

const instance = await WebAssembly.instantiate(module, {
  wasi_snapshot_preview1: wasiImport,
  wasi_unstable: wasiImport,
  host_process: hostProcessImport,
});

if (instance.exports.memory instanceof WebAssembly.Memory) {
  instanceMemory = instance.exports.memory;
}

if (typeof instance.exports._start === 'function') {
  const exitCode = wasi.start(instance);
  if (typeof exitCode === 'number' && exitCode !== 0) {
    process.exitCode = exitCode;
  }
} else if (typeof instance.exports.run === 'function') {
  const result = await instance.exports.run();
  if (typeof result !== 'undefined') {
    console.log(String(result));
  }
} else {
  throw new Error('WebAssembly module must export _start or run');
}
"#;

const NODE_PYTHON_RUNNER_SOURCE: &str = r#"
import { closeSync, createReadStream, readSync, writeSync } from 'node:fs';
import { readFile } from 'node:fs/promises';
import { register } from 'node:module';
import { performance as realPerformance } from 'node:perf_hooks';
import path from 'node:path';
import readline from 'node:readline';
import { fileURLToPath, pathToFileURL } from 'node:url';

const ACCESS_DENIED_CODE = 'ERR_ACCESS_DENIED';
const ASSET_ROOT_ENV = 'AGENT_OS_NODE_IMPORT_CACHE_ASSET_ROOT';
const PYODIDE_INDEX_URL_ENV = 'AGENT_OS_PYODIDE_INDEX_URL';
const PYTHON_CODE_ENV = 'AGENT_OS_PYTHON_CODE';
const PYTHON_FILE_ENV = 'AGENT_OS_PYTHON_FILE';
const PYTHON_PREWARM_ONLY_ENV = 'AGENT_OS_PYTHON_PREWARM_ONLY';
const PYTHON_WARMUP_DEBUG_ENV = 'AGENT_OS_PYTHON_WARMUP_DEBUG';
const PYTHON_WARMUP_METRICS_PREFIX = '__AGENT_OS_PYTHON_WARMUP_METRICS__:';
const PYTHON_PRELOAD_PACKAGES_ENV = 'AGENT_OS_PYTHON_PRELOAD_PACKAGES';
const PYTHON_VFS_RPC_REQUEST_FD_ENV = 'AGENT_OS_PYTHON_VFS_RPC_REQUEST_FD';
const PYTHON_VFS_RPC_RESPONSE_FD_ENV = 'AGENT_OS_PYTHON_VFS_RPC_RESPONSE_FD';
const STDIN_FD = 0;
const SUPPORTED_PRELOAD_PACKAGES = ['numpy', 'pandas'];
const SUPPORTED_PRELOAD_PACKAGE_SET = new Set(SUPPORTED_PRELOAD_PACKAGES);
const DENIED_BUILTINS = new Set([
  'child_process',
  'cluster',
  'dgram',
  'diagnostics_channel',
  'dns',
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
]);
const originalFetch =
  typeof globalThis.fetch === 'function'
    ? globalThis.fetch.bind(globalThis)
    : null;
const originalRequire =
  typeof globalThis.require === 'function'
    ? globalThis.require.bind(globalThis)
    : null;
const originalGetBuiltinModule =
  typeof process.getBuiltinModule === 'function'
    ? process.getBuiltinModule.bind(process)
    : null;
const CONTROL_PIPE_FD = parseControlPipeFd(process.env.AGENT_OS_CONTROL_PIPE_FD);

function requiredEnv(name) {
  const value = process.env[name];
  if (value == null) {
    throw new Error(`${name} is required`);
  }
  return value;
}

function parseControlPipeFd(value) {
  if (typeof value !== 'string' || value.trim() === '') {
    return null;
  }

  const parsed = Number.parseInt(value, 10);
  return Number.isInteger(parsed) && parsed >= 0 ? parsed : null;
}

function emitControlMessage(message) {
  if (CONTROL_PIPE_FD == null) {
    return;
  }

  try {
    writeSync(CONTROL_PIPE_FD, `${JSON.stringify(message)}\n`);
  } catch {
    // Ignore control-channel write failures during teardown.
  }
}

function normalizeDirectoryPath(value) {
  return value.endsWith(path.sep) ? value : `${value}${path.sep}`;
}

function resolveIndexLocation(value) {
  if (/^[A-Za-z][A-Za-z0-9+.-]*:/.test(value)) {
    const normalizedUrl = value.endsWith('/') ? value : `${value}/`;
    if (!normalizedUrl.startsWith('file:')) {
      return {
        indexPath: normalizedUrl,
        indexUrl: normalizedUrl,
      };
    }

    const indexPath = normalizeDirectoryPath(fileURLToPath(normalizedUrl));
    return {
      indexPath,
      indexUrl: pathToFileURL(indexPath).href,
    };
  }

  const indexPath = normalizeDirectoryPath(path.resolve(value));
  return {
    indexPath,
    indexUrl: pathToFileURL(indexPath).href,
  };
}

function writeStream(stream, message) {
  if (message == null) {
    return;
  }

  const value = typeof message === 'string' ? message : String(message);
  stream.write(value.endsWith('\n') ? value : `${value}\n`);
}

function formatError(error) {
  if (error instanceof Error) {
    return error.stack || error.message || String(error);
  }

  return String(error);
}

function emitPythonStartupMetrics({
  prewarmOnly,
  startupMs,
  loadPyodideMs,
  packageLoadMs,
  packageCount,
  source,
}) {
  if (process.env[PYTHON_WARMUP_DEBUG_ENV] !== '1') {
    return;
  }

  writeStream(
    process.stderr,
    `${PYTHON_WARMUP_METRICS_PREFIX}${JSON.stringify({
      phase: 'startup',
      prewarmOnly,
      startupMs,
      loadPyodideMs,
      packageLoadMs,
      packageCount,
      source,
    })}`,
  );
}

function parsePreloadPackages(value) {
  if (value == null || value.trim() === '') {
    return [];
  }

  let parsed;
  try {
    parsed = JSON.parse(value);
  } catch (error) {
    throw new Error(
      `${PYTHON_PRELOAD_PACKAGES_ENV} must be a JSON array of package names: ${formatError(error)}`,
    );
  }

  if (!Array.isArray(parsed)) {
    throw new Error(`${PYTHON_PRELOAD_PACKAGES_ENV} must be a JSON array of package names`);
  }

  const packages = [];
  const seen = new Set();

  for (const entry of parsed) {
    if (typeof entry !== 'string') {
      throw new Error(`${PYTHON_PRELOAD_PACKAGES_ENV} entries must be strings`);
    }

    const name = entry.trim();
    if (name.length === 0) {
      throw new Error(`${PYTHON_PRELOAD_PACKAGES_ENV} entries must not be empty`);
    }

    if (!SUPPORTED_PRELOAD_PACKAGE_SET.has(name)) {
      throw new Error(
        `Unsupported bundled Python package "${name}". Available packages: ${SUPPORTED_PRELOAD_PACKAGES.join(', ')}`,
      );
    }

    if (!seen.has(name)) {
      seen.add(name);
      packages.push(name);
    }
  }

  return packages;
}

function parseOptionalFd(name) {
  const value = process.env[name];
  if (value == null || value.trim() === '') {
    return null;
  }

  const fd = Number.parseInt(value, 10);
  if (!Number.isInteger(fd) || fd < 0) {
    throw new Error(`${name} must be a non-negative integer file descriptor`);
  }

  return fd;
}

function rejectPendingRpcRequests(pending, error) {
  for (const { reject } of pending.values()) {
    reject(error);
  }
  pending.clear();
}

function createPythonVfsRpcBridge() {
  const requestFd = parseOptionalFd(PYTHON_VFS_RPC_REQUEST_FD_ENV);
  const responseFd = parseOptionalFd(PYTHON_VFS_RPC_RESPONSE_FD_ENV);

  if (requestFd == null && responseFd == null) {
    return null;
  }

  if (requestFd == null || responseFd == null) {
    throw new Error(
      `both ${PYTHON_VFS_RPC_REQUEST_FD_ENV} and ${PYTHON_VFS_RPC_RESPONSE_FD_ENV} are required`,
    );
  }

  let nextRequestId = 1;
  let responseBuffer = '';
  const queuedResponses = new Map();

  function readResponseLineSync() {
    while (true) {
      const newlineIndex = responseBuffer.indexOf('\n');
      if (newlineIndex >= 0) {
        const line = responseBuffer.slice(0, newlineIndex);
        responseBuffer = responseBuffer.slice(newlineIndex + 1);
        return line;
      }

      const chunk = Buffer.alloc(4096);
      const bytesRead = readSync(responseFd, chunk, 0, chunk.length, null);
      if (bytesRead === 0) {
        throw new Error('Agent OS Python VFS RPC response channel closed unexpectedly');
      }
      responseBuffer += chunk.subarray(0, bytesRead).toString('utf8');
    }
  }

  function parseResponseLine(line) {
    try {
      return JSON.parse(line);
    } catch (error) {
      throw new Error(`invalid Agent OS Python VFS RPC response: ${formatError(error)}`);
    }
  }

  function waitForResponseSync(id) {
    const queued = queuedResponses.get(id);
    if (queued) {
      queuedResponses.delete(id);
      return queued;
    }

    while (true) {
      const line = readResponseLineSync();
      if (line.trim() === '') {
        continue;
      }

      const message = parseResponseLine(line);
      if (message?.id === id) {
        return message;
      }
      queuedResponses.set(message?.id, message);
    }
  }

  function requestSync(method, payload = {}) {
    const id = nextRequestId++;
    writeSync(
      requestFd,
      `${JSON.stringify({
        id,
        method,
        ...payload,
      })}\n`,
    );

    const message = waitForResponseSync(id);
    if (message?.ok) {
      return message.result ?? {};
    }

    const error = new Error(message?.error?.message || `Agent OS Python VFS RPC request ${id} failed`);
    error.code = message?.error?.code || 'ERR_AGENT_OS_PYTHON_VFS_RPC';
    throw error;
  }

  function request(method, payload = {}) {
    return Promise.resolve().then(() => requestSync(method, payload));
  }

  function normalizeWriteContent(content) {
    if (typeof content === 'string') {
      return content;
    }
    if (ArrayBuffer.isView(content)) {
      return Buffer.from(content.buffer, content.byteOffset, content.byteLength).toString('base64');
    }
    if (content instanceof ArrayBuffer) {
      return Buffer.from(content).toString('base64');
    }
    throw new Error('fsWrite requires a base64 string or Uint8Array');
  }

  return {
    fsReadSync(path) {
      const result = requestSync('fsRead', { path });
      return result.contentBase64 ?? '';
    },
    async fsRead(path) {
      return this.fsReadSync(path);
    },
    fsWriteSync(path, content) {
      requestSync('fsWrite', {
        path,
        contentBase64: normalizeWriteContent(content),
      });
    },
    async fsWrite(path, content) {
      this.fsWriteSync(path, content);
    },
    fsStatSync(path) {
      const result = requestSync('fsStat', { path });
      return result.stat ?? null;
    },
    async fsStat(path) {
      return this.fsStatSync(path);
    },
    fsReaddirSync(path) {
      const result = requestSync('fsReaddir', { path });
      return result.entries ?? [];
    },
    async fsReaddir(path) {
      return this.fsReaddirSync(path);
    },
    fsMkdirSync(path, options = {}) {
      requestSync('fsMkdir', {
        path,
        recursive: options?.recursive === true,
      });
    },
    async fsMkdir(path, options = {}) {
      this.fsMkdirSync(path, options);
    },
    dispose() {
      try {
        closeSync(requestFd);
      } catch {
        // Ignore repeated-close shutdown races.
      }
      try {
        closeSync(responseFd);
      } catch {
        // Ignore repeated-close shutdown races.
      }
    },
  };
}

function accessDenied(subject) {
  const error = new Error(`${subject} is not available in the Agent OS guest Python runtime`);
  error.code = ACCESS_DENIED_CODE;
  return error;
}

const PYTHON_GUEST_IMPORT_BLOCKLIST_SOURCE = String.raw`
import builtins as _agent_os_builtins
import sys as _agent_os_sys
import types as _agent_os_types

def _agent_os_raise_access_denied(module_name):
    raise RuntimeError(f"{module_name} is not available in the Agent OS guest Python runtime")

class _AgentOsBlockedModule(_agent_os_types.ModuleType):
    def __init__(self, name):
        super().__init__(name)
        self.__dict__['__all__'] = ()

    def __getattr__(self, _name):
        _agent_os_raise_access_denied(self.__name__)

    def __dir__(self):
        return []

_agent_os_blocked_modules = {
    _agent_os_module_name: _AgentOsBlockedModule(_agent_os_module_name)
    for _agent_os_module_name in ('js', 'pyodide_js')
}

_agent_os_original_import = _agent_os_builtins.__import__

def _agent_os_import(name, globals=None, locals=None, fromlist=(), level=0):
    if name in _agent_os_blocked_modules:
        return _agent_os_blocked_modules[name]
    return _agent_os_original_import(name, globals, locals, fromlist, level)

_agent_os_builtins.__import__ = _agent_os_import
_agent_os_sys.modules.update(_agent_os_blocked_modules)
`;

function hardenProperty(target, key, value) {
  try {
    Object.defineProperty(target, key, {
      value,
      writable: false,
      configurable: false,
    });
    return;
  } catch {
    // Fall back to assignment below.
  }

  try {
    target[key] = value;
  } catch {
    // Ignore immutable properties.
  }
}

function normalizeBuiltin(specifier) {
  if (typeof specifier !== 'string') {
    return null;
  }

  return specifier.startsWith('node:') ? specifier.slice('node:'.length) : specifier;
}

function installPythonGuestImportBlocklist(pyodide) {
  if (typeof pyodide?.runPython !== 'function') {
    return;
  }

  pyodide.runPython(PYTHON_GUEST_IMPORT_BLOCKLIST_SOURCE);
}

function installPythonGuestHardening() {
  const assetRoot = process.env[ASSET_ROOT_ENV];
  if (assetRoot) {
    register(new URL('./loader.mjs', import.meta.url), import.meta.url);
  }

  hardenProperty(process, 'binding', () => {
    throw accessDenied('process.binding');
  });
  hardenProperty(process, '_linkedBinding', () => {
    throw accessDenied('process._linkedBinding');
  });
  hardenProperty(process, 'dlopen', () => {
    throw accessDenied('process.dlopen');
  });

  if (originalGetBuiltinModule) {
    hardenProperty(process, 'getBuiltinModule', (specifier) => {
      const normalized = normalizeBuiltin(specifier);
      if (normalized && DENIED_BUILTINS.has(normalized)) {
        throw accessDenied(`node:${normalized}`);
      }
      return originalGetBuiltinModule(specifier);
    });
  }

  if (originalRequire) {
    hardenProperty(globalThis, 'require', () => {
      throw accessDenied('require');
    });
  }

  if (originalFetch) {
    const restrictedFetch = (resource, init) => {
      const candidate =
        typeof resource === 'string'
          ? resource
          : resource instanceof URL
            ? resource.href
            : resource?.url;

      let url;
      try {
        url = new URL(String(candidate ?? ''));
      } catch {
        throw accessDenied('network access');
      }

      if (url.protocol !== 'data:') {
        throw accessDenied(`network access to ${url.protocol}`);
      }

      return originalFetch(resource, init);
    };

    hardenProperty(globalThis, 'fetch', restrictedFetch);
  }
}

function installPythonVfsRpcBridge() {
  const bridge = createPythonVfsRpcBridge();
  if (!bridge) {
    return null;
  }

  hardenProperty(globalThis, '__agentOsPythonVfsRpc', bridge);
  return bridge;
}

function installPythonWorkspaceFs(pyodide, bridge) {
  if (!bridge) {
    return;
  }

  const { FS, ERRNO_CODES } = pyodide;
  if (!FS?.mount || !FS?.filesystems?.MEMFS || !ERRNO_CODES) {
    return;
  }

  const MEMFS = FS.filesystems.MEMFS;
  const memfsDirNodeOps = MEMFS.ops_table.dir.node;
  const memfsDirStreamOps = MEMFS.ops_table.dir.stream;
  const memfsFileNodeOps = MEMFS.ops_table.file.node;
  const memfsFileStreamOps = MEMFS.ops_table.file.stream;
  const workspaceDirStreamOps = memfsDirStreamOps;

  function joinGuestPath(parentPath, name) {
    return parentPath === '/' ? `/${name}` : `${parentPath}/${name}`;
  }

  function nodeGuestPath(node) {
    return node.agentOsGuestPath || node.mount?.mountpoint || '/workspace';
  }

  function createFsError(error) {
    if (error instanceof FS.ErrnoError) {
      return error;
    }

    const message = String(error?.message || error);
    let errno = ERRNO_CODES.EIO;
    if (/permission denied|access denied|denied/i.test(message)) {
      errno = ERRNO_CODES.EACCES;
    } else if (/read-only|erofs/i.test(message)) {
      errno = ERRNO_CODES.EROFS;
    } else if (/not a directory|enotdir/i.test(message)) {
      errno = ERRNO_CODES.ENOTDIR;
    } else if (/is a directory|eisdir/i.test(message)) {
      errno = ERRNO_CODES.EISDIR;
    } else if (/exists|already exists|eexist/i.test(message)) {
      errno = ERRNO_CODES.EEXIST;
    } else if (/not found|no such file|enoent/i.test(message)) {
      errno = ERRNO_CODES.ENOENT;
    }

    return new FS.ErrnoError(errno);
  }

  function withFsErrors(operation) {
    try {
      return operation();
    } catch (error) {
      throw createFsError(error);
    }
  }

  function updateNodeFromRemoteStat(node, stat) {
    if (!stat) {
      throw new FS.ErrnoError(ERRNO_CODES.ENOENT);
    }

    node.mode = stat.mode;
    node.timestamp = Date.now();
    if (FS.isFile(stat.mode) && !node.agentOsDirty) {
      node.agentOsRemoteSize = stat.size;
    }
  }

  function createWorkspaceNode(parent, name, mode, dev, guestPath) {
    const node = MEMFS.createNode(parent, name, mode, dev);
    node.agentOsGuestPath = guestPath;
    node.agentOsDirty = false;
    node.agentOsLoaded = FS.isDir(mode);
    node.agentOsRemoteSize = 0;
    if (FS.isDir(mode)) {
      node.node_ops = workspaceDirNodeOps;
      node.stream_ops = workspaceDirStreamOps;
    } else if (FS.isFile(mode)) {
      node.node_ops = workspaceFileNodeOps;
      node.stream_ops = workspaceFileStreamOps;
    }
    return node;
  }

  function syncDirectory(node) {
    const guestPath = nodeGuestPath(node);
    const entries = withFsErrors(() => bridge.fsReaddirSync(guestPath));
    const remoteNames = new Set(entries);

    for (const name of Object.keys(node.contents || {})) {
      if (remoteNames.has(name)) {
        continue;
      }

      const child = node.contents[name];
      if (FS.isDir(child.mode)) {
        memfsDirNodeOps.rmdir(node, name);
      } else {
        memfsDirNodeOps.unlink(node, name);
      }
    }

    for (const name of entries) {
      const childPath = joinGuestPath(guestPath, name);
      const stat = withFsErrors(() => bridge.fsStatSync(childPath));
      const existing = node.contents[name];

      if (existing) {
        const existingIsDir = FS.isDir(existing.mode);
        const remoteIsDir = Boolean(stat?.isDirectory);
        if (existingIsDir !== remoteIsDir) {
          if (existingIsDir) {
            memfsDirNodeOps.rmdir(node, name);
          } else {
            memfsDirNodeOps.unlink(node, name);
          }
        } else {
          existing.agentOsGuestPath = childPath;
          updateNodeFromRemoteStat(existing, stat);
          if (FS.isFile(existing.mode) && !existing.agentOsDirty) {
            existing.agentOsLoaded = false;
          }
          continue;
        }
      }

      const mode = stat?.mode ?? (stat?.isDirectory ? 0o040755 : 0o100644);
      const child = createWorkspaceNode(node, name, mode, 0, childPath);
      updateNodeFromRemoteStat(child, stat);
    }
  }

  function loadFileContents(node) {
    if (node.agentOsDirty) {
      return;
    }

    const stat = withFsErrors(() => bridge.fsStatSync(nodeGuestPath(node)));
    updateNodeFromRemoteStat(node, stat);
    const contentBase64 = withFsErrors(() => bridge.fsReadSync(nodeGuestPath(node)));
    const bytes = Uint8Array.from(Buffer.from(contentBase64, 'base64'));
    node.contents = bytes;
    node.usedBytes = bytes.length;
    node.agentOsLoaded = true;
    node.agentOsRemoteSize = bytes.length;
  }

  function persistFile(node) {
    const contents = node.contents ? MEMFS.getFileDataAsTypedArray(node) : new Uint8Array(0);
    withFsErrors(() => bridge.fsWriteSync(nodeGuestPath(node), contents));
    node.agentOsDirty = false;
    node.agentOsLoaded = true;
    node.agentOsRemoteSize = contents.length;
    node.timestamp = Date.now();
  }

  function makeStat(node, stat) {
    const mode = stat?.mode ?? node.mode;
    const size = FS.isDir(mode) ? 4096 : (node.agentOsDirty ? node.usedBytes : (stat?.size ?? node.usedBytes ?? 0));
    const timestamp = new Date(node.timestamp || Date.now());

    return {
      dev: 1,
      ino: node.id,
      mode,
      nlink: FS.isDir(mode) ? 2 : 1,
      uid: 0,
      gid: 0,
      rdev: 0,
      size,
      atime: timestamp,
      mtime: timestamp,
      ctime: timestamp,
      blksize: 4096,
      blocks: Math.max(1, Math.ceil(size / 4096)),
    };
  }

  const workspaceFileNodeOps = {
    getattr(node) {
      const stat = node.agentOsDirty
        ? null
        : withFsErrors(() => bridge.fsStatSync(nodeGuestPath(node)));
      if (stat) {
        updateNodeFromRemoteStat(node, stat);
      }
      return makeStat(node, stat);
    },
    setattr(node, attr) {
      memfsFileNodeOps.setattr(node, attr);
      if (attr?.size != null) {
        node.agentOsDirty = true;
        node.agentOsLoaded = true;
      }
    },
  };

  const workspaceFileStreamOps = {
    llseek(stream, offset, whence) {
      return memfsFileStreamOps.llseek(stream, offset, whence);
    },
    read(stream, buffer, offset, length, position) {
      if (!stream.node.agentOsLoaded && !stream.node.agentOsDirty) {
        loadFileContents(stream.node);
      }
      return memfsFileStreamOps.read(stream, buffer, offset, length, position);
    },
    write(stream, buffer, offset, length, position, canOwn) {
      if (!stream.node.agentOsLoaded && !stream.node.agentOsDirty) {
        loadFileContents(stream.node);
      }
      const written = memfsFileStreamOps.write(stream, buffer, offset, length, position, canOwn);
      stream.node.agentOsDirty = true;
      persistFile(stream.node);
      return written;
    },
    mmap(stream, length, position, prot, flags) {
      if (!stream.node.agentOsLoaded && !stream.node.agentOsDirty) {
        loadFileContents(stream.node);
      }
      return memfsFileStreamOps.mmap(stream, length, position, prot, flags);
    },
    msync(stream, buffer, offset, length, mmapFlags) {
      const result = memfsFileStreamOps.msync(stream, buffer, offset, length, mmapFlags);
      stream.node.agentOsDirty = true;
      persistFile(stream.node);
      return result;
    },
  };

  const workspaceDirNodeOps = {
    getattr(node) {
      const stat = withFsErrors(() => bridge.fsStatSync(nodeGuestPath(node)));
      updateNodeFromRemoteStat(node, stat);
      return makeStat(node, stat);
    },
    setattr(node, attr) {
      memfsDirNodeOps.setattr(node, attr);
    },
    lookup(parent, name) {
      syncDirectory(parent);
      try {
        return memfsDirNodeOps.lookup(parent, name);
      } catch (error) {
        if (!(error instanceof FS.ErrnoError) || error.errno !== ERRNO_CODES.ENOENT) {
          throw error;
        }

        const guestPath = joinGuestPath(nodeGuestPath(parent), name);
        const stat = withFsErrors(() => bridge.fsStatSync(guestPath));
        const child = createWorkspaceNode(parent, name, stat.mode, 0, guestPath);
        updateNodeFromRemoteStat(child, stat);
        return child;
      }
    },
    mknod(parent, name, mode, dev) {
      const guestPath = joinGuestPath(nodeGuestPath(parent), name);
      const node = createWorkspaceNode(parent, name, mode, dev, guestPath);
      if (FS.isDir(mode)) {
        withFsErrors(() => bridge.fsMkdirSync(guestPath, { recursive: false }));
      } else if (FS.isFile(mode)) {
        node.contents = new Uint8Array(0);
        node.usedBytes = 0;
        node.agentOsDirty = true;
        persistFile(node);
      }
      return node;
    },
    rename() {
      throw new FS.ErrnoError(ERRNO_CODES.ENOSYS);
    },
    unlink() {
      throw new FS.ErrnoError(ERRNO_CODES.ENOSYS);
    },
    rmdir() {
      throw new FS.ErrnoError(ERRNO_CODES.ENOSYS);
    },
    readdir(node) {
      syncDirectory(node);
      return memfsDirNodeOps.readdir(node);
    },
    symlink() {
      throw new FS.ErrnoError(ERRNO_CODES.ENOSYS);
    },
  };

  try {
    FS.mkdir('/workspace');
  } catch (error) {
    if (!(error instanceof FS.ErrnoError) || error.errno !== ERRNO_CODES.EEXIST) {
      throw error;
    }
  }

  FS.mount(
    {
      mount(mount) {
        const root = MEMFS.mount(mount);
        root.agentOsGuestPath = mount.mountpoint;
        root.agentOsDirty = false;
        root.agentOsLoaded = true;
        root.agentOsRemoteSize = 0;
        root.node_ops = workspaceDirNodeOps;
        root.stream_ops = workspaceDirStreamOps;
        return root;
      },
    },
    {},
    '/workspace',
  );
}

async function readLockFileContents(indexURL) {
  const lockFileUrl = new URL('pyodide-lock.json', indexURL);
  return readFile(lockFileUrl, 'utf8');
}

function installPythonStdin(pyodide) {
  if (typeof pyodide?.setStdin !== 'function') {
    return;
  }

  pyodide.setStdin({
    isatty: false,
    read(buffer) {
      return readSync(STDIN_FD, buffer, 0, buffer.length, null);
    },
  });
}

function resolvePythonSource(pyodide) {
  const filePath = process.env[PYTHON_FILE_ENV];
  if (filePath != null) {
    if (typeof pyodide?.FS?.readFile !== 'function') {
      throw new Error(`Pyodide FS.readFile() is required to execute ${filePath}`);
    }

    return pyodide.FS.readFile(filePath, { encoding: 'utf8' });
  }

  return requiredEnv(PYTHON_CODE_ENV);
}

let pythonVfsRpcBridge = null;

try {
  const startupStarted = realPerformance.now();
  const { indexPath, indexUrl } = resolveIndexLocation(requiredEnv(PYODIDE_INDEX_URL_ENV));
  const prewarmOnly = process.env[PYTHON_PREWARM_ONLY_ENV] === '1';
  const preloadPackages = parsePreloadPackages(process.env[PYTHON_PRELOAD_PACKAGES_ENV]);
  const lockFileContents = await readLockFileContents(indexUrl);
  const pyodideModuleUrl = new URL('pyodide.mjs', indexUrl).href;
  const { loadPyodide } = await import(pyodideModuleUrl);

  if (typeof loadPyodide !== 'function') {
    throw new Error(`pyodide.mjs at ${indexUrl} does not export loadPyodide()`);
  }

  const loadPyodideStarted = realPerformance.now();
  const pyodide = await loadPyodide({
    indexURL: indexPath,
    lockFileContents,
    packageBaseUrl: indexPath,
    stdout: (message) => writeStream(process.stdout, message),
    stderr: (message) => writeStream(process.stderr, message),
  });
  const loadPyodideMs = realPerformance.now() - loadPyodideStarted;
  let packageLoadMs = 0;

  if (prewarmOnly) {
    emitPythonStartupMetrics({
      prewarmOnly: true,
      startupMs: realPerformance.now() - startupStarted,
      loadPyodideMs,
      packageLoadMs,
      packageCount: 0,
      source: 'prewarm',
    });
    process.exitCode = 0;
  } else {
  installPythonStdin(pyodide);
  pythonVfsRpcBridge = installPythonVfsRpcBridge();
  installPythonWorkspaceFs(pyodide, pythonVfsRpcBridge);
  installPythonGuestHardening();
  if (preloadPackages.length > 0) {
    const packageLoadStarted = realPerformance.now();
    await pyodide.loadPackage(preloadPackages);
    packageLoadMs = realPerformance.now() - packageLoadStarted;
  }
  installPythonGuestImportBlocklist(pyodide);
  const source = process.env[PYTHON_FILE_ENV] != null ? 'file' : 'inline';
  emitPythonStartupMetrics({
    prewarmOnly: false,
    startupMs: realPerformance.now() - startupStarted,
    loadPyodideMs,
    packageLoadMs,
    packageCount: preloadPackages.length,
    source,
  });
  const code = resolvePythonSource(pyodide);
  await pyodide.runPythonAsync(code);
  }
} catch (error) {
  writeStream(process.stderr, formatError(error));
  process.exitCode = 1;
} finally {
  pythonVfsRpcBridge?.dispose();
  emitControlMessage({ type: 'python_exit', exitCode: process.exitCode ?? 0 });
}
process.exit(process.exitCode ?? 0);
"#;

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
        name: "fs",
        module_specifier: "node:fs",
        init_counter_key: "__agentOsBuiltinFsInitCount",
    },
    BuiltinAsset {
        name: "path",
        module_specifier: "node:path",
        init_counter_key: "__agentOsBuiltinPathInitCount",
    },
    BuiltinAsset {
        name: "url",
        module_specifier: "node:url",
        init_counter_key: "__agentOsBuiltinUrlInitCount",
    },
    BuiltinAsset {
        name: "fs-promises",
        module_specifier: "node:fs/promises",
        init_counter_key: "__agentOsBuiltinFsPromisesInitCount",
    },
    BuiltinAsset {
        name: "child-process",
        module_specifier: "node:child_process",
        init_counter_key: "__agentOsBuiltinChildProcessInitCount",
    },
    BuiltinAsset {
        name: "net",
        module_specifier: "node:net",
        init_counter_key: "__agentOsBuiltinNetInitCount",
    },
    BuiltinAsset {
        name: "dgram",
        module_specifier: "node:dgram",
        init_counter_key: "__agentOsBuiltinDgramInitCount",
    },
    BuiltinAsset {
        name: "dns",
        module_specifier: "node:dns",
        init_counter_key: "__agentOsBuiltinDnsInitCount",
    },
    BuiltinAsset {
        name: "tls",
        module_specifier: "node:tls",
        init_counter_key: "__agentOsBuiltinTlsInitCount",
    },
    BuiltinAsset {
        name: "os",
        module_specifier: "node:os",
        init_counter_key: "__agentOsBuiltinOsInitCount",
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
        name: "diagnostics_channel",
        module_specifier: "node:diagnostics_channel",
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
    DeniedBuiltinAsset {
        name: "v8",
        module_specifier: "node:v8",
    },
    DeniedBuiltinAsset {
        name: "vm",
        module_specifier: "node:vm",
    },
    DeniedBuiltinAsset {
        name: "worker_threads",
        module_specifier: "node:worker_threads",
    },
];

const PATH_POLYFILL_ASSET_NAME: &str = "path";
const PATH_POLYFILL_INIT_COUNTER_KEY: &str = "__agentOsPolyfillPathInitCount";

#[derive(Debug, Clone)]
pub(crate) struct NodeImportCache {
    root_dir: PathBuf,
    cache_path: PathBuf,
    loader_path: PathBuf,
    register_path: PathBuf,
    runner_path: PathBuf,
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
        let cache_id = NEXT_NODE_IMPORT_CACHE_ID.fetch_add(1, Ordering::Relaxed);
        let root_dir = env::temp_dir().join(format!(
            "agent-os-node-import-cache-{}-{cache_id}",
            std::process::id()
        ));

        Self {
            root_dir: root_dir.clone(),
            cache_path: root_dir.join("state.json"),
            loader_path: root_dir.join("loader.mjs"),
            register_path: root_dir.join("register.mjs"),
            runner_path: root_dir.join("runner.mjs"),
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

impl NodeImportCache {
    pub(crate) fn cache_path(&self) -> &Path {
        &self.cache_path
    }

    pub(crate) fn loader_path(&self) -> &Path {
        &self.loader_path
    }

    pub(crate) fn register_path(&self) -> &Path {
        &self.register_path
    }

    pub(crate) fn runner_path(&self) -> &Path {
        &self.runner_path
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn python_runner_path(&self) -> &Path {
        &self.python_runner_path
    }

    pub(crate) fn timing_bootstrap_path(&self) -> &Path {
        &self.timing_bootstrap_path
    }

    pub(crate) fn prewarm_path(&self) -> &Path {
        &self.prewarm_path
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

    pub(crate) fn ensure_materialized(&self) -> Result<(), io::Error> {
        fs::create_dir_all(&self.root_dir)?;
        fs::create_dir_all(self.asset_root.join("builtins"))?;
        fs::create_dir_all(self.asset_root.join("denied"))?;
        fs::create_dir_all(self.asset_root.join("polyfills"))?;
        fs::create_dir_all(&self.pyodide_dist_path)?;
        fs::create_dir_all(&self.prewarm_marker_dir)?;

        write_file_if_changed(&self.loader_path, &render_loader_source())?;
        write_file_if_changed(&self.register_path, &render_register_source())?;
        write_file_if_changed(&self.runner_path, NODE_EXECUTION_RUNNER_SOURCE)?;
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
        write_bytes_if_changed(
            &self.pyodide_dist_path.join("pyodide.mjs"),
            BUNDLED_PYODIDE_MJS,
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
            write_bytes_if_changed(&self.pyodide_dist_path.join(asset.file_name), asset.bytes)?;
        }
        Ok(())
    }
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
            "__AGENT_OS_BUILTIN_SPECIFIER_PREFIX__",
            AGENT_OS_BUILTIN_SPECIFIER_PREFIX,
        )
        .replace(
            "__AGENT_OS_POLYFILL_SPECIFIER_PREFIX__",
            AGENT_OS_POLYFILL_SPECIFIER_PREFIX,
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
        "fs" => render_fs_builtin_asset_source(asset.init_counter_key),
        "fs-promises" => render_fs_promises_builtin_asset_source(asset.init_counter_key),
        "child-process" => render_child_process_builtin_asset_source(asset.init_counter_key),
        "net" => render_net_builtin_asset_source(asset.init_counter_key),
        "dgram" => render_dgram_builtin_asset_source(asset.init_counter_key),
        "dns" => render_dns_builtin_asset_source(asset.init_counter_key),
        "tls" => render_tls_builtin_asset_source(asset.init_counter_key),
        "os" => render_os_builtin_asset_source(asset.init_counter_key),
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
export const __agentOsInitCount = initCount;\n\
export default builtin;\n\
export * from {module_specifier};\n"
    )
}

fn render_fs_builtin_asset_source(init_counter_key: &str) -> String {
    let init_counter_key = format!("{init_counter_key:?}");

    format!(
        "const initCount = (globalThis[{init_counter_key}] ?? 0) + 1;\n\
globalThis[{init_counter_key}] = initCount;\n\
const mod = globalThis.__agentOsBuiltinFs ?? globalThis.__agentOsGuestFs ?? process.getBuiltinModule?.(\"node:fs\");\n\
if (!mod) {{\n\
  throw new Error('Agent OS guest fs polyfill was not initialized');\n\
}}\n\n\
export const __agentOsInitCount = initCount;\n\
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
export const writeSync = mod.writeSync;\n\
export * from \"node:fs\";\n"
    )
}

fn render_fs_promises_builtin_asset_source(init_counter_key: &str) -> String {
    let init_counter_key = format!("{init_counter_key:?}");

    format!(
        "import fsModule from \"agent-os:builtin/fs\";\n\n\
const initCount = (globalThis[{init_counter_key}] ?? 0) + 1;\n\
globalThis[{init_counter_key}] = initCount;\n\
const mod = fsModule.promises;\n\n\
export const __agentOsInitCount = initCount;\n\
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
export const writeFile = mod.writeFile;\n\
export * from \"node:fs/promises\";\n"
    )
}

fn render_child_process_builtin_asset_source(init_counter_key: &str) -> String {
    let init_counter_key = format!("{init_counter_key:?}");

    format!(
        "const ACCESS_DENIED_CODE = \"ERR_ACCESS_DENIED\";\n\
const initCount = (globalThis[{init_counter_key}] ?? 0) + 1;\n\
globalThis[{init_counter_key}] = initCount;\n\
if (!globalThis.__agentOsBuiltinChildProcess) {{\n\
  const error = new Error(\"node:child_process is not available in the Agent OS guest runtime\");\n\
  error.code = ACCESS_DENIED_CODE;\n\
  throw error;\n\
}}\n\n\
const mod = globalThis.__agentOsBuiltinChildProcess;\n\n\
export const __agentOsInitCount = initCount;\n\
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
if (!globalThis.__agentOsBuiltinNet) {{\n\
  const error = new Error(\"node:net is not available in the Agent OS guest runtime\");\n\
  error.code = ACCESS_DENIED_CODE;\n\
  throw error;\n\
}}\n\n\
const mod = globalThis.__agentOsBuiltinNet;\n\n\
export const __agentOsInitCount = initCount;\n\
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
if (!globalThis.__agentOsBuiltinDgram) {{\n\
  const error = new Error(\"node:dgram is not available in the Agent OS guest runtime\");\n\
  error.code = ACCESS_DENIED_CODE;\n\
  throw error;\n\
}}\n\n\
const mod = globalThis.__agentOsBuiltinDgram;\n\n\
export const __agentOsInitCount = initCount;\n\
export default mod;\n\
export const Socket = mod.Socket;\n\
export const createSocket = mod.createSocket;\n"
    )
}

fn render_dns_builtin_asset_source(init_counter_key: &str) -> String {
    let init_counter_key = format!("{init_counter_key:?}");

    format!(
        "const ACCESS_DENIED_CODE = \"ERR_ACCESS_DENIED\";\n\
const initCount = (globalThis[{init_counter_key}] ?? 0) + 1;\n\
globalThis[{init_counter_key}] = initCount;\n\
if (!globalThis.__agentOsBuiltinDns) {{\n\
  const error = new Error(\"node:dns is not available in the Agent OS guest runtime\");\n\
  error.code = ACCESS_DENIED_CODE;\n\
  throw error;\n\
}}\n\n\
const mod = globalThis.__agentOsBuiltinDns;\n\n\
export const __agentOsInitCount = initCount;\n\
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

fn render_tls_builtin_asset_source(init_counter_key: &str) -> String {
    let init_counter_key = format!("{init_counter_key:?}");

    format!(
        "const ACCESS_DENIED_CODE = \"ERR_ACCESS_DENIED\";\n\
const initCount = (globalThis[{init_counter_key}] ?? 0) + 1;\n\
globalThis[{init_counter_key}] = initCount;\n\
if (!globalThis.__agentOsBuiltinTls) {{\n\
  const error = new Error(\"node:tls is not available in the Agent OS guest runtime\");\n\
  error.code = ACCESS_DENIED_CODE;\n\
  throw error;\n\
}}\n\n\
const mod = globalThis.__agentOsBuiltinTls;\n\n\
export const __agentOsInitCount = initCount;\n\
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
if (!globalThis.__agentOsBuiltinOs) {{\n\
  const error = new Error(\"node:os is not available in the Agent OS guest runtime\");\n\
  error.code = ACCESS_DENIED_CODE;\n\
  throw error;\n\
}}\n\n\
const mod = globalThis.__agentOsBuiltinOs;\n\n\
export const __agentOsInitCount = initCount;\n\
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

fn render_denied_asset_source(module_specifier: &str) -> String {
    let message = format!("{module_specifier} is not available in the Agent OS guest runtime");
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
export const __agentOsInitCount = initCount;\n\
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
    use super::NodeImportCache;
    use crate::node_process::node_binary;
    use serde_json::Value;
    use std::collections::BTreeSet;
    use std::fs;
    use std::io::Write;
    use std::path::Path;
    use std::process::{Command, Output, Stdio};
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
            .arg(import_cache.python_runner_path())
            .env("AGENT_OS_PYODIDE_INDEX_URL", pyodide_index_url)
            .env("AGENT_OS_PYTHON_CODE", code);

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
            .arg(import_cache.python_runner_path())
            .env("AGENT_OS_PYODIDE_INDEX_URL", pyodide_index_url)
            .env("AGENT_OS_PYTHON_PREWARM_ONLY", "1");

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
            .arg(import_cache.python_runner_path())
            .env("AGENT_OS_PYODIDE_INDEX_URL", pyodide_index_url)
            .env("AGENT_OS_PYTHON_CODE", code)
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
            &[("AGENT_OS_PYTHON_FILE", "/workspace/script.py")],
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
    fn materialized_python_runner_prewarm_loads_pyodide_without_running_guest_code() {
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

        let output = run_python_runner_prewarm(
            &import_cache,
            pyodide_dir.path(),
            &[("AGENT_OS_PYTHON_CODE", "print('ignored')")],
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
        assert!(stdout.is_empty(), "unexpected stdout: {stdout}");
        assert!(
            stderr.contains("prewarm:"),
            "expected Pyodide load during prewarm: {stderr}"
        );
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
            &[("AGENT_OS_PYTHON_PRELOAD_PACKAGES", "[\"numpy\",\"pandas\"]")],
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let expected_package_base = format!(
            "base:{}{}",
            pyodide_dir.path().display(),
            std::path::MAIN_SEPARATOR
        );

        assert_eq!(output.status.code(), Some(0));
        assert_eq!(stdout, "packages:numpy,pandas\ncode:print('hello')\n");
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
            &[("AGENT_OS_PYTHON_PRELOAD_PACKAGES", "[\"requests\"]")],
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
            String::from("diagnostics_channel"),
            String::from("http"),
            String::from("http2"),
            String::from("https"),
            String::from("inspector"),
            String::from("module"),
            String::from("net"),
            String::from("trace_events"),
            String::from("v8"),
            String::from("vm"),
            String::from("worker_threads"),
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
    fn ensure_materialized_writes_os_builtin_asset() {
        let import_cache = NodeImportCache::default();
        import_cache
            .ensure_materialized()
            .expect("materialize node import cache");

        let os_asset =
            fs::read_to_string(import_cache.asset_root().join("builtins").join("os.mjs"))
                .expect("read os builtin asset");

        assert!(os_asset.contains("__agentOsBuiltinOs"));
        assert!(os_asset.contains("export const hostname = mod.hostname"));
        assert!(os_asset.contains("export const userInfo = mod.userInfo"));
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

        assert!(net_asset.contains("__agentOsBuiltinNet"));
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

        assert!(dgram_asset.contains("__agentOsBuiltinDgram"));
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

        assert!(dns_asset.contains("__agentOsBuiltinDns"));
        assert!(dns_asset.contains("export const lookup = mod.lookup"));
        assert!(dns_asset.contains("export const resolve4 = mod.resolve4"));
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

        assert!(tls_asset.contains("__agentOsBuiltinTls"));
        assert!(tls_asset.contains("export const connect = mod.connect"));
        assert!(tls_asset.contains("export const createServer = mod.createServer"));
    }
}
