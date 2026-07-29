import { describe, expect, it } from "vitest";
import { readFile } from "node:fs/promises";
import { createInMemoryFileSystem } from "../../src/os-filesystem.js";
import {
	moduleFormat,
	POLYFILL_CODE_MAP,
	resolveModule,
} from "../../src/runtime.js";

async function writeJson(
	fs: ReturnType<typeof createInMemoryFileSystem>,
	path: string,
	value: unknown,
) {
	await fs.writeFile(path, JSON.stringify(value));
}

interface ModuleResolutionConformanceFixture {
	cases: Array<{
		name: string;
		files: Record<string, string>;
		resolves: Array<{
			specifier: string;
			from: string;
			mode: "require" | "import";
			expected: string | null;
		}>;
	}>;
	formats: Array<{
		name: string;
		files: Record<string, string>;
		path: string;
		expected: "module" | "commonjs" | "json" | null;
	}>;
}

async function loadModuleResolutionConformanceFixture() {
	const source = await readFile(
		new URL("../../../../tests/fixtures/module-resolution-conformance.json", import.meta.url),
		"utf8",
	);
	return JSON.parse(source) as ModuleResolutionConformanceFixture;
}

describe("browser module resolution", () => {
	it("matches the shared native/browser conformance fixture", async () => {
		const fixture = await loadModuleResolutionConformanceFixture();
		for (const testCase of fixture.cases) {
			const fs = createInMemoryFileSystem();
			for (const [path, contents] of Object.entries(testCase.files)) {
				await fs.writeFile(`/root/${path}`, contents);
			}
			for (const resolution of testCase.resolves) {
				await expect(
					resolveModule(
						resolution.specifier,
						resolution.from,
						fs,
						resolution.mode,
					),
					testCase.name,
				).resolves.toBe(resolution.expected);
			}
		}

		for (const formatCase of fixture.formats) {
			const fs = createInMemoryFileSystem();
			for (const [path, contents] of Object.entries(formatCase.files)) {
				await fs.writeFile(`/root/${path}`, contents);
			}
			await expect(
				moduleFormat(formatCase.path, fs),
				formatCase.name,
			).resolves.toBe(formatCase.expected);
		}
	});

	it("provides browser bridge polyfills for dns builtins and node aliases", () => {
		const loadPolyfill = (moduleName: string) =>
			POLYFILL_CODE_MAP[moduleName.replace(/^node:/, "")] ?? null;

		expect(loadPolyfill("dns")).toContain("_networkDnsLookupRaw");
		expect(loadPolyfill("node:dns")).toContain("_networkDnsLookupRaw");
		expect(loadPolyfill("dns/promises")).toContain("promises");
		expect(loadPolyfill("node:dns/promises")).toContain("promises");
		expect(loadPolyfill("dgram")).toContain("_dgramSocketCreateRaw");
		expect(loadPolyfill("node:dgram")).toContain("_dgramSocketCreateRaw");
		expect(loadPolyfill("agentos:wasi-command-host")).toContain(
			"createWasiCommandHost",
		);
	});

	it("walks ancestor node_modules directories", async () => {
		const fs = createInMemoryFileSystem();
		await fs.writeFile("/workspace/node_modules/pkg/index.js", "module.exports = 1;");

		await expect(
			resolveModule("pkg", "/workspace/src/deep/app.js", fs),
		).resolves.toBe("/workspace/node_modules/pkg/index.js");
	});

	it("resolves scoped packages and package main entries", async () => {
		const fs = createInMemoryFileSystem();
		await writeJson(fs, "/workspace/node_modules/@scope/pkg/package.json", {
			main: "./lib/main.js",
		});
		await fs.writeFile(
			"/workspace/node_modules/@scope/pkg/lib/main.js",
			"module.exports = 1;",
		);

		await expect(
			resolveModule("@scope/pkg", "/workspace/app.js", fs),
		).resolves.toBe("/workspace/node_modules/@scope/pkg/lib/main.js");
	});

	it("resolves package exports conditions by mode", async () => {
		const fs = createInMemoryFileSystem();
		await writeJson(fs, "/workspace/node_modules/pkg/package.json", {
			exports: {
				".": {
					import: "./esm/index.mjs",
					require: "./cjs/index.js",
				},
			},
		});
		await fs.writeFile("/workspace/node_modules/pkg/esm/index.mjs", "export default 1;");
		await fs.writeFile("/workspace/node_modules/pkg/cjs/index.js", "module.exports = 1;");

		await expect(resolveModule("pkg", "/workspace/app.js", fs, "import")).resolves.toBe(
			"/workspace/node_modules/pkg/esm/index.mjs",
		);
		await expect(resolveModule("pkg", "/workspace/app.js", fs, "require")).resolves.toBe(
			"/workspace/node_modules/pkg/cjs/index.js",
		);
	});

	it("uses native resolver condition order instead of browser-only conditions", async () => {
		const fs = createInMemoryFileSystem();
		await writeJson(fs, "/workspace/node_modules/pkg/package.json", {
			exports: {
				".": {
					browser: "./browser.js",
					node: "./node.js",
					default: "./default.js",
				},
			},
		});
		await fs.writeFile("/workspace/node_modules/pkg/browser.js", "module.exports = 'browser';");
		await fs.writeFile("/workspace/node_modules/pkg/node.js", "module.exports = 'node';");
		await fs.writeFile("/workspace/node_modules/pkg/default.js", "module.exports = 'default';");

		await expect(resolveModule("pkg", "/workspace/app.js", fs, "import")).resolves.toBe(
			"/workspace/node_modules/pkg/node.js",
		);
		await expect(resolveModule("pkg", "/workspace/app.js", fs, "require")).resolves.toBe(
			"/workspace/node_modules/pkg/node.js",
		);
	});

	it("resolves exported subpaths and wildcard exports", async () => {
		const fs = createInMemoryFileSystem();
		await writeJson(fs, "/workspace/node_modules/pkg/package.json", {
			exports: {
				"./feature": "./dist/feature.js",
				"./utils/*": "./dist/utils/*.js",
			},
		});
		await fs.writeFile("/workspace/node_modules/pkg/dist/feature.js", "module.exports = 1;");
		await fs.writeFile("/workspace/node_modules/pkg/dist/utils/math.js", "module.exports = 2;");

		await expect(
			resolveModule("pkg/feature", "/workspace/app.js", fs),
		).resolves.toBe("/workspace/node_modules/pkg/dist/feature.js");
		await expect(
			resolveModule("pkg/utils/math", "/workspace/app.js", fs),
		).resolves.toBe("/workspace/node_modules/pkg/dist/utils/math.js");
	});

	it("returns real paths for symlinked node_modules packages", async () => {
		const fs = createInMemoryFileSystem();
		await fs.writeFile("/store/pkg/index.js", "module.exports = 1;");
		await fs.symlink("/store/pkg", "/workspace/node_modules/pkg");

		await expect(resolveModule("pkg", "/workspace/app.js", fs)).resolves.toBe(
			"/store/pkg/index.js",
		);
	});

	it("resolves file URL specifiers", async () => {
		const fs = createInMemoryFileSystem();
		await fs.writeFile("/workspace/space name.js", "module.exports = 1;");

		await expect(
			resolveModule("file:///workspace/space%20name.js?cache=1", "/workspace/app.js", fs),
		).resolves.toBe("/workspace/space name.js");
		await expect(
			resolveModule("file://remote/workspace/app.js", "/workspace/app.js", fs),
		).resolves.toBeNull();
	});

	it("resolves package imports from nearest package.json", async () => {
		const fs = createInMemoryFileSystem();
		await writeJson(fs, "/workspace/package.json", {
			imports: {
				"#config": "./src/config.js",
				"#utils/*": "./src/utils/*.js",
			},
		});
		await fs.writeFile("/workspace/src/config.js", "export const value = 1;");
		await fs.writeFile("/workspace/src/utils/math.js", "export const value = 2;");

		await expect(
			resolveModule("#config", "/workspace/src/app.js", fs, "import"),
		).resolves.toBe("/workspace/src/config.js");
		await expect(
			resolveModule("#utils/math", "/workspace/src/app.js", fs, "import"),
		).resolves.toBe("/workspace/src/utils/math.js");
		await expect(
			resolveModule("#missing", "/workspace/src/app.js", fs, "import"),
		).resolves.toBeNull();
	});

	it("falls back to root node_modules conventions", async () => {
		const fs = createInMemoryFileSystem();
		await fs.writeFile("/root/node_modules/pkg/index.js", "module.exports = 1;");
		await fs.writeFile("/node_modules/other/index.js", "module.exports = 2;");

		await expect(resolveModule("pkg", "/workspace/app.js", fs)).resolves.toBe(
			"/root/node_modules/pkg/index.js",
		);
		await expect(resolveModule("other", "/workspace/app.js", fs)).resolves.toBe(
			"/node_modules/other/index.js",
		);
	});

	it("detects module formats using extensions and nearest package type", async () => {
		const fs = createInMemoryFileSystem();
		await writeJson(fs, "/workspace/package.json", { type: "module" });
		await writeJson(fs, "/workspace/cjs/package.json", { type: "commonjs" });

		await expect(moduleFormat("/workspace/lib/index.mjs", fs)).resolves.toBe(
			"module",
		);
		await expect(moduleFormat("/workspace/lib/index.cjs", fs)).resolves.toBe(
			"commonjs",
		);
		await expect(moduleFormat("/workspace/lib/data.json", fs)).resolves.toBe(
			"json",
		);
		await expect(moduleFormat("/workspace/lib/app.js", fs)).resolves.toBe(
			"module",
		);
		await expect(moduleFormat("/workspace/cjs/app.js", fs)).resolves.toBe(
			"commonjs",
		);
		await expect(moduleFormat("/workspace/lib/app.txt", fs)).resolves.toBeNull();
	});
});
