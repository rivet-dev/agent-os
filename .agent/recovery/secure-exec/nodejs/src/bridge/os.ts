// OS module polyfill for the sandbox
// Provides Node.js os module emulation for sandbox compatibility

import type * as nodeOs from "os";
import { exposeCustomGlobal } from "@secure-exec/core/internal/shared/global-exposure";

// Configuration interface - values are set via globals before bridge loads
export interface OSConfig {
  platform?: string;
  arch?: string;
  type?: string;
  release?: string;
  version?: string;
  homedir?: string;
  tmpdir?: string;
  hostname?: string;
}

// Declare the config global that host sets up
declare const _osConfig: OSConfig | undefined;

// Get config with defaults
const config: Required<OSConfig> = {
  platform: (typeof _osConfig !== "undefined" && _osConfig.platform) || "linux",
  arch: (typeof _osConfig !== "undefined" && _osConfig.arch) || "x64",
  type: (typeof _osConfig !== "undefined" && _osConfig.type) || "Linux",
  release: (typeof _osConfig !== "undefined" && _osConfig.release) || "5.15.0",
  version: (typeof _osConfig !== "undefined" && _osConfig.version) || "#1 SMP",
  homedir: (typeof _osConfig !== "undefined" && _osConfig.homedir) || "/root",
  tmpdir: (typeof _osConfig !== "undefined" && _osConfig.tmpdir) || "/tmp",
  hostname: (typeof _osConfig !== "undefined" && _osConfig.hostname) || "sandbox",
};

function getRuntimeHomeDir(): string {
  return globalThis.process?.env?.HOME || config.homedir;
}

function getRuntimeTmpDir(): string {
  return globalThis.process?.env?.TMPDIR || config.tmpdir;
}

// Signal constants (subset — sandbox only emulates Linux signals)
const signals = {
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
  SIGSYS: 31,
};

// Errno constants
const errno = {
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
  EXDEV: 18,
};

// Priority constants
const priority = {
  PRIORITY_LOW: 19,
  PRIORITY_BELOW_NORMAL: 10,
  PRIORITY_NORMAL: 0,
  PRIORITY_ABOVE_NORMAL: -7,
  PRIORITY_HIGH: -14,
  PRIORITY_HIGHEST: -20,
};

// OS module implementation (polyfill — partial coverage of Node.js os types)
const os = {
  // Platform information
  platform(): NodeJS.Platform {
    return config.platform as NodeJS.Platform;
  },
  arch(): string {
    return config.arch;
  },
  type(): string {
    return config.type;
  },
  release(): string {
    return config.release;
  },
  version(): string {
    return config.version;
  },

  // Directory information
  homedir(): string {
    return getRuntimeHomeDir();
  },
  tmpdir(): string {
    return getRuntimeTmpDir();
  },

  // System information
  hostname(): string {
    return config.hostname;
  },

  // User information
  userInfo(_options?: nodeOs.UserInfoOptions): nodeOs.UserInfo<string> {
    return {
      username: "root",
      uid: 0,
      gid: 0,
      shell: "/bin/bash",
      homedir: getRuntimeHomeDir(),
    };
  },

  // CPU information
  cpus(): nodeOs.CpuInfo[] {
    return [
      {
        model: "Virtual CPU",
        speed: 2000,
        times: {
          user: 100000,
          nice: 0,
          sys: 50000,
          idle: 800000,
          irq: 0,
        },
      },
    ];
  },

  // Memory information
  totalmem(): number {
    return 1073741824; // 1GB
  },
  freemem(): number {
    return 536870912; // 512MB
  },

  // System load
  loadavg(): number[] {
    return [0.1, 0.1, 0.1];
  },

  // System uptime
  uptime(): number {
    return 3600; // 1 hour
  },

  // Network interfaces (empty - not supported in sandbox)
  networkInterfaces(): NodeJS.Dict<nodeOs.NetworkInterfaceInfo[]> {
    return {};
  },

  // System endianness
  endianness(): "BE" | "LE" {
    return "LE";
  },

  // Line endings
  EOL: "\n",

  // Dev null path
  devNull: "/dev/null",

  // Machine type
  machine(): string {
    return config.arch;
  },

  // Constants (partial — Linux subset, no Windows WSA* or RTLD_DEEPBIND)
  constants: {
    signals: signals as nodeOs.SignalConstants,
    errno: errno as (typeof nodeOs.constants)["errno"],
    priority,
    dlopen: {
      RTLD_LAZY: 1,
      RTLD_NOW: 2,
      RTLD_GLOBAL: 256,
      RTLD_LOCAL: 0,
    } as (typeof nodeOs.constants)["dlopen"],
    UV_UDP_REUSEADDR: 4,
  },

  // Priority getters/setters (stubs)
  getPriority(_pid?: number): number {
    return 0;
  },
  setPriority(pid: number | undefined, priority?: number): void {
    void pid;
    void priority;
  },

  // Parallelism hint
  availableParallelism(): number {
    return 1;
  },
} as typeof nodeOs;

// Expose to global for require() to use.
exposeCustomGlobal("_osModule", os);

export default os;
