/**
 * Integration test helpers for kernel tests that depend on WASM command binaries.
 *
 * Re-exports infrastructure from the parent helpers.ts and provides
 * createIntegrationKernel / skipUnlessWasmBuilt for cross-runtime tests.
 */

import {
  AF_INET,
  AF_UNIX,
  COMMANDS_DIR,
  C_BUILD_DIR,
  hasWasmBinaries,
  NodeFileSystem,
  SIGTERM,
  SOCK_DGRAM,
  SOCK_STREAM,
  TerminalHarness,
  skipReason,
  createInMemoryFileSystem,
  createKernel,
  createWasmVmRuntime,
  createNodeRuntime,
} from "../helpers.js";
import type { Kernel, VirtualFileSystem } from "../helpers.js";

export {
  AF_INET,
  AF_UNIX,
  COMMANDS_DIR,
  C_BUILD_DIR,
  hasWasmBinaries,
  NodeFileSystem,
  SIGTERM,
  SOCK_DGRAM,
  SOCK_STREAM,
  TerminalHarness,
  skipReason,
  createInMemoryFileSystem,
  createKernel,
  createWasmVmRuntime,
  createNodeRuntime,
} from "../helpers.js";
export type { Kernel, VirtualFileSystem } from "../helpers.js";

export interface IntegrationKernelResult {
  kernel: Kernel;
  vfs: VirtualFileSystem;
  dispose: () => Promise<void>;
}

export interface IntegrationKernelOptions {
  runtimes?: ("wasmvm" | "node")[];
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
  const kernel = createKernel({ filesystem: vfs });

  if (runtimes.includes("wasmvm")) {
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));
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
