import { defineConfig } from "vitest/config";

export default defineConfig({
	test: {
		// The dev-shell suite spins up full Wasm/Node runtimes and the justfile wrapper.
		// Running files concurrently can produce intermittent crashes under workspace load.
		fileParallelism: false,
		include: ["test/**/*.test.ts"],
		testTimeout: 60000,
	},
});
