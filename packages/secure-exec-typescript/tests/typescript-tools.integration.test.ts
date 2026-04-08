import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import {
	allowAllFs,
	createInMemoryFileSystem,
	createNodeDriver,
	createNodeRuntimeDriverFactory,
} from "secure-exec";
import { describe, expect, it } from "vitest";
import { createTypeScriptTools } from "../src/index.js";

const workspaceRoot = resolve(
	fileURLToPath(new URL("../../..", import.meta.url)),
);

function createTools() {
	const filesystem = createInMemoryFileSystem();
	return {
		filesystem,
		tools: createTypeScriptTools({
			systemDriver: createNodeDriver({
				filesystem,
				moduleAccess: { cwd: workspaceRoot },
				permissions: allowAllFs,
			}),
			runtimeDriverFactory: createNodeRuntimeDriverFactory(),
		}),
	};
}

describe("@secure-exec/typescript", () => {
	it("typechecks a project with node types from node_modules", async () => {
		const { filesystem, tools } = createTools();
		await filesystem.mkdir("/root");
		await filesystem.mkdir("/root/src");
		await filesystem.writeFile(
			"/root/tsconfig.json",
			JSON.stringify({
				compilerOptions: {
					module: "nodenext",
					moduleResolution: "nodenext",
					target: "es2022",
					types: ["node"],
					skipLibCheck: true,
				},
				include: ["src/**/*.ts"],
			}),
		);
		await filesystem.writeFile(
			"/root/src/index.ts",
			'import { Buffer } from "node:buffer";\nexport const output: Buffer = Buffer.from("ok");\n',
		);

		const result = await tools.typecheckProject({ cwd: "/root" });

		expect(result.success).toBe(true);
		expect(result.diagnostics).toEqual([]);
	});

	it("compiles a project into the virtual filesystem and the output executes", async () => {
		const { filesystem, tools } = createTools();
		await filesystem.mkdir("/root");
		await filesystem.mkdir("/root/src");
		await filesystem.writeFile(
			"/root/tsconfig.json",
			JSON.stringify({
				compilerOptions: {
					module: "commonjs",
					target: "es2022",
					outDir: "/root/dist",
				},
				include: ["src/**/*.ts"],
			}),
		);
		await filesystem.writeFile(
			"/root/src/index.ts",
			"export const value: number = 7;\n",
		);

		const compileResult = await tools.compileProject({ cwd: "/root" });

		expect(compileResult.success).toBe(true);
		expect(compileResult.emitSkipped).toBe(false);
		expect(compileResult.emittedFiles).toContain("/root/dist/index.js");
		const emitted = await filesystem.readTextFile("/root/dist/index.js");
		expect(emitted).toContain("exports.value = 7");

		const module = { exports: {} as Record<string, unknown> };
		const execute = new Function("module", "exports", emitted);
		execute(module, module.exports);

		expect(module.exports).toEqual({ value: 7 });
	});

	it("typechecks a source string without mutating the filesystem", async () => {
		const { tools } = createTools();

		const result = await tools.typecheckSource({
			sourceText: "const value: string = 1;\n",
			filePath: "/root/input.ts",
		});

		expect(result.success).toBe(false);
		expect(
			result.diagnostics.some((diagnostic) => diagnostic.code === 2322),
		).toBe(true);
	});

	it("compiles a source string to JavaScript text", async () => {
		const { tools } = createTools();

		const result = await tools.compileSource({
			sourceText: "export const value: number = 3;\n",
			filePath: "/root/input.ts",
			compilerOptions: {
				module: "commonjs",
				target: "es2022",
			},
		});

		expect(result.success).toBe(true);
		expect(result.outputText).toContain("exports.value = 3");
	});

	it("returns a diagnostic when the compiler module cannot be loaded", async () => {
		const brokenTools = createTypeScriptTools({
			systemDriver: createNodeDriver({
				filesystem: createInMemoryFileSystem(),
				moduleAccess: { cwd: workspaceRoot },
				permissions: allowAllFs,
			}),
			runtimeDriverFactory: createNodeRuntimeDriverFactory(),
			compilerSpecifier: "typescript-does-not-exist",
		});

		const result = await brokenTools.typecheckSource({
			sourceText: "export const value = 1;\n",
			filePath: "/root/input.ts",
		});

		expect(result.success).toBe(false);
		expect(result.diagnostics).toEqual([
			expect.objectContaining({
				category: "error",
				code: 0,
				message: expect.stringContaining("typescript-does-not-exist"),
			}),
		]);
	});
});
