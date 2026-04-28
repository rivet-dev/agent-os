import { existsSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, it } from "vitest";

const __dirname = dirname(fileURLToPath(import.meta.url));

/** Directory containing WASM command binaries built from Rust. */
export const COMMANDS_DIR = resolve(
  __dirname,
  "../native/target/wasm32-wasip1/release/commands",
);

/** Directory containing C-compiled WASM binaries. */
export const C_BUILD_DIR = resolve(__dirname, "../native/c/build/");

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
    return `WASM binaries not found at ${COMMANDS_DIR} — build with \`make wasm\` first`;
  }
  return false;
}

export function describeIf(
	condition: unknown,
	...args: Parameters<typeof describe>
): void {
	if (condition) {
		// Vitest's overloaded tuple shape is awkward to preserve across helper forwarding.
		// @ts-expect-error forwarded describe() arguments stay runtime-compatible.
		describe(...args);
		return;
	}
	const [name] = args;
	describe(String(name), () => {
		it('environment prerequisites not met', () => {});
	});
}

export function itIf(
	condition: unknown,
	...args: Parameters<typeof it>
): void {
	if (condition) {
		// Vitest's overloaded tuple shape is awkward to preserve across helper forwarding.
		// @ts-expect-error forwarded it() arguments stay runtime-compatible.
		it(...args);
		return;
	}
	const [name] = args;
	it(String(name), () => {});
}

// Re-exports from the repo-owned Agent OS test runtime surface.
export {
  AF_INET,
  AF_UNIX,
  allowAll,
  createInMemoryFileSystem,
  SIGTERM,
  SOCK_DGRAM,
  SOCK_STREAM,
} from "@rivet-dev/agent-os-core/test/runtime";
import {
	allowAll,
	createKernel as createKernelBase,
} from "@rivet-dev/agent-os-core/test/runtime";
export type {
  DriverProcess,
  Kernel,
  KernelInterface,
  KernelRuntimeDriver,
  ProcessContext,
  VirtualFileSystem,
} from "@rivet-dev/agent-os-core/test/runtime";
export {
	createWasmVmRuntime,
	DEFAULT_FIRST_PARTY_TIERS,
	WASMVM_COMMANDS,
	type PermissionTier,
	type WasmVmRuntimeOptions,
} from "@rivet-dev/agent-os-core/test/runtime";
export {
  createNodeHostNetworkAdapter,
  createNodeRuntime,
  NodeFileSystem,
  TerminalHarness,
} from "@rivet-dev/agent-os-core/test/runtime";

/**
 * Registry integration tests assume they can bootstrap runtimes and /bin stubs
 * unless they explicitly opt into a stricter permission policy.
 */
export function createKernel(
	options: Parameters<typeof createKernelBase>[0],
): ReturnType<typeof createKernelBase> {
	return createKernelBase({
		...options,
		permissions: options.permissions ?? allowAll,
	});
}
