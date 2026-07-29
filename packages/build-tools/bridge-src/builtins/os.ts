import { exposeCustomGlobal } from "../global-exposure.js";

var config = {
  platform: typeof _osConfig !== "undefined" && _osConfig.platform || "linux",
  arch: typeof _osConfig !== "undefined" && _osConfig.arch || "x64",
  type: typeof _osConfig !== "undefined" && _osConfig.type || "Linux",
  release: typeof _osConfig !== "undefined" && _osConfig.release || "6.8.0-agentos",
  version: typeof _osConfig !== "undefined" && _osConfig.version || "#1 SMP PREEMPT_DYNAMIC agentos",
  homedir: typeof _osConfig !== "undefined" && _osConfig.homedir || "/home/user",
  tmpdir: typeof _osConfig !== "undefined" && _osConfig.tmpdir || "/tmp",
  hostname: typeof _osConfig !== "undefined" && _osConfig.hostname || "agentos",
  machine: typeof _osConfig !== "undefined" && _osConfig.machine || "x86_64"
};
function getRuntimeHomeDir() {
  return globalThis.process?.env?.HOME || runtimeVirtualOsString("homedir", config.homedir);
}
function getRuntimeTmpDir() {
  return globalThis.process?.env?.TMPDIR || runtimeVirtualOsString("tmpdir", config.tmpdir);
}
function getRuntimeUserHomeDir() {
  return runtimeVirtualOsString("homedir", config.homedir);
}
function getRuntimeUserName() {
  return runtimeVirtualOsString(
    "user",
    globalThis.process?.env?.USER || globalThis.process?.env?.LOGNAME || "user"
  );
}
function getRuntimeShell() {
  return runtimeVirtualOsString("shell", globalThis.process?.env?.SHELL || "/bin/sh");
}
function getRuntimeUid() {
  const value = globalThis.process?.uid;
  return Number.isFinite(value) ? value : 0;
}
function getRuntimeGid() {
  const value = globalThis.process?.gid;
  return Number.isFinite(value) ? value : 0;
}
function getRuntimeInternalEnv(name) {
  const bridgedValue = globalThis.__agentOSProcessConfigEnv?.[name];
  if (typeof bridgedValue === "string" && bridgedValue.length > 0) {
    return bridgedValue;
  }
  const hiddenValue = typeof _processConfig !== "undefined" ? _processConfig.env?.[name] : void 0;
  if (typeof hiddenValue === "string" && hiddenValue.length > 0) {
    return hiddenValue;
  }
  const publicValue = globalThis.process?.env?.[name];
  return typeof publicValue === "string" ? publicValue : void 0;
}
function getRuntimePositiveIntEnv(name, fallback) {
  const rawValue = getRuntimeInternalEnv(name);
  if (typeof rawValue !== "string" || rawValue.length === 0) {
    return fallback;
  }
  const parsed = Number.parseInt(rawValue, 10);
  return Number.isSafeInteger(parsed) && parsed > 0 ? parsed : fallback;
}
function getRuntimeVirtualOs() {
  return globalThis.__agentOSVirtualOs || {};
}
function runtimeVirtualOsString(name, fallback) {
  const value = getRuntimeVirtualOs()[name];
  return typeof value === "string" && value.length > 0 ? value : fallback;
}
function runtimeVirtualOsPositiveInt(name, fallback) {
  const parsed = Number(getRuntimeVirtualOs()[name]);
  return Number.isSafeInteger(parsed) && parsed > 0 ? parsed : fallback;
}
function getRuntimeVirtualCpuCount() {
  return runtimeVirtualOsPositiveInt("cpuCount", 1);
}
function getRuntimeVirtualTotalMem() {
  return runtimeVirtualOsPositiveInt("totalmem", 1073741824);
}
function getRuntimeVirtualFreeMem() {
  return Math.min(
    runtimeVirtualOsPositiveInt("freemem", 536870912),
    getRuntimeVirtualTotalMem()
  );
}
var signals = {
  SIGHUP: 1,
  SIGINT: 2,
  SIGQUIT: 3,
  SIGILL: 4,
  SIGTRAP: 5,
  SIGABRT: 6,
  SIGIOT: 6,
  SIGBUS: 7,
  SIGFPE: 8,
  SIGKILL: 9,
  SIGUSR1: 10,
  SIGSEGV: 11,
  SIGUSR2: 12,
  SIGPIPE: 13,
  SIGALRM: 14,
  SIGTERM: 15,
  SIGSTKFLT: 16,
  SIGCHLD: 17,
  SIGCONT: 18,
  SIGSTOP: 19,
  SIGTSTP: 20,
  SIGTTIN: 21,
  SIGTTOU: 22,
  SIGURG: 23,
  SIGXCPU: 24,
  SIGXFSZ: 25,
  SIGVTALRM: 26,
  SIGPROF: 27,
  SIGWINCH: 28,
  SIGIO: 29,
  SIGPOLL: 29,
  SIGPWR: 30,
  SIGSYS: 31
};
var canonicalChildProcessSignalNamesByNumber = {
  1: "SIGHUP",
  2: "SIGINT",
  3: "SIGQUIT",
  4: "SIGILL",
  5: "SIGTRAP",
  6: "SIGABRT",
  7: "SIGBUS",
  8: "SIGFPE",
  9: "SIGKILL",
  10: "SIGUSR1",
  11: "SIGSEGV",
  12: "SIGUSR2",
  13: "SIGPIPE",
  14: "SIGALRM",
  15: "SIGTERM",
  16: "SIGSTKFLT",
  17: "SIGCHLD",
  18: "SIGCONT",
  19: "SIGSTOP",
  20: "SIGTSTP",
  21: "SIGTTIN",
  22: "SIGTTOU",
  23: "SIGURG",
  24: "SIGXCPU",
  25: "SIGXFSZ",
  26: "SIGVTALRM",
  27: "SIGPROF",
  28: "SIGWINCH",
  29: "SIGIO",
  30: "SIGPWR",
  31: "SIGSYS"
};
function normalizeChildProcessSignal(signal) {
  if (signal == null) {
    return { bridgeSignal: "SIGTERM", signalCode: "SIGTERM" };
  }
  if (signal === 0 || signal === "0") {
    return { bridgeSignal: "0", signalCode: null };
  }
  if (typeof signal === "number") {
    const signalCode = canonicalChildProcessSignalNamesByNumber[signal];
    if (signalCode) {
      return { bridgeSignal: signalCode, signalCode };
    }
    throw new Error("Unknown signal: " + signal);
  }
  if (typeof signal === "string") {
    const signalNumber = signals[signal];
    if (signalNumber !== void 0) {
      const signalCode = canonicalChildProcessSignalNamesByNumber[signalNumber] ?? signal;
      return { bridgeSignal: signalCode, signalCode };
    }
  }
  throw new Error("Unknown signal: " + signal);
}
var errno = {
  E2BIG: 7,
  EACCES: 13,
  EADDRINUSE: 98,
  EADDRNOTAVAIL: 99,
  EAFNOSUPPORT: 97,
  EAGAIN: 11,
  EALREADY: 114,
  EBADF: 9,
  EBADMSG: 74,
  EBUSY: 16,
  ECANCELED: 125,
  ECHILD: 10,
  ECONNABORTED: 103,
  ECONNREFUSED: 111,
  ECONNRESET: 104,
  EDEADLK: 35,
  EDESTADDRREQ: 89,
  EDOM: 33,
  EDQUOT: 122,
  EEXIST: 17,
  EFAULT: 14,
  EFBIG: 27,
  EHOSTUNREACH: 113,
  EIDRM: 43,
  EILSEQ: 84,
  EINPROGRESS: 115,
  EINTR: 4,
  EINVAL: 22,
  EIO: 5,
  EISCONN: 106,
  EISDIR: 21,
  ELOOP: 40,
  EMFILE: 24,
  EMLINK: 31,
  EMSGSIZE: 90,
  EMULTIHOP: 72,
  ENAMETOOLONG: 36,
  ENETDOWN: 100,
  ENETRESET: 102,
  ENETUNREACH: 101,
  ENFILE: 23,
  ENOBUFS: 105,
  ENODATA: 61,
  ENODEV: 19,
  ENOENT: 2,
  ENOEXEC: 8,
  ENOLCK: 37,
  ENOLINK: 67,
  ENOMEM: 12,
  ENOMSG: 42,
  ENOPROTOOPT: 92,
  ENOSPC: 28,
  ENOSR: 63,
  ENOSTR: 60,
  ENOSYS: 38,
  ENOTCONN: 107,
  ENOTDIR: 20,
  ENOTEMPTY: 39,
  ENOTSOCK: 88,
  ENOTSUP: 95,
  ENOTTY: 25,
  ENXIO: 6,
  EOPNOTSUPP: 95,
  EOVERFLOW: 75,
  EPERM: 1,
  EPIPE: 32,
  EPROTO: 71,
  EPROTONOSUPPORT: 93,
  EPROTOTYPE: 91,
  ERANGE: 34,
  EROFS: 30,
  ESPIPE: 29,
  ESRCH: 3,
  ESTALE: 116,
  ETIME: 62,
  ETIMEDOUT: 110,
  ETXTBSY: 26,
  EWOULDBLOCK: 11,
  EXDEV: 18
};
var priority = {
  PRIORITY_LOW: 19,
  PRIORITY_BELOW_NORMAL: 10,
  PRIORITY_NORMAL: 0,
  PRIORITY_ABOVE_NORMAL: -7,
  PRIORITY_HIGH: -14,
  PRIORITY_HIGHEST: -20
};
var os = {
  // Platform information
  platform() {
    return runtimeVirtualOsString("platform", config.platform);
  },
  arch() {
    return runtimeVirtualOsString("arch", config.arch);
  },
  type() {
    return runtimeVirtualOsString("type", config.type);
  },
  release() {
    return runtimeVirtualOsString("release", config.release);
  },
  version() {
    return runtimeVirtualOsString("version", config.version);
  },
  // Directory information
  homedir() {
    return getRuntimeHomeDir();
  },
  tmpdir() {
    return getRuntimeTmpDir();
  },
  // System information
  hostname() {
    return runtimeVirtualOsString("hostname", config.hostname);
  },
  // User information
  userInfo(_options) {
    return {
      username: getRuntimeUserName(),
      uid: getRuntimeUid(),
      gid: getRuntimeGid(),
      shell: getRuntimeShell(),
      homedir: getRuntimeUserHomeDir()
    };
  },
  // CPU information
  cpus() {
    return Array.from({ length: getRuntimeVirtualCpuCount() }, () => ({
      model: "Virtual CPU",
      speed: 2e3,
      times: {
        user: 1e5,
        nice: 0,
        sys: 5e4,
        idle: 8e5,
        irq: 0
      }
    }));
  },
  // Memory information
  totalmem() {
    return getRuntimeVirtualTotalMem();
  },
  freemem() {
    return getRuntimeVirtualFreeMem();
  },
  // System load
  loadavg() {
    return [0.1, 0.1, 0.1];
  },
  // System uptime
  uptime() {
    return 3600;
  },
  // Network interfaces (empty - not supported in sandbox)
  networkInterfaces() {
    return {};
  },
  // System endianness
  endianness() {
    return "LE";
  },
  // Line endings
  EOL: "\n",
  // Dev null path
  devNull: "/dev/null",
  // Machine type
  machine() {
    return runtimeVirtualOsString("machine", config.machine);
  },
  // Constants (partial — Linux subset, no Windows WSA* or RTLD_DEEPBIND)
  constants: {
    signals,
    errno,
    priority,
    dlopen: {
      RTLD_LAZY: 1,
      RTLD_NOW: 2,
      RTLD_GLOBAL: 256,
      RTLD_LOCAL: 0
    },
    UV_UDP_REUSEADDR: 4
  },
  // Priority getters/setters (stubs)
  getPriority(_pid) {
    return 0;
  },
  setPriority(pid, priority2) {
    void pid;
    void priority2;
  },
  // Parallelism hint
  availableParallelism() {
    return getRuntimeVirtualCpuCount();
  }
};
exposeCustomGlobal("_osModule", os);
var os_default = os;
export { config, getRuntimeHomeDir, getRuntimeTmpDir, getRuntimeUserHomeDir, getRuntimeUserName, getRuntimeShell, getRuntimeUid, getRuntimeGid, getRuntimeInternalEnv, getRuntimePositiveIntEnv, getRuntimeVirtualOs, runtimeVirtualOsString, runtimeVirtualOsPositiveInt, getRuntimeVirtualCpuCount, getRuntimeVirtualTotalMem, getRuntimeVirtualFreeMem, signals, canonicalChildProcessSignalNamesByNumber, normalizeChildProcessSignal, errno, priority, os, os_default };
