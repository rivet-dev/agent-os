import { existsSync, statSync } from "node:fs";
import { resolve } from "node:path";
import { describe, it } from "vitest";

/** Directory containing WASM command binaries built from Rust. */
export const COMMANDS_DIR = resolve(
	process.env.AGENTOS_WASM_COMMANDS_DIR ??
		resolve(import.meta.dirname, "../../runtime-core/commands"),
);

/** Directory containing C-compiled WASM binaries. */
export const C_BUILD_DIR = resolve(
	process.env.AGENTOS_C_WASM_COMMANDS_DIR ??
		resolve(import.meta.dirname, "../../../toolchain/c/build"),
);

/** Whether the main WASM command binaries are available (includes 'sh'). */
export const hasWasmBinaries =
	existsSync(COMMANDS_DIR) && existsSync(resolve(COMMANDS_DIR, "sh"));

/**
 * Check whether specific C WASM binaries are present.
 * @param names - Binary names to check for inside C_BUILD_DIR.
 * @returns true if all requested binaries exist.
 */
export function hasCWasmBinaries(...names: string[]): boolean {
	if (!existsSync(C_BUILD_DIR)) return false;
	return names.every((name) => existsSync(resolve(C_BUILD_DIR, name)));
}

/**
 * Returns a skip-reason string if WASM binaries are missing, or false if
 * they are available and tests should run.
 */
export function skipReason(): string | false {
	if (!hasWasmBinaries) {
		return `WASM binaries not found at ${COMMANDS_DIR} - build toolchain first`;
	}
	return false;
}

export function describeIf(
	condition: unknown,
	...args: Parameters<typeof describe>
): void {
	if (condition) {
		describe(...args);
		return;
	}
	const [name] = args;
	describe.skip(`${String(name)} [environment prerequisites not met]`, () => {});
}

export function itIf(condition: unknown, ...args: Parameters<typeof it>): void {
	if (condition) {
		it(...args);
		return;
	}
	const [name] = args;
	it.skip(`${String(name)} [environment prerequisites not met]`, () => {});
}

export {
	AF_INET,
	AF_UNIX,
	allowAll,
	createInMemoryFileSystem,
	SIGTERM,
	SOCK_DGRAM,
	SOCK_STREAM,
} from "../../runtime-core/src/test-runtime.js";

import {
	allowAll,
	createInMemoryFileSystem,
	createKernel as createKernelBase,
	createNodeRuntime,
	createWasmVmRuntime,
	NodeFileSystem,
} from "../../runtime-core/src/test-runtime.js";

export type {
	DriverProcess,
	Kernel,
	KernelInterface,
	KernelRuntimeDriver,
	Permissions,
	ProcessContext,
	VirtualFileSystem,
} from "../../runtime-core/src/test-runtime.js";
export {
	createNodeHostNetworkAdapter,
	createNodeRuntime,
	createWasmVmRuntime,
	DEFAULT_FIRST_PARTY_TIERS,
	NodeFileSystem,
	type PermissionTier,
	WASMVM_COMMANDS,
	type WasmVmRuntimeOptions,
} from "../../runtime-core/src/test-runtime.js";
export { TerminalHarness } from "./terminal-harness.js";

type TestWasmBackend = "v8" | "wasmtime";

function configuredTestWasmBackend(): TestWasmBackend | undefined {
	const backend = process.env.AGENTOS_TEST_WASM_BACKEND;
	if (backend === undefined || backend === "v8" || backend === "wasmtime") {
		return backend;
	}
	throw new Error(
		`AGENTOS_TEST_WASM_BACKEND must be "v8" or "wasmtime", got ${JSON.stringify(backend)}`,
	);
}

/**
 * Keep existing V8 regression ceilings while allowing Wasmtime's measured
 * debug-mode cold compilation cost in dual-backend integration suites.
 */
export function wasmBackendTestTimeout(
	v8TimeoutMs: number,
	wasmtimeTimeoutMs: number,
): number {
	return configuredTestWasmBackend() === "wasmtime"
		? wasmtimeTimeoutMs
		: v8TimeoutMs;
}

/**
 * Registry integration tests assume they can bootstrap runtimes and /bin stubs
 * unless they explicitly opt into a stricter permission policy.
 */
export function createKernel(
	options: Parameters<typeof createKernelBase>[0],
): ReturnType<typeof createKernelBase> {
	// Node-backed fixtures retain their host numeric ownership in the VM
	// snapshot. Match that owner by default so tests do not depend on the
	// runner account happening to use agentOS's usual uid/gid 1000.
	const fixtureOwner =
		options.filesystem instanceof NodeFileSystem
			? statSync(options.filesystem.rootPath)
			: undefined;
	return createKernelBase({
		...options,
		permissions: options.permissions ?? allowAll,
		user:
			options.user ??
			(fixtureOwner
				? {
						uid: fixtureOwner.uid,
						gid: fixtureOwner.gid,
						euid: fixtureOwner.uid,
						egid: fixtureOwner.gid,
					}
				: undefined),
		wasmBackend: options.wasmBackend ?? configuredTestWasmBackend(),
	});
}

export interface IntegrationKernelResult {
	kernel: ReturnType<typeof createKernelBase>;
	vfs: ReturnType<typeof createInMemoryFileSystem>;
	dispose: () => Promise<void>;
}

export interface IntegrationKernelOptions {
	runtimes?: ("wasmvm" | "node")[];
	loopbackExemptPorts?: number[];
	commandDirs?: string[];
	permissions?: Parameters<typeof createKernelBase>[0]["permissions"];
	/** VM-wide engine used by standalone WASM commands in this test kernel. */
	wasmBackend?: "v8" | "wasmtime" | "wasmtime-threads";
}

/**
 * Create a kernel with the in-scope runtime drivers for integration testing.
 *
 * Mount order matters. Last-mounted driver wins for overlapping commands:
 *   1. WasmVM first: provides sh/bash/coreutils (90+ commands)
 *   2. Node second: overrides WasmVM's 'node' stub with real V8
 */
export async function createIntegrationKernel(
	options?: IntegrationKernelOptions,
): Promise<IntegrationKernelResult> {
	const runtimes = options?.runtimes ?? ["wasmvm"];
	const vfs = createInMemoryFileSystem();
	const kernel = createKernel({
		filesystem: vfs,
		loopbackExemptPorts: options?.loopbackExemptPorts,
		permissions: options?.permissions,
		wasmBackend: options?.wasmBackend,
	});

	if (runtimes.includes("wasmvm")) {
		await kernel.mount(
			createWasmVmRuntime({
				commandDirs: options?.commandDirs ?? [COMMANDS_DIR],
			}),
		);
	}
	if (runtimes.includes("node")) {
		await kernel.mount(createNodeRuntime());
	}

	return {
		kernel,
		vfs,
		dispose: () => kernel.dispose(),
	};
}

/**
 * Skip helper: returns a reason string if the WASM binaries are not built,
 * or false if the commands directory exists and tests can run.
 */
export function skipUnlessWasmBuilt(): string | false {
	return skipReason();
}
