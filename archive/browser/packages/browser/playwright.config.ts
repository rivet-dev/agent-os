import { defineConfig, devices } from "@playwright/test";

const PLAYGROUND_PORT = 43173;

export default defineConfig({
	testDir: "./tests/browser",
	timeout: 30_000,
	use: {
		baseURL: `http://localhost:${PLAYGROUND_PORT}`,
		trace: "retain-on-failure",
	},
	webServer: {
		command: `sh -c 'PORT=${PLAYGROUND_PORT} pnpm build && PORT=${PLAYGROUND_PORT} pnpm --dir ../playground dev'`,
		port: PLAYGROUND_PORT,
		reuseExistingServer: false,
		timeout: 120_000,
	},
	projects: [
		{
			name: "chromium",
			use: {
				...devices["Desktop Chrome"],
			},
		},
	],
});
