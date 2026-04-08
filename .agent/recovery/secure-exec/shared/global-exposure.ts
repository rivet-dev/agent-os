/**
 * Classification for globals the runtime installs on the isolate's `globalThis`.
 *
 * - `hardened`: non-writable, non-configurable. Prevents sandbox code from
 *   replacing bridge callbacks or lifecycle hooks.
 * - `mutable-runtime-state`: writable per-execution state (module cache,
 *   stdin data, CJS module/exports wrappers) that must be reset between runs.
 */
export type CustomGlobalClassification =
	| "hardened"
	| "mutable-runtime-state";

export interface CustomGlobalInventoryEntry {
	name: string;
	classification: CustomGlobalClassification;
	rationale: string;
}

// Canonical Node runtime + bridge custom-global inventory.
export const NODE_CUSTOM_GLOBAL_INVENTORY: readonly CustomGlobalInventoryEntry[] = [
	{
		name: "_processConfig",
		classification: "hardened",
		rationale: "Bridge bootstrap configuration must not be replaced by sandbox code.",
	},
	{
		name: "_osConfig",
		classification: "hardened",
		rationale: "Bridge bootstrap configuration must not be replaced by sandbox code.",
	},
	{
		name: "bridge",
		classification: "hardened",
		rationale: "Bridge export object is runtime-owned control-plane state.",
	},
	{
		name: "_registerHandle",
		classification: "hardened",
		rationale: "Active-handle lifecycle hook controls runtime completion semantics.",
	},
	{
		name: "_unregisterHandle",
		classification: "hardened",
		rationale: "Active-handle lifecycle hook controls runtime completion semantics.",
	},
	{
		name: "_waitForActiveHandles",
		classification: "hardened",
		rationale: "Active-handle lifecycle hook controls runtime completion semantics.",
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
		rationale: "Bridge-owned child_process module handle for require resolution.",
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
		name: "_netSocketDispatch",
		classification: "hardened",
		rationale: "Host-to-sandbox net socket event dispatch entrypoint.",
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
		rationale: "Host console capture reference consumed by sandbox console shim.",
	},
	{
		name: "_error",
		classification: "hardened",
		rationale: "Host console capture reference consumed by sandbox console shim.",
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
		name: "_cryptoSubtle",
		classification: "hardened",
		rationale: "Host WebCrypto subtle bridge reference.",
	},
	{
		name: "_fsReadFile",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsWriteFile",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsReadFileBinary",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsWriteFileBinary",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsReadDir",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsMkdir",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsRmdir",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsExists",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsStat",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsUnlink",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsRename",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsChmod",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsChown",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsLink",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsSymlink",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsReadlink",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsLstat",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsTruncate",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
	},
	{
		name: "_fsUtimes",
		classification: "hardened",
		rationale: "Host filesystem bridge reference.",
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
		name: "_childProcessStdinWrite",
		classification: "hardened",
		rationale: "Host child_process bridge reference.",
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
		name: "_networkFetchRaw",
		classification: "hardened",
		rationale: "Host network bridge reference.",
	},
	{
		name: "_networkDnsLookupRaw",
		classification: "hardened",
		rationale: "Host network bridge reference.",
	},
	{
		name: "_networkHttpRequestRaw",
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
		rationale: "Host network bridge reference for sandbox HTTP server responses.",
	},
	{
		name: "_networkHttpServerWaitRaw",
		classification: "hardened",
		rationale: "Host network bridge reference for sandbox HTTP server lifetime tracking.",
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
		name: "_netSocketWaitConnectRaw",
		classification: "hardened",
		rationale: "Host net socket connect-wait bridge reference.",
	},
	{
		name: "_netSocketReadRaw",
		classification: "hardened",
		rationale: "Host net socket read bridge reference.",
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
		name: "_netServerListenRaw",
		classification: "hardened",
		rationale: "Host net server listen bridge reference.",
	},
	{
		name: "_netServerAcceptRaw",
		classification: "hardened",
		rationale: "Host net server accept bridge reference.",
	},
	{
		name: "_netServerCloseRaw",
		classification: "hardened",
		rationale: "Host net server close bridge reference.",
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
		name: "_batchResolveModules",
		classification: "hardened",
		rationale: "Host bridge for batched module resolution to reduce IPC round-trips.",
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
		rationale: "Runtime-owned host callback reference for dynamic import resolution.",
	},
	{
		name: "__dynamicImport",
		classification: "hardened",
		rationale: "Runtime-owned dynamic-import shim entrypoint.",
	},
	{
		name: "_moduleCache",
		classification: "hardened",
		rationale: "Per-execution CommonJS/require cache — hardened via read-only Proxy to prevent cache poisoning.",
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
		rationale: "Network fetch API global — must not be replaceable by sandbox code.",
	},
	{
		name: "Headers",
		classification: "hardened",
		rationale: "Network Headers API global — must not be replaceable by sandbox code.",
	},
	{
		name: "Request",
		classification: "hardened",
		rationale: "Network Request API global — must not be replaceable by sandbox code.",
	},
	{
		name: "Response",
		classification: "hardened",
		rationale: "Network Response API global — must not be replaceable by sandbox code.",
	},
	{
		name: "DOMException",
		classification: "hardened",
		rationale: "DOMException global stub for undici/bootstrap compatibility.",
	},
	{
		name: "__importMetaResolve",
		classification: "hardened",
		rationale: "Internal import.meta.resolve helper for transformed ESM modules.",
	},
	{
		name: "Blob",
		classification: "hardened",
		rationale: "Blob API global stub — must not be replaceable by sandbox code.",
	},
	{
		name: "File",
		classification: "hardened",
		rationale: "File API global stub — must not be replaceable by sandbox code.",
	},
	{
		name: "FormData",
		classification: "hardened",
		rationale: "FormData API global stub — must not be replaceable by sandbox code.",
	},
];

export const HARDENED_NODE_CUSTOM_GLOBALS = NODE_CUSTOM_GLOBAL_INVENTORY
	.filter((entry) => entry.classification === "hardened")
	.map((entry) => entry.name);

export const MUTABLE_NODE_CUSTOM_GLOBALS = NODE_CUSTOM_GLOBAL_INVENTORY
	.filter((entry) => entry.classification === "mutable-runtime-state")
	.map((entry) => entry.name);

interface ExposeGlobalOptions {
	mutable?: boolean;
	enumerable?: boolean;
}

/**
 * Define a property on `target` using `Object.defineProperty`.
 * By default the property is non-writable/non-configurable (hardened).
 */
export function exposeGlobalBinding(
	target: Record<string, unknown>,
	name: string,
	value: unknown,
	options: ExposeGlobalOptions = {},
): void {
	const mutable = options.mutable === true;
	const enumerable = options.enumerable !== false;
	Object.defineProperty(target, name, {
		value,
		writable: mutable,
		configurable: mutable,
		enumerable,
	});
}

/** Install a hardened (non-writable) global on `globalThis`. */
export function exposeCustomGlobal(name: string, value: unknown): void {
	exposeGlobalBinding(globalThis as Record<string, unknown>, name, value);
}

/** Install a writable global on `globalThis` for per-execution state. */
export function exposeMutableRuntimeStateGlobal(
	name: string,
	value: unknown,
): void {
	exposeGlobalBinding(globalThis as Record<string, unknown>, name, value, {
		mutable: true,
	});
}

/**
 * Inline JavaScript source that provides `exposeCustomGlobal` and
 * `exposeMutableRuntimeStateGlobal` inside the isolate's V8 context.
 * Evaluated by the host after context creation so that bridge/runtime
 * scripts can harden their own globals.
 */
export const ISOLATE_GLOBAL_EXPOSURE_HELPER_SOURCE = `(() => {
  const exposeGlobalBinding = (name, value, mutable = false) => {
    Object.defineProperty(globalThis, name, {
      value,
      writable: mutable,
      configurable: mutable,
      enumerable: true,
    });
  };
  const exposeCustomGlobal = (name, value) => exposeGlobalBinding(name, value, false);
  const exposeMutableRuntimeStateGlobal = (name, value) =>
    exposeGlobalBinding(name, value, true);
  return {
    exposeCustomGlobal,
    exposeMutableRuntimeStateGlobal,
  };
})()`;
