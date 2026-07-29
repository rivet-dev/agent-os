#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const packageRoot = path.resolve(scriptDir, "..");
const repoRoot = path.resolve(packageRoot, "..", "..");
const contractPath = path.join(
	repoRoot,
	"crates",
	"bridge",
	"bridge-contract.json",
);
const workerPath = path.join(packageRoot, "src", "worker.ts");
const runtimePath = path.join(packageRoot, "src", "runtime.ts");
const runtimeDriverPath = path.join(packageRoot, "src", "runtime-driver.ts");
const syncBridgePath = path.join(packageRoot, "src", "sync-bridge.ts");

const contract = JSON.parse(fs.readFileSync(contractPath, "utf8"));
const workerSource = fs.readFileSync(workerPath, "utf8");
const runtimeSource = fs.readFileSync(runtimePath, "utf8");
const runtimeDriverSource = fs.readFileSync(runtimeDriverPath, "utf8");
const syncBridgeSource = fs.readFileSync(syncBridgePath, "utf8");
// Converged servicer modules: fs/net/dns/dgram/module guest ops are serviced by
// the converged wasm kernel (kernel-routed), NOT by runtime-driver.ts's
// handleSyncBridgeOperation (which now only handles host-capability families
// child_process.* / process.signal_state). Gather their handled-op declarations
// so the sync-bridge-operation consistency check covers the converged path.
const convergedSource = [
	"converged-fs-bridge.ts",
	"converged-net-bridge.ts",
	"converged-dgram-bridge.ts",
	"converged-pty-bridge.ts",
	"converged-module-servicer.ts",
	"converged-sync-bridge-handler.ts",
]
	.map((file) => fs.readFileSync(path.join(packageRoot, "src", file), "utf8"))
	.join("\n");

const contractMethods = new Map();
for (const group of contract.groups ?? []) {
	for (const name of group.names ?? []) {
		contractMethods.set(name, group.convention);
	}
}

const allowedBrowserOnlyGlobals = new Set([
	"__agentOSEncoding",
	"__agentOSWasiHost",
	"__agentOSTtyState",
	"__agentOSVirtualOs",
	"__runtimeProcessCwdOverride",
	"_currentModule",
	"_fs",
	"_fsModule",
	"_moduleCache",
	"_networkFetchRaw",
	"_osConfig",
	"_processConfig",
]);

// Contracted names that the browser runtime intentionally covers through
// browser-specific facades instead of one global per native bridge symbol.
const browserFacadeContractGlobals = new Set([
	"_fsAccessAsync",
	"_fsChmod",
	"_fsChmodAsync",
	"_fsChown",
	"_fsChownAsync",
	"_fsExists",
	"_fsLink",
	"_fsLinkAsync",
	"_fsLstat",
	"_fsLstatAsync",
	"_fsLutimes",
	"_fsLutimesAsync",
	"_fsMkdir",
	"_fsMkdirAsync",
	"_fsReadDir",
	"_fsReadDirAsync",
	"_fsReadFile",
	"_fsReadFileAsync",
	"_fsReadFileBinary",
	"_fsReadFileBinaryAsync",
	"_fsReadlink",
	"_fsReadlinkAsync",
	"_fsRename",
	"_fsRenameAsync",
	"_fsRmdir",
	"_fsRmdirAsync",
	"_fsStat",
	"_fsStatAsync",
	"_fsSymlink",
	"_fsSymlinkAsync",
	"_fsTruncate",
	"_fsTruncateAsync",
	"_fsUnlink",
	"_fsUnlinkAsync",
	"_fsUtimes",
	"_fsUtimesAsync",
	"_fsWriteFile",
	"_fsWriteFileAsync",
	"_fsWriteFileBinary",
	"_fsWriteFileBinaryAsync",
	"fs.closeSync",
	"fs._getPathSync",
	"fs.fstatSync",
	"fs.futimesSync",
	"fs.openSync",
	"fs.readSync",
	"fs.writeSync",
	"process.cpuUsage",
	"process.memoryUsage",
	"process.resourceUsage",
	"process.versions",
]);

// Contracted native bridge names that are not browser-installed yet. Keeping
// this list explicit makes browser contract coverage drift fail closed.
const browserUnsupportedContractGlobals = new Set([
	"_benchNoop",
	"_benchNetTcpMetricsResetRaw",
	"_benchNetTcpMetricsSnapshotRaw",
	"_dgramSocketConnectRaw",
	"_dgramSocketDisconnectRaw",
	"_dgramSocketRemoteAddressRaw",
	"_dgramSocketSetOptionRaw",
	"_dynamicImport",
	"_fsAccess",
	"_fsBlockingIoTimeoutMs",
	"_fsChmodForProcess",
	"_fsCollapseRange",
	"_fsFallocate",
	"_fsFiemap",
	"_fsGetxattr",
	"_fsInsertRange",
	"_fsLchown",
	"_fsLinkFd",
	"_fsListxattr",
	"_fsMknod",
	"_fsNamedFifoPeerReady",
	"_fsOpenTmpfile",
	"_fsPunchHole",
	"_fsReadRaw",
	"_fsReadFileRangeRaw",
	"_fsRemount",
	"_fsRemovexattr",
	"_fsRenameAt2",
	"_fsSetxattr",
	"_fsStatfs",
	"_fsTruncateForProcess",
	"_fsWriteFileBinaryRaw",
	"_fsWriteRaw",
	"_fsWritevRaw",
	"_fsZeroRange",
	"_cryptoHashCreate",
	"_cryptoHashUpdate",
	"_cryptoHashFinal",
	"_cryptoHashDestroy",
	"_kernelDescendantStdinWaitingRaw",
	"_kernelFlockRaw",
	"_kernelIsattyRaw",
	"_kernelPoll",
	"_kernelTtySizeRaw",
	"_kernelPollRaw",
	"_kernelStdinRead",
	"_kernelStdinReadRaw",
	"_kernelStdioWriteRaw",
	"_netBindConnectedUnixRaw",
	"_netBindUnixRaw",
	"_netReserveTcpPortRaw",
	"_netReleaseTcpPortRaw",
	"_netServerAcceptRaw",
	"_netServerCloseRaw",
	"_netServerCloseSyncRaw",
	"_netServerListenRaw",
	"_netSocketConnectRaw",
	"_netSocketDestroyRaw",
	"_netSocketEndRaw",
	"_netSocketGetTlsClientHelloRaw",
	"_netSocketPollRaw",
	"_netSocketReadRaw",
	"_netSocketSetReadInterestRaw",
	"_netSocketSetKeepAliveRaw",
	"_netSocketSetNoDelayRaw",
	"_netSocketTlsQueryRaw",
	"_netSocketUpgradeTlsAsyncRaw",
	"_netSocketUpgradeTlsRaw",
	"_netSocketWaitConnectRaw",
	"_netSocketWaitConnectSyncRaw",
	"_netSocketWriteRaw",
	"_netSocketWriteSyncRaw",
	"_networkDnsLookupSyncRaw",
	"_networkDnsResolveRaw",
	"_networkHttp2ServerCloseRaw",
	"_networkHttp2ServerListenRaw",
	"_networkHttp2ServerPollRaw",
	"_networkHttp2ServerRespondRaw",
	"_networkHttp2ServerWaitRaw",
	"_networkHttp2SessionCloseRaw",
	"_networkHttp2SessionConnectRaw",
	"_networkHttp2SessionDestroyRaw",
	"_networkHttp2SessionGoawayRaw",
	"_networkHttp2SessionPollRaw",
	"_networkHttp2SessionRequestRaw",
	"_networkHttp2SessionSetLocalWindowSizeRaw",
	"_networkHttp2SessionSettingsRaw",
	"_networkHttp2SessionWaitRaw",
	"_networkHttp2StreamCloseRaw",
	"_networkHttp2StreamEndRaw",
	"_networkHttp2StreamPauseRaw",
	"_networkHttp2StreamPushStreamRaw",
	"_networkHttp2StreamRespondRaw",
	"_networkHttp2StreamRespondWithFileRaw",
	"_networkHttp2StreamResumeRaw",
	"_networkHttp2StreamWriteRaw",
	"_networkHttpServerCloseRaw",
	"_networkHttpServerListenRaw",
	"_networkHttpServerRequestRaw",
	"_networkHttpServerRespondRaw",
	"_networkHttpServerWaitRaw",
	"_processKill",
	"_processExec",
	"_processExecFdImageCommit",
	"_processTakeSignal",
	"_processWasmSyncRpc",
	"_ptySetRawMode",
	"_pythonRpc",
	"_pythonStdinRead",
	"_sqliteConstantsRaw",
	"_sqliteDatabaseCheckpointRaw",
	"_sqliteDatabaseCloseRaw",
	"_sqliteDatabaseExecRaw",
	"_sqliteDatabaseLocationRaw",
	"_sqliteDatabaseOpenRaw",
	"_sqliteDatabasePrepareRaw",
	"_sqliteDatabaseQueryRaw",
	"_sqliteStatementAllRaw",
	"_sqliteStatementColumnsRaw",
	"_sqliteStatementFinalizeRaw",
	"_sqliteStatementGetRaw",
	"_sqliteStatementRunRaw",
	"_sqliteStatementSetAllowBareNamedParametersRaw",
	"_sqliteStatementSetAllowUnknownNamedParametersRaw",
	"_sqliteStatementSetReadBigIntsRaw",
	"_sqliteStatementSetReturnArraysRaw",
	"_tlsGetCiphersRaw",
	"_upgradeSocketDestroyRaw",
	"_upgradeSocketEndRaw",
	"_upgradeSocketWriteRaw",
	"_vmCreateContext",
	"_vmRunInContext",
	"_vmRunInThisContext",
	"process.fcntlLock",
	"process.flock",
	"process.getegid",
	"process.geteuid",
	"process.getgid",
	"process.getgrent",
	"process.getgrgid",
	"process.getgrnam",
	"process.getgroups",
	"process.getpwent",
	"process.getpwnam",
	"process.getpwuid",
	"process.getresgid",
	"process.getresuid",
	"process.getuid",
	"process.setegid",
	"process.seteuid",
	"process.setgid",
	"process.setgroups",
	"process.setregid",
	"process.setresgid",
	"process.setresuid",
	"process.setreuid",
	"process.setuid",
	"process.umask",
]);

const exposedGlobals = new Set(
	[...workerSource.matchAll(/exposeCustomGlobal\(\s*"([^"]+)"/g)].map(
		(match) => match[1],
	),
);

const errors = [];

function sorted(values) {
	return [...values].sort((a, b) => a.localeCompare(b));
}

function sameSet(left, right) {
	if (left.size !== right.size) return false;
	for (const value of left) {
		if (!right.has(value)) return false;
	}
	return true;
}

function reportSetDifference(label, left, right) {
	for (const value of sorted(left)) {
		if (!right.has(value)) {
			errors.push(`${value} is ${label}`);
		}
	}
}

function escapeRegExp(value) {
	return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function browserInstalledConvention(name) {
	const start = workerSource.search(
		new RegExp(`exposeCustomGlobal\\(\\s*"${escapeRegExp(name)}"`),
	);
	if (start < 0) return undefined;
	const body = workerSource.slice(start, workerSource.indexOf("\n\t);", start));
	if (body.includes("makeApplySyncPromise(")) return "syncPromise";
	if (body.includes("makeApplySync(")) return "syncOrSyncPromise";
	if (body.includes("makeApplyPromise(") || /\bapply\s*\(/.test(body)) {
		return "async";
	}
	return "unknown";
}

function conventionCompatible(actual, expected) {
	return (
		actual === expected ||
		(actual === "syncOrSyncPromise" &&
			(expected === "sync" || expected === "syncPromise"))
	);
}

function checkInstalledConvention(name) {
	const installedConvention = browserInstalledConvention(name);
	if (
		installedConvention !== undefined &&
		!conventionCompatible(installedConvention, contractMethods.get(name))
	) {
		errors.push(
			`${name} is installed as ${installedConvention} in worker.ts but bridge-contract.json lists ${contractMethods.get(name)}`,
		);
	}
}

for (const name of exposedGlobals) {
	if (!name.startsWith("_") && !name.startsWith("__")) {
		continue;
	}
	if (allowedBrowserOnlyGlobals.has(name)) {
		continue;
	}
	if (contractMethods.has(name)) {
		checkInstalledConvention(name);
	} else {
		errors.push(`${name} is installed from worker.ts but not in bridge-contract.json or the browser-only allowlist`);
	}
}

for (const name of sorted(contractMethods.keys())) {
	if (
		!exposedGlobals.has(name) &&
		!browserFacadeContractGlobals.has(name) &&
		!browserUnsupportedContractGlobals.has(name)
	) {
		errors.push(
			`${name} is listed in bridge-contract.json but is not installed, covered by a browser facade, or explicitly marked unsupported`,
		);
	}
}

for (const name of sorted(browserFacadeContractGlobals)) {
	if (!contractMethods.has(name)) {
		errors.push(`${name} is listed as browser facade-covered but is missing from bridge-contract.json`);
	}
}

for (const name of sorted(browserUnsupportedContractGlobals)) {
	if (!contractMethods.has(name)) {
		errors.push(`${name} is listed as browser-unsupported but is missing from bridge-contract.json`);
	}
}

const runtimeGlobalReferences = new Set(
	[...runtimeSource.matchAll(/\bglobalThis\.(_{1,2}[A-Za-z0-9]+)/g)].map(
		(match) => match[1],
	),
);
for (const name of sorted(runtimeGlobalReferences)) {
	if (allowedBrowserOnlyGlobals.has(name)) {
		continue;
	}
	if (contractMethods.has(name)) {
		if (!exposedGlobals.has(name)) {
			errors.push(`${name} is referenced from runtime.ts but is not installed in worker.ts`);
		} else {
			checkInstalledConvention(name);
		}
	} else {
		errors.push(`${name} is referenced from runtime.ts but not in bridge-contract.json or the browser-only allowlist`);
	}
}

const syncOperationList = syncBridgeSource.match(
	/export const BROWSER_SYNC_BRIDGE_OPERATIONS = \[([\s\S]*?)\] as const;/,
);
if (!syncOperationList) {
	errors.push(
		"BROWSER_SYNC_BRIDGE_OPERATIONS is missing from sync-bridge.ts",
	);
} else {
	const declaredOperations = new Set(
		[...syncOperationList[1].matchAll(/"([^"]+)"/g)].map(
			(match) => match[1],
		),
	);
	const workerOperations = new Set(
		[
			...workerSource.matchAll(
				/syncBridge\.request(?:Void|Text|NullableText|Binary|Json)(?:<[^>]+>)?\(\s*"([^"]+)"/g,
			),
		].map((match) => match[1]),
	);
	const convergedOperations = new Set(
		[
			...convergedSource.matchAll(
				/"((?:fs|module|dgram|pty)\.[a-zA-Z0-9_]+)"/g,
			),
		].map((match) => match[1]),
	);
	// "Handled" = the union of (a) the legacy host-capability fallback in
	// runtime-driver.ts (child_process.* / process.* / network.* via handleSyncBridgeOperation)
	// and (b) the converged servicer's kernel-routed families (fs/module/net/dns/
	// dgram/pty), declared as string literals in the converged-* modules.
	const driverOperations = new Set([
		...[
			...runtimeDriverSource.matchAll(
				/case "((?:crypto|child_process|process|network)\.[^"]+)":/g,
			),
		].map((match) => match[1]),
		// Only the sync-bridge families declared as BROWSER_SYNC_BRIDGE_OPERATIONS:
		// fs/module/dgram/pty. (net.*/dns.lookup ride the separate `guest_kernel_call`
		// channel directly from the worker's net module, not the sync-bridge op list,
		// so they are intentionally NOT declared here.)
		...convergedOperations,
	]);

	const emittableOperations = new Set([
		...workerOperations,
		...convergedOperations,
	]);
	if (!sameSet(emittableOperations, declaredOperations)) {
		reportSetDifference(
			"called by worker.ts but missing from BROWSER_SYNC_BRIDGE_OPERATIONS",
			emittableOperations,
			declaredOperations,
		);
		reportSetDifference(
			"declared in BROWSER_SYNC_BRIDGE_OPERATIONS but not called by worker.ts",
			declaredOperations,
			emittableOperations,
		);
	}
	if (!sameSet(driverOperations, declaredOperations)) {
		reportSetDifference(
			"handled by runtime-driver.ts but missing from BROWSER_SYNC_BRIDGE_OPERATIONS",
			driverOperations,
			declaredOperations,
		);
		reportSetDifference(
			"declared in BROWSER_SYNC_BRIDGE_OPERATIONS but not handled by runtime-driver.ts",
			declaredOperations,
			driverOperations,
		);
	}
}

if (errors.length > 0) {
	console.error("Browser bridge contract drift detected:");
	for (const error of errors) {
		console.error(`  - ${error}`);
	}
	process.exit(1);
}
