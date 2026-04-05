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
const NODE_IMPORT_CACHE_LOADER_VERSION: &str = "5";
const NODE_IMPORT_CACHE_ASSET_VERSION: &str = "2";
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
const PROJECTED_SOURCE_CACHE_ROOT = CACHE_PATH
  ? path.join(path.dirname(CACHE_PATH), 'projected-sources')
  : null;
const ASSET_ROOT = process.env.__NODE_IMPORT_CACHE_ASSET_ROOT_ENV__;
const DEBUG_ENABLED = process.env.__NODE_IMPORT_CACHE_DEBUG_ENV__ === '1';
const METRICS_PREFIX = '__NODE_IMPORT_CACHE_METRICS_PREFIX__';
const SCHEMA_VERSION = '__NODE_IMPORT_CACHE_SCHEMA_VERSION__';
const LOADER_VERSION = '__NODE_IMPORT_CACHE_LOADER_VERSION__';
const ASSET_VERSION = '__NODE_IMPORT_CACHE_ASSET_VERSION__';
const BUILTIN_PREFIX = '__AGENT_OS_BUILTIN_SPECIFIER_PREFIX__';
const POLYFILL_PREFIX = '__AGENT_OS_POLYFILL_SPECIFIER_PREFIX__';
const FS_ASSET_SPECIFIER = `${BUILTIN_PREFIX}fs`;
const FS_PROMISES_ASSET_SPECIFIER = `${BUILTIN_PREFIX}fs-promises`;
const CHILD_PROCESS_ASSET_SPECIFIER = `${BUILTIN_PREFIX}child-process`;
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
  'os',
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
  const resolved = await nextResolve(specifier, translatedContext);
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
  const filePath = filePathFromUrl(url);
  const format = lookupModuleFormat(url) ?? context.format;

  if (!filePath || !format || format === 'builtin') {
    return nextLoad(url, context);
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

  try {
    process.stderr.write(`${METRICS_PREFIX}${JSON.stringify(payload)}\n`);
  } catch {
    // Ignore stderr write failures during teardown.
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
import fs from 'node:fs';
import Module, { syncBuiltinESMExports } from 'node:module';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const GUEST_PATH_MAPPINGS = parseGuestPathMappings(process.env.AGENT_OS_GUEST_PATH_MAPPINGS);
const ALLOWED_BUILTINS = new Set(parseJsonArray(process.env.AGENT_OS_ALLOWED_NODE_BUILTINS));
const LOOPBACK_EXEMPT_PORTS = new Set(parseJsonArray(process.env.AGENT_OS_LOOPBACK_EXEMPT_PORTS));
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
  'os',
  'tls',
  'trace_events',
  'v8',
  'vm',
  'worker_threads',
].filter((name) => !ALLOWED_BUILTINS.has(name)));
const originalModuleLoad =
  typeof Module._load === 'function' ? Module._load.bind(Module) : null;
const originalFetch =
  typeof globalThis.fetch === 'function'
    ? globalThis.fetch.bind(globalThis)
    : null;
const hostRequire = Module.createRequire(import.meta.url);
const guestEntryPoint = process.env.AGENT_OS_GUEST_ENTRYPOINT ?? process.env.AGENT_OS_ENTRYPOINT;

function isPathLike(specifier) {
  return specifier.startsWith('.') || specifier.startsWith('/') || specifier.startsWith('file:');
}

function toImportSpecifier(specifier) {
  if (specifier.startsWith('file:')) {
    return specifier;
  }
  if (isPathLike(specifier)) {
    if (specifier.startsWith('/')) {
      return pathToFileURL(
        pathExists(specifier) ? path.resolve(specifier) : path.posix.normalize(specifier),
      ).href;
    }
    return pathToFileURL(path.resolve(process.cwd(), specifier)).href;
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

  const wrapped = {
    ...fsModule,
    accessSync: wrapPathFirst('accessSync'),
    appendFileSync: wrapPathFirst('appendFileSync'),
    chmodSync: wrapPathFirst('chmodSync'),
    chownSync: wrapPathFirst('chownSync'),
    createReadStream: wrapPathFirst('createReadStream'),
    createWriteStream: wrapPathFirst('createWriteStream'),
    existsSync: (target) => {
      const translated = translateGuestPath(target, fromGuestDir);
      return existsSync(translated) || guestMappedChildNames(target).length > 0;
    },
    lstatSync: wrapPathFirst('lstatSync'),
    mkdirSync: wrapPathFirst('mkdirSync'),
    openSync: wrapPathFirst('openSync'),
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
    utimesSync: wrapPathFirst('utimesSync'),
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
  }

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

function wrapChildProcessModule(childProcessModule, fromGuestDir = '/') {
  const isNodeCommand = (command) =>
    command === 'node' || String(command).endsWith('/node');
  const isNodeScriptCommand = (command) =>
    typeof command === 'string' &&
    (command.startsWith('./') ||
      command.startsWith('../') ||
      command.startsWith('/') ||
      command.startsWith('file:')) &&
    /\.(?:[cm]?js)$/i.test(command);
  const usesNodeRuntime = (command) =>
    isNodeCommand(command) || isNodeScriptCommand(command);
  const translateCommand = (command) =>
    usesNodeRuntime(command)
      ? process.execPath
      : translateGuestPath(command, fromGuestDir);
  const isGuestCommandPath = (command) =>
    typeof command === 'string' &&
    (command.startsWith('/') || command.startsWith('file:'));
  const ensureRuntimeEnv = (env) => {
    const sourceEnv =
      env && typeof env === 'object' ? env : process.env;
    const { NODE_OPTIONS: _nodeOptions, ...safeEnv } = sourceEnv;
    for (const key of ['HOME', 'PWD', 'TMPDIR', 'TEMP', 'TMP', 'PI_CODING_AGENT_DIR']) {
      if (typeof safeEnv[key] === 'string') {
        safeEnv[key] = translateGuestPath(safeEnv[key], fromGuestDir);
      }
    }
    const nodeDir = path.dirname(process.execPath);
    const existingPath =
      typeof safeEnv.PATH === 'string'
        ? safeEnv.PATH
        : typeof process.env.PATH === 'string'
          ? process.env.PATH
          : '';
    const segments = existingPath
      .split(path.delimiter)
      .filter(Boolean);

    if (!segments.includes(nodeDir)) {
      segments.unshift(nodeDir);
    }

    return {
      ...safeEnv,
      PATH: segments.join(path.delimiter),
    };
  };
  const translateProcessOptions = (options) => {
    if (options == null) {
      return {
        env: ensureRuntimeEnv(process.env),
      };
    }

    if (typeof options !== 'object') {
      return options;
    }

    return {
      ...options,
      cwd:
        typeof options.cwd === 'string'
          ? translateGuestPath(options.cwd, fromGuestDir)
          : options.cwd,
      env: ensureRuntimeEnv(options.env),
    };
  };
  const translateArgs = (command, args) => {
    if (isNodeScriptCommand(command)) {
      const translatedScript = translateGuestPath(command, fromGuestDir);
      const translatedArgs = Array.isArray(args)
        ? args.map((arg) => translateGuestPath(arg, fromGuestDir))
        : [];
      return [translatedScript, ...translatedArgs];
    }

    if (!Array.isArray(args)) {
      return args;
    }
    if (!isNodeCommand(command)) {
      return args.map((arg) => translateGuestPath(arg, fromGuestDir));
    }
    return args.map((arg, index) =>
      index === 0 ? translateGuestPath(arg, fromGuestDir) : arg,
    );
  };
  const prependNodePermissionArgs = (command, args, options) => {
    if (!usesNodeRuntime(command)) {
      return args;
    }

    const translatedArgs = Array.isArray(args) ? args : [];
    const readPaths = new Set();
    const writePaths = new Set();
    const addReadPathChain = (value) => {
      if (typeof value !== 'string' || value.length === 0) {
        return;
      }
      let current = value;
      while (true) {
        readPaths.add(current);
        const parent = path.dirname(current);
        if (parent === current) {
          break;
        }
        current = parent;
      }
    };
    const addWritePath = (value) => {
      if (typeof value !== 'string' || value.length === 0) {
        return;
      }
      writePaths.add(value);
    };

    if (typeof options?.cwd === 'string') {
      addReadPathChain(options.cwd);
      addWritePath(options.cwd);
    }

    const homePath =
      typeof options?.env?.HOME === 'string'
        ? translateGuestPath(options.env.HOME, fromGuestDir)
        : typeof process.env.HOME === 'string'
          ? translateGuestPath(process.env.HOME, fromGuestDir)
          : null;
    if (homePath) {
      addReadPathChain(homePath);
      addWritePath(homePath);
    }

    if (translatedArgs.length > 0 && typeof translatedArgs[0] === 'string') {
      addReadPathChain(translatedArgs[0]);
    }

    const permissionArgs = [
      '--allow-child-process',
      '--allow-worker',
      '--disable-warning=SecurityWarning',
    ];

    for (const allowedPath of readPaths) {
      permissionArgs.push(`--allow-fs-read=${allowedPath}`);
    }
    for (const allowedPath of writePaths) {
      permissionArgs.push(`--allow-fs-write=${allowedPath}`);
    }

    return [...permissionArgs, ...translatedArgs];
  };

  return {
    ...childProcessModule,
    exec: childProcessModule.exec.bind(childProcessModule),
    execFile: (file, args, options, callback) => {
      const translatedOptions = translateProcessOptions(options);
      return childProcessModule.execFile(
        translateCommand(file),
        prependNodePermissionArgs(
          file,
          translateArgs(file, args),
          translatedOptions,
        ),
        translatedOptions,
        callback,
      );
    },
    execFileSync: (file, args, options) => {
      const translatedOptions = translateProcessOptions(options);
      return childProcessModule.execFileSync(
        translateCommand(file),
        prependNodePermissionArgs(
          file,
          translateArgs(file, args),
          translatedOptions,
        ),
        translatedOptions,
      );
    },
    execSync: childProcessModule.execSync.bind(childProcessModule),
    fork: (modulePath, args, options) => {
      const translatedOptions = translateProcessOptions(options);
      return childProcessModule.fork(
        translateGuestPath(modulePath, fromGuestDir),
        prependNodePermissionArgs(
          'node',
          translateArgs('node', args),
          translatedOptions,
        ),
        translatedOptions,
      );
    },
    spawn: (command, args, options) => {
      const translatedOptions = translateProcessOptions(options);
      return childProcessModule.spawn(
        translateCommand(command),
        prependNodePermissionArgs(
          command,
          translateArgs(command, args),
          translatedOptions,
        ),
        translatedOptions,
      );
    },
    spawnSync: (command, args, options) =>
      {
        const translatedOptions = translateProcessOptions(options);
        const result = childProcessModule.spawnSync(
          translateCommand(command),
          prependNodePermissionArgs(
            command,
            translateArgs(command, args),
            translatedOptions,
          ),
          translatedOptions,
        );
        if (
          isGuestCommandPath(command) &&
          result?.status == null &&
          (result.error?.code === 'ENOENT' || result.error?.code === 'EACCES')
        ) {
          return {
            ...result,
            status: 1,
            stderr: Buffer.from(result.error.message),
          };
        }
        return result;
      },
  };
}

const guestRequireCache = new Map();
let rootGuestRequire = null;
const hostFs = fs;
const hostFsPromises = fs.promises;
const hostChildProcess = hostRequire('child_process');
const guestFs = wrapFsModule(hostFs);
const guestChildProcess = wrapChildProcessModule(hostChildProcess);

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

function createGuestRequire(fromGuestDir) {
  const normalizedGuestDir = path.posix.normalize(fromGuestDir || '/');
  const cached = guestRequireCache.get(normalizedGuestDir);
  if (cached) {
    return cached;
  }

  const hostDir = hostPathFromGuestPath(normalizedGuestDir) ?? process.cwd();
  const baseRequire = Module.createRequire(
    pathToFileURL(path.join(hostDir, '__agent_os_require__.cjs')),
  );

  const guestRequire = function(specifier) {
    const translated = hostPathForSpecifier(specifier, normalizedGuestDir);
    if (translated) {
      return baseRequire(translated);
    }

    try {
      return baseRequire(specifier);
    } catch (error) {
      if (rootGuestRequire && rootGuestRequire !== guestRequire && isBareSpecifier(specifier)) {
        return rootGuestRequire(specifier);
      }
      throw error;
    }
  };

  guestRequire.resolve = (specifier) => {
    const translated = hostPathForSpecifier(specifier, normalizedGuestDir);
    if (translated) {
      return baseRequire.resolve(translated);
    }

    try {
      return baseRequire.resolve(specifier);
    } catch (error) {
      if (rootGuestRequire && rootGuestRequire !== guestRequire && isBareSpecifier(specifier)) {
        return rootGuestRequire.resolve(specifier);
      }
      throw error;
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

function installGuestHardening() {
  syncBuiltinModuleExports(hostFs, guestFs);
  syncBuiltinModuleExports(hostFsPromises, guestFs.promises);
  try {
    syncBuiltinESMExports();
  } catch {
    // Ignore runtimes that reject syncing builtin ESM exports.
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

  if (originalModuleLoad) {
    Module._load = function(request, parent, isMain) {
      const normalized =
        typeof request === 'string' ? normalizeBuiltin(request) : null;
      if (normalized === 'fs') {
        return cloneFsModule(guestFs);
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

const entrypoint = process.env.AGENT_OS_ENTRYPOINT;
if (!entrypoint) {
  throw new Error('AGENT_OS_ENTRYPOINT is required');
}

installGuestHardening();
rootGuestRequire = createGuestRequire('/root/node_modules');
if (ALLOWED_BUILTINS.has('child_process')) {
  hardenProperty(globalThis, '__agentOsBuiltinChildProcess', guestChildProcess);
}
hardenProperty(globalThis, '__agentOsBuiltinFs', guestFs);
hardenProperty(globalThis, '_requireFrom', (specifier, fromDir = '/') =>
  createGuestRequire(fromDir)(specifier),
);
hardenProperty(
  globalThis,
  'require',
  createGuestRequire(path.posix.dirname(guestEntryPoint ?? entrypoint)),
);

if (process.env.AGENT_OS_KEEP_STDIN_OPEN === '1') {
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

const guestArgv = JSON.parse(process.env.AGENT_OS_GUEST_ARGV ?? '[]');
const bootstrapModule = process.env.AGENT_OS_BOOTSTRAP_MODULE;
const entrypointPath = isPathLike(entrypoint)
  ? path.resolve(process.cwd(), entrypoint)
  : entrypoint;

process.argv = [process.execPath, guestEntryPoint ?? entrypointPath, ...guestArgv];

if (bootstrapModule) {
  await import(toImportSpecifier(bootstrapModule));
}

await import(toImportSpecifier(entrypoint));
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
const SIGNAL_STATE_CONTROL_PREFIX = '__AGENT_OS_SIGNAL_STATE__:';

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

const hostProcessImport = {
  proc_sigaction(signal, action, maskLo, maskHi, flags) {
    try {
      const registration = {
        action: action === 0 ? 'default' : action === 1 ? 'ignore' : 'user',
        mask: decodeSignalMask(maskLo, maskHi),
        flags: Number(flags) >>> 0,
      };
      process.stderr.write(
        `${SIGNAL_STATE_CONTROL_PREFIX}${JSON.stringify({
          signal: Number(signal) >>> 0,
          registration,
        })}\n`,
      );
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
const PYTHON_EXIT_CONTROL_PREFIX = '__AGENT_OS_PYTHON_EXIT__:';
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
  'os',
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

function requiredEnv(name) {
  const value = process.env[name];
  if (value == null) {
    throw new Error(`${name} is required`);
  }
  return value;
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
  writeStream(process.stderr, `${PYTHON_EXIT_CONTROL_PREFIX}${process.exitCode ?? 0}`);
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
        name: "dns",
        module_specifier: "node:dns",
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
        name: "os",
        module_specifier: "node:os",
    },
    DeniedBuiltinAsset {
        name: "tls",
        module_specifier: "node:tls",
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
        "import fs from \"node:fs\";\n\
import path from \"node:path\";\n\n\
const GUEST_PATH_MAPPINGS = parseGuestPathMappings(process.env.AGENT_OS_GUEST_PATH_MAPPINGS);\n\
const initCount = (globalThis[{init_counter_key}] ?? 0) + 1;\n\
globalThis[{init_counter_key}] = initCount;\n\
const mod = wrapFsModule(fs);\n\n\
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
export * from \"node:fs\";\n\n\
function parseGuestPathMappings(value) {{\n\
  if (!value) {{\n\
    return [];\n\
  }}\n\n\
  try {{\n\
    const parsed = JSON.parse(value);\n\
    if (!Array.isArray(parsed)) {{\n\
      return [];\n\
    }}\n\n\
    return parsed\n\
      .map((entry) => {{\n\
        const guestPath =\n\
          entry && typeof entry.guestPath === \"string\"\n\
            ? path.posix.normalize(entry.guestPath)\n\
            : null;\n\
        const hostPath =\n\
          entry && typeof entry.hostPath === \"string\"\n\
            ? path.resolve(entry.hostPath)\n\
            : null;\n\
        return guestPath && hostPath ? {{ guestPath, hostPath }} : null;\n\
      }})\n\
      .filter(Boolean)\n\
      .sort((left, right) => right.guestPath.length - left.guestPath.length);\n\
  }} catch {{\n\
    return [];\n\
  }}\n\
}}\n\n\
function hostPathFromGuestPath(guestPath) {{\n\
  if (typeof guestPath !== \"string\") {{\n\
    return null;\n\
  }}\n\n\
  const normalized = path.posix.normalize(guestPath);\n\
  for (const mapping of GUEST_PATH_MAPPINGS) {{\n\
    if (mapping.guestPath === \"/\") {{\n\
      const suffix = normalized.replace(/^\\/+/, \"\");\n\
      return suffix ? path.join(mapping.hostPath, suffix) : mapping.hostPath;\n\
    }}\n\n\
    if (\n\
      normalized !== mapping.guestPath &&\n\
      !normalized.startsWith(`${{mapping.guestPath}}/`)\n\
    ) {{\n\
      continue;\n\
    }}\n\n\
    const suffix =\n\
      normalized === mapping.guestPath\n\
        ? \"\"\n\
        : normalized.slice(mapping.guestPath.length + 1);\n\
    return suffix ? path.join(mapping.hostPath, suffix) : mapping.hostPath;\n\
  }}\n\n\
  return null;\n\
}}\n\n\
function safeRealpath(targetPath) {{\n\
  try {{\n\
    return fs.realpathSync.native(targetPath);\n\
  }} catch {{\n\
    return null;\n\
  }}\n\
}}\n\n\
function isKnownHostPath(hostPath) {{\n\
  if (typeof hostPath !== \"string\") {{\n\
    return false;\n\
  }}\n\n\
  const normalized = path.resolve(hostPath);\n\
  const hasPrefix = (hostRoot) =>\n\
    !!hostRoot &&\n\
    (normalized === hostRoot || normalized.startsWith(`${{hostRoot}}${{path.sep}}`));\n\
  for (const mapping of GUEST_PATH_MAPPINGS) {{\n\
    for (const hostRoot of [path.resolve(mapping.hostPath), safeRealpath(mapping.hostPath)]) {{\n\
      if (hasPrefix(hostRoot)) {{\n\
        return true;\n\
      }}\n\
    }}\n\n\
    let current = path.dirname(mapping.hostPath);\n\
    while (true) {{\n\
      const candidate = path.join(current, \"node_modules\");\n\
      if (pathExists(candidate)) {{\n\
        for (const hostRoot of [path.resolve(candidate), safeRealpath(candidate)]) {{\n\
          if (hasPrefix(hostRoot)) {{\n\
            return true;\n\
          }}\n\
        }}\n\
      }}\n\n\
      const parent = path.dirname(current);\n\
      if (parent === current) {{\n\
        break;\n\
      }}\n\
      current = parent;\n\
    }}\n\n\
  }}\n\n\
  return false;\n\
}}\n\n\
function pathExists(targetPath) {{\n\
  try {{\n\
    return fs.existsSync(targetPath);\n\
  }} catch {{\n\
    return false;\n\
  }}\n\
}}\n\n\
function translateGuestPath(value, fromGuestDir = \"/\") {{\n\
  if (typeof value !== \"string\") {{\n\
    return value;\n\
  }}\n\n\
  if (value.startsWith(\"file:\")) {{\n\
    try {{\n\
      const pathname = new URL(value).pathname;\n\
      if (pathExists(pathname) && isKnownHostPath(pathname)) {{\n\
        return value;\n\
      }}\n\
      const hostPath = hostPathFromGuestPath(pathname);\n\
      return hostPath ?? value;\n\
    }} catch {{\n\
      return value;\n\
    }}\n\
  }}\n\n\
  if (value.startsWith(\"/\")) {{\n\
    if (pathExists(value) && isKnownHostPath(value)) {{\n\
      return value;\n\
    }}\n\
    return hostPathFromGuestPath(value) ?? value;\n\
  }}\n\n\
  if (value.startsWith(\"./\") || value.startsWith(\"../\")) {{\n\
    const guestPath = path.posix.normalize(path.posix.join(fromGuestDir, value));\n\
    return hostPathFromGuestPath(guestPath) ?? value;\n\
  }}\n\n\
  return value;\n\
}}\n\n\
function guestMappedChildNames(guestDir) {{\n\
  if (typeof guestDir !== \"string\") {{\n\
    return [];\n\
  }}\n\n\
  const normalized = path.posix.normalize(guestDir);\n\
  const prefix = normalized === \"/\" ? \"/\" : `${{normalized}}/`;\n\
  const children = new Set();\n\n\
  for (const mapping of GUEST_PATH_MAPPINGS) {{\n\
    if (!mapping.guestPath.startsWith(prefix)) {{\n\
      continue;\n\
    }}\n\
    const remainder = mapping.guestPath.slice(prefix.length);\n\
    const childName = remainder.split(\"/\")[0];\n\
    if (childName) {{\n\
      children.add(childName);\n\
    }}\n\
  }}\n\n\
  return [...children].sort();\n\
}}\n\n\
function createSyntheticDirent(name) {{\n\
  return {{\n\
    name,\n\
    isBlockDevice: () => false,\n\
    isCharacterDevice: () => false,\n\
    isDirectory: () => true,\n\
    isFIFO: () => false,\n\
    isFile: () => false,\n\
    isSocket: () => false,\n\
    isSymbolicLink: () => false,\n\
  }};\n\
}}\n\n\
function wrapFsModule(fsModule, fromGuestDir = \"/\") {{\n\
  const wrapPathFirst = (methodName) => (...args) =>\n\
    fsModule[methodName](translateGuestPath(args[0], fromGuestDir), ...args.slice(1));\n\
  const wrapRenameLike = (methodName) => (...args) =>\n\
    fsModule[methodName](\n\
      translateGuestPath(args[0], fromGuestDir),\n\
      translateGuestPath(args[1], fromGuestDir),\n\
      ...args.slice(2),\n\
    );\n\n\
  const wrapped = {{\n\
    ...fsModule,\n\
    accessSync: wrapPathFirst(\"accessSync\"),\n\
    appendFileSync: wrapPathFirst(\"appendFileSync\"),\n\
    chmodSync: wrapPathFirst(\"chmodSync\"),\n\
    chownSync: wrapPathFirst(\"chownSync\"),\n\
    createReadStream: wrapPathFirst(\"createReadStream\"),\n\
    createWriteStream: wrapPathFirst(\"createWriteStream\"),\n\
    existsSync: (target) => {{\n\
      const translated = translateGuestPath(target, fromGuestDir);\n\
      return fsModule.existsSync(translated) || guestMappedChildNames(target).length > 0;\n\
    }},\n\
    lstatSync: wrapPathFirst(\"lstatSync\"),\n\
    mkdirSync: wrapPathFirst(\"mkdirSync\"),\n\
    openSync: wrapPathFirst(\"openSync\"),\n\
    readFileSync: wrapPathFirst(\"readFileSync\"),\n\
    readdirSync: (target, options) => {{\n\
      const translated = translateGuestPath(target, fromGuestDir);\n\
      if (fsModule.existsSync(translated)) {{\n\
        return fsModule.readdirSync(translated, options);\n\
      }}\n\n\
      const synthetic = guestMappedChildNames(target);\n\
      if (synthetic.length > 0) {{\n\
        return options && typeof options === \"object\" && options.withFileTypes\n\
          ? synthetic.map((name) => createSyntheticDirent(name))\n\
          : synthetic;\n\
      }}\n\n\
      return fsModule.readdirSync(translated, options);\n\
    }},\n\
    readlinkSync: wrapPathFirst(\"readlinkSync\"),\n\
    realpathSync: wrapPathFirst(\"realpathSync\"),\n\
    renameSync: wrapRenameLike(\"renameSync\"),\n\
    rmSync: wrapPathFirst(\"rmSync\"),\n\
    rmdirSync: wrapPathFirst(\"rmdirSync\"),\n\
    statSync: wrapPathFirst(\"statSync\"),\n\
    symlinkSync: wrapRenameLike(\"symlinkSync\"),\n\
    unlinkSync: wrapPathFirst(\"unlinkSync\"),\n\
    utimesSync: wrapPathFirst(\"utimesSync\"),\n\
    writeFileSync: wrapPathFirst(\"writeFileSync\"),\n\
  }};\n\n\
  if (fsModule.promises) {{\n\
    wrapped.promises = {{\n\
      ...fsModule.promises,\n\
      access: wrapPathFirstAsync(fsModule.promises.access, fromGuestDir),\n\
      appendFile: wrapPathFirstAsync(fsModule.promises.appendFile, fromGuestDir),\n\
      chmod: wrapPathFirstAsync(fsModule.promises.chmod, fromGuestDir),\n\
      chown: wrapPathFirstAsync(fsModule.promises.chown, fromGuestDir),\n\
      lstat: wrapPathFirstAsync(fsModule.promises.lstat, fromGuestDir),\n\
      mkdir: wrapPathFirstAsync(fsModule.promises.mkdir, fromGuestDir),\n\
      open: wrapPathFirstAsync(fsModule.promises.open, fromGuestDir),\n\
      readFile: wrapPathFirstAsync(fsModule.promises.readFile, fromGuestDir),\n\
      readdir: wrapPathFirstAsync(fsModule.promises.readdir, fromGuestDir),\n\
      readlink: wrapPathFirstAsync(fsModule.promises.readlink, fromGuestDir),\n\
      realpath: wrapPathFirstAsync(fsModule.promises.realpath, fromGuestDir),\n\
      rename: wrapRenameLikeAsync(fsModule.promises.rename, fromGuestDir),\n\
      rm: wrapPathFirstAsync(fsModule.promises.rm, fromGuestDir),\n\
      rmdir: wrapPathFirstAsync(fsModule.promises.rmdir, fromGuestDir),\n\
      stat: wrapPathFirstAsync(fsModule.promises.stat, fromGuestDir),\n\
      symlink: wrapRenameLikeAsync(fsModule.promises.symlink, fromGuestDir),\n\
      unlink: wrapPathFirstAsync(fsModule.promises.unlink, fromGuestDir),\n\
      utimes: wrapPathFirstAsync(fsModule.promises.utimes, fromGuestDir),\n\
      writeFile: wrapPathFirstAsync(fsModule.promises.writeFile, fromGuestDir),\n\
    }};\n\
  }}\n\n\
  return wrapped;\n\
}}\n\n\
function wrapPathFirstAsync(fn, fromGuestDir) {{\n\
  return (...args) =>\n\
    fn(translateGuestPath(args[0], fromGuestDir), ...args.slice(1));\n\
}}\n\n\
function wrapRenameLikeAsync(fn, fromGuestDir) {{\n\
  return (...args) =>\n\
    fn(\n\
      translateGuestPath(args[0], fromGuestDir),\n\
      translateGuestPath(args[1], fromGuestDir),\n\
      ...args.slice(2),\n\
    );\n\
}}\n"
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
        "import childProcess from \"node:child_process\";\n\
import path from \"node:path\";\n\n\
const GUEST_PATH_MAPPINGS = parseGuestPathMappings(process.env.AGENT_OS_GUEST_PATH_MAPPINGS);\n\
const ALLOWED_BUILTINS = new Set(parseJsonArray(process.env.AGENT_OS_ALLOWED_NODE_BUILTINS));\n\
const initCount = (globalThis[{init_counter_key}] ?? 0) + 1;\n\
globalThis[{init_counter_key}] = initCount;\n\
if (!ALLOWED_BUILTINS.has(\"child_process\")) {{\n\
  const error = new Error(\"node:child_process is not available in the Agent OS guest runtime\");\n\
  error.code = \"ERR_ACCESS_DENIED\";\n\
  throw error;\n\
}}\n\n\
const mod = wrapChildProcessModule(childProcess);\n\n\
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
export const spawnSync = mod.spawnSync;\n\n\
function parseJsonArray(value) {{\n\
  if (!value) {{\n\
    return [];\n\
  }}\n\n\
  try {{\n\
    const parsed = JSON.parse(value);\n\
    return Array.isArray(parsed) ? parsed.filter((entry) => typeof entry === \"string\") : [];\n\
  }} catch {{\n\
    return [];\n\
  }}\n\
}}\n\n\
function parseGuestPathMappings(value) {{\n\
  if (!value) {{\n\
    return [];\n\
  }}\n\n\
  try {{\n\
    const parsed = JSON.parse(value);\n\
    if (!Array.isArray(parsed)) {{\n\
      return [];\n\
    }}\n\n\
    return parsed\n\
      .map((entry) => {{\n\
        const guestPath =\n\
          entry && typeof entry.guestPath === \"string\"\n\
            ? path.posix.normalize(entry.guestPath)\n\
            : null;\n\
        const hostPath =\n\
          entry && typeof entry.hostPath === \"string\"\n\
            ? path.resolve(entry.hostPath)\n\
            : null;\n\
        return guestPath && hostPath ? {{ guestPath, hostPath }} : null;\n\
      }})\n\
      .filter(Boolean)\n\
      .sort((left, right) => right.guestPath.length - left.guestPath.length);\n\
  }} catch {{\n\
    return [];\n\
  }}\n\
}}\n\n\
function hostPathFromGuestPath(guestPath) {{\n\
  if (typeof guestPath !== \"string\") {{\n\
    return null;\n\
  }}\n\n\
  const normalized = path.posix.normalize(guestPath);\n\
  for (const mapping of GUEST_PATH_MAPPINGS) {{\n\
    if (mapping.guestPath === \"/\") {{\n\
      const suffix = normalized.replace(/^\\/+/, \"\");\n\
      return suffix ? path.join(mapping.hostPath, suffix) : mapping.hostPath;\n\
    }}\n\n\
    if (\n\
      normalized !== mapping.guestPath &&\n\
      !normalized.startsWith(`${{mapping.guestPath}}/`)\n\
    ) {{\n\
      continue;\n\
    }}\n\n\
    const suffix =\n\
      normalized === mapping.guestPath\n\
        ? \"\"\n\
        : normalized.slice(mapping.guestPath.length + 1);\n\
    return suffix ? path.join(mapping.hostPath, suffix) : mapping.hostPath;\n\
  }}\n\n\
  return null;\n\
}}\n\n\
function translateGuestPath(value, fromGuestDir = \"/\") {{\n\
  if (typeof value !== \"string\") {{\n\
    return value;\n\
  }}\n\n\
  if (value.startsWith(\"file:\")) {{\n\
    try {{\n\
      const hostPath = hostPathFromGuestPath(new URL(value).pathname);\n\
      return hostPath ?? value;\n\
    }} catch {{\n\
      return value;\n\
    }}\n\
  }}\n\n\
  if (value.startsWith(\"/\")) {{\n\
    return hostPathFromGuestPath(value) ?? value;\n\
  }}\n\n\
  if (value.startsWith(\"./\") || value.startsWith(\"../\")) {{\n\
    const guestPath = path.posix.normalize(path.posix.join(fromGuestDir, value));\n\
    return hostPathFromGuestPath(guestPath) ?? value;\n\
  }}\n\n\
  return value;\n\
}}\n\n\
function wrapChildProcessModule(childProcessModule, fromGuestDir = \"/\") {{\n\
  const isNodeCommand = (command) =>\n\
    command === \"node\" || String(command).endsWith(\"/node\");\n\
  const isNodeScriptCommand = (command) =>\n\
    typeof command === \"string\" &&\n\
    (command.startsWith(\"./\") ||\n\
      command.startsWith(\"../\") ||\n\
      command.startsWith(\"/\") ||\n\
      command.startsWith(\"file:\")) &&\n\
    /\\.(?:[cm]?js)$/i.test(command);\n\
  const usesNodeRuntime = (command) =>\n\
    isNodeCommand(command) || isNodeScriptCommand(command);\n\
  const translateCommand = (command) =>\n\
    usesNodeRuntime(command)\n\
      ? process.execPath\n\
      : translateGuestPath(command, fromGuestDir);\n\
  const isGuestCommandPath = (command) =>\n\
    typeof command === \"string\" &&\n\
    (command.startsWith(\"/\") || command.startsWith(\"file:\"));\n\
  const ensureRuntimeEnv = (env) => {{\n\
    const sourceEnv =\n\
      env && typeof env === \"object\" ? env : process.env;\n\
    const {{ NODE_OPTIONS: _nodeOptions, ...safeEnv }} = sourceEnv;\n\
    for (const key of [\"HOME\", \"PWD\", \"TMPDIR\", \"TEMP\", \"TMP\", \"PI_CODING_AGENT_DIR\"]) {{\n\
      if (typeof safeEnv[key] === \"string\") {{\n\
        safeEnv[key] = translateGuestPath(safeEnv[key], fromGuestDir);\n\
      }}\n\
    }}\n\
    const nodeDir = path.dirname(process.execPath);\n\
    const existingPath =\n\
      typeof safeEnv.PATH === \"string\"\n\
        ? safeEnv.PATH\n\
        : typeof process.env.PATH === \"string\"\n\
          ? process.env.PATH\n\
          : \"\";\n\
    const segments = existingPath\n\
      .split(path.delimiter)\n\
      .filter(Boolean);\n\n\
    if (!segments.includes(nodeDir)) {{\n\
      segments.unshift(nodeDir);\n\
    }}\n\n\
    return {{\n\
      ...safeEnv,\n\
      PATH: segments.join(path.delimiter),\n\
    }};\n\
  }};\n\
  const translateProcessOptions = (options) => {{\n\
    if (options == null) {{\n\
      return {{\n\
        env: ensureRuntimeEnv(process.env),\n\
      }};\n\
    }}\n\n\
    if (typeof options !== \"object\") {{\n\
      return options;\n\
    }}\n\n\
    return {{\n\
      ...options,\n\
      cwd:\n\
        typeof options.cwd === \"string\"\n\
          ? translateGuestPath(options.cwd, fromGuestDir)\n\
          : options.cwd,\n\
      env: ensureRuntimeEnv(options.env),\n\
    }};\n\
  }};\n\
  const translateArgs = (command, args) => {{\n\
    if (isNodeScriptCommand(command)) {{\n\
      const translatedScript = translateGuestPath(command, fromGuestDir);\n\
      const translatedArgs = Array.isArray(args)\n\
        ? args.map((arg) => translateGuestPath(arg, fromGuestDir))\n\
        : [];\n\
      return [translatedScript, ...translatedArgs];\n\
    }}\n\n\
    if (!Array.isArray(args)) {{\n\
      return args;\n\
    }}\n\
    if (!isNodeCommand(command)) {{\n\
      return args.map((arg) => translateGuestPath(arg, fromGuestDir));\n\
    }}\n\
    return args.map((arg, index) =>\n\
      index === 0 ? translateGuestPath(arg, fromGuestDir) : arg,\n\
    );\n\
  }};\n\n\
  const prependNodePermissionArgs = (command, args, options) => {{\n\
    if (!usesNodeRuntime(command)) {{\n\
      return args;\n\
    }}\n\n\
    const translatedArgs = Array.isArray(args) ? args : [];\n\
    const readPaths = new Set();\n\
    const writePaths = new Set();\n\
    const addReadPathChain = (value) => {{\n\
      if (typeof value !== \"string\" || value.length === 0) {{\n\
        return;\n\
      }}\n\
      let current = value;\n\
      while (true) {{\n\
        readPaths.add(current);\n\
        const parent = path.dirname(current);\n\
        if (parent === current) {{\n\
          break;\n\
        }}\n\
        current = parent;\n\
      }}\n\
    }};\n\
    const addWritePath = (value) => {{\n\
      if (typeof value !== \"string\" || value.length === 0) {{\n\
        return;\n\
      }}\n\
      writePaths.add(value);\n\
    }};\n\n\
    if (typeof options?.cwd === \"string\") {{\n\
      addReadPathChain(options.cwd);\n\
      addWritePath(options.cwd);\n\
    }}\n\n\
    const homePath =\n\
      typeof options?.env?.HOME === \"string\"\n\
        ? translateGuestPath(options.env.HOME, fromGuestDir)\n\
        : typeof process.env.HOME === \"string\"\n\
          ? translateGuestPath(process.env.HOME, fromGuestDir)\n\
          : null;\n\
    if (homePath) {{\n\
      addReadPathChain(homePath);\n\
      addWritePath(homePath);\n\
    }}\n\n\
    if (translatedArgs.length > 0 && typeof translatedArgs[0] === \"string\") {{\n\
      addReadPathChain(translatedArgs[0]);\n\
    }}\n\n\
    const permissionArgs = [\n\
      \"--allow-child-process\",\n\
      \"--allow-worker\",\n\
      \"--disable-warning=SecurityWarning\",\n\
    ];\n\n\
    for (const allowedPath of readPaths) {{\n\
      permissionArgs.push(`--allow-fs-read=${{allowedPath}}`);\n\
    }}\n\
    for (const allowedPath of writePaths) {{\n\
      permissionArgs.push(`--allow-fs-write=${{allowedPath}}`);\n\
    }}\n\n\
    return [...permissionArgs, ...translatedArgs];\n\
  }};\n\n\
  return {{\n\
    ...childProcessModule,\n\
    exec: childProcessModule.exec.bind(childProcessModule),\n\
    execFile: (file, args, options, callback) => {{\n\
      const translatedOptions = translateProcessOptions(options);\n\
      return childProcessModule.execFile(\n\
        translateCommand(file),\n\
        prependNodePermissionArgs(\n\
          file,\n\
          translateArgs(file, args),\n\
          translatedOptions,\n\
        ),\n\
        translatedOptions,\n\
        callback,\n\
      );\n\
    }},\n\
    execFileSync: (file, args, options) => {{\n\
      const translatedOptions = translateProcessOptions(options);\n\
      return childProcessModule.execFileSync(\n\
        translateCommand(file),\n\
        prependNodePermissionArgs(\n\
          file,\n\
          translateArgs(file, args),\n\
          translatedOptions,\n\
        ),\n\
        translatedOptions,\n\
      );\n\
    }},\n\
    execSync: childProcessModule.execSync.bind(childProcessModule),\n\
    fork: (modulePath, args, options) => {{\n\
      const translatedOptions = translateProcessOptions(options);\n\
      return childProcessModule.fork(\n\
        translateGuestPath(modulePath, fromGuestDir),\n\
        prependNodePermissionArgs(\n\
          \"node\",\n\
          translateArgs(\"node\", args),\n\
          translatedOptions,\n\
        ),\n\
        translatedOptions,\n\
      );\n\
    }},\n\
    spawn: (command, args, options) => {{\n\
      const translatedOptions = translateProcessOptions(options);\n\
      return childProcessModule.spawn(\n\
        translateCommand(command),\n\
        prependNodePermissionArgs(\n\
          command,\n\
          translateArgs(command, args),\n\
          translatedOptions,\n\
        ),\n\
        translatedOptions,\n\
      );\n\
    }},\n\
    spawnSync: (command, args, options) => {{\n\
      const translatedOptions = translateProcessOptions(options);\n\
      const result = childProcessModule.spawnSync(\n\
        translateCommand(command),\n\
        prependNodePermissionArgs(\n\
          command,\n\
          translateArgs(command, args),\n\
          translatedOptions,\n\
        ),\n\
        translatedOptions,\n\
      );\n\
      if (\n\
        isGuestCommandPath(command) &&\n\
        result?.status == null &&\n\
        (result.error?.code === \"ENOENT\" || result.error?.code === \"EACCES\")\n\
      ) {{\n\
        return {{\n\
          ...result,\n\
          status: 1,\n\
          stderr: Buffer.from(result.error.message),\n\
        }};\n\
      }}\n\
      return result;\n\
    }},\n\
  }};\n\
}}\n"
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
            String::from("dns"),
            String::from("http"),
            String::from("http2"),
            String::from("https"),
            String::from("inspector"),
            String::from("module"),
            String::from("net"),
            String::from("os"),
            String::from("tls"),
            String::from("trace_events"),
            String::from("v8"),
            String::from("vm"),
            String::from("worker_threads"),
        ]);

        assert_eq!(actual, expected);

        let os_asset =
            fs::read_to_string(denied_root.join("os.mjs")).expect("read os denied asset");
        let module_asset =
            fs::read_to_string(denied_root.join("module.mjs")).expect("read module denied asset");
        let trace_events_asset = fs::read_to_string(denied_root.join("trace_events.mjs"))
            .expect("read trace_events denied asset");

        assert!(os_asset.contains("node:os is not available"));
        assert!(module_asset.contains("node:module is not available"));
        assert!(trace_events_asset.contains("ERR_ACCESS_DENIED"));
    }
}
