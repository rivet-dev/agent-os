import { defineConfig } from "vitest/config";

export default defineConfig({
	test: {
		// The core suite includes multiple heavyweight ACP integration tests
		// that spawn full agent runtimes. Running files concurrently causes
		// intermittent SIGKILLs and early agent exits under resource pressure.
		fileParallelism: false,
		hookTimeout: 30000,
		setupFiles: ["tests/helpers/default-vm-permissions.ts"],
		testTimeout: 30000,
		include: ["tests/**/*.test.ts"],
	},
});
