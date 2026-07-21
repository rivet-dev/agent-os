const fsModule =
  typeof globalThis._requireFrom === 'function'
    ? globalThis._requireFrom('node:fs', '/')
    : __agentOSRequireBuiltin('node:fs');
const fs = fsModule.promises;
const { readSync, writeSync } = fsModule;
const path =
  typeof globalThis._requireFrom === 'function'
    ? globalThis._requireFrom('node:path', '/')
    : __agentOSRequireBuiltin('node:path');
const { WASI } = globalThis.__agentOSWasiModule;
const HOST_CWD =
  typeof process?.env?.AGENTOS_WASM_HOST_CWD === 'string' &&
  process.env.AGENTOS_WASM_HOST_CWD.length > 0
    ? path.resolve(process.env.AGENTOS_WASM_HOST_CWD)
    : path.resolve('.');

const WASI_ERRNO_SUCCESS = 0;
const WASI_ERRNO_ACCES = 2;
const WASI_ERRNO_ADDRINUSE = 3;
const WASI_ERRNO_ADDRNOTAVAIL = 4;
const WASI_ERRNO_AFNOSUPPORT = 5;
const WASI_ERRNO_AGAIN = 6;
const WASI_ERRNO_ALREADY = 7;
const WASI_ERRNO_BADF = 8;
const WASI_ERRNO_BUSY = 10;
const WASI_ERRNO_CHILD = 12;
const WASI_ERRNO_CONNREFUSED = 14;
const WASI_ERRNO_CONNRESET = 15;
const WASI_ERRNO_DEADLK = 16;
const WASI_ERRNO_DESTADDRREQ = 17;
const WASI_ERRNO_EXIST = 20;
const WASI_ERRNO_FBIG = 22;
const WASI_ERRNO_ILSEQ = 25;
const WASI_ERRNO_INPROGRESS = 26;
const WASI_ERRNO_INVAL = 28;
const WASI_ERRNO_INTR = 27;
const WASI_ERRNO_IO = 29;
const WASI_ERRNO_ISCONN = 30;
const WASI_ERRNO_ISDIR = 31;
const WASI_ERRNO_LOOP = 32;
const WASI_ERRNO_MFILE = 33;
const WASI_ERRNO_MSGSIZE = 35;
const WASI_ERRNO_NETUNREACH = 40;
const WASI_ERRNO_NFILE = 41;
const WASI_ERRNO_NAMETOOLONG = 37;
const WASI_ERRNO_NOBUFS = 42;
const WASI_ERRNO_NOENT = 44;
const WASI_ERRNO_NOEXEC = 45;
const WASI_ERRNO_NOMEM = 48;
const WASI_ERRNO_NOSPC = 51;
const WASI_ERRNO_NOSYS = 52;
const WASI_ERRNO_HOSTUNREACH = 23;
const WASI_ERRNO_NOTDIR = 54;
const WASI_ERRNO_NOTEMPTY = 55;
const WASI_ERRNO_NOTCONN = 53;
const WASI_ERRNO_NOTSOCK = 57;
const WASI_ERRNO_NOTSUP = 58;
const WASI_ERRNO_NXIO = 60;
const WASI_ERRNO_OVERFLOW = 61;
const WASI_ERRNO_PERM = 63;
const WASI_ERRNO_PIPE = 64;
const WASI_ERRNO_PROTONOSUPPORT = 66;
const WASI_ERRNO_RANGE = 68;
const WASI_ERRNO_2BIG = 1;
const WASI_ERRNO_ROFS = 69;
const WASI_ERRNO_SPIPE = 70;
const WASI_ERRNO_SRCH = 71;
const WASI_ERRNO_TIMEDOUT = 73;
const WASI_ERRNO_XDEV = 75;
const WASI_ERRNO_NODATA = 78;
const WASI_ERRNO_FAULT = 21;
const WASI_RIGHT_FD_WRITE = 64n;
const WASI_FILETYPE_UNKNOWN = 0;
const WASI_FILETYPE_CHARACTER_DEVICE = 2;
const WASI_FILETYPE_DIRECTORY = 3;
const WASI_FILETYPE_REGULAR_FILE = 4;
const WASI_FILETYPE_SOCKET_DGRAM = 5;
const WASI_FILETYPE_SOCKET_STREAM = 6;
const WASI_OFLAGS_CREAT = 1;
const WASI_OFLAGS_DIRECTORY = 2;
const WASI_OFLAGS_EXCL = 4;
const WASI_OFLAGS_TRUNC = 8;
const WASI_FDFLAGS_APPEND = 1;
const WASI_FDFLAGS_NONBLOCK = 4;
const WASI_LIBC_O_DIRECT = 0x20000000;
const WASI_LIBC_O_RDONLY = 0x04000000;
const WASI_LIBC_O_WRONLY = 0x10000000;
const KERNEL_O_WRONLY = 0o1;
const KERNEL_O_RDWR = 0o2;
const KERNEL_O_CREAT = 0o100;
const KERNEL_O_EXCL = 0o200;
const KERNEL_O_TRUNC = 0o1000;
const KERNEL_O_APPEND = 0o2000;
const KERNEL_O_NONBLOCK = 0o4000;
const KERNEL_O_DIRECT = 0o40000;
const KERNEL_O_DIRECTORY = 0o200000;
const KERNEL_O_NOFOLLOW = 0o400000;
const WASI_WHENCE_SET = 0;
const WASI_WHENCE_CUR = 1;
const WASI_WHENCE_END = 2;
const WASM_PAGE_BYTES = 65536;
// Linux exposes a separate numeric descriptor ceiling (`fs.nr_open`) in
// addition to the process' open-description limit. Keep the guest namespace
// below that ceiling so ordinary allocation can never collide with the
// private 0x40000000 pathname-preopen tag.
const LINUX_GUEST_FD_LIMIT = 1 << 20;
// Linux readv(2)/writev(2) reject vectors larger than UIO_MAXIOV. Keep the
// same bound for Preview1 iovec arrays and for poll subscriptions, which are
// likewise attacker-controlled fixed-size guest tables.
const LINUX_IOV_MAX = 1024;
const WASI_POLL_MAX_SUBSCRIPTIONS = 1024;
const XATTR_NAME_MAX = 255;
const XATTR_SIZE_MAX = 64 * 1024;
const LINUX_BINPRM_BUF_SIZE = 256;
const LINUX_MAX_INTERPRETER_DEPTH = 4;
const DEFAULT_WASM_MAX_MODULE_FILE_BYTES = 256 * 1024 * 1024;
const warnedFixedRequestLimits = new Set();

function checkFixedRequestLimit(limitName, observed, maximum, errno = WASI_ERRNO_INVAL) {
  const numericObserved = Number(observed) >>> 0;
  const warningAt = Math.max(1, Math.ceil(maximum * 0.8));
  if (
    numericObserved >= warningAt &&
    !warnedFixedRequestLimits.has(limitName)
  ) {
    warnedFixedRequestLimits.add(limitName);
    if (typeof process?.stderr?.write === 'function') {
      process.stderr.write(
        `[agentos] WASM request is near ${limitName} ` +
        `(${numericObserved}/${maximum}); split the request if needed\n`,
      );
    }
  }
  return numericObserved > maximum ? errno : WASI_ERRNO_SUCCESS;
}

function boundedWasmSyncRpcReadLength(length) {
  return Math.min(
    Number(length) >>> 0,
    __agentOSWasmSyncRpcReadPayloadBytes,
  );
}
const POSIX_SPAWN_RESETIDS = 1;
const POSIX_SPAWN_SETPGROUP = 2;
const POSIX_SPAWN_SETSIGDEF = 4;
const POSIX_SPAWN_SETSIGMASK = 8;
const LINUX_SA_NODEFER = 0x40000000;
const LINUX_SA_RESTART = 0x10000000;
const POSIX_SPAWN_SETSCHEDPARAM = 16;
const POSIX_SPAWN_SETSCHEDULER = 32;
const POSIX_SPAWN_USEVFORK = 64;
const POSIX_SPAWN_SETSID = 128;
const SUPPORTED_POSIX_SPAWN_FLAGS =
  POSIX_SPAWN_RESETIDS |
  POSIX_SPAWN_SETPGROUP |
  POSIX_SPAWN_SETSIGDEF |
  POSIX_SPAWN_SETSIGMASK |
  POSIX_SPAWN_SETSCHEDPARAM |
  POSIX_SPAWN_SETSCHEDULER |
  POSIX_SPAWN_USEVFORK |
  POSIX_SPAWN_SETSID;
const LINUX_SIGKILL = 9;
const LINUX_SIGSTOP = 19;
const INTERNAL_KERNEL_COMMAND_STUB = Buffer.from('#!/bin/sh\n# kernel command stub\n');
const DEFAULT_VIRTUAL_PID = 1;
const DEFAULT_VIRTUAL_PPID = 0;
const DEFAULT_VIRTUAL_UID = 0;
const DEFAULT_VIRTUAL_GID = 0;
const DEFAULT_VIRTUAL_OS_USER = 'root';
const DEFAULT_VIRTUAL_OS_HOMEDIR = '/root';
const DEFAULT_VIRTUAL_OS_SHELL = '/bin/sh';

function parseVirtualProcessNumber(value, fallback) {
  if (typeof value !== 'string' || value.trim() === '') {
    return fallback;
  }
  const parsed = Number.parseInt(value, 10);
  return Number.isInteger(parsed) && parsed >= 0 ? parsed : fallback;
}

function parseVirtualProcessString(value, fallback) {
  return typeof value === 'string' && value.length > 0 ? value : fallback;
}

function parseInitialKernelFdMappings(value, limit) {
  if (typeof value !== 'string' || value.length === 0) {
    return new Map();
  }
  let parsed;
  try {
    parsed = JSON.parse(value);
  } catch (error) {
    throw new Error(`AGENTOS_WASM_INHERITED_FD_MAPPINGS must be a JSON pair array: ${error}`);
  }
  if (!Array.isArray(parsed) || parsed.length > limit) {
    throw new Error(
      `AGENTOS_WASM_INHERITED_FD_MAPPINGS exceeds the ${limit}-descriptor runtime limit`,
    );
  }
  const byKernelFd = new Map();
  const guestFds = new Set();
  for (const pair of parsed) {
    if (!Array.isArray(pair) || pair.length !== 2) {
      throw new Error('AGENTOS_WASM_INHERITED_FD_MAPPINGS entries must be [guestFd, kernelFd]');
    }
    const guestFd = Number(pair[0]);
    const kernelFd = Number(pair[1]);
    if (
      !Number.isSafeInteger(guestFd) || guestFd < 0 || guestFd >= LINUX_GUEST_FD_LIMIT ||
      !Number.isSafeInteger(kernelFd) || kernelFd < 0 || kernelFd > 0xffffffff ||
      guestFds.has(guestFd) || byKernelFd.has(kernelFd)
    ) {
      throw new Error('AGENTOS_WASM_INHERITED_FD_MAPPINGS contains invalid or duplicate fds');
    }
    guestFds.add(guestFd);
    byKernelFd.set(kernelFd, guestFd);
  }
  return byKernelFd;
}

function parseInitialClosedGuestFds(value, limit) {
  if (typeof value !== 'string' || value.length === 0) {
    return new Set();
  }
  let parsed;
  try {
    parsed = JSON.parse(value);
  } catch (error) {
    throw new Error(`AGENTOS_WASM_CLOSED_INHERITED_FDS must be a JSON fd array: ${error}`);
  }
  if (!Array.isArray(parsed) || parsed.length > limit) {
    throw new Error(
      `AGENTOS_WASM_CLOSED_INHERITED_FDS exceeds the ${limit}-descriptor runtime limit`,
    );
  }
  const descriptors = new Set();
  for (const value of parsed) {
    const fd = Number(value);
    if (
      !Number.isSafeInteger(fd) || fd < 0 || fd >= LINUX_GUEST_FD_LIMIT ||
      descriptors.has(fd)
    ) {
      throw new Error('AGENTOS_WASM_CLOSED_INHERITED_FDS contains invalid or duplicate fds');
    }
    descriptors.add(fd);
  }
  return descriptors;
}

function resolveVirtualPath(value, fallback) {
  const resolved = parseVirtualProcessString(value, fallback);
  return resolved.startsWith('/') ? path.posix.normalize(resolved) : fallback;
}

function isPathLike(specifier) {
  return specifier.startsWith('.') || specifier.startsWith('/') || specifier.startsWith('file:');
}

function resolveModuleGuestPathToHostPath(guestPath) {
  return resolveModuleGuestPathToHostMapping(guestPath)?.hostPath ?? null;
}

function resolveModuleGuestPathToHostMapping(guestPath) {
  if (typeof guestPath !== 'string') {
    return null;
  }

  const normalized = path.posix.normalize(guestPath);
  for (const mapping of GUEST_PATH_MAPPINGS) {
    if (mapping.guestPath === '/') {
      const suffix = normalized.replace(/^\/+/, '');
      return {
        hostPath: suffix ? path.join(mapping.hostPath, suffix) : mapping.hostPath,
        readOnly: mapping.readOnly === true,
      };
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
    return {
      hostPath: suffix ? path.join(mapping.hostPath, ...suffix.split('/')) : mapping.hostPath,
      readOnly: mapping.readOnly === true,
    };
  }

  return null;
}

function resolveModulePath(specifier) {
  if (specifier.startsWith('file:')) {
    const guestPath = guestFilePathFromUrl(specifier);
    if (guestPath) {
      return resolveModuleGuestPathToHostPath(guestPath) ?? new URL(specifier);
    }
    return new URL(specifier);
  }
  if (isPathLike(specifier)) {
    if (specifier.startsWith('/')) {
      return resolveModuleGuestPathToHostPath(specifier) ?? path.resolve(process.cwd(), specifier);
    }
    return path.resolve(process.cwd(), specifier);
  }
  return specifier;
}

function parseGuestPathMappings(value) {
  if (typeof value !== 'string' || value.length === 0) {
    return [];
  }
  try {
    return JSON.parse(value)
      .map((entry) => {
        const guestPath =
          entry && typeof entry.guestPath === 'string'
            ? path.posix.normalize(entry.guestPath)
            : null;
        const hostPath =
          entry && typeof entry.hostPath === 'string'
            ? path.resolve(entry.hostPath)
            : null;
        return guestPath && hostPath
          ? { guestPath, hostPath, readOnly: entry.readOnly === true }
          : null;
      })
      .filter(Boolean)
      .sort((left, right) => right.guestPath.length - left.guestPath.length);
  } catch {
    return [];
  }
}

const modulePath = process.env.AGENTOS_WASM_MODULE_PATH;
if (!modulePath) {
  throw new Error('AGENTOS_WASM_MODULE_PATH is required');
}
const moduleBase64 = process.env.AGENTOS_WASM_MODULE_BASE64;
const __agentOSWasmPhaseDebug = process.env.AGENTOS_WASM_WARMUP_DEBUG === '1';
const __agentOSWasmPhaseTimings = [];
const __agentOSWasiSyscallPhasesEnabled = process.env.AGENTOS_WASI_SYSCALL_PHASES === '1';
const __agentOSWasiSyscallMetrics = new Map();
const __agentOSWasiSyncRpcMetrics = new Map();

function __agentOSWasmNowNs() {
  return process.hrtime.bigint();
}

function __agentOSWasmRecordPhase(name, startedNs) {
  if (!__agentOSWasmPhaseDebug) {
    return;
  }
  const elapsedNs = __agentOSWasmNowNs() - startedNs;
  __agentOSWasmPhaseTimings.push({
    name,
    ms: Number(elapsedNs) / 1000000,
  });
}

function __agentOSWasmMeasurePhase(name, run) {
  const startedNs = __agentOSWasmNowNs();
  try {
    return run();
  } finally {
    __agentOSWasmRecordPhase(name, startedNs);
  }
}

function __agentOSWasiMetricFor(map, key, factory) {
  let metric = map.get(key);
  if (!metric) {
    metric = factory();
    map.set(key, metric);
  }
  return metric;
}

function __agentOSWasiSyscallMetric(name) {
  return __agentOSWasiMetricFor(__agentOSWasiSyscallMetrics, name, () => ({
    name,
    count: 0,
    totalNs: 0n,
    phases: new Map(),
  }));
}

function __agentOSWasiRecordSyscall(name, startedNs) {
  if (!__agentOSWasiSyscallPhasesEnabled) {
    return;
  }
  const metric = __agentOSWasiSyscallMetric(name);
  metric.count += 1;
  metric.totalNs += __agentOSWasmNowNs() - startedNs;
}

function __agentOSWasiRecordPhase(syscallName, phaseName, startedNs) {
  if (!__agentOSWasiSyscallPhasesEnabled) {
    return;
  }
  const syscall = __agentOSWasiSyscallMetric(syscallName);
  const phase = __agentOSWasiMetricFor(syscall.phases, phaseName, () => ({
    name: phaseName,
    count: 0,
    totalNs: 0n,
  }));
  phase.count += 1;
  phase.totalNs += __agentOSWasmNowNs() - startedNs;
}

function __agentOSWasiMeasurePhase(syscallName, phaseName, run) {
  if (!__agentOSWasiSyscallPhasesEnabled) {
    return run();
  }
  const startedNs = __agentOSWasmNowNs();
  try {
    return run();
  } finally {
    __agentOSWasiRecordPhase(syscallName, phaseName, startedNs);
  }
}

function __agentOSWasiRecordSyncRpc(method, route, startedNs) {
  if (!__agentOSWasiSyscallPhasesEnabled) {
    return;
  }
  const key = `${route}:${method}`;
  const metric = __agentOSWasiMetricFor(__agentOSWasiSyncRpcMetrics, key, () => ({
    method,
    route,
    count: 0,
    totalNs: 0n,
  }));
  metric.count += 1;
  metric.totalNs += __agentOSWasmNowNs() - startedNs;
}

function __agentOSWasiNsToUs(ns) {
  return Number(ns) / 1000;
}

function __agentOSWasiMetricSummary(metric) {
  const totalUs = __agentOSWasiNsToUs(metric.totalNs);
  return {
    name: metric.name,
    count: metric.count,
    totalUs,
    avgUs: metric.count > 0 ? totalUs / metric.count : 0,
    phases: Array.from(metric.phases.values())
      .map((phase) => {
        const phaseTotalUs = __agentOSWasiNsToUs(phase.totalNs);
        return {
          name: phase.name,
          count: phase.count,
          totalUs: phaseTotalUs,
          avgUs: phase.count > 0 ? phaseTotalUs / phase.count : 0,
        };
      })
      .sort((left, right) => right.totalUs - left.totalUs),
  };
}

function __agentOSWasiEmitSyscallPhaseMetrics() {
  if (!__agentOSWasiSyscallPhasesEnabled || typeof process?.stderr?.write !== 'function') {
    return;
  }
  try {
    process.stderr.write(`__AGENTOS_WASI_SYSCALL_PHASE_METRICS__:${JSON.stringify({
      modulePath,
      syscalls: Array.from(__agentOSWasiSyscallMetrics.values())
        .map(__agentOSWasiMetricSummary)
        .sort((left, right) => right.totalUs - left.totalUs),
      syncRpc: Array.from(__agentOSWasiSyncRpcMetrics.values())
        .map((metric) => {
          const totalUs = __agentOSWasiNsToUs(metric.totalNs);
          return {
            method: metric.method,
            route: metric.route,
            count: metric.count,
            totalUs,
            avgUs: metric.count > 0 ? totalUs / metric.count : 0,
          };
        })
        .sort((left, right) => right.totalUs - left.totalUs),
    })}\n`);
  } catch {
    // Diagnostics must never change command behavior.
  }
}

if (__agentOSWasiSyscallPhasesEnabled && typeof process?.on === 'function') {
  process.on('exit', () => {
  });
}

function __agentOSWasmEmitPhaseMetrics(reason, extra = {}) {
  if (!__agentOSWasmPhaseDebug || typeof process?.stderr?.write !== 'function') {
    return;
  }
  try {
    process.stderr.write(`__AGENTOS_WASM_PHASE_METRICS__:${JSON.stringify({
      reason,
      modulePath,
      sourceModuleBytes: typeof moduleBytes !== 'undefined' ? moduleBytes.byteLength : null,
      moduleBytes: typeof moduleBinary !== 'undefined' ? moduleBinary.byteLength : null,
      phases: __agentOSWasmPhaseTimings,
      ...extra,
    })}\n`);
  } catch {
    // Diagnostics must never change command behavior.
  }
}

let guestArgv = JSON.parse(process.env.AGENTOS_GUEST_ARGV ?? '[]');
let guestEnv = JSON.parse(process.env.AGENTOS_GUEST_ENV ?? '{}');
const GUEST_PATH_MAPPINGS = parseGuestPathMappings(process.env.AGENTOS_GUEST_PATH_MAPPINGS);
const permissionTier = process.env.AGENTOS_WASM_PERMISSION_TIER ?? 'full';
const prewarmOnly = process.env.AGENTOS_WASM_PREWARM_ONLY === '1';
const maxMemoryBytesValue = Number(process.env.AGENTOS_WASM_MAX_MEMORY_BYTES);
const maxMemoryPages = Number.isFinite(maxMemoryBytesValue)
  ? Math.max(0, Math.floor(maxMemoryBytesValue / WASM_PAGE_BYTES))
  : null;
const maxModuleFileBytesValue = Number(process.env.AGENTOS_WASM_MAX_MODULE_FILE_BYTES);
const maxModuleFileBytes =
  Number.isFinite(maxModuleFileBytesValue) && maxModuleFileBytesValue >= 0
    ? Math.floor(maxModuleFileBytesValue)
    : DEFAULT_WASM_MAX_MODULE_FILE_BYTES;
const maxSpawnFileActionsValue = Number(process.env.AGENTOS_WASM_MAX_SPAWN_FILE_ACTIONS);
const maxSpawnFileActions =
  Number.isFinite(maxSpawnFileActionsValue) && maxSpawnFileActionsValue > 0
    ? Math.floor(maxSpawnFileActionsValue)
    : 4096;
const maxSpawnFileActionBytesValue = Number(
  process.env.AGENTOS_WASM_MAX_SPAWN_FILE_ACTION_BYTES,
);
const maxSpawnFileActionBytes =
  Number.isFinite(maxSpawnFileActionBytesValue) && maxSpawnFileActionBytesValue > 0
    ? Math.floor(maxSpawnFileActionBytesValue)
    : 1024 * 1024;
let warnedSpawnFileActions = false;
let warnedSpawnFileActionBytes = false;
// This value is injected by the trusted bootstrap from the typed execution
// limits. The legacy AGENTOS_WASM_MAX_STACK_BYTES guest env key is scrubbed so
// guest code cannot raise its own limit or forge limit-attribution diagnostics.
const maxStackBytesValue = Number(process.env.AGENTOS_INTERNAL_WASM_MAX_STACK_BYTES);
const maxStackBytes =
  Number.isFinite(maxStackBytesValue) && maxStackBytesValue > 0
    ? Math.floor(maxStackBytesValue)
    : null;
const DEFAULT_SYNC_RPC_RESPONSE_LINE_BYTES = 16 * 1024 * 1024;
const maxSyncRpcResponseLineBytesValue = Number(
  process.env.AGENTOS_INTERNAL_WASM_SYNC_RPC_RESPONSE_LINE_BYTES,
);
const maxSyncRpcResponseLineBytes =
  Number.isSafeInteger(maxSyncRpcResponseLineBytesValue) && maxSyncRpcResponseLineBytesValue > 0
    ? maxSyncRpcResponseLineBytesValue
    : DEFAULT_SYNC_RPC_RESPONSE_LINE_BYTES;
const maxOpenFdsValue = Number(process.env.AGENTOS_WASM_MAX_OPEN_FDS);
const configuredMaxOpenFds = Number.isFinite(maxOpenFdsValue) && maxOpenFdsValue >= 0
  ? Math.floor(maxOpenFdsValue)
  : 1024;
const maxSocketsValue = Number(process.env.AGENTOS_WASM_MAX_SOCKETS);
const maxSockets = Number.isFinite(maxSocketsValue) && maxSocketsValue >= 0
  ? Math.floor(maxSocketsValue)
  : null;
const SIDECAR_MANAGED_PROCESS = globalThis.__agentOSManagedKernelHost === true;
// Shared with the typed host-dispatch NODE_CWD_FD sentinel. This is an
// executor ABI value, not a real guest descriptor, so path authorization
// must never mistake stdin (fd 0) for the absolute/cwd capability.
const NODE_CWD_FD = 0xffffffff;
const initialKernelFdMappings = parseInitialKernelFdMappings(
  process.env.AGENTOS_WASM_INHERITED_FD_MAPPINGS,
  configuredMaxOpenFds,
);
const initialHostNetDescriptions = parseInitialHostNetFds(
  process.env.AGENTOS_WASM_INHERITED_HOSTNET_FDS,
  configuredMaxOpenFds,
  maxSockets,
);
const initialHostNetGuestFds = initialHostNetDescriptions
  .flatMap((description) => description.guestFds.map((fd) => Number(fd) >>> 0));
const initialKernelGuestFds = new Set(initialKernelFdMappings.values());
// Managed host-network metadata describes the same canonical kernel-backed
// descriptor, rather than a second guest descriptor. Legacy/standalone socket
// bootstraps remain disjoint from kernel mappings.
const managedHostNetKernelGuestFds = new Set(
  SIDECAR_MANAGED_PROCESS
    ? initialHostNetDescriptions
        .filter((description) =>
          typeof description.descriptionId === 'string' &&
          /^[0-9]+$/.test(description.descriptionId)
        )
        .flatMap((description) => description.guestFds.map((fd) => Number(fd) >>> 0))
    : [],
);
const initialMappedGuestFds = new Set([
  ...initialKernelGuestFds,
  ...initialHostNetGuestFds,
]);
// Keep explicit inherited guest destinations unavailable while bootstrap
// assigns guest numbers to otherwise-unmapped kernel descriptors. Without
// this reservation, an earlier unmapped kernel fd can take (for example)
// guest fd 7 before a later kernel fd is installed at its required guest fd 7.
const pendingInitialKernelGuestFds = new Set(initialKernelFdMappings.values());
for (const guestFd of initialHostNetGuestFds) {
  if (initialKernelGuestFds.has(guestFd) && !managedHostNetKernelGuestFds.has(guestFd)) {
    throw new Error('inherited kernel and host-network descriptors overlap in the guest fd table');
  }
}
if (initialMappedGuestFds.size > configuredMaxOpenFds) {
  throw new Error(
    `inherited descriptors exceed limits.resources.maxOpenFds (${configuredMaxOpenFds})`,
  );
}
const initialClosedGuestFds = parseInitialClosedGuestFds(
  process.env.AGENTOS_WASM_CLOSED_INHERITED_FDS,
  configuredMaxOpenFds,
);
traceHostProcess('kernel-fd-bootstrap', {
  mappings: [...initialKernelFdMappings.entries()],
  closedGuestFds: [...initialClosedGuestFds],
  hostNetGuestFds: initialHostNetGuestFds,
});
const maxBlockingReadMsValue = Number(process.env.AGENTOS_WASM_MAX_BLOCKING_READ_MS);
const maxBlockingReadMs = Number.isFinite(maxBlockingReadMsValue) && maxBlockingReadMsValue >= 0
  ? Math.floor(maxBlockingReadMsValue)
  : null;
const unixConnectTimeoutMs = maxBlockingReadMs ?? 30_000;

function warnNearBlockingReadLimit(operation, startedAt, alreadyWarned, applies = true) {
  if (
    !applies ||
    alreadyWarned ||
    Date.now() < startedAt + Math.floor(unixConnectTimeoutMs * 0.8)
  ) {
    return alreadyWarned;
  }
  process.stderr.write(
    `[agentos] ${operation} is nearing limits.resources.maxBlockingReadMs (${unixConnectTimeoutMs} ms)\n`,
  );
  return true;
}

// A guest can drive WebAssembly into never-returning recursion. V8's default
// native stack guard already traps that as a generic `RangeError`, but the
// operator-configured typed stack budget was previously
// never consulted, so the cap was dead. When a stack byte budget is set, treat
// a stack-exhaustion trap as enforcement of THAT budget: terminate the guest
// nonzero and attribute the failure to the configured limit instead of leaking
// the engine's generic default-guard message.
function isWasmStackExhaustionTrap(error) {
  const message = typeof error?.message === 'string' ? error.message : '';
  // V8 raises `RangeError: Maximum call stack size exceeded` when its native
  // stack guard fires on runaway recursion (the WebAssembly call stack is
  // mapped onto V8's). Match that explicitly rather than treating every
  // `RangeError` as stack exhaustion, so unrelated range failures still
  // surface with their own message.
  return /maximum call stack size exceeded/i.test(message);
}

function reportConfiguredStackLimitExceeded(error) {
  const detail = typeof error?.message === 'string' && error.message.length > 0
    ? ` (${error.message})`
    : '';
  if (typeof process?.stderr?.write === 'function') {
    process.stderr.write(
      `WebAssembly guest exhausted its configured stack budget (${maxStackBytes} bytes); ` +
        `raise limits.resources.maxWasmStackBytes to allow deeper guest call stacks${detail}.\n`,
    );
  }
}
const frozenTimeValue = Number(process.env.AGENTOS_FROZEN_TIME_MS);
const frozenTimeMs = Number.isFinite(frozenTimeValue) ? Math.trunc(frozenTimeValue) : Date.now();
const frozenTimeNs = BigInt(frozenTimeMs) * 1000000n;
const VIRTUAL_UID = parseVirtualProcessNumber(
  process.env.AGENTOS_VIRTUAL_PROCESS_UID,
  DEFAULT_VIRTUAL_UID,
);
const VIRTUAL_GID = parseVirtualProcessNumber(
  process.env.AGENTOS_VIRTUAL_PROCESS_GID,
  DEFAULT_VIRTUAL_GID,
);
const VIRTUAL_PID = parseVirtualProcessNumber(
  process.env.AGENTOS_VIRTUAL_PROCESS_PID,
  DEFAULT_VIRTUAL_PID,
);
const VIRTUAL_PPID = parseVirtualProcessNumber(
  process.env.AGENTOS_VIRTUAL_PROCESS_PPID,
  DEFAULT_VIRTUAL_PPID,
);
const VIRTUAL_OS_USER = parseVirtualProcessString(
  (globalThis.__agentOSVirtualOs||{}).user,
  DEFAULT_VIRTUAL_OS_USER,
);
const VIRTUAL_OS_HOMEDIR = resolveVirtualPath(
  (globalThis.__agentOSVirtualOs||{}).homedir,
  DEFAULT_VIRTUAL_OS_HOMEDIR,
);
const VIRTUAL_OS_SHELL = resolveVirtualPath(
  (globalThis.__agentOSVirtualOs||{}).shell,
  DEFAULT_VIRTUAL_OS_SHELL,
);
const CONTROL_PIPE_FD = parseControlPipeFd(process.env.AGENTOS_CONTROL_PIPE_FD);
const NODE_SYNC_RPC_ENABLE = process.env.AGENTOS_NODE_SYNC_RPC_ENABLE === '1';
const NODE_SYNC_RPC_REQUEST_FD = parseControlPipeFd(process.env.AGENTOS_NODE_SYNC_RPC_REQUEST_FD);
const NODE_SYNC_RPC_RESPONSE_FD = parseControlPipeFd(process.env.AGENTOS_NODE_SYNC_RPC_RESPONSE_FD);
const KERNEL_STDIO_SYNC_RPC = process.env.AGENTOS_WASI_STDIO_SYNC_RPC === '1';
const SIDECAR_EXEC_COMMIT_RPC = process.env.AGENTOS_WASM_EXEC_COMMIT_RPC === '1';
let nextSyncRpcId = 1;
let syncRpcResponseBuffer = Buffer.alloc(0);
let warnedSyncRpcResponseLineBytes = false;
// Managed execution keeps only bounded transport correlations here. Child
// lifecycle, process groups, and wait status remain kernel-authoritative.
const childCorrelationsByPid = new Map();
const childCorrelationsById = new Map();
let nextBlockingChildPumpIndex = 0;
let nextManagedCorrelationCollectionAtMs = 0;
let nextSyntheticChildPid = 0x40000000;
// Managed entries are projections of live kernel descriptions. Standalone
// entries emulate Node-WASI descriptors and may own runner-local semantics.
const managedKernelFdProjections = new Map();
const standaloneSyntheticFdEntries = new Map();
const activeFdProjections = SIDECAR_MANAGED_PROCESS
  ? managedKernelFdProjections
  : standaloneSyntheticFdEntries;

function setActiveFdProjection(fd, handle) {
  if (SIDECAR_MANAGED_PROCESS && handle?.kind !== 'kernel-fd') {
    throw new Error('managed descriptor projections must reference a kernel fd');
  }
  activeFdProjections.set(Number(fd) >>> 0, handle);
}

// Only the standalone Node-WASI fallback lacks kernel descriptor flags.
const standaloneCloexecFds = new Set();
const standaloneDelegateFdRefCounts = new Map();
const closedPassthroughFds = new Set();
globalThis.__agentOSWasiDelegateFdRefCount = (fd) =>
  standaloneDelegateFdRefCounts.get(Number(fd) >>> 0) ?? 0;
const passthroughHandles = new Map([
  [0, { kind: 'passthrough', targetFd: 0, displayFd: 0, refCount: 0, open: true }],
  [1, { kind: 'passthrough', targetFd: 1, displayFd: 1, refCount: 0, open: true }],
  [2, { kind: 'passthrough', targetFd: 2, displayFd: 2, refCount: 0, open: true }],
]);
// POSIX spawn close actions are applied before the child runner starts. Node's
// WASI bootstrap still creates private 0/1/2 entries, so explicitly hide any
// stdio descriptor the child inherited as closed.
for (const fd of initialClosedGuestFds) {
  if (fd <= 2) {
    passthroughHandles.delete(fd);
    closedPassthroughFds.add(fd);
  }
}
const retainedSyntheticHandlesByDisplayFd = new Map();
const retainedSpawnOutputHandlesByFd = new Map();
const FIRST_SYNTHETIC_FD = 3;
let nextSyntheticFd = FIRST_SYNTHETIC_FD;
let nextSyntheticPipeId = 1;
const syntheticWaitArray = new Int32Array(new SharedArrayBuffer(4));
let delegateWriteScratch = { base: 0, capacity: 0 };
const EXEC_REPLACEMENT_MARKER = Symbol('agentos.wasm.exec-replacement');
let warnedExecCloseFailure = false;

function warnExecCloseFailure(fd, detail) {
  if (warnedExecCloseFailure || typeof process?.stderr?.write !== 'function') return;
  warnedExecCloseFailure = true;
  process.stderr.write(
    `[agentos] exec committed but closing FD_CLOEXEC descriptor ${fd} failed (${detail}); further close failures are suppressed\n`,
  );
}

function isExecReplacement(error) {
  return error && typeof error === 'object' && error.marker === EXEC_REPLACEMENT_MARKER;
}

function readExecCloexecFds(ptr, count) {
  const length = Number(count) >>> 0;
  if (length > configuredMaxOpenFds) {
    const error = new RangeError(
      `proc_exec CLOEXEC descriptor count ${length} exceeds limits.resources.maxOpenFds (${configuredMaxOpenFds}); raise limits.resources.maxOpenFds if needed`,
    );
    error.code = 'EINVAL';
    throw error;
  }
  if (!(instanceMemory instanceof WebAssembly.Memory)) {
    throw new Error('WebAssembly memory is unavailable');
  }
  const base = Number(ptr) >>> 0;
  const byteLength = length * 4;
  if (base + byteLength > instanceMemory.buffer.byteLength) {
    const error = new RangeError('proc_exec CLOEXEC descriptor list is outside guest memory');
    error.code = 'EINVAL';
    throw error;
  }
  const view = new DataView(instanceMemory.buffer);
  const fds = [];
  for (let index = 0; index < length; index += 1) {
    fds.push(view.getUint32(base + index * 4, true));
  }
  return fds;
}

function resolveExecModulePath(command) {
  const raw = String(command);
  const guestPath = raw.startsWith('/')
    ? path.posix.normalize(raw)
    : path.posix.resolve(HOST_FS_GUEST_CWD, raw);
  return resolveModuleGuestPathToHostPath(guestPath) ?? guestPath;
}

function execError(code, message) {
  const error = new Error(message);
  error.code = code;
  return error;
}

function isProjectedCommandGuestPath(subject) {
  const raw = String(subject);
  const guestPath = raw.startsWith('/')
    ? path.posix.normalize(raw)
    : path.posix.resolve(HOST_FS_GUEST_CWD, raw);
  return (
    /^\/__agentos\/commands\/\d+(?:\/|$)/u.test(guestPath) ||
    /^\/opt\/agentos\/bin\/[^/]+$/u.test(guestPath)
  );
}

function validateExecutableStat(stats, subject, projectedExecutable = false) {
  if (typeof stats?.isFile !== 'function' || !stats.isFile()) {
    throw execError('EACCES', `${subject} is not a regular executable file`);
  }
  if (!projectedExecutable && (Number(stats.mode) & 0o111) === 0) {
    throw execError('EACCES', `${subject} does not have an executable mode bit`);
  }
  const size = Number(stats.size);
  if (!Number.isSafeInteger(size) || size < 0) {
    throw execError('EFBIG', `${subject} has an invalid executable image size`);
  }
  if (size > maxModuleFileBytes) {
    throw execError(
      'EFBIG',
      `${subject} is ${size} bytes, exceeding limits.wasm.maxModuleFileBytes (${maxModuleFileBytes}); raise limits.wasm.maxModuleFileBytes if needed`,
    );
  }
  return size;
}

function readExecutableFdBytes(targetFd, stats, subject, projectedExecutable = false) {
  const size = validateExecutableStat(stats, subject, projectedExecutable);
  const bytes = Buffer.alloc(size);
  let offset = 0;
  while (offset < size) {
    const count = fsModule.readSync(targetFd, bytes, offset, size - offset, offset);
    if (!Number.isInteger(count) || count <= 0) {
      throw execError('EIO', `${subject} changed while its executable image was read`);
    }
    offset += count;
  }
  return bytes;
}

function readExecutablePathBytes(command) {
  if (SIDECAR_MANAGED_PROCESS) {
    const subject = String(command);
    const image = callSyncRpc('process.exec_image_open', [subject]);
    const handle = String(image?.handle ?? '');
    try {
      const projectedExecutable = isProjectedCommandGuestPath(subject);
      if (!projectedExecutable && (Number(image?.mode) & 0o111) === 0) {
        throw execError('EACCES', `${subject} does not have an executable mode bit`);
      }
      const size = Number(image?.size);
      if (!Number.isSafeInteger(size) || size < 0) {
        throw execError('EFBIG', `${subject} has an invalid executable image size`);
      }
      if (size > maxModuleFileBytes) {
        throw execError(
          'EFBIG',
          `${subject} is ${size} bytes, exceeding limits.wasm.maxModuleFileBytes (${maxModuleFileBytes}); raise limits.wasm.maxModuleFileBytes if needed`,
        );
      }

      const bytes = Buffer.alloc(size);
      let offset = 0;
      while (offset < size) {
        const requested = boundedWasmSyncRpcReadLength(size - offset);
        const chunk = Buffer.from(callSyncRpc('process.exec_image_read', [
          handle,
          String(offset),
          requested,
        ]) ?? []);
        if (chunk.byteLength === 0 || chunk.byteLength > requested) {
          throw execError('EIO', `${subject} changed while its executable image was read`);
        }
        chunk.copy(bytes, offset);
        offset += chunk.byteLength;
      }
      return bytes;
    } finally {
      if (handle.length > 0) {
        callSyncRpc('process.exec_image_close', [handle]);
      }
    }
  }

  const hostPath = resolveExecModulePath(command);
  const stats = fsModule.statSync(hostPath);
  const size = validateExecutableStat(
    stats,
    String(command),
    isProjectedCommandGuestPath(command),
  );
  const bytes = fsModule.readFileSync(hostPath);
  if (bytes.byteLength > size || bytes.byteLength > maxModuleFileBytes) {
    throw execError(
      'EFBIG',
      `${command} grew beyond limits.wasm.maxModuleFileBytes (${maxModuleFileBytes}) while being read; raise limits.wasm.maxModuleFileBytes if needed`,
    );
  }
  return bytes;
}

function projectedCommandImageBytes(command) {
  const name = path.posix.basename(String(command));
  if (!name || name === '.' || name === '/') return null;
  for (const mapping of GUEST_PATH_MAPPINGS) {
    if (
      !/^\/__agentos\/commands\/\d+$/u.test(mapping?.guestPath ?? '') &&
      mapping?.guestPath !== '/opt/agentos/bin'
    ) continue;
    const guestCandidate = path.posix.join(mapping.guestPath, name);
    try {
      const bytes = readExecutablePathBytes(guestCandidate);
      traceHostProcess('projected-command-image', {
        command,
        guestCandidate,
        byteLength: bytes.byteLength,
        magic: Array.from(bytes.subarray(0, 4)),
      });
      return bytes;
    } catch (error) {
      if (error?.code !== 'ENOENT') throw error;
    }
  }
  return null;
}

function parseLinuxShebang(bytes) {
  if (bytes.byteLength < 2 || bytes[0] !== 0x23 || bytes[1] !== 0x21) {
    return null;
  }

  const header = bytes.subarray(2, Math.min(bytes.byteLength, LINUX_BINPRM_BUF_SIZE));
  const newline = header.indexOf(0x0a);
  let line = Buffer.from(newline >= 0 ? header.subarray(0, newline) : header).toString('utf8');
  line = line.replace(/[\t ]+$/u, '');
  const first = line.search(/[^\t ]/u);
  if (first < 0) {
    throw execError('ENOEXEC', 'shebang does not name an interpreter');
  }
  line = line.slice(first);
  const separator = line.search(/[\t ]/u);
  if (newline < 0 && bytes.byteLength >= LINUX_BINPRM_BUF_SIZE && separator < 0) {
    throw execError('ENOEXEC', 'shebang interpreter path exceeds the Linux header limit');
  }
  if (separator < 0) {
    return { interpreter: line, optionalArgument: null };
  }
  const interpreter = line.slice(0, separator);
  const optionalArgument = line.slice(separator).replace(/^[\t ]+|[\t ]+$/gu, '');
  return {
    interpreter,
    optionalArgument: optionalArgument.length > 0 ? optionalArgument : null,
  };
}

function compileResolvedExecImage(bytes, subject, argv) {
  try {
    const binary = enforceMemoryLimit(bytes, maxMemoryPages);
    return { module: new WebAssembly.Module(binary), argv };
  } catch (error) {
    if (
      error instanceof WebAssembly.CompileError ||
      error?.message === 'module is not a valid WebAssembly binary'
    ) {
      throw execError('ENOEXEC', `${subject} is not a supported WebAssembly executable image`);
    }
    throw error;
  }
}

function compileExecImage(bytes, subject, argv, interpreterDepth = 0) {
  const shebang = parseLinuxShebang(bytes);
  if (shebang) {
    if (interpreterDepth >= LINUX_MAX_INTERPRETER_DEPTH) {
      throw execError('ELOOP', `interpreter recursion for ${subject} exceeds the Linux limit`);
    }
    const interpreterArgv = [
      shebang.interpreter,
      ...(shebang.optionalArgument === null ? [] : [shebang.optionalArgument]),
      String(subject),
      ...argv.slice(1),
    ];
    return loadExecImageFromPath(
      shebang.interpreter,
      interpreterArgv,
      interpreterDepth + 1,
    );
  }

  return compileResolvedExecImage(bytes, subject, argv);
}

function loadExecImageFromPath(command, argv, interpreterDepth = 0) {
  if (SIDECAR_MANAGED_PROCESS) {
    const subject = String(command);
    const image = callSyncRpc('process.exec_image_open', [subject, argv]);
    const handle = String(image?.handle ?? '');
    let bytes;
    try {
      const size = Number(image?.size);
      if (!Number.isSafeInteger(size) || size < 0) {
        throw execError('EFBIG', `${subject} has an invalid executable image size`);
      }
      if (size > maxModuleFileBytes) {
        throw execError(
          'EFBIG',
          `${subject} is ${size} bytes, exceeding limits.wasm.maxModuleFileBytes (${maxModuleFileBytes}); raise limits.wasm.maxModuleFileBytes if needed`,
        );
      }
      bytes = Buffer.alloc(size);
      let offset = 0;
      while (offset < size) {
        const requested = boundedWasmSyncRpcReadLength(size - offset);
        const chunk = Buffer.from(callSyncRpc('process.exec_image_read', [
          handle,
          String(offset),
          requested,
        ]) ?? []);
        if (chunk.byteLength === 0 || chunk.byteLength > requested) {
          throw execError('EIO', `${subject} changed while its executable image was read`);
        }
        chunk.copy(bytes, offset);
        offset += chunk.byteLength;
      }
    } finally {
      if (handle.length > 0) {
        callSyncRpc('process.exec_image_close', [handle]);
      }
    }
    if (!Array.isArray(image?.argv) || image.argv.some((value) => typeof value !== 'string')) {
      throw execError('EIO', `${subject} resolution did not return a valid argv`);
    }
    traceHostProcess('exec-image-bytes', {
      command,
      byteLength: bytes.byteLength,
      magic: Array.from(bytes.subarray(0, Math.min(16, bytes.byteLength))),
    });
    return compileResolvedExecImage(bytes, subject, image.argv);
  }

  let bytes = readExecutablePathBytes(command);
  traceHostProcess('exec-image-bytes', {
    command,
    byteLength: bytes.byteLength,
    magic: Array.from(bytes.subarray(0, Math.min(16, bytes.byteLength))),
  });
  if (bytes.equals(INTERNAL_KERNEL_COMMAND_STUB)) {
    bytes = projectedCommandImageBytes(command);
    if (bytes === null) {
      throw execError('ENOENT', `registered command image for ${command} is unavailable`);
    }
  }
  return compileExecImage(
    bytes,
    String(command),
    argv,
    interpreterDepth,
  );
}

function executableTargetForHandle(handle) {
  if (handle?.kind === 'guest-file' && typeof handle.targetFd === 'number') {
    return handle.targetFd;
  }
  if (handle?.kind === 'kernel-fd' && typeof handle.targetFd === 'number') {
    return handle.targetFd;
  }
  if (handle?.kind === 'passthrough' && typeof handle.ioFd === 'number') {
    return handle.ioFd;
  }
  return null;
}

function loadExecImageFromFd(fd, argv, closeFds) {
  const descriptor = Number(fd) >>> 0;
  const scriptRef = `/proc/self/fd/${descriptor}`;
  if (SIDECAR_MANAGED_PROCESS) {
    const image = callSyncRpc('process.exec_image_open_fd', [
      canonicalKernelFdForSpawnAction(descriptor),
      argv,
      kernelCloexecFdsForCommit(closeFds),
    ]);
    const imageHandle = String(image?.handle ?? '');
    let bytes;
    try {
      const size = Number(image?.size);
      if (!Number.isSafeInteger(size) || size < 0) {
        throw execError('EFBIG', `${scriptRef} has an invalid executable image size`);
      }
      if (size > maxModuleFileBytes) {
        throw execError(
          'EFBIG',
          `${scriptRef} is ${size} bytes, exceeding limits.wasm.maxModuleFileBytes (${maxModuleFileBytes}); raise limits.wasm.maxModuleFileBytes if needed`,
        );
      }
      bytes = Buffer.alloc(size);
      let offset = 0;
      while (offset < size) {
        const requested = boundedWasmSyncRpcReadLength(size - offset);
        const chunk = Buffer.from(callSyncRpc('process.exec_image_read', [
          imageHandle,
          String(offset),
          requested,
        ]) ?? []);
        if (chunk.byteLength === 0 || chunk.byteLength > requested) {
          throw execError('EIO', `${scriptRef} changed while its executable image was read`);
        }
        chunk.copy(bytes, offset);
        offset += chunk.byteLength;
      }
    } finally {
      if (imageHandle.length > 0) {
        callSyncRpc('process.exec_image_close', [imageHandle]);
      }
    }
    if (!Array.isArray(image?.argv) || image.argv.some((value) => typeof value !== 'string')) {
      throw execError('EIO', `${scriptRef} resolution did not return a valid argv`);
    }
    return {
      ...compileExecImage(bytes, scriptRef, image.argv),
      scriptRef,
    };
  }

  const handle = lookupFdHandle(descriptor);
  const targetFd = executableTargetForHandle(handle);
  if (targetFd === null) {
    throw execError('EBADF', `fexecve descriptor ${descriptor} is not an open file`);
  }
  const bytes = readExecutableFdBytes(
    targetFd,
    fsModule.fstatSync(targetFd),
    scriptRef,
    isProjectedCommandGuestPath(handle?.guestPath),
  );
  if (parseLinuxShebang(bytes) && closeFds.includes(descriptor)) {
    // Linux cannot hand a close-on-exec script descriptor to its interpreter,
    // which subsequently opens the generated /proc/self/fd path.
    throw execError('ENOENT', `${scriptRef} will be closed before its interpreter opens it`);
  }
  return {
    ...compileExecImage(bytes, scriptRef, argv),
    scriptRef,
  };
}

function traceHostProcess(event, details) {
  const enabled =
    (typeof TRACE_HOST_PROCESS === 'boolean' && TRACE_HOST_PROCESS) ||
    (typeof HOST_PROCESS_ENV !== 'undefined' &&
      HOST_PROCESS_ENV?.AGENTOS_TRACE_HOST_PROCESS === '1') ||
    (typeof process !== 'undefined' && process?.env?.AGENTOS_TRACE_HOST_PROCESS === '1');
  if (!enabled) {
    return;
  }
  try {
    process.stderr.write(`[agent-os-host-process] ${event} ${JSON.stringify(details)}\n`);
  } catch {
    // Ignore tracing failures.
  }
}

const WASI_RIGHT_FD_DATASYNC = 1n << 0n;
const WASI_RIGHT_FD_READ = 1n << 1n;
const WASI_RIGHT_FD_SEEK = 1n << 2n;
const WASI_RIGHT_FD_FDSTAT_SET_FLAGS = 1n << 3n;
const WASI_RIGHT_FD_SYNC = 1n << 4n;
const WASI_RIGHT_FD_TELL = 1n << 5n;
const WASI_RIGHT_FD_ADVISE = 1n << 7n;
const WASI_RIGHT_FD_ALLOCATE = 1n << 8n;
const WASI_RIGHT_PATH_CREATE_DIRECTORY = 1n << 9n;
const WASI_RIGHT_PATH_LINK_SOURCE = 1n << 10n;
const WASI_RIGHT_PATH_LINK_TARGET = 1n << 11n;
const WASI_RIGHT_PATH_CREATE_FILE = 1n << 12n;
const WASI_RIGHT_PATH_OPEN = 1n << 13n;
const WASI_RIGHT_FD_READDIR = 1n << 14n;
const WASI_RIGHT_PATH_READLINK = 1n << 15n;
const WASI_RIGHT_PATH_RENAME_SOURCE = 1n << 16n;
const WASI_RIGHT_PATH_RENAME_TARGET = 1n << 17n;
const WASI_RIGHT_PATH_FILESTAT_GET = 1n << 18n;
const WASI_RIGHT_PATH_FILESTAT_SET_SIZE = 1n << 19n;
const WASI_RIGHT_PATH_FILESTAT_SET_TIMES = 1n << 20n;
const WASI_RIGHT_FD_FILESTAT_GET = 1n << 21n;
const WASI_RIGHT_FD_FILESTAT_SET_SIZE = 1n << 22n;
const WASI_RIGHT_FD_FILESTAT_SET_TIMES = 1n << 23n;
const WASI_RIGHT_PATH_SYMLINK = 1n << 24n;
const WASI_RIGHT_PATH_REMOVE_DIRECTORY = 1n << 25n;
const WASI_RIGHT_PATH_UNLINK_FILE = 1n << 26n;
const WASI_RIGHT_POLL_FD_READWRITE = 1n << 27n;

const READ_ONLY_PREOPEN_RIGHTS_BASE =
  WASI_RIGHT_FD_READ |
  WASI_RIGHT_FD_SEEK |
  WASI_RIGHT_FD_FDSTAT_SET_FLAGS |
  WASI_RIGHT_FD_TELL |
  WASI_RIGHT_PATH_OPEN |
  WASI_RIGHT_FD_READDIR |
  WASI_RIGHT_PATH_READLINK |
  WASI_RIGHT_PATH_FILESTAT_GET |
  WASI_RIGHT_FD_FILESTAT_GET |
  WASI_RIGHT_POLL_FD_READWRITE;
const READ_ONLY_PREOPEN_RIGHTS_INHERITING =
  WASI_RIGHT_FD_READ |
  WASI_RIGHT_FD_SEEK |
  WASI_RIGHT_FD_FDSTAT_SET_FLAGS |
  WASI_RIGHT_FD_TELL |
  WASI_RIGHT_FD_FILESTAT_GET |
  WASI_RIGHT_POLL_FD_READWRITE;
const READ_WRITE_PREOPEN_RIGHTS_BASE =
  READ_ONLY_PREOPEN_RIGHTS_BASE |
  WASI_RIGHT_FD_DATASYNC |
  WASI_RIGHT_FD_SYNC |
  WASI_RIGHT_FD_WRITE |
  WASI_RIGHT_FD_ADVISE |
  WASI_RIGHT_FD_ALLOCATE |
  WASI_RIGHT_PATH_CREATE_DIRECTORY |
  WASI_RIGHT_PATH_CREATE_FILE |
  WASI_RIGHT_PATH_FILESTAT_SET_SIZE |
  WASI_RIGHT_PATH_FILESTAT_SET_TIMES |
  WASI_RIGHT_FD_FILESTAT_SET_SIZE |
  WASI_RIGHT_FD_FILESTAT_SET_TIMES;
const READ_WRITE_PREOPEN_RIGHTS_INHERITING = READ_WRITE_PREOPEN_RIGHTS_BASE;
const FULL_PREOPEN_RIGHTS_BASE =
  READ_WRITE_PREOPEN_RIGHTS_BASE |
  WASI_RIGHT_PATH_LINK_SOURCE |
  WASI_RIGHT_PATH_LINK_TARGET |
  WASI_RIGHT_PATH_RENAME_SOURCE |
  WASI_RIGHT_PATH_RENAME_TARGET |
  WASI_RIGHT_PATH_SYMLINK |
  WASI_RIGHT_PATH_REMOVE_DIRECTORY |
  WASI_RIGHT_PATH_UNLINK_FILE;
const FULL_PREOPEN_RIGHTS_INHERITING = READ_WRITE_PREOPEN_RIGHTS_INHERITING;

function buildPreopenRights() {
  switch (permissionTier) {
    case 'read-only':
      return {
        rightsBase: READ_ONLY_PREOPEN_RIGHTS_BASE,
        rightsInheriting: READ_ONLY_PREOPEN_RIGHTS_INHERITING,
      };
    case 'read-write':
      return {
        rightsBase: READ_WRITE_PREOPEN_RIGHTS_BASE,
        rightsInheriting: READ_WRITE_PREOPEN_RIGHTS_INHERITING,
      };
    case 'full':
    default:
      return {
        rightsBase: FULL_PREOPEN_RIGHTS_BASE,
        rightsInheriting: FULL_PREOPEN_RIGHTS_INHERITING,
      };
  }
}

function createPreopen(hostPath, readOnly = false) {
  const rights =
    readOnly === true
      ? {
          rightsBase: READ_ONLY_PREOPEN_RIGHTS_BASE,
          rightsInheriting: READ_ONLY_PREOPEN_RIGHTS_INHERITING,
        }
      : buildPreopenRights();
  return {
    hostPath,
    readOnly: readOnly === true,
    rightsBase: rights.rightsBase,
    rightsInheriting: rights.rightsInheriting,
  };
}

function mappingContainsGuestPath(mapping, guestPath) {
  if (!mapping || typeof mapping.guestPath !== 'string' || typeof guestPath !== 'string') {
    return false;
  }
  const normalized = path.posix.normalize(guestPath);
  return (
    normalized === mapping.guestPath ||
    mapping.guestPath === '/' ||
    normalized.startsWith(`${mapping.guestPath}/`)
  );
}

function mappingContainsHostPath(mapping, hostPath) {
  if (!mapping || typeof mapping.hostPath !== 'string' || typeof hostPath !== 'string') {
    return false;
  }
  const normalized = path.resolve(hostPath);
  const root = path.resolve(mapping.hostPath);
  return normalized === root || normalized.startsWith(`${root}${path.sep}`);
}

function readOnlyForCwd(guestCwd) {
  for (const mapping of GUEST_PATH_MAPPINGS) {
    if (
      mapping?.readOnly === true &&
      (mappingContainsGuestPath(mapping, guestCwd) ||
        mappingContainsHostPath(mapping, HOST_CWD))
    ) {
      return true;
    }
  }
  return false;
}

function buildPreopens() {
  switch (permissionTier) {
    case 'isolated':
      return {};
    case 'read-only':
    case 'read-write':
    case 'full':
    default:
      const guestCwd =
        typeof guestEnv?.PWD === 'string' && guestEnv.PWD.startsWith('/')
          ? path.posix.normalize(guestEnv.PWD)
          : typeof process.env.PWD === 'string' && process.env.PWD.startsWith('/')
            ? path.posix.normalize(process.env.PWD)
            : null;
      const preopens = {};
      const seen = new Set();
      const cwdReadOnly = readOnlyForCwd(guestCwd);
      const cwdMount = guestCwd || '/workspace';
      preopens[cwdMount] = createPreopen(HOST_CWD, cwdReadOnly);
      seen.add(cwdMount);
      const rootMapping = GUEST_PATH_MAPPINGS.find(
        (mapping) => mapping && mapping.guestPath === '/',
      );
      if (rootMapping && !seen.has('/')) {
        preopens['/'] = createPreopen(rootMapping.hostPath, rootMapping.readOnly);
        seen.add('/');
      }
      for (const mapping of GUEST_PATH_MAPPINGS) {
        if (!mapping || typeof mapping.guestPath !== 'string' || typeof mapping.hostPath !== 'string') {
          continue;
        }
        const guestPath = path.posix.normalize(mapping.guestPath);
        if (
          !path.posix.isAbsolute(guestPath) ||
          seen.has(guestPath) ||
          guestPath === guestCwd
        ) {
          continue;
        }
        preopens[guestPath] = createPreopen(mapping.hostPath, mapping.readOnly);
        seen.add(guestPath);
      }
      if (cwdMount !== '/workspace' && !seen.has('/workspace')) {
        preopens['/workspace'] = createPreopen(HOST_CWD, cwdReadOnly);
        seen.add('/workspace');
      }
      return preopens;
  }
}

function readVarUint(bytes, offset, label) {
  let value = 0;
  let shift = 0;
  let cursor = offset;
  for (let count = 0; count < 10; count += 1) {
    if (cursor >= bytes.length) {
      throw new Error(`WebAssembly ${label} truncated`);
    }
    const byte = bytes[cursor];
    cursor += 1;
    value += (byte & 0x7f) * 2 ** shift;
    if ((byte & 0x80) === 0) {
      return { value, offset: cursor };
    }
    shift += 7;
  }
  throw new Error(`WebAssembly ${label} exceeds varuint limit`);
}

function encodeVarUint(value) {
  const encoded = [];
  let remaining = Math.trunc(value);
  do {
    let byte = remaining & 0x7f;
    remaining = Math.floor(remaining / 128);
    if (remaining > 0) {
      byte |= 0x80;
    }
    encoded.push(byte);
  } while (remaining > 0);
  return encoded;
}

function appendBytes(out, bytes) {
  for (let i = 0; i < bytes.length; i += 1) {
    out.push(bytes[i]);
  }
}

function rewriteMemorySection(sectionBytes, limitPages) {
  let offset = 0;
  const countResult = readVarUint(sectionBytes, offset, 'memory count');
  const count = countResult.value;
  offset = countResult.offset;
  const rewritten = [];
  appendBytes(rewritten, encodeVarUint(count));

  for (let index = 0; index < count; index += 1) {
    const flagsResult = readVarUint(sectionBytes, offset, 'memory flags');
    const flags = flagsResult.value;
    offset = flagsResult.offset;

    if ((flags & ~1) !== 0) {
      throw new Error(
        `configured WebAssembly memory limit does not support memory flags ${flags}`,
      );
    }

    const initialResult = readVarUint(sectionBytes, offset, 'memory minimum');
    const initialPages = initialResult.value;
    offset = initialResult.offset;

    let maximumPages = null;
    if ((flags & 1) !== 0) {
      const maximumResult = readVarUint(sectionBytes, offset, 'memory maximum');
      maximumPages = maximumResult.value;
      offset = maximumResult.offset;
    }

    if (initialPages > limitPages) {
      throw new Error(
        `initial WebAssembly memory of ${initialPages * WASM_PAGE_BYTES} bytes exceeds the configured limit of ${limitPages * WASM_PAGE_BYTES} bytes`,
      );
    }

    const cappedMaximumPages =
      maximumPages == null ? limitPages : Math.min(maximumPages, limitPages);
    appendBytes(rewritten, encodeVarUint(1));
    appendBytes(rewritten, encodeVarUint(initialPages));
    appendBytes(rewritten, encodeVarUint(cappedMaximumPages));
  }

  if (offset !== sectionBytes.length) {
    throw new Error('memory section parsing did not consume the full section');
  }

  return rewritten;
}

function enforceMemoryLimit(moduleBytes, limitPages) {
  if (!Number.isInteger(limitPages)) {
    return moduleBytes;
  }

  const bytes = moduleBytes instanceof Uint8Array ? moduleBytes : new Uint8Array(moduleBytes);
  if (bytes.length < 8 || bytes[0] !== 0 || bytes[1] !== 0x61 || bytes[2] !== 0x73 || bytes[3] !== 0x6d) {
    throw new Error('module is not a valid WebAssembly binary');
  }

  const rewritten = Array.from(bytes.slice(0, 8));
  let offset = 8;

  while (offset < bytes.length) {
    const sectionStart = offset;
    const sectionId = bytes[offset];
    offset += 1;
    const sectionSizeResult = readVarUint(bytes, offset, 'section size');
    const sectionSize = sectionSizeResult.value;
    offset = sectionSizeResult.offset;
    const sectionEnd = offset + sectionSize;
    if (sectionEnd > bytes.length) {
      throw new Error('section extends past end of module');
    }

    if (sectionId !== 5) {
      appendBytes(rewritten, bytes.slice(sectionStart, sectionEnd));
      offset = sectionEnd;
      continue;
    }

    const rewrittenSection = rewriteMemorySection(bytes.slice(offset, sectionEnd), limitPages);
    rewritten.push(sectionId);
    appendBytes(rewritten, encodeVarUint(rewrittenSection.length));
    appendBytes(rewritten, rewrittenSection);
    offset = sectionEnd;
  }

  return Buffer.from(rewritten);
}

function decodeBase64ToUint8Array(value) {
  return Buffer.from(value, 'base64');
}

function decodeFsBytesPayload(value, label) {
  if (Buffer.isBuffer(value)) return Buffer.from(value);
  if (ArrayBuffer.isView(value)) {
    return Buffer.from(value.buffer, value.byteOffset, value.byteLength);
  }
  if (value instanceof ArrayBuffer) return Buffer.from(value);
  if (Array.isArray(value) && value.every((byte) => Number.isInteger(byte) && byte >= 0 && byte <= 255)) {
    return Buffer.from(value);
  }
  if (value && typeof value === 'object') {
    if (value.__agentOSType === 'bytes' && typeof value.base64 === 'string') {
      return Buffer.from(value.base64, 'base64');
    }
    if (value.__type === 'Buffer' && typeof value.data === 'string') {
      return Buffer.from(value.data, 'base64');
    }
    if (typeof value.base64 === 'string') return Buffer.from(value.base64, 'base64');
  }
  const error = new Error(`${label} is not a byte payload`);
  error.code = 'EIO';
  throw error;
}

// Live kernel-PTY probe for any guest fd. Rust's
// `stdin().is_terminal()` (and wasi-libc `isatty`) ask `fd_fdstat_get` for a
// CHARACTER_DEVICE filetype; the runner-process fds are pipes, so a delegated
// answer hides the kernel PTY and interactive guests (e.g. brush's prompt)
// believe stdin is not a terminal. Ask the sidecar's kernel instead.
function stdioFdIsKernelTty(fd) {
  const descriptor = Number(fd) >>> 0;
  const handle = lookupFdHandle(descriptor);
  const kernelFd = handle?.kind === 'kernel-fd'
    ? Number(handle.targetFd) >>> 0
    : descriptor;
  // Even in kernel-stdio sync-RPC mode the answer must come from the kernel:
  // that mode is on for EVERY sidecar wasm execution, including piped
  // vm.exec() runs whose stdio is NOT a PTY. Hardcoding true here made
  // non-interactive shells think they had a terminal (fd_fdstat_get reported
  // CHARACTER_DEVICE, host_tty.isatty said 1), so they enabled raw mode and
  // the kernel's truthful "not a PTY end" refusal trapped the guest with
  // exit 1 after otherwise-successful commands.
  let isTty = false;
  try {
    isTty = callSyncRpc('__kernel_isatty', [kernelFd]) === true;
  } catch {
    isTty = false;
  }
  return isTty;
}

// Long event-driven in-RPC waits: the sidecar services __kernel_stdin_read /
// __kernel_poll by parking the RPC and replying when kernel poll state changes
// (reply-by-token), so a long wait costs no dispatch-loop time and near-zero
// CPU (the guest thread blocks in Atomics.wait inside callSync). Keep each
// slice under the 30s guest sync-RPC deadline.
const KERNEL_WAIT_SLICE_MS = 10_000;
const KERNEL_STDIN_WOULD_BLOCK = Symbol('kernel-stdin-would-block');
// Standalone descendants share this runner's cooperative child path. Managed
// descendants are scheduled and pumped independently by the sidecar.
const SPAWNED_CHILD_WAIT_SLICE_MS = 10;

function hasActiveSpawnedChildren() {
  if (SIDECAR_MANAGED_PROCESS) return false;
  for (const record of childCorrelationsByPid.values()) {
    if (record && !childBridgeTerminal(record)) {
      return true;
    }
  }
  return false;
}

function readKernelStdinChunk(maxBytes, nonblocking = false) {
  const requestedLength = Math.max(1, Number(maxBytes) >>> 0);
  while (true) {
    const response = callSyncRpc('__kernel_stdin_read', [
      requestedLength,
      nonblocking ? 0 : KERNEL_WAIT_SLICE_MS,
    ]);
    if (response && typeof response.dataBase64 === 'string') {
      return Buffer.from(response.dataBase64, 'base64');
    }
    if (response && response.done === true) {
      return null;
    }
    if (nonblocking) {
      return KERNEL_STDIN_WOULD_BLOCK;
    }
  }
}

const rawModuleBytes = globalThis.__agentOSWasmModuleBytes;
const moduleSource =
  rawModuleBytes instanceof Uint8Array
    ? Buffer.from(rawModuleBytes.buffer, rawModuleBytes.byteOffset, rawModuleBytes.byteLength)
    : typeof moduleBase64 === 'string' && moduleBase64.length > 0
    ? moduleBase64
    : fsModule.readFileSync(resolveModulePath(modulePath));
const moduleBytes =
  typeof moduleSource === 'string'
    ? __agentOSWasmMeasurePhase('decodeBase64ToUint8Array', () => decodeBase64ToUint8Array(moduleSource))
    : moduleSource;
const moduleBinary = __agentOSWasmMeasurePhase('enforceMemoryLimit', () => enforceMemoryLimit(moduleBytes, maxMemoryPages));
const module = __agentOSWasmMeasurePhase('WebAssembly.Module', () => new WebAssembly.Module(moduleBinary));

if (prewarmOnly) {
  __agentOSWasmEmitPhaseMetrics('prewarm');
  process.exit(0);
}

const WASI_PREOPEN_FD_BASE = 3;
// Patched wasi-libc tags descriptors returned by its absolute/cwd pathname
// resolver. The tag separates hidden WASI capability roots from the Linux
// guest descriptor namespace, where fd 3 is free to be closed or replaced.
const AGENTOS_HIDDEN_PREOPEN_FD_TAG = 0x40000000;
const AGENTOS_HIDDEN_PREOPEN_FD_MASK = 0x3fffffff;
const AMBIENT_WASI_PREOPENS = SIDECAR_MANAGED_PROCESS ? {} : buildPreopens();
const KERNEL_WASI_PREOPENS = SIDECAR_MANAGED_PROCESS
  ? callSyncRpc('process.fd_preopens', [])
  : null;
if (
  KERNEL_WASI_PREOPENS !== null &&
  (!Array.isArray(KERNEL_WASI_PREOPENS) ||
    KERNEL_WASI_PREOPENS.length > configuredMaxOpenFds)
) {
  throw new Error(
    `kernel WASI preopens exceed the ${configuredMaxOpenFds}-descriptor runtime limit`,
  );
}
const WASI_PREOPEN_ENTRIES = SIDECAR_MANAGED_PROCESS
  ? KERNEL_WASI_PREOPENS.map((entry, index) => {
      const kernelFd = Number(entry?.fd);
      const fd = WASI_PREOPEN_FD_BASE + index;
      const guestPath = entry?.guestPath;
      const rightsBase = Number(entry?.rightsBase);
      const rightsInheriting = Number(entry?.rightsInheriting);
      if (
        !Number.isSafeInteger(kernelFd) || kernelFd < WASI_PREOPEN_FD_BASE ||
        typeof guestPath !== 'string' || !path.posix.isAbsolute(guestPath) ||
        !Number.isSafeInteger(rightsBase) || rightsBase < 0 ||
        !Number.isSafeInteger(rightsInheriting) || rightsInheriting < 0
      ) {
        throw new Error('kernel returned invalid WASI preopen metadata');
      }
      return {
        fd,
        kernelFd,
        guestPath: path.posix.normalize(guestPath),
        preopenSpec: {
          readOnly: (BigInt(rightsBase) & WASI_RIGHT_FD_WRITE) === 0n,
          rightsBase: BigInt(rightsBase),
          rightsInheriting: BigInt(rightsInheriting),
        },
        kernelManaged: true,
      };
    })
  : Object.entries(AMBIENT_WASI_PREOPENS).map(
      ([guestPath, preopenSpec], index) => ({
        fd: WASI_PREOPEN_FD_BASE + index,
        kernelFd: WASI_PREOPEN_FD_BASE + index,
        guestPath,
        preopenSpec,
        kernelManaged: false,
      }),
    );
const hiddenPreopenHandles = new Map();

const wasi = new WASI({
  version: 'preview1',
  args: guestArgv,
  env: guestEnv,
  // Managed execution must never hand node:wasi an ambient host capability.
  // Its capability descriptors and metadata come from the sidecar kernel.
  preopens: AMBIENT_WASI_PREOPENS,
  returnOnExit: true,
});

let instanceMemory = null;
const wasiImport = { ...wasi.wasiImport };
// node:wasi omits sock_shutdown. Kernel-owned socketpair descriptors need a
// real half-close so readers observe Linux EOF; host-net transport teardown is
// still owned by net.destroy/fd_close.
if (typeof wasiImport.sock_shutdown !== 'function') {
  wasiImport.sock_shutdown = (fd, how) => {
    try {
      const numericFd = Number(fd) >>> 0;
      const handle = lookupFdHandle(numericFd);
      const kernelFd = handle?.kind === 'kernel-fd'
        ? Number(handle.targetFd) >>> 0
        : standaloneDelegateFdRefCounts.has(numericFd)
          ? numericFd
          : null;
      if (kernelFd == null) return WASI_ERRNO_SUCCESS;
      const numericHow = Number(how) >>> 0;
      const mode = numericHow === 1 ? 0 : numericHow === 2 ? 1 : numericHow === 3 ? 2 : null;
      if (mode == null) return WASI_ERRNO_INVAL;
      callSyncRpc('process.fd_socket_shutdown', [kernelFd, mode]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapHostProcessError(error);
    }
  };
}
if (typeof wasiImport.fd_allocate !== 'function') {
  wasiImport.fd_allocate = (fd, offset, length) =>
    mutateGuestFileRange(fd, offset, length, 'fs.fallocateSync');
}
const delegateClockTimeGet =
  typeof wasi.wasiImport.clock_time_get === 'function'
    ? wasi.wasiImport.clock_time_get.bind(wasi.wasiImport)
    : null;
const delegateClockResGet =
  typeof wasi.wasiImport.clock_res_get === 'function'
    ? wasi.wasiImport.clock_res_get.bind(wasi.wasiImport)
    : null;
const delegatePathOpen =
  typeof wasi.wasiImport.path_open === 'function'
    ? wasi.wasiImport.path_open.bind(wasi.wasiImport)
    : null;
const delegateFdWrite =
  typeof wasi.wasiImport.fd_write === 'function'
    ? wasi.wasiImport.fd_write.bind(wasi.wasiImport)
    : null;
const delegateFdPread =
  typeof wasi.wasiImport.fd_pread === 'function'
    ? wasi.wasiImport.fd_pread.bind(wasi.wasiImport)
    : null;
const delegateFdPwrite =
  typeof wasi.wasiImport.fd_pwrite === 'function'
    ? wasi.wasiImport.fd_pwrite.bind(wasi.wasiImport)
    : null;
const delegateFdSync =
  typeof wasi.wasiImport.fd_sync === 'function'
    ? wasi.wasiImport.fd_sync.bind(wasi.wasiImport)
    : null;
const delegateFdDatasync =
  typeof wasi.wasiImport.fd_datasync === 'function'
    ? wasi.wasiImport.fd_datasync.bind(wasi.wasiImport)
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

function encodeSignalMask(signals) {
  let lo = 0;
  let hi = 0;
  for (const signal of signals) {
    const numeric = Number(signal);
    if (numeric >= 1 && numeric <= 32) {
      lo = (lo | (1 << (numeric - 1))) >>> 0;
    } else if (numeric >= 33 && numeric <= 64) {
      hi = (hi | (1 << (numeric - 33))) >>> 0;
    }
  }
  return { lo, hi };
}

function spawnActionError(code, message) {
  const error = new Error(message);
  error.code = code;
  return error;
}

function spawnActionLimitError(message) {
  if (typeof process?.stderr?.write === 'function') {
    process.stderr.write(`[agentos] ${message}\n`);
  }
  return spawnActionError('E2BIG', message);
}

function warnNearSpawnActionLimit(kind, current, limit) {
  const countLimit = kind === 'actions';
  if ((countLimit ? warnedSpawnFileActions : warnedSpawnFileActionBytes) ||
      current < Math.ceil(limit * 0.9)) {
    return;
  }
  if (countLimit) warnedSpawnFileActions = true;
  else warnedSpawnFileActionBytes = true;
  const setting = countLimit
    ? 'limits.process.maxSpawnFileActions'
    : 'limits.process.maxSpawnFileActionBytes';
  if (typeof process?.stderr?.write === 'function') {
    process.stderr.write(
      `[agentos] posix_spawn file-action ${kind} near ${setting} ` +
      `(${current}/${limit}); raise ${setting} if needed\n`,
    );
  }
}

function canonicalKernelFdForSpawnAction(fd) {
  const numericFd = Number(fd) >>> 0;
  const handle = lookupFdHandle(numericFd);
  return handle?.kind === 'kernel-fd' ? Number(handle.targetFd) >>> 0 : numericFd;
}

function managedKernelFdForDuplicate(fd) {
  if (!SIDECAR_MANAGED_PROCESS) return null;
  const numericFd = Number(fd) >>> 0;
  const handle = lookupFdHandle(numericFd);
  if (handle?.kind === 'kernel-fd') {
    return Number(handle.targetFd) >>> 0;
  }
  // The managed kernel owns canonical stdin/stdout/stderr even though
  // node:wasi needs local passthrough handles for the initial 0/1/2 I/O.
  // Duplicating those descriptions must therefore allocate in the kernel and
  // project the result back into the guest descriptor namespace.
  if (
    numericFd <= 2 &&
    handle?.kind === 'passthrough' &&
    Number(handle.targetFd) === numericFd
  ) {
    return numericFd;
  }
  return null;
}

function kernelCloexecFdsForCommit(closeFds) {
  return closeFds.flatMap((fd) => {
    const handle = lookupFdHandle(fd);
    return handle?.kind === 'kernel-fd' ? [Number(handle.targetFd) >>> 0] : [];
  });
}

function decodeSpawnActions(actionsPtr, actionsLen, initialCwd) {
  const byteLength = Number(actionsLen) >>> 0;
  if (byteLength > maxSpawnFileActionBytes) {
    throw spawnActionLimitError(
      `posix_spawn file-action payload is ${byteLength} bytes, exceeding ` +
        `limits.process.maxSpawnFileActionBytes (${maxSpawnFileActionBytes}); ` +
        'raise limits.process.maxSpawnFileActionBytes if needed',
    );
  }
  warnNearSpawnActionLimit('bytes', byteLength, maxSpawnFileActionBytes);
  const bytes = readGuestBytes(actionsPtr, byteLength);
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let scanOffset = 0;
  let actionCount = 0;
  while (scanOffset < bytes.byteLength) {
    if (bytes.byteLength - scanOffset < 24) {
      throw spawnActionError('EINVAL', 'truncated posix_spawn action header');
    }
    const pathLength = view.getUint32(scanOffset + 20, true);
    scanOffset += 24;
    const pathEnd = scanOffset + pathLength;
    if (pathEnd < scanOffset || pathEnd > bytes.byteLength) {
      throw spawnActionError('EINVAL', 'truncated posix_spawn action path');
    }
    scanOffset = pathEnd;
    actionCount += 1;
    if (actionCount > maxSpawnFileActions) {
      throw spawnActionLimitError(
        `posix_spawn has ${actionCount} file actions, exceeding ` +
          `limits.process.maxSpawnFileActions (${maxSpawnFileActions}); ` +
          'raise limits.process.maxSpawnFileActions if needed',
      );
    }
    warnNearSpawnActionLimit('actions', actionCount, maxSpawnFileActions);
  }
  const stdio = [0, 1, 2];
  const closed = new Set();
  const actions = [];
  const actionFdPaths = new Map();
  const actionFdSources = new Map();
  let offset = 0;
  while (offset < bytes.byteLength) {
    if (bytes.byteLength - offset < 24) {
      throw spawnActionError('EINVAL', 'truncated posix_spawn action header');
    }
    const command = view.getUint32(offset, true);
    const fd = view.getInt32(offset + 4, true);
    const sourceFd = view.getInt32(offset + 8, true);
    const oflag = view.getInt32(offset + 12, true);
    const mode = view.getUint32(offset + 16, true);
    const pathLength = view.getUint32(offset + 20, true);
    offset += 24;
    const pathEnd = offset + pathLength;
    if (pathEnd < offset || pathEnd > bytes.byteLength) {
      throw spawnActionError('EINVAL', 'truncated posix_spawn action path');
    }
    const actionPath = bytes.subarray(offset, pathEnd).toString('utf8');
    offset = pathEnd;

    if (command === 1) {
      if (fd < 0) {
        throw spawnActionError('EBADF', `posix_spawn close has invalid fd ${fd}`);
      }
      if (
        fd > 2 &&
        !actionFdPaths.has(fd) &&
        !actionFdSources.has(fd) &&
        !lookupFdHandle(fd) &&
        !hasHostNetSocket(fd)
      ) {
        throw spawnActionError('EBADF', `posix_spawn close references unopened fd ${fd}`);
      }
      closed.add(fd);
      actionFdPaths.delete(fd);
      actionFdSources.delete(fd);
      if (fd <= 2) {
        stdio[fd] = 0xffffffff;
      }
      actions.push({
        command,
        guestFd: fd,
        fd: canonicalKernelFdForSpawnAction(fd),
        guestSourceFd: sourceFd,
        sourceFd,
        oflag,
        mode,
        path: '',
      });
      continue;
    }
    if (command === 2) {
      if (fd < 0 || sourceFd < 0 || closed.has(sourceFd)) {
        throw spawnActionError('EBADF', 'posix_spawn dup2 references a closed fd');
      }
      const source = sourceFd <= 2 ? stdio[sourceFd] : sourceFd;
      if (source === 0xffffffff) {
        throw spawnActionError('EBADF', 'posix_spawn dup2 references a closed fd');
      }
      if (
        sourceFd > 2 &&
        !actionFdPaths.has(sourceFd) &&
        !actionFdSources.has(sourceFd) &&
        !lookupFdHandle(sourceFd) &&
        !hasHostNetSocket(sourceFd)
      ) {
        throw spawnActionError(
          'EBADF',
          `posix_spawn dup2 references unopened fd ${sourceFd}`,
        );
      }
      if (fd <= 2) {
        stdio[fd] = source;
      }
      closed.delete(fd);
      if (actionFdPaths.has(sourceFd)) {
        actionFdPaths.set(fd, actionFdPaths.get(sourceFd));
        actionFdSources.delete(fd);
      } else if (actionFdSources.has(sourceFd)) {
        actionFdPaths.delete(fd);
        actionFdSources.set(fd, actionFdSources.get(sourceFd));
      } else {
        actionFdPaths.delete(fd);
        const sourceHandle = lookupFdHandle(sourceFd);
        if (sourceHandle?.kind !== 'kernel-fd' && typeof sourceHandle?.guestPath === 'string') {
          actionFdPaths.set(fd, sourceHandle.guestPath);
          actionFdSources.delete(fd);
        } else {
          actionFdSources.set(fd, canonicalKernelFdForSpawnAction(sourceFd));
        }
      }
      actions.push({
        command,
        guestFd: fd,
        fd: canonicalKernelFdForSpawnAction(fd),
        guestSourceFd: sourceFd,
        sourceFd: canonicalKernelFdForSpawnAction(sourceFd),
        oflag,
        mode,
        path: '',
      });
      continue;
    }
    if (command === 3) {
      if (fd < 0) {
        throw spawnActionError('EBADF', `posix_spawn open has invalid fd ${fd}`);
      }
      if (actionPath.length === 0) {
        throw spawnActionError('ENOENT', 'posix_spawn open path is empty');
      }
      // Keep the guest pathname raw. Earlier chdir/fchdir actions can change
      // its meaning through kernel symlinks, which only the sidecar can resolve.
      closed.delete(fd);
      actionFdPaths.set(fd, actionPath);
      actionFdSources.delete(fd);
      if (fd <= 2) {
        stdio[fd] = fd;
      }
      actions.push({
        command,
        guestFd: fd,
        fd,
        guestSourceFd: sourceFd,
        sourceFd,
        oflag,
        mode,
        path: actionPath,
      });
      continue;
    }
    if (command === 4) {
      if (actionPath.length === 0) {
        throw spawnActionError('ENOENT', 'posix_spawn chdir path is empty');
      }
      actions.push({ command, fd, sourceFd, oflag, mode, path: actionPath });
      continue;
    }
    if (command === 5) {
      if (fd < 0 || closed.has(fd)) {
        throw spawnActionError('EBADF', `posix_spawn fchdir has invalid fd ${fd}`);
      }
      if (
        !actionFdPaths.has(fd) &&
        !actionFdSources.has(fd) &&
        !lookupFdHandle(fd) &&
        !hasHostNetSocket(fd)
      ) {
        throw spawnActionError('EBADF', `posix_spawn fchdir references unopened fd ${fd}`);
      }
      actions.push({
        command,
        guestFd: fd,
        fd: canonicalKernelFdForSpawnAction(fd),
        sourceFd,
        oflag,
        mode,
        path: actionPath,
      });
      continue;
    }
    if (command === 6) {
      if (fd < 0) {
        throw spawnActionError('EBADF', `posix_spawn closefrom has invalid fd ${fd}`);
      }
      // Snapshot the parent's live inheritable descriptors. Retained handles
      // owned only by older children and Node-WASI's private backing table are
      // deliberately excluded: neither is visible in the parent's fd table.
      const inheritedGuestFds = new Set([
        ...activeFdProjections.keys(),
        ...passthroughHandles.keys(),
        ...standaloneDelegateFdRefCounts.keys(),
        ...hostNetGuestFds(),
        ...actionFdPaths.keys(),
        ...actionFdSources.keys(),
        ...WASI_PREOPEN_ENTRIES.map((entry) => entry.fd),
      ]);
      for (const guestFd of inheritedGuestFds) {
        if (guestFd >= fd) {
          closed.add(guestFd);
          actionFdPaths.delete(guestFd);
          actionFdSources.delete(guestFd);
        }
      }
      for (let stdioFd = Math.max(fd, 0); stdioFd <= 2; stdioFd += 1) {
        closed.add(stdioFd);
        stdio[stdioFd] = 0xffffffff;
      }
      actions.push({
        command,
        guestFd: fd,
        fd,
        guestSourceFd: sourceFd,
        sourceFd,
        oflag,
        mode,
        path: '',
        closeFromGuestFds: [...inheritedGuestFds]
          .filter((guestFd) => guestFd >= fd)
          .sort((left, right) => left - right),
      });
      continue;
    }
    throw spawnActionError('EINVAL', `unknown posix_spawn action opcode ${command}`);
  }
  return { stdio, cwd: initialCwd, actions };
}

function spawnActionsControlGuestFd(actions, fd) {
  const guestFd = Number(fd) >>> 0;
  return (actions ?? []).some(
    (action) => {
      const command = Number(action?.command);
      const actionGuestFd = Number(action?.guestFd ?? action?.fd) >>> 0;
      return (
        ([1, 2, 3].includes(command) && actionGuestFd === guestFd) ||
        (command === 6 && guestFd >= actionGuestFd)
      );
    },
  );
}

function parseControlPipeFd(value) {
  if (typeof value !== 'string' || value.trim() === '') {
    return null;
  }

  const parsed = Number.parseInt(value, 10);
  return Number.isInteger(parsed) && parsed >= 3 ? parsed : null;
}

function emitControlMessage(message) {
  const emitSignalStateFallback = () => {
    if (
      message?.type === 'signal_state' &&
      typeof process?.stdout?.write === 'function'
    ) {
      try {
        process.stdout.write(`__AGENTOS_WASM_SIGNAL_STATE__:${JSON.stringify(message)}\n`);
      } catch {
        // Ignore signal-state bridge failures during teardown.
      }
    }
  };

  if (CONTROL_PIPE_FD == null) {
    emitSignalStateFallback();
    return;
  }

  try {
    writeSync(CONTROL_PIPE_FD, `${JSON.stringify(message)}\n`);
  } catch {
    emitSignalStateFallback();
  }
}

function isWorkspaceReadOnly() {
  return permissionTier === 'read-only' || permissionTier === 'isolated';
}

function hasWriteRights(rights) {
  try {
    return (BigInt(rights) & WASI_RIGHT_FD_WRITE) !== 0n;
  } catch {
    return true;
  }
}

function hasReadRights(rights) {
  try {
    return (BigInt(rights) & WASI_RIGHT_FD_READ) !== 0n;
  } catch {
    return true;
  }
}

function hasMutationOpenFlags(oflags) {
  const normalized = Number(oflags) >>> 0;
  return (
    (normalized & WASI_OFLAGS_CREAT) !== 0 ||
    (normalized & WASI_OFLAGS_EXCL) !== 0 ||
    (normalized & WASI_OFLAGS_TRUNC) !== 0
  );
}

function kernelOpenFlagsFromWasi(oflags, rightsBase, fdflags, lookupflags, direct = false) {
  const wantsRead = hasReadRights(rightsBase);
  const wantsWrite = hasWriteRights(rightsBase);
  let flags = wantsWrite ? (wantsRead ? KERNEL_O_RDWR : KERNEL_O_WRONLY) : 0;
  const normalizedOflags = Number(oflags) >>> 0;
  const normalizedFdflags = Number(fdflags) >>> 0;
  if ((normalizedOflags & WASI_OFLAGS_CREAT) !== 0) flags |= KERNEL_O_CREAT;
  if ((normalizedOflags & WASI_OFLAGS_DIRECTORY) !== 0) flags |= KERNEL_O_DIRECTORY;
  if ((normalizedOflags & WASI_OFLAGS_EXCL) !== 0) flags |= KERNEL_O_EXCL;
  if ((normalizedOflags & WASI_OFLAGS_TRUNC) !== 0) flags |= KERNEL_O_TRUNC;
  if ((normalizedFdflags & WASI_FDFLAGS_APPEND) !== 0) flags |= KERNEL_O_APPEND;
  if ((normalizedFdflags & WASI_FDFLAGS_NONBLOCK) !== 0) flags |= KERNEL_O_NONBLOCK;
  if (direct) flags |= KERNEL_O_DIRECT;
  if (((Number(lookupflags) >>> 0) & WASI_LOOKUPFLAGS_SYMLINK_FOLLOW) === 0) {
    flags |= KERNEL_O_NOFOLLOW;
  }
  return flags;
}

function errorHasCode(error, code) {
  return error != null && typeof error === 'object' && error.code === code;
}

function blockingIoDeadline(timeoutMs) {
  return Number.isFinite(timeoutMs) ? Date.now() + Math.max(0, Number(timeoutMs)) : null;
}

function throwBlockingFifoOpenTimeout(guestPath) {
  const error = new Error(`EAGAIN: FIFO open timed out for ${guestPath}`);
  error.code = 'EAGAIN';
  throw error;
}

function openBlockingGuestFifoForPathOpen(
  kernelDirFd,
  relativePath,
  guestPath,
  oflags,
  rightsBase,
  rightsInheriting,
  fdflags,
  lookupflags,
  openedFdPtr,
  mode,
  direct,
) {
  let stat;
  try {
    stat = fsModule.statSync(guestPath);
  } catch (error) {
    if (errorHasCode(error, 'ENOENT')) return null;
    throw error;
  }
  const isFifo = stat?.isFIFO?.() === true ||
    (Number(stat?.mode ?? 0) & 0xf000) === 0x1000;
  const flags = kernelOpenFlagsFromWasi(oflags, rightsBase, fdflags, lookupflags, direct);
  if (
    !isFifo ||
    (Number(fdflags) & WASI_FDFLAGS_NONBLOCK) !== 0 ||
    (flags & 0b11) === KERNEL_O_RDWR
  ) {
    return null;
  }

  const timeoutMs = callSyncRpc('fs.blockingIoTimeoutMsSync', []);
  const deadline = blockingIoDeadline(timeoutMs);
  const nonblockingFlags = flags | KERNEL_O_NONBLOCK;
  const accessMode = flags & 0b11;
  let kernelFd = null;

  for (;;) {
    if (kernelFd === null) {
      try {
        kernelFd = Number(callSyncRpc('process.path_open_at', [
          kernelDirFd,
          relativePath,
          nonblockingFlags,
          mode,
          rightsBase.toString(),
          rightsInheriting.toString(),
        ])) >>> 0;
      } catch (error) {
        if (accessMode !== KERNEL_O_WRONLY || !errorHasCode(error, 'ENXIO')) {
          throw error;
        }
      }
    }
    if (
      kernelFd !== null &&
      callSyncRpc('fs.namedFifoPeerReadySync', [kernelFd]) === true
    ) {
      callSyncRpc('process.fd_set_flags', [kernelFd, flags]);
      break;
    }
    if (deadline !== null && Date.now() >= deadline) {
      if (kernelFd !== null) callSyncRpc('process.fd_close', [kernelFd]);
      throwBlockingFifoOpenTimeout(guestPath);
    }
    // Returning between short RPCs lets the sidecar pump sibling processes,
    // including the peer whose open completes this Linux blocking operation.
    Atomics.wait(syntheticWaitArray, 0, 0, 1);
  }
  const openedFd = registerKernelDelegateFd(kernelFd);
  const result = writeGuestUint32(openedFdPtr, openedFd);
  if (result !== WASI_ERRNO_SUCCESS) wasiImport.fd_close(openedFd);
  return result;
}

function denyReadOnlyMutation() {
  return WASI_ERRNO_ROFS;
}

function guestPathForPreopenKey(key) {
  if (key === '.') {
    return HOST_FS_GUEST_CWD;
  }
  return path.posix.normalize(key);
}

function resolvePathOpenGuestPath(fd, pathPtr, pathLen) {
  const target = readGuestString(pathPtr, pathLen);
  if (target.startsWith('/')) {
    return path.posix.normalize(target);
  }

  const handle = lookupFdHandle(fd);
  if (handle?.kind === 'kernel-fd' && SIDECAR_MANAGED_PROCESS) {
    try {
      const base = callSyncRpc('process.fd_chdir_path', [Number(handle.targetFd) >>> 0]);
      return path.posix.resolve(String(base), target);
    } catch {
      // The combined process.path_*_at RPC below returns the authoritative
      // ENOTDIR/EBADF. Path resolution is also used by policy probes, which
      // must not throw out of the WASI import and abort the entire runtime.
      return null;
    }
  }
  if (handle && typeof handle.guestPath === 'string') {
    return path.posix.resolve(handle.guestPath, target);
  }

  const numericFd = Number(fd) >>> 0;
  const preopenFd =
    (numericFd & AGENTOS_HIDDEN_PREOPEN_FD_TAG) !== 0
      ? numericFd & AGENTOS_HIDDEN_PREOPEN_FD_MASK
      : numericFd;
  const preopen = WASI_PREOPEN_ENTRIES.find((entry) => entry.fd === preopenFd);
  if (preopen) {
    return path.posix.resolve(guestPathForPreopenKey(preopen.guestPath), target);
  }

  return null;
}

function guestPathIsReadOnly(guestPath) {
  return GUEST_PATH_MAPPINGS.some(
    (mapping) => mapping?.readOnly === true && mappingContainsGuestPath(mapping, guestPath),
  );
}

function resolvedGuestPathIsReadOnly(fd, pathPtr, pathLen) {
  try {
    const guestPath = resolvePathOpenGuestPath(fd, pathPtr, pathLen);
    return typeof guestPath === 'string' && guestPathIsReadOnly(guestPath);
  } catch {
    return false;
  }
}

// Managed paths stay kernel-authoritative; standalone handles may retain a
// runner-local path because Node-WASI owns that fallback description.
function guestPathForManagedFd(fd) {
  const handle = lookupFdHandle(fd);
  if (handle?.kind === 'kernel-fd' && SIDECAR_MANAGED_PROCESS) {
    try {
      return String(callSyncRpc('process.fd_path', [Number(handle.targetFd) >>> 0]));
    } catch {
      return null;
    }
  }
  if (typeof handle?.guestPath === 'string') {
    return handle.guestPath;
  }
  return null;
}

// WASI fstflags bits for *_filestat_set_times.
const WASI_FSTFLAGS_ATIM = 1;
const WASI_FSTFLAGS_ATIM_NOW = 2;
const WASI_FSTFLAGS_MTIM = 4;
const WASI_FSTFLAGS_MTIM_NOW = 8;
const WASI_LOOKUPFLAGS_SYMLINK_FOLLOW = 1;

// Resolve the (atime, mtime) seconds to apply for a *_filestat_set_times call:
// explicit nanosecond args, "now", or preserve-current (the OMIT case; node's
// utimes has no omit, so re-apply the current value from stat).
function resolveSetTimes(currentStat, atimNs, mtimNs, fstFlags) {
  const flags = Number(fstFlags) >>> 0;
  const nowSec = Date.now() / 1000;
  let atime = currentStat.atimeMs / 1000;
  let mtime = currentStat.mtimeMs / 1000;
  if (flags & WASI_FSTFLAGS_ATIM_NOW) {
    atime = nowSec;
  } else if (flags & WASI_FSTFLAGS_ATIM) {
    atime = Number(BigInt(atimNs)) / 1e9;
  }
  if (flags & WASI_FSTFLAGS_MTIM_NOW) {
    mtime = nowSec;
  } else if (flags & WASI_FSTFLAGS_MTIM) {
    mtime = Number(BigInt(mtimNs)) / 1e9;
  }
  return { atime, mtime };
}

// The embedded WASI shim omits the *_filestat_set_times ops entirely, which
// makes instantiation of any module importing them fail with a LinkError
// (vim uses path_filestat_set_times to restore file timestamps). Implement
// them against the bridge fs (the kernel VFS) when the base has no delegate.
if (typeof wasiImport.path_filestat_set_times !== 'function') {
  wasiImport.path_filestat_set_times = (fd, flags, pathPtr, pathLen, atimNs, mtimNs, fstFlags) => {
    try {
      const guestPath = resolvePathOpenGuestPath(fd, pathPtr, pathLen);
      if (typeof guestPath !== 'string') {
        return WASI_ERRNO_BADF;
      }
      const follow = (Number(flags) >>> 0) & WASI_LOOKUPFLAGS_SYMLINK_FOLLOW;
      const stat = follow ? fsModule.statSync(guestPath) : fsModule.lstatSync(guestPath);
      const { atime, mtime } = resolveSetTimes(stat, atimNs, mtimNs, fstFlags);
      if (follow || typeof fsModule.lutimesSync !== 'function') {
        fsModule.utimesSync(guestPath, atime, mtime);
      } else {
        fsModule.lutimesSync(guestPath, atime, mtime);
      }
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return error?.code === 'ENOENT' ? WASI_ERRNO_NOENT : WASI_ERRNO_INVAL;
    }
  };
}

if (typeof wasiImport.fd_filestat_set_times !== 'function') {
  wasiImport.fd_filestat_set_times = (fd, atimNs, mtimNs, fstFlags) => {
    try {
      const guestPath = guestPathForManagedFd(fd);
      if (typeof guestPath !== 'string') {
        return WASI_ERRNO_BADF;
      }
      const stat = fsModule.statSync(guestPath);
      const { atime, mtime } = resolveSetTimes(stat, atimNs, mtimNs, fstFlags);
      fsModule.utimesSync(guestPath, atime, mtime);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return error?.code === 'ENOENT' ? WASI_ERRNO_NOENT : WASI_ERRNO_INVAL;
    }
  };
}

function pathOpenMayCreateTarget(oflags, rightsBase, fdflags) {
  const normalizedOflags = Number(oflags) >>> 0;
  const normalizedFdflags = Number(fdflags) >>> 0;
  return (
    (normalizedOflags & WASI_OFLAGS_CREAT) !== 0 ||
    ((normalizedFdflags & WASI_FDFLAGS_APPEND) !== 0 && hasWriteRights(rightsBase))
  );
}

function precreatePathOpenTarget(fd, pathPtr, pathLen, oflags, rightsBase, fdflags) {
  if (!pathOpenMayCreateTarget(oflags, rightsBase, fdflags)) {
    return null;
  }

  const guestPath = resolvePathOpenGuestPath(fd, pathPtr, pathLen);
  if (typeof guestPath !== 'string') {
    return null;
  }

  if (!fsModule.existsSync(guestPath)) {
    fsModule.writeFileSync(guestPath, Buffer.alloc(0));
  }
  return guestPath;
}

function fsOpenFlagForPathOpen(oflags, rightsBase, fdflags) {
  const normalizedOflags = Number(oflags) >>> 0;
  const normalizedFdflags = Number(fdflags) >>> 0;
  const wantsRead = hasReadRights(rightsBase);
  const wantsWrite = hasWriteRights(rightsBase);
  const wantsExclusive = (normalizedOflags & WASI_OFLAGS_EXCL) !== 0;
  const wantsAppend = (normalizedFdflags & WASI_FDFLAGS_APPEND) !== 0;
  const wantsTruncate = (normalizedOflags & WASI_OFLAGS_TRUNC) !== 0;

  if (!wantsWrite) {
    return 'r';
  }

  if (wantsAppend) {
    if (wantsExclusive) {
      return wantsRead ? 'ax+' : 'ax';
    }
    return wantsRead ? 'a+' : 'a';
  }

  if (wantsTruncate) {
    if (wantsExclusive) {
      return wantsRead ? 'wx+' : 'wx';
    }
    return wantsRead ? 'w+' : 'w';
  }

  return 'r+';
}

function runnerFdMappingInUse(fd) {
  return (
    pendingInitialKernelGuestFds.has(fd) ||
    activeFdProjections.has(fd) ||
    passthroughHandles.has(fd) ||
    retainedSpawnOutputHandlesByFd.has(fd) ||
    retainedSyntheticHandlesByDisplayFd.has(fd) ||
    standaloneDelegateFdRefCounts.has(fd) ||
    hasHostNetSocket(fd)
  );
}

function syntheticFdInUse(fd) {
  return runnerFdMappingInUse(fd) || wasi?.fdTable?.has?.(fd) === true;
}

function currentNofileSoftLimit() {
  if (!SIDECAR_MANAGED_PROCESS) return configuredMaxOpenFds;
  try {
    const limit = callSyncRpc('process.getrlimit', [7]);
    const soft = BigInt(limit.soft);
    if (soft > BigInt(Number.MAX_SAFE_INTEGER)) return LINUX_GUEST_FD_LIMIT;
    return Number(soft);
  } catch (error) {
    if (error?.code === 'ERR_AGENTOS_WASM_SYNC_RPC_UNAVAILABLE') {
      return configuredMaxOpenFds;
    }
    throw error;
  }
}

function allocateSyntheticFd(minFd = nextSyntheticFd, reservedCapacity = false) {
  if (!reservedCapacity && !hasRunnerOpenFdCapacity(1)) {
    return null;
  }
  const numericMinimum = Number(minFd);
  if (
    !Number.isSafeInteger(numericMinimum) || numericMinimum < 0 ||
    numericMinimum >= LINUX_GUEST_FD_LIMIT
  ) {
    return null;
  }
  const descriptorLimit = Math.min(LINUX_GUEST_FD_LIMIT, currentNofileSoftLimit());
  let fd = Math.max(FIRST_SYNTHETIC_FD, numericMinimum);
  while (
    fd < descriptorLimit &&
    (syntheticFdInUse(fd) || initialMappedGuestFds.has(fd))
  ) {
    fd += 1;
  }
  if (fd >= descriptorLimit) return null;
  nextSyntheticFd = fd + 1 < descriptorLimit ? fd + 1 : FIRST_SYNTHETIC_FD;
  return fd;
}

function allocateKernelGuestFd(minFd = 3) {
  if (!hasRunnerOpenFdCapacity(1)) return null;
  const numericMinimum = Number(minFd);
  if (
    !Number.isSafeInteger(numericMinimum) || numericMinimum < 0 ||
    numericMinimum >= LINUX_GUEST_FD_LIMIT
  ) {
    return null;
  }
  const descriptorLimit = Math.min(LINUX_GUEST_FD_LIMIT, currentNofileSoftLimit());
  let fd = Math.max(3, numericMinimum);
  while (
    fd < descriptorLimit &&
    (syntheticFdInUse(fd) || initialMappedGuestFds.has(fd))
  ) {
    fd += 1;
  }
  return fd < descriptorLimit ? fd : null;
}

function openGuestFileForPathOpen(fd, pathPtr, pathLen, oflags, rightsBase, fdflags, openedFdPtr) {
  if (SIDECAR_MANAGED_PROCESS) {
    throw execError('EIO', 'managed path_open must use a kernel file descriptor');
  }
  if (!pathOpenMayCreateTarget(oflags, rightsBase, fdflags)) {
    return null;
  }

  const normalizedOflags = Number(oflags) >>> 0;
  const normalizedFdflags = Number(fdflags) >>> 0;

  const guestPath = resolvePathOpenGuestPath(fd, pathPtr, pathLen);
  if (typeof guestPath !== 'string') {
    return null;
  }
  if (!hasRunnerOpenFdCapacity(1)) {
    return WASI_ERRNO_MFILE;
  }

  const append = (normalizedFdflags & WASI_FDFLAGS_APPEND) !== 0;
  const exclusive = (normalizedOflags & WASI_OFLAGS_EXCL) !== 0;
  const truncate = (normalizedOflags & WASI_OFLAGS_TRUNC) !== 0;
  if (!append && !exclusive && !truncate && !fsModule.existsSync(guestPath)) {
    fsModule.writeFileSync(guestPath, Buffer.alloc(0));
  }
  const targetFd = fsModule.openSync(
    guestPath,
    fsOpenFlagForPathOpen(oflags, rightsBase, fdflags),
    0o666,
  );
  const openedFd = allocateSyntheticFd(nextSyntheticFd, true);
  setActiveFdProjection(openedFd, {
    kind: 'guest-file',
    targetFd,
    displayFd: openedFd,
    refCount: 1,
    open: true,
    guestPath,
    position: append ? Number(fsModule.fstatSync(targetFd).size ?? 0) : 0,
    append,
  });
  return writeGuestUint32(openedFdPtr, openedFd);
}

function openProcSelfFdAlias(guestPath, oflags, rightsBase, lookupflags, openedFdPtr) {
  const match = /^\/(?:proc\/self\/fd|dev\/fd)\/(\d+)$/u.exec(String(guestPath));
  if (!match) {
    return null;
  }
  const sourceFd = Number(match[1]);
  if (!Number.isSafeInteger(sourceFd) || sourceFd < 0) {
    return WASI_ERRNO_NOENT;
  }
  const sourceHandle = lookupFdHandle(sourceFd);
  if (!sourceHandle) {
    return WASI_ERRNO_NOENT;
  }
  if (((Number(lookupflags) >>> 0) & WASI_LOOKUPFLAGS_SYMLINK_FOLLOW) === 0) {
    return WASI_ERRNO_LOOP;
  }
  if ((Number(oflags) & WASI_OFLAGS_DIRECTORY) !== 0) {
    return WASI_ERRNO_NOTDIR;
  }
  if (hasMutationOpenFlags(oflags) || hasWriteRights(rightsBase)) {
    return WASI_ERRNO_ACCES;
  }
  if (!hasRunnerOpenFdCapacity(1)) {
    return WASI_ERRNO_MFILE;
  }
  if (SIDECAR_MANAGED_PROCESS) {
    if (sourceHandle.kind !== 'kernel-fd') {
      return WASI_ERRNO_NOENT;
    }
    try {
      const openedFd = registerKernelDelegateFd(
        callSyncRpc('process.fd_dup', [Number(sourceHandle.targetFd) >>> 0]),
      );
      const writeResult = writeGuestUint32(openedFdPtr, openedFd);
      if (writeResult !== WASI_ERRNO_SUCCESS) {
        wasiImport.fd_close(openedFd);
      }
      return writeResult;
    } catch (error) {
      return mapHostProcessError(error);
    }
  }

  const targetFd = executableTargetForHandle(sourceHandle);
  if (targetFd === null) {
    return WASI_ERRNO_NOENT;
  }

  sourceHandle.refCount += 1;
  const openedFd = allocateSyntheticFd(nextSyntheticFd, true);
  setActiveFdProjection(openedFd, {
    kind: 'guest-file',
    targetFd,
    displayFd: openedFd,
    refCount: 1,
    open: true,
    guestPath: String(guestPath),
    position: 0,
    append: false,
    ownsTargetFd: false,
    backingHandle: sourceHandle,
  });
  return writeGuestUint32(openedFdPtr, openedFd);
}

function kernelProcFdPathForGuestPath(guestPath) {
  const match = /^(\/?)proc\/self\/fd\/(\d+)$/u.exec(String(guestPath));
  if (!match) return guestPath;
  const guestFd = Number(match[2]);
  if (!Number.isSafeInteger(guestFd) || guestFd < 0) return guestPath;
  const handle = lookupFdHandle(guestFd);
  return handle?.kind === 'kernel-fd'
    ? `${match[1]}proc/self/fd/${Number(handle.targetFd) >>> 0}`
    : guestPath;
}

function kernelOpenPathForGuestPath(guestPath) {
  const match = /^\/?(?:proc\/self\/fd|dev\/fd)\/(\d+)$/u.exec(String(guestPath));
  if (!match) return guestPath;
  const guestFd = Number(match[1]);
  if (!Number.isSafeInteger(guestFd) || guestFd < 0) return guestPath;
  const handle = lookupFdHandle(guestFd);
  // Keep the operand relative to its root capability. The kernel recognizes
  // the normalized /dev/fd path as an open-description alias and intersects
  // the requested rights with the backing descriptor's rights.
  return handle?.kind === 'kernel-fd'
    ? `dev/fd/${Number(handle.targetFd) >>> 0}`
    : guestPath;
}

function fsOpenNumericFlagsForManagedPath(rightsBase, fdflags) {
  const wantsRead = hasReadRights(rightsBase);
  const wantsWrite = hasWriteRights(rightsBase);
  let flags = wantsWrite ? (wantsRead ? 0o2 : 0o1) : 0;
  if ((Number(fdflags) & WASI_FDFLAGS_APPEND) !== 0) {
    flags |= 0o2000;
  }
  return flags;
}

function openStandalonePathIoFd(guestPath, rightsBase, fdflags) {
  if (SIDECAR_MANAGED_PROCESS) {
    throw execError('EIO', 'managed path I/O must use the kernel descriptor table');
  }
  if (typeof guestPath !== 'string' || guestPath === '/dev/null') {
    return null;
  }
  try {
    const hostPath = resolveHostFsPath(guestPath) ?? guestPath;
    return fsModule.openSync(
      hostPath,
      fsOpenNumericFlagsForManagedPath(rightsBase, fdflags),
      0o666,
    );
  } catch {
    return null;
  }
}

function retainPathOpenDelegateFd(openedFdPtr, guestPath, fdflags, rightsBase) {
  if (SIDECAR_MANAGED_PROCESS) {
    throw execError('EIO', 'managed path_open must not retain an ambient WASI descriptor');
  }
  if (!(instanceMemory instanceof WebAssembly.Memory)) {
    return WASI_ERRNO_SUCCESS;
  }

  try {
    const openedFd = new DataView(instanceMemory.buffer).getUint32(Number(openedFdPtr), true);
    let retainedFd = openedFd;
    if (openedFd > 2 && runnerFdMappingInUse(openedFd)) {
      if (typeof delegateManagedFdRenumber !== 'function') {
        return WASI_ERRNO_FAULT;
      }
      retainedFd = allocateSyntheticFd(openedFd + 1, true);
      const renumberResult = delegateManagedFdRenumber(openedFd, retainedFd);
      if (renumberResult !== WASI_ERRNO_SUCCESS) {
        return renumberResult;
      }
      const writeResult = writeGuestUint32(openedFdPtr, retainedFd);
      if (writeResult !== WASI_ERRNO_SUCCESS) {
        return writeResult;
      }
      traceHostProcess('path-open-delegate-renumber', {
        openedFd,
        retainedFd,
      });
    }
    const append = (Number(fdflags) & WASI_FDFLAGS_APPEND) !== 0;
    retainDelegateFd(retainedFd);
    if (retainedFd > 2 && !passthroughHandles.has(retainedFd)) {
      const ioFd = openStandalonePathIoFd(guestPath, rightsBase, fdflags);
      closedPassthroughFds.delete(retainedFd);
      passthroughHandles.set(retainedFd, {
        kind: 'passthrough',
        targetFd: retainedFd,
        ioFd,
        displayFd: retainedFd,
        refCount: 0,
        open: true,
        readOnly:
          typeof guestPath === 'string' &&
          resolveModuleGuestPathToHostMapping(guestPath)?.readOnly === true,
        append,
        position: append && typeof ioFd === 'number'
          ? Number(fsModule.fstatSync(ioFd).size ?? 0)
          : 0,
        ...(typeof guestPath === 'string' ? { guestPath } : {}),
      });
    }
    return WASI_ERRNO_SUCCESS;
  } catch {
    return WASI_ERRNO_FAULT;
  }
}

function writeGuestUint32(ptr, value) {
  if (!(instanceMemory instanceof WebAssembly.Memory)) {
    return WASI_ERRNO_FAULT;
  }

  try {
    new DataView(instanceMemory.buffer).setUint32(Number(ptr), Number(value) >>> 0, true);
    return WASI_ERRNO_SUCCESS;
  } catch {
    return WASI_ERRNO_FAULT;
  }
}

function guestRangeIsValid(ptr, length) {
  if (!(instanceMemory instanceof WebAssembly.Memory)) return false;
  const offset = Number(ptr);
  const byteLength = Number(length);
  return Number.isInteger(offset) && offset >= 0 &&
    Number.isInteger(byteLength) && byteLength >= 0 &&
    offset <= instanceMemory.buffer.byteLength - byteLength;
}

function guestRangesAreValid(...ranges) {
  return ranges.every(([ptr, length]) => guestRangeIsValid(ptr, length));
}

function validateGuestIovRequest(iovs, iovsLen) {
  const count = Number(iovsLen) >>> 0;
  const countErrno = checkFixedRequestLimit(
    'wasm.abi.maxIovecs',
    count,
    LINUX_IOV_MAX,
  );
  if (countErrno !== WASI_ERRNO_SUCCESS) {
    return { errno: countErrno, entries: [], totalLength: 0 };
  }
  if (!guestRangeIsValid(iovs, count * 8)) {
    return { errno: WASI_ERRNO_FAULT, entries: [], totalLength: 0 };
  }

  const view = new DataView(instanceMemory.buffer);
  const tableOffset = Number(iovs) >>> 0;
  const entries = [];
  let totalLength = 0;
  for (let index = 0; index < count; index += 1) {
    const entryOffset = tableOffset + index * 8;
    const ptr = view.getUint32(entryOffset, true);
    const length = view.getUint32(entryOffset + 4, true);
    if (!guestRangeIsValid(ptr, length)) {
      return { errno: WASI_ERRNO_FAULT, entries: [], totalLength: 0 };
    }
    totalLength += length;
    if (!Number.isSafeInteger(totalLength) || totalLength > 0xffffffff) {
      return { errno: WASI_ERRNO_INVAL, entries: [], totalLength: 0 };
    }
    entries.push({ ptr, length });
  }
  return { errno: WASI_ERRNO_SUCCESS, entries, totalLength };
}

function readGuestUint32(ptr) {
  if (!(instanceMemory instanceof WebAssembly.Memory)) {
    throw new Error('WebAssembly memory is unavailable');
  }
  return new DataView(instanceMemory.buffer).getUint32(Number(ptr), true);
}

function writeGuestUint64(ptr, value) {
  if (!(instanceMemory instanceof WebAssembly.Memory)) {
    return WASI_ERRNO_FAULT;
  }

  try {
    new DataView(instanceMemory.buffer).setBigUint64(Number(ptr), BigInt(value), true);
    return WASI_ERRNO_SUCCESS;
  } catch {
    return WASI_ERRNO_FAULT;
  }
}

function statTimestampNs(value) {
  const numeric = Number(value);
  return BigInt(Math.trunc((Number.isFinite(numeric) ? numeric : 0) * 1000000));
}

function writeGuestFilestat(ptr, stats, filetype = WASI_FILETYPE_REGULAR_FILE) {
  if (!guestRangeIsValid(ptr, 64)) {
    return WASI_ERRNO_FAULT;
  }

  try {
    const view = new DataView(instanceMemory.buffer);
    const offset = Number(ptr) >>> 0;
    view.setBigUint64(offset, 0n, true);
    view.setBigUint64(offset + 8, BigInt(stats?.ino ?? 0), true);
    view.setUint8(offset + 16, Number(filetype) >>> 0);
    view.setBigUint64(offset + 24, BigInt(stats?.nlink ?? 1), true);
    view.setBigUint64(offset + 32, BigInt(stats?.size ?? 0), true);
    view.setBigUint64(offset + 40, statTimestampNs(stats?.atimeMs), true);
    view.setBigUint64(offset + 48, statTimestampNs(stats?.mtimeMs), true);
    view.setBigUint64(offset + 56, statTimestampNs(stats?.ctimeMs), true);
    return WASI_ERRNO_SUCCESS;
  } catch {
    return WASI_ERRNO_FAULT;
  }
}

function wasiFiletypeFromStats(stats) {
  if (typeof stats?.isDirectory === 'function' && stats.isDirectory()) {
    return WASI_FILETYPE_DIRECTORY;
  }
  if (typeof stats?.isCharacterDevice === 'function' && stats.isCharacterDevice()) {
    return WASI_FILETYPE_CHARACTER_DEVICE;
  }
  if (typeof stats?.isFile === 'function' && stats.isFile()) {
    return WASI_FILETYPE_REGULAR_FILE;
  }
  return WASI_FILETYPE_UNKNOWN;
}

function writeGuestFdstat(ptr, filetype, flags, rightsBase, rightsInheriting) {
  if (!guestRangeIsValid(ptr, 24)) {
    return WASI_ERRNO_FAULT;
  }

  try {
    const view = new DataView(instanceMemory.buffer);
    const offset = Number(ptr) >>> 0;
    view.setUint8(offset, Number(filetype) >>> 0);
    view.setUint16(offset + 2, Number(flags) >>> 0, true);
    view.setBigUint64(offset + 8, BigInt(rightsBase), true);
    view.setBigUint64(offset + 16, BigInt(rightsInheriting), true);
    return WASI_ERRNO_SUCCESS;
  } catch {
    return WASI_ERRNO_FAULT;
  }
}

function mapSyntheticFsError(error) {
  switch (error?.code) {
    case 'E2BIG':
      return WASI_ERRNO_2BIG;
    case 'EAGAIN':
    case 'EWOULDBLOCK':
      return WASI_ERRNO_AGAIN;
    case 'EBADF':
      return WASI_ERRNO_BADF;
    case 'EACCES':
      return WASI_ERRNO_ACCES;
    case 'EPERM':
      return WASI_ERRNO_PERM;
    case 'EROFS':
      return WASI_ERRNO_ROFS;
    case 'EEXIST':
      return WASI_ERRNO_EXIST;
    case 'EFAULT':
      return WASI_ERRNO_FAULT;
    case 'EISDIR':
      return WASI_ERRNO_ISDIR;
    case 'ELOOP':
      return WASI_ERRNO_LOOP;
    case 'EMFILE':
      return WASI_ERRNO_MFILE;
    case 'ENFILE':
      return WASI_ERRNO_NFILE;
    case 'ENOENT':
      return WASI_ERRNO_NOENT;
    case 'ENOTEMPTY':
      return WASI_ERRNO_NOTEMPTY;
    case 'ENOEXEC':
      return WASI_ERRNO_NOEXEC;
    case 'ENOMEM':
      return WASI_ERRNO_NOMEM;
    case 'ENOSPC':
      return WASI_ERRNO_NOSPC;
    case 'ENOSYS':
      return WASI_ERRNO_NOSYS;
    case 'EOPNOTSUPP':
    case 'ENOTSUP':
      return WASI_ERRNO_NOTSUP;
    case 'ENODATA':
      return WASI_ERRNO_NODATA;
    case 'ENOTDIR':
      return WASI_ERRNO_NOTDIR;
    case 'EINVAL':
      return WASI_ERRNO_INVAL;
    case 'ENXIO':
      return WASI_ERRNO_NXIO;
    case 'ERANGE':
      return WASI_ERRNO_RANGE;
    case 'EOVERFLOW':
      return WASI_ERRNO_OVERFLOW;
    case 'EXDEV':
      return WASI_ERRNO_XDEV;
    default:
      return WASI_ERRNO_IO;
  }
}

function mapHostProcessError(error) {
  switch (error?.code) {
    case 'E2BIG':
      return WASI_ERRNO_2BIG;
    case 'EBADF':
      return WASI_ERRNO_BADF;
    case 'EBUSY':
      return WASI_ERRNO_BUSY;
    case 'EACCES':
      return WASI_ERRNO_ACCES;
    case 'EADDRINUSE':
      return WASI_ERRNO_ADDRINUSE;
    case 'EADDRNOTAVAIL':
      return WASI_ERRNO_ADDRNOTAVAIL;
    case 'EAFNOSUPPORT':
      return WASI_ERRNO_AFNOSUPPORT;
    case 'EAGAIN':
    case 'EWOULDBLOCK':
      return WASI_ERRNO_AGAIN;
    case 'EALREADY':
      return WASI_ERRNO_ALREADY;
    case 'ECHILD':
      return WASI_ERRNO_CHILD;
    case 'EFAULT':
      return WASI_ERRNO_FAULT;
    case 'EFBIG':
      return WASI_ERRNO_FBIG;
    case 'EEXIST':
      return WASI_ERRNO_EXIST;
    case 'ECONNREFUSED':
      return WASI_ERRNO_CONNREFUSED;
    case 'ECONNRESET':
      return WASI_ERRNO_CONNRESET;
    case 'EDEADLK':
      return WASI_ERRNO_DEADLK;
    case 'EDESTADDRREQ':
      return WASI_ERRNO_DESTADDRREQ;
    case 'EHOSTUNREACH':
      return WASI_ERRNO_HOSTUNREACH;
    case 'EINPROGRESS':
      return WASI_ERRNO_INPROGRESS;
    case 'EINTR':
      return WASI_ERRNO_INTR;
    case 'EIO':
      return WASI_ERRNO_IO;
    case 'EILSEQ':
      return WASI_ERRNO_ILSEQ;
    case 'EISCONN':
      return WASI_ERRNO_ISCONN;
    case 'EISDIR':
      return WASI_ERRNO_ISDIR;
    case 'ELOOP':
      return WASI_ERRNO_LOOP;
    case 'ENOENT':
      return WASI_ERRNO_NOENT;
    case 'ENOEXEC':
      return WASI_ERRNO_NOEXEC;
    case 'ENOMEM':
      return WASI_ERRNO_NOMEM;
    case 'ENOTDIR':
      return WASI_ERRNO_NOTDIR;
    case 'ENOTEMPTY':
      return WASI_ERRNO_NOTEMPTY;
    case 'ENOTSUP':
      return WASI_ERRNO_NOTSUP;
    case 'EPERM':
      return WASI_ERRNO_PERM;
    case 'EROFS':
      return WASI_ERRNO_ROFS;
    case 'EXDEV':
      return WASI_ERRNO_XDEV;
    case 'ESRCH':
      return WASI_ERRNO_SRCH;
    case 'ETIMEDOUT':
      return WASI_ERRNO_TIMEDOUT;
    case 'EINVAL':
      return WASI_ERRNO_INVAL;
    case 'EMFILE':
      return WASI_ERRNO_MFILE;
    case 'EMSGSIZE':
      return WASI_ERRNO_MSGSIZE;
    case 'ENAMETOOLONG':
      return WASI_ERRNO_NAMETOOLONG;
    case 'ENOBUFS':
      return WASI_ERRNO_NOBUFS;
    case 'ENETUNREACH':
      return WASI_ERRNO_NETUNREACH;
    case 'ENOTCONN':
      return WASI_ERRNO_NOTCONN;
    case 'ENOTSOCK':
      return WASI_ERRNO_NOTSOCK;
    case 'ENXIO':
      return WASI_ERRNO_NXIO;
    case 'EPIPE':
      return WASI_ERRNO_PIPE;
    case 'ESPIPE':
      return WASI_ERRNO_SPIPE;
    case 'EPROTONOSUPPORT':
      return WASI_ERRNO_PROTONOSUPPORT;
    default:
      return WASI_ERRNO_FAULT;
  }
}

function seekGuestFileHandle(handle, offset, whence) {
  const numericWhence = Number(whence) >>> 0;
  let base;
  if (numericWhence === WASI_WHENCE_SET) {
    base = 0n;
  } else if (numericWhence === WASI_WHENCE_CUR) {
    base = BigInt(handle.position ?? 0);
  } else if (numericWhence === WASI_WHENCE_END) {
    // Passthrough (read-only delegate) handles keep the real host fd in ioFd;
    // targetFd is only a synthetic guest fd number and fstat'ing it reports
    // size 0. Prefer ioFd so SEEK_END returns the true file size (e.g. mbedTLS
    // sizing a CA bundle via fseek(SEEK_END)+ftell before reading it).
    const sizeFd = typeof handle.ioFd === 'number' ? handle.ioFd : handle.targetFd;
    base = BigInt(Number(fsModule.fstatSync(sizeFd).size ?? 0));
  } else {
    return null;
  }

  const next = base + BigInt(offset);
  if (next < 0n || next > BigInt(Number.MAX_SAFE_INTEGER)) {
    return null;
  }

  handle.position = Number(next);
  return next;
}

function createPipeHandle(kind, pipe, displayFd) {
  if (kind === 'pipe-read') {
    pipe.readHandleCount += 1;
  } else if (kind === 'pipe-write') {
    pipe.writeHandleCount += 1;
  }

  return {
    kind,
    pipe,
    displayFd: Number(displayFd) >>> 0,
    refCount: 1,
    open: true,
  };
}

function retainDelegateFd(fd) {
  if (SIDECAR_MANAGED_PROCESS) {
    throw new Error('managed execution cannot retain a Node-WASI delegate fd');
  }
  const numericFd = Number(fd) >>> 0;
  standaloneDelegateFdRefCounts.set(numericFd, (standaloneDelegateFdRefCounts.get(numericFd) ?? 0) + 1);
}

function registerKernelDelegateFd(
  fd,
  preferredGuestFd = null,
  minimumGuestFd = 3,
  shadowsInternalPreopen = false,
) {
  if (!SIDECAR_MANAGED_PROCESS) {
    const error = new Error('kernel descriptor projection requires managed execution');
    error.code = 'EIO';
    throw error;
  }
  const rawKernelFd = Number(fd);
  if (!Number.isSafeInteger(rawKernelFd) || rawKernelFd < 0 || rawKernelFd > 0xffffffff) {
    const error = new Error(`kernel returned invalid file descriptor ${fd}`);
    error.code = 'EIO';
    throw error;
  }
  const kernelFd = rawKernelFd >>> 0;
  const rejectRegistration = (error) => {
    try {
      callSyncRpc('process.fd_close', [kernelFd]);
    } catch (closeError) {
      error.message = `${error.message}; additionally failed to close kernel fd ${kernelFd}: ${closeError?.message ?? closeError}`;
    }
    throw error;
  };
  let guestFd;
  let replacesBootstrapStdio = false;
  if (preferredGuestFd != null) {
    const rawPreferredFd = Number(preferredGuestFd);
    if (
      !Number.isSafeInteger(rawPreferredFd) || rawPreferredFd < 0 ||
      rawPreferredFd >= LINUX_GUEST_FD_LIMIT
    ) {
      const error = new Error(`invalid preferred guest file descriptor ${preferredGuestFd}`);
      error.code = 'EINVAL';
      rejectRegistration(error);
    }
    guestFd = rawPreferredFd >>> 0;
    // node:wasi pre-installs 0/1/2, but a spawned process may inherit kernel
    // pipe/PTY descriptions at those exact Linux descriptor numbers. Let the
    // kernel mapping shadow only those bootstrap stdio entries; every real
    // runner mapping remains collision-protected.
    const bootstrapStdioHandle = passthroughHandles.get(guestFd);
    const shadowsRetainedBootstrapStdio =
      guestFd <= 2 &&
      bootstrapStdioHandle == null &&
      closedPassthroughFds.has(guestFd) &&
      wasi?.fdTable?.has?.(guestFd) === true &&
      !activeFdProjections.has(guestFd) &&
      !retainedSpawnOutputHandlesByFd.has(guestFd);
    const shadowsPrivatePreopen =
      shadowsInternalPreopen ||
      shadowsRetainedBootstrapStdio ||
      (hiddenPreopenHandles.has(guestFd) &&
        (bootstrapStdioHandle == null || bootstrapStdioHandle.internalPreopen === true) &&
        !activeFdProjections.has(guestFd) &&
        !retainedSpawnOutputHandlesByFd.has(guestFd));
    replacesBootstrapStdio =
      guestFd <= 2 &&
      bootstrapStdioHandle?.kind === 'passthrough' &&
      Number(bootstrapStdioHandle.targetFd) === guestFd &&
      Number(bootstrapStdioHandle.refCount) === 0;
    if (syntheticFdInUse(guestFd) && !replacesBootstrapStdio && !shadowsPrivatePreopen) {
      const error = new Error(`guest file descriptor ${guestFd} is already in use`);
      error.code = 'EBUSY';
      rejectRegistration(error);
    }
    if (!replacesBootstrapStdio && !shadowsPrivatePreopen && !hasRunnerOpenFdCapacity(1)) {
      const error = new Error('guest file descriptor limit reached');
      error.code = 'EMFILE';
      rejectRegistration(error);
    }
  } else {
    guestFd = allocateKernelGuestFd(minimumGuestFd);
    if (guestFd == null) {
      const error = new Error('guest file descriptor limit reached');
      error.code = 'EMFILE';
      rejectRegistration(error);
    }
  }

  const handle = {
    kind: 'kernel-fd',
    targetFd: kernelFd,
  };
  closedPassthroughFds.delete(guestFd);
  if (replacesBootstrapStdio || passthroughHandles.get(guestFd)?.internalPreopen === true) {
    passthroughHandles.delete(guestFd);
    standaloneDelegateFdRefCounts.delete(guestFd);
  }
  if (guestFd === kernelFd) {
    passthroughHandles.set(guestFd, handle);
  } else {
    setActiveFdProjection(guestFd, handle);
  }
  return guestFd;
}

function releaseDelegateFd(fd) {
  const numericFd = Number(fd) >>> 0;
  const current = standaloneDelegateFdRefCounts.get(numericFd);
  if (current == null) {
    return false;
  }
  if (current <= 1) {
    standaloneDelegateFdRefCounts.delete(numericFd);
    return true;
  }
  standaloneDelegateFdRefCounts.set(numericFd, current - 1);
  return false;
}

function lookupFdHandle(fd) {
  const numericFd = Number(fd) >>> 0;
  if ((numericFd & AGENTOS_HIDDEN_PREOPEN_FD_TAG) !== 0) {
    return hiddenPreopenHandles.get(numericFd & AGENTOS_HIDDEN_PREOPEN_FD_MASK) ?? null;
  }
  return (
    activeFdProjections.get(numericFd) ??
    retainedSpawnOutputHandlesByFd.get(numericFd)?.handle ??
    passthroughHandles.get(numericFd) ??
    null
  );
}

function kernelFdMappingsForSpawn() {
  const mappings = new Map();
  for (const [guestFd, handle] of [...passthroughHandles, ...activeFdProjections]) {
    if (
      handle?.kind === 'kernel-fd' &&
      handle.open !== false &&
      handle.internalPreopen !== true
    ) {
      mappings.set(Number(guestFd) >>> 0, Number(handle.targetFd) >>> 0);
    }
  }
  if (mappings.size > configuredMaxOpenFds) {
    const error = new Error(
      `inherited kernel descriptor mappings exceed the ${configuredMaxOpenFds}-descriptor runtime limit`,
    );
    error.code = 'EMFILE';
    throw error;
  }
  return [...mappings.entries()];
}

function guestFdCloseOnExec(fd) {
  const numericFd = Number(fd) >>> 0;
  const handle = lookupFdHandle(numericFd);
  if (SIDECAR_MANAGED_PROCESS && handle?.kind === 'kernel-fd') {
    return (
      Number(callSyncRpc('process.fd_getfd', [Number(handle.targetFd) >>> 0])) & 1
    ) !== 0;
  }
  return standaloneCloexecFds.has(numericFd);
}

function hostNetFdsForSpawn() {
  if (SIDECAR_MANAGED_PROCESS) {
    const inherited = [...passthroughHandles, ...activeFdProjections]
      .filter(([, handle]) =>
        handle?.kind === 'kernel-fd' &&
        typeof handle.hostNetDescriptionId === 'string'
      )
      .map(([guestFd, handle]) => ({
        guestFd: Number(guestFd) >>> 0,
        descriptionId: handle.hostNetDescriptionId,
        closeOnExec: guestFdCloseOnExec(guestFd),
      }));
    if (inherited.length > configuredMaxOpenFds) {
      const error = new Error(
        `inherited host-network descriptors exceed limits.resources.maxOpenFds (${configuredMaxOpenFds}); ` +
          'raise limits.resources.maxOpenFds if needed',
      );
      error.code = 'EMFILE';
      throw error;
    }
    return inherited;
  }
  const entries = [...standaloneHostNetSockets.entries()].map(([guestFd, socket]) => [
        guestFd,
        socket,
        null,
      ]);
  const descriptions = new Set(entries.map(([, socket]) => socket));
  if (maxSockets != null && descriptions.size > maxSockets) {
    const error = new Error(
      `inherited host-network descriptions exceed limits.resources.maxSockets (${maxSockets}); ` +
        'raise limits.resources.maxSockets if needed',
    );
    error.code = 'EMFILE';
    throw error;
  }
  const inherited = entries.map(([guestFd, socket, descriptionId]) => ({
    guestFd: Number(guestFd) >>> 0,
    descriptionId,
    closeOnExec: guestFdCloseOnExec(guestFd),
    socketId: socket.socketId ?? null,
    serverId: socket.serverId ?? null,
    udpSocketId: socket.udpSocketId ?? null,
    metadata: {
      domain: Number(socket.domain) >>> 0,
      socketType: Number(socket.sockType) >>> 0,
      protocol: Number(socket.protocol) >>> 0,
      nonblocking: socket.nonblock === true,
      recvTimeoutMs: socket.recvTimeoutMs ?? null,
      bindOptions: socket.bindOptions ?? null,
      localInfo: socket.localInfo ?? null,
      localUnixAddress: socket.localUnixAddress ?? null,
      localReservation: socket.localReservation ?? null,
      remoteInfo: socket.remoteInfo ?? null,
      remoteUnixAddress: socket.remoteUnixAddress ?? null,
      listening: socket.listening === true,
    },
  }));
  if (inherited.length > configuredMaxOpenFds) {
    const error = new Error(
      `inherited host-network descriptors exceed limits.resources.maxOpenFds (${configuredMaxOpenFds}); ` +
        'raise limits.resources.maxOpenFds if needed',
    );
    error.code = 'EMFILE';
    throw error;
  }
  return inherited;
}

function lookupSyntheticHandleByDisplayFd(fd, expectedKind = null) {
  const numericFd = Number(fd) >>> 0;
  for (const handle of activeFdProjections.values()) {
    if (!handle || handle.displayFd !== numericFd) {
      continue;
    }
    if (expectedKind && handle.kind !== expectedKind) {
      continue;
    }
    return handle;
  }

  const retainedHandle = retainedSyntheticHandlesByDisplayFd.get(numericFd) ?? null;
  if (
    retainedHandle &&
    (!expectedKind || retainedHandle.kind === expectedKind)
  ) {
    return retainedHandle;
  }

  return null;
}

function retainSyntheticHandleByDisplayFd(handle) {
  if (
    handle &&
    (handle.kind === 'pipe-read' || handle.kind === 'pipe-write')
  ) {
    retainedSyntheticHandlesByDisplayFd.set(handle.displayFd >>> 0, handle);
  }
}

function releaseRetainedSyntheticHandleByDisplayFd(handle) {
  if (!handle) {
    return;
  }

  const displayFd = handle.displayFd >>> 0;
  if (retainedSyntheticHandlesByDisplayFd.get(displayFd) === handle) {
    retainedSyntheticHandlesByDisplayFd.delete(displayFd);
  }
}

function cloneFdHandle(fd) {
  const handle = lookupFdHandle(fd);
  if (!handle) {
    return null;
  }
  if (handle.kind === 'kernel-fd') {
    throw new Error('managed kernel descriptors must be duplicated by the kernel');
  }
  handle.refCount += 1;
  return handle;
}

function passthroughHandleHasCanonicalMapping(handle) {
  for (const current of passthroughHandles.values()) {
    if (current === handle) {
      return true;
    }
  }
  return false;
}

function releaseFdHandle(handle) {
  if (!handle) {
    return;
  }

  if (handle.kind === 'passthrough') {
    handle.refCount = Math.max(0, handle.refCount - 1);
    // Node's WASI preopens are capability roots used internally by libc to
    // implement ordinary absolute-path opens. close(2)/closefrom(2) must make
    // their guest descriptor numbers unusable without destroying those hidden
    // roots, which do not exist as process FDs on native Linux.
    if (handle.internalPreopen === true) {
      return;
    }
    if (
      handle.refCount === 0 &&
      handle.open &&
      handle.targetFd > 2 &&
      !passthroughHandleHasCanonicalMapping(handle) &&
      releaseDelegateFd(handle.targetFd) &&
      typeof delegateManagedFdClose === 'function'
    ) {
      delegateManagedFdClose(handle.targetFd);
    }
    if (handle.refCount === 0 && handle.open && typeof handle.ioFd === 'number') {
      try {
        fsModule.closeSync(handle.ioFd);
      } catch (error) {
        writeStream(
          process.stderr,
          `agentos: failed to close delegated fd ${handle.ioFd}: ${formatError(error)}\n`,
        );
      }
      handle.ioFd = null;
    }
    return;
  }

  if (handle.kind === 'guest-file') {
    handle.refCount = Math.max(0, handle.refCount - 1);
    if (handle.refCount === 0 && handle.open) {
      handle.open = false;
      if (handle.ownsTargetFd === false) {
        releaseFdHandle(handle.backingHandle);
      } else {
        fsModule.closeSync(handle.targetFd);
      }
    }
    return;
  }

  if (handle.kind === 'kernel-fd') {
    // Managed preopens are hidden capability roots, not guest-owned Linux
    // descriptors. Closing their untagged guest aliases must not destroy the
    // backing kernel descriptions used by tagged libc pathname operations.
    if (handle.internalPreopen === true) {
      return;
    }
    callSyncRpc('process.fd_close', [Number(handle.targetFd) >>> 0]);
    return;
  }

  handle.refCount = Math.max(0, handle.refCount - 1);
  if (handle.refCount > 0 || !handle.open) {
    return;
  }

  handle.open = false;
  if (handle.kind === 'pipe-read') {
    handle.pipe.readHandleCount = Math.max(0, handle.pipe.readHandleCount - 1);
  } else if (handle.kind === 'pipe-write') {
    handle.pipe.writeHandleCount = Math.max(0, handle.pipe.writeHandleCount - 1);
    if (handle.pipe.writeHandleCount === 0 && (handle.pipe.producers?.size ?? 0) === 0) {
      closePipeConsumers(handle.pipe);
    }
  }
}

function closeSyntheticFd(fd) {
  const numericFd = Number(fd) >>> 0;
  const handle = activeFdProjections.get(numericFd);
  if (!handle) {
    return false;
  }

  const shouldRetainMapping =
    ((handle.kind === 'pipe-write' && (handle.pipe.producers?.size ?? 0) > 0) ||
      (handle.kind === 'pipe-read' && (handle.pipe.consumers?.size ?? 0) > 0));
  if (shouldRetainMapping) {
    retainSyntheticHandleByDisplayFd(handle);
  }
  // The retained display mapping exists only so an already-spawned child can
  // continue routing pipe data. It must never leave the descriptor visible to
  // the parent after close(2). Mask the underlying Node-WASI/bootstrap slot as
  // well; otherwise a later fstat(2) can fall through to a runtime-owned fd
  // with the same number and make a second close appear to be the first.
  activeFdProjections.delete(numericFd);
  closedPassthroughFds.add(numericFd);
  releaseFdHandle(handle);
  if (shouldRetainMapping) {
    collectInactivePipeHandles(handle.pipe);
  }
  return true;
}

function closePassthroughFd(fd) {
  const numericFd = Number(fd) >>> 0;
  const handle = passthroughHandles.get(numericFd);
  if (!handle) {
    return false;
  }

  passthroughHandles.delete(numericFd);
  closedPassthroughFds.add(numericFd);
  if ((handle.refCount ?? 0) === 0) {
    releaseFdHandle(handle);
  }
  return true;
}

function forgetSidecarClosedKernelFd(fd) {
  const numericFd = Number(fd) >>> 0;
  const handle = lookupFdHandle(numericFd);
  if (handle?.kind !== 'kernel-fd') {
    return false;
  }
  if (activeFdProjections.get(numericFd) === handle) {
    activeFdProjections.delete(numericFd);
  }
  if (passthroughHandles.get(numericFd) === handle) {
    passthroughHandles.delete(numericFd);
  }
  closedPassthroughFds.add(numericFd);
  standaloneCloexecFds.delete(numericFd);
  traceHostProcess('exec-cloexec-kernel-fd-forgotten', {
    fd: numericFd,
    targetFd: Number(handle.targetFd) >>> 0,
  });
  return true;
}

function forgetSidecarClosedKernelTargetFd(targetFd) {
  const numericTargetFd = Number(targetFd) >>> 0;
  const guestFds = new Set([
    ...activeFdProjections.keys(),
    ...passthroughHandles.keys(),
  ]);
  let forgotten = false;
  for (const guestFd of guestFds) {
    const handle = lookupFdHandle(guestFd);
    if (
      handle?.kind === 'kernel-fd' &&
      Number(handle.targetFd) >>> 0 === numericTargetFd
    ) {
      forgotten = forgetSidecarClosedKernelFd(guestFd) || forgotten;
    }
  }
  const passthrough = passthroughHandles.get(numericTargetFd);
  if (
    passthrough?.kind === 'passthrough' &&
    passthrough.internalPreopen !== true &&
    Number(passthrough.targetFd) >>> 0 === numericTargetFd
  ) {
    forgotten = closePassthroughFd(numericTargetFd) || forgotten;
    standaloneCloexecFds.delete(numericTargetFd);
  }
  return forgotten;
}

function rejectClosedPassthroughFd(fd) {
  return closedPassthroughFds.has(Number(fd) >>> 0);
}

function collectInactivePipeHandles(pipe) {
  if (!pipe) {
    return;
  }

  if (
    (pipe.readHandleCount ?? 0) > 0 ||
    (pipe.writeHandleCount ?? 0) > 0 ||
    (pipe.producers?.size ?? 0) > 0 ||
    (pipe.consumers?.size ?? 0) > 0
  ) {
    return;
  }

  for (const [fd, handle] of Array.from(activeFdProjections.entries())) {
    if (
      (handle.kind === 'pipe-read' || handle.kind === 'pipe-write') &&
      handle.pipe === pipe &&
      !handle.open &&
      handle.refCount === 0
    ) {
      activeFdProjections.delete(fd);
    }
  }

  for (const [displayFd, handle] of Array.from(retainedSyntheticHandlesByDisplayFd.entries())) {
    if (
      (handle.kind === 'pipe-read' || handle.kind === 'pipe-write') &&
      handle.pipe === pipe &&
      !handle.open &&
      handle.refCount === 0
    ) {
      retainedSyntheticHandlesByDisplayFd.delete(displayFd);
    }
  }
}

function resolveSpawnFd(fd) {
  const numericFd = Number(fd) >>> 0;
  const handle = lookupFdHandle(fd);
  if (!handle) {
    return numericFd;
  }
  if (handle.kind === 'passthrough') {
    return handle.targetFd >>> 0;
  }
  if (handle.kind === 'guest-file') {
    return numericFd;
  }
  return handle.displayFd >>> 0;
}

function spawnStdinFdIsSyntheticPipe(fd) {
  const handle =
    lookupFdHandle(fd) ?? lookupSyntheticHandleByDisplayFd(fd, 'pipe-read');
  return handle?.kind === 'pipe-read';
}

function spawnFdIsKernelBacked(fd) {
  const numericFd = Number(fd) >>> 0;
  return lookupFdHandle(numericFd)?.kind === 'kernel-fd' ||
    standaloneDelegateFdRefCounts.has(numericFd);
}

// Shell input redirects (`cmd < file`) reach proc_spawn as a plain file fd in
// stdin_fd. The child cannot share that descriptor across the spawn boundary,
// so the remaining file contents are materialized and written to the child's
// stdin pipe, exactly like POSIX children reading an inherited file fd to EOF.
// Returns null when the fd is not a readable file-backed handle so callers can
// fail loudly instead of leaving the child hanging on an open stdin pipe.
function readSpawnStdinRedirectBytes(fd) {
  const numericFd = Number(fd) >>> 0;
  const handle = lookupFdHandle(numericFd);
  if (!handle) {
    return null;
  }

  if (handle.kind === 'guest-file') {
    const chunks = [];
    let position = handle.position ?? 0;
    for (;;) {
      const buffer = Buffer.alloc(65536);
      const bytesRead = fsModule.readSync(
        handle.targetFd,
        buffer,
        0,
        buffer.length,
        position,
      );
      if (bytesRead <= 0) {
        break;
      }
      chunks.push(buffer.subarray(0, bytesRead));
      position += bytesRead;
    }
    handle.position = position;
    return Buffer.concat(chunks);
  }

  if (handle.kind === 'passthrough' && typeof handle.guestPath === 'string') {
    if (handle.guestPath === '/dev/null') {
      return Buffer.alloc(0);
    }
    const stats = fsModule.statSync(handle.guestPath);
    if (!stats.isFile()) {
      return null;
    }
    return Buffer.from(fsModule.readFileSync(handle.guestPath));
  }

  return null;
}

function retainSpawnOutputHandle(fd) {
  const numericFd = Number(fd) >>> 0;
  if (numericFd <= 2) {
    return null;
  }

  const retained = retainedSpawnOutputHandlesByFd.get(numericFd);
  if (retained) {
    retained.refCount += 1;
    retained.handle.refCount += 1;
    return { fd: numericFd, handle: retained.handle };
  }

  const handle = lookupFdHandle(numericFd);
  if (
    handle?.kind !== 'guest-file' &&
    handle?.kind !== 'passthrough'
  ) {
    return null;
  }

  handle.refCount += 1;
  retainedSpawnOutputHandlesByFd.set(numericFd, { handle, refCount: 1 });
  return { fd: numericFd, handle };
}

function releaseSpawnOutputHandles(retainedHandles) {
  for (const retained of retainedHandles ?? []) {
    if (!retained || typeof retained.fd !== 'number' || !retained.handle) {
      continue;
    }
    const retainedEntry = retainedSpawnOutputHandlesByFd.get(retained.fd);
    if (retainedEntry?.handle === retained.handle) {
      retainedEntry.refCount -= 1;
      if (retainedEntry.refCount <= 0) {
        retainedSpawnOutputHandlesByFd.delete(retained.fd);
      }
    }
    releaseFdHandle(retained.handle);
  }
}

function collectGuestIovBytes(iovs, iovsLen, validatedRequest = null) {
  const request = validatedRequest ?? validateGuestIovRequest(iovs, iovsLen);
  if (request.errno !== WASI_ERRNO_SUCCESS) {
    throw new RangeError(`invalid guest iovec request: WASI errno ${request.errno}`);
  }
  const chunks = [];
  for (const { ptr, length } of request.entries) {
    const chunk = readGuestBytes(ptr, length);
    chunks.push(chunk);
  }
  return Buffer.concat(chunks, request.totalLength);
}

function writeBytesToGuestIovs(iovs, iovsLen, bytes, validatedRequest = null) {
  const request = validatedRequest ?? validateGuestIovRequest(iovs, iovsLen);
  if (request.errno !== WASI_ERRNO_SUCCESS) {
    throw new RangeError(`invalid guest iovec request: WASI errno ${request.errno}`);
  }
  const source = Buffer.from(bytes ?? []);
  const memory = new Uint8Array(instanceMemory.buffer);
  let written = 0;
  for (const { ptr, length } of request.entries) {
    if (written >= source.length) break;
    const remaining = source.length - written;
    const chunkLength = Math.min(length, remaining);
    memory.set(source.subarray(written, written + chunkLength), ptr);
    written += chunkLength;
  }
  return written >>> 0;
}

function guestIovByteLength(iovs, iovsLen, validatedRequest = null) {
  const request = validatedRequest ?? validateGuestIovRequest(iovs, iovsLen);
  if (request.errno !== WASI_ERRNO_SUCCESS) {
    throw new RangeError(`invalid guest iovec request: WASI errno ${request.errno}`);
  }
  return request.totalLength;
}

function writeHostNetBytesToGuestIovs(iovs, iovsLen, bytes, nreadPtr) {
  try {
    return writeGuestUint32(
      nreadPtr,
      writeBytesToGuestIovs(iovs, iovsLen, bytes),
    );
  } catch (error) {
    const memoryBytes = instanceMemory instanceof WebAssembly.Memory
      ? instanceMemory.buffer.byteLength
      : 0;
    process.stderr.write(
      `[agentos] ERR_AGENTOS_V8_HOSTNET_GUEST_WRITE: failed to commit socket bytes to guest memory (iovs=${Number(iovs) >>> 0}, iovsLen=${Number(iovsLen) >>> 0}, nread=${Number(nreadPtr) >>> 0}, sourceBytes=${Buffer.from(bytes ?? []).length}, memoryBytes=${memoryBytes}): ${error?.message ?? String(error)}\n`,
    );
    return WASI_ERRNO_FAULT;
  }
}

function readHostNetSocketToGuestIovs(socket, iovs, iovsLen, nreadPtr, iovRequest) {
  try {
    const requestedLength = guestIovByteLength(iovs, iovsLen, iovRequest);
    if (requestedLength === 0) {
      return writeGuestUint32(nreadPtr, 0);
    }

    if (socket?.managed === true) {
      const targetFd = managedHostNetTargetFd(socket);
      const stat = callSyncRpc('process.fd_stat', [targetFd]);
      const nonblocking = (Number(stat?.flags) & KERNEL_O_NONBLOCK) !== 0;
      const configured = nonblocking ? null : callSyncRpc(
        'process.hostnet_get_option',
        [targetFd, 'receive-timeout'],
      );
      const configuredTimeout = Number(configured?.durationMs);
      const hasConfiguredTimeout = configured?.durationMs != null &&
        Number.isFinite(configuredTimeout) && configuredTimeout >= 0;
      const waitLimit = nonblocking
        ? 0
        : hasConfiguredTimeout ? configuredTimeout : unixConnectTimeoutMs;
      const startedAt = Date.now();
      for (;;) {
        // Linux's nonblocking-I/O pattern is read, then wait, then retry.
        // Probing first also closes the race where bytes become durable just
        // before a readiness subscription is armed and its wake is coalesced.
        const result = callSyncRpc('process.hostnet_recv', [
          targetFd,
          requestedLength,
          0,
          0,
        ]);
        if (result != null && !isHostNetWouldBlock(result)) {
          const bytes = result?.type === 'message'
            ? hostNetDatagramBytes(result)
            : decodeFsBytesPayload(result, 'managed host-network read');
          return writeHostNetBytesToGuestIovs(iovs, iovsLen, bytes, nreadPtr);
        }
        if (result == null) return writeGuestUint32(nreadPtr, 0);
        if (nonblocking) return WASI_ERRNO_AGAIN;
        const remaining = Math.max(0, waitLimit - (Date.now() - startedAt));
        if (remaining === 0) {
          return hasConfiguredTimeout ? WASI_ERRNO_AGAIN : WASI_ERRNO_TIMEDOUT;
        }
        const readable = waitManagedHostNetReadable(socket, remaining, true);
        if (readable === WASI_ERRNO_INTR) return WASI_ERRNO_INTR;
        if (!readable) {
          // Readiness is only a hint. A final receive probe wins over a
          // simultaneous timeout and prevents a coalesced wake from turning
          // already-buffered bytes into a spurious 30-second failure.
          const finalResult = callSyncRpc('process.hostnet_recv', [
            targetFd,
            requestedLength,
            0,
            0,
          ]);
          if (finalResult != null && !isHostNetWouldBlock(finalResult)) {
            const bytes = finalResult?.type === 'message'
              ? hostNetDatagramBytes(finalResult)
              : decodeFsBytesPayload(finalResult, 'managed host-network read');
            return writeHostNetBytesToGuestIovs(iovs, iovsLen, bytes, nreadPtr);
          }
          if (finalResult == null) return writeGuestUint32(nreadPtr, 0);
          return hasConfiguredTimeout ? WASI_ERRNO_AGAIN : WASI_ERRNO_TIMEDOUT;
        }
      }
    }

    if (socket.nonblock) {
      let queued = dequeueHostNetBytes(socket, requestedLength);
      if (queued.length > 0) {
        return writeHostNetBytesToGuestIovs(iovs, iovsLen, queued, nreadPtr);
      }
      if (socket.lastError) return mapHostProcessError(socket.lastError);
      if (socket.readableEnded || socket.closed || !socket.socketId) {
        return writeGuestUint32(nreadPtr, 0);
      }
      const result = readReadyHostNetSocket(socket, requestedLength, false, 0);
      if (result?.kind === 'data' && result.bytes.length > 0) {
        return writeHostNetBytesToGuestIovs(iovs, iovsLen, result.bytes, nreadPtr);
      }
      queued = dequeueHostNetBytes(socket, requestedLength);
      if (queued.length > 0) {
        return writeHostNetBytesToGuestIovs(iovs, iovsLen, queued, nreadPtr);
      }
      if (socket.readableEnded || socket.closed || !socket.socketId) {
        return writeGuestUint32(nreadPtr, 0);
      }
      return WASI_ERRNO_AGAIN;
    }

    const startedAt = Date.now();
    const receiveDeadline = socket.recvTimeoutMs == null
      ? null
      : startedAt + Math.max(0, socket.recvTimeoutMs);
    const safeguardDeadline = startedAt + unixConnectTimeoutMs;
    let warnedNearLimit = false;
    while (true) {
      if (dispatchPendingWasmSignals(true)) return WASI_ERRNO_INTR;
      const queued = dequeueHostNetBytes(socket, requestedLength);
      if (queued.length > 0) {
        return writeHostNetBytesToGuestIovs(iovs, iovsLen, queued, nreadPtr);
      }
      if (socket.lastError) return mapHostProcessError(socket.lastError);
      if (socket.readableEnded || socket.closed || !socket.socketId) {
        return writeGuestUint32(nreadPtr, 0);
      }

      const now = Date.now();
      if (receiveDeadline != null && now >= receiveDeadline) {
        return WASI_ERRNO_AGAIN;
      }
      warnedNearLimit = warnNearBlockingReadLimit(
        'blocking socket read',
        startedAt,
        warnedNearLimit,
      );
      if (now >= safeguardDeadline) {
        process.stderr.write(
          `[agentos] blocking socket read exceeded limits.resources.maxBlockingReadMs (${unixConnectTimeoutMs} ms); raise limits.resources.maxBlockingReadMs if needed\n`,
        );
        return WASI_ERRNO_TIMEDOUT;
      }
      const nextDeadline = receiveDeadline == null
        ? safeguardDeadline
        : Math.min(receiveDeadline, safeguardDeadline);
      // A blocking host-net read must not monopolize the VM dispatcher while a
      // local child is the peer that will make it readable. Probe without an
      // inline sidecar wait and drive the child between attempts, matching the
      // existing kernel-fd and poll(2) descendant fairness paths.
      const pumpsLocalChildren = hasActiveSpawnedChildren();
      const pollWaitMs = pumpsLocalChildren
        ? 0
        : Math.max(0, Math.min(50, nextDeadline - now));
      const result = readReadyHostNetSocket(socket, requestedLength, false, pollWaitMs);
      if (dispatchPendingWasmSignals(true)) return WASI_ERRNO_INTR;
      if (result?.kind === 'data' && result.bytes.length > 0) {
        return writeHostNetBytesToGuestIovs(iovs, iovsLen, result.bytes, nreadPtr);
      }
      if (pumpsLocalChildren) {
        pumpSpawnedChildren(SPAWNED_CHILD_WAIT_SLICE_MS);
        if (dispatchPendingWasmSignals(true)) return WASI_ERRNO_INTR;
      }
      if (receiveDeadline != null && Date.now() >= receiveDeadline) {
        return WASI_ERRNO_AGAIN;
      }
    }
  } catch (error) {
    return mapHostProcessError(error);
  }
}

function writeHostNetSocketFromGuestIovs(socket, iovs, iovsLen, nwrittenPtr, iovRequest) {
  if (!socket || (socket.managed !== true && (!socket.socketId || socket.closed))) {
    return WASI_ERRNO_BADF;
  }

  let bytes;
  try {
    bytes = collectGuestIovBytes(iovs, iovsLen, iovRequest);
  } catch {
    return WASI_ERRNO_FAULT;
  }
  if (bytes.length === 0) {
    return writeGuestUint32(nwrittenPtr, 0);
  }

  try {
    const written = Number(socket.managed === true
      ? callSyncRpc('process.hostnet_send', [
        managedHostNetTargetFd(socket),
        bytes,
        0,
        null,
        unixConnectTimeoutMs,
      ])
      : callSyncRpc('net.write', [socket.socketId, bytes, socket.nonblock === true])) >>> 0;
    return writeGuestUint32(nwrittenPtr, written);
  } catch (error) {
    return mapHostProcessError(error);
  }
}

function dequeuePipeBytes(pipe, maxBytes) {
  const requested = Math.max(0, Number(maxBytes) >>> 0);
  if (requested === 0 || pipe.chunks.length === 0) {
    return Buffer.alloc(0);
  }

  const parts = [];
  let remaining = requested;
  while (remaining > 0 && pipe.chunks.length > 0) {
    const chunk = pipe.chunks[0];
    if (chunk.length <= remaining) {
      parts.push(chunk);
      pipe.chunks.shift();
      remaining -= chunk.length;
      continue;
    }

    parts.push(chunk.subarray(0, remaining));
    pipe.chunks[0] = chunk.subarray(remaining);
    remaining = 0;
  }

  return Buffer.concat(parts);
}

function enqueuePipeBytes(pipe, bytes) {
  const chunk = Buffer.from(bytes ?? []);
  if (chunk.length === 0) {
    return;
  }
  pipe.chunks.push(chunk);
}

function pipeHasReaders(pipe) {
  return (
    (pipe?.readHandleCount ?? 0) > 0 ||
    (pipe?.consumers?.size ?? 0) > 0
  );
}

function unregisterPipeProducer(pipe, producerKey) {
  if (!pipe || typeof pipe.producers?.delete !== 'function') {
    return;
  }
  pipe.producers.delete(producerKey);
  if (pipe.producers.size === 0 && (pipe.writeHandleCount ?? 0) === 0) {
    closePipeConsumers(pipe);
  }
  collectInactivePipeHandles(pipe);
}

function unregisterPipeConsumer(pipe, consumerKey) {
  if (!pipe || typeof pipe.consumers?.delete !== 'function') {
    return;
  }
  pipe.consumers.delete(consumerKey);
  collectInactivePipeHandles(pipe);
}

function unregisterChildPipeProducers(record) {
  if (!record || !record.childId) {
    return;
  }

  for (const [stream, fd, pipe] of [
    ['stdout', record.stdoutFd, record.stdoutPipe],
    ['stderr', record.stderrFd, record.stderrPipe],
  ]) {
    const outputPipe =
      pipe ??
      (() => {
        const handle =
          lookupFdHandle(fd) ?? lookupSyntheticHandleByDisplayFd(fd, 'pipe-write');
        return handle?.kind === 'pipe-write' ? handle.pipe : null;
      })();
    if (outputPipe) {
      unregisterPipeProducer(outputPipe, `${record.childId}:${stream}`);
    }
  }
}

function unregisterChildPipeConsumers(record) {
  if (!record || !record.childId) {
    return;
  }

  const inputPipe = resolveChildInputPipe(record);
  if (inputPipe) {
    unregisterPipeConsumer(inputPipe, `${record.childId}:stdin`);
  }
}

function resolveChildInputPipe(record) {
  if (!record) {
    return null;
  }
  if (record.directPosixStdin === true) {
    return null;
  }

  return (
    record.stdinPipe ??
    (() => {
      const handle =
        lookupFdHandle(record.stdinFd) ??
        lookupSyntheticHandleByDisplayFd(record.stdinFd, 'pipe-read');
      return handle?.kind === 'pipe-read' ? handle.pipe : null;
    })()
  );
}

function registerPipeProducer(fd, childId, stream) {
  const handle =
    lookupFdHandle(fd) ?? lookupSyntheticHandleByDisplayFd(fd, 'pipe-write');
  if (handle?.kind !== 'pipe-write') {
    return null;
  }
  handle.pipe.producers.set(`${childId}:${stream}`, { childId, stream });
  traceHostProcess('register-producer', { fd: Number(fd) >>> 0, childId, stream, pipeId: handle.pipe.id });
  return handle.pipe;
}

function registerPipeConsumer(fd, childId, stream) {
  const handle =
    lookupFdHandle(fd) ?? lookupSyntheticHandleByDisplayFd(fd, 'pipe-read');
  if (handle?.kind !== 'pipe-read') {
    return null;
  }
  handle.pipe.consumers.set(`${childId}:${stream}`, { childId, stream });
  const shouldDeferInitialDelivery =
    stream === 'stdin' && !childCorrelationsById.has(childId);
  traceHostProcess('register-consumer', {
    fd: Number(fd) >>> 0,
    childId,
    stream,
    pipeId: handle.pipe.id,
    deferred: shouldDeferInitialDelivery,
  });
  if (!shouldDeferInitialDelivery) {
    if (handle.pipe.chunks.length > 0) {
      flushPipeConsumers(handle.pipe);
    }
    if (handle.pipe.producers.size === 0 && (handle.pipe.writeHandleCount ?? 0) === 0) {
      closePipeConsumers(handle.pipe);
    }
  }
  return handle.pipe;
}

function flushPipeConsumers(pipe) {
  if (
    !pipe ||
    typeof pipe.consumers?.size !== 'number' ||
    !Array.isArray(pipe.chunks) ||
    pipe.consumers.size === 0 ||
    pipe.chunks.length === 0
  ) {
    return false;
  }

  let flushed = false;
  while (pipe.chunks.length > 0) {
    const chunk = pipe.chunks[0];
    if (!chunk || chunk.length === 0) {
      pipe.chunks.shift();
      continue;
    }
    let shouldRetryChunk = false;
    for (const [consumerKey, consumer] of Array.from(pipe.consumers.entries())) {
      try {
        callSyncRpc('child_process.write_stdin', [consumer.childId, chunk]);
        traceHostProcess('flush-consumer-write', {
          pipeId: pipe.id,
          childId: consumer.childId,
          bytes: chunk.length,
        });
        flushed = true;
      } catch (error) {
        if (childCorrelationsById.has(consumer?.childId) && isChildProcessGoneError(error)) {
          shouldRetryChunk = true;
          continue;
        }
        traceHostProcess('flush-consumer-write-failed', {
          pipeId: pipe.id,
          childId: consumer?.childId ?? null,
        });
        pipe.consumers.delete(consumerKey);
      }
    }
    if (shouldRetryChunk) {
      break;
    }
    pipe.chunks.shift();
  }

  return flushed;
}

function closePipeConsumers(pipe) {
  if (!pipe || typeof pipe.consumers?.size !== 'number' || pipe.consumers.size === 0) {
    return false;
  }

  let closed = false;
  for (const [consumerKey, consumer] of Array.from(pipe.consumers.entries())) {
    try {
      callSyncRpc('child_process.close_stdin', [consumer.childId]);
      traceHostProcess('close-consumer-stdin', {
        pipeId: pipe.id,
        childId: consumer.childId,
      });
      closed = true;
    } catch (error) {
      if (childCorrelationsById.has(consumer?.childId) && isChildProcessGoneError(error)) {
        continue;
      }
      traceHostProcess('close-consumer-stdin-failed', {
        pipeId: pipe.id,
        childId: consumer?.childId ?? null,
      });
      // Ignore close errors during teardown.
    }
    pipe.consumers.delete(consumerKey);
  }

  collectInactivePipeHandles(pipe);
  return closed;
}

function parseInitialHostNetFds(value, fdLimit, socketLimit) {
  if (typeof value !== 'string' || value.length === 0) return [];
  let parsed;
  try {
    parsed = JSON.parse(value);
  } catch (error) {
    throw new Error(`AGENTOS_WASM_INHERITED_HOSTNET_FDS must be JSON: ${error}`);
  }
  if (!Array.isArray(parsed)) {
    throw new Error('AGENTOS_WASM_INHERITED_HOSTNET_FDS must be an array');
  }
  if (socketLimit != null && parsed.length > socketLimit) {
    throw new Error(
      `AGENTOS_WASM_INHERITED_HOSTNET_FDS exceeds limits.resources.maxSockets (${socketLimit})`,
    );
  }
  const guestFds = new Set();
  for (const entry of parsed) {
    if (!entry || typeof entry !== 'object' || !Array.isArray(entry.guestFds)) {
      throw new Error('inherited host-network entries require a guestFds array');
    }
    const ids = [entry.socketId, entry.serverId, entry.udpSocketId]
      .filter((id) => typeof id === 'string' && id.length > 0);
    const managedPending =
      SIDECAR_MANAGED_PROCESS &&
      ids.length === 0 &&
      typeof entry.descriptionId === 'string' &&
      /^[0-9]+$/.test(entry.descriptionId);
    if (ids.length !== 1 && !managedPending) {
      throw new Error('inherited host-network entries require exactly one sidecar resource id');
    }
    if (entry.guestFds.length === 0) {
      throw new Error('inherited host-network entries require at least one guest fd');
    }
    for (const rawFd of entry.guestFds) {
      const fd = Number(rawFd);
      if (
        !Number.isSafeInteger(fd) || fd < 0 || fd >= LINUX_GUEST_FD_LIMIT ||
        guestFds.has(fd)
      ) {
        throw new Error('inherited host-network entries contain invalid or duplicate guest fds');
      }
      guestFds.add(fd);
    }
  }
  if (guestFds.size > fdLimit) {
    throw new Error(
      `AGENTOS_WASM_INHERITED_HOSTNET_FDS exceeds limits.resources.maxOpenFds (${fdLimit})`,
    );
  }
  return parsed;
}

function routeChunkToFd(fd, bytes) {
  const numericFd = Number(fd) >>> 0;
  const handle =
    lookupFdHandle(numericFd) ??
    lookupSyntheticHandleByDisplayFd(numericFd) ??
    (typeof globalThis.lookupFdHandle === 'function'
      ? globalThis.lookupFdHandle(numericFd)
      : null);
  traceHostProcess('route-chunk', {
    fd: numericFd,
    handleKind: handle?.kind ?? null,
    bytes: Buffer.from(bytes ?? []).length,
  });
  if (!handle) {
    if (isStdioFd(numericFd) && routeChunkToDelegateFd(numericFd, bytes)) {
      return;
    }
    if (isStdioFd(numericFd)) {
      writeToStdioFd(numericFd, Buffer.from(bytes ?? []));
      return;
    }
    if (numericFd > 2 && routeChunkToDelegateFd(numericFd, bytes)) {
      return;
    }
    writeSync(numericFd, bytes);
    return;
  }

  if (handle.kind === 'kernel-fd') {
    const chunk = Buffer.from(bytes ?? []);
    let offset = 0;
    while (offset < chunk.length) {
      const written = Number(callSyncRpc('process.fd_write', [
        Number(handle.targetFd) >>> 0,
        chunk.subarray(offset),
      ]));
      if (!Number.isSafeInteger(written) || written <= 0 || written > chunk.length - offset) {
        throw new Error(`invalid kernel fd write result ${String(written)}`);
      }
      offset += written;
    }
    return;
  }

  if (handle.kind === 'passthrough') {
    if (handle.append === true && typeof handle.guestPath === 'string') {
      fsModule.appendFileSync(handle.guestPath, Buffer.from(bytes ?? []));
      return;
    }
    if (routeChunkToDelegateFd(handle.targetFd, bytes)) {
      return;
    }
    if (isStdioFd(handle.targetFd)) {
      writeToStdioFd(handle.targetFd, Buffer.from(bytes ?? []));
      return;
    }
    writeSync(handle.targetFd, bytes);
    return;
  }

  if (handle.kind === 'host-passthrough') {
    if (routeChunkToDelegateFd(handle.displayFd ?? numericFd, bytes)) {
      return;
    }
    if (routeChunkToDelegateFd(handle.targetFd, bytes)) {
      return;
    }
    if (isStdioFd(handle.targetFd)) {
      writeToStdioFd(handle.targetFd, Buffer.from(bytes ?? []));
      return;
    }
    writeSync(handle.targetFd, bytes);
    return;
  }

  if (handle.kind === 'pipe-write') {
    enqueuePipeBytes(handle.pipe, bytes);
    flushPipeConsumers(handle.pipe);
    return;
  }

  if (handle.kind === 'guest-file') {
    writeBytesToGuestFileHandle(handle, Buffer.from(bytes ?? []));
    return;
  }

  throw new Error(`bad file descriptor ${numericFd}`);
}

function writeBytesToGuestFileHandle(handle, bytes) {
  const chunk = Buffer.from(bytes ?? []);
  const position = handle.append ? null : (handle.position ?? 0);
  const written = fsModule.writeSync(
    handle.targetFd,
    chunk,
    0,
    chunk.length,
    position,
  );
  if (handle.append) {
    handle.position = Number(fsModule.fstatSync(handle.targetFd).size ?? 0);
  } else {
    handle.position = (handle.position ?? 0) + written;
  }
  return written;
}

function routeChunkToDelegateFd(fd, bytes) {
  if (!(instanceMemory instanceof WebAssembly.Memory) || typeof delegateManagedFdWrite !== 'function') {
    return false;
  }

  const chunk = Buffer.from(bytes ?? []);
  const needed = 8 + chunk.length + 4;
  if (
    delegateWriteScratch.capacity < needed ||
    delegateWriteScratch.base + needed > instanceMemory.buffer.byteLength
  ) {
    const pages = Math.max(1, Math.ceil(needed / 65536));
    const basePage = instanceMemory.grow(pages);
    delegateWriteScratch = {
      base: basePage * 65536,
      capacity: pages * 65536,
    };
  }

  try {
    const iovsPtr = delegateWriteScratch.base;
    const dataPtr = iovsPtr + 8;
    const nwrittenPtr = dataPtr + chunk.length;
    const memory = new Uint8Array(instanceMemory.buffer);
    const view = new DataView(instanceMemory.buffer);
    memory.set(chunk, dataPtr);
    view.setUint32(iovsPtr, dataPtr, true);
    view.setUint32(iovsPtr + 4, chunk.length, true);
    const result = delegateManagedFdWrite(fd, iovsPtr, 1, nwrittenPtr);
    traceHostProcess('route-chunk-delegate', {
      fd: Number(fd) >>> 0,
      bytes: chunk.length,
      result,
    });
    return result === WASI_ERRNO_SUCCESS;
  } catch (error) {
    traceHostProcess('route-chunk-delegate-error', {
      fd: Number(fd) >>> 0,
      bytes: chunk.length,
      message: error instanceof Error ? error.message : String(error),
    });
    return false;
  }
}

function finalizeChildExit(record, exitCode, signal, coreDumped = false) {
  if (SIDECAR_MANAGED_PROCESS) {
    // This flag is only the bridge's "stdout/stderr EOF delivered" gate. The
    // kernel remains the sole source of terminal kind, status, and reaping.
    record.terminalEventObserved = true;
    record.terminalEventObservedAtMs = Date.now();
    for (const fd of record.delegateRetainedFds ?? []) {
      if (releaseDelegateFd(fd) && typeof delegateManagedFdClose === 'function') {
        delegateManagedFdClose(fd);
      }
    }
    releaseSpawnOutputHandles(record.retainedSpawnOutputHandles);
    unregisterChildPipeProducers(record);
    unregisterChildPipeConsumers(record);
    return 0;
  }
  const signalNumber = signal == null ? 0 : signalNumberFromName(signal) & 0x7f;
  const rawExitCode = signalNumber === 0 ? Number(exitCode ?? 1) & 0xff : 0;
  const status = signalNumber === 0 ? rawExitCode : 128 + signalNumber;
  record.exitCode = rawExitCode;
  record.exitSignal = signalNumber;
  record.coreDumped = signalNumber !== 0 && coreDumped === true;
  record.exitStatus = status;
  record.rawWaitStatus =
    signalNumber === 0
      ? (rawExitCode << 8) >>> 0
      : (signalNumber | (record.coreDumped ? 0x80 : 0)) >>> 0;
  for (const fd of record.delegateRetainedFds ?? []) {
    if (releaseDelegateFd(fd) && typeof delegateManagedFdClose === 'function') {
      delegateManagedFdClose(fd);
    }
  }
  releaseSpawnOutputHandles(record.retainedSpawnOutputHandles);
  unregisterChildPipeProducers(record);
  unregisterChildPipeConsumers(record);
  return status;
}

function takeManagedWaitTransition(selector, options, blocking) {
  const numericSelector = Number(selector) | 0;
  const normalizedOptions = Number(options) >>> 0;
  for (;;) {
    try {
      // A POSIX blocking wait can legitimately outlive the bridge operation
      // deadline. Ask the kernel for one bounded event-driven slice at a time
      // and re-probe without polling or holding a sidecar worker.
      const transition = callSyncRpc('process.waitpid', [
        numericSelector,
        normalizedOptions,
        KERNEL_WAIT_SLICE_MS,
      ]);
      dispatchPendingWasmSignals();
      if (transition == null && blocking) continue;
      return transition;
    } catch (error) {
      const mustInterrupt = dispatchPendingWasmSignals(true);
      if (error?.code === 'EINTR' && !mustInterrupt) {
        // The caught handlers all requested SA_RESTART. They have run on the
        // guest thread, so reissue the kernel-authoritative blocking wait.
        continue;
      }
      throw error;
    }
  }
}

function reapManagedChildCorrelation(transition) {
  if (transition?.event !== 'exit') return;
  reapSpawnedChild(childCorrelationsByPid.get(Number(transition.pid) >>> 0));
}

function collectStaleManagedChildCorrelations() {
  if (!SIDECAR_MANAGED_PROCESS) return;
  const now = Date.now();
  if (now < nextManagedCorrelationCollectionAtMs) return;
  nextManagedCorrelationCollectionAtMs = now + 1000;
  for (const record of childCorrelationsByPid.values()) {
    if (record?.terminalEventObserved !== true) continue;
    try {
      callSyncRpc('process.getpgid', [record.pid]);
    } catch (error) {
      if (error?.code === 'ESRCH' || error?.code === 'ECHILD') {
        reapSpawnedChild(record);
      } else {
        traceHostProcess('managed-child-correlation-collection-fault', {
          pid: record.pid,
          code: error?.code ?? null,
          message: error instanceof Error ? error.message : String(error),
        });
      }
    }
  }
}

function pollChildEvent(record, waitMs) {
  if (Array.isArray(record?.pendingEvents) && record.pendingEvents.length > 0) {
    return record.pendingEvents.shift() ?? null;
  }
  if (record?.synthetic) {
    return null;
  }
  return callSyncRpc('child_process.poll', [record.childId, waitMs]);
}

function isChildProcessGoneError(error) {
  return error != null && typeof error === 'object' && error.code === 'ECHILD';
}

function resolveSyntheticGuestPath(value, fromGuestDir = '/') {
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

function resolveSyntheticHostPath(value, fromGuestDir = '/') {
  const mapping = resolveSyntheticHostMapping(value, fromGuestDir);
  return mapping?.hostPath ?? null;
}

function resolveSyntheticHostMapping(value, fromGuestDir = '/') {
  const guestPath = resolveSyntheticGuestPath(value, fromGuestDir);
  if (typeof guestPath !== 'string') {
    return null;
  }
  return resolveModuleGuestPathToHostMapping(guestPath);
}

function chmodMappedGuestPath(guestPath, hostPath, mode) {
  fsModule.chmodSync(hostPath, mode);
  try {
    if (typeof guestPath === 'string' && guestPath.length > 0) {
      fsModule.chmodSync(guestPath, mode);
    }
  } catch {
    // Best effort: host-mapped paths may not also exist as direct kernel paths.
  }
}

function maybeCreateSyntheticCommandResult(command, args, cwd) {
  if (SIDECAR_MANAGED_PROCESS) return null;
  const basename = path.posix.basename(String(command || ''));

  if (basename === 'chmod') {
    if (args.length < 2 || !args.every((arg) => typeof arg === 'string')) {
      return null;
    }
    const modeArg = args[0];
    if (!/^[0-7]{3,4}$/.test(modeArg)) {
      return null;
    }
    const mode = Number.parseInt(modeArg, 8) >>> 0;
    try {
      for (const targetArg of args.slice(1)) {
        const guestPath = resolveSyntheticGuestPath(targetArg, cwd || '/');
        const mapping = resolveSyntheticHostMapping(targetArg, cwd || '/');
        if (!mapping || typeof mapping.hostPath !== 'string') {
          throw new Error(`No such file or directory: ${targetArg}`);
        }
        if (mapping.readOnly) {
          const error = new Error(`Read-only file system: ${targetArg}`);
          error.code = 'EROFS';
          throw error;
        }
        chmodMappedGuestPath(guestPath, mapping.hostPath, mode);
      }
      return { exitCode: 0, stdout: '', stderr: '' };
    } catch (error) {
      return {
        exitCode: 1,
        stdout: '',
        stderr: `chmod: ${error instanceof Error ? error.message : String(error)}\n`,
      };
    }
  }

  if (basename === 'stat') {
    if (
      args.length === 3 &&
      args[0] === '-c' &&
      (args[1] === '%a' || args[1] === '"%a"') &&
      typeof args[2] === 'string'
    ) {
      try {
        const hostPath = resolveSyntheticHostPath(args[2], cwd || '/');
        if (typeof hostPath !== 'string') {
          return null;
        }
        const stat = fsModule.statSync(hostPath);
        const mode = Number(stat?.mode) >>> 0;
        return {
          exitCode: 0,
          stdout: `${(mode & 0o777).toString(8)}\n`,
          stderr: '',
        };
      } catch {
        return null;
      }
    }
    return null;
  }

  return null;
}

function createSyntheticChildRecord(result, stdinTarget, stdoutTarget, stderrTarget) {
  const pid = nextSyntheticChildPid++;
  const childId = `synthetic-child-${pid}`;
  const pendingEvents = [{
    type: 'exit',
    exitCode: Number(result?.exitCode ?? 1) >>> 0,
    signal: null,
  }];

  return {
    childId,
    pid,
    stdinFd: stdinTarget,
    stdoutFd: stdoutTarget,
    stderrFd: stderrTarget,
    stdinPipe: null,
    stdoutPipe: null,
    stderrPipe: null,
    delegateRetainedFds: [],
    exitCode: null,
    exitSignal: null,
    exitStatus: null,
    rawWaitStatus: null,
    processGroup: 0,
    pendingEvents,
    synthetic: true,
  };
}

function emitSyntheticCommandOutput(record, result) {
  const syntheticOutputs = [
    ['stdout', record.stdoutFd, result?.stdout],
    ['stderr', record.stderrFd, result?.stderr],
  ];

  for (const [stream, targetFd, value] of syntheticOutputs) {
    const text = typeof value === 'string' ? value : '';
    const pipe = registerPipeProducer(targetFd, record.childId, stream);
    if (text.length > 0 && targetFd !== 0xffffffff) {
      routeChunkToFd(targetFd, Buffer.from(text, 'utf8'));
    }
    if (pipe) {
      unregisterPipeProducer(pipe, `${record.childId}:${stream}`);
    }
  }
}

function reapSpawnedChild(record) {
  if (!record) {
    return;
  }

  childCorrelationsByPid.delete(record.pid);
  if (typeof record.childId === 'string' && record.childId.length > 0) {
    childCorrelationsById.delete(record.childId);
  }
}

function returnWaitedChild(
  record,
  retExitCodePtr,
  retSignalPtr,
  retPidPtr,
  retCoreDumpedPtr,
) {
  if (!guestRangesAreValid(
    [retExitCodePtr, 4],
    [retSignalPtr, 4],
    [retPidPtr, 4],
    [retCoreDumpedPtr, 4],
  )) {
    return WASI_ERRNO_FAULT;
  }
  // A successful wait may reap the child that generated SIGCHLD. Linux runs
  // the caught handler before returning the status without rewriting that
  // successful wait result to EINTR.
  dispatchPendingWasmSignals();
  if (writeGuestUint32(retExitCodePtr, record.exitCode ?? 0) !== WASI_ERRNO_SUCCESS) {
    return WASI_ERRNO_FAULT;
  }
  if (writeGuestUint32(retSignalPtr, record.exitSignal ?? 0) !== WASI_ERRNO_SUCCESS) {
    return WASI_ERRNO_FAULT;
  }
  if (writeGuestUint32(retCoreDumpedPtr, record.coreDumped ? 1 : 0) !== WASI_ERRNO_SUCCESS) {
    return WASI_ERRNO_FAULT;
  }
  const writePidResult = writeGuestUint32(retPidPtr, record.pid);
  if (writePidResult === WASI_ERRNO_SUCCESS) {
    reapSpawnedChild(record);
  }
  return writePidResult;
}

function returnLegacyWaitedChild(record, retStatusPtr, retPidPtr) {
  if (!guestRangesAreValid([retStatusPtr, 4], [retPidPtr, 4])) {
    return WASI_ERRNO_FAULT;
  }
  dispatchPendingWasmSignals();
  if (writeGuestUint32(retStatusPtr, record.exitStatus ?? 0) !== WASI_ERRNO_SUCCESS) {
    return WASI_ERRNO_FAULT;
  }
  const writePidResult = writeGuestUint32(retPidPtr, record.pid);
  if (writePidResult === WASI_ERRNO_SUCCESS) {
    reapSpawnedChild(record);
  }
  return writePidResult;
}

function returnRawWaitedChild(record, retStatusPtr, retPidPtr) {
  if (!guestRangesAreValid([retStatusPtr, 4], [retPidPtr, 4])) {
    return WASI_ERRNO_FAULT;
  }
  dispatchPendingWasmSignals();
  if (writeGuestUint32(retStatusPtr, record.rawWaitStatus ?? 0) !== WASI_ERRNO_SUCCESS) {
    return WASI_ERRNO_FAULT;
  }
  const writePidResult = writeGuestUint32(retPidPtr, record.pid);
  if (writePidResult === WASI_ERRNO_SUCCESS) {
    reapSpawnedChild(record);
  }
  return writePidResult;
}

function processChildEvent(record, event) {
  if (!event) {
    return false;
  }
  traceHostProcess('child-event', {
    childId: record?.childId ?? null,
    pid: record?.pid ?? null,
    type: event.type,
    exitCode: event.exitCode ?? null,
    signal: event.signal ?? null,
  });

  if (event.type === 'stdout' && record.stdoutFd !== 0xffffffff) {
    const chunk = decodeSyncRpcValue(event.data);
    if (chunk?.length > 0) {
      routeChunkToFd(record.stdoutFd, chunk);
    }
    return true;
  }

  if (event.type === 'stderr' && record.stderrFd !== 0xffffffff) {
    const chunk = decodeSyncRpcValue(event.data);
    if (chunk?.length > 0) {
      routeChunkToFd(record.stderrFd, chunk);
    }
    return true;
  }

  if (event.type === 'signal') {
    dispatchWasmSignal(
      typeof event.number === 'number' ? event.number : signalNumberFromName(event.signal),
    );
    return true;
  }

  if (event.type === 'exit') {
    const exitCode =
      typeof event.exitCode === 'number' ? Math.trunc(event.exitCode) : null;
    const signal =
      typeof event.signal === 'string' ? event.signal : null;
    // The child-process bridge emits exit only after both output streams have
    // reached EOF. Do not infer EOF from one empty zero-time poll: that races
    // delayed pipe delivery and can truncate the final output chunk.
    finalizeChildExit(record, exitCode, signal, event.coreDumped === true);
    return true;
  }

  return false;
}

function childBridgeTerminal(record) {
  return SIDECAR_MANAGED_PROCESS
    ? record?.terminalEventObserved === true
    : typeof record?.exitStatus === 'number';
}

function pumpPipeProducers(pipe, waitMs) {
  let processed = false;
  for (const [producerKey, producer] of Array.from(pipe.producers.entries())) {
    const record = childCorrelationsById.get(producer.childId);
    if (!record) {
      unregisterPipeProducer(pipe, producerKey);
      continue;
    }
    if (childBridgeTerminal(record)) {
      unregisterPipeProducer(pipe, producerKey);
      continue;
    }

    processed = pumpChildInputPipe(record, 0) || processed;

    const event = pollChildEvent(record, waitMs);
    if (!event) {
      continue;
    }

    processed = true;
    processChildEvent(record, event);
  }

  return processed;
}

function pumpChildInputPipe(record, waitMs) {
  const inputPipe = resolveChildInputPipe(record);
  if (!inputPipe) {
    traceHostProcess('pump-child-input-skip-no-pipe', {
      childId: record?.childId ?? null,
    });
    return false;
  }
  if (record.pumpingInputPipe === true) {
    return false;
  }
  record.pumpingInputPipe = true;
  try {
    const stdinReadyAt = Number(record?.stdinReadyAtMs) || 0;
    if (stdinReadyAt > Date.now()) {
      traceHostProcess('pump-child-input-deferred', {
        childId: record?.childId ?? null,
        waitMs: Number(waitMs) >>> 0,
        stdinReadyAt,
        now: Date.now(),
        chunkCount: inputPipe.chunks.length,
        writeHandleCount: inputPipe.writeHandleCount ?? null,
        producerCount: inputPipe.producers?.size ?? null,
      });
      return false;
    }

    let progressed = false;
    traceHostProcess('pump-child-input-begin', {
      childId: record?.childId ?? null,
      waitMs: Number(waitMs) >>> 0,
      chunkCount: inputPipe.chunks.length,
      writeHandleCount: inputPipe.writeHandleCount ?? null,
      producerCount: inputPipe.producers?.size ?? null,
    });
    if (inputPipe.chunks.length > 0) {
      progressed = flushPipeConsumers(inputPipe) || progressed;
    }

    if (inputPipe.producers.size === 0 && (inputPipe.writeHandleCount ?? 0) === 0) {
      return closePipeConsumers(inputPipe) || progressed;
    }

    const pumped = pumpPipeProducers(inputPipe, waitMs);
    progressed = pumped || progressed;
    if (inputPipe.chunks.length > 0) {
      progressed = flushPipeConsumers(inputPipe) || progressed;
    }
    if (inputPipe.producers.size === 0 && (inputPipe.writeHandleCount ?? 0) === 0) {
      progressed = closePipeConsumers(inputPipe) || progressed;
    }

    return progressed;
  } finally {
    record.pumpingInputPipe = false;
  }
}

function pumpSpawnedChildren(waitMs) {
  if (SIDECAR_MANAGED_PROCESS) {
    // Managed child lifecycle and stream routing are sidecar-owned. Pulling
    // child events here races the global process pump and deadlocks when this
    // guest is itself parked in a direct host call. Collection is safe here:
    // it only queries kernel-owned lifecycle state from the guest thread.
    collectStaleManagedChildCorrelations();
    return false;
  }
  const records = Array.from(childCorrelationsByPid.values()).filter(
    (record) => record && !childBridgeTerminal(record),
  );
  if (records.length === 0) {
    return false;
  }

  const boundedWaitMs = Math.max(0, Number(waitMs) || 0);
  const startIndex =
    boundedWaitMs > 0 ? nextBlockingChildPumpIndex % records.length : 0;
  if (boundedWaitMs > 0) {
    // One child receives the sweep's blocking quantum. Rotate that slot so a
    // busy early child cannot permanently make later siblings zero-poll only.
    nextBlockingChildPumpIndex = (startIndex + 1) % records.length;
  }

  let progressed = false;
  for (let offset = 0; offset < records.length; offset += 1) {
    const record = records[(startIndex + offset) % records.length];
    try {
      // Bound the whole sweep to one blocking poll instead of waitMs per
      // child. Every other live child still receives a zero-time service pass.
      const event = pollChildEvent(record, offset === 0 ? boundedWaitMs : 0);
      if (event) {
        processChildEvent(record, event);
        progressed = true;
      }
      progressed = pumpChildInputPipe(record, 0) || progressed;
    } catch (error) {
      if (!isChildProcessGoneError(error)) {
        throw error;
      }
    }
  }
  return progressed;
}

function pumpSpawnedChildrenOrWait(waitMs) {
  const boundedWaitMs = Math.max(1, Number(waitMs) >>> 0);
  if (SIDECAR_MANAGED_PROCESS) {
    callSyncRpc('process.sleep', [boundedWaitMs]);
    return false;
  }
  const progressed = pumpSpawnedChildren(boundedWaitMs);
  if (!progressed) {
    Atomics.wait(syntheticWaitArray, 0, 0, boundedWaitMs);
  }
  return progressed;
}

function pumpSpawnedChildrenOrWaitRestartable(waitMs) {
  try {
    pumpSpawnedChildrenOrWait(waitMs);
    return false;
  } catch (error) {
    if (error?.code !== 'EINTR') throw error;
    // The sidecar wakes managed sleeps with typed EINTR. Run the handler on
    // the guest thread, then tell the caller whether its syscall must return or
    // may be reissued under SA_RESTART.
    return dispatchPendingWasmSignals(true);
  }
}

function encodeGuestBytes(value) {
  return new TextEncoder().encode(String(value));
}

function readGuestBytes(ptr, len) {
  if (!(instanceMemory instanceof WebAssembly.Memory)) {
    throw execError('EFAULT', 'WebAssembly memory is not available');
  }

  const start = Number(ptr) >>> 0;
  const length = Number(len) >>> 0;
  const end = start + length;
  if (
    !Number.isSafeInteger(end) ||
    start > instanceMemory.buffer.byteLength ||
    end > instanceMemory.buffer.byteLength
  ) {
    throw execError('EFAULT', 'guest byte range is outside WebAssembly memory');
  }
  return Buffer.from(new Uint8Array(instanceMemory.buffer, start, length));
}

function readGuestString(ptr, len) {
  return readGuestBytes(ptr, len).toString('utf8');
}

function decodeNullSeparatedStrings(buffer) {
  if (!buffer || buffer.length === 0) {
    return [];
  }

  const entries = buffer.toString('utf8').split('\0');
  // The serializer terminates every string, including an empty string, with
  // NUL. Remove exactly that framing terminator while preserving empty argv
  // entries in every other position (Linux permits argv[i] == "").
  if (entries.at(-1) === '') {
    entries.pop();
  }
  return entries;
}

function parseSerializedEnv(buffer) {
  const env = {};
  for (const entry of decodeNullSeparatedStrings(buffer)) {
    const delimiter = entry.indexOf('=');
    if (delimiter <= 0) {
      continue;
    }
    env[entry.slice(0, delimiter)] = entry.slice(delimiter + 1);
  }
  return env;
}

function encodeSyncRpcValue(value) {
  if (
    value == null ||
    typeof value === 'string' ||
    typeof value === 'number' ||
    typeof value === 'boolean'
  ) {
    return value;
  }

  if (typeof Buffer === 'function' && Buffer.isBuffer(value)) {
    return {
      __agentOSType: 'bytes',
      base64: value.toString('base64'),
    };
  }

  if (ArrayBuffer.isView(value)) {
    return {
      __agentOSType: 'bytes',
      base64: Buffer.from(value.buffer, value.byteOffset, value.byteLength).toString('base64'),
    };
  }

  if (value instanceof ArrayBuffer) {
    return {
      __agentOSType: 'bytes',
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

  if (Buffer.isBuffer(value)) {
    return value;
  }

  if (ArrayBuffer.isView(value)) {
    return Buffer.from(value.buffer, value.byteOffset, value.byteLength);
  }

  if (value && typeof value === 'object') {
    if (value.__type === 'Buffer' && typeof value.data === 'string') {
      return Buffer.from(value.data, 'base64');
    }

    if (value.__agentOSType === 'bytes' && typeof value.base64 === 'string') {
      return Buffer.from(value.base64, 'base64');
    }

    return Object.fromEntries(
      Object.entries(value).map(([key, entry]) => [key, decodeSyncRpcValue(entry)]),
    );
  }

  return value;
}

// AGENTOS_SYNC_RPC_RESPONSE_LIMIT_HELPERS_BEGIN
function syncRpcResponseLineLimitError(limit, observed) {
  const limitName = 'limits.reactor.maxBridgeResponseBytes';
  const error = new Error(
    `WASM sync RPC response line exceeds ${limitName} (${limit} encoded bytes); ` +
      `raise ${limitName} if larger host replies are required`,
  );
  error.code = 'ERR_AGENTOS_RESOURCE_LIMIT';
  error.details = {
    limitName,
    limit,
    observed,
    resource: 'syncRpcResponseLineBytes',
  };
  return error;
}

function appendSyncRpcResponseChunk(buffer, chunk, limit) {
  const newlineIndex = chunk.indexOf(0x0a);
  const lineChunkBytes = newlineIndex >= 0 ? newlineIndex : chunk.byteLength;
  const observedLineBytes = buffer.byteLength + lineChunkBytes;
  if (observedLineBytes > limit) {
    throw syncRpcResponseLineLimitError(limit, observedLineBytes);
  }

  if (newlineIndex < 0) {
    return {
      buffer: Buffer.concat([buffer, chunk], observedLineBytes),
      line: null,
      observedLineBytes,
    };
  }

  const suffix = chunk.subarray(newlineIndex + 1);
  if (suffix.byteLength > limit) {
    throw syncRpcResponseLineLimitError(limit, suffix.byteLength);
  }
  const line = buffer.byteLength === 0
    ? chunk.subarray(0, newlineIndex).toString('utf8')
    : Buffer.concat([buffer, chunk.subarray(0, newlineIndex)], observedLineBytes).toString('utf8');
  return {
    buffer: Buffer.from(suffix),
    line,
    observedLineBytes,
  };
}
// AGENTOS_SYNC_RPC_RESPONSE_LIMIT_HELPERS_END

function observeSyncRpcResponseLineBytes(observed) {
  const warnAt = Math.max(1, Math.ceil(maxSyncRpcResponseLineBytes * 0.8));
  if (warnedSyncRpcResponseLineBytes || observed < warnAt) return;
  warnedSyncRpcResponseLineBytes = true;
  if (typeof process?.stderr?.write === 'function') {
    process.stderr.write(
      `[agentos] WASM sync RPC response line usage ${observed}/${maxSyncRpcResponseLineBytes} ` +
        'encoded bytes is near limits.reactor.maxBridgeResponseBytes; ' +
        'raise limits.reactor.maxBridgeResponseBytes if larger host replies are required\n',
    );
  }
}

function readSyncRpcLine() {
  while (true) {
    const newlineIndex = syncRpcResponseBuffer.indexOf(0x0a);
    if (newlineIndex >= 0) {
      observeSyncRpcResponseLineBytes(newlineIndex);
      const line = syncRpcResponseBuffer.subarray(0, newlineIndex).toString('utf8');
      syncRpcResponseBuffer = Buffer.from(syncRpcResponseBuffer.subarray(newlineIndex + 1));
      return line;
    }

    const remaining = maxSyncRpcResponseLineBytes - syncRpcResponseBuffer.byteLength;
    const readCapacity = Math.min(4096, remaining > 4095 ? 4096 : remaining + 1);
    const chunk = Buffer.alloc(readCapacity);
    const bytesRead = readSync(NODE_SYNC_RPC_RESPONSE_FD, chunk, 0, chunk.length, null);
    if (bytesRead === 0) {
      throw new Error('agentos WASM sync RPC response channel closed unexpectedly');
    }
    const appended = appendSyncRpcResponseChunk(
      syncRpcResponseBuffer,
      chunk.subarray(0, bytesRead),
      maxSyncRpcResponseLineBytes,
    );
    observeSyncRpcResponseLineBytes(appended.observedLineBytes);
    syncRpcResponseBuffer = appended.buffer;
    if (appended.line !== null) return appended.line;
  }
}

let activeSpawnCallContext = null;

function currentKernelSignalMask() {
  try {
    const response = callSyncRpc('process.signal_mask', [3, []]);
    return Array.isArray(response?.signals) ? response.signals : [];
  } catch (error) {
    if (error?.code === 'ERR_AGENTOS_WASM_SYNC_RPC_UNAVAILABLE') {
      return [];
    }
    throw error;
  }
}

function callSyncRpc(method, args = []) {
  if (
    globalThis.__agentOSSyncRpc &&
    typeof globalThis.__agentOSSyncRpc.callSync === 'function'
  ) {
    const startedNs = __agentOSWasmNowNs();
    try {
      // Give the V8 bridge the same explicit, engine-neutral byte projection
      // used by the pipe fallback. Passing a Node Buffer directly makes its
      // serialization depend on the bridge serializer and previously lost
      // binary arguments for newly typed syscall methods such as sendmsg.
      return decodeSyncRpcValue(
        globalThis.__agentOSSyncRpc.callSync(method, encodeSyncRpcValue(args)),
      );
    } finally {
      __agentOSWasiRecordSyncRpc(method, 'glue', startedNs);
    }
  }

  if (!NODE_SYNC_RPC_ENABLE || NODE_SYNC_RPC_REQUEST_FD == null || NODE_SYNC_RPC_RESPONSE_FD == null) {
    const error = new Error(`agentos WASM sync RPC is unavailable for ${method}`);
    error.code = 'ERR_AGENTOS_WASM_SYNC_RPC_UNAVAILABLE';
    throw error;
  }

  const startedNs = __agentOSWasmNowNs();
  try {
    const payload = JSON.stringify({
      id: nextSyncRpcId++,
      method,
      args: encodeSyncRpcValue(args),
    });
    writeSync(NODE_SYNC_RPC_REQUEST_FD, `${payload}\n`);

    const response = JSON.parse(readSyncRpcLine());
    if (response?.ok) {
      return decodeSyncRpcValue(response.result);
    }

    const error = new Error(
      response?.error?.message || `agentos WASM sync RPC ${method} failed`,
    );
    if (typeof response?.error?.code === 'string') {
      error.code = response.error.code;
    }
    if (response?.error?.details !== undefined && response.error.details !== null) {
      error.details = decodeSyncRpcValue(response.error.details);
    }
    throw error;
  } finally {
    __agentOSWasiRecordSyncRpc(method, 'pipe', startedNs);
  }
}

// Standalone Node execution has no kernel descriptor table, so its compatibility
// fallback remains fd-keyed. Managed execution never consults this map: kernel
// file descriptions own allocation, aliasing, inheritance, and close lifetime.
const standaloneHostNetSockets = new Map();
const initialManagedHostNetDescriptionIds = new Set(
  SIDECAR_MANAGED_PROCESS
    ? initialHostNetDescriptions
        .map((entry) => String(entry?.descriptionId ?? ''))
        .filter((descriptionId) => /^[0-9]+$/.test(descriptionId))
    : [],
);
for (const inherited of initialHostNetDescriptions) {
  const metadata = inherited.metadata && typeof inherited.metadata === 'object'
    ? inherited.metadata
    : {};
  const socket = {
    domain: Number(metadata.domain) >>> 0,
    sockType: Number(metadata.socketType) >>> 0,
    protocol: Number(metadata.protocol) >>> 0,
    bindOptions: metadata.bindOptions ?? null,
    localInfo: metadata.localInfo ?? null,
    localUnixAddress: metadata.localUnixAddress ?? null,
    localReservation: metadata.localReservation ?? null,
    remoteInfo: metadata.remoteInfo ?? null,
    remoteUnixAddress: metadata.remoteUnixAddress ?? null,
    listening: metadata.listening === true,
    serverId: inherited.serverId ?? null,
    socketId: inherited.socketId ?? null,
    udpSocketId: inherited.udpSocketId ?? null,
    pendingDatagram: null,
    recvTimeoutMs: metadata.recvTimeoutMs ?? null,
    readChunks: [],
    pendingAccepts: [],
    readableEnded: false,
    closed: false,
    lastError: null,
    nonblock: metadata.nonblocking === true,
  };
  if (!SIDECAR_MANAGED_PROCESS) {
    for (const rawFd of inherited.guestFds) {
      standaloneHostNetSockets.set(Number(rawFd) >>> 0, socket);
    }
  }
}
let warnedAboutOpenFdLimit = false;

function runnerOpenFdSet() {
  const openFds = new Set([0, 1, 2]);
  for (const table of [
    activeFdProjections,
    passthroughHandles,
    retainedSpawnOutputHandlesByFd,
    retainedSyntheticHandlesByDisplayFd,
    standaloneDelegateFdRefCounts,
    standaloneHostNetSockets,
    wasi?.fdTable,
  ]) {
    if (!table || typeof table.keys !== 'function') continue;
    for (const fd of table.keys()) openFds.add(Number(fd) >>> 0);
  }
  return openFds;
}

function hasRunnerOpenFdCapacity(additionalFds) {
  const openCount = runnerOpenFdSet().size;
  const nofileSoft = currentNofileSoftLimit();
  const warnAt = Math.max(1, Math.floor(nofileSoft * 0.9));
  if (!warnedAboutOpenFdLimit && openCount >= warnAt) {
    warnedAboutOpenFdLimit = true;
    process.stderr.write(
      `[agentos] WASM open fd usage ${openCount}/${nofileSoft} is near RLIMIT_NOFILE; raise the soft limit or limits.resources.maxOpenFds if needed\n`,
    );
  }
  return openCount + Math.max(0, Number(additionalFds) >>> 0) <= nofileSoft;
}
// Host-net socket fds must stay BELOW the guests' FD_SETSIZE (1024 in the
// wasi-libc sysroot): libcurl's select-based Curl_poll / curl_multi_fdset
// guard every socket with `s < FD_SETSIZE` and silently drop larger fds from
// the pollset, which stalls any transfer that has to WAIT for socket
// readiness (non-blocking TLS handshakes, >16 KiB uploads). Allocate the
// lowest free guest descriptor, as Linux does, while keeping it below both
// FD_SETSIZE and the process's current RLIMIT_NOFILE soft limit.
const HOST_NET_SOCKET_FD_MAX = 1023;
const HOST_NET_TIMEOUT_SENTINEL = '__agentos_net_timeout__';
const HOST_NET_MSG_PEEK = 0x0002;
const HOST_NET_MSG_DONTWAIT = 0x0040;
const HOST_NET_MSG_TRUNC = 0x0020;

function isHostNetWouldBlock(value) {
  return value === HOST_NET_TIMEOUT_SENTINEL || (
    value != null &&
    typeof value === 'object' &&
    value.kind === 'wouldBlock'
  );
}

function waitManagedHostNetReadable(socket, timeoutMs, restartableOperation) {
  const numericTimeoutMs = Number(timeoutMs);
  const deadline = timeoutMs == null || numericTimeoutMs < 0
    ? null
    : Date.now() + (Number.isFinite(numericTimeoutMs) ? numericTimeoutMs : 0);
  for (;;) {
    if (dispatchPendingWasmSignals(restartableOperation === true)) {
      return WASI_ERRNO_INTR;
    }
    const remaining = deadline == null
      ? SPAWNED_CHILD_WAIT_SLICE_MS
      : Math.max(0, deadline - Date.now());
    if (deadline == null || remaining > 0) {
      // Keep runner-side signal and child-lifecycle checkpoints between
      // sidecar waits. Managed child execution stays sidecar-owned; this pump
      // performs only the collection work appropriate to the current mode.
      pumpSpawnedChildren(0);
    }
    const waitMs = deadline == null
      ? SPAWNED_CHILD_WAIT_SLICE_MS
      : Math.min(SPAWNED_CHILD_WAIT_SLICE_MS, Math.max(0, deadline - Date.now()));
    try {
      const response = callSyncRpc('process.posix_poll', [[{
        fd: managedHostNetTargetFd(socket),
        events: 0x001 | 0x040,
      }], waitMs, null]);
      if (Number(response?.readyCount) > 0) return true;
    } catch (error) {
      if (error?.code !== 'EINTR') throw error;
      const mustInterrupt = dispatchPendingWasmSignals(restartableOperation === true);
      if (restartableOperation !== true || mustInterrupt) return WASI_ERRNO_INTR;
    }
    if (deadline != null && Date.now() >= deadline) return false;
  }
}

function getHostNetSocket(fd) {
  const numericFd = Number(fd) >>> 0;
  if (!SIDECAR_MANAGED_PROCESS) {
    return standaloneHostNetSockets.get(numericFd) ?? null;
  }
  const handle = lookupFdHandle(numericFd);
  if (
    handle?.kind !== 'kernel-fd' ||
    typeof handle.hostNetDescriptionId !== 'string'
  ) return null;
  return {
    managed: true,
    guestFd: numericFd,
    targetFd: Number(handle.targetFd) >>> 0,
  };
}

function hasHostNetSocket(fd) {
  return getHostNetSocket(fd) != null;
}

function attachManagedHostNetDescription(guestFd, descriptionId) {
  const normalized = String(descriptionId ?? '');
  if (!/^[0-9]+$/.test(normalized)) {
    const error = new Error('host-network fd does not match a managed kernel description');
    error.code = 'EIO';
    throw error;
  }
  const handle = lookupFdHandle(guestFd);
  if (handle?.kind !== 'kernel-fd') {
    const error = new Error('managed host-network fd is not kernel-backed');
    error.code = 'EIO';
    throw error;
  }
  handle.hostNetDescriptionId = normalized;
  return guestFd;
}

function copyManagedHostNetDescription(sourceFd, targetFd) {
  if (!SIDECAR_MANAGED_PROCESS) return;
  const source = lookupFdHandle(Number(sourceFd) >>> 0);
  if (typeof source?.hostNetDescriptionId !== 'string') return;
  const target = lookupFdHandle(Number(targetFd) >>> 0);
  if (target?.kind === 'kernel-fd') {
    target.hostNetDescriptionId = source.hostNetDescriptionId;
  }
}

function hostNetGuestFds() {
  if (!SIDECAR_MANAGED_PROCESS) return [...standaloneHostNetSockets.keys()];
  return [...passthroughHandles, ...activeFdProjections]
    .filter(([, handle]) =>
      handle?.kind === 'kernel-fd' &&
      typeof handle.hostNetDescriptionId === 'string'
    )
    .map(([guestFd]) => Number(guestFd) >>> 0);
}

function registerNewHostNetSocket(socket) {
  if (!SIDECAR_MANAGED_PROCESS) {
    const fd = allocateHostNetSocketFd();
    if (fd == null) return null;
    standaloneHostNetSockets.set(fd, socket);
    return fd;
  }
  const socketType = Number(socket.sockType) >>> 0;
  const result = callSyncRpc('process.hostnet_fd_open', [
    Number(socket.domain) >>> 0,
    socketType & HOST_NET_SOCKET_TYPE_MASK,
    socket.nonblock === true,
    (socketType & HOST_NET_SOCK_CLOEXEC) !== 0,
  ]);
  const descriptionId = String(result?.descriptionId ?? '');
  if (!/^[0-9]+$/.test(descriptionId)) {
    const error = new Error('kernel returned an invalid host-network description identity');
    error.code = 'EIO';
    throw error;
  }
  let guestFd;
  try {
    guestFd = registerKernelDelegateFd(result?.fd, null, FIRST_SYNTHETIC_FD);
    if (guestFd > HOST_NET_SOCKET_FD_MAX) {
      const error = new Error('no host-network descriptor is available below FD_SETSIZE');
      error.code = 'EMFILE';
      throw error;
    }
    attachManagedHostNetDescription(guestFd, descriptionId);
    if (!SIDECAR_MANAGED_PROCESS && (socketType & HOST_NET_SOCK_CLOEXEC) !== 0) {
      standaloneCloexecFds.add(guestFd);
    }
    return guestFd;
  } catch (error) {
    if (guestFd != null) wasiImport.fd_close(guestFd);
    throw error;
  }
}

function validateHostNetSocketDescriptor(fd, requireListening = false) {
  const numericFd = Number(fd) >>> 0;
  if (SIDECAR_MANAGED_PROCESS) {
    try {
      callSyncRpc('process.hostnet_validate', [
        canonicalKernelFdForSpawnAction(numericFd),
        requireListening === true,
      ]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapHostProcessError(error);
    }
  }
  const socket = getHostNetSocket(numericFd);
  if (socket && !socket.closed) {
    if (!requireListening || (socket.serverId && socket.listening === true)) {
      return WASI_ERRNO_SUCCESS;
    }
    return WASI_ERRNO_INVAL;
  }
  if (lookupFdHandle(numericFd) || standaloneDelegateFdRefCounts.has(numericFd)) {
    return WASI_ERRNO_NOTSOCK;
  }
  return WASI_ERRNO_BADF;
}

function allocateHostNetSocketFd() {
  if (maxSockets != null && new Set(standaloneHostNetSockets.values()).size >= maxSockets) {
    return null;
  }
  if (!hasRunnerOpenFdCapacity(1)) return null;
  const openFds = runnerOpenFdSet();
  const descriptorLimit = Math.min(HOST_NET_SOCKET_FD_MAX + 1, currentNofileSoftLimit());
  for (let fd = FIRST_SYNTHETIC_FD; fd < descriptorLimit; fd += 1) {
    if (!openFds.has(fd)) {
      return fd;
    }
  }
  return null;
}

function allocateHostNetDuplicateFd(minimumFd = 0) {
  const minimum = Number(minimumFd);
  if (!Number.isSafeInteger(minimum) || minimum < 0 || minimum >= LINUX_GUEST_FD_LIMIT) {
    return null;
  }
  if (!hasRunnerOpenFdCapacity(1)) return null;
  const openFds = runnerOpenFdSet();
  const descriptorLimit = Math.min(LINUX_GUEST_FD_LIMIT, currentNofileSoftLimit());
  for (let fd = minimum; fd < descriptorLimit; fd += 1) {
    if (!openFds.has(fd)) return fd;
  }
  return null;
}

function dequeueHostNetBytes(socket, maxBytes) {
  const requested = Math.max(0, Number(maxBytes) >>> 0);
  if (requested === 0) {
    return Buffer.alloc(0);
  }
  if (socket.readChunks.length === 0) {
    socket.readableHint = false;
    return Buffer.alloc(0);
  }

  const parts = [];
  let remaining = requested;
  while (remaining > 0 && socket.readChunks.length > 0) {
    const chunk = socket.readChunks[0];
    if (chunk.length <= remaining) {
      parts.push(chunk);
      socket.readChunks.shift();
      remaining -= chunk.length;
      continue;
    }

    parts.push(chunk.subarray(0, remaining));
    socket.readChunks[0] = chunk.subarray(remaining);
    remaining = 0;
  }

  if (socket.readChunks.length === 0) {
    socket.readableHint = false;
  }

  return Buffer.concat(parts);
}

function peekHostNetBytes(socket, maxBytes) {
  const requested = Math.max(0, Number(maxBytes) >>> 0);
  if (requested === 0 || socket.readChunks.length === 0) {
    return Buffer.alloc(0);
  }

  const parts = [];
  let remaining = requested;
  for (const chunk of socket.readChunks) {
    if (remaining === 0) break;
    const chunkLength = Math.min(chunk.length, remaining);
    parts.push(chunk.subarray(0, chunkLength));
    remaining -= chunkLength;
  }

  return Buffer.concat(parts);
}

function decodeHostNetSocketReadResult(result) {
  if (result == null) {
    return { kind: 'end' };
  }

  if (isHostNetWouldBlock(result)) {
    return { kind: 'timeout' };
  }

  if (typeof result === 'string') {
    return { kind: 'data', bytes: Buffer.from(result, 'base64') };
  }

  const decoded = decodeSyncRpcValue(result);
  if (Buffer.isBuffer(decoded)) {
    return { kind: 'data', bytes: decoded };
  }
  if (decoded == null) {
    return { kind: 'end' };
  }
  if (isHostNetWouldBlock(decoded)) {
    return { kind: 'timeout' };
  }
  return { kind: 'timeout' };
}

function readReadyHostNetSocket(socket, maxBytes = 64 * 1024, peek = false, waitMs = 0) {
  if (!socket?.socketId || socket.closed) {
    socket.readableEnded = true;
    return null;
  }

  const result = decodeHostNetSocketReadResult(
    callSyncRpc('net.socket_read', [
      socket.socketId,
      Math.max(0, Number(maxBytes) >>> 0),
      peek === true,
      Math.max(0, Number(waitMs) >>> 0),
    ]),
  );
  if (result.kind === 'data') {
    // The sidecar owns the OS read quantum and can return more bytes than this
    // guest recv/read requested (TLS commonly asks for its 5-byte record
    // header first). Preserve the entire transport chunk and consume only the
    // requested prefix; dropping the remainder corrupts the byte stream.
    if (result.bytes.length > 0) {
      socket.readChunks.push(Buffer.from(result.bytes));
    }
    const bytes = peek === true
      ? peekHostNetBytes(socket, maxBytes)
      : dequeueHostNetBytes(socket, maxBytes);
    socket.readableHint = socket.readChunks.length > 0;
    return { kind: 'data', bytes };
  }
  // poll(2) readiness is only a snapshot: another read may consume the data
  // before recv(2), which then returns EAGAIN. Do not keep reporting POLLIN
  // from a stale hint after a read attempt observed no bytes.
  socket.readableHint = false;
  if (result.kind === 'end') {
    socket.readableEnded = true;
  }
  return result;
}

function pollHostNetSocket(socket, waitMs) {
  if (!socket?.socketId || socket.closed) {
    return null;
  }

  const event = callSyncRpc('net.poll', [socket.socketId, Math.max(0, Number(waitMs) >>> 0)]);
  if (!event) {
    return null;
  }

  if (event.type === 'data') {
    const chunk = decodeSyncRpcValue(event.data);
    if (chunk?.length > 0) {
      socket.readChunks.push(Buffer.from(chunk));
    }
    socket.readableHint = true;
    return event;
  }

  if (event.type === 'end' || event.type === 'close') {
    socket.readableEnded = true;
    if (event.type === 'close') {
      socket.closed = true;
      socket.socketId = null;
    }
    return event;
  }

  if (event.type === 'error') {
    socket.lastError = {
      code: String(event.code || 'EIO'),
      message: String(event.message || event.code || 'socket error'),
    };
    return event;
  }

  if (event.readable === true || (Number(event.revents) & 0x001) !== 0) {
    // Poll must populate durable runner state without consuming the bytes that
    // the following recv/read call owns.
    return readReadyHostNetSocket(socket, 64 * 1024, true, 0);
  }

  if (event.hangup === true) {
    socket.readableEnded = true;
    return event;
  }

  if (event.error === true) {
    socket.lastError = { code: 'EIO', message: 'socket error' };
    return event;
  }

  return event;
}

function parseHostNetAddress(raw) {
  const value = String(raw ?? '').trim();
  if (!value) {
    throw new Error('host_net address is required');
  }

  if (value.startsWith('[')) {
    const end = value.indexOf(']');
    if (end < 0 || value.charCodeAt(end + 1) !== 58) {
      throw new Error(`invalid host_net address ${value}`);
    }
    return {
      host: value.slice(1, end),
      port: Number.parseInt(value.slice(end + 2), 10),
    };
  }

  const separator = value.lastIndexOf(':');
  if (separator <= 0 || separator === value.length - 1) {
    throw new Error(`invalid host_net address ${value}`);
  }

  return {
    host: value.slice(0, separator),
    port: Number.parseInt(value.slice(separator + 1), 10),
  };
}

function parseHostNetUnixAddress(raw) {
  const value = String(raw ?? '');
  if (value === 'unix-autobind') {
    return { autobind: true };
  }
  if (value.startsWith('unix-abstract:')) {
    const abstractPathHex = value.slice('unix-abstract:'.length);
    if (abstractPathHex.length % 2 !== 0 || !/^[0-9a-f]*$/i.test(abstractPathHex)) {
      throw new Error('invalid abstract host_net Unix address');
    }
    return { abstractPathHex: abstractPathHex.toLowerCase() };
  }
  if (value.startsWith('unix-path-hex:')) {
    const pathHex = value.slice('unix-path-hex:'.length);
    if (pathHex.length % 2 !== 0 || !/^[0-9a-f]*$/i.test(pathHex)) {
      const error = new Error('invalid hexadecimal host_net Unix pathname');
      error.code = 'EINVAL';
      throw error;
    }
    const pathBytes = Buffer.from(pathHex, 'hex');
    const decoded = pathBytes.toString('utf8');
    if (!Buffer.from(decoded, 'utf8').equals(pathBytes)) {
      const error = new Error('AF_UNIX pathname is not valid UTF-8');
      error.code = 'EILSEQ';
      throw error;
    }
    return { path: decoded };
  }
  return value.startsWith('unix:') ? { path: value.slice(5) } : null;
}

function formatHostNetUnixAddress(address) {
  if (typeof address?.abstractPathHex === 'string') {
    return `unix-abstract:${address.abstractPathHex.toLowerCase()}`;
  }
  if (typeof address?.path === 'string') {
    return `unix:${address.path}`;
  }
  return 'unix-unnamed';
}

function managedHostNetAddressValue(raw) {
  const unix = parseHostNetUnixAddress(raw);
  if (unix?.autobind === true) return { type: 'unix-autobind' };
  if (typeof unix?.abstractPathHex === 'string') {
    return { type: 'unix-abstract', hex: unix.abstractPathHex };
  }
  if (typeof unix?.path === 'string') {
    return { type: 'unix-path', path: unix.path };
  }
  const inet = parseHostNetAddress(raw);
  return { type: 'inet', host: inet.host, port: inet.port };
}

function managedHostNetTargetFd(socket) {
  if (socket?.managed !== true || !Number.isInteger(socket.targetFd)) {
    const error = new Error('managed host-network socket projection is invalid');
    error.code = 'EBADF';
    throw error;
  }
  return Number(socket.targetFd) >>> 0;
}

function managedHostNetAddressText(value) {
  if (typeof value?.address === 'string' && Number.isInteger(Number(value?.port))) {
    return formatHostNetAddressInfo(value);
  }
  if (typeof value?.abstractPathHex === 'string') {
    return `unix-abstract:${value.abstractPathHex}`;
  }
  if (typeof value?.path === 'string') return `unix:${value.path}`;
  return 'unix-unnamed';
}

function hostNetUnixNodePath(address) {
  if (typeof address?.abstractPathHex === 'string') {
    return `\0${Buffer.from(address.abstractPathHex, 'hex').toString('utf8')}`;
  }
  return typeof address?.path === 'string' ? address.path : undefined;
}

function unixAddressFromSidecarInfo(info, prefix) {
  const abstractPathHex = info?.[`${prefix}AbstractPathHex`];
  if (typeof abstractPathHex === 'string') {
    return { abstractPathHex };
  }
  const path = info?.[`${prefix}Path`];
  return typeof path === 'string' ? { path } : null;
}

function refreshHostNetUnixSocketInfo(socket) {
  if (!socket?.socketId || Number(socket.domain) !== HOST_NET_AF_UNIX) return;
  let info = callSyncRpc('net.socket_wait_connect', [socket.socketId]);
  if (typeof info === 'string') info = JSON.parse(info);
  const local = unixAddressFromSidecarInfo(info, 'local');
  const remote = unixAddressFromSidecarInfo(info, 'remote');
  socket.localUnixAddress = local
    ? formatHostNetUnixAddress(local)
    : 'unix-unnamed';
  socket.remoteUnixAddress = remote
    ? formatHostNetUnixAddress(remote)
    : 'unix-unnamed';
}

function parseHostNetListenAddress(raw) {
  const value = String(raw ?? '');
  const unixAddress = parseHostNetUnixAddress(value);
  if (unixAddress != null) {
    return unixAddress;
  }
  const inetValue = value.trim();
  if (!inetValue) {
    throw new Error('host_net listen address is required');
  }
  const address = parseHostNetAddress(inetValue);
  return { host: address.host, port: address.port };
}

function normalizeHostNetAddressInfo(address, port) {
  const host = String(address ?? '');
  const numericPort = Number(port);
  if (!host || !Number.isInteger(numericPort) || numericPort < 0 || numericPort > 65535) {
    return null;
  }
  return { address: host, port: numericPort };
}

function formatHostNetAddressInfo(info) {
  const address = String(info?.address ?? '');
  const port = Number(info?.port);
  if (!address || !Number.isInteger(port) || port < 0 || port > 65535) {
    throw new Error('host_net socket address is incomplete');
  }
  return `${address}:${port}`;
}

// These are the AgentOS wasi-libc p1 ABI values, not Linux's numeric values.
// libc serializes Linux-compatible socket behavior over host_net, while the
// private guest/runner boundary retains wasi-libc's AF_INET=1, AF_INET6=2,
// AF_UNIX=3 assignments.
const HOST_NET_AF_INET = 1;
const HOST_NET_AF_INET6 = 2;
const HOST_NET_AF_UNIX = 3;
const HOST_NET_SOCK_DGRAM = 5;
const HOST_NET_SOCK_STREAM = 6;
const HOST_NET_SOCKET_TYPE_MASK = 0xf;
const POSIX_SOCK_STREAM = 1;
const POSIX_SOCK_DGRAM = 2;
// wasi-libc <sys/socket.h>: SOCK_NONBLOCK / SOCK_CLOEXEC bits OR'd into the
// socket(2) type argument (Linux-style socket(..., SOCK_STREAM | SOCK_NONBLOCK)).
const HOST_NET_SOCK_NONBLOCK = 0x4000;
const HOST_NET_SOCK_CLOEXEC = 0x2000;
const HOST_NET_SOL_SOCKET = 1;
const HOST_NET_WASI_SOL_SOCKET = 0x7fffffff;
const HOST_NET_SO_ERROR = 4;
const HOST_NET_SO_RCVTIMEO_64 = 20;
const HOST_NET_SO_RCVTIMEO_32 = 66;
const HOST_NET_TIMEVAL_BYTES = 16;
// Performance/QoS socket options that guests may set but the host transport
// neither needs nor can honor per-socket: Node's net sockets already run
// with sensible defaults, and DSCP/traffic-class marking is not observable
// through the adapter. Accepted and ignored (values from the patched
// wasi-libc headers, matching Linux): setsockopt(2) succeeds, matching a
// Linux host where these are best-effort hints. OpenSSH sets all four on
// every connection (ssh_packet_set_tos / set_nodelay in opacket/misc) and
// treats failure as per-connection stderr noise.
const HOST_NET_SO_KEEPALIVE = 9; // SOL_SOCKET, socket(7)
const HOST_NET_IPPROTO_IP = 0;
const HOST_NET_IP_TOS = 1; // ip(7)
const HOST_NET_IPPROTO_TCP = 6;
const HOST_NET_TCP_NODELAY = 1; // tcp(7)
const HOST_NET_IPPROTO_IPV6 = 41;
const HOST_NET_IPV6_TCLASS = 67; // ipv6(7)

function hostNetSocketBaseType(socket) {
  return Number(socket?.sockType ?? 0) & HOST_NET_SOCKET_TYPE_MASK;
}

function normalizeHostNetSocketType(sockType) {
  const numericType = Number(sockType) >>> 0;
  const flags = numericType & ~HOST_NET_SOCKET_TYPE_MASK;
  switch (numericType & HOST_NET_SOCKET_TYPE_MASK) {
    case POSIX_SOCK_STREAM:
      return flags | HOST_NET_SOCK_STREAM;
    case POSIX_SOCK_DGRAM:
      return flags | HOST_NET_SOCK_DGRAM;
    default:
      return numericType;
  }
}

function hostNetSockoptKind(level, optname, optvalLen) {
  const normalizedLevel = Number(level) >>> 0;
  const normalizedOptname = Number(optname) >>> 0;
  const normalizedOptvalLen = Number(optvalLen) >>> 0;
  // Accept-and-ignore QoS/keepalive/nagle hints (see constant block above).
  // Option values are plain ints; accept any sane small buffer.
  if (normalizedOptvalLen >= 1 && normalizedOptvalLen <= 16) {
    if (
      (normalizedLevel === HOST_NET_SOL_SOCKET ||
        normalizedLevel === HOST_NET_WASI_SOL_SOCKET) &&
      normalizedOptname === HOST_NET_SO_KEEPALIVE
    ) {
      return 'ignore';
    }
    if (
      normalizedLevel === HOST_NET_IPPROTO_TCP &&
      normalizedOptname === HOST_NET_TCP_NODELAY
    ) {
      return 'ignore';
    }
    if (
      normalizedLevel === HOST_NET_IPPROTO_IP &&
      normalizedOptname === HOST_NET_IP_TOS
    ) {
      return 'ignore';
    }
    if (
      normalizedLevel === HOST_NET_IPPROTO_IPV6 &&
      normalizedOptname === HOST_NET_IPV6_TCLASS
    ) {
      return 'ignore';
    }
  }
  if (
    normalizedLevel !== HOST_NET_SOL_SOCKET &&
    normalizedLevel !== HOST_NET_WASI_SOL_SOCKET
  ) {
    return null;
  }
  if (normalizedOptvalLen !== HOST_NET_TIMEVAL_BYTES) {
    return null;
  }
  if (
    normalizedOptname === HOST_NET_SO_RCVTIMEO_64 ||
    normalizedOptname === HOST_NET_SO_RCVTIMEO_32
  ) {
    return 'recv-timeout';
  }
  return null;
}

function parseHostNetTimevalMs(bytes) {
  if (bytes.byteLength !== HOST_NET_TIMEVAL_BYTES) {
    return null;
  }
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const seconds = view.getBigInt64(0, true);
  const microseconds = view.getBigInt64(8, true);
  if (seconds < 0n || microseconds < 0n || microseconds > 999999n) {
    return null;
  }
  if (seconds === 0n && microseconds === 0n) {
    return null;
  }
  const milliseconds = seconds * 1000n + (microseconds + 999n) / 1000n;
  if (milliseconds > BigInt(Number.MAX_SAFE_INTEGER)) {
    return null;
  }
  return Number(milliseconds);
}

function ensureHostNetUdpSocket(socket) {
  if (!socket || socket.closed || hostNetSocketBaseType(socket) !== HOST_NET_SOCK_DGRAM) {
    return null;
  }
  if (socket.udpSocketId) {
    return socket.udpSocketId;
  }

  const type = socket.domain === HOST_NET_AF_INET6 ? 'udp6' : 'udp4';
  const result = callSyncRpc('dgram.createSocket', [{ type }]);
  if (!result || typeof result.socketId !== 'string') {
    throw new Error('host_net dgram socket creation failed');
  }
  socket.udpSocketId = result.socketId;
  return socket.udpSocketId;
}

function pollHostNetDatagram(socket, waitMs) {
  if (socket?.pendingDatagram) {
    return socket.pendingDatagram;
  }
  if (!socket?.udpSocketId || socket.closed) {
    return null;
  }
  const event = callSyncRpc('dgram.poll', [
    socket.udpSocketId,
    Math.max(0, Number(waitMs) >>> 0),
  ]);
  if (event?.type === 'error') {
    socket.lastError = event;
    return event;
  }
  if (event?.type === 'message') {
    // poll(2) must not consume a datagram. Keep exactly one bounded datagram
    // until recv/recvfrom takes it, preserving Linux message boundaries.
    socket.pendingDatagram = event;
    return event;
  }
  return null;
}

function hostNetDatagramBytes(event) {
  if (event?.data && typeof event.data === 'object' && typeof event.data.base64 === 'string') {
    return Buffer.from(event.data.base64, 'base64');
  }
  return decodeFsBytesPayload(event?.data, 'host_net datagram data');
}

function receiveHostNetDatagramEvent(socket, flags) {
  const recvFlags = Number(flags) >>> 0;
  const nonblocking = socket.nonblock || (recvFlags & HOST_NET_MSG_DONTWAIT) !== 0;
  const deadline = nonblocking
    ? Date.now()
    : socket.recvTimeoutMs == null
      ? null
      : Date.now() + Math.max(0, socket.recvTimeoutMs);

  while (true) {
    const waitMs = nonblocking
      ? 0
      : deadline == null
        ? 50
        : Math.max(0, Math.min(50, deadline - Date.now()));
    const event = pollHostNetDatagram(socket, waitMs);
    if (event?.type === 'message') {
      if ((recvFlags & HOST_NET_MSG_PEEK) === 0) {
        socket.pendingDatagram = null;
      }
      return event;
    }
    if (event?.type === 'error' || socket.lastError) {
      throw new Error(event?.message || socket.lastError?.message || 'UDP receive failed');
    }
    if (nonblocking || (deadline != null && Date.now() >= deadline)) {
      return null;
    }
  }
}

function signalNumberFromName(signal) {
  const mapped = LINUX_SIGNAL_NAMES.indexOf(String(signal));
  if (mapped > 0) {
    return mapped;
  }
  if (String(signal).startsWith('SIG')) {
    const numeric = Number.parseInt(String(signal).slice(3), 10);
    return Number.isInteger(numeric) ? numeric : 15;
  }
  return 15;
}

function signalNameFromNumber(signal) {
  const numeric = Number(signal) >>> 0;
  return LINUX_SIGNAL_NAMES[numeric] ?? `SIG${numeric}`;
}

const LINUX_SIGNAL_NAMES = [
  null,
  'SIGHUP',
  'SIGINT',
  'SIGQUIT',
  'SIGILL',
  'SIGTRAP',
  'SIGABRT',
  'SIGBUS',
  'SIGFPE',
  'SIGKILL',
  'SIGUSR1',
  'SIGSEGV',
  'SIGUSR2',
  'SIGPIPE',
  'SIGALRM',
  'SIGTERM',
  null,
  'SIGCHLD',
  'SIGCONT',
  'SIGSTOP',
  'SIGTSTP',
  'SIGTTIN',
  'SIGTTOU',
  'SIGURG',
  'SIGXCPU',
  'SIGXFSZ',
  'SIGVTALRM',
  'SIGPROF',
  'SIGWINCH',
  'SIGIO',
  'SIGPWR',
  'SIGSYS',
];
const LINUX_MAX_SIGNAL_NUMBER = 64;
const MAX_SUPPLEMENTARY_GROUPS = 64;
const MAX_ACCOUNT_RECORD_BYTES = 4096;

function writeGuestBytes(ptr, maxLen, bytes, actualLenPtr) {
  if (!(instanceMemory instanceof WebAssembly.Memory)) {
    return WASI_ERRNO_FAULT;
  }

  try {
    const requestedLength = Number(maxLen) >>> 0;
    const written = Math.min(requestedLength, bytes.byteLength);
    if (!guestRangesAreValid([ptr, written], [actualLenPtr, 4])) {
      return WASI_ERRNO_FAULT;
    }
    const memory = new Uint8Array(instanceMemory.buffer);
    memory.set(bytes.subarray(0, written), Number(ptr));
    return writeGuestUint32(actualLenPtr, written);
  } catch {
    return WASI_ERRNO_FAULT;
  }
}

// Account lookup records follow the reentrant libc contract: publish the
// required size and return ERANGE without a partial record when the caller's
// buffer is short. Validate both output ranges before the first write.
function writeGuestAccountRecord(ptr, maxLen, bytes, actualLenPtr) {
  if (!(instanceMemory instanceof WebAssembly.Memory)) {
    return WASI_ERRNO_FAULT;
  }
  const memoryLength = instanceMemory.buffer.byteLength;
  const outputOffset = Number(ptr) >>> 0;
  const required = bytes.byteLength >>> 0;
  const capacity = Number(maxLen) >>> 0;
  const lengthOffset = Number(actualLenPtr) >>> 0;
  if (lengthOffset > memoryLength - 4) {
    return WASI_ERRNO_FAULT;
  }
  if (capacity >= required && outputOffset > memoryLength - required) {
    return WASI_ERRNO_FAULT;
  }
  if (writeGuestUint32(lengthOffset, required) !== WASI_ERRNO_SUCCESS) {
    return WASI_ERRNO_FAULT;
  }
  if (capacity < required) {
    return WASI_ERRNO_RANGE;
  }
  try {
    new Uint8Array(instanceMemory.buffer).set(bytes, outputOffset);
    return WASI_ERRNO_SUCCESS;
  } catch {
    return WASI_ERRNO_FAULT;
  }
}

// Perform a single NON-BLOCKING accept on a listening host_net socket. On success it
// registers the accepted connection as a new host_net socket and returns
// { acceptedFd, address } (address is a Buffer: "host:port" for TCP, the peer path for
// AF_UNIX), or { error } when accepting the pending connection failed. Returns null when
// no connection is currently pending. Used by both net_poll
// (to report accurate listener readiness) and net_accept (non-blocking semantics) so the
// server never blocks inside accept() and starves already-connected clients.
function tryHostNetAcceptOnce(socket) {
  let result = callSyncRpc('net.server_accept', [socket.serverId]);
  if (!result || isHostNetWouldBlock(result)) {
    return null;
  }
  if (typeof result === 'string') {
    result = JSON.parse(result);
  }
  if (!result || typeof result.socketId !== 'string') {
    return null;
  }

  const localUnix = unixAddressFromSidecarInfo(result.info, 'local') ??
    ((socket.bindOptions?.path != null || socket.bindOptions?.abstractPathHex != null)
      ? socket.bindOptions
      : null);
  const remoteUnix = unixAddressFromSidecarInfo(result.info, 'remote');
  const acceptedSocket = {
    domain: socket.domain,
    // Linux accept(2) does not inherit SOCK_CLOEXEC or O_NONBLOCK from the
    // listener; accept4(2) supplies those flags explicitly.
    sockType: Number(socket.sockType) & HOST_NET_SOCKET_TYPE_MASK,
    protocol: socket.protocol,
    bindOptions: null,
    localInfo: normalizeHostNetAddressInfo(result.info?.localAddress, result.info?.localPort),
    localUnixAddress: localUnix ? formatHostNetUnixAddress(localUnix) : null,
    localReservation: null,
    remoteInfo: normalizeHostNetAddressInfo(result.info?.remoteAddress, result.info?.remotePort),
    remoteUnixAddress: remoteUnix ? formatHostNetUnixAddress(remoteUnix) : 'unix-unnamed',
    listening: false,
    serverId: null,
    socketId: result.socketId,
    udpSocketId: null,
    pendingDatagram: null,
    recvTimeoutMs: socket.recvTimeoutMs,
    readChunks: [],
    readableEnded: false,
    closed: false,
    lastError: null,
  };
  let registeredAcceptedFd;
  try {
    registeredAcceptedFd = registerNewHostNetSocket(acceptedSocket);
  } catch (error) {
    callSyncRpc('net.destroy', [result.socketId]);
    return { error: mapHostProcessError(error) };
  }
  if (registeredAcceptedFd == null) {
    callSyncRpc('net.destroy', [result.socketId]);
    return { error: WASI_ERRNO_MFILE };
  }

  let address;
  if (result.info?.remoteAddress != null && result.info?.remotePort != null) {
    address = Buffer.from(formatHostNetAddressInfo({
      address: result.info.remoteAddress,
      port: result.info.remotePort,
    }), 'utf8');
  } else {
    address = Buffer.from(remoteUnix ? formatHostNetUnixAddress(remoteUnix) : 'unix-unnamed', 'utf8');
  }
  return { acceptedFd: registeredAcceptedFd, address };
}

function cleanupAcceptedHostNetSocket(accepted, reason) {
  const acceptedFd = Number(accepted?.acceptedFd);
  if (!Number.isInteger(acceptedFd)) return null;
  const acceptedSocket = getHostNetSocket(acceptedFd);
  if (!acceptedSocket?.socketId) return null;
  const result = wasiImport.fd_close(acceptedFd);
  if (result === WASI_ERRNO_SUCCESS) return null;
  try {
    const error = new Error(`close returned WASI errno ${result}`);
    error.code = 'EIO';
    process.stderr.write(
      `[agentos] failed to destroy accepted socket during ${reason}: ${error instanceof Error ? error.message : String(error)}\n`,
    );
    return error;
  } catch (error) {
    return error;
  }
}

function cleanupDetachedHostNetSocket(numericFd, socket) {
  let firstError = null;
  const cleanup = (label, operation) => {
    try {
      operation();
    } catch (error) {
      if (firstError == null) firstError = error;
      process.stderr.write(
        `[agentos] failed to ${label} while closing host_net fd ${numericFd}: ${error instanceof Error ? error.message : String(error)}\n`,
      );
    }
  };
  if (Array.isArray(socket.pendingAccepts)) {
    for (const accepted of socket.pendingAccepts.splice(0)) {
      const error = cleanupAcceptedHostNetSocket(accepted, 'listener close');
      if (firstError == null && error != null) firstError = error;
    }
  }
  if (socket.localReservation != null) {
    cleanup('release TCP port reservation', () => {
      callSyncRpc('net.release_tcp_port', [socket.localReservation]);
    });
  }
  if (socket.socketId && !socket.closed) {
    cleanup('destroy connected socket', () => {
      callSyncRpc('net.destroy', [socket.socketId]);
    });
  }
  if (socket.serverId) {
    cleanup('close listener', () => {
      callSyncRpc('net.server_close', [socket.serverId]);
    });
  }
  if (socket.udpSocketId) {
    cleanup('close datagram socket', () => {
      callSyncRpc('dgram.close', [socket.udpSocketId]);
    });
  }
  return firstError == null ? WASI_ERRNO_SUCCESS : mapHostProcessError(firstError);
}

const hostNetImport = {
  // Poll an array of pollfd entries (8 bytes each: i32 fd, i16 events, i16 revents).
  // Connected sockets report POLLIN when data is queued; listening sockets report POLLIN
  // only when a connection is actually pending (a buffered non-blocking accept), so the
  // server's WaitForSomething does not spin forever inside a blocking accept().
  // POLLOUT is always writable.
  net_poll(fdsPtr, nfds, timeoutMs, retReadyPtr, temporarySignalMask = null) {
    const n = Number(nfds) >>> 0;
    const base0 = Number(fdsPtr) >>> 0;
    const pollFdLimit = Math.min(LINUX_IOV_MAX, currentNofileSoftLimit());
    const countErrno = checkFixedRequestLimit(
      'wasm.abi.maxPollFds',
      n,
      pollFdLimit,
    );
    if (countErrno !== WASI_ERRNO_SUCCESS) return countErrno;
    if (!guestRangesAreValid([base0, n * 8], [retReadyPtr, 4])) {
      return WASI_ERRNO_FAULT;
    }
    // Match Linux's public poll(2) ABI in the owned sysroot exactly.
    const POLLIN = 0x001;
    const POLLOUT = 0x004;
    const POLLERR = 0x008;
    const POLLHUP = 0x010;
    const POLLNVAL = 0x020;
    const POLLRDNORM = 0x040;
    const POLLWRNORM = 0x100;
    const NORMAL_READ_EVENTS = POLLIN | POLLRDNORM;
    const NORMAL_WRITE_EVENTS = POLLOUT | POLLWRNORM;
    const t = Number(timeoutMs) | 0;
    if (SIDECAR_MANAGED_PROCESS) {
      // One sidecar-owned wait snapshots kernel fds and managed TCP/Unix/UDP
      // sockets/listeners together. The optional ppoll mask travels in this
      // same RPC; the guest never owns a mask-scope token and never pumps a
      // managed socket or child while parked.
      const view = new DataView(instanceMemory.buffer);
      const targets = [];
      const targetEntries = [];
      let ready = 0;
      for (let i = 0; i < n; i++) {
        const base = base0 + i * 8;
        const fd = view.getInt32(base, true);
        const events = view.getUint16(base + 4, true);
        let revents = 0;
        const socket = getHostNetSocket(fd);
        const handle = fd >= 0 ? lookupFdHandle(fd >>> 0) : undefined;
        let targetFd = null;
        if (socket?.managed === true) {
          targetFd = managedHostNetTargetFd(socket);
        } else if (handle?.kind === 'kernel-fd' || handle?.kind === 'passthrough') {
          targetFd = Number(handle.targetFd) >>> 0;
        } else if (handle) {
          revents = events & (NORMAL_READ_EVENTS | NORMAL_WRITE_EVENTS);
        } else if (fd >= 0 && fd <= 2) {
          targetFd = fd >>> 0;
        } else if (fd >= 0) {
          revents = POLLNVAL;
        }
        view.setUint16(base + 6, revents, true);
        if (revents !== 0) ready++;
        if (targetFd != null) {
          targets.push({ fd: targetFd, events });
          targetEntries.push({ base, events });
        }
      }

      if (ready > 0) {
        if (writeGuestUint32(retReadyPtr, ready) !== WASI_ERRNO_SUCCESS) {
          return WASI_ERRNO_FAULT;
        }
        return WASI_ERRNO_SUCCESS;
      }
      if (targets.length > 0 || temporarySignalMask != null) {
        const requestedWait = t < 0 ? null : t;
        try {
          const response = callSyncRpc('process.posix_poll', [
            targets,
            requestedWait,
            temporarySignalMask,
          ]);
          const observed = Array.isArray(response?.fds) ? response.fds : [];
          for (let i = 0; i < targetEntries.length; i++) {
            const revents = Number(observed[i]?.revents) & 0xffff;
            new DataView(instanceMemory.buffer).setUint16(
              targetEntries[i].base + 6,
              revents,
              true,
            );
            if (revents !== 0) ready++;
          }
          // A signal released only when ppoll restored the caller's mask is
          // dispatched before returning to guest code without rewriting an
          // already-observed successful readiness result.
          dispatchPendingWasmSignals();
          if (writeGuestUint32(retReadyPtr, ready) !== WASI_ERRNO_SUCCESS) {
            return WASI_ERRNO_FAULT;
          }
          return WASI_ERRNO_SUCCESS;
        } catch (error) {
          if (error?.code === 'EINTR') {
            dispatchPendingWasmSignals();
            if (writeGuestUint32(retReadyPtr, 0) !== WASI_ERRNO_SUCCESS) {
              return WASI_ERRNO_FAULT;
            }
            return WASI_ERRNO_INTR;
          }
          return mapHostProcessError(error);
        }
      }
      if (t === 0) {
        if (writeGuestUint32(retReadyPtr, 0) !== WASI_ERRNO_SUCCESS) {
          return WASI_ERRNO_FAULT;
        }
        return WASI_ERRNO_SUCCESS;
      }
      // With no waitable descriptor, retain the runner's bounded timer loop
      // below. It is the only path that can emit the 80%-of-timeout warning
      // while preserving poll(2)'s zero-fd sleep semantics.
    }
    const startedAt = Date.now();
    const deadline = t < 0 ? null : startedAt + Math.max(0, t);
    const safeguardDeadline = startedAt + unixConnectTimeoutMs;
    const safeguardApplies = deadline == null || safeguardDeadline < deadline;
    const effectiveDeadline = safeguardApplies ? safeguardDeadline : deadline;
    let warnedNearLimit = false;
    const kernelManagedStdio = KERNEL_STDIO_SYNC_RPC || SIDECAR_MANAGED_PROCESS;
    try {
      while (true) {
        warnedNearLimit = warnNearBlockingReadLimit(
          'blocking poll',
          startedAt,
          warnedNearLimit,
          safeguardApplies,
        );
        if (dispatchPendingWasmSignals()) {
          if (writeGuestUint32(retReadyPtr, 0) !== WASI_ERRNO_SUCCESS) {
            return WASI_ERRNO_FAULT;
          }
          return WASI_ERRNO_INTR;
        }
        // A spawned WASM child is serviced through child_process.poll_event.
        // Drive it while this process waits on inherited kernel pipes; otherwise
        // parent poll(2) can starve the child that must make those pipes ready.
        pumpSpawnedChildren(0);
        // Child exit cleanup may have queued SIGCHLD while the poll-event RPC
        // above was in flight. Drain again before observing fd readiness so an
        // unblocked caught signal interrupts ppoll(2), as it does on Linux.
        if (dispatchPendingWasmSignals()) {
          if (writeGuestUint32(retReadyPtr, 0) !== WASI_ERRNO_SUCCESS) {
            return WASI_ERRNO_FAULT;
          }
          return WASI_ERRNO_INTR;
        }
        const view = new DataView(instanceMemory.buffer);
        let ready = 0;
        // fds the kernel owns (PTY/pipe stdio in sidecar-managed mode): their readiness
        // comes from a batched __kernel_poll below, which doubles as the wait slice.
        const kernelTargets = [];
        const kernelEntries = [];
        let hasHostNetWaitTarget = false;
        for (let i = 0; i < n; i++) {
          const base = base0 + i * 8;
          const fd = view.getInt32(base, true);
          const events = view.getUint16(base + 4, true);
          let revents = 0;
          const socket = getHostNetSocket(fd);
          const handle = fd >= 0 ? lookupFdHandle(fd >>> 0) : undefined;
          if (socket?.managed === true) {
            hasHostNetWaitTarget = true;
            const response = callSyncRpc('process.hostnet_poll', [[{
              fd: managedHostNetTargetFd(socket),
              readable: (events & NORMAL_READ_EVENTS) !== 0,
              writable: (events & NORMAL_WRITE_EVENTS) !== 0,
            }], 0]);
            const observed = Array.isArray(response?.ready) ? response.ready[0] : null;
            if (observed?.readable === true) revents |= events & NORMAL_READ_EVENTS;
            if (observed?.writable === true) revents |= events & NORMAL_WRITE_EVENTS;
          } else if (socket && !socket.closed) {
            hasHostNetWaitTarget = true;
            if (socket.serverId) {
              if (events & NORMAL_READ_EVENTS) {
                // Report the listener readable only when a connection is actually pending.
                if (!socket.pendingAccepts) socket.pendingAccepts = [];
                if (socket.pendingAccepts.length === 0) {
                  const accepted = tryHostNetAcceptOnce(socket);
                  if (accepted) socket.pendingAccepts.push(accepted);
                }
                if (socket.pendingAccepts.length > 0) {
                  revents |= events & NORMAL_READ_EVENTS;
                }
              }
            } else if (socket.socketId) {
              // poll(2) snapshots every target before returning. Probe the
              // sidecar-backed socket without waiting even when another fd is
              // already ready; otherwise a permanently ready kernel target
              // (for example stdin POLLHUP after Git finishes writing to ssh)
              // can starve the socket's queued exit-status/EOF forever.
              if (
                (events & NORMAL_READ_EVENTS) !== 0 &&
                !socket.readableEnded &&
                !socket.lastError &&
                (!socket.readChunks || socket.readChunks.length === 0) &&
                socket.readableHint !== true
              ) {
                pollHostNetSocket(socket, 0);
              }
              if (events & NORMAL_READ_EVENTS && (
                (socket.readChunks && socket.readChunks.length > 0) || socket.readableHint === true
              )) {
                revents |= events & NORMAL_READ_EVENTS;
              }
              // poll(2) reports peer shutdown as POLLHUP even when it was not
              // requested, and a read after the queued data drains must return
              // EOF without blocking. OpenSSH waits on this transition before
              // exiting after the remote command closes its connection.
              // https://man7.org/linux/man-pages/man2/poll.2.html
              if (socket.readableEnded) {
                revents |= POLLHUP;
                if (events & NORMAL_READ_EVENTS) {
                  revents |= events & NORMAL_READ_EVENTS;
                }
              }
              if (socket.lastError) revents |= POLLERR;
              revents |= events & NORMAL_WRITE_EVENTS;
            } else if (socket.udpSocketId) {
              if (
                events & NORMAL_READ_EVENTS &&
                pollHostNetDatagram(socket, 0)?.type === 'message'
              ) {
                revents |= events & NORMAL_READ_EVENTS;
              }
              if (socket.lastError) revents |= POLLERR;
              revents |= events & NORMAL_WRITE_EVENTS;
            }
          } else if (handle?.kind === 'pipe-read') {
            if (events & NORMAL_READ_EVENTS) {
              pumpPipeProducers(handle.pipe, 0);
              if (handle.pipe.chunks.length > 0) {
                revents |= events & NORMAL_READ_EVENTS;
              } else if (
                handle.pipe.writeHandleCount === 0 &&
                handle.pipe.producers.size === 0
              ) {
                revents |= POLLHUP;
              }
            }
          } else if (handle?.kind === 'pipe-write') {
            revents |= events & NORMAL_WRITE_EVENTS;
          } else if (handle?.kind === 'kernel-fd' || (fd >= 0 && kernelManagedStdio && (
            (!handle && fd <= 2) ||
            (handle?.kind === 'passthrough' && Number(handle.targetFd) >= 0 &&
              Number(handle.targetFd) <= 2)
          ))) {
            // poll(2): readiness means the requested operation will not block.
            // https://man7.org/linux/man-pages/man2/poll.2.html
            // Kernel-managed stdio (PTY slave / stdio pipes), including dup'd
            // aliases: ask the kernel instead of treating a high alias like a
            // regular file that is always ready. A false POLLIN here makes a
            // guest block on an empty stdin pipe before it services another
            // ready fd (for example OpenSSH flushing an exec request).
            const kernelFd = handle?.kind === 'kernel-fd' || handle?.kind === 'passthrough'
              ? Number(handle.targetFd) >>> 0
              : fd;
            kernelTargets.push({
              fd: kernelFd,
              events:
                ((events & NORMAL_READ_EVENTS) !== 0 ? KERNEL_POLLIN : 0) |
                ((events & NORMAL_WRITE_EVENTS) !== 0 ? KERNEL_POLLOUT : 0),
            });
            kernelEntries.push({ base, fd, kernelFd, events });
          } else if (handle) {
            // Regular files / other VFS-backed fds: always ready, as on Linux.
            revents |= events & (NORMAL_READ_EVENTS | NORMAL_WRITE_EVENTS);
          } else if (fd >= 0 && fd <= 2) {
            // Non-kernel-managed stdio (plain runner stdio): report requested
            // readiness rather than blocking a guest forever on fds we cannot wait on.
            revents |= events & (NORMAL_READ_EVENTS | NORMAL_WRITE_EVENTS);
          } else if (fd >= 0) {
            revents |= POLLNVAL;
          }
          view.setUint16(base + 6, revents, true);
          if (revents) ready++;
        }

        if (kernelTargets.length > 0) {
          // If something is already ready (or this is a non-blocking poll), probe the
          // kernel without waiting. Mixed host-net + kernel polls must also keep
          // this probe nonblocking: __kernel_poll cannot wake for a host socket,
          // so sleeping here starves each queued SSH packet for a full 10s slice.
          // The socket pump below supplies the bounded wait in that case.
          const remaining = effectiveDeadline == null
            ? Infinity
            : effectiveDeadline - Date.now();
          const maxSliceMs = hasActiveSpawnedChildren()
            ? SPAWNED_CHILD_WAIT_SLICE_MS
            : KERNEL_WAIT_SLICE_MS;
          const sliceMs =
            ready > 0 || t === 0 || hasHostNetWaitTarget
              ? 0
              : Math.max(0, Math.min(maxSliceMs, remaining));
          let response = null;
          try {
            response = callSyncRpc('__kernel_poll', [kernelTargets, sliceMs]);
          } catch (error) {
            traceHostProcess('kernel-poll-error', {
              message: error instanceof Error ? error.message : String(error),
            });
            return mapHostProcessError(error);
          }
          const responseEntries = Array.isArray(response?.fds) ? response.fds : [];
          for (const entry of kernelEntries) {
            const responseEntry = responseEntries.find(
              (item) => (Number(item?.fd) >>> 0) === (entry.kernelFd >>> 0),
            );
            const kernelRevents = Number(responseEntry?.revents) >>> 0;
            let revents = 0;
            if (kernelRevents & KERNEL_POLLIN) {
              revents |= NORMAL_READ_EVENTS & entry.events;
            }
            if (kernelRevents & KERNEL_POLLOUT) {
              revents |= NORMAL_WRITE_EVENTS & entry.events;
            }
            if (kernelRevents & KERNEL_POLLERR) revents |= POLLERR;
            if (kernelRevents & KERNEL_POLLHUP) revents |= POLLHUP;
            new DataView(instanceMemory.buffer).setUint16(entry.base + 6, revents, true);
            if (revents) ready++;
          }
        }

        if (ready > 0 || t === 0 || (deadline != null && Date.now() >= deadline)) {
          if (dispatchPendingWasmSignals()) {
            if (writeGuestUint32(retReadyPtr, 0) !== WASI_ERRNO_SUCCESS) {
              return WASI_ERRNO_FAULT;
            }
            return WASI_ERRNO_INTR;
          }
          if (writeGuestUint32(retReadyPtr, ready) !== WASI_ERRNO_SUCCESS) {
            return WASI_ERRNO_FAULT;
          }
          return 0;
        }
        if (safeguardApplies && Date.now() >= safeguardDeadline) {
          process.stderr.write(
            `[agentos] blocking poll exceeded limits.resources.maxBlockingReadMs (${unixConnectTimeoutMs} ms); raise limits.resources.maxBlockingReadMs if needed\n`,
          );
          if (writeGuestUint32(retReadyPtr, 0) !== WASI_ERRNO_SUCCESS) {
            return WASI_ERRNO_FAULT;
          }
          return WASI_ERRNO_TIMEDOUT;
        }
        let pumpedSocket = false;
        const v2 = new DataView(instanceMemory.buffer);
        for (let i = 0; i < n; i++) {
          const fd = v2.getInt32(base0 + i * 8, true);
          const s = getHostNetSocket(fd);
          if (s?.managed === true) {
            callSyncRpc('process.hostnet_poll', [[{
              fd: managedHostNetTargetFd(s),
              readable: true,
              writable: false,
            }], 10]);
            pumpedSocket = true;
          } else if (s && !s.serverId) {
            if (s.socketId) {
              pollHostNetSocket(s, 10);
              pumpedSocket = true;
            } else if (s.udpSocketId) {
              pollHostNetDatagram(s, 10);
              pumpedSocket = true;
            }
          }
        }
        if (kernelTargets.length === 0 && !pumpedSocket) {
          // Nothing to wait on except time: sleep a slice instead of hot-spinning.
          const remaining = effectiveDeadline == null
            ? Infinity
            : effectiveDeadline - Date.now();
          Atomics.wait(syntheticWaitArray, 0, 0, Math.max(1, Math.min(10, remaining)));
        }
      }
    } catch (error) {
      return mapHostProcessError(error);
    }
  },
  net_socket(domain, sockType, protocol, retFdPtr) {
    try {
      if (!guestRangeIsValid(retFdPtr, 4)) return WASI_ERRNO_FAULT;
      const numericDomain = Number(domain) >>> 0;
      // Rust's WASI networking compatibility path uses POSIX socket type
      // values (1/2), while the owned wasi-libc ABI and sidecar transfer
      // metadata use WASI filetypes (6/5). Store one canonical form so a live
      // socket can be inherited by a subsequently spawned child.
      const numericType = normalizeHostNetSocketType(sockType);
      const numericProtocol = Number(protocol) >>> 0;
      if (
        numericDomain === HOST_NET_AF_UNIX &&
        (numericType & HOST_NET_SOCKET_TYPE_MASK) !== HOST_NET_SOCK_STREAM
      ) {
        return WASI_ERRNO_NOTSUP;
      }

      const socket = {
        domain: numericDomain,
        sockType: numericType,
        protocol: numericProtocol,
        bindOptions: null,
        localInfo: null,
        localUnixAddress: numericDomain === HOST_NET_AF_UNIX ? 'unix-unnamed' : null,
        localReservation: null,
        remoteInfo: null,
        remoteUnixAddress: null,
        listening: false,
        serverId: null,
        socketId: null,
        udpSocketId: null,
        pendingDatagram: null,
        recvTimeoutMs: null,
        readChunks: [],
        readableEnded: false,
        closed: false,
        lastError: null,
        // Honor Linux-style socket(..., type | SOCK_NONBLOCK): guests like
        // libcurl rely on O_NONBLOCK semantics (EAGAIN instead of blocking
        // reads) to interleave send/recv on one connection. Dropping this bit
        // deadlocks any upload larger than one TLS record: curl checks for an
        // early server response mid-upload, and a blocking recv() waits on a
        // server that is itself waiting for the rest of the request body.
        nonblock: (numericType & HOST_NET_SOCK_NONBLOCK) !== 0,
      };
      const fd = registerNewHostNetSocket(socket);
      if (fd == null) return WASI_ERRNO_MFILE;
      const copyout = writeGuestUint32(retFdPtr, fd);
      if (copyout !== WASI_ERRNO_SUCCESS) {
        wasiImport.fd_close(fd);
      }
      return copyout;
    } catch (error) {
      return mapHostProcessError(error);
    }
  },
  // Mark a host_net socket non-blocking (O_NONBLOCK). The patched wasi-libc fcntl cannot reach
  // host_net fds, so libxcb calls this directly. Non-blocking recv returns EAGAIN on no data.
  net_set_nonblock(fd, enable) {
    const socket = getHostNetSocket(fd);
    if (!socket) return WASI_ERRNO_BADF;
    const nonblocking = (Number(enable) >>> 0) !== 0;
    try {
      if (SIDECAR_MANAGED_PROCESS) {
        const handle = lookupFdHandle(Number(fd) >>> 0);
        if (handle?.kind !== 'kernel-fd') return WASI_ERRNO_BADF;
        const stat = callSyncRpc('process.fd_stat', [Number(handle.targetFd) >>> 0]);
        const currentFlags = Number(stat?.flags) >>> 0;
        const nextFlags = nonblocking
          ? currentFlags | KERNEL_O_NONBLOCK
          : currentFlags & ~KERNEL_O_NONBLOCK;
        callSyncRpc('process.fd_set_flags', [Number(handle.targetFd) >>> 0, nextFlags]);
      }
      if (socket.managed !== true) socket.nonblock = nonblocking;
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapHostProcessError(error);
    }
  },
  net_connect(fd, addrPtr, addrLen) {
    const socket = getHostNetSocket(fd);
    if (!socket) {
      return validateHostNetSocketDescriptor(fd);
    }
    if (socket.socketId != null) {
      return WASI_ERRNO_ISCONN;
    }
    if (socket.listening === true) {
      return WASI_ERRNO_INVAL;
    }

    try {
      let rawAddr = String(readGuestString(addrPtr, addrLen) ?? '');
      // A sockaddr_un serialized from sizeof(struct sockaddr_un) carries trailing NUL
      // padding; cut at the first NUL so the unix path is clean before classification.
      const nulAt = rawAddr.indexOf(String.fromCharCode(0));
      if (nulAt >= 0) rawAddr = rawAddr.slice(0, nulAt);
      if (socket.managed === true) {
        callSyncRpc('process.hostnet_connect', [
          managedHostNetTargetFd(socket),
          managedHostNetAddressValue(rawAddr),
          unixConnectTimeoutMs,
        ]);
        return WASI_ERRNO_SUCCESS;
      }
      // AF_UNIX addresses use an explicit wire prefix so relative paths and paths containing ':'
      // cannot be mistaken for TCP host:port strings.
      const unixAddress = parseHostNetUnixAddress(rawAddr);
      if (unixAddress != null) {
        if (Number(socket.domain) !== HOST_NET_AF_UNIX) return WASI_ERRNO_AFNOSUPPORT;
        if (unixAddress.autobind === true) {
          return WASI_ERRNO_INVAL;
        }
        const request = { ...unixAddress };
        if (socket.serverId) {
          request.boundServerId = socket.serverId;
        }
        const startedAt = Date.now();
        const deadline = startedAt + unixConnectTimeoutMs;
        let warnedNearLimit = false;
        let result;
        for (;;) {
          if (dispatchPendingWasmSignals(true)) return WASI_ERRNO_INTR;
          try {
            result = callSyncRpc('net.connect', [request]);
            break;
          } catch (error) {
            if (mapHostProcessError(error) !== WASI_ERRNO_AGAIN) throw error;
            if (socket.nonblock) return WASI_ERRNO_AGAIN;
            warnedNearLimit = warnNearBlockingReadLimit(
              'blocking AF_UNIX connect',
              startedAt,
              warnedNearLimit,
            );
            if (Date.now() >= deadline) {
              process.stderr.write(
                `[agentos] blocking AF_UNIX connect exceeded limits.resources.maxBlockingReadMs (${unixConnectTimeoutMs} ms); raise limits.resources.maxBlockingReadMs if needed\n`,
              );
              return WASI_ERRNO_TIMEDOUT;
            }
            if (
              pumpSpawnedChildrenOrWaitRestartable(
                Math.min(10, Math.max(1, deadline - Date.now())),
              )
            ) {
              return WASI_ERRNO_INTR;
            }
          }
        }
        if (!result || typeof result.socketId !== 'string') {
          return WASI_ERRNO_FAULT;
        }
        socket.socketId = result.socketId;
        socket.localInfo = null;
        const localUnix = unixAddressFromSidecarInfo(result, 'local');
        socket.localUnixAddress = localUnix
          ? formatHostNetUnixAddress(localUnix)
          : socket.localUnixAddress ?? 'unix-unnamed';
        socket.localReservation = null;
        socket.remoteInfo = null;
        const remoteUnix = unixAddressFromSidecarInfo(result, 'remote');
        socket.remoteUnixAddress = formatHostNetUnixAddress(remoteUnix ?? unixAddress);
        socket.serverId = null;
        socket.listening = false;
        socket.readChunks.length = 0;
        socket.readableEnded = false;
        socket.closed = false;
        socket.lastError = null;
        return WASI_ERRNO_SUCCESS;
      }
      if (Number(socket.domain) === HOST_NET_AF_UNIX) return WASI_ERRNO_INVAL;
      const { host, port } = parseHostNetAddress(rawAddr);
      if (!Number.isInteger(port) || port < 0 || port > 65535) {
        return WASI_ERRNO_FAULT;
      }

      const request = {
        host,
        port,
        family: Number(socket.domain) === HOST_NET_AF_INET6 ? 6 : 4,
      };
      if (socket.bindOptions?.host != null) {
        request.localAddress = socket.bindOptions.host;
      }
      if (socket.bindOptions?.port != null) {
        request.localPort = socket.bindOptions.port;
      }
      if (socket.localReservation != null) {
        request.localReservation = socket.localReservation;
      }

      const result = callSyncRpc('net.connect', [request]);
      if (!result || typeof result.socketId !== 'string') {
        return WASI_ERRNO_FAULT;
      }

      socket.socketId = result.socketId;
      socket.localInfo = normalizeHostNetAddressInfo(result.localAddress, result.localPort);
      socket.localReservation = null;
      socket.remoteInfo = normalizeHostNetAddressInfo(result.remoteAddress, result.remotePort);
      socket.readChunks.length = 0;
      socket.readableEnded = false;
      socket.closed = false;
      socket.lastError = null;
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapHostProcessError(error);
    }
  },
  net_getaddrinfo(hostPtr, hostLen, portPtr, portLen, family, retAddrPtr, retAddrLenPtr) {
    try {
      if (!guestRangeIsValid(retAddrLenPtr, 4)) return WASI_ERRNO_FAULT;
      const outputCapacity = readGuestUint32(retAddrLenPtr);
      if (!guestRangeIsValid(retAddrPtr, outputCapacity)) return WASI_ERRNO_FAULT;
      const hostname = readGuestString(hostPtr, hostLen);
      const numericFamily = Number(family) >>> 0;
      const lookupOptions = { hostname, all: true };
      if (numericFamily === 4) {
        lookupOptions.family = 4;
      } else if (numericFamily === 6) {
        lookupOptions.family = 6;
      } else if (numericFamily !== 0) {
        return WASI_ERRNO_INVAL;
      }

      const records = callSyncRpc('dns.lookup', [lookupOptions]);
      if (!Array.isArray(records)) {
        return WASI_ERRNO_FAULT;
      }
      const payload = records.map((record) => {
        const family = Number(record?.family);
        if (family !== 4 && family !== 6) {
          throw new Error('host_net dns record family is unsupported');
        }
        return {
          addr: String(record?.address ?? ''),
          family,
        };
      });
      const encoded = Buffer.from(JSON.stringify(payload), 'utf8');
      return writeGuestBytes(
        retAddrPtr,
        outputCapacity,
        encoded,
        retAddrLenPtr,
      );
    } catch {
      return WASI_ERRNO_FAULT;
    }
  },
  net_dns_query_rr_v1(
    namePtr,
    nameLen,
    rrtype,
    outPtr,
    outCap,
    retLenPtr,
    retTtlPtr,
    retFlagsPtr,
  ) {
    try {
      const outputCapacity = Number(outCap) >>> 0;
      if (!guestRangesAreValid(
        [outPtr, outputCapacity],
        [retLenPtr, 4],
        [retTtlPtr, 4],
        [retFlagsPtr, 4],
      )) {
        return WASI_ERRNO_FAULT;
      }
      const numericType = Number(rrtype) >>> 0;
      const requestedType = numericType === 12
        ? 'PTR'
        : numericType === 44
          ? 'SSHFP'
          : null;
      if (requestedType === null) {
        return WASI_ERRNO_NOTSUP;
      }
      const response = callSyncRpc('dns.resolveRawRr', [{
        hostname: readGuestString(namePtr, nameLen),
        rrtype: requestedType,
      }]);
      const status = String(response?.status ?? '');
      const records = response?.records;
      if (
        !['ok', 'nxdomain', 'nodata'].includes(status) ||
        !Array.isArray(records) ||
        (status !== 'ok' && records.length !== 0)
      ) {
        return WASI_ERRNO_FAULT;
      }
      if (records.length > 4096) {
        return WASI_ERRNO_NOBUFS;
      }

      const rawRecords = [];
      let ttl = 0;
      for (const record of records) {
        const encoded = typeof record?.data === 'string' ? record.data : '';
        if (
          !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(encoded)
        ) {
          return WASI_ERRNO_FAULT;
        }
        const raw = Buffer.from(encoded, 'base64');
        if (
          raw.toString('base64') !== encoded ||
          (requestedType === 'PTR' && raw.length === 0) ||
          (requestedType === 'SSHFP' && raw.length < 2)
        ) {
          return WASI_ERRNO_FAULT;
        }
        const recordTtl = Number(record?.ttl);
        if (!Number.isInteger(recordTtl) || recordTtl < 0 || recordTtl > 0xffffffff) {
          return WASI_ERRNO_FAULT;
        }
        ttl = rawRecords.length === 0 ? recordTtl : Math.min(ttl, recordTtl);
        rawRecords.push(raw);
      }
      const payloadLength = rawRecords.reduce(
        (total, record) => total + 4 + record.length,
        4,
      );
      if (payloadLength > 64 * 1024) {
        return WASI_ERRNO_NOBUFS;
      }
      if (writeGuestUint32(retLenPtr, payloadLength) !== WASI_ERRNO_SUCCESS) {
        return WASI_ERRNO_FAULT;
      }
      if (outputCapacity < payloadLength) {
        return WASI_ERRNO_NOBUFS;
      }

      const payload = Buffer.alloc(payloadLength);
      payload.writeUInt32LE(rawRecords.length, 0);
      let offset = 4;
      for (const record of rawRecords) {
        payload.writeUInt32LE(record.length, offset);
        offset += 4;
        record.copy(payload, offset);
        offset += record.length;
      }
      const memory = new Uint8Array(instanceMemory.buffer);
      const outputOffset = Number(outPtr) >>> 0;
      if (outputOffset > memory.length || payload.length > memory.length - outputOffset) {
        return WASI_ERRNO_FAULT;
      }
      memory.set(payload, outputOffset);
      const flags = status === 'nxdomain' ? 2 : status === 'nodata' ? 4 : 0;
      if (
        writeGuestUint32(retTtlPtr, ttl) !== WASI_ERRNO_SUCCESS ||
        writeGuestUint32(retFlagsPtr, flags) !== WASI_ERRNO_SUCCESS
      ) {
        return WASI_ERRNO_FAULT;
      }
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapHostProcessError(error);
    }
  },
  net_bind(fd, addrPtr, addrLen) {
    const socket = getHostNetSocket(fd);
    if (!socket || socket.closed) {
      return validateHostNetSocketDescriptor(fd);
    }

    try {
      if (socket.managed === true) {
        callSyncRpc('process.hostnet_bind', [
          managedHostNetTargetFd(socket),
          managedHostNetAddressValue(readGuestString(addrPtr, addrLen)),
        ]);
        return WASI_ERRNO_SUCCESS;
      }
      if (socket.bindOptions != null || socket.serverId != null) {
        return WASI_ERRNO_INVAL;
      }
      if (socket.localReservation != null) {
        callSyncRpc('net.release_tcp_port', [socket.localReservation]);
        socket.localReservation = null;
      }

      const bindOptions = parseHostNetListenAddress(readGuestString(addrPtr, addrLen));
      const isUnixBind = bindOptions.path != null ||
        bindOptions.abstractPathHex != null || bindOptions.autobind === true;
      if (isUnixBind && Number(socket.domain) !== HOST_NET_AF_UNIX) {
        return WASI_ERRNO_AFNOSUPPORT;
      }
      if (!isUnixBind && Number(socket.domain) === HOST_NET_AF_UNIX) {
        return WASI_ERRNO_INVAL;
      }
      if (hostNetSocketBaseType(socket) === HOST_NET_SOCK_DGRAM) {
        if (bindOptions.path != null || bindOptions.abstractPathHex != null) {
          return WASI_ERRNO_NOTSUP;
        }
        const udpSocketId = ensureHostNetUdpSocket(socket);
        if (!udpSocketId) {
          return WASI_ERRNO_FAULT;
        }
        const result = callSyncRpc('dgram.bind', [
          udpSocketId,
          {
            address: bindOptions.host,
            port: bindOptions.port,
          },
        ]);
        const localInfo = normalizeHostNetAddressInfo(result?.localAddress, result?.localPort);
        if (!localInfo) return WASI_ERRNO_FAULT;
        socket.bindOptions = bindOptions;
        socket.localInfo = localInfo;
        return WASI_ERRNO_SUCCESS;
      }

      if (isUnixBind) {
        if (socket.socketId != null) {
          const result = callSyncRpc('net.bind_connected_unix', [{
            socketId: socket.socketId,
            ...bindOptions,
          }]);
          const boundAddress = unixAddressFromSidecarInfo(result, 'local');
          if (!boundAddress) return WASI_ERRNO_FAULT;
          socket.localUnixAddress = formatHostNetUnixAddress(boundAddress);
          socket.bindOptions = boundAddress;
          socket.localInfo = null;
          return WASI_ERRNO_SUCCESS;
        }
        const result = callSyncRpc('net.bind_unix', [bindOptions]);
        if (!result || typeof result.serverId !== 'string') {
          return WASI_ERRNO_FAULT;
        }
        socket.serverId = result.serverId;
        const boundAddress = unixAddressFromSidecarInfo(result, 'local');
        socket.localUnixAddress = boundAddress
          ? formatHostNetUnixAddress(boundAddress)
          : formatHostNetUnixAddress(bindOptions);
        socket.bindOptions = boundAddress ?? bindOptions;
        socket.localInfo = null;
      } else {
        if (socket.socketId != null) return WASI_ERRNO_INVAL;
        const reservation = callSyncRpc('net.reserve_tcp_port', [bindOptions]);
        if (
          !reservation ||
          typeof reservation.reservationId !== 'string' ||
          !Number.isInteger(Number(reservation.localPort))
        ) {
          if (typeof reservation?.reservationId === 'string') {
            callSyncRpc('net.release_tcp_port', [reservation.reservationId]);
          }
          return WASI_ERRNO_FAULT;
        }
        socket.localReservation = reservation.reservationId;
        socket.bindOptions = {
          ...bindOptions,
          host: reservation.localAddress ?? bindOptions.host,
          port: Number(reservation.localPort),
        };
        socket.localInfo = normalizeHostNetAddressInfo(
          socket.bindOptions.host ?? '127.0.0.1',
          socket.bindOptions.port,
        );
      }
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapHostProcessError(error);
    }
  },
  net_listen(fd, backlog) {
    const socket = getHostNetSocket(fd);
    if (!socket || socket.closed) {
      return validateHostNetSocketDescriptor(fd);
    }
    if (socket.managed !== true && socket.socketId != null) {
      return WASI_ERRNO_INVAL;
    }
    if (socket.managed !== true && !socket.bindOptions) {
      return WASI_ERRNO_INVAL;
    }

    try {
      if (socket.managed === true) {
        callSyncRpc('process.hostnet_listen', [
          managedHostNetTargetFd(socket),
          Math.max(0, Number(backlog) >>> 0),
        ]);
        return WASI_ERRNO_SUCCESS;
      }
      const request = {
        backlog: Math.max(0, Number(backlog) >>> 0),
      };
      if (socket.serverId) {
        request.boundServerId = socket.serverId;
      } else {
        Object.assign(request, socket.bindOptions);
      }
      if (socket.localReservation != null) {
        request.localReservation = socket.localReservation;
      }

      const result = callSyncRpc('net.listen', [request]);
      if (!result || typeof result.serverId !== 'string') {
        return WASI_ERRNO_FAULT;
      }
      socket.serverId = result.serverId;
      const localUnix = unixAddressFromSidecarInfo(result, 'local');
      if (localUnix) {
        socket.localUnixAddress = formatHostNetUnixAddress(localUnix);
        socket.bindOptions = localUnix;
      }
      socket.localReservation = null;
      socket.localInfo = normalizeHostNetAddressInfo(result.localAddress, result.localPort);
      socket.listening = true;
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapHostProcessError(error);
    }
  },
  net_accept(fd, retFdPtr, retAddrPtr, retAddrLenPtr) {
    const socket = getHostNetSocket(fd);
    const validation = validateHostNetSocketDescriptor(fd, true);
    if (validation !== WASI_ERRNO_SUCCESS) return validation;
    if (socket?.managed === true) {
      if (!guestRangesAreValid([retFdPtr, 4], [retAddrLenPtr, 4])) {
        return WASI_ERRNO_FAULT;
      }
      const addressCapacity = readGuestUint32(retAddrLenPtr);
      if (!guestRangeIsValid(retAddrPtr, addressCapacity)) return WASI_ERRNO_FAULT;
      let acceptedGuestFd = null;
      try {
        const listenerStat = callSyncRpc('process.fd_stat', [
          managedHostNetTargetFd(socket),
        ]);
        const listenerNonblocking =
          (Number(listenerStat?.flags) & KERNEL_O_NONBLOCK) !== 0;
        const startedAt = Date.now();
        let result = callSyncRpc('process.hostnet_accept', [
          managedHostNetTargetFd(socket),
          false,
          false,
          0,
        ]);
        while (result == null || isHostNetWouldBlock(result)) {
          if (listenerNonblocking) return WASI_ERRNO_AGAIN;
          const remaining = Math.max(0, unixConnectTimeoutMs - (Date.now() - startedAt));
          const readable = waitManagedHostNetReadable(socket, remaining, true);
          if (readable === WASI_ERRNO_INTR) return WASI_ERRNO_INTR;
          if (!readable) return WASI_ERRNO_TIMEDOUT;
          result = callSyncRpc('process.hostnet_accept', [
            managedHostNetTargetFd(socket),
            false,
            false,
            0,
          ]);
          if (result == null || isHostNetWouldBlock(result)) {
            if (Date.now() - startedAt >= unixConnectTimeoutMs) return WASI_ERRNO_TIMEDOUT;
          }
        }
        acceptedGuestFd = registerKernelDelegateFd(result.fd, null, FIRST_SYNTHETIC_FD);
        attachManagedHostNetDescription(acceptedGuestFd, result.descriptionId);
        const info = result.info ?? {};
        const address = Buffer.from(managedHostNetAddressText({
          address: info.remoteAddress,
          port: info.remotePort,
          path: info.remotePath,
          abstractPathHex: info.remoteAbstractPathHex,
        }), 'utf8');
        if (writeGuestUint32(retFdPtr, acceptedGuestFd) !== WASI_ERRNO_SUCCESS) {
          wasiImport.fd_close(acceptedGuestFd);
          return WASI_ERRNO_FAULT;
        }
        const copied = writeGuestBytes(
          retAddrPtr,
          addressCapacity,
          address,
          retAddrLenPtr,
        );
        if (copied !== WASI_ERRNO_SUCCESS) wasiImport.fd_close(acceptedGuestFd);
        return copied;
      } catch (error) {
        if (acceptedGuestFd != null) wasiImport.fd_close(acceptedGuestFd);
        return mapHostProcessError(error);
      }
    }
    if (!socket.serverId || socket.listening !== true) {
      return WASI_ERRNO_INVAL;
    }
    if (!guestRangesAreValid([retFdPtr, 4], [retAddrLenPtr, 4])) {
      return WASI_ERRNO_FAULT;
    }
    const addressCapacity = readGuestUint32(retAddrLenPtr);
    if (!guestRangeIsValid(retAddrPtr, addressCapacity)) return WASI_ERRNO_FAULT;

    let accepted = null;
    try {
      // First drain a connection already buffered by net_poll's readiness probe; otherwise block
      // until one arrives (POSIX blocking-accept semantics, for guests that accept() without polling
      // first). This no longer starves connected clients: net_poll now reports the listener readable
      // only when a connection is actually pending, so the X server only reaches accept() when there
      // is one to take, and otherwise services connected client fds instead.
      if (!socket.pendingAccepts) socket.pendingAccepts = [];
      accepted = socket.pendingAccepts.shift();
      const startedAt = Date.now();
      const safeguardDeadline = startedAt + unixConnectTimeoutMs;
      const receiveTimeoutMs = Number(socket.recvTimeoutMs);
      const receiveDeadline = Number.isFinite(receiveTimeoutMs) && receiveTimeoutMs > 0
        ? startedAt + receiveTimeoutMs
        : null;
      let warnedNearLimit = false;
      while (!accepted) {
        if (dispatchPendingWasmSignals(true)) return WASI_ERRNO_INTR;
        accepted = tryHostNetAcceptOnce(socket);
        if (!accepted && socket.nonblock) return WASI_ERRNO_AGAIN;
        if (!accepted) {
          const now = Date.now();
          if (receiveDeadline != null && now >= receiveDeadline) {
            return WASI_ERRNO_AGAIN;
          }
          warnedNearLimit = warnNearBlockingReadLimit(
            'blocking accept',
            startedAt,
            warnedNearLimit,
          );
          if (now >= safeguardDeadline) {
            process.stderr.write(
              `[agentos] blocking accept exceeded limits.resources.maxBlockingReadMs (${unixConnectTimeoutMs} ms); raise limits.resources.maxBlockingReadMs if needed\n`,
            );
            return WASI_ERRNO_TIMEDOUT;
          }
          const nextDeadline = receiveDeadline == null
            ? safeguardDeadline
            : Math.min(receiveDeadline, safeguardDeadline);
          if (
            pumpSpawnedChildrenOrWaitRestartable(
              Math.min(10, Math.max(1, nextDeadline - now)),
            )
          ) {
            return WASI_ERRNO_INTR;
          }
        }
      }
      if (accepted.error != null) {
        return accepted.error;
      }
      if (writeGuestUint32(retFdPtr, accepted.acceptedFd) !== WASI_ERRNO_SUCCESS) {
        cleanupAcceptedHostNetSocket(accepted, 'accept fd copyout');
        return WASI_ERRNO_FAULT;
      }
      const addressCopyout = writeGuestBytes(
        retAddrPtr,
        addressCapacity,
        accepted.address,
        retAddrLenPtr,
      );
      if (addressCopyout !== WASI_ERRNO_SUCCESS) {
        cleanupAcceptedHostNetSocket(accepted, 'accept address copyout');
      }
      return addressCopyout;
    } catch (error) {
      cleanupAcceptedHostNetSocket(accepted, 'accept failure');
      return mapHostProcessError(error);
    }
  },
  net_validate_socket(fd) {
    return validateHostNetSocketDescriptor(fd);
  },
  net_validate_accept(fd) {
    return validateHostNetSocketDescriptor(fd, true);
  },
  net_getsockname(fd, addrPtr, addrLenPtr) {
    const socket = getHostNetSocket(fd);
    if (!socket || socket.closed) {
      return validateHostNetSocketDescriptor(fd);
    }
    try {
      if (!guestRangeIsValid(addrLenPtr, 4)) return WASI_ERRNO_FAULT;
      const addressCapacity = readGuestUint32(addrLenPtr);
      if (!guestRangeIsValid(addrPtr, addressCapacity)) return WASI_ERRNO_FAULT;
      if (socket.managed === true) {
        const value = callSyncRpc('process.hostnet_local_address', [
          managedHostNetTargetFd(socket),
        ]);
        if (value == null) return WASI_ERRNO_INVAL;
        return writeGuestBytes(
          addrPtr,
          addressCapacity,
          Buffer.from(managedHostNetAddressText(value), 'utf8'),
          addrLenPtr,
        );
      }
      refreshHostNetUnixSocketInfo(socket);
      if (socket.localUnixAddress != null) {
        const address = Buffer.from(socket.localUnixAddress, 'utf8');
        return writeGuestBytes(addrPtr, addressCapacity, address, addrLenPtr);
      }
      if (!socket.localInfo) {
        return WASI_ERRNO_INVAL;
      }
      const address = Buffer.from(formatHostNetAddressInfo(socket.localInfo), 'utf8');
      return writeGuestBytes(addrPtr, addressCapacity, address, addrLenPtr);
    } catch (error) {
      return mapHostProcessError(error);
    }
  },
  net_getpeername(fd, addrPtr, addrLenPtr) {
    const socket = getHostNetSocket(fd);
    if (!socket || socket.closed) {
      return validateHostNetSocketDescriptor(fd);
    }
    try {
      if (!guestRangeIsValid(addrLenPtr, 4)) return WASI_ERRNO_FAULT;
      const addressCapacity = readGuestUint32(addrLenPtr);
      if (!guestRangeIsValid(addrPtr, addressCapacity)) return WASI_ERRNO_FAULT;
      if (socket.managed === true) {
        const value = callSyncRpc('process.hostnet_peer_address', [
          managedHostNetTargetFd(socket),
        ]);
        return writeGuestBytes(
          addrPtr,
          addressCapacity,
          Buffer.from(managedHostNetAddressText(value), 'utf8'),
          addrLenPtr,
        );
      }
      refreshHostNetUnixSocketInfo(socket);
      if (socket.remoteUnixAddress != null) {
        const address = Buffer.from(socket.remoteUnixAddress, 'utf8');
        return writeGuestBytes(addrPtr, addressCapacity, address, addrLenPtr);
      }
      if (!socket.remoteInfo) {
        return WASI_ERRNO_NOTCONN;
      }
      const address = Buffer.from(formatHostNetAddressInfo(socket.remoteInfo), 'utf8');
      return writeGuestBytes(addrPtr, addressCapacity, address, addrLenPtr);
    } catch (error) {
      return mapHostProcessError(error);
    }
  },
  net_send(fd, bufPtr, bufLen, flags, retSentPtr) {
    if (!guestRangesAreValid([bufPtr, Number(bufLen) >>> 0], [retSentPtr, 4])) {
      return WASI_ERRNO_FAULT;
    }
    const socket = getHostNetSocket(fd);
    const handle = lookupFdHandle(Number(fd) >>> 0);
    if (!socket && handle?.kind === 'kernel-fd') {
      try {
        const chunk = readGuestBytes(bufPtr, bufLen);
        const written = Number(callSyncRpc('process.fd_sendmsg_rights', [
          Number(handle.targetFd) >>> 0,
          chunk,
          [],
          Number(flags) >>> 0,
        ]));
        return writeGuestUint32(retSentPtr, written);
      } catch (error) {
        return mapHostProcessError(error);
      }
    }
    if (!socket || (socket.managed !== true && (!socket.socketId || socket.closed))) {
      return WASI_ERRNO_BADF;
    }

    try {
      const chunk = readGuestBytes(bufPtr, bufLen);
      if (socket.managed === true) {
        const written = Number(callSyncRpc('process.hostnet_send', [
          managedHostNetTargetFd(socket),
          chunk,
          Number(flags) >>> 0,
          null,
          unixConnectTimeoutMs,
        ])) >>> 0;
        return writeGuestUint32(retSentPtr, written);
      }
      if ((Number(flags) >>> 0) !== 0) {
        // Non-zero send flags are currently ignored in the WASM host_net shim.
      }
      const written = Number(
        callSyncRpc('net.write', [
          socket.socketId,
          chunk,
          socket.nonblock === true ||
            ((Number(flags) >>> 0) & HOST_NET_MSG_DONTWAIT) !== 0,
        ]),
      ) >>> 0;
      return writeGuestUint32(retSentPtr, written);
    } catch (error) {
      return mapHostProcessError(error);
    }
  },
  net_recv(fd, bufPtr, bufLen, flags, retReceivedPtr) {
    if (!guestRangesAreValid([bufPtr, Number(bufLen) >>> 0], [retReceivedPtr, 4])) {
      return WASI_ERRNO_FAULT;
    }
    const socket = getHostNetSocket(fd);
    const handle = lookupFdHandle(Number(fd) >>> 0);
    if (!socket && handle?.kind === 'kernel-fd') {
      try {
        const recvFlags = Number(flags) >>> 0;
        const result = callSyncRpc('process.fd_recvmsg_rights', [
          Number(handle.targetFd) >>> 0,
          Number(bufLen) >>> 0,
          0,
          false,
          (recvFlags & 0x0002) !== 0,
          (recvFlags & 0x0040) !== 0,
          (recvFlags & 0x0100) !== 0,
        ]);
        const bytes = Buffer.from(result?.data ?? []);
        const write = writeGuestBytes(bufPtr, bufLen, bytes, retReceivedPtr);
        if (write !== WASI_ERRNO_SUCCESS) return write;
        if ((recvFlags & 0x0020) !== 0 && Number(result?.fullLength) > bytes.length) {
          return writeGuestUint32(retReceivedPtr, Number(result.fullLength) >>> 0);
        }
        return WASI_ERRNO_SUCCESS;
      } catch (error) {
        return mapHostProcessError(error);
      }
    }
    if (!socket) {
      return WASI_ERRNO_BADF;
    }

    try {
      const recvFlags = Number(flags) >>> 0;
      if (socket.managed === true) {
        const nonblocking = (recvFlags & HOST_NET_MSG_DONTWAIT) !== 0;
        const configured = nonblocking ? null : callSyncRpc(
          'process.hostnet_get_option',
          [managedHostNetTargetFd(socket), 'receive-timeout'],
        );
        const configuredTimeout = Number(configured?.durationMs);
        const hasConfiguredTimeout = configured?.durationMs != null
          && Number.isFinite(configuredTimeout) && configuredTimeout >= 0;
        const waitLimit = nonblocking
          ? 0
          : hasConfiguredTimeout ? configuredTimeout : unixConnectTimeoutMs;
        const startedAt = Date.now();
        let result;
        for (;;) {
          if (!nonblocking) {
            const remaining = Math.max(0, waitLimit - (Date.now() - startedAt));
            const readable = waitManagedHostNetReadable(socket, remaining, true);
            if (readable === WASI_ERRNO_INTR) return WASI_ERRNO_INTR;
            if (!readable) {
              return hasConfiguredTimeout ? WASI_ERRNO_AGAIN : WASI_ERRNO_TIMEDOUT;
            }
          }
          result = callSyncRpc('process.hostnet_recv', [
            managedHostNetTargetFd(socket),
            Number(bufLen) >>> 0,
            recvFlags,
            0,
          ]);
          if (!isHostNetWouldBlock(result)) break;
          if (nonblocking) return WASI_ERRNO_AGAIN;
          if (Date.now() - startedAt >= waitLimit) {
            return hasConfiguredTimeout ? WASI_ERRNO_AGAIN : WASI_ERRNO_TIMEDOUT;
          }
        }
        if (result == null) return writeGuestUint32(retReceivedPtr, 0);
        const bytes = result?.type === 'message'
          ? hostNetDatagramBytes(result)
          : decodeFsBytesPayload(result, 'managed host-network receive');
        const write = writeGuestBytes(bufPtr, bufLen, bytes, retReceivedPtr);
        if (write !== WASI_ERRNO_SUCCESS) return write;
        if ((recvFlags & HOST_NET_MSG_TRUNC) !== 0 && bytes.length > (Number(bufLen) >>> 0)) {
          return writeGuestUint32(retReceivedPtr, bytes.length);
        }
        return WASI_ERRNO_SUCCESS;
      }
      if (hostNetSocketBaseType(socket) === HOST_NET_SOCK_DGRAM) {
        const supportedFlags = HOST_NET_MSG_PEEK | HOST_NET_MSG_DONTWAIT | HOST_NET_MSG_TRUNC;
        if ((recvFlags & ~supportedFlags) !== 0) {
          return WASI_ERRNO_INVAL;
        }
        const udpSocketId = ensureHostNetUdpSocket(socket);
        if (!udpSocketId) {
          return WASI_ERRNO_BADF;
        }
        const event = receiveHostNetDatagramEvent(socket, recvFlags);
        if (!event) {
          return WASI_ERRNO_AGAIN;
        }
        const bytes = hostNetDatagramBytes(event);
        const write = writeGuestBytes(bufPtr, bufLen, bytes, retReceivedPtr);
        if (write !== WASI_ERRNO_SUCCESS) {
          return write;
        }
        if ((recvFlags & HOST_NET_MSG_TRUNC) !== 0 && bytes.length > (Number(bufLen) >>> 0)) {
          return writeGuestUint32(retReceivedPtr, bytes.length);
        }
        return WASI_ERRNO_SUCCESS;
      }
      if (!socket.socketId || socket.closed) {
        return WASI_ERRNO_BADF;
      }
      const peek = (recvFlags & HOST_NET_MSG_PEEK) !== 0;
      if ((Number(bufLen) >>> 0) === 0) {
        return writeGuestUint32(retReceivedPtr, 0);
      }

      // Non-blocking sockets (O_NONBLOCK via net_set_nonblock, used by libxcb's poll_for_*):
      // pull whatever is queued, do ONE short readiness probe, and return EAGAIN if still empty
      // instead of blocking. libxcb assumes its "poll" reads never block on an empty socket.
      if (socket.nonblock) {
        let queued = peek ? peekHostNetBytes(socket, bufLen) : dequeueHostNetBytes(socket, bufLen);
        if (queued.length > 0) {
          return writeGuestBytes(bufPtr, bufLen, queued, retReceivedPtr);
        }
        if (socket.lastError) return mapHostProcessError(socket.lastError);
        if (socket.readableEnded || socket.closed || !socket.socketId) {
          return writeGuestUint32(retReceivedPtr, 0);
        }
        const result = readReadyHostNetSocket(socket, bufLen, peek, 0);
        if (result?.kind === 'data' && result.bytes.length > 0) {
          return writeGuestBytes(bufPtr, bufLen, result.bytes, retReceivedPtr);
        }
        queued = peek ? peekHostNetBytes(socket, bufLen) : dequeueHostNetBytes(socket, bufLen);
        if (queued.length > 0) {
          return writeGuestBytes(bufPtr, bufLen, queued, retReceivedPtr);
        }
        if (socket.readableEnded || socket.closed || !socket.socketId) {
          return writeGuestUint32(retReceivedPtr, 0);
        }
        return WASI_ERRNO_AGAIN;
      }

      const startedAt = Date.now();
      const receiveDeadline = socket.recvTimeoutMs == null
        ? null
        : startedAt + Math.max(0, socket.recvTimeoutMs);
      const safeguardDeadline = startedAt + unixConnectTimeoutMs;
      let warnedNearLimit = false;
      while (true) {
        if (dispatchPendingWasmSignals(true)) return WASI_ERRNO_INTR;
        const queued = peek ? peekHostNetBytes(socket, bufLen) : dequeueHostNetBytes(socket, bufLen);
        if (queued.length > 0) {
          return writeGuestBytes(bufPtr, bufLen, queued, retReceivedPtr);
        }

        if (socket.lastError) {
          return mapHostProcessError(socket.lastError);
        }

        if (socket.readableEnded || socket.closed || !socket.socketId) {
          return writeGuestUint32(retReceivedPtr, 0);
        }

        const now = Date.now();
        if (receiveDeadline != null && now >= receiveDeadline) {
          return WASI_ERRNO_AGAIN;
        }
        warnedNearLimit = warnNearBlockingReadLimit(
          'blocking socket receive',
          startedAt,
          warnedNearLimit,
        );
        if (now >= safeguardDeadline) {
          process.stderr.write(
            `[agentos] blocking socket receive exceeded limits.resources.maxBlockingReadMs (${unixConnectTimeoutMs} ms); raise limits.resources.maxBlockingReadMs if needed\n`,
          );
          return WASI_ERRNO_TIMEDOUT;
        }
        const nextDeadline = receiveDeadline == null
          ? safeguardDeadline
          : Math.min(receiveDeadline, safeguardDeadline);
        // A child can be the peer that produces this read. Keep the sidecar
        // probe nonblocking in that case and return to child_process.poll
        // between attempts so the child's recv/send RPCs can run.
        const pumpsLocalChildren = hasActiveSpawnedChildren();
        const pollWaitMs = pumpsLocalChildren
          ? 0
          : Math.max(0, Math.min(50, nextDeadline - now));
        const result = readReadyHostNetSocket(socket, bufLen, peek, pollWaitMs);
        if (dispatchPendingWasmSignals(true)) return WASI_ERRNO_INTR;
        if (result?.kind === 'data' && result.bytes.length > 0) {
          return writeGuestBytes(bufPtr, bufLen, result.bytes, retReceivedPtr);
        }
        if (pumpsLocalChildren) {
          pumpSpawnedChildren(SPAWNED_CHILD_WAIT_SLICE_MS);
          if (dispatchPendingWasmSignals(true)) return WASI_ERRNO_INTR;
        }
        if (receiveDeadline != null && Date.now() >= receiveDeadline) {
          return WASI_ERRNO_AGAIN;
        }
      }
    } catch (error) {
      return mapHostProcessError(error);
    }
  },
  net_sendto(fd, bufPtr, bufLen, flags, addrPtr, addrLen, retSentPtr) {
    if (!guestRangesAreValid(
      [bufPtr, Number(bufLen) >>> 0],
      [addrPtr, Number(addrLen) >>> 0],
      [retSentPtr, 4],
    )) {
      return WASI_ERRNO_FAULT;
    }
    const socket = getHostNetSocket(fd);
    if (!socket || socket.closed) {
      return WASI_ERRNO_BADF;
    }

    try {
      if ((Number(flags) >>> 0) !== 0) {
        return WASI_ERRNO_INVAL;
      }
      const { host, port } = parseHostNetAddress(readGuestString(addrPtr, addrLen));
      const chunk = readGuestBytes(bufPtr, bufLen);
      if (socket.managed === true) {
        const result = callSyncRpc('process.hostnet_send', [
          managedHostNetTargetFd(socket),
          chunk,
          Number(flags) >>> 0,
          { type: 'inet', host, port },
          unixConnectTimeoutMs,
        ]);
        const written = Number(result?.bytes ?? result) >>> 0;
        return writeGuestUint32(retSentPtr, written);
      }
      const udpSocketId = ensureHostNetUdpSocket(socket);
      if (!udpSocketId) {
        return WASI_ERRNO_FAULT;
      }
      const result = callSyncRpc('dgram.send', [
        udpSocketId,
        chunk,
        { address: host, port },
      ]);
      socket.localInfo = normalizeHostNetAddressInfo(result?.localAddress, result?.localPort);
      const written = Number(result?.bytes) >>> 0;
      return writeGuestUint32(retSentPtr, written);
    } catch (error) {
      return mapHostProcessError(error);
    }
  },
  net_recvfrom(fd, bufPtr, bufLen, flags, retReceivedPtr, retAddrPtr, retAddrLenPtr) {
    const socket = getHostNetSocket(fd);
    if (!socket || socket.closed) {
      return WASI_ERRNO_BADF;
    }

    try {
      if (!guestRangesAreValid(
        [bufPtr, Number(bufLen) >>> 0],
        [retReceivedPtr, 4],
        [retAddrLenPtr, 4],
      )) {
        return WASI_ERRNO_FAULT;
      }
      const addressCapacity = readGuestUint32(retAddrLenPtr);
      if (!guestRangeIsValid(retAddrPtr, addressCapacity)) return WASI_ERRNO_FAULT;
      const recvFlags = Number(flags) >>> 0;
      const supportedFlags = HOST_NET_MSG_PEEK | HOST_NET_MSG_DONTWAIT | HOST_NET_MSG_TRUNC;
      if ((recvFlags & ~supportedFlags) !== 0) {
        return WASI_ERRNO_INVAL;
      }
      if (socket.managed === true) {
        const nonblocking = (recvFlags & HOST_NET_MSG_DONTWAIT) !== 0;
        const configured = nonblocking ? null : callSyncRpc(
          'process.hostnet_get_option',
          [managedHostNetTargetFd(socket), 'receive-timeout'],
        );
        const configuredTimeout = Number(configured?.durationMs);
        const hasConfiguredTimeout = configured?.durationMs != null
          && Number.isFinite(configuredTimeout) && configuredTimeout >= 0;
        const waitLimit = nonblocking
          ? 0
          : hasConfiguredTimeout ? configuredTimeout : unixConnectTimeoutMs;
        const startedAt = Date.now();
        let event;
        for (;;) {
          if (!nonblocking) {
            const remaining = Math.max(0, waitLimit - (Date.now() - startedAt));
            const readable = waitManagedHostNetReadable(socket, remaining, true);
            if (readable === WASI_ERRNO_INTR) return WASI_ERRNO_INTR;
            if (!readable) {
              return hasConfiguredTimeout ? WASI_ERRNO_AGAIN : WASI_ERRNO_TIMEDOUT;
            }
          }
          event = callSyncRpc('process.hostnet_recv', [
            managedHostNetTargetFd(socket),
            Number(bufLen) >>> 0,
            recvFlags,
            0,
          ]);
          if (event != null && !isHostNetWouldBlock(event)) break;
          if (nonblocking) return WASI_ERRNO_AGAIN;
          if (Date.now() - startedAt >= waitLimit) {
            return hasConfiguredTimeout ? WASI_ERRNO_AGAIN : WASI_ERRNO_TIMEDOUT;
          }
        }
        const bytes = hostNetDatagramBytes(event);
        const dataResult = writeGuestBytes(bufPtr, bufLen, bytes, retReceivedPtr);
        if (dataResult !== WASI_ERRNO_SUCCESS) return dataResult;
        const address = Buffer.from(formatHostNetAddressInfo({
          address: event.remoteAddress,
          port: event.remotePort,
        }), 'utf8');
        const addressResult = writeGuestBytes(retAddrPtr, addressCapacity, address, retAddrLenPtr);
        if (addressResult !== WASI_ERRNO_SUCCESS) return addressResult;
        if ((recvFlags & HOST_NET_MSG_TRUNC) !== 0 && bytes.length > (Number(bufLen) >>> 0)) {
          return writeGuestUint32(retReceivedPtr, bytes.length);
        }
        return WASI_ERRNO_SUCCESS;
      }
      const udpSocketId = ensureHostNetUdpSocket(socket);
      if (!udpSocketId) {
        return WASI_ERRNO_FAULT;
      }
      const event = receiveHostNetDatagramEvent(socket, recvFlags);
      if (!event) {
        return WASI_ERRNO_AGAIN;
      }
      const bytes = hostNetDatagramBytes(event);
      const dataResult = writeGuestBytes(bufPtr, bufLen, bytes, retReceivedPtr);
      if (dataResult !== WASI_ERRNO_SUCCESS) {
        return dataResult;
      }
      if (!event.remoteAddress || !Number.isInteger(Number(event.remotePort))) {
        return WASI_ERRNO_BADF;
      }
      let address;
      try {
        address = Buffer.from(formatHostNetAddressInfo({
          address: event.remoteAddress,
          port: event.remotePort,
        }), 'utf8');
      } catch {
        return WASI_ERRNO_INVAL;
      }
      const addressResult = writeGuestBytes(retAddrPtr, addressCapacity, address, retAddrLenPtr);
      if (addressResult !== WASI_ERRNO_SUCCESS) {
        return addressResult;
      }
      if ((recvFlags & HOST_NET_MSG_TRUNC) !== 0 && bytes.length > (Number(bufLen) >>> 0)) {
        return writeGuestUint32(retReceivedPtr, bytes.length);
      }
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapHostProcessError(error);
    }
  },
  net_setsockopt(fd, level, optname, optvalPtr, optvalLen) {
    const socket = getHostNetSocket(fd);
    if (!socket || socket.closed) {
      return WASI_ERRNO_BADF;
    }
    const sockoptKind = hostNetSockoptKind(level, optname, optvalLen);
    if (sockoptKind == null) {
      return WASI_ERRNO_INVAL;
    }
    if (sockoptKind === 'ignore') {
      return WASI_ERRNO_SUCCESS;
    }
    try {
      const timeoutMs = parseHostNetTimevalMs(readGuestBytes(optvalPtr, optvalLen));
      if (timeoutMs == null && readGuestBytes(optvalPtr, optvalLen).some((byte) => byte !== 0)) {
        return WASI_ERRNO_INVAL;
      }
      if (sockoptKind === 'recv-timeout') {
        if (socket.managed === true) {
          callSyncRpc('process.hostnet_set_option', [
            managedHostNetTargetFd(socket),
            'receive-timeout',
            { durationMs: timeoutMs },
          ]);
        } else {
          socket.recvTimeoutMs = timeoutMs;
        }
      }
    } catch {
      return WASI_ERRNO_FAULT;
    }
    return WASI_ERRNO_SUCCESS;
  },
  net_getsockopt(fd, level, optname, optvalPtr, optvalLenPtr) {
    const socket = getHostNetSocket(fd);
    if (!socket || socket.closed) {
      return WASI_ERRNO_BADF;
    }

    try {
      if (!guestRangeIsValid(optvalLenPtr, 4)) return WASI_ERRNO_FAULT;
      const optvalLen = readGuestUint32(optvalLenPtr);
      if (!guestRangeIsValid(optvalPtr, optvalLen)) return WASI_ERRNO_FAULT;
      const normalizedLevel = Number(level) >>> 0;
      const normalizedOptname = Number(optname) >>> 0;
      if (
        (normalizedLevel === HOST_NET_SOL_SOCKET ||
          normalizedLevel === HOST_NET_WASI_SOL_SOCKET) &&
        normalizedOptname === HOST_NET_SO_ERROR
      ) {
        if (optvalLen < 4) {
          return WASI_ERRNO_INVAL;
        }
        const socketError = socket.managed === true
          ? Number(callSyncRpc('process.hostnet_get_option', [
            managedHostNetTargetFd(socket),
            'error',
          ])) | 0
          : 0;
        new DataView(instanceMemory.buffer).setInt32(
          Number(optvalPtr) >>> 0,
          socketError,
          true,
        );
        return writeGuestUint32(optvalLenPtr, 4);
      }
      return WASI_ERRNO_INVAL;
    } catch {
      return WASI_ERRNO_FAULT;
    }
  },
  net_close(fd) {
    const numericFd = Number(fd) >>> 0;
    const socket = getHostNetSocket(numericFd);
    if (!socket) {
      return WASI_ERRNO_BADF;
    }
    if (SIDECAR_MANAGED_PROCESS) {
      const handle = lookupFdHandle(numericFd);
      if (handle?.kind !== 'kernel-fd') {
        return WASI_ERRNO_BADF;
      }
      try {
        const closed = activeFdProjections.get(numericFd) === handle
          ? closeSyntheticFd(numericFd)
          : closePassthroughFd(numericFd);
        if (!closed) return WASI_ERRNO_BADF;
        return WASI_ERRNO_SUCCESS;
      } catch (error) {
        return mapHostProcessError(error);
      }
    } else {
      standaloneHostNetSockets.delete(numericFd);
      // dup/dup2 aliases share one compatibility object when no kernel exists.
      if ([...standaloneHostNetSockets.values()].some((candidate) => candidate === socket)) {
        return WASI_ERRNO_SUCCESS;
      }
    }
    return SIDECAR_MANAGED_PROCESS
      ? WASI_ERRNO_SUCCESS
      : cleanupDetachedHostNetSocket(numericFd, socket);
  },
  net_tls_connect(fd, hostnamePtr, hostnameLen, flags = 0) {
    const socket = getHostNetSocket(fd);
    if (!socket || (socket.managed !== true && (!socket.socketId || socket.closed))) {
      return WASI_ERRNO_BADF;
    }

    try {
      const servername = readGuestString(hostnamePtr, hostnameLen);
      if (socket.managed === true) {
        const rejectUnauthorized = !(
          (Number(flags) & 1) === 1 || guestEnv.NODE_TLS_REJECT_UNAUTHORIZED === '0'
        );
        callSyncRpc('process.hostnet_tls_connect', [
          managedHostNetTargetFd(socket),
          servername,
          [],
          unixConnectTimeoutMs,
          rejectUnauthorized,
        ]);
        return WASI_ERRNO_SUCCESS;
      }
      const tlsOptions = { servername };
      if ((Number(flags) & 1) === 1 || guestEnv.NODE_TLS_REJECT_UNAUTHORIZED === '0') {
        tlsOptions.rejectUnauthorized = false;
      }
      callSyncRpc('net.socket_upgrade_tls', [
        socket.socketId,
        JSON.stringify(tlsOptions),
      ]);
      return WASI_ERRNO_SUCCESS;
    } catch {
      return WASI_ERRNO_FAULT;
    }
  },
};

const hostProcessImport = {
        proc_spawn(
          argvPtr,
          argvLen,
          envpPtr,
          envpLen,
          stdinFd,
          stdoutFd,
          stderrFd,
          cwdPtr,
          cwdLen,
          retPidPtr,
        ) {
          if (!guestRangeIsValid(retPidPtr, 4)) return WASI_ERRNO_FAULT;
          // Legacy ABI used by checked-in command modules. In this contract the
          // executable is argv[0]; newer callers use proc_spawn_v2 so argv[0]
          // can differ from the executable path.
          try {
            const argvBytes = readGuestBytes(argvPtr, argvLen);
            const commandLength = argvBytes.indexOf(0);
            if (commandLength <= 0) {
              return WASI_ERRNO_FAULT;
            }
            return hostProcessImport.proc_spawn_v2(
              argvPtr,
              commandLength,
              argvPtr,
              argvLen,
              envpPtr,
              envpLen,
              stdinFd,
              stdoutFd,
              stderrFd,
              cwdPtr,
              cwdLen,
              retPidPtr,
            );
          } catch (error) {
            traceHostProcess('proc-spawn-legacy-fault', {
              message: error instanceof Error ? error.message : String(error),
            });
            return mapHostProcessError(error);
          }
        },
        proc_spawn_v3(
          execPathPtr,
          execPathLen,
          argvPtr,
          argvLen,
          envpPtr,
          envpLen,
          actionsPtr,
          actionsLen,
          cwdPtr,
          cwdLen,
          attrFlags,
          sigDefaultLo,
          sigDefaultHi,
          sigMaskLo,
          sigMaskHi,
          pgroup,
          retPidPtr,
        ) {
          if (!guestRangeIsValid(retPidPtr, 4)) return WASI_ERRNO_FAULT;
          const flags = Number(attrFlags) >>> 0;
          if ((flags & ~SUPPORTED_POSIX_SPAWN_FLAGS) !== 0) {
            return WASI_ERRNO_NOTSUP;
          }
          if (activeSpawnCallContext !== null) {
            return WASI_ERRNO_FAULT;
          }
          try {
            const initialCwd =
              Number(cwdLen) > 0 ? readGuestString(cwdPtr, cwdLen) : undefined;
            const actions = decodeSpawnActions(actionsPtr, actionsLen, initialCwd);
            const defaultSignals =
              flags & POSIX_SPAWN_SETSIGDEF
                ? decodeSignalMask(sigDefaultLo, sigDefaultHi)
                : [];
            const signalMask = decodeSignalMask(sigMaskLo, sigMaskHi).filter(
              (signal) => signal !== LINUX_SIGKILL && signal !== LINUX_SIGSTOP,
            );
            activeSpawnCallContext = {
              attrFlags: flags,
              // v3 implements posix_spawn, not posix_spawnp. Preserve the
              // caller's executable path and argv exactly instead of routing
              // through Node-style package/PATH resolution.
              exactExecPath: true,
              searchPath: null,
              pgroup: Number(pgroup) | 0,
              signalDefaults: defaultSignals,
              signalMask,
              fileActions: actions.actions,
            };
            return hostProcessImport.proc_spawn_v2(
              execPathPtr,
              execPathLen,
              argvPtr,
              argvLen,
              envpPtr,
              envpLen,
              actions.stdio[0],
              actions.stdio[1],
              actions.stdio[2],
              cwdPtr,
              0,
              retPidPtr,
              initialCwd,
            );
          } catch (error) {
            return mapHostProcessError(error);
          } finally {
            activeSpawnCallContext = null;
          }
        },
        proc_spawn_v4(
          execPathPtr,
          execPathLen,
          argvPtr,
          argvLen,
          envpPtr,
          envpLen,
          actionsPtr,
          actionsLen,
          cwdPtr,
          cwdLen,
          searchPathPtr,
          searchPathLen,
          attrFlags,
          sigDefaultLo,
          sigDefaultHi,
          sigMaskLo,
          sigMaskHi,
          pgroup,
          schedPolicy,
          schedPriority,
          retPidPtr,
        ) {
          if (!guestRangeIsValid(retPidPtr, 4)) return WASI_ERRNO_FAULT;
          const flags = Number(attrFlags) >>> 0;
          if ((flags & ~SUPPORTED_POSIX_SPAWN_FLAGS) !== 0) {
            return WASI_ERRNO_NOTSUP;
          }
          if (
            (flags & POSIX_SPAWN_SETSID) !== 0 &&
            (flags & POSIX_SPAWN_SETPGROUP) !== 0
          ) {
            // Linux rejects this combination because setsid() makes the child
            // a process-group leader before the requested setpgid operation.
            return WASI_ERRNO_PERM;
          }
          const requestedPolicy = Number(schedPolicy) | 0;
          const requestedPriority = Number(schedPriority) | 0;
          if (
            (flags & (POSIX_SPAWN_SETSCHEDPARAM | POSIX_SPAWN_SETSCHEDULER)) !== 0 &&
            requestedPriority !== 0
          ) {
            // SCHED_OTHER has exactly one valid priority on Linux.
            return WASI_ERRNO_INVAL;
          }
          if (
            (flags & POSIX_SPAWN_SETSCHEDULER) !== 0 &&
            requestedPolicy !== 0
          ) {
            // AgentOS exposes SCHED_OTHER. Real-time policies require host
            // scheduling privileges and are deliberately not virtualized.
            return WASI_ERRNO_PERM;
          }
          if (activeSpawnCallContext !== null) {
            return WASI_ERRNO_FAULT;
          }
          try {
            const initialCwd =
              Number(cwdLen) > 0 ? readGuestString(cwdPtr, cwdLen) : undefined;
            const actions = decodeSpawnActions(actionsPtr, actionsLen, initialCwd);
            // The pointer carries presence independently from the length:
            // posix_spawn passes NULL/0, while posix_spawnp with PATH=""
            // passes a non-NULL pointer and zero length. Linux treats that
            // empty PATH as one current-directory entry.
            const searchPath =
              Number(searchPathPtr) !== 0
                ? readGuestString(searchPathPtr, searchPathLen)
                : null;
            const defaultSignals =
              flags & POSIX_SPAWN_SETSIGDEF
                ? decodeSignalMask(sigDefaultLo, sigDefaultHi)
                : [];
            const signalMask = decodeSignalMask(sigMaskLo, sigMaskHi).filter(
              (signal) => signal !== LINUX_SIGKILL && signal !== LINUX_SIGSTOP,
            );
            activeSpawnCallContext = {
              attrFlags: flags,
              exactExecPath: searchPath === null,
              searchPath,
              schedPolicy: requestedPolicy,
              schedPriority: requestedPriority,
              pgroup: Number(pgroup) | 0,
              signalDefaults: defaultSignals,
              signalMask,
              fileActions: actions.actions,
            };
            return hostProcessImport.proc_spawn_v2(
              execPathPtr,
              execPathLen,
              argvPtr,
              argvLen,
              envpPtr,
              envpLen,
              actions.stdio[0],
              actions.stdio[1],
              actions.stdio[2],
              cwdPtr,
              0,
              retPidPtr,
              initialCwd,
            );
          } catch (error) {
            return mapHostProcessError(error);
          } finally {
            activeSpawnCallContext = null;
          }
        },
        proc_spawn_v2(
          execPathPtr,
          execPathLen,
          argvPtr,
          argvLen,
          envpPtr,
          envpLen,
          stdinFd,
          stdoutFd,
          stderrFd,
          cwdPtr,
          cwdLen,
          retPidPtr,
          resolvedCwdOverride,
        ) {
          if (!guestRangeIsValid(retPidPtr, 4)) return WASI_ERRNO_FAULT;
          if (permissionTier !== 'full') {
            return WASI_ERRNO_FAULT;
          }
          traceHostProcess('proc-spawn-call', {
            execPathPtr: Number(execPathPtr) >>> 0,
            execPathLen: Number(execPathLen) >>> 0,
            argvPtr: Number(argvPtr) >>> 0,
            argvLen: Number(argvLen) >>> 0,
            envpPtr: Number(envpPtr) >>> 0,
            envpLen: Number(envpLen) >>> 0,
            stdinFd: Number(stdinFd) >>> 0,
            stdoutFd: Number(stdoutFd) >>> 0,
            stderrFd: Number(stderrFd) >>> 0,
            cwdPtr: Number(cwdPtr) >>> 0,
            cwdLen: Number(cwdLen) >>> 0,
            retPidPtr: Number(retPidPtr) >>> 0,
          });
          try {
            const command = readGuestString(execPathPtr, execPathLen);
            if (command.length === 0) {
              return WASI_ERRNO_NOENT;
            }
            const argv = decodeNullSeparatedStrings(readGuestBytes(argvPtr, argvLen));
            const argv0 = argv[0] ?? command;
            const args = argv.slice(1);
            const env = parseSerializedEnv(readGuestBytes(envpPtr, envpLen));
            const cwd =
              typeof resolvedCwdOverride === 'string'
                ? resolvedCwdOverride
                : Number(cwdLen) > 0
                  ? readGuestString(cwdPtr, cwdLen)
                  : undefined;
            const stdinTarget = resolveSpawnFd(stdinFd);
            const stdoutTarget = resolveSpawnFd(stdoutFd);
            const stderrTarget = resolveSpawnFd(stderrFd);
            const syntheticResult = maybeCreateSyntheticCommandResult(command, args, cwd);
            if (syntheticResult) {
              const record = createSyntheticChildRecord(
                syntheticResult,
                stdinTarget,
                stdoutTarget,
                stderrTarget,
              );
              record.processGroup = Number(
                callSyncRpc('process.getpgid', [VIRTUAL_PID]),
              ) >>> 0;
              childCorrelationsByPid.set(record.pid, record);
              childCorrelationsById.set(record.childId, record);
              traceHostProcess('proc-spawn-synthetic', {
                command,
                childId: record.childId,
                pid: record.pid,
                exitCode: syntheticResult.exitCode,
              });
              emitSyntheticCommandOutput(record, syntheticResult);
              return writeGuestUint32(retPidPtr, record.pid);
            }
            traceHostProcess('proc-spawn-begin', {
              command,
              argv0,
              args,
              cwd: cwd ?? null,
              stdinFd: Number(stdinFd) >>> 0,
              stdoutFd: Number(stdoutFd) >>> 0,
              stderrFd: Number(stderrFd) >>> 0,
              stdinTarget,
              stdoutTarget,
              stderrTarget,
              kernelFdMappings: kernelFdMappingsForSpawn(),
              internalBootstrapEnv:
                activeSpawnCallContext?.internalBootstrapEnv ?? {},
              spawnAttrFlags: activeSpawnCallContext?.attrFlags ?? 0,
              spawnPgroup: activeSpawnCallContext?.pgroup ?? null,
            });
            let stdinRedirectBytes = null;
            if (
              stdinTarget > 2 &&
              stdinTarget !== 0xffffffff &&
              !spawnStdinFdIsSyntheticPipe(stdinTarget) &&
              !spawnFdIsKernelBacked(stdinTarget)
            ) {
              stdinRedirectBytes = readSpawnStdinRedirectBytes(stdinTarget);
              if (stdinRedirectBytes == null) {
                traceHostProcess('proc-spawn-stdin-redirect-unreadable', {
                  command,
                  stdinFd: stdinTarget,
                });
                return WASI_ERRNO_FAULT;
              }
            }
            const result = callSyncRpc('child_process.spawn', [
              {
                command,
                args,
                options: {
                  argv0,
                  cwd,
                  env,
                  internalBootstrapEnv:
                    activeSpawnCallContext?.internalBootstrapEnv ?? {},
                  spawnAttrFlags: activeSpawnCallContext?.attrFlags ?? 0,
                  spawnExactPath: activeSpawnCallContext?.exactExecPath ?? false,
                  spawnSearchPath: activeSpawnCallContext?.searchPath,
                  spawnSchedPolicy: activeSpawnCallContext?.schedPolicy,
                  spawnSchedPriority: activeSpawnCallContext?.schedPriority,
                  spawnPgroup: activeSpawnCallContext?.pgroup,
                  spawnSignalDefaults: activeSpawnCallContext?.signalDefaults ?? [],
                  spawnSignalMask: activeSpawnCallContext?.signalMask ?? [],
                  spawnFileActions: activeSpawnCallContext?.fileActions ?? [],
                  spawnFdMappings: kernelFdMappingsForSpawn(),
                  spawnHostNetFds: hostNetFdsForSpawn(),
                  shell: false,
                  stdio: [
                    stdinTarget === 0
                      ? 'inherit'
                      : stdinTarget === 0xffffffff
                        ? 'ignore'
                        : 'pipe',
                    stdoutTarget === 1
                      ? 'inherit'
                      : stdoutTarget === 0xffffffff
                        ? 'ignore'
                        : 'pipe',
                    stderrTarget === 2
                      ? 'inherit'
                      : stderrTarget === 0xffffffff
                        ? 'ignore'
                        : 'pipe',
                  ],
                },
              },
            ]);
            const pid = Number(result?.pid) >>> 0;
            if (!Number.isInteger(pid) || pid === 0 || typeof result?.childId !== 'string') {
              return WASI_ERRNO_FAULT;
            }
            let processGroup = 0;
            if (!SIDECAR_MANAGED_PROCESS) {
              processGroup = Number(result?.pgid) >>> 0;
              if (processGroup === 0) {
                processGroup = Number(callSyncRpc('process.getpgid', [pid])) >>> 0;
              }
            }

            const directPosixStdin =
              result?.directPosixStdin === true ||
              spawnActionsControlGuestFd(activeSpawnCallContext?.fileActions, 0);
            // A POSIX file action that installs stdout/stderr gives the child
            // its own kernel descriptor. Routing the child event through the
            // parent as well duplicates bytes, and retaining the parent's pipe
            // end until child exit creates an EOF/exit ownership cycle.
            const directPosixStdout =
              spawnActionsControlGuestFd(activeSpawnCallContext?.fileActions, 1);
            const directPosixStderr =
              spawnActionsControlGuestFd(activeSpawnCallContext?.fileActions, 2);
            const stdinPipe = directPosixStdin
              ? null
              : registerPipeConsumer(stdinTarget, result.childId, 'stdin');
            const stdoutPipe = directPosixStdout
              ? null
              : registerPipeProducer(stdoutTarget, result.childId, 'stdout');
            const stderrPipe = directPosixStderr
              ? null
              : registerPipeProducer(stderrTarget, result.childId, 'stderr');
            // A POSIX spawn action that controls fd 1/2 installs the target in
            // the child's kernel fd table. Keeping a parent-side routing
            // reference in that case suppresses EOF after the child closes its
            // descriptor and can deadlock protocols that use close(2) as a
            // message boundary. Only event-routed output needs retention.
            const retainedSpawnOutputHandles = [
              directPosixStdout ? null : stdoutTarget,
              directPosixStderr ? null : stderrTarget,
            ]
              .filter((fd) => fd != null)
              .filter((fd, index, values) => values.indexOf(fd) === index)
              .map((fd) => retainSpawnOutputHandle(fd))
              .filter(Boolean);
            const delegateRetainedFds = [
              directPosixStdin ? null : stdinTarget,
              directPosixStdout ? null : stdoutTarget,
              directPosixStderr ? null : stderrTarget,
            ].filter(
              (fd, index, values) =>
                fd != null &&
                fd > 2 &&
                standaloneDelegateFdRefCounts.has(fd) &&
                values.indexOf(fd) === index,
            );
            for (const fd of delegateRetainedFds) {
              retainDelegateFd(fd);
            }
            const record = {
              childId: result.childId,
              pid,
              stdinFd: stdinTarget,
              directPosixStdin,
              stdoutFd: directPosixStdout ? 0xffffffff : stdoutTarget,
              stderrFd: directPosixStderr ? 0xffffffff : stderrTarget,
              stdinPipe,
              stdoutPipe,
              stderrPipe,
              stdinReadyAtMs: Date.now() + 100,
              delegateRetainedFds,
              retainedSpawnOutputHandles,
              exitCode: null,
              exitSignal: null,
              exitStatus: null,
              rawWaitStatus: null,
              ...(SIDECAR_MANAGED_PROCESS ? {} : { processGroup }),
            };
            childCorrelationsByPid.set(pid, record);
            childCorrelationsById.set(result.childId, record);
            traceHostProcess('proc-spawn-ready', {
              command,
              childId: result.childId,
              pid,
              directPosixStdin,
              resolvedArgs: result.args ?? null,
            });
            if (stdinRedirectBytes != null) {
              if (stdinRedirectBytes.length > 0) {
                callSyncRpc('child_process.write_stdin', [
                  result.childId,
                  stdinRedirectBytes,
                ]);
              }
              callSyncRpc('child_process.close_stdin', [result.childId]);
            }
            return writeGuestUint32(retPidPtr, pid);
          } catch (error) {
            traceHostProcess('proc-spawn-fault', {
              message: error instanceof Error ? error.message : String(error),
            });
            return mapHostProcessError(error);
          }
        },
        proc_exec(
          execPathPtr,
          execPathLen,
          argvPtr,
          argvLen,
          envpPtr,
          envpLen,
          cloexecFdsPtr,
          cloexecFdsLen,
        ) {
          if (permissionTier !== 'full') {
            return WASI_ERRNO_PERM;
          }
          try {
            const command = readGuestString(execPathPtr, execPathLen);
            if (command.length === 0) {
              return WASI_ERRNO_NOENT;
            }
            const argv = decodeNullSeparatedStrings(readGuestBytes(argvPtr, argvLen));
            const env = parseSerializedEnv(readGuestBytes(envpPtr, envpLen));
            const closeFds = readExecCloexecFds(cloexecFdsPtr, cloexecFdsLen);
            let replacement;
            try {
              replacement = loadExecImageFromPath(command, argv);
            } catch (loadError) {
              // The trusted sidecar owns AgentOS image selection. The local
              // runner only replaces WASM->WASM after successful compilation;
              // an otherwise valid non-WASM image is prepared and committed
              // by the sidecar without ever resuming this image.
              if (SIDECAR_EXEC_COMMIT_RPC && loadError?.code === 'ENOEXEC') {
                callSyncRpc('process.exec', [{
                  command,
                  args: argv.slice(1),
                  options: {
                    argv0: argv[0] ?? command,
                    env,
                    shell: false,
                    cloexecFds: kernelCloexecFdsForCommit(closeFds),
                    localReplacement: false,
                    internalBootstrapEnv: {},
                  },
                }]);
                const returned = new Error(
                  'cross-runtime process.exec returned after committing the replacement image',
                );
                returned.code = 'EIO';
                throw returned;
              }
              throw loadError;
            }
            let sidecarCommitted = false;
            if (SIDECAR_EXEC_COMMIT_RPC) {
              const result = callSyncRpc('process.exec', [{
                command,
                args: replacement.argv.slice(1),
                options: {
                  argv0: replacement.argv[0] ?? command,
                  env,
                  shell: false,
                  cloexecFds: kernelCloexecFdsForCommit(closeFds),
                  localReplacement: true,
                  internalBootstrapEnv: {},
                },
              }]);
              if (result?.committed !== true) {
                const error = new Error('process.exec did not confirm the sidecar commit');
                error.code = 'EIO';
                throw error;
              }
              sidecarCommitted = true;
            }
            throw {
              marker: EXEC_REPLACEMENT_MARKER,
              image: {
                command,
                module: replacement.module,
                argv: replacement.argv,
                env,
                closeFds,
                sidecarCommitted,
              },
            };
          } catch (error) {
            if (isExecReplacement(error)) throw error;
            traceHostProcess('proc-exec-fault', {
              code: error?.code ?? null,
              message: error instanceof Error ? error.message : String(error),
            });
            return mapHostProcessError(error);
          }
        },
        proc_fexec(
          execFd,
          argvPtr,
          argvLen,
          envpPtr,
          envpLen,
          cloexecFdsPtr,
          cloexecFdsLen,
        ) {
          if (permissionTier !== 'full') {
            return WASI_ERRNO_PERM;
          }
          try {
            const descriptor = Number(execFd) >>> 0;
            const argv = decodeNullSeparatedStrings(readGuestBytes(argvPtr, argvLen));
            const env = parseSerializedEnv(readGuestBytes(envpPtr, envpLen));
            const closeFds = readExecCloexecFds(cloexecFdsPtr, cloexecFdsLen);
            const replacement = loadExecImageFromFd(descriptor, argv, closeFds);
            let sidecarCommitted = false;
            if (SIDECAR_EXEC_COMMIT_RPC) {
              const result = callSyncRpc('process.exec_fd_image_commit', [{
                command: replacement.scriptRef,
                args: replacement.argv.slice(1),
                options: {
                  argv0: replacement.argv[0] ?? replacement.scriptRef,
                  env,
                  shell: false,
                  cloexecFds: kernelCloexecFdsForCommit(closeFds),
                  localReplacement: true,
                  executableFd: canonicalKernelFdForSpawnAction(descriptor),
                  internalBootstrapEnv: {},
                },
              }]);
              if (result?.committed !== true) {
                const error = new Error(
                  'process.exec_fd_image_commit did not confirm the sidecar commit',
                );
                error.code = 'EIO';
                throw error;
              }
              sidecarCommitted = true;
            }
            throw {
              marker: EXEC_REPLACEMENT_MARKER,
              image: {
                command: replacement.scriptRef,
                module: replacement.module,
                argv: replacement.argv,
                env,
                closeFds,
                sidecarCommitted,
              },
            };
          } catch (error) {
            if (isExecReplacement(error)) throw error;
            traceHostProcess('proc-fexec-fault', {
              code: error?.code ?? null,
              message: error instanceof Error ? error.message : String(error),
            });
            return mapHostProcessError(error);
          }
        },
        proc_waitpid(pid, options, retStatusPtr, retPidPtr) {
          if (!guestRangesAreValid([retStatusPtr, 4], [retPidPtr, 4])) {
            return WASI_ERRNO_FAULT;
          }
          const requestedPid = Number(pid) >>> 0;
          if (permissionTier !== 'full') {
            return WASI_ERRNO_CHILD;
          }
          if (SIDECAR_MANAGED_PROCESS) {
            try {
              const normalizedOptions = Number(options) >>> 0;
              if ((normalizedOptions & ~1) !== 0) return WASI_ERRNO_INVAL;
              const transition = takeManagedWaitTransition(
                Number(pid) | 0,
                normalizedOptions,
                (normalizedOptions & 1) === 0,
              );
              if (!transition) {
                if (writeGuestUint32(retStatusPtr, 0) !== WASI_ERRNO_SUCCESS) {
                  return WASI_ERRNO_FAULT;
                }
                return writeGuestUint32(retPidPtr, 0);
              }
              if (
                writeGuestUint32(retStatusPtr, Number(transition.status) >>> 0) !==
                WASI_ERRNO_SUCCESS
              ) {
                return WASI_ERRNO_FAULT;
              }
              const result = writeGuestUint32(retPidPtr, Number(transition.pid) >>> 0);
              if (result === WASI_ERRNO_SUCCESS) reapManagedChildCorrelation(transition);
              return result;
            } catch (error) {
              return mapHostProcessError(error);
            }
          }
          const waitAny = requestedPid === 0xffffffff;
          if (!waitAny && !childCorrelationsByPid.has(requestedPid)) {
            return WASI_ERRNO_CHILD;
          }
          if (childCorrelationsByPid.size === 0) {
            return WASI_ERRNO_CHILD;
          }

          try {
            const normalizedOptions = Number(options) >>> 0;
            const nonBlocking = (normalizedOptions & 1) !== 0;
            if ((normalizedOptions & ~1) !== 0) {
              return WASI_ERRNO_INVAL;
            }
            while (true) {
              const records = waitAny
                ? Array.from(childCorrelationsByPid.values())
                : [childCorrelationsByPid.get(requestedPid)].filter(Boolean);
              if (records.length === 0) {
                return WASI_ERRNO_CHILD;
              }
              for (const record of records) {
                if (typeof record.exitStatus === 'number') {
                  return returnLegacyWaitedChild(record, retStatusPtr, retPidPtr);
                }
                pumpChildInputPipe(record, 0);
                const event = pollChildEvent(record, 0);
                if (event) {
                  processChildEvent(record, event);
                }
                if (typeof record.exitStatus === 'number') {
                  return returnLegacyWaitedChild(record, retStatusPtr, retPidPtr);
                }
              }
              if (nonBlocking) {
                if (writeGuestUint32(retStatusPtr, 0) !== WASI_ERRNO_SUCCESS) {
                  return WASI_ERRNO_FAULT;
                }
                // Linux keeps every child scheduled while waitpid(...,
                // WNOHANG) only limits which status may be reaped. Give all
                // children one zero-time service sweep so a selected child can
                // consume pipeline data produced by a sibling, then re-check
                // the selected set before reporting that no status is ready.
                pumpSpawnedChildren(0);
                const readyRecord = records.find(
                  (record) => typeof record.exitStatus === 'number',
                );
                if (readyRecord) {
                  return returnLegacyWaitedChild(readyRecord, retStatusPtr, retPidPtr);
                }
                return writeGuestUint32(retPidPtr, 0);
              }
              // waitpid(pid) limits which child may be reaped; it does not
              // stop sibling processes from running. All WASM descendants
              // share this cooperative runner, so pump every live child while
              // waiting for the selected one. Otherwise a selected child can
              // deadlock waiting on output that only a sibling can produce.
              pumpSpawnedChildren(SPAWNED_CHILD_WAIT_SLICE_MS);
              const readyRecord = records.find(
                (record) => typeof record.exitStatus === 'number',
              );
              if (readyRecord) {
                return returnLegacyWaitedChild(readyRecord, retStatusPtr, retPidPtr);
              }
              // A matching status wins over a simultaneously delivered
              // signal. Otherwise a caught signal (including SIGCHLD from a
              // non-selected sibling) interrupts blocking waitpid on Linux.
              if (dispatchPendingWasmSignals(true)) {
                return WASI_ERRNO_INTR;
              }
            }
          } catch (error) {
            traceHostProcess('proc-waitpid-legacy-fault', {
              requestedPid,
              message: error instanceof Error ? error.message : String(error),
            });
            return WASI_ERRNO_FAULT;
          }
        },
        proc_waitpid_v2(
          pid,
          options,
          retExitCodePtr,
          retSignalPtr,
          retPidPtr,
          retCoreDumpedPtr,
        ) {
          if (!guestRangesAreValid(
            [retExitCodePtr, 4],
            [retSignalPtr, 4],
            [retPidPtr, 4],
            [retCoreDumpedPtr, 4],
          )) {
            return WASI_ERRNO_FAULT;
          }
          const requestedPid = Number(pid) >>> 0;
          if (permissionTier !== 'full') {
            return WASI_ERRNO_CHILD;
          }
          if (SIDECAR_MANAGED_PROCESS) {
            try {
              const normalizedOptions = Number(options) >>> 0;
              if ((normalizedOptions & ~1) !== 0) return WASI_ERRNO_INVAL;
              const transition = takeManagedWaitTransition(
                Number(pid) | 0,
                normalizedOptions,
                (normalizedOptions & 1) === 0,
              );
              if (!transition) return writeGuestUint32(retPidPtr, 0);
              for (const [ptr, value] of [
                [retExitCodePtr, transition.exitCode],
                [retSignalPtr, transition.signal],
                [retCoreDumpedPtr, transition.coreDumped ? 1 : 0],
                [retPidPtr, transition.pid],
              ]) {
                if (writeGuestUint32(ptr, Number(value) >>> 0) !== WASI_ERRNO_SUCCESS) {
                  return WASI_ERRNO_FAULT;
                }
              }
              reapManagedChildCorrelation(transition);
              return WASI_ERRNO_SUCCESS;
            } catch (error) {
              return mapHostProcessError(error);
            }
          }
          const waitAny = requestedPid === 0xffffffff;
          if (!waitAny && !childCorrelationsByPid.has(requestedPid)) {
            // Linux waitpid reports ECHILD when pid does not name a child of
            // this process. ESRCH is reserved for operations such as kill(2).
            return WASI_ERRNO_CHILD;
          }
          if (childCorrelationsByPid.size === 0) {
            return WASI_ERRNO_CHILD;
          }

          try {
            const normalizedOptions = Number(options) >>> 0;
            const nonBlocking = (normalizedOptions & 1) !== 0;
            if ((normalizedOptions & ~1) !== 0) {
              return WASI_ERRNO_INVAL;
            }
            traceHostProcess('proc-waitpid-begin', {
              requestedPid,
              waitAny,
            });

            while (true) {
              const records = waitAny
                ? Array.from(childCorrelationsByPid.values())
                : [childCorrelationsByPid.get(requestedPid)].filter(Boolean);
              if (records.length === 0) {
                return WASI_ERRNO_CHILD;
              }

              for (const record of records) {
                if (typeof record.exitStatus === 'number') {
                  return returnWaitedChild(
                    record,
                    retExitCodePtr,
                    retSignalPtr,
                    retPidPtr,
                    retCoreDumpedPtr,
                  );
                }

                pumpChildInputPipe(record, 0);
                const event = pollChildEvent(record, 0);
                if (!event) {
                  continue;
                }
                traceHostProcess('proc-waitpid-poll', {
                  requestedPid,
                  childId: record.childId,
                  type: event.type,
                });
                processChildEvent(record, event);
                if (typeof record.exitStatus === 'number') {
                  return returnWaitedChild(
                    record,
                    retExitCodePtr,
                    retSignalPtr,
                    retPidPtr,
                    retCoreDumpedPtr,
                  );
                }
              }

              if (nonBlocking) {
                // A zero-time sibling sweep supplies the scheduling progress
                // that Linux processes receive independently of waitpid. It
                // does not wait for an event, and only a matching child can be
                // reaped by this call.
                pumpSpawnedChildren(0);
                const readyRecord = records.find(
                  (record) => typeof record.exitStatus === 'number',
                );
                if (readyRecord) {
                  return returnWaitedChild(
                    readyRecord,
                    retExitCodePtr,
                    retSignalPtr,
                    retPidPtr,
                    retCoreDumpedPtr,
                  );
                }
                return writeGuestUint32(retPidPtr, 0);
              }

              // No child was ready. Block briefly on one child, then rescan all
              // children so waitpid(-1) returns whichever one changes state
              // first instead of waiting for map insertion order.
              // Keep non-selected siblings scheduled while waitpid limits
              // only which child status may be returned to the caller.
              pumpSpawnedChildren(SPAWNED_CHILD_WAIT_SLICE_MS);
              const readyRecord = records.find(
                (record) => typeof record.exitStatus === 'number',
              );
              if (readyRecord) {
                return returnWaitedChild(
                  readyRecord,
                  retExitCodePtr,
                  retSignalPtr,
                  retPidPtr,
                  retCoreDumpedPtr,
                );
              }
              if (dispatchPendingWasmSignals(true)) {
                return WASI_ERRNO_INTR;
              }
            }
          } catch (error) {
            traceHostProcess('proc-waitpid-fault', {
              requestedPid,
              message: error instanceof Error ? error.message : String(error),
            });
            return WASI_ERRNO_FAULT;
          }
        },
        proc_waitpid_v3(pid, options, retStatusPtr, retPidPtr) {
          if (!guestRangesAreValid([retStatusPtr, 4], [retPidPtr, 4])) {
            return WASI_ERRNO_FAULT;
          }
          const requestedPid = Number(pid) | 0;
          if (permissionTier !== 'full') {
            return WASI_ERRNO_CHILD;
          }
          const normalizedOptions = Number(options) >>> 0;
          const waitNoHang = (normalizedOptions & 1) !== 0;
          const waitUntraced = (normalizedOptions & 2) !== 0;
          const waitContinued = (normalizedOptions & 8) !== 0;
          if ((normalizedOptions & ~(1 | 2 | 8)) !== 0) {
            return WASI_ERRNO_INVAL;
          }

          try {
            if (SIDECAR_MANAGED_PROCESS) {
              const transition = takeManagedWaitTransition(
                requestedPid,
                normalizedOptions,
                !waitNoHang,
              );
              if (!transition) {
                if (writeGuestUint32(retStatusPtr, 0) !== WASI_ERRNO_SUCCESS) {
                  return WASI_ERRNO_FAULT;
                }
                return writeGuestUint32(retPidPtr, 0);
              }
              if (
                writeGuestUint32(retStatusPtr, Number(transition.rawStatus) >>> 0) !==
                WASI_ERRNO_SUCCESS
              ) {
                return WASI_ERRNO_FAULT;
              }
              const result = writeGuestUint32(retPidPtr, Number(transition.pid) >>> 0);
              if (result === WASI_ERRNO_SUCCESS) reapManagedChildCorrelation(transition);
              return result;
            }
            const callerProcessGroup =
              requestedPid === 0
                ? Number(callSyncRpc('process.getpgid', [VIRTUAL_PID])) >>> 0
                : 0;
            const matchingRecords = () => {
              if (requestedPid > 0) {
                return [childCorrelationsByPid.get(requestedPid)].filter(Boolean);
              }
              if (requestedPid === -1) {
                return Array.from(childCorrelationsByPid.values());
              }
              const selectedGroup =
                requestedPid === 0 ? callerProcessGroup : Math.abs(requestedPid) >>> 0;
              return Array.from(childCorrelationsByPid.values()).filter(
                (record) => record.processGroup === selectedGroup,
              );
            };

            if (matchingRecords().length === 0) {
              return WASI_ERRNO_CHILD;
            }

            while (true) {
              const records = matchingRecords();
              if (records.length === 0) {
                return WASI_ERRNO_CHILD;
              }

              if (
                (waitUntraced || waitContinued) &&
                records.some((record) => record.synthetic !== true)
              ) {
                const transition = callSyncRpc('process.waitpid_transition', [
                  requestedPid,
                  normalizedOptions,
                ]);
                if (transition && typeof transition.pid === 'number') {
                  const transitionedRecord = childCorrelationsByPid.get(
                    Number(transition.pid) >>> 0,
                  );
                  if (transitionedRecord && records.includes(transitionedRecord)) {
                    // Deliver the notification handler before returning the
                    // matching state change, without converting success to
                    // EINTR. This is the same ordering as child exit status.
                    dispatchPendingWasmSignals();
                    if (
                      writeGuestUint32(retStatusPtr, Number(transition.status) >>> 0) !==
                      WASI_ERRNO_SUCCESS
                    ) {
                      return WASI_ERRNO_FAULT;
                    }
                    return writeGuestUint32(retPidPtr, transitionedRecord.pid);
                  }
                }
              }

              for (const record of records) {
                if (typeof record.rawWaitStatus === 'number') {
                  return returnRawWaitedChild(record, retStatusPtr, retPidPtr);
                }
                pumpChildInputPipe(record, 0);
                const event = pollChildEvent(record, 0);
                if (event) {
                  processChildEvent(record, event);
                }
                if (typeof record.rawWaitStatus === 'number') {
                  return returnRawWaitedChild(record, retStatusPtr, retPidPtr);
                }
              }

              if (waitNoHang) {
                if (writeGuestUint32(retStatusPtr, 0) !== WASI_ERRNO_SUCCESS) {
                  return WASI_ERRNO_FAULT;
                }
                pumpSpawnedChildren(0);
                const readyRecord = records.find(
                  (record) => typeof record.rawWaitStatus === 'number',
                );
                if (readyRecord) {
                  return returnRawWaitedChild(readyRecord, retStatusPtr, retPidPtr);
                }
                return writeGuestUint32(retPidPtr, 0);
              }

              // Linux continues scheduling every child while the parent
              // blocks in waitpid(pid). The cooperative WASM runner must do
              // the same even though only a matching status can be reaped.
              pumpSpawnedChildren(SPAWNED_CHILD_WAIT_SLICE_MS);
              if (
                (waitUntraced || waitContinued) &&
                records.some((record) => record.synthetic !== true)
              ) {
                const transition = callSyncRpc('process.waitpid_transition', [
                  requestedPid,
                  normalizedOptions,
                ]);
                if (transition && typeof transition.pid === 'number') {
                  const transitionedRecord = childCorrelationsByPid.get(
                    Number(transition.pid) >>> 0,
                  );
                  if (transitionedRecord && records.includes(transitionedRecord)) {
                    dispatchPendingWasmSignals();
                    if (
                      writeGuestUint32(retStatusPtr, Number(transition.status) >>> 0) !==
                      WASI_ERRNO_SUCCESS
                    ) {
                      return WASI_ERRNO_FAULT;
                    }
                    return writeGuestUint32(retPidPtr, transitionedRecord.pid);
                  }
                }
              }
              const readyRecord = records.find(
                (record) => typeof record.rawWaitStatus === 'number',
              );
              if (readyRecord) {
                return returnRawWaitedChild(readyRecord, retStatusPtr, retPidPtr);
              }
              if (dispatchPendingWasmSignals(true)) {
                return WASI_ERRNO_INTR;
              }
            }
          } catch (error) {
            traceHostProcess('proc-waitpid-v3-fault', {
              requestedPid,
              message: error instanceof Error ? error.message : String(error),
            });
            return mapHostProcessError(error);
          }
        },
        proc_kill(pid, signal) {
          if (permissionTier !== 'full') {
            return WASI_ERRNO_SRCH;
          }
          const targetPid = Number(pid) | 0;
          const numericSignal = Number(signal) >>> 0;
          if (numericSignal > 31) return WASI_ERRNO_INVAL;
          const signalName = numericSignal === 0 ? '0' : signalNameFromNumber(numericSignal);

          try {
            if (targetPid === VIRTUAL_PID) {
              // Self-signals take the same kernel path as external and
              // kernel-generated signals. The kernel decides ignore/default/
              // caught behavior and retains blocked standard signals.
              callSyncRpc('process.kill', [VIRTUAL_PID, signalName]);
              if (numericSignal !== 0) {
                dispatchPendingWasmSignals();
              }
              return WASI_ERRNO_SUCCESS;
            }

            // The sidecar authorizes direct children from the kernel process
            // table and delivers through the child's runtime endpoint. The
            // runner's childId map is transport correlation only.
            callSyncRpc('process.kill', [targetPid, signalName]);
            return WASI_ERRNO_SUCCESS;
          } catch (error) {
            return mapHostProcessError(error);
          }
        },
        proc_getpid(retPidPtr) {
          if (!guestRangeIsValid(retPidPtr, 4)) return WASI_ERRNO_FAULT;
          return writeGuestUint32(retPidPtr, VIRTUAL_PID);
        },
        proc_getppid(retPidPtr) {
          if (!guestRangeIsValid(retPidPtr, 4)) return WASI_ERRNO_FAULT;
          return writeGuestUint32(retPidPtr, VIRTUAL_PPID);
        },
        proc_getrlimit(resource, retSoftPtr, retHardPtr) {
          if (!guestRangesAreValid([retSoftPtr, 8], [retHardPtr, 8])) {
            return WASI_ERRNO_FAULT;
          }
          const resourceKind = Number(resource) >>> 0;
          if (resourceKind > 9) return WASI_ERRNO_INVAL;
          if (!SIDECAR_MANAGED_PROCESS && resourceKind === 7) {
            const softResult = writeGuestUint64(retSoftPtr, configuredMaxOpenFds);
            if (softResult !== WASI_ERRNO_SUCCESS) return softResult;
            return writeGuestUint64(retHardPtr, configuredMaxOpenFds);
          }
          try {
            const limit = callSyncRpc('process.getrlimit', [resourceKind]);
            const softResult = writeGuestUint64(retSoftPtr, limit.soft);
            if (softResult !== WASI_ERRNO_SUCCESS) {
              return softResult;
            }
            return writeGuestUint64(retHardPtr, limit.hard);
          } catch (error) {
            if (
              error?.code === 'ERR_AGENTOS_WASM_SYNC_RPC_UNAVAILABLE' &&
              resourceKind === 7
            ) {
              const softResult = writeGuestUint64(retSoftPtr, configuredMaxOpenFds);
              if (softResult !== WASI_ERRNO_SUCCESS) return softResult;
              return writeGuestUint64(retHardPtr, configuredMaxOpenFds);
            }
            return mapHostProcessError(error);
          }
        },
        proc_setrlimit(resource, soft, hard) {
          const resourceKind = Number(resource) >>> 0;
          if (resourceKind > 9) return WASI_ERRNO_INVAL;
          const requestedSoft = BigInt.asUintN(64, BigInt(soft));
          const requestedHard = BigInt.asUintN(64, BigInt(hard));
          try {
            callSyncRpc('process.setrlimit', [
              resourceKind,
              requestedSoft.toString(),
              requestedHard.toString(),
            ]);
            warnedAboutOpenFdLimit = false;
            return WASI_ERRNO_SUCCESS;
          } catch (error) {
            return mapHostProcessError(error);
          }
        },
        proc_umask(mask, retPreviousPtr) {
          if (!guestRangeIsValid(retPreviousPtr, 4)) return WASI_ERRNO_FAULT;
          try {
            const previous = Number(
              callSyncRpc('process.umask', [Number(mask) & 0o777]),
            ) >>> 0;
            return writeGuestUint32(retPreviousPtr, previous);
          } catch (error) {
            return mapHostProcessError(error);
          }
        },
        umask(mask, setMask, retPreviousPtr) {
          if (!guestRangeIsValid(retPreviousPtr, 4)) return WASI_ERRNO_FAULT;
          try {
            const args = Number(setMask) !== 0 ? [Number(mask) & 0o777] : [];
            const previous = Number(callSyncRpc('process.umask', args)) >>> 0;
            return writeGuestUint32(retPreviousPtr, previous);
          } catch (error) {
            return mapHostProcessError(error);
          }
        },
        proc_itimer_real(operation, valueUs, intervalUs, retRemainingUsPtr, retIntervalUsPtr) {
          if (!guestRangesAreValid([retRemainingUsPtr, 8], [retIntervalUsPtr, 8])) {
            return WASI_ERRNO_FAULT;
          }
          try {
            const numericOperation = Number(operation) >>> 0;
            if (numericOperation > 1) {
              return WASI_ERRNO_INVAL;
            }
            const value = Number(valueUs);
            const interval = Number(intervalUs);
            if (
              !Number.isSafeInteger(value) ||
              value < 0 ||
              !Number.isSafeInteger(interval) ||
              interval < 0
            ) {
              return WASI_ERRNO_INVAL;
            }
            const result = callSyncRpc('process.itimer_real', [
              numericOperation,
              value,
              interval,
            ]);
            if (
              writeGuestUint64(
                retRemainingUsPtr,
                BigInt(result?.remainingUs ?? 0),
              ) !== WASI_ERRNO_SUCCESS
            ) {
              return WASI_ERRNO_FAULT;
            }
            return writeGuestUint64(
              retIntervalUsPtr,
              BigInt(result?.intervalUs ?? 0),
            );
          } catch (error) {
            return mapHostProcessError(error);
          }
        },
        proc_getpgid(pid, retPgidPtr) {
          if (!guestRangeIsValid(retPgidPtr, 4)) return WASI_ERRNO_FAULT;
          if (permissionTier !== 'full') {
            return WASI_ERRNO_SRCH;
          }
          const requestedPid = Number(pid) | 0;
          if (requestedPid < 0) {
            return WASI_ERRNO_SRCH;
          }
          try {
            const targetPid = requestedPid === 0 ? VIRTUAL_PID : requestedPid;
            const pgid = Number(callSyncRpc('process.getpgid', [targetPid]));
            if (!Number.isSafeInteger(pgid) || pgid <= 0 || pgid > 0xffffffff) {
              return WASI_ERRNO_FAULT;
            }
            return writeGuestUint32(retPgidPtr, pgid);
          } catch (error) {
            return mapHostProcessError(error);
          }
        },
        proc_setpgid(pid, pgid) {
          if (permissionTier !== 'full') {
            return WASI_ERRNO_SRCH;
          }
          const requestedPid = Number(pid) | 0;
          const requestedPgid = Number(pgid) | 0;
          if (requestedPid < 0 || requestedPgid < 0) {
            return WASI_ERRNO_INVAL;
          }
          try {
            const targetPid = requestedPid === 0 ? VIRTUAL_PID : requestedPid;
            callSyncRpc('process.setpgid', [targetPid, requestedPgid]);
            return WASI_ERRNO_SUCCESS;
          } catch (error) {
            return mapHostProcessError(error);
          }
        },
        fd_pipe(retReadFdPtr, retWriteFdPtr) {
          if (!guestRangesAreValid([retReadFdPtr, 4], [retWriteFdPtr, 4])) {
            return WASI_ERRNO_FAULT;
          }
          let readFd = null;
          let writeFd = null;
          try {
            if (!hasRunnerOpenFdCapacity(2)) return WASI_ERRNO_MFILE;
            if (!SIDECAR_MANAGED_PROCESS) {
              const pipe = {
                id: nextSyntheticPipeId++,
                chunks: [],
                consumers: new Map(),
                producers: new Map(),
                readHandleCount: 0,
                writeHandleCount: 0,
              };
              readFd = allocateSyntheticFd(nextSyntheticFd, true);
              writeFd = allocateSyntheticFd(nextSyntheticFd, true);
              if (readFd == null || writeFd == null) return WASI_ERRNO_MFILE;
              setActiveFdProjection(readFd, createPipeHandle('pipe-read', pipe, readFd));
              setActiveFdProjection(writeFd, createPipeHandle('pipe-write', pipe, writeFd));
            } else {
              const result = callSyncRpc('process.fd_pipe');
              readFd = registerKernelDelegateFd(result?.readFd);
              writeFd = registerKernelDelegateFd(result?.writeFd);
            }
            if (writeGuestUint32(retReadFdPtr, readFd) !== WASI_ERRNO_SUCCESS) {
              wasiImport.fd_close(readFd);
              wasiImport.fd_close(writeFd);
              return WASI_ERRNO_FAULT;
            }
            if (writeGuestUint32(retWriteFdPtr, writeFd) !== WASI_ERRNO_SUCCESS) {
              wasiImport.fd_close(readFd);
              wasiImport.fd_close(writeFd);
              return WASI_ERRNO_FAULT;
            }
            return WASI_ERRNO_SUCCESS;
          } catch (error) {
            if (readFd != null) wasiImport.fd_close(readFd);
            if (writeFd != null) wasiImport.fd_close(writeFd);
            return mapHostProcessError(error);
          }
        },
        fd_dup(fd, retNewFdPtr) {
          if (!guestRangeIsValid(retNewFdPtr, 4)) return WASI_ERRNO_FAULT;
          try {
            const sourceFd = Number(fd) >>> 0;
            const hostNetSource = getHostNetSocket(sourceFd);
            if (!SIDECAR_MANAGED_PROCESS && hostNetSource) {
              const duplicatedFd = allocateHostNetDuplicateFd(0);
              if (duplicatedFd == null) return WASI_ERRNO_MFILE;
              standaloneHostNetSockets.set(duplicatedFd, hostNetSource);
              standaloneCloexecFds.delete(duplicatedFd);
              if (writeGuestUint32(retNewFdPtr, duplicatedFd) !== WASI_ERRNO_SUCCESS) {
                hostNetImport.net_close(duplicatedFd);
                return WASI_ERRNO_FAULT;
              }
              return WASI_ERRNO_SUCCESS;
            }
            const kernelSourceFd = managedKernelFdForDuplicate(sourceFd);
            if (kernelSourceFd != null) {
              const duplicatedFd = registerKernelDelegateFd(
                callSyncRpc('process.fd_dup', [kernelSourceFd]),
              );
              copyManagedHostNetDescription(sourceFd, duplicatedFd);
              if (writeGuestUint32(retNewFdPtr, duplicatedFd) !== WASI_ERRNO_SUCCESS) {
                wasiImport.fd_close(duplicatedFd);
                return WASI_ERRNO_FAULT;
              }
              return WASI_ERRNO_SUCCESS;
            }
            const handle = cloneFdHandle(sourceFd);
            if (!handle) {
              return WASI_ERRNO_BADF;
            }
            const duplicatedFd = allocateSyntheticFd(0);
            if (duplicatedFd == null) {
              releaseFdHandle(handle);
              return WASI_ERRNO_MFILE;
            }
            setActiveFdProjection(duplicatedFd, handle);
            traceHostProcess('fd-dup', {
              fd: Number(fd) >>> 0,
              duplicatedFd,
              handleKind: handle.kind,
              targetFd: handle.targetFd ?? null,
              displayFd: handle.displayFd ?? null,
            });
            return writeGuestUint32(retNewFdPtr, duplicatedFd);
          } catch {
            return WASI_ERRNO_FAULT;
          }
        },
        fd_dup2(oldFd, newFd) {
          try {
            const sourceFd = Number(oldFd) >>> 0;
            const targetFd = Number(newFd) >>> 0;
            if (sourceFd >= LINUX_GUEST_FD_LIMIT || targetFd >= LINUX_GUEST_FD_LIMIT) {
              return WASI_ERRNO_BADF;
            }
            if (sourceFd === targetFd) {
              if (!lookupFdHandle(sourceFd) && !hasHostNetSocket(sourceFd)) {
                return WASI_ERRNO_BADF;
              }
              traceHostProcess('fd-dup2-same-fd', {
                oldFd: sourceFd,
                newFd: targetFd,
              });
              return WASI_ERRNO_SUCCESS;
            }
            if (targetFd >= currentNofileSoftLimit()) {
              return WASI_ERRNO_BADF;
            }

            const hostNetSource = getHostNetSocket(sourceFd);
            if (!SIDECAR_MANAGED_PROCESS && hostNetSource) {
              const targetWasOpen = runnerOpenFdSet().has(targetFd);
              if (!targetWasOpen && !hasRunnerOpenFdCapacity(1)) {
                return WASI_ERRNO_MFILE;
              }
              const closeResult = wasiImport.fd_close(targetFd);
              if (closeResult !== WASI_ERRNO_SUCCESS && closeResult !== WASI_ERRNO_BADF) {
                return closeResult;
              }
              standaloneHostNetSockets.set(targetFd, hostNetSource);
              // dup2(2) always clears FD_CLOEXEC on the replacement descriptor.
              standaloneCloexecFds.delete(targetFd);
              closedPassthroughFds.delete(targetFd);
              return WASI_ERRNO_SUCCESS;
            }

            const kernelSourceFd = managedKernelFdForDuplicate(sourceFd);
            if (kernelSourceFd != null) {
              const targetHandle = lookupFdHandle(targetFd);
              const targetIsInternalPreopen = targetHandle?.internalPreopen === true;
              const shadowsInternalPreopen = hiddenPreopenHandles.has(targetFd);
              const targetPassthroughHasAliases =
                targetHandle?.kind === 'passthrough' &&
                Number(targetHandle.refCount) > 0;
              const replacesBootstrapStdio =
                targetFd <= 2 &&
                targetHandle?.kind === 'passthrough' &&
                Number(targetHandle.targetFd) === targetFd &&
                Number(targetHandle.refCount) === 0;
              if (!replacesBootstrapStdio) {
                const closeResult = targetIsInternalPreopen
                  ? (() => {
                      // The node:wasi preopen is hidden runtime
                      // infrastructure, not the Linux descriptor being
                      // replaced. Remove only its guest-visible mirrors so
                      // kernel fd targetFd shadows it; retain fdTable/backing
                      // state for future path resolution.
                      passthroughHandles.delete(targetFd);
                      standaloneDelegateFdRefCounts.delete(targetFd);
                      return WASI_ERRNO_SUCCESS;
                    })()
                  : wasiImport.fd_close(targetFd);
                if (closeResult !== WASI_ERRNO_SUCCESS && closeResult !== WASI_ERRNO_BADF) {
                  return closeResult;
                }
                if (
                  !shadowsInternalPreopen &&
                  !targetPassthroughHasAliases &&
                  wasi?.fdTable?.has?.(targetFd) === true
                ) {
                  // closePassthroughFd already closed the backing preopen fd.
                  // node:wasi retains a stale bookkeeping entry, which must be
                  // removed without attempting to close that resource twice.
                  // A runner-level dup alias keeps the same backing entry live,
                  // so never delete it while such aliases still exist.
                  wasi.fdTable.delete(targetFd);
                }
              }
              // Guest and kernel fd numbers are separate namespaces. Allocate
              // a fresh kernel descriptor, then install it at the exact guest
              // destination; using the raw guest number as a kernel dup2
              // target can alias the source after preopen collisions.
              const duplicatedKernelFd = callSyncRpc('process.fd_dup', [
                kernelSourceFd,
              ]);
              const duplicatedGuestFd = registerKernelDelegateFd(
                duplicatedKernelFd,
                targetFd,
                3,
                shadowsInternalPreopen,
              );
              copyManagedHostNetDescription(sourceFd, duplicatedGuestFd);
              return WASI_ERRNO_SUCCESS;
            }

            const sourceHandle = cloneFdHandle(sourceFd);
            if (!sourceHandle) {
              return WASI_ERRNO_BADF;
            }
            if (!syntheticFdInUse(targetFd) && !hasRunnerOpenFdCapacity(1)) {
              releaseFdHandle(sourceHandle);
              return WASI_ERRNO_MFILE;
            }

            traceHostProcess('fd-dup2-begin', {
              oldFd: sourceFd,
              newFd: targetFd,
              sourceKind: sourceHandle.kind,
              sourceTargetFd: sourceHandle.targetFd ?? null,
              sourceDisplayFd: sourceHandle.displayFd ?? null,
              existingKind: activeFdProjections.get(targetFd)?.kind ?? passthroughHandles.get(targetFd)?.kind ?? null,
            });

            if (hasHostNetSocket(targetFd)) {
              const closeResult = hostNetImport.net_close(targetFd);
              if (closeResult !== WASI_ERRNO_SUCCESS) {
                releaseFdHandle(sourceHandle);
                return closeResult;
              }
            }
            closeSyntheticFd(targetFd);
            closePassthroughFd(targetFd);
            setActiveFdProjection(targetFd, sourceHandle);
            closedPassthroughFds.delete(targetFd);
            traceHostProcess('fd-dup2-installed', {
              oldFd: sourceFd,
              newFd: targetFd,
              sourceKind: sourceHandle.kind,
            });
            return WASI_ERRNO_SUCCESS;
          } catch (error) {
            traceHostProcess('fd-dup2-fault', {
              oldFd: Number(oldFd) >>> 0,
              newFd: Number(newFd) >>> 0,
              code: error?.code ?? null,
              message: error instanceof Error ? error.message : String(error),
            });
            return WASI_ERRNO_FAULT;
          }
        },
        fd_dup_min(fd, minFd, retNewFdPtr) {
          if (!guestRangeIsValid(retNewFdPtr, 4)) return WASI_ERRNO_FAULT;
          try {
            const sourceFd = Number(fd);
            const minimumFdNumber = Number(minFd);
            if (!Number.isInteger(sourceFd) || sourceFd < 0) {
              return WASI_ERRNO_BADF;
            }
            if (!Number.isInteger(minimumFdNumber) || minimumFdNumber < 0) {
              return WASI_ERRNO_INVAL;
            }
            if (minimumFdNumber >= LINUX_GUEST_FD_LIMIT) {
              return WASI_ERRNO_INVAL;
            }
            if (minimumFdNumber >= currentNofileSoftLimit()) {
              return WASI_ERRNO_INVAL;
            }

            const hostNetSource = getHostNetSocket(sourceFd);
            if (!SIDECAR_MANAGED_PROCESS && hostNetSource) {
              const duplicatedFd = allocateHostNetDuplicateFd(minimumFdNumber);
              if (duplicatedFd == null) return WASI_ERRNO_MFILE;
              standaloneHostNetSockets.set(duplicatedFd, hostNetSource);
              standaloneCloexecFds.delete(duplicatedFd);
              if (writeGuestUint32(retNewFdPtr, duplicatedFd) !== WASI_ERRNO_SUCCESS) {
                hostNetImport.net_close(duplicatedFd);
                return WASI_ERRNO_FAULT;
              }
              return WASI_ERRNO_SUCCESS;
            }

            const kernelSourceFd = managedKernelFdForDuplicate(sourceFd);
            if (kernelSourceFd != null) {
              // Kernel and guest allocation share the authoritative
              // RLIMIT_NOFILE. The guest projection still chooses its own
              // lowest visible alias because hidden preopens can occupy a
              // different numeric slot than the backing descriptor.
              const duplicatedFd = registerKernelDelegateFd(
                callSyncRpc('process.fd_dup_min', [kernelSourceFd, minimumFdNumber]),
                null,
                minimumFdNumber,
              );
              copyManagedHostNetDescription(sourceFd, duplicatedFd);
              if (writeGuestUint32(retNewFdPtr, duplicatedFd) !== WASI_ERRNO_SUCCESS) {
                wasiImport.fd_close(duplicatedFd);
                return WASI_ERRNO_FAULT;
              }
              return WASI_ERRNO_SUCCESS;
            }

            const handle = cloneFdHandle(sourceFd);
            if (!handle) {
              return WASI_ERRNO_BADF;
            }

            const duplicatedFd = allocateSyntheticFd(minimumFdNumber);

            if (duplicatedFd == null) {
              releaseFdHandle(handle);
              return WASI_ERRNO_MFILE;
            }

            setActiveFdProjection(duplicatedFd, handle);
            traceHostProcess('fd-dup-min', {
              fd: sourceFd >>> 0,
              minimumFd: minimumFdNumber >>> 0,
              duplicatedFd,
              handleKind: handle.kind,
              targetFd: handle.targetFd ?? null,
              displayFd: handle.displayFd ?? null,
            });
            return writeGuestUint32(retNewFdPtr, duplicatedFd);
          } catch (error) {
            traceHostProcess('fd-dup-min-fault', {
              fd: Number(fd),
              minimumFd: Number(minFd),
              code: error?.code ?? null,
              message: error instanceof Error ? error.message : String(error),
            });
            return mapHostProcessError(error);
          }
        },
        fd_getfd(fd, retFlagsPtr) {
          if (!guestRangeIsValid(retFlagsPtr, 4)) return WASI_ERRNO_FAULT;
          try {
            const numericFd = Number(fd) >>> 0;
            const handle = lookupFdHandle(numericFd);
            let flags;
            if (handle?.kind === 'kernel-fd') {
              flags = Number(callSyncRpc('process.fd_getfd', [Number(handle.targetFd) >>> 0]));
            } else if (handle || hasHostNetSocket(numericFd)) {
              flags = standaloneCloexecFds.has(numericFd) ? 1 : 0;
            } else {
              return WASI_ERRNO_BADF;
            }
            if (!Number.isSafeInteger(flags) || flags < 0 || flags > 0xffffffff) {
              return WASI_ERRNO_IO;
            }
            return writeGuestUint32(retFlagsPtr, flags);
          } catch (error) {
            return mapHostProcessError(error);
          }
        },
        fd_setfd(fd, flags) {
          try {
            const numericFd = Number(fd) >>> 0;
            const normalizedFlags = Number(flags) >>> 0;
            const handle = lookupFdHandle(numericFd);
            if (handle?.kind === 'kernel-fd') {
              callSyncRpc('process.fd_setfd', [
                Number(handle.targetFd) >>> 0,
                normalizedFlags,
              ]);
            } else if (!handle && !hasHostNetSocket(numericFd)) {
              return WASI_ERRNO_BADF;
            } else {
              if ((normalizedFlags & 1) !== 0) {
                standaloneCloexecFds.add(numericFd);
              } else {
                standaloneCloexecFds.delete(numericFd);
              }
            }
            return WASI_ERRNO_SUCCESS;
          } catch (error) {
            return mapHostProcessError(error);
          }
        },
        fd_flock(fd, operation) {
          const LOCK_NB = 4;
          const LOCK_UN = 8;
          try {
            const numericFd = Number(fd) >>> 0;
            const normalizedOperation = Number(operation) >>> 0;
            const handle = lookupFdHandle(numericFd);
            if (handle?.kind !== 'kernel-fd') {
              return handle || hasHostNetSocket(numericFd)
                ? WASI_ERRNO_NOTSUP
                : WASI_ERRNO_BADF;
            }

            const blocking =
              (normalizedOperation & LOCK_NB) === 0 &&
              (normalizedOperation & ~LOCK_NB) !== LOCK_UN;
            const kernelOperation = blocking
              ? normalizedOperation | LOCK_NB
              : normalizedOperation;
            const startedAt = Date.now();
            const deadline = startedAt + unixConnectTimeoutMs;
            let warnedNearLimit = false;
            while (true) {
              try {
                callSyncRpc('process.fd_flock', [
                  Number(handle.targetFd) >>> 0,
                  kernelOperation,
                ]);
                return WASI_ERRNO_SUCCESS;
              } catch (error) {
                if (
                  !blocking ||
                  (error?.code !== 'EAGAIN' && error?.code !== 'EWOULDBLOCK')
                ) {
                  return mapHostProcessError(error);
                }
                const now = Date.now();
                warnedNearLimit = warnNearBlockingReadLimit(
                  'flock',
                  startedAt,
                  warnedNearLimit,
                );
                if (now >= deadline) {
                  process.stderr.write(
                    `[agentos] flock exceeded limits.resources.maxBlockingReadMs (${unixConnectTimeoutMs} ms); raise limits.resources.maxBlockingReadMs if needed\n`,
                  );
                  return WASI_ERRNO_TIMEDOUT;
                }
                if (dispatchPendingWasmSignals(true)) return WASI_ERRNO_INTR;
                // Keep the sidecar dispatcher free so the lock owner can run
                // and unlock while this guest observes blocking flock semantics.
                if (pumpSpawnedChildrenOrWaitRestartable(SPAWNED_CHILD_WAIT_SLICE_MS)) {
                  return WASI_ERRNO_INTR;
                }
                if (dispatchPendingWasmSignals(true)) return WASI_ERRNO_INTR;
              }
            }
          } catch (error) {
            return mapHostProcessError(error);
          }
        },
        fd_record_lock(
          fd,
          command,
          lockType,
          startOffset,
          length,
          retTypePtr,
          retPidPtr,
          retStartPtr,
          retLengthPtr,
        ) {
          const numericCommand = Number(command) >>> 0;
          if (
            numericCommand === 12 &&
            !guestRangesAreValid(
              [retTypePtr, 4],
              [retPidPtr, 4],
              [retStartPtr, 8],
              [retLengthPtr, 8],
            )
          ) {
            return WASI_ERRNO_FAULT;
          }
          let blockingWaitRegistered = false;
          const cancelBlockingLockWait = () => {
            callSyncRpc('process.fd_record_lock_cancel', []);
            blockingWaitRegistered = false;
          };
          try {
            const numericFd = Number(fd) >>> 0;
            const handle = lookupFdHandle(numericFd);
            if (handle?.kind !== 'kernel-fd') {
              // A runner-local or host-network descriptor has no stable VFS
              // inode identity. Never report a lock that the kernel cannot
              // enforce against other VM processes.
              return handle || hasHostNetSocket(numericFd)
                ? WASI_ERRNO_NOTSUP
                : WASI_ERRNO_BADF;
            }
            const normalizedStart = BigInt(startOffset);
            const normalizedLength = BigInt(length);
            if (normalizedStart < 0n || normalizedLength < 0n) {
              return WASI_ERRNO_INVAL;
            }
            const lockWaitStartedAt = Date.now();
            const lockWaitDeadline = lockWaitStartedAt + unixConnectTimeoutMs;
            let warnedNearLockWaitLimit = false;
            let response;
            while (true) {
              try {
                response = callSyncRpc('process.fd_record_lock', [
                  Number(handle.targetFd) >>> 0,
                  numericCommand,
                  Number(lockType) >>> 0,
                  normalizedStart.toString(),
                  normalizedLength.toString(),
                ]);
                blockingWaitRegistered = false;
                break;
              } catch (error) {
                if (
                  numericCommand !== 14 ||
                  (error?.code !== 'EAGAIN' && error?.code !== 'EWOULDBLOCK')
                ) {
                  throw error;
                }
                blockingWaitRegistered = true;
                const now = Date.now();
                warnedNearLockWaitLimit = warnNearBlockingReadLimit(
                  'F_SETLKW',
                  lockWaitStartedAt,
                  warnedNearLockWaitLimit,
                );
                if (now >= lockWaitDeadline) {
                  cancelBlockingLockWait();
                  process.stderr.write(
                    `[agentos] F_SETLKW exceeded limits.resources.maxBlockingReadMs (${unixConnectTimeoutMs} ms); raise limits.resources.maxBlockingReadMs if needed\n`,
                  );
                  return WASI_ERRNO_TIMEDOUT;
                }
                // The sidecar's sync-RPC dispatcher must stay nonblocking so
                // another VM process can reach close/unlock. Suspend in the
                // runner instead, advancing descendants and yielding briefly
                // to independently scheduled processes between retries.
                if (dispatchPendingWasmSignals(true)) {
                  cancelBlockingLockWait();
                  return WASI_ERRNO_INTR;
                }
                if (pumpSpawnedChildrenOrWaitRestartable(SPAWNED_CHILD_WAIT_SLICE_MS)) {
                  cancelBlockingLockWait();
                  return WASI_ERRNO_INTR;
                }
                if (dispatchPendingWasmSignals(true)) {
                  cancelBlockingLockWait();
                  return WASI_ERRNO_INTR;
                }
              }
            }
            if (!response || typeof response !== 'object') {
              return WASI_ERRNO_IO;
            }
            if (numericCommand !== 12) {
              return WASI_ERRNO_SUCCESS;
            }
            for (const errno of [
              writeGuestUint32(retTypePtr, Number(response.type) >>> 0),
              writeGuestUint32(retPidPtr, Number(response.pid) >>> 0),
              writeGuestUint64(retStartPtr, BigInt(response.start)),
              writeGuestUint64(retLengthPtr, BigInt(response.length)),
            ]) {
              if (errno !== WASI_ERRNO_SUCCESS) return errno;
            }
            return WASI_ERRNO_SUCCESS;
          } catch (error) {
            if (blockingWaitRegistered) {
              try {
                cancelBlockingLockWait();
              } catch (cancelError) {
                return mapHostProcessError(cancelError);
              }
            }
            return mapHostProcessError(error);
          }
        },
        proc_closefrom(lowFd) {
          const minimumFd = Number(lowFd) >>> 0;
          const openVirtualFds = new Set([
            ...activeFdProjections.keys(),
            ...passthroughHandles.keys(),
            ...retainedSpawnOutputHandlesByFd.keys(),
            ...retainedSyntheticHandlesByDisplayFd.keys(),
            ...hostNetGuestFds(),
            ...standaloneDelegateFdRefCounts.keys(),
            ...(wasi?.fdTable?.keys?.() ?? []),
          ]);
          if (SIDECAR_MANAGED_PROCESS) {
            const exactKernelFds = new Set();
            for (const fd of openVirtualFds) {
              if (fd < minimumFd) continue;
              const handle = lookupFdHandle(fd);
              if (
                handle?.internalPreopen === true ||
                hiddenPreopenHandles.has(fd)
              ) {
                continue;
              }
              if (handle?.kind === 'kernel-fd') {
                exactKernelFds.add(Number(handle.targetFd) >>> 0);
              } else if (
                fd <= 2 &&
                handle?.kind === 'passthrough' &&
                Number(handle.targetFd) === fd
              ) {
                exactKernelFds.add(fd);
              }
            }
            let response;
            try {
              response = callSyncRpc('process.fd_closefrom', [
                minimumFd,
                [...exactKernelFds],
              ]);
            } catch (error) {
              return mapHostProcessError(error);
            }
            if (!Array.isArray(response?.closedFds)) {
              return WASI_ERRNO_IO;
            }
            const closedKernelFds = new Set(
              response.closedFds.map((fd) => Number(fd) >>> 0),
            );
            for (const fd of closedKernelFds) {
              forgetSidecarClosedKernelTargetFd(fd);
            }

            let firstError = WASI_ERRNO_SUCCESS;
            for (const fd of [...openVirtualFds].sort((left, right) => left - right)) {
              if (fd < minimumFd) continue;
              const handle = lookupFdHandle(fd);
              // Preopens remain private capability roots after closefrom. Only
              // their ordinary, untagged Linux aliases disappear here.
              if (
                handle?.internalPreopen === true ||
                hiddenPreopenHandles.has(fd)
              ) {
                activeFdProjections.delete(fd);
                passthroughHandles.delete(fd);
                closedPassthroughFds.add(fd);
                standaloneCloexecFds.delete(fd);
                continue;
              }
              if (handle?.kind === 'kernel-fd') {
                // Bulk kernel retirement already removed every canonical
                // descriptor in its range; never issue a second per-fd close.
                if (closedKernelFds.has(Number(handle.targetFd) >>> 0)) {
                  forgetSidecarClosedKernelFd(fd);
                }
                continue;
              }
              const result = wasiImport.fd_close(fd);
              if (
                result !== WASI_ERRNO_SUCCESS &&
                result !== WASI_ERRNO_BADF &&
                firstError === WASI_ERRNO_SUCCESS
              ) {
                firstError = result;
              }
            }
            return firstError;
          }
          let firstError = WASI_ERRNO_SUCCESS;
          for (const fd of [...openVirtualFds].sort((left, right) => left - right)) {
            if (fd < minimumFd) {
              continue;
            }
            // WASI preopens are hidden path-resolution capabilities, not
            // Linux process descriptors. Remove the public untagged alias,
            // but retain the private tagged handle used by libc path lookup.
            if (
              passthroughHandles.get(fd)?.internalPreopen === true ||
              hiddenPreopenHandles.has(fd)
            ) {
              passthroughHandles.delete(fd);
              closedPassthroughFds.add(fd);
              standaloneCloexecFds.delete(fd);
              continue;
            }
            const result = wasiImport.fd_close(fd);
            if (
              result !== WASI_ERRNO_SUCCESS &&
              result !== WASI_ERRNO_BADF &&
              firstError === WASI_ERRNO_SUCCESS
            ) {
              firstError = result;
            }
          }
          return firstError;
        },
        fd_socketpair(socketKind, nonblocking, closeOnExec, retFirstPtr, retSecondPtr) {
          let firstFd = null;
          let secondFd = null;
          try {
            if (!guestRangeIsValid(retFirstPtr, 4) || !guestRangeIsValid(retSecondPtr, 4)) {
              return WASI_ERRNO_FAULT;
            }
            if (!hasRunnerOpenFdCapacity(2)) return WASI_ERRNO_MFILE;
            const result = callSyncRpc('process.fd_socketpair', [
              Number(socketKind) >>> 0,
              Number(nonblocking) !== 0,
              Number(closeOnExec) !== 0,
            ]);
            firstFd = registerKernelDelegateFd(result?.firstFd);
            secondFd = registerKernelDelegateFd(result?.secondFd);
            const firstWrite = writeGuestUint32(retFirstPtr, firstFd);
            const secondWrite = writeGuestUint32(retSecondPtr, secondFd);
            if (firstWrite !== WASI_ERRNO_SUCCESS || secondWrite !== WASI_ERRNO_SUCCESS) {
              wasiImport.fd_close(firstFd);
              wasiImport.fd_close(secondFd);
              return WASI_ERRNO_FAULT;
            }
            return WASI_ERRNO_SUCCESS;
          } catch (error) {
            if (firstFd != null) wasiImport.fd_close(firstFd);
            if (secondFd != null) wasiImport.fd_close(secondFd);
            return mapHostProcessError(error);
          }
        },
        fd_sendmsg_rights(socketFd, dataPtr, dataLen, rightsPtr, rightsLen, flags, retSentPtr) {
          if (!guestRangeIsValid(retSentPtr, 4)) return WASI_ERRNO_FAULT;
          try {
            if (!(instanceMemory instanceof WebAssembly.Memory)) return WASI_ERRNO_FAULT;
            const byteLength = Number(dataLen) >>> 0;
            const rightsLength = Number(rightsLen) >>> 0;
            const memoryLength = instanceMemory.buffer.byteLength;
            const dataOffset = Number(dataPtr) >>> 0;
            const rightsOffset = Number(rightsPtr) >>> 0;
            if (dataOffset > memoryLength || byteLength > memoryLength - dataOffset) {
              return WASI_ERRNO_FAULT;
            }
            if (
              rightsLength > 253 ||
              rightsOffset > memoryLength ||
              rightsLength * 4 > memoryLength - rightsOffset
            ) {
              return rightsLength > 253 ? WASI_ERRNO_INVAL : WASI_ERRNO_FAULT;
            }
            const bytes = Buffer.from(
              new Uint8Array(instanceMemory.buffer, dataOffset, byteLength),
            );
            const view = new DataView(instanceMemory.buffer);
            const rights = [];
            for (let index = 0; index < rightsLength; index += 1) {
              const guestFd = view.getUint32(rightsOffset + index * 4, true);
              if (hasHostNetSocket(guestFd)) {
                const handle = lookupFdHandle(guestFd);
                rights.push({
                  kind: 'hostNet',
                  fd: handle?.kind === 'kernel-fd'
                    ? Number(handle.targetFd) >>> 0
                    : null,
                  descriptionId: handle?.hostNetDescriptionId ?? null,
                });
                continue;
              }
              const handle = lookupFdHandle(guestFd);
              const kernelFd = handle?.kind === 'kernel-fd'
                ? Number(handle.targetFd) >>> 0
                : standaloneDelegateFdRefCounts.has(guestFd)
                  ? guestFd
                  : null;
              if (kernelFd == null) {
                const error = new Error(`bad transferable file descriptor ${guestFd}`);
                error.code = 'EBADF';
                throw error;
              }
              rights.push(kernelFd);
            }
            const sent = Number(callSyncRpc('process.fd_sendmsg_rights', [
              canonicalKernelFdForSpawnAction(socketFd),
              bytes,
              rights,
              Number(flags) >>> 0,
            ]));
            traceHostProcess('fd-sendmsg-rights', {
              guestSocketFd: Number(socketFd) >>> 0,
              kernelSocketFd: canonicalKernelFdForSpawnAction(socketFd),
              rights,
              bytes: bytes.length,
              sent,
            });
            if (!Number.isSafeInteger(sent) || sent < 0 || sent > byteLength) {
              return WASI_ERRNO_FAULT;
            }
            const writeResult = writeGuestUint32(retSentPtr, sent);
            return writeResult === WASI_ERRNO_SUCCESS ? WASI_ERRNO_SUCCESS : WASI_ERRNO_FAULT;
          } catch (error) {
            return mapHostProcessError(error);
          }
        },
        fd_recvmsg_rights(
          socketFd,
          dataPtr,
          dataLen,
          rightsPtr,
          rightsCapacity,
          flags,
          retReceivedPtr,
          retRightsLenPtr,
          retMessageFlagsPtr,
        ) {
          const installedFds = [];
          try {
            if (!(instanceMemory instanceof WebAssembly.Memory)) return WASI_ERRNO_FAULT;
            const byteLength = Number(dataLen) >>> 0;
            const rightsLength = Number(rightsCapacity) >>> 0;
            const memoryLength = instanceMemory.buffer.byteLength;
            const dataOffset = Number(dataPtr) >>> 0;
            const rightsOffset = Number(rightsPtr) >>> 0;
            if (dataOffset > memoryLength || byteLength > memoryLength - dataOffset) {
              return WASI_ERRNO_FAULT;
            }
            if (
              rightsLength > 253 ||
              rightsOffset > memoryLength ||
              rightsLength * 4 > memoryLength - rightsOffset
            ) {
              return rightsLength > 253 ? WASI_ERRNO_INVAL : WASI_ERRNO_FAULT;
            }
            const numericFlags = Number(flags) >>> 0;
            const kernelSocketFd = canonicalKernelFdForSpawnAction(socketFd);
            const socketStat = callSyncRpc('process.fd_stat', [kernelSocketFd]);
            const dontwait =
              (numericFlags & 0x0040) !== 0 ||
              (Number(socketStat?.flags) & KERNEL_O_NONBLOCK) !== 0;
            const requestArgs = [
              kernelSocketFd,
              byteLength,
              rightsLength,
              (numericFlags & 0x40000000) !== 0,
              (numericFlags & 0x0002) !== 0,
              // A blocking sidecar recv monopolizes the dispatch loop and can
              // deadlock against an inherited child whose sendmsg is waiting
              // for that same loop. Poll the kernel non-blocking and pump child
              // events between attempts. This preserves Linux blocking
              // behavior while allowing the child send transaction to run.
              true,
              (numericFlags & 0x0100) !== 0,
            ];
            const deadline = dontwait
              ? Date.now()
              : maxBlockingReadMs == null
                ? null
                : Date.now() + maxBlockingReadMs;
            let result;
            while (result == null) {
              try {
                result = callSyncRpc('process.fd_recvmsg_rights', requestArgs);
              } catch (error) {
                if (error?.code !== 'EAGAIN' && error?.code !== 'EWOULDBLOCK') {
                  throw error;
                }
                if (dontwait) {
                  throw error;
                }
                if (deadline != null && Date.now() >= deadline) {
                  const timeout = new Error(
                    'blocking socket receive timed out; raise limits.resources.maxBlockingReadMs',
                  );
                  timeout.code = 'EAGAIN';
                  throw timeout;
                }
                const progressed = pumpSpawnedChildren(10);
                dispatchPendingWasmSignals();
                if (!progressed && childCorrelationsByPid.size === 0) {
                  Atomics.wait(syntheticWaitArray, 0, 0, 1);
                }
              }
            }
            traceHostProcess('fd-recvmsg-rights', {
              guestSocketFd: Number(socketFd) >>> 0,
              kernelSocketFd,
              result,
            });
            const bytes = Buffer.from(result?.data ?? []);
            const receivedRights = Array.isArray(result?.rights) ? result.rights : [];
            if (bytes.length > byteLength || receivedRights.length > rightsLength) {
              return WASI_ERRNO_FAULT;
            }
            const memory = new Uint8Array(instanceMemory.buffer);
            const view = new DataView(instanceMemory.buffer);
            memory.set(bytes, dataOffset);
            let installedCount = 0;
            let localControlTruncated = result?.controlTruncated === true;
            for (const received of receivedRights) {
              let fd;
              if (received?.kind === 'kernel') {
                fd = registerKernelDelegateFd(received.fd);
              } else if (received?.kind === 'hostNet') {
                if (SIDECAR_MANAGED_PROCESS) {
                  const descriptionId = String(received.descriptionId ?? '');
                  try {
                    fd = registerKernelDelegateFd(received.fd);
                    attachManagedHostNetDescription(fd, descriptionId);
                  } catch (error) {
                    try {
                      callSyncRpc('process.fd_close', [Number(received.fd) >>> 0]);
                    } catch (closeError) {
                      process.stderr.write(
                        `[agentos] failed to roll back received managed fd: ${closeError instanceof Error ? closeError.message : String(closeError)}\n`,
                      );
                    }
                    throw error;
                  }
                } else {
                  const socket = {
                    domain: Number(received.domain) >>> 0,
                    sockType: Number(received.socketType) >>> 0,
                    protocol: Number(received.protocol) >>> 0,
                    bindOptions: received.bindOptions ?? null,
                    localInfo: received.localInfo ?? null,
                    localUnixAddress: received.localUnixAddress ?? null,
                    localReservation: received.localReservation ?? null,
                    remoteInfo: received.remoteInfo ?? null,
                    remoteUnixAddress: received.remoteUnixAddress ?? null,
                    listening: received.listening === true,
                    serverId: received.serverId ?? null,
                    socketId: received.socketId ?? null,
                    udpSocketId: received.udpSocketId ?? null,
                    pendingDatagram: null,
                    recvTimeoutMs: received.recvTimeoutMs ?? null,
                    readChunks: [],
                    pendingAccepts: [],
                    readableEnded: false,
                    closed: false,
                    lastError: null,
                    nonblock: received.nonblocking === true,
                  };
                  fd = allocateHostNetSocketFd();
                  if (fd != null) standaloneHostNetSockets.set(fd, socket);
                }
                if (fd == null) {
                  localControlTruncated = true;
                  if (typeof received.socketId === 'string') {
                    callSyncRpc('net.destroy', [received.socketId]);
                  }
                  if (typeof received.serverId === 'string') {
                    callSyncRpc('net.server_close', [received.serverId]);
                  }
                  if (typeof received.udpSocketId === 'string') {
                    callSyncRpc('dgram.close', [received.udpSocketId]);
                  }
                  if (typeof received.localReservation === 'string') {
                    callSyncRpc('net.release_tcp_port', [received.localReservation]);
                  }
                  continue;
                }
              } else {
                return WASI_ERRNO_FAULT;
              }
              if ((numericFlags & 0x40000000) !== 0) {
                if (!SIDECAR_MANAGED_PROCESS) standaloneCloexecFds.add(fd);
              }
              installedFds.push(fd);
              view.setUint32(rightsOffset + installedCount * 4, fd, true);
              installedCount += 1;
            }
            const internalFlags = (result?.payloadTruncated === true ? 1 : 0)
              | (localControlTruncated ? 2 : 0)
              | ((Number(result?.fullLength) >>> 0) << 2);
            for (const [ptr, value] of [
              [retReceivedPtr, bytes.length],
              [retRightsLenPtr, installedCount],
              [retMessageFlagsPtr, internalFlags],
            ]) {
              if (writeGuestUint32(ptr, value) !== WASI_ERRNO_SUCCESS) {
                for (const fd of installedFds) wasiImport.fd_close(fd);
                return WASI_ERRNO_FAULT;
              }
            }
            return WASI_ERRNO_SUCCESS;
          } catch (error) {
            for (const fd of installedFds) wasiImport.fd_close(fd);
            return mapHostProcessError(error);
          }
        },
        sleep_ms(milliseconds) {
          const durationMs = Number(milliseconds) >>> 0;
          if (!SIDECAR_MANAGED_PROCESS) {
            Atomics.wait(syntheticWaitArray, 0, 0, durationMs);
            return WASI_ERRNO_SUCCESS;
          }
          try {
            callSyncRpc('process.sleep', [durationMs]);
            dispatchPendingWasmSignals();
            return WASI_ERRNO_SUCCESS;
          } catch (error) {
            dispatchPendingWasmSignals();
            return mapHostProcessError(error);
          }
        },
        pty_open(retMasterFdPtr, retSlaveFdPtr) {
          let masterFd = null;
          let slaveFd = null;
          try {
            if (!guestRangeIsValid(retMasterFdPtr, 4) || !guestRangeIsValid(retSlaveFdPtr, 4)) {
              return WASI_ERRNO_FAULT;
            }
            if (!hasRunnerOpenFdCapacity(2)) return WASI_ERRNO_MFILE;
            const result = callSyncRpc('process.pty_open', []);
            masterFd = registerKernelDelegateFd(result?.masterFd);
            slaveFd = registerKernelDelegateFd(result?.slaveFd);
            writeGuestUint32(retMasterFdPtr, masterFd);
            writeGuestUint32(retSlaveFdPtr, slaveFd);
            return WASI_ERRNO_SUCCESS;
          } catch (error) {
            if (masterFd != null) wasiImport.fd_close(masterFd);
            if (slaveFd != null) wasiImport.fd_close(slaveFd);
            return mapHostProcessError(error);
          }
        },
        proc_sigaction(signal, action, maskLo, maskHi, flags) {
          if (permissionTier !== 'full') {
            return WASI_ERRNO_FAULT;
          }
          const numericSignal = Number(signal) >>> 0;
          if (numericSignal === 0 || numericSignal > 64) {
            return WASI_ERRNO_INVAL;
          }
          try {
            const registration = {
              action: action === 0 ? 'default' : action === 1 ? 'ignore' : 'user',
              mask: decodeSignalMask(maskLo, maskHi),
              flags: Number(flags) >>> 0,
            };
            if (
              registration.action === 'user' &&
              typeof instance?.exports?.__wasi_signal_trampoline !== 'function'
            ) {
              // Accepting this registration would let the kernel publish a
              // caught delivery that this image can never run. Reject it at
              // registration time instead of silently consuming the token.
              return WASI_ERRNO_NOTSUP;
            }
            callSyncRpc('process.signal_state', [
              numericSignal,
              registration.action,
              JSON.stringify(registration.mask),
              registration.flags,
            ]);
            traceHostProcess('proc-sigaction', {
              signal: numericSignal,
              action: registration.action,
              mask: registration.mask,
              flags: registration.flags,
            });
            return WASI_ERRNO_SUCCESS;
          } catch {
            return WASI_ERRNO_FAULT;
          }
        },
        proc_signal_mask_v2(how, setLo, setHi, retOldLoPtr, retOldHiPtr) {
          if (!guestRangesAreValid([retOldLoPtr, 4], [retOldHiPtr, 4])) {
            return WASI_ERRNO_FAULT;
          }
          if (permissionTier !== 'full') {
            return WASI_ERRNO_FAULT;
          }
          try {
            const operation = Number(how) >>> 0;
            if (operation > 3) {
              return WASI_ERRNO_INVAL;
            }
            const requested = decodeSignalMask(setLo, setHi).filter(
              (signal) => signal !== LINUX_SIGKILL && signal !== LINUX_SIGSTOP,
            );
            const response = callSyncRpc('process.signal_mask', [operation, requested]);
            const previous = encodeSignalMask(response?.signals ?? []);
            writeGuestUint32(retOldLoPtr, previous.lo);
            writeGuestUint32(retOldHiPtr, previous.hi);
            dispatchPendingWasmSignals();
            return WASI_ERRNO_SUCCESS;
          } catch {
            return WASI_ERRNO_FAULT;
          }
        },
        proc_ppoll_v1(
          fdsPtr,
          nfds,
          timeoutSec,
          timeoutNsec,
          sigmaskLo,
          sigmaskHi,
          hasSigmask,
          retReadyPtr,
        ) {
          if (!guestRangeIsValid(retReadyPtr, 4)) return WASI_ERRNO_FAULT;
          if (permissionTier !== 'full') {
            return WASI_ERRNO_PERM;
          }
          try {
            const seconds = BigInt(timeoutSec);
            const nanoseconds = BigInt(timeoutNsec);
            let timeoutMs = -1;
            if (seconds >= 0n || nanoseconds >= 0n) {
              if (seconds < 0n || nanoseconds < 0n || nanoseconds >= 1_000_000_000n) {
                return WASI_ERRNO_INVAL;
              }
              const milliseconds = seconds * 1000n + (nanoseconds + 999_999n) / 1_000_000n;
              timeoutMs = Number(milliseconds > 2_147_483_647n ? 2_147_483_647n : milliseconds);
            }
            const temporaryMask = (Number(hasSigmask) >>> 0) !== 0
              ? decodeSignalMask(sigmaskLo, sigmaskHi).filter(
                (signal) => signal !== LINUX_SIGKILL && signal !== LINUX_SIGSTOP,
              )
              : null;
            return hostNetImport.net_poll(
              fdsPtr,
              nfds,
              timeoutMs,
              retReadyPtr,
              temporaryMask,
            );
          } catch {
            return WASI_ERRNO_FAULT;
          }
        },
};

const limitedHostProcessImport = {
  fd_dup_min: hostProcessImport.fd_dup_min,
  fd_flock: hostProcessImport.fd_flock,
  fd_getfd: hostProcessImport.fd_getfd,
  fd_setfd: hostProcessImport.fd_setfd,
  fd_record_lock: hostProcessImport.fd_record_lock,
  proc_getrlimit: hostProcessImport.proc_getrlimit,
  proc_setrlimit: hostProcessImport.proc_setrlimit,
  proc_umask: hostProcessImport.proc_umask,
  umask: hostProcessImport.umask,
};

function hostUserLookup(lookup, bufPtr, bufLen, retLenPtr) {
  try {
    const capacity = Number(bufLen) >>> 0;
    if (
      !guestRangeIsValid(retLenPtr, 4) ||
      !guestRangeIsValid(bufPtr, capacity)
    ) {
      return WASI_ERRNO_FAULT;
    }
    const entry = String(lookup());
    return writeGuestAccountRecord(bufPtr, capacity, encodeGuestBytes(entry), retLenPtr);
  } catch (error) {
    return mapSyntheticFsError(error);
  }
}

function hostUserNameLookup(lookup, namePtr, nameLen, bufPtr, bufLen, retLenPtr) {
  try {
    const nameErrno = checkFixedRequestLimit(
      'wasm.abi.maxAccountNameBytes',
      nameLen,
      MAX_ACCOUNT_RECORD_BYTES,
      WASI_ERRNO_NAMETOOLONG,
    );
    if (nameErrno !== WASI_ERRNO_SUCCESS) return nameErrno;
    return hostUserLookup(
      () => lookup(readGuestString(namePtr, nameLen)),
      bufPtr,
      bufLen,
      retLenPtr,
    );
  } catch (error) {
    return mapSyntheticFsError(error);
  }
}

function hostUserOptionalId(value) {
  const id = Number(value) >>> 0;
  return id === 0xffffffff ? null : id;
}

const hostUserImport = {
  getuid(retUidPtr) {
    try {
      if (!guestRangeIsValid(retUidPtr, 4)) return WASI_ERRNO_FAULT;
      return writeGuestUint32(retUidPtr, callSyncRpc('process.getuid', []));
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  },
  getgid(retGidPtr) {
    try {
      if (!guestRangeIsValid(retGidPtr, 4)) return WASI_ERRNO_FAULT;
      return writeGuestUint32(retGidPtr, callSyncRpc('process.getgid', []));
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  },
  geteuid(retUidPtr) {
    try {
      if (!guestRangeIsValid(retUidPtr, 4)) return WASI_ERRNO_FAULT;
      return writeGuestUint32(retUidPtr, callSyncRpc('process.geteuid', []));
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  },
  getegid(retGidPtr) {
    try {
      if (!guestRangeIsValid(retGidPtr, 4)) return WASI_ERRNO_FAULT;
      return writeGuestUint32(retGidPtr, callSyncRpc('process.getegid', []));
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  },
  getresuid(retUidPtr, retEuidPtr, retSuidPtr) {
    try {
      if (
        !guestRangeIsValid(retUidPtr, 4) ||
        !guestRangeIsValid(retEuidPtr, 4) ||
        !guestRangeIsValid(retSuidPtr, 4)
      ) {
        return WASI_ERRNO_FAULT;
      }
      const [uid, euid, suid] = callSyncRpc('process.getresuid', []);
      return writeGuestUint32(retUidPtr, uid) ||
        writeGuestUint32(retEuidPtr, euid) ||
        writeGuestUint32(retSuidPtr, suid);
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  },
  getresgid(retGidPtr, retEgidPtr, retSgidPtr) {
    try {
      if (
        !guestRangeIsValid(retGidPtr, 4) ||
        !guestRangeIsValid(retEgidPtr, 4) ||
        !guestRangeIsValid(retSgidPtr, 4)
      ) {
        return WASI_ERRNO_FAULT;
      }
      const [gid, egid, sgid] = callSyncRpc('process.getresgid', []);
      return writeGuestUint32(retGidPtr, gid) ||
        writeGuestUint32(retEgidPtr, egid) ||
        writeGuestUint32(retSgidPtr, sgid);
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  },
  setuid(uid) {
    try {
      callSyncRpc('process.setuid', [Number(uid) >>> 0]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  },
  seteuid(uid) {
    try {
      callSyncRpc('process.seteuid', [Number(uid) >>> 0]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  },
  setreuid(uid, euid) {
    try {
      callSyncRpc('process.setreuid', [hostUserOptionalId(uid), hostUserOptionalId(euid)]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  },
  setresuid(uid, euid, suid) {
    try {
      callSyncRpc('process.setresuid', [
        hostUserOptionalId(uid),
        hostUserOptionalId(euid),
        hostUserOptionalId(suid),
      ]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  },
  setgid(gid) {
    try {
      callSyncRpc('process.setgid', [Number(gid) >>> 0]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  },
  setegid(gid) {
    try {
      callSyncRpc('process.setegid', [Number(gid) >>> 0]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  },
  setregid(gid, egid) {
    try {
      callSyncRpc('process.setregid', [hostUserOptionalId(gid), hostUserOptionalId(egid)]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  },
  setresgid(gid, egid, sgid) {
    try {
      callSyncRpc('process.setresgid', [
        hostUserOptionalId(gid),
        hostUserOptionalId(egid),
        hostUserOptionalId(sgid),
      ]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  },
  getgroups(size, groupsPtr, retCountPtr) {
    try {
      if (!guestRangeIsValid(retCountPtr, 4)) {
        return WASI_ERRNO_FAULT;
      }
      const capacity = Number(size) >>> 0;
      if (
        capacity !== 0 &&
        !guestRangeIsValid(groupsPtr, Math.min(capacity, MAX_SUPPLEMENTARY_GROUPS) * 4)
      ) {
        return WASI_ERRNO_FAULT;
      }
      const groups = callSyncRpc('process.getgroups', []);
      if (!Array.isArray(groups) || groups.length > MAX_SUPPLEMENTARY_GROUPS) {
        return WASI_ERRNO_INVAL;
      }
      if (capacity !== 0 && capacity < groups.length) {
        return WASI_ERRNO_INVAL;
      }
      if (capacity !== 0 && !guestRangeIsValid(groupsPtr, groups.length * 4)) {
        return WASI_ERRNO_FAULT;
      }
      if (capacity !== 0) {
        for (let index = 0; index < groups.length; index += 1) {
          const errno = writeGuestUint32(Number(groupsPtr) + index * 4, groups[index]);
          if (errno !== WASI_ERRNO_SUCCESS) return errno;
        }
      }
      return writeGuestUint32(retCountPtr, groups.length);
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  },
  setgroups(count, groupsPtr) {
    try {
      const groupCount = Number(count) >>> 0;
      const countErrno = checkFixedRequestLimit(
        'wasm.abi.maxSupplementaryGroups',
        groupCount,
        MAX_SUPPLEMENTARY_GROUPS,
      );
      if (countErrno !== WASI_ERRNO_SUCCESS) return countErrno;
      if (!guestRangeIsValid(groupsPtr, groupCount * 4)) {
        return WASI_ERRNO_FAULT;
      }
      const groups = [];
      for (let index = 0; index < groupCount; index += 1) {
        groups.push(readGuestUint32(Number(groupsPtr) + index * 4));
      }
      callSyncRpc('process.setgroups', [groups]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  },
  isatty(fd, retBoolPtr) {
    const descriptor = Number(fd) >>> 0;
    const isTerminal = stdioFdIsKernelTty(descriptor) ? 1 : 0;
    return writeGuestUint32(retBoolPtr, isTerminal);
  },
  getpwuid(uid, bufPtr, bufLen, retLenPtr) {
    return hostUserLookup(
      () => callSyncRpc('process.getpwuid', [Number(uid) >>> 0]),
      bufPtr,
      bufLen,
      retLenPtr,
    );
  },
  getpwnam(namePtr, nameLen, bufPtr, bufLen, retLenPtr) {
    return hostUserNameLookup(
      (name) => callSyncRpc('process.getpwnam', [name]),
      namePtr,
      nameLen,
      bufPtr,
      bufLen,
      retLenPtr,
    );
  },
  getpwent(index, bufPtr, bufLen, retLenPtr) {
    return hostUserLookup(
      () => callSyncRpc('process.getpwent', [Number(index) >>> 0]),
      bufPtr,
      bufLen,
      retLenPtr,
    );
  },
  getgrgid(gid, bufPtr, bufLen, retLenPtr) {
    return hostUserLookup(
      () => callSyncRpc('process.getgrgid', [Number(gid) >>> 0]),
      bufPtr,
      bufLen,
      retLenPtr,
    );
  },
  getgrnam(namePtr, nameLen, bufPtr, bufLen, retLenPtr) {
    return hostUserNameLookup(
      (name) => callSyncRpc('process.getgrnam', [name]),
      namePtr,
      nameLen,
      bufPtr,
      bufLen,
      retLenPtr,
    );
  },
  getgrent(index, bufPtr, bufLen, retLenPtr) {
    return hostUserLookup(
      () => callSyncRpc('process.getgrent', [Number(index) >>> 0]),
      bufPtr,
      bufLen,
      retLenPtr,
    );
  },
};

const HOST_FS_MODE_REGULAR = 0o100644;
const HOST_FS_MODE_CHARACTER = 0o020666;
const HOST_FS_MODE_FIFO = 0o010600;
const HOST_FS_GUEST_CWD =
  typeof guestEnv?.PWD === 'string' && guestEnv.PWD.startsWith('/')
    ? path.posix.normalize(guestEnv.PWD)
    : '/';

for (const preopen of WASI_PREOPEN_ENTRIES) {
  const { fd, kernelFd, guestPath, preopenSpec, kernelManaged } = preopen;
  const preopenHandle = {
    kind: kernelManaged ? 'kernel-fd' : 'passthrough',
    targetFd: kernelFd,
    displayFd: fd,
    refCount: 0,
    open: true,
    guestPath: guestPathForPreopenKey(guestPath),
    readOnly: preopenSpec?.readOnly === true,
    internalPreopen: true,
    rightsBase: BigInt(preopenSpec?.rightsBase ?? 0),
    rightsInheriting: BigInt(preopenSpec?.rightsInheriting ?? 0),
  };
  // Patched libc reaches capability roots through the tagged alias. Managed
  // execution points that alias at the kernel fd; standalone execution keeps
  // node:wasi's private descriptor behind the same bookkeeping layer.
  hiddenPreopenHandles.set(fd, preopenHandle);
  if (initialClosedGuestFds.has(fd)) {
    // Keep the private tagged capability for libc path resolution, but make
    // the same untagged Linux descriptor observably closed (including
    // fd_prestat_get) after spawn close/closefrom actions.
    closedPassthroughFds.add(fd);
  } else if (
    !initialMappedGuestFds.has(fd) &&
    !passthroughHandles.has(fd)
  ) {
    if (!kernelManaged) retainDelegateFd(fd);
    closedPassthroughFds.delete(fd);
    passthroughHandles.set(fd, preopenHandle);
  }
}

if (SIDECAR_MANAGED_PROCESS) {
  const inheritedEntries = callSyncRpc('process.fd_snapshot', []);
  if (!Array.isArray(inheritedEntries) || inheritedEntries.length > configuredMaxOpenFds) {
    throw new Error(
      `kernel descriptor snapshot exceeds the ${configuredMaxOpenFds}-descriptor runtime limit`,
    );
  }
  inheritedEntries.sort((left, right) => Number(left?.fd) - Number(right?.fd));
  const projectedPreopenKernelFds = new Set(
    WASI_PREOPEN_ENTRIES
      .filter((entry) => entry.kernelManaged)
      .map((entry) => Number(entry.kernelFd) >>> 0),
  );
  for (const entry of inheritedEntries) {
    const kernelFd = Number(entry?.fd);
    if (!Number.isSafeInteger(kernelFd) || kernelFd < 0 || kernelFd > 0xffffffff) {
      throw new Error(`kernel descriptor snapshot contains invalid fd ${String(entry?.fd)}`);
    }
    // The WASI adapter already projected capability roots into the stable
    // guest preopen range beginning at fd 3. Kernel allocation order is an
    // implementation detail and must not create a second Linux-visible alias.
    if (projectedPreopenKernelFds.has(kernelFd >>> 0)) {
      continue;
    }
    const mappedGuestFd = initialKernelFdMappings.get(kernelFd);
    // The kernel always has canonical stdio entries. Leave an unmapped entry
    // on Node's bootstrap handle, but do not discard a POSIX-spawn dup2 that
    // deliberately installed a pipe at kernel fd 0/1/2. The explicit inverse
    // mapping is what distinguishes inherited transport from default stdio.
    if (kernelFd <= 2 && mappedGuestFd == null) {
      continue;
    }
    if (mappedGuestFd != null) {
      pendingInitialKernelGuestFds.delete(mappedGuestFd);
    }
    const existingHandle = lookupFdHandle(kernelFd);
    if (
      existingHandle?.kind === 'kernel-fd' &&
      Number(existingHandle.targetFd) === kernelFd &&
      mappedGuestFd == null
    ) {
      const descriptionId = String(entry?.descriptionId ?? '');
      if (initialManagedHostNetDescriptionIds.has(descriptionId)) {
        existingHandle.hostNetDescriptionId = descriptionId;
      }
      continue;
    }
    const guestFd = registerKernelDelegateFd(
      kernelFd,
      mappedGuestFd ?? null,
    );
    const descriptionId = String(entry?.descriptionId ?? '');
    if (initialManagedHostNetDescriptionIds.has(descriptionId)) {
      attachManagedHostNetDescription(guestFd, descriptionId);
    }
  }
}

function resolveHostFsPath(value, fromGuestDir = HOST_FS_GUEST_CWD) {
  return resolveHostFsMapping(value, fromGuestDir)?.hostPath ?? null;
}

function resolveHostFsMapping(value, fromGuestDir = HOST_FS_GUEST_CWD) {
  const guestPath = resolveSyntheticGuestPath(value, fromGuestDir);
  if (typeof guestPath !== 'string') {
    return null;
  }
  return resolveModuleGuestPathToHostMapping(guestPath);
}

let pendingOpenCreateMode = null;
let pendingOpenDirect = false;

function mutateGuestFileRange(fd, offset, length, method, extraArgs = []) {
  try {
    const handle = lookupFdHandle(Number(fd) >>> 0);
    if (handle?.readOnly === true || isWorkspaceReadOnly()) {
      return WASI_ERRNO_ROFS;
    }
    if (handle?.kind !== 'guest-file' && handle?.kind !== 'kernel-fd' || typeof handle.targetFd !== 'number') {
      return WASI_ERRNO_BADF;
    }
    const rangeOffset = Number(offset);
    const rangeLength = Number(length);
    if (
      !Number.isSafeInteger(rangeOffset) ||
      !Number.isSafeInteger(rangeLength) ||
      rangeOffset < 0 ||
      rangeLength < 0
    ) {
      return WASI_ERRNO_INVAL;
    }
    const args = [
      Number(handle.targetFd) >>> 0,
      rangeOffset,
      rangeLength,
      ...extraArgs,
    ];
    switch (method) {
      case 'fs.fallocateSync':
        callSyncRpc('fs.fallocateSync', args);
        break;
      case 'fs.zeroRangeSync':
        callSyncRpc('fs.zeroRangeSync', args);
        break;
      case 'fs.insertRangeSync':
        callSyncRpc('fs.insertRangeSync', args);
        break;
      case 'fs.collapseRangeSync':
        callSyncRpc('fs.collapseRangeSync', args);
        break;
      default:
        throw Object.assign(new Error(`unsupported file-range method ${method}`), {
          code: 'EINVAL',
        });
    }
    return WASI_ERRNO_SUCCESS;
  } catch (error) {
    traceHostProcess('guest-file-range-error', {
      method,
      code: error?.code,
      message: error?.message,
    });
    return mapSyntheticFsError(error);
  }
}


const hostFsImport = {
  open_tmpfile(dirFd, pathPtr, pathLen, flags, mode, retFdPtr) {
    try {
      if (!guestRangeIsValid(retFdPtr, 4)) return WASI_ERRNO_FAULT;
      const directory = resolvePathOpenGuestPath(dirFd, pathPtr, pathLen);
      if (typeof directory !== 'string') return WASI_ERRNO_BADF;
      if (isWorkspaceReadOnly() || guestPathIsReadOnly(directory)) return WASI_ERRNO_ROFS;

      const normalizedFlags = Number(flags) >>> 0;
      const wantsRead = (normalizedFlags & WASI_LIBC_O_RDONLY) !== 0;
      const wantsWrite = (normalizedFlags & WASI_LIBC_O_WRONLY) !== 0;
      let kernelFlags = wantsRead && wantsWrite
        ? KERNEL_O_RDWR
        : wantsWrite
          ? KERNEL_O_WRONLY
          : 0;
      if ((normalizedFlags & WASI_FDFLAGS_APPEND) !== 0) kernelFlags |= KERNEL_O_APPEND;
      if ((normalizedFlags & WASI_FDFLAGS_NONBLOCK) !== 0) kernelFlags |= KERNEL_O_NONBLOCK;
      if ((normalizedFlags & WASI_LIBC_O_DIRECT) !== 0) kernelFlags |= KERNEL_O_DIRECT;

      const kernelFd = Number(callSyncRpc('fs.openTmpfileSync', [
        directory,
        kernelFlags,
        Number(mode) >>> 0,
        (normalizedFlags & (WASI_OFLAGS_EXCL << 12)) === 0,
      ])) >>> 0;
      const guestFd = registerKernelDelegateFd(kernelFd);
      const result = writeGuestUint32(retFdPtr, guestFd);
      if (result !== WASI_ERRNO_SUCCESS) wasiImport.fd_close(guestFd);
      return result;
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  },
  fd_link(fd, newDirFd, pathPtr, pathLen) {
    try {
      const handle = lookupFdHandle(Number(fd) >>> 0);
      const destination = resolvePathOpenGuestPath(newDirFd, pathPtr, pathLen);
      if (
        handle?.kind !== 'guest-file' && handle?.kind !== 'kernel-fd' ||
        typeof handle.targetFd !== 'number'
      ) {
        return WASI_ERRNO_INVAL;
      }
      if (typeof destination !== 'string') {
        return WASI_ERRNO_BADF;
      }
      if (isWorkspaceReadOnly() || guestPathIsReadOnly(destination)) {
        return WASI_ERRNO_ROFS;
      }
      callSyncRpc('fs.linkFdSync', [Number(handle.targetFd) >>> 0, destination]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  },
  remount(pathPtr, pathLen, optionsPtr, optionsLen) {
    try {
      const target = path.posix.resolve(HOST_FS_GUEST_CWD, readGuestString(pathPtr, pathLen));
      const options = readGuestString(optionsPtr, optionsLen);
      callSyncRpc('fs.remountSync', [target, options]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  },
  path_mknod(fd, pathPtr, pathLen, mode, rdev) {
    let target;
    try {
      const rawTarget = readGuestString(pathPtr, pathLen);
      target = Number(fd) >>> 0 === 0xffffffff
        ? path.posix.resolve(HOST_FS_GUEST_CWD, rawTarget)
        : resolvePathOpenGuestPath(fd, pathPtr, pathLen);
      if (typeof target !== 'string') {
        return WASI_ERRNO_BADF;
      }
      callSyncRpc('fs.mknodSync', [target, Number(mode) >>> 0, Number(rdev)]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      traceHostProcess('host-fs-path-mknod-error', {
        code: error?.code,
        message: error?.message,
        target,
      });
      return mapSyntheticFsError(error);
    }
  },
  path_renameat2(
    oldFd,
    oldPathPtr,
    oldPathLen,
    newFd,
    newPathPtr,
    newPathLen,
    flags,
  ) {
    let oldPath;
    let newPath;
    try {
      const rawOldPath = readGuestString(oldPathPtr, oldPathLen);
      const rawNewPath = readGuestString(newPathPtr, newPathLen);
      const oldDirectoryFd = Number(oldFd) >>> 0;
      const newDirectoryFd = Number(newFd) >>> 0;
      oldPath = oldDirectoryFd === 0xfffffffe || oldDirectoryFd === 0xffffffff
        ? path.posix.resolve(HOST_FS_GUEST_CWD, rawOldPath)
        : resolvePathOpenGuestPath(oldFd, oldPathPtr, oldPathLen);
      newPath = newDirectoryFd === 0xfffffffe || newDirectoryFd === 0xffffffff
        ? path.posix.resolve(HOST_FS_GUEST_CWD, rawNewPath)
        : resolvePathOpenGuestPath(newFd, newPathPtr, newPathLen);
      if (typeof oldPath !== 'string' || typeof newPath !== 'string') {
        return WASI_ERRNO_BADF;
      }
      if (
        isWorkspaceReadOnly() ||
        guestPathIsReadOnly(oldPath) ||
        guestPathIsReadOnly(newPath)
      ) {
        return WASI_ERRNO_ROFS;
      }
      callSyncRpc('fs.renameAt2Sync', [oldPath, newPath, Number(flags) >>> 0]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      traceHostProcess('host-fs-path-renameat2-error', {
        code: error?.code,
        message: error?.message,
        oldPath,
        newPath,
        flags: Number(flags) >>> 0,
      });
      return mapSyntheticFsError(error);
    }
  },
  path_statfs(
    fd,
    pathPtr,
    pathLen,
    retTotalBytesPtr,
    retUsedBytesPtr,
    retAvailableBytesPtr,
    retTotalInodesPtr,
    retFreeInodesPtr,
  ) {
    let target;
    try {
      if (!guestRangesAreValid(
        [retTotalBytesPtr, 8],
        [retUsedBytesPtr, 8],
        [retAvailableBytesPtr, 8],
        [retTotalInodesPtr, 8],
        [retFreeInodesPtr, 8],
      )) {
        return WASI_ERRNO_FAULT;
      }
      const rawTarget = readGuestString(pathPtr, pathLen);
      target = Number(fd) >>> 0 === 0xffffffff
        ? path.posix.resolve(HOST_FS_GUEST_CWD, rawTarget)
        : resolvePathOpenGuestPath(fd, pathPtr, pathLen);
      if (typeof target !== 'string') {
        return WASI_ERRNO_BADF;
      }
      const stats = callSyncRpc('fs.statfsSync', [target]);
      return (
        writeGuestUint64(retTotalBytesPtr, BigInt(stats.totalBytes)) ||
        writeGuestUint64(retUsedBytesPtr, BigInt(stats.usedBytes)) ||
        writeGuestUint64(retAvailableBytesPtr, BigInt(stats.availableBytes)) ||
        writeGuestUint64(retTotalInodesPtr, BigInt(stats.totalInodes)) ||
        writeGuestUint64(retFreeInodesPtr, BigInt(stats.freeInodes))
      );
    } catch (error) {
      traceHostProcess('host-fs-path-statfs-error', {
        code: error?.code,
        message: error?.message,
        target,
      });
      return mapSyntheticFsError(error);
    }
  },
  fd_fiemap(fd, index, retStartPtr, retEndPtr, retFlagsPtr) {
    try {
      if (!guestRangesAreValid([retStartPtr, 8], [retEndPtr, 8], [retFlagsPtr, 4])) {
        return WASI_ERRNO_FAULT;
      }
      const handle = lookupFdHandle(Number(fd) >>> 0);
      if (handle?.kind !== 'guest-file' && handle?.kind !== 'kernel-fd' || typeof handle.targetFd !== 'number') {
        return WASI_ERRNO_BADF;
      }
      const range = callSyncRpc('fs.fiemapAtSync', [
        Number(handle.targetFd) >>> 0,
        Number(index) >>> 0,
      ]);
      if (!range) {
        return WASI_ERRNO_NODATA;
      }
      return (
        writeGuestUint64(retStartPtr, BigInt(range.start)) ||
        writeGuestUint64(retEndPtr, BigInt(range.end)) ||
        writeGuestUint32(retFlagsPtr, range.unwritten === true ? 0x800 : 0)
      );
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  },
  fd_punch_hole(fd, offset, length) {
    try {
      const handle = lookupFdHandle(Number(fd) >>> 0);
      if (handle?.readOnly === true || isWorkspaceReadOnly()) {
        return WASI_ERRNO_ROFS;
      }
      if (handle?.kind !== 'guest-file' && handle?.kind !== 'kernel-fd' || typeof handle.targetFd !== 'number') {
        return WASI_ERRNO_BADF;
      }
      const punchOffset = Number(offset);
      const punchLength = Number(length);
      if (
        !Number.isSafeInteger(punchOffset) ||
        !Number.isSafeInteger(punchLength) ||
        punchOffset < 0 ||
        punchLength < 0
      ) {
        return WASI_ERRNO_INVAL;
      }
      callSyncRpc('fs.punchHoleSync', [
        Number(handle.targetFd) >>> 0,
        punchOffset,
        punchLength,
      ]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  },
  fd_zero_range(fd, offset, length, keepSize) {
    return mutateGuestFileRange(
      fd,
      offset,
      length,
      'fs.zeroRangeSync',
      [Number(keepSize) !== 0 ? 1 : 0],
    );
  },
  fd_insert_range(fd, offset, length) {
    return mutateGuestFileRange(fd, offset, length, 'fs.insertRangeSync');
  },
  fd_collapse_range(fd, offset, length) {
    return mutateGuestFileRange(fd, offset, length, 'fs.collapseRangeSync');
  },
  set_open_mode(mode) {
    pendingOpenCreateMode = Number(mode) & 0o7777;
    return WASI_ERRNO_SUCCESS;
  },
  set_open_direct(enabled) {
    pendingOpenDirect = Number(enabled) !== 0;
    return WASI_ERRNO_SUCCESS;
  },
  path_owner(fd, pathPtr, pathLen, followSymlinks, retUidPtr, retGidPtr) {
    try {
      if (!guestRangesAreValid([retUidPtr, 4], [retGidPtr, 4])) {
        return WASI_ERRNO_FAULT;
      }
      const numericFd = Number(fd) >>> 0;
      const rawTarget = readGuestString(pathPtr, pathLen);
      const operand = numericFd === NODE_CWD_FD
        ? { dirFd: NODE_CWD_FD, path: path.posix.resolve(HOST_FS_GUEST_CWD, rawTarget) }
        : kernelPathOperand(numericFd, pathPtr, pathLen);
      if (!operand) {
        return WASI_ERRNO_BADF;
      }
      const stat = callSyncRpc('process.path_stat_at', [
        operand.dirFd,
        operand.path,
        Number(followSymlinks) !== 0,
      ]);
      return writeGuestUint32(retUidPtr, stat.uid) || writeGuestUint32(retGidPtr, stat.gid);
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  },
  fd_owner(fd, retUidPtr, retGidPtr) {
    try {
      if (!guestRangesAreValid([retUidPtr, 4], [retGidPtr, 4])) {
        return WASI_ERRNO_FAULT;
      }
      const handle = lookupFdHandle(fd);
      if (handle?.kind === 'kernel-fd') {
        const stat = callSyncRpc('process.fd_filestat', [Number(handle.targetFd) >>> 0]);
        return writeGuestUint32(retUidPtr, stat.uid) || writeGuestUint32(retGidPtr, stat.gid);
      }
      if (handle?.kind === 'pipe-read' || handle?.kind === 'pipe-write') {
        const uid = callSyncRpc('process.geteuid', []);
        const gid = callSyncRpc('process.getegid', []);
        return writeGuestUint32(retUidPtr, uid) || writeGuestUint32(retGidPtr, gid);
      }
      const target = guestPathForManagedFd(fd);
      if (typeof target !== 'string') {
        return WASI_ERRNO_BADF;
      }
      const stat = callSyncRpc('fs.statSync', [target]);
      return writeGuestUint32(retUidPtr, stat.uid) || writeGuestUint32(retGidPtr, stat.gid);
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  },
  path_access(fd, pathPtr, pathLen, mode, effectiveIds) {
    try {
      const rawTarget = readGuestString(pathPtr, pathLen);
      const target = Number(fd) >>> 0 === 0xffffffff
        ? path.posix.resolve(HOST_FS_GUEST_CWD, rawTarget)
        : resolvePathOpenGuestPath(fd, pathPtr, pathLen);
      if (typeof target !== 'string') {
        return WASI_ERRNO_BADF;
      }
      callSyncRpc('fs.accessSync', [target, Number(mode) >>> 0, Number(effectiveIds) !== 0]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      traceHostProcess('host-fs-path-access-error', {
        code: error?.code,
        message: error?.message,
      });
      return mapSyntheticFsError(error);
    }
  },
  path_chown(fd, pathPtr, pathLen, uid, gid, followSymlinks) {
    try {
      const numericFd = Number(fd) >>> 0;
      const rawTarget = readGuestString(pathPtr, pathLen);
      const operand = numericFd === NODE_CWD_FD
        ? { dirFd: NODE_CWD_FD, path: path.posix.resolve(HOST_FS_GUEST_CWD, rawTarget) }
        : kernelPathOperand(numericFd, pathPtr, pathLen);
      if (!operand) {
        return WASI_ERRNO_BADF;
      }
      callSyncRpc('process.path_chown_at', [
        operand.dirFd,
        operand.path,
        Number(uid) >>> 0,
        Number(gid) >>> 0,
        Number(followSymlinks) !== 0,
      ]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      traceHostProcess('host-fs-path-chown-error', {
        code: error?.code,
        message: error?.message,
        followSymlinks: Number(followSymlinks) !== 0,
        fd: Number(fd) >>> 0,
      });
      return mapSyntheticFsError(error);
    }
  },
  fd_chown(fd, uid, gid) {
    try {
      const handle = lookupFdHandle(fd);
      if (handle?.kind !== 'kernel-fd') {
        return WASI_ERRNO_BADF;
      }
      callSyncRpc('process.fd_chown', [
        Number(handle.targetFd) >>> 0,
        Number(uid) >>> 0,
        Number(gid) >>> 0,
      ]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  },
  path_getxattr(
    fd,
    pathPtr,
    pathLen,
    namePtr,
    nameLen,
    valuePtr,
    size,
    followSymlinks,
    retSizePtr,
  ) {
    try {
      const nameErrno = checkFixedRequestLimit(
        'wasm.abi.maxXattrNameBytes',
        nameLen,
        XATTR_NAME_MAX,
        WASI_ERRNO_RANGE,
      );
      if (nameErrno !== WASI_ERRNO_SUCCESS) return nameErrno;
      const capacity = Number(size) >>> 0;
      if (!guestRangesAreValid([retSizePtr, 4], [valuePtr, capacity])) {
        return WASI_ERRNO_FAULT;
      }
      const rawTarget = readGuestString(pathPtr, pathLen);
      const target = Number(fd) >>> 0 === 0xffffffff
        ? path.posix.resolve(HOST_FS_GUEST_CWD, rawTarget)
        : resolvePathOpenGuestPath(fd, pathPtr, pathLen);
      if (typeof target !== 'string') return WASI_ERRNO_BADF;
      const value = callSyncRpc('fs.getxattrSync', [
        target,
        readGuestString(namePtr, nameLen),
        Number(followSymlinks) !== 0,
      ]);
      const bytes = Buffer.isBuffer(value) ? value : Buffer.from(value ?? []);
      const sizeResult = writeGuestUint32(retSizePtr, bytes.byteLength);
      if (sizeResult !== WASI_ERRNO_SUCCESS) return sizeResult;
      if (capacity === 0) return WASI_ERRNO_SUCCESS;
      if (capacity < bytes.byteLength) return WASI_ERRNO_RANGE;
      new Uint8Array(instanceMemory.buffer).set(bytes, Number(valuePtr) >>> 0);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  },
  fd_getxattr(fd, namePtr, nameLen, valuePtr, size, retSizePtr) {
    try {
      const nameErrno = checkFixedRequestLimit(
        'wasm.abi.maxXattrNameBytes',
        nameLen,
        XATTR_NAME_MAX,
        WASI_ERRNO_RANGE,
      );
      if (nameErrno !== WASI_ERRNO_SUCCESS) return nameErrno;
      const capacity = Number(size) >>> 0;
      if (!guestRangesAreValid([retSizePtr, 4], [valuePtr, capacity])) {
        return WASI_ERRNO_FAULT;
      }
      const handle = lookupFdHandle(fd);
      if (handle?.kind !== 'kernel-fd') return WASI_ERRNO_BADF;
      const value = callSyncRpc('fs.fgetxattrSync', [
        Number(handle.targetFd) >>> 0,
        readGuestString(namePtr, nameLen),
      ]);
      const bytes = Buffer.isBuffer(value) ? value : Buffer.from(value ?? []);
      const sizeResult = writeGuestUint32(retSizePtr, bytes.byteLength);
      if (sizeResult !== WASI_ERRNO_SUCCESS) return sizeResult;
      if (capacity === 0) return WASI_ERRNO_SUCCESS;
      if (capacity < bytes.byteLength) return WASI_ERRNO_RANGE;
      new Uint8Array(instanceMemory.buffer).set(bytes, Number(valuePtr) >>> 0);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  },
  path_listxattr(fd, pathPtr, pathLen, listPtr, size, followSymlinks, retSizePtr) {
    try {
      const capacity = Number(size) >>> 0;
      if (!guestRangesAreValid([retSizePtr, 4], [listPtr, capacity])) {
        return WASI_ERRNO_FAULT;
      }
      const rawTarget = readGuestString(pathPtr, pathLen);
      const target = Number(fd) >>> 0 === 0xffffffff
        ? path.posix.resolve(HOST_FS_GUEST_CWD, rawTarget)
        : resolvePathOpenGuestPath(fd, pathPtr, pathLen);
      if (typeof target !== 'string') return WASI_ERRNO_BADF;
      const names = callSyncRpc('fs.listxattrSync', [target, Number(followSymlinks) !== 0]);
      const bytes = Buffer.from((Array.isArray(names) ? names : []).map((name) => `${name}\0`).join(''));
      const sizeResult = writeGuestUint32(retSizePtr, bytes.byteLength);
      if (sizeResult !== WASI_ERRNO_SUCCESS) return sizeResult;
      if (capacity === 0) return WASI_ERRNO_SUCCESS;
      if (capacity < bytes.byteLength) return WASI_ERRNO_RANGE;
      new Uint8Array(instanceMemory.buffer).set(bytes, Number(listPtr) >>> 0);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  },
  fd_listxattr(fd, listPtr, size, retSizePtr) {
    try {
      const capacity = Number(size) >>> 0;
      if (!guestRangesAreValid([retSizePtr, 4], [listPtr, capacity])) {
        return WASI_ERRNO_FAULT;
      }
      const handle = lookupFdHandle(fd);
      if (handle?.kind !== 'kernel-fd') return WASI_ERRNO_BADF;
      const names = callSyncRpc('fs.flistxattrSync', [Number(handle.targetFd) >>> 0]);
      const bytes = Buffer.from((Array.isArray(names) ? names : []).map((name) => `${name}\0`).join(''));
      const sizeResult = writeGuestUint32(retSizePtr, bytes.byteLength);
      if (sizeResult !== WASI_ERRNO_SUCCESS) return sizeResult;
      if (capacity === 0) return WASI_ERRNO_SUCCESS;
      if (capacity < bytes.byteLength) return WASI_ERRNO_RANGE;
      new Uint8Array(instanceMemory.buffer).set(bytes, Number(listPtr) >>> 0);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  },
  path_setxattr(
    fd,
    pathPtr,
    pathLen,
    namePtr,
    nameLen,
    valuePtr,
    size,
    flags,
    followSymlinks,
  ) {
    let target;
    try {
      const nameErrno = checkFixedRequestLimit(
        'wasm.abi.maxXattrNameBytes',
        nameLen,
        XATTR_NAME_MAX,
        WASI_ERRNO_RANGE,
      );
      if (nameErrno !== WASI_ERRNO_SUCCESS) return nameErrno;
      const valueErrno = checkFixedRequestLimit(
        'wasm.abi.maxXattrValueBytes',
        size,
        XATTR_SIZE_MAX,
        WASI_ERRNO_2BIG,
      );
      if (valueErrno !== WASI_ERRNO_SUCCESS) return valueErrno;
      const rawTarget = readGuestString(pathPtr, pathLen);
      target = Number(fd) >>> 0 === 0xffffffff
        ? path.posix.resolve(HOST_FS_GUEST_CWD, rawTarget)
        : resolvePathOpenGuestPath(fd, pathPtr, pathLen);
      if (typeof target !== 'string') return WASI_ERRNO_BADF;
      callSyncRpc('fs.setxattrSync', [
        target,
        readGuestString(namePtr, nameLen),
        readGuestBytes(valuePtr, size),
        Number(flags) >>> 0,
        Number(followSymlinks) !== 0,
      ]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      traceHostProcess('host-fs-path-setxattr-error', {
        code: error?.code,
        message: error?.message,
        target,
      });
      return mapSyntheticFsError(error);
    }
  },
  fd_setxattr(fd, namePtr, nameLen, valuePtr, size, flags) {
    try {
      const nameErrno = checkFixedRequestLimit(
        'wasm.abi.maxXattrNameBytes',
        nameLen,
        XATTR_NAME_MAX,
        WASI_ERRNO_RANGE,
      );
      if (nameErrno !== WASI_ERRNO_SUCCESS) return nameErrno;
      const valueErrno = checkFixedRequestLimit(
        'wasm.abi.maxXattrValueBytes',
        size,
        XATTR_SIZE_MAX,
        WASI_ERRNO_2BIG,
      );
      if (valueErrno !== WASI_ERRNO_SUCCESS) return valueErrno;
      const handle = lookupFdHandle(fd);
      if (handle?.kind !== 'kernel-fd') return WASI_ERRNO_BADF;
      callSyncRpc('fs.fsetxattrSync', [
        Number(handle.targetFd) >>> 0,
        readGuestString(namePtr, nameLen),
        readGuestBytes(valuePtr, size),
        Number(flags) >>> 0,
      ]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  },
  path_removexattr(fd, pathPtr, pathLen, namePtr, nameLen, followSymlinks) {
    try {
      const nameErrno = checkFixedRequestLimit(
        'wasm.abi.maxXattrNameBytes',
        nameLen,
        XATTR_NAME_MAX,
        WASI_ERRNO_RANGE,
      );
      if (nameErrno !== WASI_ERRNO_SUCCESS) return nameErrno;
      const rawTarget = readGuestString(pathPtr, pathLen);
      const target = Number(fd) >>> 0 === 0xffffffff
        ? path.posix.resolve(HOST_FS_GUEST_CWD, rawTarget)
        : resolvePathOpenGuestPath(fd, pathPtr, pathLen);
      if (typeof target !== 'string') return WASI_ERRNO_BADF;
      callSyncRpc('fs.removexattrSync', [
        target,
        readGuestString(namePtr, nameLen),
        Number(followSymlinks) !== 0,
      ]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  },
  fd_removexattr(fd, namePtr, nameLen) {
    try {
      const nameErrno = checkFixedRequestLimit(
        'wasm.abi.maxXattrNameBytes',
        nameLen,
        XATTR_NAME_MAX,
        WASI_ERRNO_RANGE,
      );
      if (nameErrno !== WASI_ERRNO_SUCCESS) return nameErrno;
      const handle = lookupFdHandle(fd);
      if (handle?.kind !== 'kernel-fd') return WASI_ERRNO_BADF;
      callSyncRpc('fs.fremovexattrSync', [
        Number(handle.targetFd) >>> 0,
        readGuestString(namePtr, nameLen),
      ]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  },
  fd_mode(fd) {
    const descriptor = Number(fd) >>> 0;
    const handle = lookupFdHandle(descriptor);
    if (handle?.kind === 'pipe-read' || handle?.kind === 'pipe-write') {
      return HOST_FS_MODE_FIFO;
    }

    if (handle?.kind === 'kernel-fd') {
      try {
        const stat = callSyncRpc('process.fd_filestat', [Number(handle.targetFd) >>> 0]);
        return Number(stat?.mode) >>> 0;
      } catch {
        return 0;
      }
    }

    if (descriptor <= 2) {
      return HOST_FS_MODE_CHARACTER;
    }

    return 0;
  },
  fd_size(fd) {
    const descriptor = Number(fd) >>> 0;
    try {
      const handle = lookupFdHandle(descriptor);
      if (handle?.kind !== 'kernel-fd') return (1n << 64n) - 1n;
      const stat = callSyncRpc('process.fd_filestat', [Number(handle.targetFd) >>> 0]);
      return BigInt(stat?.size ?? -1);
    } catch {
      return (1n << 64n) - 1n;
    }
  },
  fd_blocks(fd) {
    const descriptor = Number(fd) >>> 0;
    try {
      const handle = lookupFdHandle(descriptor);
      if (handle?.kind !== 'kernel-fd') return (1n << 64n) - 1n;
      const stat = callSyncRpc('process.fd_filestat', [Number(handle.targetFd) >>> 0]);
      return BigInt(stat?.blocks ?? -1);
    } catch {
      return (1n << 64n) - 1n;
    }
  },
  path_mode(fd, pathPtr, pathLen, followSymlinks) {
    try {
      const operand = kernelPathOperand(fd, pathPtr, pathLen);
      if (!operand) return 0;
      const stat = callSyncRpc('process.path_stat_at', [
        operand.dirFd,
        operand.path,
        Number(followSymlinks) !== 0,
      ]);
      return Number(stat?.mode) >>> 0;
    } catch (error) {
      traceHostProcess('host-fs-path-mode-fault', {
        fd: Number(fd) >>> 0,
        path: (() => {
          try { return readGuestString(pathPtr, pathLen); } catch { return null; }
        })(),
        code: error?.code,
        message: error?.message,
      });
      return 0;
    }
  },
  path_size(fd, pathPtr, pathLen, followSymlinks) {
    try {
      const operand = kernelPathOperand(fd, pathPtr, pathLen);
      if (!operand) return (1n << 64n) - 1n;
      const stat = callSyncRpc('process.path_stat_at', [
        operand.dirFd,
        operand.path,
        Number(followSymlinks) !== 0,
      ]);
      return BigInt(stat?.size ?? -1);
    } catch {
      return (1n << 64n) - 1n;
    }
  },
  path_blocks(fd, pathPtr, pathLen, followSymlinks) {
    try {
      const operand = kernelPathOperand(fd, pathPtr, pathLen);
      if (!operand) return (1n << 64n) - 1n;
      const stat = callSyncRpc('process.path_stat_at', [
        operand.dirFd,
        operand.path,
        Number(followSymlinks) !== 0,
      ]);
      return BigInt(stat?.blocks ?? -1);
    } catch {
      return (1n << 64n) - 1n;
    }
  },
  path_rdev(fd, pathPtr, pathLen, followSymlinks) {
    try {
      const operand = kernelPathOperand(fd, pathPtr, pathLen);
      if (!operand) return 0n;
      const stat = callSyncRpc('process.path_stat_at', [
        operand.dirFd,
        operand.path,
        Number(followSymlinks) !== 0,
      ]);
      return BigInt(stat?.rdev ?? 0);
    } catch {
      return 0n;
    }
  },
  chmod(fd, pathPtr, pathLen, mode) {
    try {
      const operand = kernelPathOperand(fd, pathPtr, pathLen);
      if (!operand) return WASI_ERRNO_BADF;
      callSyncRpc('process.path_chmod_at', [
        operand.dirFd,
        operand.path,
        Number(mode) >>> 0,
      ]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      traceHostProcess('host-fs-chmod-fault', {
        message: error instanceof Error ? error.message : String(error),
      });
      return mapSyntheticFsError(error);
    }
  },
  fchmod(fd, mode) {
    try {
      const descriptor = Number(fd) >>> 0;
      const handle = lookupFdHandle(descriptor);
      if (handle?.kind !== 'kernel-fd') return WASI_ERRNO_BADF;
      callSyncRpc('process.fd_chmod', [
        Number(handle.targetFd) >>> 0,
        Number(mode) >>> 0,
      ]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      traceHostProcess('host-fs-fchmod-fault', {
        message: error instanceof Error ? error.message : String(error),
      });
      return mapSyntheticFsError(error);
    }
  },
  chown(fd, pathPtr, pathLen, uid, gid, followSymlinks) {
    try {
      const operand = kernelPathOperand(fd, pathPtr, pathLen);
      if (!operand) {
        return WASI_ERRNO_BADF;
      }
      callSyncRpc('process.path_chown_at', [
        operand.dirFd,
        operand.path,
        Number(uid) >>> 0,
        Number(gid) >>> 0,
        Number(followSymlinks) !== 0,
      ]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      traceHostProcess('host-fs-chown-fault', {
        message: error instanceof Error ? error.message : String(error),
      });
      return mapSyntheticFsError(error);
    }
  },
  fchown(fd, uid, gid) {
    try {
      const descriptor = Number(fd) >>> 0;
      const handle = lookupFdHandle(descriptor);
      if (handle?.kind !== 'kernel-fd') {
        return WASI_ERRNO_BADF;
      }
      callSyncRpc('process.fd_chown', [
        Number(handle.targetFd) >>> 0,
        Number(uid) >>> 0,
        Number(gid) >>> 0,
      ]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      traceHostProcess('host-fs-fchown-fault', {
        message: error instanceof Error ? error.message : String(error),
      });
      return mapSyntheticFsError(error);
    }
  },
  ftruncate(fd, length) {
    try {
      const descriptor = Number(fd) >>> 0;
      const nextSize = Number(length);
      if (!Number.isFinite(nextSize) || nextSize < 0) {
        return WASI_ERRNO_INVAL;
      }
      const handle = lookupFdHandle(descriptor);
      if (handle?.kind !== 'kernel-fd') return WASI_ERRNO_BADF;
      callSyncRpc('process.fd_truncate', [
        Number(handle.targetFd) >>> 0,
        BigInt(nextSize).toString(),
      ]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  },
};

wasiImport.clock_time_get = (clockId, precision, resultPtr) => {
  if (!guestRangeIsValid(resultPtr, 8)) return WASI_ERRNO_FAULT;
  const numericClockId = Number(clockId) >>> 0;
  if (SIDECAR_MANAGED_PROCESS) {
    try {
      const deterministicRealtime = numericClockId === 0 ? frozenTimeNs.toString() : null;
      const value = callSyncRpc('process.clock_time', [
        numericClockId,
        BigInt(precision).toString(),
        deterministicRealtime,
      ]);
      const nanoseconds = BigInt(value);
      if (nanoseconds < 0n || nanoseconds > 0xffffffffffffffffn) {
        return WASI_ERRNO_OVERFLOW;
      }
      new DataView(instanceMemory.buffer).setBigUint64(Number(resultPtr), nanoseconds, true);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapHostProcessError(error);
    }
  }
  if (numericClockId !== 0 && delegateClockTimeGet) {
    return delegateClockTimeGet(clockId, precision, resultPtr);
  }
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
  if (!guestRangeIsValid(resultPtr, 8)) return WASI_ERRNO_FAULT;
  const numericClockId = Number(clockId) >>> 0;
  if (SIDECAR_MANAGED_PROCESS) {
    try {
      const value = callSyncRpc('process.clock_resolution', [numericClockId]);
      const nanoseconds = BigInt(value);
      if (nanoseconds < 0n || nanoseconds > 0xffffffffffffffffn) {
        return WASI_ERRNO_OVERFLOW;
      }
      new DataView(instanceMemory.buffer).setBigUint64(Number(resultPtr), nanoseconds, true);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapHostProcessError(error);
    }
  }
  if (numericClockId !== 0 && delegateClockResGet) {
    return delegateClockResGet(clockId, resultPtr);
  }
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

// Managed VMs use the shared sidecar entropy source. Validate the complete
// guest output range and configured total limit before the first host request
// or write, then use bounded response chunks so neither bridge lane nor host
// allocation scales with an attacker-selected linear-memory length.
if (SIDECAR_MANAGED_PROCESS) {
  wasiImport.random_get = (bufferPtr, bufferLength) => {
    const length = Number(bufferLength) >>> 0;
    if (!guestRangeIsValid(bufferPtr, length)) return WASI_ERRNO_FAULT;
    if (length > __agentOSWasmEntropyLimitBytes) return WASI_ERRNO_2BIG;
    const output = new Uint8Array(instanceMemory.buffer);
    const base = Number(bufferPtr) >>> 0;
    const chunkBytes = 64 * 1024;
    try {
      for (let offset = 0; offset < length; offset += chunkBytes) {
        const requested = Math.min(chunkBytes, length - offset);
        const bytes = Buffer.from(callSyncRpc('process.random_get', [requested]) ?? []);
        if (bytes.byteLength !== requested) return WASI_ERRNO_IO;
        output.set(bytes, base + offset);
      }
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapHostProcessError(error);
    }
  };
}

if (delegatePathOpen) {
  wasiImport.path_open = (
    fd,
    dirflags,
    pathPtr,
    pathLen,
    oflags,
    rightsBase,
    rightsInheriting,
    fdflags,
    openedFdPtr,
  ) => {
    if (!guestRangesAreValid([pathPtr, Number(pathLen) >>> 0], [openedFdPtr, 4])) {
      return WASI_ERRNO_FAULT;
    }
    const requestedCreateMode = pendingOpenCreateMode;
    pendingOpenCreateMode = null;
    const requestedDirect = pendingOpenDirect;
    pendingOpenDirect = false;
    const workspaceReadOnlyDenied = __agentOSWasiMeasurePhase('path_open', 'readonly_policy', () =>
      isWorkspaceReadOnly() &&
      (hasMutationOpenFlags(oflags) || hasWriteRights(rightsBase))
    );
    if (workspaceReadOnlyDenied) {
      return denyReadOnlyMutation();
    }

    const passthroughDirHandle = __agentOSWasiMeasurePhase('path_open', 'lookup_handle', () =>
      lookupFdHandle(fd)
    );
    if (
      passthroughDirHandle &&
      passthroughDirHandle.kind !== 'passthrough' &&
      passthroughDirHandle.kind !== 'kernel-fd'
    ) {
      return WASI_ERRNO_BADF;
    }
    if (!passthroughDirHandle && rejectClosedPassthroughFd(fd)) {
      return WASI_ERRNO_BADF;
    }
    if (SIDECAR_MANAGED_PROCESS && passthroughDirHandle?.kind !== 'kernel-fd') {
      return WASI_ERRNO_BADF;
    }
    const managedOpenPath = SIDECAR_MANAGED_PROCESS
      ? kernelOpenPathForGuestPath(readGuestString(pathPtr, pathLen))
      : null;

    const delegateDirFd =
      passthroughDirHandle?.kind === 'passthrough'
        ? passthroughDirHandle.targetFd
        : fd;
    const guestPath = __agentOSWasiMeasurePhase('path_open', 'path_resolution', () =>
      resolvePathOpenGuestPath(fd, pathPtr, pathLen)
    );
    if (hasMutationOpenFlags(oflags) || hasWriteRights(rightsBase)) {
      traceHostProcess('path-open-mutation', {
        fd: Number(fd) >>> 0,
        guestPath,
        permissionTier,
        handleKind: passthroughDirHandle?.kind ?? null,
        workspaceReadOnly: isWorkspaceReadOnly(),
        guestReadOnly: guestPathIsReadOnly(guestPath),
        oflags: Number(oflags) >>> 0,
        rightsBase: String(rightsBase),
        fdflags: Number(fdflags) >>> 0,
      });
    }
    const guestReadOnlyDenied = __agentOSWasiMeasurePhase('path_open', 'readonly_policy', () =>
      guestPathIsReadOnly(guestPath) &&
      (hasMutationOpenFlags(oflags) || hasWriteRights(rightsBase))
    );
    if (guestReadOnlyDenied) {
      return denyReadOnlyMutation();
    }
    const procFdResult = SIDECAR_MANAGED_PROCESS
      ? null
      : openProcSelfFdAlias(
          guestPath,
          oflags,
          rightsBase,
          dirflags,
          openedFdPtr,
        );
    if (procFdResult !== null) {
      return procFdResult;
    }
    if (SIDECAR_MANAGED_PROCESS) {
      try {
        if (typeof guestPath === 'string') {
          const fifoResult = openBlockingGuestFifoForPathOpen(
            Number(passthroughDirHandle.targetFd) >>> 0,
            managedOpenPath,
            guestPath,
            oflags,
            rightsBase,
            rightsInheriting,
            fdflags,
            dirflags,
            openedFdPtr,
            requestedCreateMode ?? 0o666,
            requestedDirect,
          );
          if (fifoResult !== null) return fifoResult;
        }
      } catch (error) {
        return mapHostProcessError(error);
      }
    }
    if (SIDECAR_MANAGED_PROCESS) {
      if (!hasRunnerOpenFdCapacity(1)) return WASI_ERRNO_MFILE;
      try {
        const kernelFd = Number(callSyncRpc('process.path_open_at', [
          Number(passthroughDirHandle.targetFd) >>> 0,
          managedOpenPath,
          kernelOpenFlagsFromWasi(oflags, rightsBase, fdflags, dirflags, requestedDirect),
          requestedCreateMode ?? 0o666,
          rightsBase.toString(),
          rightsInheriting.toString(),
        ])) >>> 0;
        if ((Number(oflags) & WASI_OFLAGS_DIRECTORY) !== 0) {
          const stat = callSyncRpc('process.fd_stat', [kernelFd]);
          if ((Number(stat?.filetype) >>> 0) !== WASI_FILETYPE_DIRECTORY) {
            callSyncRpc('process.fd_close', [kernelFd]);
            const error = new Error(`${guestPath} is not a directory`);
            error.code = 'ENOTDIR';
            throw error;
          }
        }
        const openedFd = registerKernelDelegateFd(kernelFd);
        const writeResult = writeGuestUint32(openedFdPtr, openedFd);
        traceHostProcess('path-open-managed', {
          inputFd: Number(fd) >>> 0,
          kernelDirFd: Number(passthroughDirHandle.targetFd) >>> 0,
          path: managedOpenPath,
          kernelFd,
          openedFd,
          writeResult,
        });
        if (writeResult !== WASI_ERRNO_SUCCESS) {
          wasiImport.fd_close(openedFd);
        }
        return writeResult;
      } catch (error) {
        traceHostProcess('path-open-managed-error', {
          inputFd: Number(fd) >>> 0,
          guestPath,
          path: managedOpenPath,
          oflags: Number(oflags) >>> 0,
          rightsBase: String(rightsBase),
          fdflags: Number(fdflags) >>> 0,
          code: error?.code,
          message: String(error?.message ?? error),
        });
        return mapHostProcessError(error);
      }
    }
    const mayCreateTarget = pathOpenMayCreateTarget(oflags, rightsBase, fdflags);
    if (!SIDECAR_MANAGED_PROCESS && mayCreateTarget) {
      try {
        const syntheticResult = __agentOSWasiMeasurePhase(
          'path_open',
          'synthetic_open',
          () => openGuestFileForPathOpen(
            fd,
            pathPtr,
            pathLen,
            oflags,
            rightsBase,
            fdflags,
            openedFdPtr,
          ),
        );
        if (syntheticResult != null) {
          return syntheticResult;
        }
      } catch {
        return WASI_ERRNO_FAULT;
      }
    }

    if (!hasRunnerOpenFdCapacity(1)) {
      return WASI_ERRNO_MFILE;
    }

    let result = __agentOSWasiMeasurePhase(
      'path_open',
      'delegate_call',
      () => delegatePathOpen(
        delegateDirFd,
        dirflags,
        pathPtr,
        pathLen,
        oflags,
        rightsBase,
        rightsInheriting,
        fdflags,
        openedFdPtr,
      ),
    );

    // Precreate-and-retry exists for creatable targets the delegate reports
    // as missing (e.g. `>>` append redirect targets). Only retry on NOENT:
    // retrying on permission errors would mask the real errno and attempt to
    // create a file the kernel just denied.
    if (
      result === WASI_ERRNO_NOENT &&
      mayCreateTarget
    ) {
      try {
        __agentOSWasiMeasurePhase('path_open', 'synthetic_precreate', () =>
          precreatePathOpenTarget(fd, pathPtr, pathLen, oflags, rightsBase, fdflags)
        );
        result = __agentOSWasiMeasurePhase(
          'path_open',
          'delegate_call',
          () => delegatePathOpen(
            delegateDirFd,
            dirflags,
            pathPtr,
            pathLen,
            oflags,
            rightsBase,
            rightsInheriting,
            fdflags,
            openedFdPtr,
          ),
        );
        if (!SIDECAR_MANAGED_PROCESS && result !== WASI_ERRNO_SUCCESS) {
          const fallbackResult = __agentOSWasiMeasurePhase(
            'path_open',
            'synthetic_open',
            () => openGuestFileForPathOpen(
              fd,
              pathPtr,
              pathLen,
              oflags,
              rightsBase,
              fdflags,
              openedFdPtr,
            ),
          );
          if (fallbackResult != null) {
            return fallbackResult;
          }
        }
      } catch {
        return WASI_ERRNO_FAULT;
      }
    }

    if (result === WASI_ERRNO_SUCCESS) {
      return __agentOSWasiMeasurePhase('path_open', 'fd_bookkeeping', () =>
        retainPathOpenDelegateFd(openedFdPtr, guestPath, fdflags, rightsBase)
      );
    }
    return result;
  };
}

function delegatePathDirFd(fd) {
  const numericFd = Number(fd) >>> 0;
  const handle = lookupFdHandle(numericFd);
  if (handle?.kind !== 'passthrough') {
    return null;
  }
  return Number(handle.targetFd) >>> 0;
}

function kernelPathOperand(fd, pathPtr, pathLen) {
  const numericFd = Number(fd) >>> 0;
  if (numericFd === NODE_CWD_FD) {
    return {
      // AgentOS extension imports use this sentinel for an ordinary pathname
      // resolved from the process cwd. WASI's private tagged preopens still go
      // through lookupFdHandle so they retain their capability-root identity.
      dirFd: numericFd,
      path: readGuestString(pathPtr, pathLen),
    };
  }
  const handle = lookupFdHandle(fd);
  if (handle?.kind === 'kernel-fd') {
    return {
      dirFd: Number(handle.targetFd) >>> 0,
      path: readGuestString(pathPtr, pathLen),
    };
  }
  const resolved = resolvePathOpenGuestPath(fd, pathPtr, pathLen);
  return typeof resolved === 'string' ? { dirFd: 0, path: resolved } : null;
}

const kernelPathOperationHandlers = {
  path_create_directory(args) {
    const operand = kernelPathOperand(args[0], args[1], args[2]);
    if (!operand) return WASI_ERRNO_BADF;
    callSyncRpc('process.path_mkdir_at', [operand.dirFd, operand.path]);
    return WASI_ERRNO_SUCCESS;
  },
  path_filestat_get(args) {
    if (!guestRangesAreValid([args[2], Number(args[3]) >>> 0], [args[4], 64])) {
      return WASI_ERRNO_FAULT;
    }
    const operand = kernelPathOperand(args[0], args[2], args[3]);
    if (!operand) return WASI_ERRNO_BADF;
    const stat = callSyncRpc('process.path_stat_at', [
      operand.dirFd,
      operand.path,
      ((Number(args[1]) >>> 0) & WASI_LOOKUPFLAGS_SYMLINK_FOLLOW) !== 0,
    ]);
    return writeGuestFilestat(args[4], stat, Number(stat?.filetype) >>> 0);
  },
  path_filestat_set_times(args) {
    const operand = kernelPathOperand(args[0], args[2], args[3]);
    if (!operand) return WASI_ERRNO_BADF;
    callSyncRpc('process.path_utimes_at', [
      operand.dirFd,
      operand.path,
      ((Number(args[1]) >>> 0) & WASI_LOOKUPFLAGS_SYMLINK_FOLLOW) !== 0,
      BigInt(args[4]).toString(),
      BigInt(args[5]).toString(),
      Number(args[6]) >>> 0,
    ]);
    return WASI_ERRNO_SUCCESS;
  },
  path_link(args) {
    const oldOperand = kernelPathOperand(args[0], args[2], args[3]);
    const newOperand = kernelPathOperand(args[4], args[5], args[6]);
    if (!oldOperand || !newOperand) return WASI_ERRNO_BADF;
    callSyncRpc('process.path_link_at', [
      oldOperand.dirFd,
      oldOperand.path,
      newOperand.dirFd,
      newOperand.path,
      ((Number(args[1]) >>> 0) & WASI_LOOKUPFLAGS_SYMLINK_FOLLOW) !== 0,
    ]);
    return WASI_ERRNO_SUCCESS;
  },
  path_readlink(args) {
    const operand = kernelPathOperand(args[0], args[1], args[2]);
    if (!operand) return WASI_ERRNO_BADF;
    const target = Buffer.from(String(callSyncRpc('process.path_readlink_at', [
      operand.dirFd,
      kernelProcFdPathForGuestPath(operand.path),
    ])));
    return writeGuestBytes(args[3], args[4], target, args[5]);
  },
  path_remove_directory(args) {
    const operand = kernelPathOperand(args[0], args[1], args[2]);
    if (!operand) return WASI_ERRNO_BADF;
    callSyncRpc('process.path_remove_dir_at', [operand.dirFd, operand.path]);
    return WASI_ERRNO_SUCCESS;
  },
  path_rename(args) {
    const oldOperand = kernelPathOperand(args[0], args[1], args[2]);
    const newOperand = kernelPathOperand(args[3], args[4], args[5]);
    if (!oldOperand || !newOperand) return WASI_ERRNO_BADF;
    callSyncRpc('process.path_rename_at', [
      oldOperand.dirFd,
      oldOperand.path,
      newOperand.dirFd,
      newOperand.path,
    ]);
    return WASI_ERRNO_SUCCESS;
  },
  path_symlink(args) {
    const operand = kernelPathOperand(args[2], args[3], args[4]);
    if (!operand) return WASI_ERRNO_BADF;
    callSyncRpc('process.path_symlink_at', [
      readGuestString(args[0], args[1]),
      operand.dirFd,
      operand.path,
    ]);
    return WASI_ERRNO_SUCCESS;
  },
  path_unlink_file(args) {
    const operand = kernelPathOperand(args[0], args[1], args[2]);
    if (!operand) return WASI_ERRNO_BADF;
    callSyncRpc('process.path_unlink_at', [operand.dirFd, operand.path]);
    return WASI_ERRNO_SUCCESS;
  },
};

// All WASI path operations take one or more capability directory descriptors.
// Keep node:wasi's private preopens outside the Linux guest fd namespace by
// translating only libc's tagged aliases. An untagged fd that has been closed
// or replaced must never fall through to node:wasi, where it could otherwise
// accidentally name the private preopen at the same numeric descriptor.
function wrapPathDirFds(name, fdIndexes) {
  const delegate = typeof wasiImport[name] === 'function' ? wasiImport[name].bind(wasiImport) : null;
  if (!delegate) {
    return;
  }
  wasiImport[name] = (...args) => {
    const delegateArgs = [...args];
    // A managed guest has one filesystem source of truth: the sidecar kernel.
    // In particular, a relative mkdir through libc's hidden cwd/root preopen
    // must not be delegated into node:wasi's private host directory while a
    // subsequent path_open is sent to the kernel. That split made a freshly
    // created empty directory disappear until a child file happened to copy
    // its ancestors into the kernel overlay.
    if (SIDECAR_MANAGED_PROCESS) {
      try {
        return kernelPathOperationHandlers[name](delegateArgs);
      } catch (error) {
        traceHostProcess('path-operation-error', {
          operation: name,
          code: error?.code,
          message: String(error?.message ?? error),
        });
        return mapHostProcessError(error);
      }
    }
    if (fdIndexes.some((index) => lookupFdHandle(delegateArgs[index])?.kind === 'kernel-fd')) {
      return WASI_ERRNO_BADF;
    }
    for (const index of fdIndexes) {
      const delegateFd = delegatePathDirFd(delegateArgs[index]);
      if (delegateFd == null) {
        return WASI_ERRNO_BADF;
      }
      delegateArgs[index] = delegateFd;
    }
    return delegate(...delegateArgs);
  };
}

wrapPathDirFds('path_create_directory', [0]);
wrapPathDirFds('path_filestat_get', [0]);
wrapPathDirFds('path_filestat_set_times', [0]);
wrapPathDirFds('path_link', [0, 4]);
wrapPathDirFds('path_readlink', [0]);
wrapPathDirFds('path_remove_directory', [0]);
wrapPathDirFds('path_rename', [0, 3]);
wrapPathDirFds('path_symlink', [2]);
wrapPathDirFds('path_unlink_file', [0]);

function wrapReadOnlyPathMutation(name, shouldDeny) {
  const delegate = typeof wasiImport[name] === 'function' ? wasiImport[name].bind(wasiImport) : null;
  if (!delegate) {
    return;
  }
  wasiImport[name] = (...args) => {
    if (shouldDeny(...args)) {
      return denyReadOnlyMutation();
    }
    return delegate(...args);
  };
}

wrapReadOnlyPathMutation('path_create_directory', (fd, pathPtr, pathLen) =>
  resolvedGuestPathIsReadOnly(fd, pathPtr, pathLen),
);
wrapReadOnlyPathMutation('path_filestat_set_times', (fd, _flags, pathPtr, pathLen) =>
  resolvedGuestPathIsReadOnly(fd, pathPtr, pathLen),
);
wrapReadOnlyPathMutation(
  'path_link',
  (oldFd, _oldFlags, oldPathPtr, oldPathLen, newFd, newPathPtr, newPathLen) =>
    resolvedGuestPathIsReadOnly(oldFd, oldPathPtr, oldPathLen) ||
    resolvedGuestPathIsReadOnly(newFd, newPathPtr, newPathLen),
);
wrapReadOnlyPathMutation('path_remove_directory', (fd, pathPtr, pathLen) =>
  resolvedGuestPathIsReadOnly(fd, pathPtr, pathLen),
);
wrapReadOnlyPathMutation(
  'path_rename',
  (oldFd, oldPathPtr, oldPathLen, newFd, newPathPtr, newPathLen) =>
    resolvedGuestPathIsReadOnly(oldFd, oldPathPtr, oldPathLen) ||
    resolvedGuestPathIsReadOnly(newFd, newPathPtr, newPathLen),
);
wrapReadOnlyPathMutation('path_symlink', (_oldPathPtr, _oldPathLen, fd, newPathPtr, newPathLen) =>
  resolvedGuestPathIsReadOnly(fd, newPathPtr, newPathLen),
);
wrapReadOnlyPathMutation('path_unlink_file', (fd, pathPtr, pathLen) =>
  resolvedGuestPathIsReadOnly(fd, pathPtr, pathLen),
);

if (isWorkspaceReadOnly()) {

  wasiImport.fd_write = (fd, iovs, iovsLen, nwrittenPtr) => {
    if (Number(fd) > 2) {
      return denyReadOnlyMutation();
    }

    return delegateFdWrite ? delegateFdWrite(fd, iovs, iovsLen, nwrittenPtr) : WASI_ERRNO_FAULT;
  };

  wasiImport.fd_pwrite = (fd, iovs, iovsLen, offset, nwrittenPtr) => {
    if (Number(fd) > 2) {
      return denyReadOnlyMutation();
    }

    return delegateFdPwrite
      ? delegateFdPwrite(fd, iovs, iovsLen, offset, nwrittenPtr)
      : WASI_ERRNO_FAULT;
  };

  for (const name of [
    'fd_allocate',
    'fd_filestat_set_size',
    'fd_filestat_set_times',
    'path_create_directory',
    'path_filestat_set_times',
    'path_link',
    'path_remove_directory',
    'path_rename',
    'path_symlink',
    'path_unlink_file',
  ]) {
    if (typeof wasiImport[name] === 'function') {
      wasiImport[name] = () => denyReadOnlyMutation();
    }
  }
}

const delegateManagedFdRead =
  typeof wasiImport.fd_read === 'function'
    ? wasiImport.fd_read.bind(wasiImport)
    : null;
const delegateManagedFdWrite =
  typeof wasiImport.fd_write === 'function'
    ? wasiImport.fd_write.bind(wasiImport)
    : null;
const delegateManagedFdPwrite =
  typeof wasiImport.fd_pwrite === 'function'
    ? wasiImport.fd_pwrite.bind(wasiImport)
    : null;
const delegateManagedFdSeek =
  typeof wasiImport.fd_seek === 'function'
    ? wasiImport.fd_seek.bind(wasiImport)
    : null;
const delegateManagedFdTell =
  typeof wasiImport.fd_tell === 'function'
    ? wasiImport.fd_tell.bind(wasiImport)
    : null;
const delegateManagedFdFdstatGet =
  typeof wasiImport.fd_fdstat_get === 'function'
    ? wasiImport.fd_fdstat_get.bind(wasiImport)
    : null;
const delegateManagedFdFdstatSetFlags =
  typeof wasiImport.fd_fdstat_set_flags === 'function'
    ? wasiImport.fd_fdstat_set_flags.bind(wasiImport)
    : null;
const delegateManagedFdFilestatGet =
  typeof wasiImport.fd_filestat_get === 'function'
    ? wasiImport.fd_filestat_get.bind(wasiImport)
    : null;
const delegateManagedFdReaddir =
  typeof wasiImport.fd_readdir === 'function'
    ? wasiImport.fd_readdir.bind(wasiImport)
    : null;
const delegateManagedFdFilestatSetSize =
  typeof wasiImport.fd_filestat_set_size === 'function'
    ? wasiImport.fd_filestat_set_size.bind(wasiImport)
    : null;
const delegateManagedFdClose =
  typeof wasiImport.fd_close === 'function'
    ? wasiImport.fd_close.bind(wasiImport)
    : null;
const delegateManagedFdRenumber =
  typeof wasiImport.fd_renumber === 'function'
    ? wasiImport.fd_renumber.bind(wasiImport)
    : null;
const delegateManagedFdPrestatGet =
  typeof wasiImport.fd_prestat_get === 'function'
    ? wasiImport.fd_prestat_get.bind(wasiImport)
    : null;
const delegateManagedFdPrestatDirName =
  typeof wasiImport.fd_prestat_dir_name === 'function'
    ? wasiImport.fd_prestat_dir_name.bind(wasiImport)
    : null;
const delegatedWasiFdFlags = new Map([
  [0, 0],
  [1, 0],
  [2, 0],
]);
const delegateManagedPollOneoff =
  typeof wasiImport.poll_oneoff === 'function'
    ? wasiImport.poll_oneoff.bind(wasiImport)
    : null;
const KERNEL_POLLIN = 0x0001;
const KERNEL_POLLOUT = 0x0004;
const KERNEL_POLLERR = 0x0008;
const KERNEL_POLLHUP = 0x0010;

wasiImport.fd_read = (fd, iovs, iovsLen, nreadPtr) => {
  const iovRequest = validateGuestIovRequest(iovs, iovsLen);
  if (iovRequest.errno !== WASI_ERRNO_SUCCESS) return iovRequest.errno;
  if (!guestRangeIsValid(nreadPtr, 4)) return WASI_ERRNO_FAULT;
  const numericFd = Number(fd) >>> 0;
  const hostNetSocket = getHostNetSocket(numericFd);
  if (hostNetSocket) {
    return readHostNetSocketToGuestIovs(
      hostNetSocket,
      iovs,
      iovsLen,
      nreadPtr,
      iovRequest,
    );
  }

  const handle = __agentOSWasiMeasurePhase('fd_read', 'lookup_handle', () =>
    lookupFdHandle(numericFd)
  );
  if (handle?.kind === 'kernel-fd') {
    try {
      const requestedLength = boundedWasmSyncRpcReadLength(
        guestIovByteLength(iovs, iovsLen, iovRequest),
      );
      const kernelFd = Number(handle.targetFd) >>> 0;
      let bytes;
      const stat = callSyncRpc('process.fd_stat', [kernelFd]);
      const nonblocking = (Number(stat?.flags) & KERNEL_O_NONBLOCK) !== 0;
      const deadline = nonblocking
        ? Date.now()
        : maxBlockingReadMs == null
          ? null
          : Date.now() + maxBlockingReadMs;
      while (bytes == null) {
        try {
          // A process with local descendants must return to its own event pump
          // between zero-time probes. A leaf process can instead issue a
          // bounded wait: the sidecar parks descendant reads by reply token,
          // freeing the parent/sibling dispatcher until data or EOF arrives.
          const pumpsLocalChildren = hasActiveSpawnedChildren();
          const remainingMs = deadline == null
            ? KERNEL_WAIT_SLICE_MS
            : Math.max(0, deadline - Date.now());
          const waitMs = nonblocking || pumpsLocalChildren
            ? 0
            : Math.min(KERNEL_WAIT_SLICE_MS, remainingMs);
          bytes = Buffer.from(callSyncRpc('process.fd_read', [
            kernelFd,
            requestedLength,
            waitMs,
          ]) ?? []);
        } catch (error) {
          if (error?.code !== 'EAGAIN' && error?.code !== 'EWOULDBLOCK') {
            throw error;
          }
          if (nonblocking) {
            throw error;
          }
          if (deadline != null && Date.now() >= deadline) {
            const timeout = new Error(
              'blocking file descriptor read timed out; raise limits.resources.maxBlockingReadMs',
            );
            timeout.code = 'EAGAIN';
            throw timeout;
          }
          const progressed = pumpSpawnedChildren(SPAWNED_CHILD_WAIT_SLICE_MS);
          dispatchPendingWasmSignals();
          if (!progressed && !hasActiveSpawnedChildren()) {
            Atomics.wait(syntheticWaitArray, 0, 0, 1);
          }
        }
      }
      const written = writeBytesToGuestIovs(iovs, iovsLen, bytes, iovRequest);
      return writeGuestUint32(nreadPtr, written);
    } catch (error) {
      traceHostProcess('fd-read-managed-error', {
        guestFd: numericFd,
        kernelFd: Number(handle.targetFd) >>> 0,
        code: error?.code,
        message: error?.message,
      });
      return mapHostProcessError(error);
    }
  }
  if (handle?.kind === 'pipe-read') {
    try {
      const requestedLength = __agentOSWasiMeasurePhase(
        'fd_read',
        'iov_scan',
        () => iovRequest.totalLength,
      );

      const pipeClosed = __agentOSWasiMeasurePhase('fd_read', 'pipe_wait', () => {
        while (handle.pipe.chunks.length === 0) {
          if (handle.pipe.writeHandleCount === 0 && handle.pipe.producers.size === 0) {
            return true;
          }

          const pumped = pumpPipeProducers(handle.pipe, 10);
          if (!pumped) {
            Atomics.wait(syntheticWaitArray, 0, 0, 10);
          }
        }
        return false;
      });
      if (pipeClosed) {
        return __agentOSWasiMeasurePhase('fd_read', 'result_marshal', () =>
          writeGuestUint32(nreadPtr, 0)
        );
      }

      const chunk = __agentOSWasiMeasurePhase('fd_read', 'host_io', () =>
        dequeuePipeBytes(handle.pipe, requestedLength)
      );
      const written = __agentOSWasiMeasurePhase('fd_read', 'guest_iov_write', () =>
        writeBytesToGuestIovs(iovs, iovsLen, chunk, iovRequest)
      );
      return __agentOSWasiMeasurePhase('fd_read', 'result_marshal', () =>
        writeGuestUint32(nreadPtr, written)
      );
    } catch {
      return WASI_ERRNO_FAULT;
    }
  }

  if (handle?.kind === 'guest-file') {
    try {
      const requestedLength = boundedWasmSyncRpcReadLength(
        __agentOSWasiMeasurePhase('fd_read', 'iov_scan', () => iovRequest.totalLength),
      );
      const buffer = Buffer.alloc(requestedLength);
      const bytesRead = __agentOSWasiMeasurePhase('fd_read', 'host_io', () =>
        fsModule.readSync(
          handle.targetFd,
          buffer,
          0,
          requestedLength,
          handle.position ?? 0,
        )
      );
      handle.position = (handle.position ?? 0) + bytesRead;
      const written = __agentOSWasiMeasurePhase('fd_read', 'guest_iov_write', () =>
        writeBytesToGuestIovs(
          iovs,
          iovsLen,
          buffer.subarray(0, bytesRead),
          iovRequest,
        )
      );
      return __agentOSWasiMeasurePhase('fd_read', 'result_marshal', () =>
        writeGuestUint32(nreadPtr, written)
      );
    } catch {
      return WASI_ERRNO_FAULT;
    }
  }

  if (handle?.kind === 'passthrough' && typeof handle.ioFd === 'number') {
    try {
      const requestedLength = boundedWasmSyncRpcReadLength(
        __agentOSWasiMeasurePhase('fd_read', 'iov_scan', () => iovRequest.totalLength),
      );
      const buffer = Buffer.alloc(requestedLength);
      const bytesRead = __agentOSWasiMeasurePhase('fd_read', 'host_io', () =>
        fsModule.readSync(
          handle.ioFd,
          buffer,
          0,
          requestedLength,
          handle.position ?? 0,
        )
      );
      handle.position = (handle.position ?? 0) + bytesRead;
      const written = __agentOSWasiMeasurePhase('fd_read', 'guest_iov_write', () =>
        writeBytesToGuestIovs(
          iovs,
          iovsLen,
          buffer.subarray(0, bytesRead),
          iovRequest,
        )
      );
      return __agentOSWasiMeasurePhase('fd_read', 'result_marshal', () =>
        writeGuestUint32(nreadPtr, written)
      );
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  }

  if (
    handle?.kind === 'passthrough' &&
    handle.targetFd === 0
  ) {
    // dup(2) aliases share the same open file description as fd 0. In a
    // sidecar-managed process they must therefore read the kernel stdin pipe,
    // not the runner process's unrelated host stdin. OpenSSH duplicates stdin
    // before its poll/read loop, so splitting these paths loses pipe EOF.
    // https://man7.org/linux/man-pages/man2/dup.2.html
    const sidecarManagedProcess = SIDECAR_MANAGED_PROCESS;
    if (sidecarManagedProcess || KERNEL_STDIO_SYNC_RPC) {
      try {
        const requestedLength = __agentOSWasiMeasurePhase(
          'fd_read',
          'iov_scan',
          () => iovRequest.totalLength,
        );
        const nonblocking =
          ((delegatedWasiFdFlags.get(numericFd) ?? 0) & WASI_FDFLAGS_NONBLOCK) !== 0;
        const chunk = __agentOSWasiMeasurePhase('fd_read', 'kernel_stdin_read', () =>
          readKernelStdinChunk(requestedLength, nonblocking)
        );
        if (chunk === KERNEL_STDIN_WOULD_BLOCK) {
          return WASI_ERRNO_AGAIN;
        }
        if (!chunk || chunk.length === 0) {
          return __agentOSWasiMeasurePhase('fd_read', 'result_marshal', () =>
            writeGuestUint32(nreadPtr, 0)
          );
        }
        const written = __agentOSWasiMeasurePhase('fd_read', 'guest_iov_write', () =>
          writeBytesToGuestIovs(iovs, iovsLen, chunk, iovRequest)
        );
        return __agentOSWasiMeasurePhase('fd_read', 'result_marshal', () =>
          writeGuestUint32(nreadPtr, written)
        );
      } catch {
        return WASI_ERRNO_FAULT;
      }
    }
  }

  if (!handle && numericFd <= 2) {
    return WASI_ERRNO_BADF;
  }

  if (handle?.kind === 'passthrough') {
    if (!delegateManagedFdRead) {
      return WASI_ERRNO_BADF;
    }
    const result = __agentOSWasiMeasurePhase('fd_read', 'delegate_call', () =>
      delegateManagedFdRead(handle.targetFd, iovs, iovsLen, nreadPtr)
    );
    return result;
  }

  if (rejectClosedPassthroughFd(numericFd)) {
    traceHostProcess('fd-read-closed', { guestFd: numericFd, handleKind: handle?.kind ?? null });
    return WASI_ERRNO_BADF;
  }

  traceHostProcess('fd-read-delegate', { guestFd: numericFd, handleKind: handle?.kind ?? null });
  return delegateManagedFdRead
    ? __agentOSWasiMeasurePhase('fd_read', 'delegate_call', () =>
        delegateManagedFdRead(numericFd, iovs, iovsLen, nreadPtr)
      )
    : WASI_ERRNO_BADF;
};

wasiImport.fd_readdir = (fd, bufPtr, bufLen, cookie, bufUsedPtr) => {
  const bufferLength = Number(bufLen) >>> 0;
  if (!guestRangesAreValid([bufPtr, bufferLength], [bufUsedPtr, 4])) {
    return WASI_ERRNO_FAULT;
  }
  const numericFd = Number(fd) >>> 0;
  const handle = lookupFdHandle(numericFd);
  if (handle?.kind === 'kernel-fd') {
    const bufferOffset = Number(bufPtr) >>> 0;
    const memoryBytes = new Uint8Array(instanceMemory.buffer);
    const zeroResult = writeGuestUint32(bufUsedPtr, 0);
    if (zeroResult !== WASI_ERRNO_SUCCESS || bufferLength === 0) {
      return zeroResult;
    }

    try {
      const numericCookie = BigInt(cookie);
      const maxEntries = Math.min(
        4096,
        Math.max(1, Math.floor(bufferLength / 24) + 1),
      );
      const entries = callSyncRpc('process.fd_readdir', [
        Number(handle.targetFd) >>> 0,
        numericCookie.toString(),
        maxEntries,
      ]);
      if (!Array.isArray(entries)) {
        return WASI_ERRNO_IO;
      }

      let bytesUsed = 0;
      for (const entry of entries) {
        const nameBytes = Buffer.from(String(entry?.name ?? ''), 'utf8');
        const record = new Uint8Array(24 + nameBytes.length);
        const recordView = new DataView(
          record.buffer,
          record.byteOffset,
          record.byteLength,
        );
        recordView.setBigUint64(0, BigInt(entry?.next ?? 0), true);
        recordView.setBigUint64(8, BigInt(entry?.ino ?? 0), true);
        recordView.setUint32(16, nameBytes.length, true);
        recordView.setUint8(20, Number(entry?.filetype) >>> 0);
        record.set(nameBytes, 24);

        const writable = Math.min(record.length, bufferLength - bytesUsed);
        memoryBytes.set(record.subarray(0, writable), bufferOffset + bytesUsed);
        bytesUsed += writable;
        if (writable < record.length || bytesUsed === bufferLength) {
          break;
        }
      }
      return writeGuestUint32(bufUsedPtr, bytesUsed);
    } catch (error) {
      return mapHostProcessError(error);
    }
  }

  if (handle?.kind === 'passthrough') {
    return delegateManagedFdReaddir
      ? delegateManagedFdReaddir(
          handle.targetFd,
          bufPtr,
          bufLen,
          cookie,
          bufUsedPtr,
        )
      : WASI_ERRNO_BADF;
  }
  if (rejectClosedPassthroughFd(numericFd)) {
    return WASI_ERRNO_BADF;
  }
  return delegateManagedFdReaddir
    ? delegateManagedFdReaddir(numericFd, bufPtr, bufLen, cookie, bufUsedPtr)
    : WASI_ERRNO_BADF;
};

wasiImport.fd_pread = (fd, iovs, iovsLen, offset, nreadPtr) => {
  const iovRequest = validateGuestIovRequest(iovs, iovsLen);
  if (iovRequest.errno !== WASI_ERRNO_SUCCESS) return iovRequest.errno;
  if (!guestRangeIsValid(nreadPtr, 4)) return WASI_ERRNO_FAULT;
  const handle = lookupFdHandle(fd);
  if (handle?.kind === 'kernel-fd') {
    try {
      const requestedLength = boundedWasmSyncRpcReadLength(
        guestIovByteLength(iovs, iovsLen, iovRequest),
      );
      const bytes = Buffer.from(callSyncRpc('process.fd_pread', [
        Number(handle.targetFd) >>> 0,
        requestedLength,
        BigInt(offset).toString(),
      ]) ?? []);
      return writeGuestUint32(
        nreadPtr,
        writeBytesToGuestIovs(iovs, iovsLen, bytes, iovRequest),
      );
    } catch (error) {
      return mapHostProcessError(error);
    }
  }
  // Managed guests must never fall through to Node-WASI's process-local file
  // descriptors. All production positioned I/O is kernel descriptor I/O.
  if (SIDECAR_MANAGED_PROCESS) return WASI_ERRNO_BADF;
  if (!SIDECAR_MANAGED_PROCESS && handle?.kind === 'guest-file') {
    try {
      const requestedLength = boundedWasmSyncRpcReadLength(
        (() => {
          if (!(instanceMemory instanceof WebAssembly.Memory)) {
            return 0;
          }
          return iovRequest.totalLength;
        })(),
      );
      const buffer = Buffer.alloc(requestedLength);
      const bytesRead = fsModule.readSync(
        handle.targetFd,
        buffer,
        0,
        requestedLength,
        Number(offset),
      );
      const written = writeBytesToGuestIovs(
        iovs,
        iovsLen,
        buffer.subarray(0, bytesRead),
        iovRequest,
      );
      return writeGuestUint32(nreadPtr, written);
    } catch {
      return WASI_ERRNO_FAULT;
    }
  }

  if (handle?.kind === 'passthrough') {
    if (!SIDECAR_MANAGED_PROCESS && typeof handle.ioFd === 'number') {
      try {
        const requestedLength = boundedWasmSyncRpcReadLength(
          (() => {
            if (!(instanceMemory instanceof WebAssembly.Memory)) {
              return 0;
            }
            return iovRequest.totalLength;
          })(),
        );
        const buffer = Buffer.alloc(requestedLength);
        const bytesRead = fsModule.readSync(
          handle.ioFd,
          buffer,
          0,
          requestedLength,
          Number(offset),
        );
        const written = writeBytesToGuestIovs(
          iovs,
          iovsLen,
          buffer.subarray(0, bytesRead),
          iovRequest,
        );
        return writeGuestUint32(nreadPtr, written);
      } catch (error) {
        return mapSyntheticFsError(error);
      }
    }
    if (!delegateFdPread) {
      return WASI_ERRNO_BADF;
    }
    return delegateFdPread(handle.targetFd, iovs, iovsLen, offset, nreadPtr);
  }

  if (rejectClosedPassthroughFd(fd)) {
    return WASI_ERRNO_BADF;
  }

  return delegateFdPread
    ? delegateFdPread(fd, iovs, iovsLen, offset, nreadPtr)
    : WASI_ERRNO_BADF;
};

wasiImport.fd_pwrite = (fd, iovs, iovsLen, offset, nwrittenPtr) => {
  const iovRequest = validateGuestIovRequest(iovs, iovsLen);
  if (iovRequest.errno !== WASI_ERRNO_SUCCESS) return iovRequest.errno;
  if (!guestRangeIsValid(nwrittenPtr, 4)) return WASI_ERRNO_FAULT;
  const handle = lookupFdHandle(fd);
  if (handle?.kind === 'kernel-fd') {
    try {
      const bytes = collectGuestIovBytes(iovs, iovsLen, iovRequest);
      const written = Number(callSyncRpc('process.fd_pwrite', [
        Number(handle.targetFd) >>> 0,
        bytes,
        BigInt(offset).toString(),
      ]));
      if (!Number.isSafeInteger(written) || written < 0 || written > bytes.length) {
        return WASI_ERRNO_IO;
      }
      return writeGuestUint32(nwrittenPtr, written);
    } catch (error) {
      return mapHostProcessError(error);
    }
  }
  // Managed guests must never fall through to Node-WASI's process-local file
  // descriptors. All production positioned I/O is kernel descriptor I/O.
  if (SIDECAR_MANAGED_PROCESS) return WASI_ERRNO_BADF;
  if (!SIDECAR_MANAGED_PROCESS && handle?.kind === 'guest-file') {
    if (handle.readOnly === true) {
      return WASI_ERRNO_ROFS;
    }
    try {
      const bytes = collectGuestIovBytes(iovs, iovsLen, iovRequest);
      const written = fsModule.writeSync(
        handle.targetFd,
        bytes,
        0,
        bytes.length,
        Number(offset),
      );
      return writeGuestUint32(nwrittenPtr, written);
    } catch {
      return WASI_ERRNO_FAULT;
    }
  }

  if (handle?.kind === 'passthrough') {
    if (handle.readOnly === true) {
      return WASI_ERRNO_ROFS;
    }
    if (!SIDECAR_MANAGED_PROCESS && typeof handle.ioFd === 'number') {
      try {
        const bytes = collectGuestIovBytes(iovs, iovsLen, iovRequest);
        const written = fsModule.writeSync(
          handle.ioFd,
          bytes,
          0,
          bytes.length,
          Number(offset),
        );
        // A positioned write can grow the file past a size remembered from a
        // prior truncate; drop the stale entry so fd_size/path_size fall
        // through to the authoritative fstat.
        return writeGuestUint32(nwrittenPtr, written);
      } catch (error) {
        return mapSyntheticFsError(error);
      }
    }
    return delegateManagedFdPwrite
      ? delegateManagedFdPwrite(handle.targetFd, iovs, iovsLen, offset, nwrittenPtr)
      : WASI_ERRNO_BADF;
  }

  if (rejectClosedPassthroughFd(fd)) {
    return WASI_ERRNO_BADF;
  }

  return delegateManagedFdPwrite
    ? delegateManagedFdPwrite(fd, iovs, iovsLen, offset, nwrittenPtr)
    : WASI_ERRNO_BADF;
};

wasiImport.fd_sync = (fd) => {
  const handle = lookupFdHandle(fd);
  if (handle?.kind === 'kernel-fd') {
    try {
      callSyncRpc('process.fd_sync', [Number(handle.targetFd) >>> 0]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapHostProcessError(error);
    }
  }
  if (handle?.kind === 'guest-file') {
    return WASI_ERRNO_SUCCESS;
  }

  if (handle?.kind === 'passthrough') {
    return delegateFdSync ? delegateFdSync(handle.targetFd) : WASI_ERRNO_SUCCESS;
  }

  if (rejectClosedPassthroughFd(fd)) {
    return WASI_ERRNO_BADF;
  }

  return delegateFdSync ? delegateFdSync(fd) : WASI_ERRNO_SUCCESS;
};

wasiImport.fd_datasync = (fd) => {
  const handle = lookupFdHandle(fd);
  if (handle?.kind === 'kernel-fd') {
    try {
      callSyncRpc('process.fd_datasync', [Number(handle.targetFd) >>> 0]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapHostProcessError(error);
    }
  }
  if (handle?.kind === 'guest-file') {
    return WASI_ERRNO_SUCCESS;
  }
  if (handle?.kind === 'passthrough') {
    return delegateFdDatasync
      ? delegateFdDatasync(handle.targetFd)
      : delegateFdSync
        ? delegateFdSync(handle.targetFd)
        : WASI_ERRNO_SUCCESS;
  }
  if (rejectClosedPassthroughFd(fd)) {
    return WASI_ERRNO_BADF;
  }
  return delegateFdDatasync
    ? delegateFdDatasync(fd)
    : delegateFdSync
      ? delegateFdSync(fd)
      : WASI_ERRNO_SUCCESS;
};

wasiImport.fd_seek = (fd, offset, whence, newOffsetPtr) => {
  if (!guestRangeIsValid(newOffsetPtr, 8)) return WASI_ERRNO_FAULT;
  const handle = lookupFdHandle(fd);
  if (handle?.kind === 'kernel-fd') {
    try {
      const next = callSyncRpc('process.fd_seek', [
        Number(handle.targetFd) >>> 0,
        BigInt(offset).toString(),
        Number(whence) >>> 0,
      ]);
      return writeGuestUint64(newOffsetPtr, BigInt(next));
    } catch (error) {
      return mapHostProcessError(error);
    }
  }
  if (handle?.kind === 'guest-file') {
    try {
      const next = seekGuestFileHandle(handle, offset, whence);
      if (next == null) {
        return WASI_ERRNO_INVAL;
      }
      return writeGuestUint64(newOffsetPtr, next);
    } catch {
      return WASI_ERRNO_FAULT;
    }
  }

  if (handle && handle.kind !== 'passthrough') {
    return WASI_ERRNO_SPIPE;
  }

  if (handle?.kind === 'passthrough') {
    if (typeof handle.ioFd === 'number') {
      try {
        const next = seekGuestFileHandle(handle, offset, whence);
        if (next == null) {
          return WASI_ERRNO_INVAL;
        }
        return writeGuestUint64(newOffsetPtr, next);
      } catch {
        return WASI_ERRNO_FAULT;
      }
    }
    return delegateManagedFdSeek
      ? delegateManagedFdSeek(handle.targetFd, offset, whence, newOffsetPtr)
      : WASI_ERRNO_BADF;
  }

  if (rejectClosedPassthroughFd(fd)) {
    return WASI_ERRNO_BADF;
  }

  return delegateManagedFdSeek
    ? delegateManagedFdSeek(fd, offset, whence, newOffsetPtr)
    : WASI_ERRNO_BADF;
};

wasiImport.fd_tell = (fd, offsetPtr) => {
  if (!guestRangeIsValid(offsetPtr, 8)) return WASI_ERRNO_FAULT;
  const handle = lookupFdHandle(fd);
  if (handle?.kind === 'kernel-fd') {
    try {
      const next = callSyncRpc('process.fd_seek', [
        Number(handle.targetFd) >>> 0,
        '0',
        WASI_WHENCE_CUR,
      ]);
      return writeGuestUint64(offsetPtr, BigInt(next));
    } catch (error) {
      return mapHostProcessError(error);
    }
  }
  if (handle?.kind === 'guest-file') {
    return writeGuestUint64(offsetPtr, BigInt(handle.position ?? 0));
  }

  if (handle && handle.kind !== 'passthrough') {
    return WASI_ERRNO_SPIPE;
  }

  if (handle?.kind === 'passthrough') {
    if (typeof handle.ioFd === 'number') {
      return writeGuestUint64(offsetPtr, BigInt(handle.position ?? 0));
    }
    return delegateManagedFdTell
      ? delegateManagedFdTell(handle.targetFd, offsetPtr)
      : WASI_ERRNO_BADF;
  }

  if (rejectClosedPassthroughFd(fd)) {
    return WASI_ERRNO_BADF;
  }

  return delegateManagedFdTell
    ? delegateManagedFdTell(fd, offsetPtr)
    : WASI_ERRNO_BADF;
};

wasiImport.fd_fdstat_get = (fd, statPtr) => {
  if (!guestRangeIsValid(statPtr, 24)) return WASI_ERRNO_FAULT;
  const handle = __agentOSWasiMeasurePhase('fd_fdstat_get', 'lookup_handle', () =>
    lookupFdHandle(fd)
  );
  if (handle?.kind === 'kernel-fd') {
    try {
      const stat = callSyncRpc('process.fd_stat', [Number(handle.targetFd) >>> 0]);
      const kernelFlags = Number(stat?.flags) >>> 0;
      const wasiFlags = (kernelFlags & KERNEL_O_APPEND ? WASI_FDFLAGS_APPEND : 0)
        | (kernelFlags & KERNEL_O_NONBLOCK ? WASI_FDFLAGS_NONBLOCK : 0);
      return writeGuestFdstat(
        statPtr,
        Number(stat?.filetype) >>> 0,
        wasiFlags,
        BigInt(stat?.rightsBase ?? 0),
        BigInt(stat?.rightsInheriting ?? 0),
      );
    } catch (error) {
      return mapHostProcessError(error);
    }
  }
  // Host-net sockets (curl/wget/git TLS transports): report a stream-socket
  // fdstat with the current O_NONBLOCK state so guest fcntl(F_GETFL) works.
  // Without this, fcntl-based non-blocking setup fails with EBADF and guests
  // that expect EAGAIN semantics (libcurl mid-upload reads) block forever.
  {
    const hostNetSocket = getHostNetSocket(fd);
    if (!SIDECAR_MANAGED_PROCESS && hostNetSocket && !hostNetSocket.closed) {
      return writeGuestFdstat(
        statPtr,
        WASI_FILETYPE_SOCKET_STREAM,
        hostNetSocket.nonblock ? WASI_FDFLAGS_NONBLOCK : 0,
        WASI_RIGHT_FD_READ |
          WASI_RIGHT_FD_WRITE |
          WASI_RIGHT_FD_FDSTAT_SET_FLAGS |
          WASI_RIGHT_FD_FILESTAT_GET |
          WASI_RIGHT_POLL_FD_READWRITE,
        0n,
      );
    }
  }
  // Kernel-PTY stdio must report CHARACTER_DEVICE so guest is_terminal()/
  // isatty() see the TTY (the runner-process fds behind the delegate are
  // pipes). Resolve dup'd passthrough handles to their target fd first.
  {
    const stdioFd =
      handle?.kind === 'passthrough' ? Number(handle.targetFd) >>> 0 : Number(fd) >>> 0;
    if ((handle == null || handle.kind === 'passthrough') && stdioFdIsKernelTty(stdioFd)) {
      return __agentOSWasiMeasurePhase(
        'fd_fdstat_get',
        'marshal_fdstat',
        () => writeGuestFdstat(
          statPtr,
          WASI_FILETYPE_CHARACTER_DEVICE,
          0,
          WASI_RIGHT_FD_READ |
            WASI_RIGHT_FD_WRITE |
            WASI_RIGHT_FD_FDSTAT_SET_FLAGS |
            WASI_RIGHT_FD_FILESTAT_GET |
            WASI_RIGHT_POLL_FD_READWRITE,
          0n,
        ),
      );
    }
  }
  if (handle?.kind === 'pipe-read') {
    return __agentOSWasiMeasurePhase(
      'fd_fdstat_get',
      'marshal_fdstat',
      () => writeGuestFdstat(
        statPtr,
        WASI_FILETYPE_UNKNOWN,
        0,
        WASI_RIGHT_FD_READ |
          WASI_RIGHT_FD_FDSTAT_SET_FLAGS |
          WASI_RIGHT_FD_FILESTAT_GET |
          WASI_RIGHT_POLL_FD_READWRITE,
        0n,
      ),
    );
  }

  if (handle?.kind === 'pipe-write') {
    return __agentOSWasiMeasurePhase(
      'fd_fdstat_get',
      'marshal_fdstat',
      () => writeGuestFdstat(
        statPtr,
        WASI_FILETYPE_UNKNOWN,
        0,
        WASI_RIGHT_FD_WRITE |
          WASI_RIGHT_FD_FDSTAT_SET_FLAGS |
          WASI_RIGHT_FD_FILESTAT_GET |
          WASI_RIGHT_POLL_FD_READWRITE,
        0n,
      ),
    );
  }

  if (handle?.kind === 'guest-file') {
    try {
      const stat = fsModule.fstatSync(handle.targetFd);
      return writeGuestFdstat(
        statPtr,
        wasiFiletypeFromStats(stat),
        0,
        WASI_RIGHT_FD_READ |
          WASI_RIGHT_FD_SEEK |
          WASI_RIGHT_FD_TELL |
          WASI_RIGHT_FD_FILESTAT_GET |
          WASI_RIGHT_FD_WRITE |
          WASI_RIGHT_FD_SYNC,
        0n,
      );
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  }

  if (handle && handle.kind !== 'passthrough') {
    return WASI_ERRNO_BADF;
  }

  if (handle?.kind === 'passthrough') {
    if (typeof handle.ioFd === 'number') {
      try {
        const stat = fsModule.fstatSync(handle.ioFd);
        return writeGuestFdstat(
          statPtr,
          wasiFiletypeFromStats(stat),
          0,
          WASI_RIGHT_FD_READ |
            WASI_RIGHT_FD_SEEK |
            WASI_RIGHT_FD_TELL |
            WASI_RIGHT_FD_FILESTAT_GET |
            WASI_RIGHT_FD_WRITE |
            WASI_RIGHT_FD_SYNC,
          0n,
        );
      } catch (error) {
        return mapSyntheticFsError(error);
      }
    }
    return delegateManagedFdFdstatGet
      ? __agentOSWasiMeasurePhase('fd_fdstat_get', 'delegate_call', () =>
          delegateManagedFdFdstatGet(handle.targetFd, statPtr)
        )
      : WASI_ERRNO_BADF;
  }

  if (rejectClosedPassthroughFd(fd)) {
    return WASI_ERRNO_BADF;
  }

  return delegateManagedFdFdstatGet
    ? __agentOSWasiMeasurePhase('fd_fdstat_get', 'delegate_call', () =>
        delegateManagedFdFdstatGet(fd, statPtr)
      )
    : WASI_ERRNO_BADF;
};

wasiImport.fd_fdstat_set_flags = (fd, flags) => {
  const handle = lookupFdHandle(fd);
  if (handle?.kind === 'kernel-fd') {
    try {
      const wasiFlags = Number(flags) >>> 0;
      const kernelFlags = (wasiFlags & WASI_FDFLAGS_APPEND ? KERNEL_O_APPEND : 0)
        | (wasiFlags & WASI_FDFLAGS_NONBLOCK ? KERNEL_O_NONBLOCK : 0);
      callSyncRpc('process.fd_set_flags', [Number(handle.targetFd) >>> 0, kernelFlags]);
      const hostNetSocket = getHostNetSocket(fd);
      if (hostNetSocket && !hostNetSocket.closed) {
        hostNetSocket.nonblock = (wasiFlags & WASI_FDFLAGS_NONBLOCK) !== 0;
      }
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapHostProcessError(error);
    }
  }
  // Host-net sockets: honor O_NONBLOCK (guest fcntl F_SETFL). net_recv/net_send
  // consult `socket.nonblock` to return EAGAIN instead of blocking, which
  // non-blocking clients like libcurl rely on to interleave send/recv.
  {
    const hostNetSocket = getHostNetSocket(fd);
    if (!SIDECAR_MANAGED_PROCESS && hostNetSocket && !hostNetSocket.closed) {
      hostNetSocket.nonblock = (Number(flags) & WASI_FDFLAGS_NONBLOCK) !== 0;
      return WASI_ERRNO_SUCCESS;
    }
  }
  if (handle && handle.kind !== 'passthrough') {
    return WASI_ERRNO_BADF;
  }

  if (handle?.kind === 'passthrough') {
    const result = delegateManagedFdFdstatSetFlags
      ? delegateManagedFdFdstatSetFlags(handle.targetFd, flags)
      : WASI_ERRNO_BADF;
    if (result === WASI_ERRNO_SUCCESS) {
      delegatedWasiFdFlags.set(Number(fd) >>> 0, Number(flags) >>> 0);
    }
    return result;
  }

  if (rejectClosedPassthroughFd(fd)) {
    return WASI_ERRNO_BADF;
  }

  const result = delegateManagedFdFdstatSetFlags
    ? delegateManagedFdFdstatSetFlags(fd, flags)
    : WASI_ERRNO_BADF;
  if (result === WASI_ERRNO_SUCCESS) {
    delegatedWasiFdFlags.set(Number(fd) >>> 0, Number(flags) >>> 0);
  }
  return result;
};

wasiImport.fd_filestat_get = (fd, statPtr) => {
  if (!guestRangeIsValid(statPtr, 64)) return WASI_ERRNO_FAULT;
  const handle = lookupFdHandle(fd);
  if (handle?.kind === 'kernel-fd') {
    try {
      const stat = callSyncRpc('process.fd_filestat', [Number(handle.targetFd) >>> 0]);
      return writeGuestFilestat(statPtr, stat, Number(stat?.filetype) >>> 0);
    } catch (error) {
      return mapHostProcessError(error);
    }
  }
  if (handle?.kind === 'guest-file') {
    try {
      return writeGuestFilestat(statPtr, fsModule.fstatSync(handle.targetFd));
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  }
  if (handle?.kind === 'pipe-read' || handle?.kind === 'pipe-write') {
    // WASI preview1 has no distinct FIFO filetype value. Preserve the same
    // unknown-filetype contract used by fd_fdstat_get while still returning a
    // stable identity for aliases of one pipe, as Linux fstat(2) does.
    return writeGuestFilestat(
      statPtr,
      { ino: handle.pipe.id, nlink: 1, size: 0 },
      WASI_FILETYPE_UNKNOWN,
    );
  }

  if (handle?.kind === 'passthrough') {
    if (typeof handle.ioFd === 'number') {
      try {
        return writeGuestFilestat(statPtr, fsModule.fstatSync(handle.ioFd));
      } catch (error) {
        return mapSyntheticFsError(error);
      }
    }
    if (!delegateManagedFdFilestatGet) {
      return WASI_ERRNO_BADF;
    }
    return delegateManagedFdFilestatGet(handle.targetFd, statPtr);
  }

  // Guest stdio always starts with an explicit mirror. If that mirror is
  // absent, the guest descriptor was closed or renumbered; do not expose the
  // runner's private Node-WASI bootstrap descriptor at the same number.
  if (!handle && (Number(fd) >>> 0) <= 2) {
    return WASI_ERRNO_BADF;
  }

  if (rejectClosedPassthroughFd(fd)) {
    return WASI_ERRNO_BADF;
  }

  return delegateManagedFdFilestatGet
    ? delegateManagedFdFilestatGet(fd, statPtr)
    : WASI_ERRNO_BADF;
};

wasiImport.fd_filestat_set_size = (fd, size) => {
  const handle = lookupFdHandle(fd);
  if (handle?.kind === 'kernel-fd') {
    try {
      callSyncRpc('process.fd_truncate', [
        Number(handle.targetFd) >>> 0,
        BigInt(size).toString(),
      ]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapHostProcessError(error);
    }
  }
  if (handle?.kind === 'guest-file') {
    try {
      const nextSize = Number(size);
      fsModule.ftruncateSync(handle.targetFd, nextSize);
      if ((handle.position ?? 0) > nextSize) {
        handle.position = nextSize;
      }
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapSyntheticFsError(error);
    }
  }

  if (handle?.kind === 'passthrough') {
    if (handle.readOnly === true) {
      return WASI_ERRNO_ROFS;
    }
    if (typeof handle.ioFd === 'number') {
      try {
        const nextSize = Number(size);
        fsModule.ftruncateSync(handle.ioFd, nextSize);
        if ((handle.position ?? 0) > nextSize) {
          handle.position = nextSize;
        }
        return WASI_ERRNO_SUCCESS;
      } catch (error) {
        return mapSyntheticFsError(error);
      }
    }
    if (typeof handle.guestPath === 'string') {
      try {
        const nextSize = Number(size);
        const pathFd = fsModule.openSync(handle.guestPath, 0o1, 0o666);
        try {
          fsModule.ftruncateSync(pathFd, nextSize);
          if ((handle.position ?? 0) > nextSize) {
            handle.position = nextSize;
          }
        } finally {
          fsModule.closeSync(pathFd);
        }
        return WASI_ERRNO_SUCCESS;
      } catch (error) {
        return mapSyntheticFsError(error);
      }
    }
    return delegateManagedFdFilestatSetSize
      ? delegateManagedFdFilestatSetSize(handle.targetFd, size)
      : WASI_ERRNO_BADF;
  }

  if (rejectClosedPassthroughFd(fd)) {
    return WASI_ERRNO_BADF;
  }

  return delegateManagedFdFilestatSetSize
    ? delegateManagedFdFilestatSetSize(fd, size)
    : WASI_ERRNO_BADF;
};

wasiImport.fd_prestat_get = (fd, prestatPtr) => {
  if (!guestRangeIsValid(prestatPtr, 8)) {
    return WASI_ERRNO_FAULT;
  }
  const handle = lookupFdHandle(fd);
  if (SIDECAR_MANAGED_PROCESS) {
    if (handle?.kind !== 'kernel-fd') return WASI_ERRNO_BADF;
    try {
      const preopen = callSyncRpc('process.fd_preopen', [
        Number(handle.targetFd) >>> 0,
      ]);
      if (!preopen || typeof preopen.guestPath !== 'string') {
        return WASI_ERRNO_BADF;
      }
      const view = new DataView(instanceMemory.buffer);
      const offset = Number(prestatPtr) >>> 0;
      view.setUint8(offset, 0);
      view.setUint32(offset + 4, Buffer.byteLength(preopen.guestPath), true);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapHostProcessError(error);
    }
  }
  if (handle && handle.kind !== 'passthrough') {
    return WASI_ERRNO_BADF;
  }

  if (handle?.kind === 'passthrough') {
    return delegateManagedFdPrestatGet
      ? delegateManagedFdPrestatGet(handle.targetFd, prestatPtr)
      : WASI_ERRNO_BADF;
  }

  if (rejectClosedPassthroughFd(fd)) {
    return WASI_ERRNO_BADF;
  }

  return delegateManagedFdPrestatGet
    ? delegateManagedFdPrestatGet(fd, prestatPtr)
    : WASI_ERRNO_BADF;
};

wasiImport.fd_prestat_dir_name = (fd, pathPtr, pathLen) => {
  const outputLength = Number(pathLen) >>> 0;
  if (!guestRangeIsValid(pathPtr, outputLength)) {
    return WASI_ERRNO_FAULT;
  }
  const handle = lookupFdHandle(fd);
  if (SIDECAR_MANAGED_PROCESS) {
    if (handle?.kind !== 'kernel-fd') return WASI_ERRNO_BADF;
    try {
      const preopen = callSyncRpc('process.fd_preopen', [
        Number(handle.targetFd) >>> 0,
      ]);
      if (!preopen || typeof preopen.guestPath !== 'string') {
        return WASI_ERRNO_BADF;
      }
      const bytes = Buffer.from(preopen.guestPath, 'utf8');
      if (outputLength < bytes.length) return WASI_ERRNO_NAMETOOLONG;
      new Uint8Array(instanceMemory.buffer, Number(pathPtr), bytes.length).set(bytes);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapHostProcessError(error);
    }
  }
  if (handle && handle.kind !== 'passthrough') {
    return WASI_ERRNO_BADF;
  }

  if (handle?.kind === 'passthrough') {
    return delegateManagedFdPrestatDirName
      ? delegateManagedFdPrestatDirName(handle.targetFd, pathPtr, pathLen)
      : WASI_ERRNO_BADF;
  }

  if (rejectClosedPassthroughFd(fd)) {
    return WASI_ERRNO_BADF;
  }

  return delegateManagedFdPrestatDirName
    ? delegateManagedFdPrestatDirName(fd, pathPtr, pathLen)
    : WASI_ERRNO_BADF;
};

function writeKernelFdCooperatively(targetFd, bytes) {
  while (true) {
    try {
      return Number(callSyncRpc('process.fd_write', [targetFd, bytes]));
    } catch (error) {
      if (error?.code !== 'EAGAIN' && error?.code !== 'EWOULDBLOCK') {
        throw error;
      }
      const stat = callSyncRpc('process.fd_stat', [targetFd]);
      if ((Number(stat?.flags) & KERNEL_O_NONBLOCK) !== 0) {
        throw error;
      }
      // The sidecar deliberately attempts kernel-pipe writes without blocking
      // its dispatcher. Wait cooperatively so sibling/descendant reads can be
      // serviced on that dispatcher, then retry Linux's logically blocking
      // write. Guest O_NONBLOCK descriptors still return EAGAIN above.
      dispatchPendingWasmSignals();
      pumpSpawnedChildren(0);
      callSyncRpc('__kernel_poll', [
        [{ fd: targetFd, events: KERNEL_POLLOUT }],
        KERNEL_WAIT_SLICE_MS,
      ]);
    }
  }
}

function kernelFdStdioStream(targetFd, descriptorPath) {
  if (descriptorPath === '/dev/stdout') return 'stdout';
  if (descriptorPath === '/dev/stderr') return 'stderr';

  // PTY descriptions retain their /dev/pts/... path. Match a duplicated
  // terminal descriptor against canonical fd 1/2 by the kernel's open-file-
  // description identity; isatty alone would incorrectly classify an
  // unrelated PTY opened by the guest as host stdio.
  let isTty = false;
  try {
    isTty = callSyncRpc('__kernel_isatty', [targetFd]) === true;
  } catch (error) {
    // Anonymous kernel objects (notably socketpairs) have no terminal
    // identity. Treat the kernel's "not a terminal/path-backed fd" answers as
    // a negative probe; the descriptor remains a valid ordinary I/O target.
    if (
      error?.code !== 'EINVAL'
      && error?.code !== 'ENOTTY'
      && error?.code !== 'ENOTSUP'
    ) {
      throw error;
    }
  }
  if (!isTty) return null;
  const targetDescription = String(
    callSyncRpc('process.fd_description_identity', [targetFd])?.descriptionId ?? '',
  );
  for (const [stdioFd, stream] of [[1, 'stdout'], [2, 'stderr']]) {
    try {
      const stdioDescription = String(
        callSyncRpc('process.fd_description_identity', [stdioFd])?.descriptionId ?? '',
      );
      if (targetDescription !== '' && targetDescription === stdioDescription) {
        return stream;
      }
    } catch (error) {
      if (error?.code !== 'EBADF') throw error;
    }
  }
  return null;
}

wasiImport.fd_write = (fd, iovs, iovsLen, nwrittenPtr) => {
  const iovRequest = validateGuestIovRequest(iovs, iovsLen);
  if (iovRequest.errno !== WASI_ERRNO_SUCCESS) {
    return iovRequest.errno;
  }
  if (!guestRangeIsValid(nwrittenPtr, 4)) {
    return WASI_ERRNO_FAULT;
  }
  const numericFd = Number(fd) >>> 0;
  const hostNetSocket = getHostNetSocket(numericFd);
  if (hostNetSocket) {
    return writeHostNetSocketFromGuestIovs(
      hostNetSocket,
      iovs,
      iovsLen,
      nwrittenPtr,
      iovRequest,
    );
  }

  const handle = __agentOSWasiMeasurePhase('fd_write', 'lookup_handle', () =>
    lookupFdHandle(fd)
  );
  if (handle?.kind === 'kernel-fd') {
    try {
      const bytes = collectGuestIovBytes(iovs, iovsLen, iovRequest);
      const kernelFd = Number(handle.targetFd) >>> 0;
      // The kernel description, not the guest fd number, decides whether an
      // alias still targets stdout/stderr. Keep the kernel as source of truth
      // so dup/fcntl aliases and 1>&2/2>&1 redirections surface through the same
      // ordered output event path as canonical fd 1/2.
      let descriptorPath = null;
      try {
        descriptorPath = String(callSyncRpc('process.fd_path', [kernelFd]));
      } catch (error) {
        // Anonymous pipes and sockets have no pathname. They are valid write
        // targets but cannot be canonical stdout/stderr path aliases.
        if (error?.code !== 'EINVAL' && error?.code !== 'ENOTSUP') throw error;
      }
      const stdioStream = kernelFdStdioStream(kernelFd, descriptorPath);
      const written = stdioStream != null
        ? Number(callSyncRpc('__kernel_stdio_write', [kernelFd, bytes])) >>> 0
        : writeKernelFdCooperatively(kernelFd, bytes);
      if (!Number.isSafeInteger(written) || written < 0 || written > bytes.length) {
        return WASI_ERRNO_FAULT;
      }
      return writeGuestUint32(nwrittenPtr, written);
    } catch (error) {
      return mapHostProcessError(error);
    }
  }
  if (handle?.kind === 'pipe-write') {
    try {
      const bytes = __agentOSWasiMeasurePhase('fd_write', 'guest_iov_collect', () =>
        collectGuestIovBytes(iovs, iovsLen, iovRequest)
      );
      if (bytes.length > 0 && !pipeHasReaders(handle.pipe)) {
        return WASI_ERRNO_PIPE;
      }
      __agentOSWasiMeasurePhase('fd_write', 'host_io', () => {
        enqueuePipeBytes(handle.pipe, bytes);
        flushPipeConsumers(handle.pipe);
      });
      return __agentOSWasiMeasurePhase('fd_write', 'result_marshal', () =>
        writeGuestUint32(nwrittenPtr, bytes.length)
      );
    } catch {
      return WASI_ERRNO_FAULT;
    }
  }

  if (handle?.kind === 'guest-file') {
    if (handle.readOnly === true) {
      return WASI_ERRNO_ROFS;
    }
    try {
      const bytes = __agentOSWasiMeasurePhase('fd_write', 'guest_iov_collect', () =>
        collectGuestIovBytes(iovs, iovsLen, iovRequest)
      );
      const written = __agentOSWasiMeasurePhase('fd_write', 'host_io', () =>
        writeBytesToGuestFileHandle(handle, bytes)
      );
      return __agentOSWasiMeasurePhase('fd_write', 'result_marshal', () =>
        writeGuestUint32(nwrittenPtr, written)
      );
    } catch {
      return WASI_ERRNO_FAULT;
    }
  }

  const passthroughStdioTarget =
    handle?.kind === 'passthrough' &&
    (Number(handle.targetFd) === 1 || Number(handle.targetFd) === 2)
      ? Number(handle.targetFd)
      : null;
  if (passthroughStdioTarget != null) {
    try {
      const bytes = __agentOSWasiMeasurePhase('fd_write', 'guest_iov_collect', () =>
        collectGuestIovBytes(iovs, iovsLen, iovRequest)
      );
      const sidecarManagedProcess = SIDECAR_MANAGED_PROCESS;
      if (sidecarManagedProcess || KERNEL_STDIO_SYNC_RPC) {
        const written = __agentOSWasiMeasurePhase('fd_write', 'sync_rpc', () =>
          Number(callSyncRpc('__kernel_stdio_write', [passthroughStdioTarget, bytes])) >>> 0
        );
        return __agentOSWasiMeasurePhase('fd_write', 'result_marshal', () =>
          writeGuestUint32(nwrittenPtr, written)
        );
      }
      __agentOSWasiMeasurePhase('fd_write', 'host_io', () =>
        (passthroughStdioTarget === 1 ? process.stdout : process.stderr).write(bytes)
      );
      return __agentOSWasiMeasurePhase('fd_write', 'result_marshal', () =>
        writeGuestUint32(nwrittenPtr, bytes.length)
      );
    } catch {
      return WASI_ERRNO_FAULT;
    }
  }

  if (handle?.kind === 'passthrough') {
    if (handle.readOnly === true) {
      return WASI_ERRNO_ROFS;
    }
    if (typeof handle.ioFd === 'number') {
      try {
        const bytes = __agentOSWasiMeasurePhase('fd_write', 'guest_iov_collect', () =>
          collectGuestIovBytes(iovs, iovsLen, iovRequest)
        );
        const written = __agentOSWasiMeasurePhase('fd_write', 'host_io', () =>
          writeBytesToGuestFileHandle({ ...handle, targetFd: handle.ioFd }, bytes)
        );
        if (handle.append) {
          handle.position = Number(fsModule.fstatSync(handle.ioFd).size ?? 0);
        } else {
          handle.position = (handle.position ?? 0) + written;
        }
        return __agentOSWasiMeasurePhase('fd_write', 'result_marshal', () =>
          writeGuestUint32(nwrittenPtr, written)
        );
      } catch (error) {
        return mapSyntheticFsError(error);
      }
    }
    return delegateManagedFdWrite
      ? __agentOSWasiMeasurePhase('fd_write', 'delegate_call', () =>
          delegateManagedFdWrite(handle.targetFd, iovs, iovsLen, nwrittenPtr)
        )
      : WASI_ERRNO_BADF;
  }

  if (!handle && numericFd <= 2) {
    return WASI_ERRNO_BADF;
  }

  if (rejectClosedPassthroughFd(fd)) {
    return WASI_ERRNO_BADF;
  }

  return delegateManagedFdWrite
    ? __agentOSWasiMeasurePhase('fd_write', 'delegate_call', () =>
        delegateManagedFdWrite(fd, iovs, iovsLen, nwrittenPtr)
      )
    : WASI_ERRNO_BADF;
};

wasiImport.fd_close = (fd) => {
  const numericFd = Number(fd) >>> 0;
  traceHostProcess('fd-close-begin', {
    fd: numericFd,
    syntheticKind: activeFdProjections.get(numericFd)?.kind ?? null,
    passthroughKind: passthroughHandles.get(numericFd)?.kind ?? null,
  });
  if (hasHostNetSocket(numericFd)) {
    const result = __agentOSWasiMeasurePhase('fd_close', 'host_socket_close', () =>
      hostNetImport.net_close(numericFd)
    );
    // net_close consumes the runner fd even when a sidecar cleanup RPC fails,
    // matching close(2)'s no-retry rule. Never let a reused fd inherit stale
    // FD_CLOEXEC state from the consumed description.
    standaloneCloexecFds.delete(numericFd);
    return result;
  }
  try {
    if (__agentOSWasiMeasurePhase('fd_close', 'synthetic_close', () => closeSyntheticFd(fd))) {
      standaloneCloexecFds.delete(numericFd);
      traceHostProcess('fd-close-synthetic', { fd: Number(fd) >>> 0 });
      return WASI_ERRNO_SUCCESS;
    }
  } catch (error) {
    return mapHostProcessError(error);
  }

  const handle = __agentOSWasiMeasurePhase('fd_close', 'lookup_handle', () =>
    lookupFdHandle(fd)
  );
  if (handle?.kind === 'kernel-fd') {
    try {
      traceHostProcess('fd-close-kernel', {
        fd: Number(fd) >>> 0,
        targetFd: handle.targetFd ?? null,
      });
      closePassthroughFd(fd);
      standaloneCloexecFds.delete(numericFd);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapHostProcessError(error);
    }
  }
  if (handle?.kind === 'passthrough') {
    traceHostProcess('fd-close-mapped', {
      fd: Number(fd) >>> 0,
      targetFd: handle.targetFd ?? null,
    });
    __agentOSWasiMeasurePhase('fd_close', 'fd_bookkeeping', () => closePassthroughFd(fd));
    standaloneCloexecFds.delete(numericFd);
    return WASI_ERRNO_SUCCESS;
  }

  if (!handle && Number(fd) >>> 0 <= 2) {
    return WASI_ERRNO_BADF;
  }

  if (rejectClosedPassthroughFd(fd)) {
    return WASI_ERRNO_BADF;
  }

  if (standaloneDelegateFdRefCounts.has(Number(fd) >>> 0)) {
    const shouldDelegateClose = __agentOSWasiMeasurePhase('fd_close', 'fd_bookkeeping', () =>
      releaseDelegateFd(fd)
    );
    traceHostProcess('fd-close-delegate-tracked', {
      fd: Number(fd) >>> 0,
      shouldDelegateClose,
      remainingRefs: standaloneDelegateFdRefCounts.get(Number(fd) >>> 0) ?? 0,
    });
    if (!shouldDelegateClose) {
      standaloneCloexecFds.delete(numericFd);
      return WASI_ERRNO_SUCCESS;
    }
    passthroughHandles.delete(Number(fd) >>> 0);
  }

  traceHostProcess('fd-close-delegate', { fd: Number(fd) >>> 0 });
  const result = delegateManagedFdClose
    ? __agentOSWasiMeasurePhase('fd_close', 'delegate_call', () =>
        delegateManagedFdClose(fd)
      )
    : WASI_ERRNO_BADF;
  if (result === WASI_ERRNO_SUCCESS) standaloneCloexecFds.delete(numericFd);
  return result;
};

wasiImport.fd_renumber = (from, to) => {
  try {
    const sourceFd = Number(from) >>> 0;
    const targetFd = Number(to) >>> 0;
    if (sourceFd >= LINUX_GUEST_FD_LIMIT || targetFd >= LINUX_GUEST_FD_LIMIT) {
      return WASI_ERRNO_BADF;
    }
    if (sourceFd === targetFd) {
      return lookupFdHandle(sourceFd) || standaloneDelegateFdRefCounts.has(sourceFd)
        ? WASI_ERRNO_SUCCESS
        : WASI_ERRNO_BADF;
    }

    const managedSource = lookupFdHandle(sourceFd);
    if (SIDECAR_MANAGED_PROCESS && managedSource?.kind === 'kernel-fd') {
      const sourceHostNetDescriptionId = managedSource.hostNetDescriptionId;
      const managedTarget = lookupFdHandle(targetFd);
      if (managedTarget && managedTarget.kind !== 'kernel-fd') {
        return WASI_ERRNO_BADF;
      }
      const shadowsInternalPreopen =
        managedTarget?.internalPreopen === true && hiddenPreopenHandles.has(targetFd);
      const movedKernelFd = callSyncRpc('process.fd_move', [
        Number(managedSource.targetFd) >>> 0,
        managedTarget?.kind === 'kernel-fd' && !shadowsInternalPreopen
          ? Number(managedTarget.targetFd) >>> 0
          : null,
      ]);
      // The kernel mutation above is atomic from the guest's perspective.
      // Only now may the runner replace its bounded identity projections.
      if (managedTarget?.kind === 'kernel-fd' && !shadowsInternalPreopen) {
        forgetSidecarClosedKernelFd(targetFd);
      }
      forgetSidecarClosedKernelFd(sourceFd);
      const movedGuestFd = registerKernelDelegateFd(
        movedKernelFd,
        targetFd,
        3,
        shadowsInternalPreopen,
      );
      if (typeof sourceHostNetDescriptionId === 'string') {
        const movedHandle = lookupFdHandle(movedGuestFd);
        if (movedHandle?.kind === 'kernel-fd') {
          movedHandle.hostNetDescriptionId = sourceHostNetDescriptionId;
        }
      }
      return WASI_ERRNO_SUCCESS;
    }

    const syntheticHandle = activeFdProjections.get(sourceFd);
    const passthroughHandle = passthroughHandles.get(sourceFd);
    const retainedSpawnOutputHandle = retainedSpawnOutputHandlesByFd.get(sourceFd);
    if (!syntheticHandle && !passthroughHandle && !retainedSpawnOutputHandle) {
      if (rejectClosedPassthroughFd(sourceFd)) {
        return WASI_ERRNO_BADF;
      }
      return delegateManagedFdRenumber
        ? delegateManagedFdRenumber(sourceFd, targetFd)
        : WASI_ERRNO_BADF;
    }

    if (
      activeFdProjections.has(targetFd) ||
      passthroughHandles.has(targetFd) ||
      retainedSpawnOutputHandlesByFd.has(targetFd) ||
      standaloneDelegateFdRefCounts.has(targetFd)
    ) {
      const closeResult = wasiImport.fd_close(targetFd);
      if (closeResult !== WASI_ERRNO_SUCCESS) {
        return closeResult;
      }
    }

    if (syntheticHandle) {
      activeFdProjections.delete(sourceFd);
      syntheticHandle.displayFd = targetFd;
      setActiveFdProjection(targetFd, syntheticHandle);
    } else if (passthroughHandle) {
      passthroughHandles.delete(sourceFd);
      passthroughHandle.displayFd = targetFd;
      passthroughHandles.set(targetFd, passthroughHandle);
      closedPassthroughFds.add(sourceFd);
      closedPassthroughFds.delete(targetFd);
    } else {
      retainedSpawnOutputHandlesByFd.delete(sourceFd);
      retainedSpawnOutputHandlesByFd.set(targetFd, retainedSpawnOutputHandle);
    }

    // renumber(2) consumes the source descriptor and installs the same open
    // description at the target. Preserve that guest-visible closure even
    // when Node-WASI has a private/bootstrap descriptor at the source number.
    closedPassthroughFds.add(sourceFd);
    closedPassthroughFds.delete(targetFd);

    const sourceWasCloexec = standaloneCloexecFds.delete(sourceFd);
    standaloneCloexecFds.delete(targetFd);
    if (sourceWasCloexec) standaloneCloexecFds.add(targetFd);

    nextSyntheticFd = Math.max(nextSyntheticFd, targetFd + 1);
    traceHostProcess('fd-renumber', {
      from: sourceFd,
      to: targetFd,
      syntheticKind: syntheticHandle?.kind ?? null,
      passthroughKind: passthroughHandle?.kind ?? null,
    });
    return WASI_ERRNO_SUCCESS;
  } catch {
    return WASI_ERRNO_FAULT;
  }
};

wasiImport.poll_oneoff = (inPtr, outPtr, nsubscriptions, neventsPtr) => {
  if (!(instanceMemory instanceof WebAssembly.Memory)) {
    return delegateManagedPollOneoff
      ? delegateManagedPollOneoff(inPtr, outPtr, nsubscriptions, neventsPtr)
      : WASI_ERRNO_FAULT;
  }

  const subscriptionCount = Number(nsubscriptions) >>> 0;
  const countErrno = checkFixedRequestLimit(
    'wasm.abi.maxPollSubscriptions',
    subscriptionCount,
    WASI_POLL_MAX_SUBSCRIPTIONS,
  );
  if (countErrno !== WASI_ERRNO_SUCCESS) return countErrno;
  const subscriptionSize = 48;
  const eventSize = 32;
  if (!guestRangesAreValid(
    [inPtr, subscriptionCount * subscriptionSize],
    [outPtr, subscriptionCount * eventSize],
    [neventsPtr, 4],
  )) {
    return WASI_ERRNO_FAULT;
  }
  if (subscriptionCount === 0) {
    return writeGuestUint32(neventsPtr, 0);
  }

  const view = new DataView(instanceMemory.buffer);
  const memory = new Uint8Array(instanceMemory.buffer);
  const subscriptions = [];
  let hasSyntheticSubscription = false;
  let hasRemappedPassthroughSubscription = false;
  const sidecarManagedProcess = SIDECAR_MANAGED_PROCESS;
  let timeoutMs = null;

  for (let index = 0; index < subscriptionCount; index += 1) {
    const base = (Number(inPtr) >>> 0) + index * subscriptionSize;
    const tag = view.getUint8(base + 8);
    const userdata = memory.slice(base, base + 8);
    if (tag === 0) {
      const timeoutNs = view.getBigUint64(base + 24, true);
      const relativeTimeoutMs = Number(timeoutNs / 1000000n);
      timeoutMs =
        timeoutMs == null ? relativeTimeoutMs : Math.min(timeoutMs, relativeTimeoutMs);
      subscriptions.push({ kind: 'clock', userdata });
      continue;
    }

    if (tag !== 1 && tag !== 2) {
      subscriptions.push({ kind: 'unsupported', userdata });
      continue;
    }

    const fd = view.getUint32(base + 16, true);
    const handle = lookupFdHandle(fd);
    if (!handle && rejectClosedPassthroughFd(fd)) {
      hasSyntheticSubscription = true;
      subscriptions.push({
        kind: tag === 1 ? 'fd_read' : 'fd_write',
        fd,
        handle,
        userdata,
        error: WASI_ERRNO_BADF,
      });
      continue;
    }
    if (handle?.kind === 'kernel-fd') {
      hasRemappedPassthroughSubscription = true;
    } else if (handle && handle.kind !== 'passthrough') {
      hasSyntheticSubscription = true;
    } else if (handle?.kind === 'passthrough') {
      const targetFd = Number(handle.targetFd) >>> 0;
      if (
        targetFd !== fd ||
        (fd === 0 && (sidecarManagedProcess || KERNEL_STDIO_SYNC_RPC))
      ) {
        hasRemappedPassthroughSubscription = true;
      }
    } else if (!handle && fd <= 2 && (sidecarManagedProcess || KERNEL_STDIO_SYNC_RPC)) {
      // Kernel-managed stdio with no local handle: fd 0/1/2 map straight to
      // the kernel PTY/pipes, so readiness must come from __kernel_poll — the
      // delegate's fds are the runner process's own stdio and never fire for
      // guest terminal input (vim's RealWaitForChar polls fd 0 this way).
      hasRemappedPassthroughSubscription = true;
    }
    subscriptions.push({
      kind: tag === 1 ? 'fd_read' : 'fd_write',
      fd,
      handle,
      userdata,
    });
  }

  const hasClockSubscription = subscriptions.some((subscription) => subscription.kind === 'clock');

  if (!hasSyntheticSubscription && !hasRemappedPassthroughSubscription && !hasClockSubscription) {
    return delegateManagedPollOneoff
      ? delegateManagedPollOneoff(inPtr, outPtr, nsubscriptions, neventsPtr)
      : WASI_ERRNO_BADF;
  }

  const deadline = timeoutMs == null ? null : Date.now() + Math.max(0, timeoutMs);
  const readyEvents = [];

  function collectKernelReadyEvents(waitMs) {
    if (!hasRemappedPassthroughSubscription) {
      return [];
    }

    const kernelPollFdFor = (subscription) => {
      if (subscription.kind !== 'fd_read' && subscription.kind !== 'fd_write') {
        return null;
      }
      const fd = Number(subscription.fd) >>> 0;
      if (subscription.handle?.kind === 'passthrough') {
        const targetFd = Number(subscription.handle.targetFd) >>> 0;
        if (targetFd !== fd || (fd === 0 && (sidecarManagedProcess || KERNEL_STDIO_SYNC_RPC))) {
          return targetFd;
        }
        return null;
      }
      if (subscription.handle?.kind === 'kernel-fd') {
        return Number(subscription.handle.targetFd) >>> 0;
      }
      if (!subscription.handle && fd <= 2 && (sidecarManagedProcess || KERNEL_STDIO_SYNC_RPC)) {
        return fd;
      }
      return null;
    };
    const pollTargets = subscriptions
      .filter((subscription) => kernelPollFdFor(subscription) != null)
      .map((subscription) => ({
        fd: kernelPollFdFor(subscription),
        events: subscription.kind === 'fd_read' ? KERNEL_POLLIN : KERNEL_POLLOUT,
      }));
    if (pollTargets.length === 0) {
      return [];
    }

    let response;
    try {
      response = callSyncRpc('__kernel_poll', [
        pollTargets,
        Math.max(0, Number(waitMs) >>> 0),
      ]);
    } catch (error) {
      traceHostProcess('kernel-poll-error', {
        message: error instanceof Error ? error.message : String(error),
      });
      return subscriptions.map((subscription) => ({
        userdata: subscription.userdata,
        error: WASI_ERRNO_FAULT,
        type: subscription.kind === 'fd_read' ? 1 : 2,
        nbytes: 0,
        flags: 0,
      }));
    }

    const responseEntries = Array.isArray(response?.fds) ? response.fds : [];
    const ready = [];
    for (const subscription of subscriptions) {
      const kernelFd = kernelPollFdFor(subscription);
      if (kernelFd == null) {
        continue;
      }

      const targetFd = kernelFd;
      const responseEntry = responseEntries.find(
        (entry) => (Number(entry?.fd) >>> 0) === targetFd
      );
      const revents = Number(responseEntry?.revents) >>> 0;
      const interested =
        subscription.kind === 'fd_read'
          ? KERNEL_POLLIN | KERNEL_POLLERR | KERNEL_POLLHUP
          : KERNEL_POLLOUT | KERNEL_POLLERR | KERNEL_POLLHUP;
      if ((revents & interested) === 0) {
        continue;
      }

      ready.push({
        userdata: subscription.userdata,
        error: WASI_ERRNO_SUCCESS,
        type: subscription.kind === 'fd_read' ? 1 : 2,
        nbytes: subscription.kind === 'fd_read' ? 1 : 65536,
        flags: 0,
      });
    }
    return ready;
  }

  while (readyEvents.length === 0) {
    dispatchPendingWasmSignals();
    // poll_oneoff is also a process scheduling point. A long parked kernel
    // poll here would otherwise starve descendants whose events are serviced
    // by this runner.
    pumpSpawnedChildren(0);
    for (const subscription of subscriptions) {
      if (subscription.error != null) {
        readyEvents.push({
          userdata: subscription.userdata,
          error: subscription.error,
          type: subscription.kind === 'fd_read' ? 1 : 2,
          nbytes: 0,
          flags: 0,
        });
        continue;
      }

      if (subscription.kind === 'fd_read' && subscription.handle?.kind === 'pipe-read') {
        const pipe = subscription.handle.pipe;
        if (pipe.chunks.length > 0 || (pipe.writeHandleCount === 0 && pipe.producers.size === 0)) {
          readyEvents.push({
            userdata: subscription.userdata,
            error: WASI_ERRNO_SUCCESS,
            type: 1,
            nbytes: pipe.chunks[0]?.length ?? 0,
            flags: 0,
          });
        }
        continue;
      }

      if (subscription.kind === 'fd_write' && subscription.handle?.kind === 'pipe-write') {
        readyEvents.push({
          userdata: subscription.userdata,
          error: WASI_ERRNO_SUCCESS,
          type: 2,
          nbytes: 65536,
          flags: 0,
        });
        continue;
      }
    }

    if (readyEvents.length > 0) {
      break;
    }

    if (hasRemappedPassthroughSubscription) {
      // Kernel fds wait event-driven in the sidecar (parked RPC), so the slice
      // can be long. Synthetic (pipe) subscriptions still need short local
      // pumping, so only use the long slice when every subscription is
      // kernel-backed.
      const maxKernelWaitMs = hasActiveSpawnedChildren()
        ? SPAWNED_CHILD_WAIT_SLICE_MS
        : KERNEL_WAIT_SLICE_MS;
      const kernelWaitMs = hasSyntheticSubscription
        ? deadline == null
          ? SPAWNED_CHILD_WAIT_SLICE_MS
          : Math.max(0, Math.min(SPAWNED_CHILD_WAIT_SLICE_MS, deadline - Date.now()))
        : deadline == null
          ? maxKernelWaitMs
          : Math.max(0, Math.min(maxKernelWaitMs, deadline - Date.now()));
      readyEvents.push(...collectKernelReadyEvents(kernelWaitMs));
      if (readyEvents.length > 0) {
        break;
      }
    }

    let pumped = false;
    for (const subscription of subscriptions) {
      if (subscription.kind === 'fd_read' && subscription.handle?.kind === 'pipe-read') {
        pumped = pumpPipeProducers(subscription.handle.pipe, 10) || pumped;
      }
    }

    if (pumped) {
      continue;
    }

    if (deadline != null && Date.now() >= deadline) {
      break;
    }

    Atomics.wait(
      syntheticWaitArray,
      0,
      0,
      deadline == null ? 10 : Math.max(0, Math.min(10, deadline - Date.now())),
    );
  }

    if (readyEvents.length === 0 && hasClockSubscription) {
    const clockSubscription = subscriptions.find((subscription) => subscription.kind === 'clock');
    readyEvents.push({
      userdata: clockSubscription.userdata,
      error: WASI_ERRNO_SUCCESS,
      type: 0,
      nbytes: 0,
      flags: 0,
    });
  }

  for (let index = 0; index < readyEvents.length; index += 1) {
    const base = (Number(outPtr) >>> 0) + index * eventSize;
    const event = readyEvents[index];
    memory.set(event.userdata, base);
    view.setUint16(base + 8, event.error, true);
    view.setUint8(base + 10, event.type);
    view.setBigUint64(base + 16, BigInt(event.nbytes), true);
    view.setUint16(base + 24, event.flags, true);
  }

  return writeGuestUint32(neventsPtr, readyEvents.length);
};

// Immutable VM system identity is sidecar/kernel-owned. The complete guest
// destination is checked before the RPC so a bad pointer cannot trigger host
// work and then fail while publishing the result.
const hostSystemIdentityFields = [
  'hostname',
  'type',
  'release',
  'version',
  'machine',
  'domainName',
];
const hostSystemImport = {
  get_identity(field, bufferPtr, bufferLength) {
    const fieldIndex = Number(field) >>> 0;
    const capacity = Number(bufferLength) >>> 0;
    if (fieldIndex >= hostSystemIdentityFields.length) return WASI_ERRNO_INVAL;
    if (!guestRangeIsValid(bufferPtr, capacity)) return WASI_ERRNO_FAULT;
    if (capacity === 0) return WASI_ERRNO_NAMETOOLONG;
    try {
      const identity = callSyncRpc('process.system_identity', []);
      const value = identity?.[hostSystemIdentityFields[fieldIndex]];
      if (typeof value !== 'string') return WASI_ERRNO_IO;
      const bytes = Buffer.from(value, 'utf8');
      if (bytes.length + 1 > capacity) return WASI_ERRNO_NAMETOOLONG;
      const output = new Uint8Array(instanceMemory.buffer);
      const base = Number(bufferPtr) >>> 0;
      output.set(bytes, base);
      output[base + bytes.length] = 0;
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapHostProcessError(error);
    }
  },
};

// Terminal event source for crossterm-based guests (brush shell, reedline).
// The patched crossterm WasiEventSource reads keystrokes through this import:
//   read(ptr, len, timeout_ms) -> usize
// It performs a timed read of the guest's stdin (the kernel PTY/pipe) and copies
// the bytes into guest memory, returning the count (0 on timeout / EOF). usize::MAX
// (-1 as i32) means block until input. Backed by the same __kernel_stdin_read RPC
// the wasi fd_read path uses, so it works identically under native and browser.
const hostTtyImport = {
  // Long event-driven waits: the sidecar parks __kernel_stdin_read and replies
  // when the PTY becomes readable (reply-by-token), so a host->guest write the
  // caller is waiting for (e.g. the CPR reply to crossterm's cursor-position
  // query) lands immediately — no polling slices, no self-deadlock.
  read(ptr, len, timeoutMs) {
    const cap = Number(len) >>> 0;
    if (cap === 0) return 0;
    // Validate the guest destination before asking the kernel for input. Once
    // __kernel_stdin_read returns bytes they have been consumed from the PTY;
    // discovering an invalid pointer afterwards would silently lose them.
    if (!guestRangeIsValid(ptr, cap)) return 0;
    const blocking = (timeoutMs >>> 0) === 0xffffffff;
    const deadline = blocking ? Infinity : Date.now() + (Number(timeoutMs) >>> 0);
    while (true) {
      const remaining = deadline - Date.now();
      if (remaining <= 0) return 0;
      const response = callSyncRpc('__kernel_stdin_read', [
        cap,
        Math.min(remaining, KERNEL_WAIT_SLICE_MS),
      ]);
      if (response && typeof response.dataBase64 === 'string') {
        const bytes = Buffer.from(response.dataBase64, 'base64');
        const n = Math.min(bytes.length, cap);
        if (n > 0) {
          new Uint8Array(instanceMemory.buffer).set(bytes.subarray(0, n), ptr >>> 0);
          return n;
        }
      }
      if (response && response.done === true) return 0;
    }
  },
  // `host_tty.isatty(fd)` -> 1 if the guest fd is a kernel PTY, else 0.
  isatty(fd) {
    const descriptor = Number(fd) >>> 0;
    return stdioFdIsKernelTty(descriptor) ? 1 : 0;
  },
  // `host_tty.get_size(fd, colsPtr, rowsPtr)` -> writes the PTY window size as two
  // little-endian u16s and returns 0; non-zero (ENOTTY) if fd is not a PTY.
  get_size(fd, colsPtr, rowsPtr) {
    if (!guestRangeIsValid(colsPtr, 2) || !guestRangeIsValid(rowsPtr, 2)) {
      return WASI_ERRNO_FAULT;
    }
    try {
      const kernelFd = canonicalKernelFdForSpawnAction(fd);
      const size = callSyncRpc('__kernel_tty_size', [kernelFd]);
      if (!size || typeof size.cols !== 'number' || typeof size.rows !== 'number') {
        return 25; // ENOTTY
      }
      const view = new DataView(instanceMemory.buffer);
      view.setUint16(colsPtr >>> 0, size.cols & 0xffff, true);
      view.setUint16(rowsPtr >>> 0, size.rows & 0xffff, true);
      return 0;
    } catch (error) {
      // host_tty is a libc/POSIX extension and returns native errno values,
      // not Preview1 errno ordinals.
      if (error?.code === 'EBADF') return 9;
      if (error?.code === 'ENOTTY') return 25;
      return 5; // EIO
    }
  },
  set_size(fd, cols, rows) {
    try {
      const numericCols = Number(cols) >>> 0;
      const numericRows = Number(rows) >>> 0;
      if (numericCols > 0xffff || numericRows > 0xffff) {
        return WASI_ERRNO_INVAL;
      }
      const kernelFd = canonicalKernelFdForSpawnAction(fd);
      callSyncRpc('__kernel_tty_set_size', [kernelFd, numericCols, numericRows]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapHostProcessError(error);
    }
  },
  get_attr(fd, flagsPtr, ccPtr) {
    try {
      if (!guestRangeIsValid(flagsPtr, 4) || !guestRangeIsValid(ccPtr, 7)) {
        return WASI_ERRNO_FAULT;
      }
      const kernelFd = canonicalKernelFdForSpawnAction(fd);
      const value = callSyncRpc('__kernel_tcgetattr', [kernelFd]);
      const cc = Array.isArray(value?.cc) ? value.cc : null;
      if (cc == null || cc.length !== 7) return WASI_ERRNO_FAULT;
      writeGuestUint32(flagsPtr, Number(value?.flags) >>> 0);
      new Uint8Array(instanceMemory.buffer).set(cc.map((byte) => Number(byte) & 0xff), ccPtr >>> 0);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapHostProcessError(error);
    }
  },
  set_attr(fd, flags, ccPtr) {
    try {
      if (!guestRangeIsValid(ccPtr, 7)) return WASI_ERRNO_FAULT;
      const cc = Array.from(new Uint8Array(instanceMemory.buffer, ccPtr >>> 0, 7));
      const kernelFd = canonicalKernelFdForSpawnAction(fd);
      callSyncRpc('__kernel_tcsetattr', [kernelFd, flags >>> 0, cc]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapHostProcessError(error);
    }
  },
  get_pgrp(fd, retPgidPtr) {
    try {
      if (!guestRangeIsValid(retPgidPtr, 4)) return WASI_ERRNO_FAULT;
      const kernelFd = canonicalKernelFdForSpawnAction(fd);
      return writeGuestUint32(retPgidPtr, callSyncRpc('__kernel_tcgetpgrp', [kernelFd]));
    } catch (error) {
      return mapHostProcessError(error);
    }
  },
  set_pgrp(fd, pgid) {
    try {
      const kernelFd = canonicalKernelFdForSpawnAction(fd);
      callSyncRpc('__kernel_tcsetpgrp', [kernelFd, pgid >>> 0]);
      return WASI_ERRNO_SUCCESS;
    } catch (error) {
      return mapHostProcessError(error);
    }
  },
  get_sid(fd, retSidPtr) {
    try {
      if (!guestRangeIsValid(retSidPtr, 4)) return WASI_ERRNO_FAULT;
      const kernelFd = canonicalKernelFdForSpawnAction(fd);
      return writeGuestUint32(retSidPtr, callSyncRpc('__kernel_tcgetsid', [kernelFd]));
    } catch (error) {
      return mapHostProcessError(error);
    }
  },
  // Toggle terminal raw mode on the guest's PTY. crossterm/pty_probe/vim call this
  // instead of tcsetattr; route it to the kernel so the guest gets raw keystrokes.
  set_raw_mode(enabled) {
    if (!stdioFdIsKernelTty(0)) {
      return 25; // ENOTTY
    }
    callSyncRpc('__pty_set_raw_mode', [(enabled >>> 0) !== 0]);
    return 0;
  },
};

function __agentOSWasiWrapImport(name, delegate) {
  if (!__agentOSWasiSyscallPhasesEnabled || typeof delegate !== 'function') {
    return delegate;
  }
  return (...args) => {
    const startedNs = __agentOSWasmNowNs();
    try {
      return delegate(...args);
    } finally {
      __agentOSWasiRecordSyscall(name, startedNs);
    }
  };
}

if (__agentOSWasiSyscallPhasesEnabled) {
  for (const [name, delegate] of Object.entries(wasiImport)) {
    if (typeof delegate === 'function') {
      wasiImport[name] = __agentOSWasiWrapImport(name, delegate.bind(wasiImport));
    }
  }
}

function instantiateWasmModule(targetModule) {
  try {
    return __agentOSWasmMeasurePhase('WebAssembly.Instance', () => new WebAssembly.Instance(targetModule, {
      wasi_snapshot_preview1: wasiImport,
      wasi_unstable: wasiImport,
      host_system: hostSystemImport,
      host_tty: hostTtyImport,
      // Read-write commands like DuckDB need fd_dup_min from the patched
      // wasi-libc surface, but broader host_process capabilities stay
      // reserved for the full tier.
      host_process:
        permissionTier === 'full'
          ? hostProcessImport
          : permissionTier === 'isolated'
            ? undefined
            : limitedHostProcessImport,
      host_net: permissionTier === 'full' ? hostNetImport : undefined,
      host_user: hostUserImport,
      host_fs: hostFsImport,
    }));
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    const unsupported = /Import #\d+ "([^"]+)" "([^"]+)"/.exec(message);
    if (unsupported) {
      process.stderr.write(
        `ERR_AGENTOS_WASM_UNSUPPORTED_IMPORT: unsupported WebAssembly host import ${unsupported[1]}.${unsupported[2]}\n`,
      );
    } else {
      process.stderr.write(
        'ERR_AGENTOS_WASM_INSTANTIATION: WebAssembly host imports do not match module requirements\n',
      );
    }
    process.exit(1);
    throw error;
  }
}

let instance = instantiateWasmModule(module);

if (instance.exports.memory instanceof WebAssembly.Memory) {
  instanceMemory = instance.exports.memory;
}

function initializeSignalMaskForInstance(targetInstance) {
  const mask = encodeSignalMask(currentKernelSignalMask());
  if (mask.lo === 0 && mask.hi === 0) {
    return;
  }
  const setter = targetInstance?.exports?.__agentos_set_initial_sigmask;
  if (typeof setter !== 'function') {
    throw new Error(
      'spawned WASM image cannot initialize its inherited signal mask; rebuild it with the current AgentOS sysroot',
    );
  }
  setter(mask.lo, mask.hi);
}

initializeSignalMaskForInstance(instance);

function dispatchWasmSignal(delivery) {
  const numeric = Number(delivery?.signal) | 0;
  const token = Number(delivery?.token);
  if (numeric <= 0) {
    return false;
  }
  if (typeof instance?.exports?.__wasi_signal_trampoline !== 'function') {
    if (Number.isSafeInteger(token)) {
      callSyncRpc('process.signal_end', [token]);
    }
    return false;
  }
  try {
    instance.exports.__wasi_signal_trampoline(numeric);
    return true;
  } finally {
    if (Number.isSafeInteger(token)) {
      callSyncRpc('process.signal_end', [token]);
    }
  }
}

// Returns true when the current syscall must observe EINTR. For the documented
// restartable family (blocking fd/socket read/write, accept/connect, waitpid,
// flock, and record locks), a batch whose every caught handler has SA_RESTART
// completes its handler checkpoints and lets the operation continue.
function dispatchPendingWasmSignals(restartableOperation = false) {
  let caught = false;
  let everyCaughtHandlerRestarts = true;
  // Standard signals coalesce in the kernel, so 64 iterations are a strict
  // bound even when handlers recursively make more signals deliverable.
  for (let index = 0; index < 64; index += 1) {
    let delivery;
    try {
      delivery = callSyncRpc('process.take_signal', []);
    } catch (error) {
      if (error?.code === 'ERR_AGENTOS_WASM_SYNC_RPC_UNAVAILABLE') {
        return caught && !(restartableOperation && everyCaughtHandlerRestarts);
      }
      throw error;
    }
    if (delivery == null || typeof delivery?.signal !== 'number') {
      return caught && !(restartableOperation && everyCaughtHandlerRestarts);
    }
    if ((Number(delivery?.flags) & LINUX_SA_RESTART) === 0) {
      everyCaughtHandlerRestarts = false;
    }
    caught = dispatchWasmSignal(delivery) || caught;
  }
  return caught && !(restartableOperation && everyCaughtHandlerRestarts);
}

Object.defineProperty(globalThis, '__agentOsWasmSignalDispatch', {
  configurable: true,
  writable: true,
  value: () => dispatchPendingWasmSignals(),
});

while (typeof instance.exports._start === 'function') {
  // The `RuntimeError: unreachable` reports that used to point at
  // `WASI.start()` were caused by the host shim around guest startup, not by
  // V8 itself. Standalone runs must keep ordinary stdio on local process
  // streams unless kernel stdio sync-RPC is explicitly enabled, while
  // `poll_oneoff` still routes readiness probes through `__kernel_poll`.
  // That preserves the expected startup ordering so guest `_start` checks can
  // observe the ready event before we exit the runner.
  let exitCode;
  try {
    exitCode = __agentOSWasmMeasurePhase('wasi.start', () => wasi.start(instance));
  } catch (error) {
    if (isExecReplacement(error)) {
      for (const fd of new Set(error.image.closeFds)) {
        if (
          error.image.sidecarCommitted === true &&
          forgetSidecarClosedKernelFd(Number(fd) >>> 0)
        ) {
          continue;
        }
        try {
          const result = wasiImport.fd_close(Number(fd) >>> 0);
          if (result !== WASI_ERRNO_SUCCESS) {
            warnExecCloseFailure(Number(fd) >>> 0, `WASI errno ${result}`);
          }
        } catch (closeError) {
          // Linux commits a valid exec even when a close-on-exec close reports an error.
          warnExecCloseFailure(
            Number(fd) >>> 0,
            closeError instanceof Error ? closeError.message : String(closeError),
          );
        }
      }
      guestArgv = error.image.argv;
      guestEnv = error.image.env;
      wasi.args = guestArgv.map((value) => String(value));
      wasi.env = Object.fromEntries(
        Object.entries(guestEnv).map(([key, value]) => [String(key), String(value)]),
      );
      instance = instantiateWasmModule(error.image.module);
      instanceMemory = instance.exports.memory instanceof WebAssembly.Memory
        ? instance.exports.memory
        : null;
      initializeSignalMaskForInstance(instance);
      continue;
    }
    __agentOSWasmEmitPhaseMetrics('wasi.start.error', {
      error: error && typeof error === 'object' && 'message' in error ? String(error.message) : String(error),
    });
    __agentOSWasiEmitSyscallPhaseMetrics();
    if (maxStackBytes !== null && isWasmStackExhaustionTrap(error)) {
      reportConfiguredStackLimitExceeded(error);
      process.exit(1);
    }
    throw error;
  }
  __agentOSWasmEmitPhaseMetrics('complete', { exitCode });
  process.exit(typeof exitCode === 'number' ? exitCode : 0);
}

if (typeof instance.exports.run === 'function') {
  const result = await instance.exports.run();
  if (typeof result !== 'undefined') {
    console.log(String(result));
  }
} else {
  throw new Error('WebAssembly module must export _start or run');
}
