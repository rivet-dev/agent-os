import { describe, expect, it } from "vitest";

describe("@rivet-dev/agent-os-posix", () => {
  it("exposes a loadable module entrypoint", async () => {
    await expect(import("../src/index.js")).resolves.toMatchObject({});
  });
});
