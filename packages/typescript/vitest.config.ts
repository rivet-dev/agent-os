import { resolve } from "node:path";
import { defineConfig } from "vitest/config";

export default defineConfig({
	resolve: {
		alias: [
			{
				find: "@rivet-dev/agentos-core/internal/runtime-compat",
				replacement: resolve(__dirname, "../core/dist/runtime-compat.js"),
			},
			{
				find: "@rivet-dev/agentos-internal-typescript",
				replacement: resolve(__dirname, "./src/index.ts"),
			},
		],
	},
	test: {
		testTimeout: 60_000,
		passWithNoTests: true,
	},
});
