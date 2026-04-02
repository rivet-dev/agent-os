import { describe, it, expect, afterEach } from "vitest";
import {
  createInMemoryFileSystem,
  createKernel,
  createWasmVmRuntime,
  hasWasmBinaries,
  skipReason,
  COMMANDS_DIR,
} from "./helpers.ts";
import type { Kernel } from "./helpers.ts";

describe.skipIf(skipReason())("smoke", () => {
  let kernel: Kernel;

  afterEach(async () => {
    await kernel?.dispose();
  });

  it("echo hello returns expected stdout", async () => {
    const vfs = createInMemoryFileSystem();
    kernel = createKernel({ filesystem: vfs });
    await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

    const result = await kernel.exec("echo hello");
    expect(result.exitCode).toBe(0);
    expect(result.stdout.trim()).toBe("hello");
  });
});
