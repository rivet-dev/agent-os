import { defineConfig } from "vitest/config";
export default defineConfig({
  test: {
    testTimeout: 30000,
    hookTimeout: 30000,
    include: [
      "tests/e2e/**/*.test.ts",
      "tests/wasmvm/**/*.test.ts",
      "tests/smoke.test.ts",
    ],
  },
});
