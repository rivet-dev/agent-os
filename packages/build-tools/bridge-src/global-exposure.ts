// .agent/recovery/agentos/shared/global-exposure.ts
var NODE_CUSTOM_GLOBAL_INVENTORY = [
	{
		name: "_processConfig",
		classification: "hardened",
		rationale:
			"Bridge bootstrap configuration must not be replaced by sandbox code.",
	},
	{
		name: "__agentOsHrNowUs",
		classification: "hardened",
		rationale:
			"High-resolution monotonic clock, only installed when high_resolution_time opt-in is set.",
	},
	{
		name: "__agentOsRequireEsmSync",
		classification: "hardened",
		rationale: "V8-owned synchronous ESM loader used by Node-compatible require().",
	},
	{
		name: "process.cpuUsage",
		classification: "hardened",
		rationale: "Host process CPU usage bridge reference.",
	},
	{
		name: "process.memoryUsage",
		classification: "hardened",
		rationale: "Host process memory usage bridge reference.",
	},
	{
		name: "process.resourceUsage",
		classification: "hardened",
		rationale: "Host process resource usage bridge reference.",
	},
	{
		name: "process.umask",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "process.flock",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "process.fcntlLock",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "process.getuid",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "process.getgid",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "process.geteuid",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "process.getegid",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "process.getresuid",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "process.getresgid",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "process.getgroups",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "process.getpwuid",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "process.getpwnam",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "process.getpwent",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "process.getgrgid",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "process.getgrnam",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "process.getgrent",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "process.setuid",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "process.seteuid",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "process.setreuid",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "process.setresuid",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "process.setgid",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "process.setegid",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "process.setregid",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "process.setresgid",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "process.setgroups",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "_fsAccess",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "_fsRenameAt2",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "_fsLchown",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "_fsGetxattr",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "_fsListxattr",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "_fsSetxattr",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "_fsRemovexattr",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "_fsTruncateForProcess",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "_fsFallocate",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "_fsInsertRange",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "_fsCollapseRange",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "_fsPunchHole",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "_fsZeroRange",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "_fsFiemap",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "_fsNamedFifoPeerReady",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "_fsBlockingIoTimeoutMs",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "_fsMknod",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "_fsOpenTmpfile",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "_fsLinkFd",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "_fsRemount",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "_fsStatfs",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "_fsChmodForProcess",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "_kernelDescendantStdinWaitingRaw",
		classification: "hardened",
		rationale: "Kernel-owned guest filesystem and process bridge reference.",
	},
	{
		name: "process.versions",
		classification: "hardened",
		rationale: "Host process versions bridge reference.",
	},
	{
		name: "_processKill",
		classification: "hardened",
		rationale: "Host process signal bridge reference.",
	},
	{
		name: "_processExec",
		classification: "hardened",
		rationale: "Host process image replacement bridge reference.",
	},
	{
		name: "_processExecFdImageCommit",
		classification: "hardened",
		rationale: "Host descriptor-backed process image commit bridge reference.",
	},
	{
		name: "_processSignalState",
		classification: "hardened",
		rationale: "Host process signal-listener state bridge reference.",
	},
	{
		name: "_processTakeSignal",
		classification: "hardened",
		rationale: "Host process pending-signal drain bridge reference.",
	},
	{
		name: "_processWasmSyncRpc",
		classification: "hardened",
		rationale: "Allowlisted WASM process syscall bridge reference.",
	},
	{
		name: "_osConfig",
		classification: "hardened",
		rationale:
			"Bridge bootstrap configuration must not be replaced by sandbox code.",
	},
	{
		name: "bridge",
		classification: "hardened",
		rationale: "Bridge export object is runtime-owned control-plane state.",
	},
	{
		name: "_registerHandle",
		classification: "hardened",
		rationale:
			"Active-handle lifecycle hook controls runtime completion semantics.",
	},
	{
		name: "_unregisterHandle",
		classification: "hardened",
		rationale:
			"Active-handle lifecycle hook controls runtime completion semantics.",
	},
	{
		name: "_waitForActiveHandles",
		classification: "hardened",
		rationale:
			"Active-handle lifecycle hook controls runtime completion semantics.",
	},
	{
		name: "_getActiveHandles",
		classification: "hardened",
		rationale: "Bridge debug hook should not be replaced by sandbox code.",
	},
	{
		name: "_childProcessDispatch",
		classification: "hardened",
		rationale: "Host-to-sandbox child-process callback dispatch entrypoint.",
	},
	{
		name: "_childProcessModule",
		classification: "hardened",
		rationale:
			"Bridge-owned child_process module handle for require resolution.",
	},
	{
		name: "_osModule",
		classification: "hardened",
		rationale: "Bridge-owned os module handle for require resolution.",
	},
	{
		name: "_moduleModule",
		classification: "hardened",
		rationale: "Bridge-owned module module handle for require resolution.",
	},
	{
		name: "_httpModule",
		classification: "hardened",
		rationale: "Bridge-owned http module handle for require resolution.",
	},
	{
		name: "_httpsModule",
		classification: "hardened",
		rationale: "Bridge-owned https module handle for require resolution.",
	},
	{
		name: "_http2Module",
		classification: "hardened",
		rationale: "Bridge-owned http2 module handle for require resolution.",
	},
	{
		name: "_dnsModule",
		classification: "hardened",
		rationale: "Bridge-owned dns module handle for require resolution.",
	},
	{
		name: "_dgramModule",
		classification: "hardened",
		rationale: "Bridge-owned dgram module handle for require resolution.",
	},
	{
		name: "_netModule",
		classification: "hardened",
		rationale: "Bridge-owned net module handle for require resolution.",
	},
	{
		name: "_tlsModule",
		classification: "hardened",
		rationale: "Bridge-owned tls module handle for require resolution.",
	},
	{
		name: "_vmCreateContext",
		classification: "hardened",
		rationale: "Host vm context creation bridge reference.",
	},
	{
		name: "_vmRunInContext",
		classification: "hardened",
		rationale: "Host vm context execution bridge reference.",
	},
	{
		name: "_vmRunInThisContext",
		classification: "hardened",
		rationale: "Host vm current-context execution bridge reference.",
	},
	{
		name: "_netSocketDispatch",
		classification: "hardened",
		rationale: "Host-to-sandbox net socket event dispatch entrypoint.",
	},
	{
		name: "_agentOSReadyDispatch",
		classification: "hardened",
		rationale: "Capability-identity readiness dispatch entrypoint.",
	},
	{
		name: "_dgramSocketDispatch",
		classification: "hardened",
		rationale: "Host-to-sandbox dgram socket event dispatch entrypoint.",
	},
	{
		name: "_http2RetainDispatch",
		classification: "hardened",
		rationale: "Host-to-sandbox HTTP/2 retain wake dispatch entrypoint.",
	},
	{
		name: "_httpServerDispatch",
		classification: "hardened",
		rationale: "Host-to-sandbox HTTP server dispatch entrypoint.",
	},
	{
		name: "_httpServerUpgradeDispatch",
		classification: "hardened",
		rationale: "Host-to-sandbox HTTP upgrade dispatch entrypoint.",
	},
	{
		name: "_httpServerConnectDispatch",
		classification: "hardened",
		rationale: "Host-to-sandbox HTTP CONNECT dispatch entrypoint.",
	},
	{
		name: "_http2Dispatch",
		classification: "hardened",
		rationale: "Host-to-sandbox HTTP/2 event dispatch entrypoint.",
	},
	{
		name: "_timerDispatch",
		classification: "hardened",
		rationale: "Host-to-sandbox timer callback dispatch entrypoint.",
	},
	{
		name: "_drainImmediates",
		classification: "hardened",
		rationale: "Runtime-owned setImmediate queue drain entrypoint.",
	},
	{
		name: "_getPendingImmediateCount",
		classification: "hardened",
		rationale: "Runtime-owned setImmediate pending-work counter.",
	},
	{
		name: "_upgradeSocketData",
		classification: "hardened",
		rationale: "Host-to-sandbox HTTP upgrade socket data dispatch entrypoint.",
	},
	{
		name: "_upgradeSocketEnd",
		classification: "hardened",
		rationale: "Host-to-sandbox HTTP upgrade socket close dispatch entrypoint.",
	},
	{
		name: "ProcessExitError",
		classification: "hardened",
		rationale: "Runtime-owned process-exit control-path error class.",
	},
	{
		name: "_log",
		classification: "hardened",
		rationale:
			"Host console capture reference consumed by sandbox console shim.",
	},
	{
		name: "_error",
		classification: "hardened",
		rationale:
			"Host console capture reference consumed by sandbox console shim.",
	},
	{
		name: "_pythonRpc",
		classification: "hardened",
		rationale: "Host Python VFS RPC bridge reference.",
	},
	{
		name: "_pythonStdinRead",
		classification: "hardened",
		rationale: "Host Python stdin bridge reference.",
	},
	{
		name: "_loadPolyfill",
		classification: "hardened",
		rationale: "Host module-loading bridge reference.",
	},
	{
		name: "_resolveModule",
		classification: "hardened",
		rationale: "Host module-resolution bridge reference.",
	},
	{
		name: "_loadFile",
		classification: "hardened",
		rationale: "Host file-loading bridge reference.",
	},
	{
		name: "_resolveModuleSync",
		classification: "hardened",
		rationale: "Host synchronous module-resolution bridge reference.",
	},
	{
		name: "_loadFileSync",
		classification: "hardened",
		rationale: "Host synchronous file-loading bridge reference.",
	},
	{
		name: "_moduleFormat",
		classification: "hardened",
		rationale:
			"Host module-format bridge reference used to enforce CommonJS and ESM boundaries.",
	},
	{
		name: "_scheduleTimer",
		classification: "hardened",
		rationale: "Host timer bridge reference used by process timers.",
	},
	{
		name: "_cryptoRandomFill",
		classification: "hardened",
		rationale: "Host entropy bridge reference for crypto.getRandomValues.",
	},
	{
		name: "_cryptoRandomUUID",
		classification: "hardened",
		rationale: "Host entropy bridge reference for crypto.randomUUID.",
	},
	{
		name: "_cryptoHashDigest",
		classification: "hardened",
		rationale: "Host crypto digest bridge reference.",
	},
	{
		name: "_cryptoHashCreate",
		classification: "hardened",
		rationale: "Host incremental crypto digest creation bridge reference.",
	},
	{
		name: "_cryptoHashUpdate",
		classification: "hardened",
		rationale: "Host incremental crypto digest update bridge reference.",
	},
	{
		name: "_cryptoHashFinal",
		classification: "hardened",
		rationale: "Host incremental crypto digest completion bridge reference.",
	},
	{
		name: "_cryptoHashDestroy",
		classification: "hardened",
		rationale: "Host incremental crypto digest cleanup bridge reference.",
	},
	{
		name: "_cryptoHmacDigest",
		classification: "hardened",
		rationale: "Host crypto HMAC bridge reference.",
	},
	{
		name: "_cryptoPbkdf2",
		classification: "hardened",
		rationale: "Host crypto PBKDF2 bridge reference.",
	},
	{
		name: "_cryptoScrypt",
		classification: "hardened",
		rationale: "Host crypto scrypt bridge reference.",
	},
	{
		name: "_cryptoCipheriv",
		classification: "hardened",
		rationale: "Host crypto cipher bridge reference.",
	},
	{
		name: "_cryptoDecipheriv",
		classification: "hardened",
		rationale: "Host crypto decipher bridge reference.",
	},
	{
		name: "_cryptoCipherivCreate",
		classification: "hardened",
		rationale: "Host streaming cipher bridge reference.",
	},
	{
		name: "_cryptoCipherivUpdate",
		classification: "hardened",
		rationale: "Host streaming cipher update bridge reference.",
	},
	{
		name: "_cryptoCipherivFinal",
		classification: "hardened",
		rationale: "Host streaming cipher finalization bridge reference.",
	},
	{
		name: "_cryptoSign",
		classification: "hardened",
		rationale: "Host crypto sign bridge reference.",
	},
	{
		name: "_cryptoVerify",
		classification: "hardened",
		rationale: "Host crypto verify bridge reference.",
	},
	{
		name: "_cryptoAsymmetricOp",
		classification: "hardened",
		rationale: "Host asymmetric crypto operation bridge reference.",
	},
	{
		name: "_cryptoCreateKeyObject",
		classification: "hardened",
		rationale: "Host asymmetric key import bridge reference.",
	},
	{
		name: "_cryptoGenerateKeyPairSync",
		classification: "hardened",
		rationale: "Host crypto key-pair generation bridge reference.",
	},
	{
		name: "_cryptoGenerateKeySync",
		classification: "hardened",
		rationale: "Host symmetric crypto key generation bridge reference.",
	},
	{
		name: "_cryptoGeneratePrimeSync",
		classification: "hardened",
		rationale: "Host prime generation bridge reference.",
	},
	{
		name: "_cryptoDiffieHellman",
		classification: "hardened",
		rationale: "Host stateless Diffie-Hellman bridge reference.",
	},
	{
		name: "_cryptoDiffieHellmanGroup",
		classification: "hardened",
		rationale: "Host Diffie-Hellman group bridge reference.",
	},
	{
		name: "_cryptoDiffieHellmanSessionCreate",
		classification: "hardened",
		rationale: "Host Diffie-Hellman/ECDH session creation bridge reference.",
	},
	{
		name: "_cryptoDiffieHellmanSessionCall",
		classification: "hardened",
		rationale: "Host Diffie-Hellman/ECDH session method bridge reference.",
	},
	{
		name: "_cryptoDiffieHellmanSessionDestroy",
		classification: "hardened",
		rationale: "Host Diffie-Hellman/ECDH session release bridge reference.",
	},
	{
		name: "_cryptoSubtle",
		classification: "hardened",
		rationale: "Host WebCrypto subtle bridge reference.",
	},
	{
		name: "_benchNoop",
		classification: "hardened",
		rationale: "Benchmark-only sync bridge diagnostic.",
	},
	{
		name: "_fsReadFile",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsReadFileAsync",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsWriteFile",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsWriteFileAsync",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsReadFileBinary",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsReadFileBinaryAsync",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsWriteFileBinary",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsWriteFileBinaryRaw",
		classification: "hardened",
		rationale: "Raw-byte host filesystem binary write bridge reference.",
	},
	{
		name: "_fsWriteFileBinaryAsync",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsReadDir",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsReadDirAsync",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsMkdir",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsMkdirAsync",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsRmdir",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsRmdirAsync",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsExists",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsAccessAsync",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsStat",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsStatAsync",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsUnlink",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsUnlinkAsync",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsRename",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsRenameAsync",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsChmod",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsChmodAsync",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsChown",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsChownAsync",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsLink",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsLinkAsync",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsSymlink",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsSymlinkAsync",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsReadlink",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsReadlinkAsync",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsLstat",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsLstatAsync",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsTruncate",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsTruncateAsync",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsUtimes",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsLutimes",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsUtimesAsync",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsLutimesAsync",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "fs.futimesSync",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "fs.openSync",
		classification: "hardened",
		rationale: "Host file-descriptor open bridge reference.",
	},
	{
		name: "fs.closeSync",
		classification: "hardened",
		rationale: "Host file-descriptor close bridge reference.",
	},
	{
		name: "fs._getPathSync",
		classification: "hardened",
		rationale: "Host file-descriptor guest-path bridge reference.",
	},
	{
		name: "fs.readSync",
		classification: "hardened",
		rationale: "Host file-descriptor read bridge reference.",
	},
	{
		name: "_fsReadRaw",
		classification: "hardened",
		rationale: "Raw-byte host file-descriptor read bridge reference.",
	},
	{
		name: "_fsReadFileRangeRaw",
		classification: "hardened",
		rationale: "Bounded raw-byte host pathname read bridge reference.",
	},
	{
		name: "fs.writeSync",
		classification: "hardened",
		rationale: "Host file-descriptor write bridge reference.",
	},
	{
		name: "_fsWriteRaw",
		classification: "hardened",
		rationale: "Raw-byte host file-descriptor write bridge reference.",
	},
	{
		name: "_fsWritevRaw",
		classification: "hardened",
		rationale: "Raw-byte host file-descriptor vector write bridge reference.",
	},
	{
		name: "fs.fstatSync",
		classification: "hardened",
		rationale: "Host file-descriptor stat bridge reference.",
	},
	{
		name: "_fs",
		classification: "hardened",
		rationale: "Bridge filesystem facade consumed by fs polyfill.",
	},
	{
		name: "_childProcessSpawnStart",
		classification: "hardened",
		rationale: "Host child_process bridge reference.",
	},
	{
		name: "_childProcessPoll",
		classification: "hardened",
		rationale: "Host child_process bridge reference.",
	},
	{
		name: "_childProcessStdinWrite",
		classification: "hardened",
		rationale: "Host child_process bridge reference.",
	},
	{
		name: "_childProcessPtyResize",
		classification: "hardened",
		rationale: "Host child_process PTY resize bridge reference.",
	},
	{
		name: "_childProcessStdinClose",
		classification: "hardened",
		rationale: "Host child_process bridge reference.",
	},
	{
		name: "_childProcessKill",
		classification: "hardened",
		rationale: "Host child_process bridge reference.",
	},
	{
		name: "_childProcessSpawnSync",
		classification: "hardened",
		rationale: "Host child_process bridge reference.",
	},
	{
		name: "_benchNetTcpMetricsResetRaw",
		classification: "hardened",
		rationale: "Benchmark-only TCP readiness counter reset bridge reference.",
	},
	{
		name: "_benchNetTcpMetricsSnapshotRaw",
		classification: "hardened",
		rationale:
			"Benchmark-only TCP readiness counter snapshot bridge reference.",
	},
	{
		name: "_networkDnsLookupRaw",
		classification: "hardened",
		rationale: "Host network bridge reference.",
	},
	{
		name: "_networkDnsLookupSyncRaw",
		classification: "hardened",
		rationale: "Host synchronous network lookup bridge reference.",
	},
	{
		name: "_networkDnsResolveRaw",
		classification: "hardened",
		rationale: "Host network bridge reference.",
	},
	{
		name: "_networkHttpServerListenRaw",
		classification: "hardened",
		rationale: "Host network bridge reference.",
	},
	{
		name: "_networkHttpServerCloseRaw",
		classification: "hardened",
		rationale: "Host network bridge reference.",
	},
	{
		name: "_networkHttpServerRespondRaw",
		classification: "hardened",
		rationale:
			"Host network bridge reference for sandbox HTTP server responses.",
	},
	{
		name: "_networkHttpServerRequestRaw",
		classification: "hardened",
		rationale:
			"Host network bridge reference for sandbox HTTP loopback requests.",
	},
	{
		name: "_networkHttpServerWaitRaw",
		classification: "hardened",
		rationale:
			"Host network bridge reference for sandbox HTTP server lifetime tracking.",
	},
	{
		name: "_networkHttp2ServerListenRaw",
		classification: "hardened",
		rationale: "Host HTTP/2 server listen bridge reference.",
	},
	{
		name: "_networkHttp2ServerCloseRaw",
		classification: "hardened",
		rationale: "Host HTTP/2 server close bridge reference.",
	},
	{
		name: "_networkHttp2ServerWaitRaw",
		classification: "hardened",
		rationale: "Host HTTP/2 server lifetime bridge reference.",
	},
	{
		name: "_networkHttp2SessionConnectRaw",
		classification: "hardened",
		rationale: "Host HTTP/2 session connect bridge reference.",
	},
	{
		name: "_networkHttp2SessionRequestRaw",
		classification: "hardened",
		rationale: "Host HTTP/2 session request bridge reference.",
	},
	{
		name: "_networkHttp2SessionSettingsRaw",
		classification: "hardened",
		rationale: "Host HTTP/2 session settings bridge reference.",
	},
	{
		name: "_networkHttp2SessionSetLocalWindowSizeRaw",
		classification: "hardened",
		rationale: "Host HTTP/2 session local-window bridge reference.",
	},
	{
		name: "_networkHttp2SessionGoawayRaw",
		classification: "hardened",
		rationale: "Host HTTP/2 session GOAWAY bridge reference.",
	},
	{
		name: "_networkHttp2SessionCloseRaw",
		classification: "hardened",
		rationale: "Host HTTP/2 session close bridge reference.",
	},
	{
		name: "_networkHttp2SessionDestroyRaw",
		classification: "hardened",
		rationale: "Host HTTP/2 session destroy bridge reference.",
	},
	{
		name: "_networkHttp2SessionWaitRaw",
		classification: "hardened",
		rationale: "Host HTTP/2 session lifetime bridge reference.",
	},
	{
		name: "_networkHttp2ServerPollRaw",
		classification: "hardened",
		rationale: "Host HTTP/2 server event-poll bridge reference.",
	},
	{
		name: "_networkHttp2SessionPollRaw",
		classification: "hardened",
		rationale: "Host HTTP/2 session event-poll bridge reference.",
	},
	{
		name: "_networkHttp2StreamRespondRaw",
		classification: "hardened",
		rationale: "Host HTTP/2 stream respond bridge reference.",
	},
	{
		name: "_networkHttp2StreamPushStreamRaw",
		classification: "hardened",
		rationale: "Host HTTP/2 push stream bridge reference.",
	},
	{
		name: "_networkHttp2StreamWriteRaw",
		classification: "hardened",
		rationale: "Host HTTP/2 stream write bridge reference.",
	},
	{
		name: "_networkHttp2StreamEndRaw",
		classification: "hardened",
		rationale: "Host HTTP/2 stream end bridge reference.",
	},
	{
		name: "_networkHttp2StreamCloseRaw",
		classification: "hardened",
		rationale: "Host HTTP/2 stream close bridge reference.",
	},
	{
		name: "_networkHttp2StreamPauseRaw",
		classification: "hardened",
		rationale: "Host HTTP/2 stream pause bridge reference.",
	},
	{
		name: "_networkHttp2StreamResumeRaw",
		classification: "hardened",
		rationale: "Host HTTP/2 stream resume bridge reference.",
	},
	{
		name: "_networkHttp2StreamRespondWithFileRaw",
		classification: "hardened",
		rationale: "Host HTTP/2 stream respondWithFile bridge reference.",
	},
	{
		name: "_networkHttp2ServerRespondRaw",
		classification: "hardened",
		rationale: "Host HTTP/2 server-response bridge reference.",
	},
	{
		name: "_upgradeSocketWriteRaw",
		classification: "hardened",
		rationale: "Host HTTP upgrade socket write bridge reference.",
	},
	{
		name: "_upgradeSocketEndRaw",
		classification: "hardened",
		rationale: "Host HTTP upgrade socket half-close bridge reference.",
	},
	{
		name: "_upgradeSocketDestroyRaw",
		classification: "hardened",
		rationale: "Host HTTP upgrade socket destroy bridge reference.",
	},
	{
		name: "_netSocketConnectRaw",
		classification: "hardened",
		rationale: "Host net socket connect bridge reference.",
	},
	{
		name: "_netSocketPollRaw",
		classification: "hardened",
		rationale: "Host net socket poll bridge reference.",
	},
	{
		name: "_netSocketWaitConnectRaw",
		classification: "hardened",
		rationale: "Host net socket connect-wait bridge reference.",
	},
	{
		name: "_netSocketWaitConnectSyncRaw",
		classification: "hardened",
		rationale: "Host synchronous net socket connect-wait bridge reference.",
	},
	{
		name: "_netSocketReadRaw",
		classification: "hardened",
		rationale: "Host net socket read bridge reference.",
	},
	{
		name: "_netSocketSetReadInterestRaw",
		classification: "hardened",
		rationale: "Host net socket application-read backpressure reference.",
	},
	{
		name: "_netSocketSetNoDelayRaw",
		classification: "hardened",
		rationale: "Host net socket no-delay bridge reference.",
	},
	{
		name: "_netSocketSetKeepAliveRaw",
		classification: "hardened",
		rationale: "Host net socket keepalive bridge reference.",
	},
	{
		name: "_netSocketWriteRaw",
		classification: "hardened",
		rationale: "Host net socket write bridge reference.",
	},
	{
		name: "_netSocketWriteSyncRaw",
		classification: "hardened",
		rationale: "Host synchronous net socket write bridge reference for WASM guests.",
	},
	{
		name: "_netSocketEndRaw",
		classification: "hardened",
		rationale: "Host net socket end bridge reference.",
	},
	{
		name: "_netSocketDestroyRaw",
		classification: "hardened",
		rationale: "Host net socket destroy bridge reference.",
	},
	{
		name: "_netSocketUpgradeTlsRaw",
		classification: "hardened",
		rationale: "Host net socket TLS-upgrade bridge reference.",
	},
	{
		name: "_netSocketUpgradeTlsAsyncRaw",
		classification: "hardened",
		rationale: "Asynchronous host net socket TLS-upgrade bridge reference.",
	},
	{
		name: "_netSocketGetTlsClientHelloRaw",
		classification: "hardened",
		rationale: "Host loopback TLS client-hello bridge reference.",
	},
	{
		name: "_netSocketTlsQueryRaw",
		classification: "hardened",
		rationale: "Host TLS socket query bridge reference.",
	},
	{
		name: "_tlsGetCiphersRaw",
		classification: "hardened",
		rationale: "Host TLS cipher-list bridge reference.",
	},
	{
		name: "_netReserveTcpPortRaw",
		classification: "hardened",
		rationale: "Host net TCP port reservation bridge reference.",
	},
	{
		name: "_netReleaseTcpPortRaw",
		classification: "hardened",
		rationale: "Host net TCP port release bridge reference.",
	},
	{
		name: "_netServerListenRaw",
		classification: "hardened",
		rationale: "Host net server listen bridge reference.",
	},
	{
		name: "_netBindUnixRaw",
		classification: "hardened",
		rationale: "Host Unix-domain listener bind bridge reference.",
	},
	{
		name: "_netBindConnectedUnixRaw",
		classification: "hardened",
		rationale: "Host connected Unix-domain socket bind bridge reference.",
	},
	{
		name: "_netServerAcceptRaw",
		classification: "hardened",
		rationale: "Host net server accept bridge reference.",
	},
	{
		name: "_netServerCloseRaw",
		classification: "hardened",
		rationale: "Asynchronous host net server close bridge reference.",
	},
	{
		name: "_netServerCloseSyncRaw",
		classification: "hardened",
		rationale: "Host synchronous net server close bridge reference.",
	},
	{
		name: "_dgramSocketCreateRaw",
		classification: "hardened",
		rationale: "Host dgram socket create bridge reference.",
	},
	{
		name: "_dgramSocketBindRaw",
		classification: "hardened",
		rationale: "Host dgram socket bind bridge reference.",
	},
	{
		name: "_dgramSocketRecvRaw",
		classification: "hardened",
		rationale: "Host dgram socket receive bridge reference.",
	},
	{
		name: "_dgramSocketSendRaw",
		classification: "hardened",
		rationale: "Host dgram socket send bridge reference.",
	},
	{
		name: "_dgramSocketConnectRaw",
		classification: "hardened",
		rationale: "Host dgram socket connect bridge reference.",
	},
	{
		name: "_dgramSocketDisconnectRaw",
		classification: "hardened",
		rationale: "Host dgram socket disconnect bridge reference.",
	},
	{
		name: "_dgramSocketRemoteAddressRaw",
		classification: "hardened",
		rationale: "Host dgram connected peer bridge reference.",
	},
	{
		name: "_dgramSocketCloseRaw",
		classification: "hardened",
		rationale: "Host dgram socket close bridge reference.",
	},
	{
		name: "_dgramSocketAddressRaw",
		classification: "hardened",
		rationale: "Host dgram socket address bridge reference.",
	},
	{
		name: "_dgramSocketSetOptionRaw",
		classification: "hardened",
		rationale: "Host dgram descriptor option bridge reference.",
	},
	{
		name: "_dgramSocketSetBufferSizeRaw",
		classification: "hardened",
		rationale: "Host dgram socket buffer-size setter bridge reference.",
	},
	{
		name: "_dgramSocketGetBufferSizeRaw",
		classification: "hardened",
		rationale: "Host dgram socket buffer-size getter bridge reference.",
	},
	{
		name: "_sqliteConstantsRaw",
		classification: "hardened",
		rationale: "Host sqlite constants bridge reference.",
	},
	{
		name: "_sqliteDatabaseOpenRaw",
		classification: "hardened",
		rationale: "Host sqlite database-open bridge reference.",
	},
	{
		name: "_sqliteDatabaseCloseRaw",
		classification: "hardened",
		rationale: "Host sqlite database-close bridge reference.",
	},
	{
		name: "_sqliteDatabaseExecRaw",
		classification: "hardened",
		rationale: "Host sqlite exec bridge reference.",
	},
	{
		name: "_sqliteDatabaseQueryRaw",
		classification: "hardened",
		rationale: "Host sqlite query bridge reference.",
	},
	{
		name: "_sqliteDatabasePrepareRaw",
		classification: "hardened",
		rationale: "Host sqlite prepare bridge reference.",
	},
	{
		name: "_sqliteDatabaseLocationRaw",
		classification: "hardened",
		rationale: "Host sqlite location bridge reference.",
	},
	{
		name: "_sqliteDatabaseCheckpointRaw",
		classification: "hardened",
		rationale: "Host sqlite checkpoint bridge reference.",
	},
	{
		name: "_sqliteStatementRunRaw",
		classification: "hardened",
		rationale: "Host sqlite statement-run bridge reference.",
	},
	{
		name: "_sqliteStatementGetRaw",
		classification: "hardened",
		rationale: "Host sqlite statement-get bridge reference.",
	},
	{
		name: "_sqliteStatementAllRaw",
		classification: "hardened",
		rationale: "Host sqlite statement-all bridge reference.",
	},
	{
		name: "_sqliteStatementColumnsRaw",
		classification: "hardened",
		rationale: "Host sqlite statement-columns bridge reference.",
	},
	{
		name: "_sqliteStatementSetReturnArraysRaw",
		classification: "hardened",
		rationale: "Host sqlite statement return-arrays bridge reference.",
	},
	{
		name: "_sqliteStatementSetReadBigIntsRaw",
		classification: "hardened",
		rationale: "Host sqlite statement read-bigints bridge reference.",
	},
	{
		name: "_sqliteStatementSetAllowBareNamedParametersRaw",
		classification: "hardened",
		rationale: "Host sqlite bare-named-parameter bridge reference.",
	},
	{
		name: "_sqliteStatementSetAllowUnknownNamedParametersRaw",
		classification: "hardened",
		rationale: "Host sqlite unknown-named-parameter bridge reference.",
	},
	{
		name: "_sqliteStatementFinalizeRaw",
		classification: "hardened",
		rationale: "Host sqlite statement-finalize bridge reference.",
	},
	{
		name: "_batchResolveModules",
		classification: "hardened",
		rationale:
			"Host bridge for batched module resolution to reduce IPC round-trips.",
	},
	{
		name: "_kernelPollRaw",
		classification: "hardened",
		rationale:
			"Host kernel poll bridge reference for multi-fd readiness waits.",
	},
	{
		name: "_kernelPoll",
		classification: "hardened",
		rationale:
			"Host asynchronous kernel poll bridge reference for readiness-driven fd operations.",
	},
	{
		name: "_kernelIsattyRaw",
		classification: "hardened",
		rationale:
			"Host kernel TTY detection bridge reference for WASM terminal commands.",
	},
	{
		name: "_kernelFlockRaw",
		classification: "hardened",
		rationale: "Host kernel file-lock bridge reference.",
	},
	{
		name: "_kernelTtySizeRaw",
		classification: "hardened",
		rationale:
			"Host kernel TTY size bridge reference for WASM terminal commands.",
	},
	{
		name: "_kernelStdioWriteRaw",
		classification: "hardened",
		rationale: "Host kernel stdio write bridge reference.",
	},
	{
		name: "_kernelStdinReadRaw",
		classification: "hardened",
		rationale: "Host synchronous kernel stdin read bridge reference.",
	},
	{
		name: "_kernelStdinRead",
		classification: "hardened",
		rationale: "Host asynchronous kernel stdin read bridge reference.",
	},
	{
		name: "_ptySetRawMode",
		classification: "hardened",
		rationale: "Host PTY bridge reference for stdin.setRawMode().",
	},
	{
		name: "require",
		classification: "hardened",
		rationale: "Runtime-owned global require shim entrypoint.",
	},
	{
		name: "_requireFrom",
		classification: "hardened",
		rationale: "Runtime-owned internal require shim used by module polyfill.",
	},
	{
		name: "_dynamicImport",
		classification: "hardened",
		rationale:
			"Runtime-owned host callback reference for dynamic import resolution.",
	},
	{
		name: "__dynamicImport",
		classification: "hardened",
		rationale: "Runtime-owned dynamic-import shim entrypoint.",
	},
	{
		name: "_moduleCache",
		classification: "hardened",
		rationale:
			"Per-execution CommonJS/require cache \u2014 hardened via read-only Proxy to prevent cache poisoning.",
	},
	{
		name: "_pendingModules",
		classification: "mutable-runtime-state",
		rationale: "Per-execution circular-load tracking state.",
	},
	{
		name: "_currentModule",
		classification: "mutable-runtime-state",
		rationale: "Per-execution module resolution context.",
	},
	{
		name: "_stdinData",
		classification: "mutable-runtime-state",
		rationale: "Per-execution stdin payload state.",
	},
	{
		name: "_stdinPosition",
		classification: "mutable-runtime-state",
		rationale: "Per-execution stdin stream cursor state.",
	},
	{
		name: "_stdinEnded",
		classification: "mutable-runtime-state",
		rationale: "Per-execution stdin completion state.",
	},
	{
		name: "_stdinFlowMode",
		classification: "mutable-runtime-state",
		rationale: "Per-execution stdin flow-control state.",
	},
	{
		name: "module",
		classification: "mutable-runtime-state",
		rationale: "Per-execution CommonJS module wrapper state.",
	},
	{
		name: "exports",
		classification: "mutable-runtime-state",
		rationale: "Per-execution CommonJS module wrapper state.",
	},
	{
		name: "__filename",
		classification: "mutable-runtime-state",
		rationale: "Per-execution CommonJS file context state.",
	},
	{
		name: "__dirname",
		classification: "mutable-runtime-state",
		rationale: "Per-execution CommonJS file context state.",
	},
	{
		name: "fetch",
		classification: "hardened",
		rationale:
			"Network fetch API global \u2014 must not be replaceable by sandbox code.",
	},
	{
		name: "Headers",
		classification: "hardened",
		rationale:
			"Network Headers API global \u2014 must not be replaceable by sandbox code.",
	},
	{
		name: "Request",
		classification: "hardened",
		rationale:
			"Network Request API global \u2014 must not be replaceable by sandbox code.",
	},
	{
		name: "Response",
		classification: "hardened",
		rationale:
			"Network Response API global \u2014 must not be replaceable by sandbox code.",
	},
	{
		name: "WebSocket",
		classification: "hardened",
		rationale:
			"Node WebSocket API global backed by the bounded kernel network stack.",
	},
	{
		name: "DOMException",
		classification: "hardened",
		rationale: "DOMException global stub for undici/bootstrap compatibility.",
	},
	{
		name: "__importMetaResolve",
		classification: "hardened",
		rationale:
			"Internal import.meta.resolve helper for transformed ESM modules.",
	},
	{
		name: "Blob",
		classification: "hardened",
		rationale:
			"Blob API global stub \u2014 must not be replaceable by sandbox code.",
	},
	{
		name: "File",
		classification: "hardened",
		rationale:
			"File API global stub \u2014 must not be replaceable by sandbox code.",
	},
	{
		name: "FormData",
		classification: "hardened",
		rationale:
			"FormData API global stub \u2014 must not be replaceable by sandbox code.",
	},
];
var HARDENED_NODE_CUSTOM_GLOBALS = NODE_CUSTOM_GLOBAL_INVENTORY.filter(
	(entry) => entry.classification === "hardened",
).map((entry) => entry.name);
var MUTABLE_NODE_CUSTOM_GLOBALS = NODE_CUSTOM_GLOBAL_INVENTORY.filter(
	(entry) => entry.classification === "mutable-runtime-state",
).map((entry) => entry.name);
function exposeGlobalBinding(target, name, value, options = {}) {
	const mutable = options.mutable === true;
	const enumerable = options.enumerable !== false;
	Object.defineProperty(target, name, {
		value,
		writable: mutable,
		// Always configurable so the per-execution jsRuntime shim can scrub
		// host globals for non-node platforms (see prepend_v8_runtime_shim).
		// This only affects the guest's own realm; the kernel boundary lives in
		// the bridge RPC layer, not these property descriptors.
		configurable: true,
		enumerable,
	});
}
function exposeCustomGlobal(name, value) {
	exposeGlobalBinding(globalThis, name, value);
}
function exposeInstallCompatibleHardenedGlobal(name, value) {
	Object.defineProperty(globalThis, name, {
		get: () => value,
		// Some Node packages install web globals by assignment. Accept the write
		// without replacing AgentOS's policy-enforcing implementation.
		set: () => {},
		configurable: true,
		enumerable: true,
	});
}
function exposeMutableRuntimeStateGlobal(name, value) {
	exposeGlobalBinding(globalThis, name, value, {
		mutable: true,
	});
}

export {
	exposeCustomGlobal,
	exposeGlobalBinding,
	exposeInstallCompatibleHardenedGlobal,
	exposeMutableRuntimeStateGlobal,
	HARDENED_NODE_CUSTOM_GLOBALS,
	MUTABLE_NODE_CUSTOM_GLOBALS,
	NODE_CUSTOM_GLOBAL_INVENTORY,
};
