// Converged driver servicer assembly.
//
// Bundles the converged executor pieces (session bootstrap, fs/net/dns handler,
// kernel-backed module resolver, router) into a single servicer the runtime
// driver uses to satisfy a guest's sync-bridge requests against the wasm kernel.
//
// The runtime driver loads this DYNAMICALLY (only when a converged sidecar is
// configured) so the legacy, unbundled `/dist/` driver load never pulls in the
// bare `@rivet-dev/agentos-runtime-core/*` imports these modules need — those resolve only
// when the converged runtime is esbuild-bundled.

import type { CreateVmConfig } from "@rivet-dev/agentos-runtime-core/vm-config";
import type { ProtocolFramePayloadCodec } from "@rivet-dev/agentos-runtime-core/protocol-frames";
import { isConvergedDgramBridgeOperation } from "./converged-dgram-bridge.js";
import type { ConvergedSyncResponse } from "./converged-fs-bridge.js";
import { ConvergedExecutorSession } from "./converged-executor-session.js";
import { ConvergedModuleServicer } from "./converged-module-servicer.js";
import { isConvergedNetBridgeOperation } from "./converged-net-bridge.js";
import { isConvergedPtyBridgeOperation } from "./converged-pty-bridge.js";
import {
	ConvergedSyncBridgeRouter,
	type LegacySyncBridgeServicer,
} from "./converged-sync-bridge-router.js";
import { KernelBackedFilesystem } from "./kernel-backed-filesystem.js";

export interface ConvergedServicerOptions {
	pushFrame: (frame: Uint8Array) => Uint8Array;
	config: CreateVmConfig;
	codec?: ProtocolFramePayloadCodec;
	// Sets the execution id the sidecar's execution host bridge will echo for the
	// next `execute`, so a guest execution registers a kernel process under the
	// same id its sync-bridge `guest_kernel_call`s use. Required for guest
	// net/dgram (which need a kernel pid); fs/module are VM-scoped and skip it.
	setNextExecutionId?: (executionId: string) => void;
	// Observability: invoked when the kernel denies a guest fs read with EACCES,
	// so callers can surface a denied-read count (the converged kernel enforces
	// in-band, replacing the legacy TS permission-callback counter).
	onFsReadDenied?: () => void;
}

const FS_READ_OPERATIONS = new Set([
	"fs.readFile",
	"fs.readFileBinary",
	"fs.pread",
	"fs.readDir",
	"fs.stat",
	"fs.lstat",
	"fs.exists",
	"fs.realpath",
	"fs.readlink",
]);

export interface ConvergedServicer {
	/**
	 * Service one guest sync-bridge operation: fs/net/dns to the wasm handler,
	 * module.* to the kernel-backed resolver, everything else to `legacy`.
	 */
	route(
		executionId: string,
		operation: string,
		args: readonly unknown[],
		legacy: LegacySyncBridgeServicer,
	): Promise<ConvergedSyncResponse>;
	/** Snapshot the VM root filesystem (for host persistence, e.g. OPFS). */
	snapshotRootFilesystem(): ReturnType<
		ConvergedExecutorSession["snapshotRootFilesystem"]
	>;
}

export function createConvergedServicer(
	options: ConvergedServicerOptions,
): ConvergedServicer {
	const session = new ConvergedExecutorSession({
		pushFrame: options.pushFrame,
		codec: options.codec,
	});
	session.bootstrap({ runtime: "java_script", config: options.config });
	const moduleServicer = new ConvergedModuleServicer(
		new KernelBackedFilesystem(session.transportForVm()),
	);
	const registeredExecutions = new Set<string>();

	const ensureExecutionRegistered = (executionId: string) => {
		if (registeredExecutions.has(executionId) || !options.setNextExecutionId) {
			return;
		}
		options.setNextExecutionId(executionId);
		session.registerExecution({ processId: executionId, args: ["node"] });
		registeredExecutions.add(executionId);
	};

	return {
		async route(executionId, operation, args, legacy) {
			// Guest net/dgram/pty need a kernel process (pid); register it lazily on
			// first use so the guest_kernel_call resolves execution_id -> pid.
			if (
				isConvergedNetBridgeOperation(operation) ||
				isConvergedDgramBridgeOperation(operation) ||
				isConvergedPtyBridgeOperation(operation)
			) {
				ensureExecutionRegistered(executionId);
			}
			const router = new ConvergedSyncBridgeRouter({
				handler: session.handlerForExecution(executionId),
				asyncServicers: [moduleServicer],
				legacy,
			});
			try {
				return await router.route(operation, args);
			} catch (error) {
				if (
					options.onFsReadDenied &&
					FS_READ_OPERATIONS.has(operation) &&
					(error as { code?: string })?.code === "EACCES"
				) {
					options.onFsReadDenied();
				}
				throw error;
			}
		},
		snapshotRootFilesystem() {
			return session.snapshotRootFilesystem();
		},
	};
}
