import { describe, expect, it } from "vitest";
import {
	sourceHasModuleSyntax,
	transformSourceForImportSync,
	transformSourceForRequireSync,
} from "../src/module-source.ts";

describe("module source transforms", () => {
	it("normalizes shebang ESM entrypoints before require-mode wrapping", () => {
		const source = [
			"#!/usr/bin/env node",
			'import { main } from "./main.js";',
			"main();",
		].join("\n");

		const transformed = transformSourceForRequireSync(source, "/pkg/dist/cli.js");

		expect(transformed.startsWith("#!")).toBe(false);
		expect(transformed).not.toContain("#!/usr/bin/env node");
		expect(transformed.startsWith("/*__secure_exec_require_esm__*/")).toBe(true);
		expect(transformed).toContain('require("./main.js")');
		expect(() =>
			new Function(
				"exports",
				"require",
				"module",
				"__secureExecFilename",
				"__secureExecDirname",
				"__dynamicImport",
				transformed,
			),
		).not.toThrow();
	});

	it("normalizes shebang ESM entrypoints for import-mode passthrough", () => {
		const source = [
			"#!/usr/bin/env node",
			'import { main } from "./main.js";',
			"main();",
		].join("\n");

		const transformed = transformSourceForImportSync(source, "/pkg/dist/cli.js");

		expect(transformed.startsWith("#!")).toBe(false);
		expect(transformed.startsWith("///usr/bin/env node")).toBe(true);
		expect(transformed).toContain('import { main } from "./main.js";');
	});

	it("detects module syntax when a BOM-prefixed shebang is present", async () => {
		const source = '\uFEFF#!/usr/bin/env node\nimport "./main.js";\n';

		await expect(sourceHasModuleSyntax(source, "/pkg/dist/cli.js")).resolves.toBe(true);
	});
});
