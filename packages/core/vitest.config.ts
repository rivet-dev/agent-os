import { configDefaults, defineConfig } from "vitest/config";

// Real-API, real-install matrix (agent × package manager). Hits a live LLM API
// and runs real npm/pnpm/yarn/bun installs, so it remains explicitly opt-in.
const MATRIX_E2E_FILES = ["tests/agent-pkg-matrix.e2e.test.ts"];

const runMatrixE2e = process.env.AGENTOS_MATRIX_E2E === "1";

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
		exclude: [
			...configDefaults.exclude,
			...(runMatrixE2e ? [] : MATRIX_E2E_FILES),
		],
	},
});
