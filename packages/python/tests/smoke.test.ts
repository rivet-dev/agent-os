import { describe, expect, it } from "vitest";

describe("@rivet-dev/agent-os-python", () => {
  it("exposes the declared module entrypoints", async () => {
    await expect(import("../src/index.js")).resolves.toMatchObject({});
    await expect(import("../src/driver.js")).resolves.toMatchObject({});
    await expect(import("../src/kernel-runtime.js")).resolves.toMatchObject({});
  });
});
