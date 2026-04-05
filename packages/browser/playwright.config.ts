import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
	testDir: "./tests/browser",
	timeout: 30_000,
	use: {
		baseURL: "http://localhost:4173",
		trace: "retain-on-failure",
	},
	webServer: {
		command: "pnpm build && pnpm --dir ../playground dev",
		port: 4173,
		reuseExistingServer: !process.env.CI,
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
