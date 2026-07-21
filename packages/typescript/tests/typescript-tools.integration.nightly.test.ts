import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createTypeScriptTools } from "@rivet-dev/agentos-internal-typescript";
import {
	allowAllFs,
	createKernel,
	createNodeDriver,
	createNodeRuntime,
	createNodeRuntimeDriverFactory,
	nodeModulesMount,
	type NodeRuntimeDriverFactory,
} from "@rivet-dev/agentos-core/internal/runtime-compat";
import { createInMemoryFileSystem } from "@rivet-dev/agentos-core/test/runtime";
import { describe, expect, it } from "vitest";

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
				mounts: [nodeModulesMount(join(workspaceRoot, "node_modules"))],
				permissions: allowAllFs,
			}),
			runtimeDriverFactory: createNodeRuntimeDriverFactory(),
		}),
	};
}

describe("AgentOS internal TypeScript compiler", () => {
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

		expect(result).toEqual({
			success: true,
			diagnostics: [],
		});
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

		expect(compileResult).toEqual({
			success: true,
			diagnostics: [],
			emitSkipped: false,
			emittedFiles: ["/root/dist/index.js"],
		});
		expect(compileResult.emittedFiles).toContain("/root/dist/index.js");
		const emitted = await filesystem.readTextFile("/root/dist/index.js");
		expect(emitted).toContain("exports.value = 7");

		const kernel = createKernel({
			filesystem,
			permissions: {
				fs: allowAllFs,
				childProcess: {
					default: "deny",
					rules: [{ mode: "allow", operations: ["*"], patterns: ["node"] }],
				},
			},
			syncFilesystemOnDispose: false,
		});
		let stdout = "";
		let stderr = "";
		try {
			await kernel.mount(createNodeRuntime());
			const child = kernel.spawn(
				"node",
				[
					"-e",
					"const value = require('/root/dist/index.js').value; console.log(JSON.stringify({ value }));",
				],
				{
					onStdout: (chunk) => {
						stdout += Buffer.from(chunk).toString("utf8");
					},
					onStderr: (chunk) => {
						stderr += Buffer.from(chunk).toString("utf8");
					},
				},
			);
			expect(await child.wait()).toBe(0);
		} finally {
			await kernel.dispose();
		}

		expect(stderr).toBe("");
		expect(JSON.parse(stdout)).toEqual({ value: 7 });
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

	it("uses a supplied runtime driver when one is available", async () => {
		const filesystem = createInMemoryFileSystem();
		let runs = 0;
		let disposed = false;
		const runtimeDriverFactory: NodeRuntimeDriverFactory = {
			createRuntimeDriver: () => ({
				exec: async () => ({ exitCode: 0, stdout: "", stderr: "" }),
				run: async () => {
					runs += 1;
					return {
						code: 0,
						value: {
							ok: true as const,
							result: {
								success: true,
								diagnostics: [],
							},
						},
					};
				},
				dispose: () => {
					disposed = true;
				},
			}),
		};
		const tools = createTypeScriptTools({
			systemDriver: createNodeDriver({
				filesystem,
				permissions: allowAllFs,
			}),
			runtimeDriverFactory,
		});

		await expect(
			tools.typecheckSource({
				sourceText: "const value: number = 1;\n",
				filePath: "/root/input.ts",
			}),
		).resolves.toEqual({
			success: true,
			diagnostics: [],
		});
		expect(runs).toBe(1);
		expect(disposed).toBe(true);
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
		expect(result.diagnostics).toEqual([]);
		expect(result.outputText).toContain("exports.value = 3");
	});

	it("returns a diagnostic when the compiler module cannot be loaded", async () => {
		const brokenTools = createTypeScriptTools({
			systemDriver: createNodeDriver({
				filesystem: createInMemoryFileSystem(),
				mounts: [nodeModulesMount(join(workspaceRoot, "node_modules"))],
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
