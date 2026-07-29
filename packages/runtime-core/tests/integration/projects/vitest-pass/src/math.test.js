import { beforeEach, describe, expect, test } from "vitest";
import { add, delayedValue } from "./math.js";

describe("math", () => {
  let calls;

  beforeEach(() => {
    calls = 0;
  });

  test("runs assertions and hooks", () => {
    calls += 1;
    expect(add(20, 22)).toBe(42);
    expect(calls).toBe(1);
  });

  test("awaits asynchronous tests", async () => {
    expect(await delayedValue("ready")).toBe("ready");
  });
});
