import { expect, test } from "vitest";

test("supports snapshots", () => {
  expect({ runtime: "agentos", supported: true }).toMatchInlineSnapshot(`
    {
      "runtime": "agentos",
      "supported": true,
    }
  `);
});
