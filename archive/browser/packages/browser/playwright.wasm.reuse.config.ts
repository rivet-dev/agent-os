import { defineConfig, devices } from "@playwright/test";

// Local-only variant of playwright.wasm.config.ts that reuses an already-running
// serve.mjs (assets pre-built by hand) instead of rebuilding the heavy pi bundles per
// run. Avoids re-bundling the 16MB pi SDK on every iteration during PTY-terminal work.
const PORT = Number(process.env.AGENTOS_WASM_TEST_PORT ?? 43175);

export default defineConfig({
	testDir: "./tests/browser-wasm",
	timeout: 60_000,
	use: { baseURL: `http://localhost:${PORT}`, trace: "retain-on-failure" },
	projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
});
