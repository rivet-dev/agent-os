import { createHash } from "node:crypto";
import { execFile, spawn } from "node:child_process";
import {
	copyFile,
	mkdtemp,
	mkdir,
	readFile,
	readdir,
	writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { promisify } from "node:util";
import { describe, expect, test } from "vitest";
import { runnerSource } from "../../../packages/agentos-apps/src/runtime.js";

const execFileAsync = promisify(execFile);
const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const repositoryRoot = resolve(packageRoot, "../..");
const builder = join(packageRoot, "cli", "apps-builder.mjs");
const rivetKitTarball = process.env.AGENTOS_APPS_RIVETKIT_TARBALL;

describe("apps-builder", () => {
	test("emits a minimal executable TypeScript release with static assets", async () => {
		const root = await mkdtemp(join(tmpdir(), "agentos-apps-builder-"));
		const workspace = join(root, "workspace");
		const release = join(root, "release");
		await mkdir(join(workspace, "src"), { recursive: true });
		await mkdir(join(workspace, "public"), { recursive: true });
		await writeFile(
			join(workspace, "entry.ts"),
			[
				'import { greeting } from "./src/app.ts";',
				'import query from "#query";',
				'import wasmPath from "./src/module.wasm";',
				"export default { greeting, query, wasmPath };",
			].join("\n"),
		);
		await writeFile(
			join(workspace, "package.json"),
			JSON.stringify({
				type: "module",
				imports: {
					"#query": {
						node: "./src/query.sql",
						default: "./src/missing.sql",
					},
				},
			}),
		);
		await writeFile(
			join(workspace, "src", "app.ts"),
			'export const greeting: string = "hello from AgentOS Apps";\n',
		);
		await writeFile(
			join(workspace, "src", "query.sql"),
			"select 'hello from sqlite';\n",
		);
		await writeFile(
			join(workspace, "src", "module.wasm"),
			new Uint8Array([0, 97, 115, 109, 1, 0, 0, 0]),
		);
		await writeFile(join(workspace, "public", "index.html"), "<h1>Hello</h1>\n");
		await writeFile(
			join(workspace, "package-lock.json"),
			'{"must":"not ship"}\n',
		);
		const configPath = join(root, "config.json");
		await writeFile(
			configPath,
			JSON.stringify({
				workspace,
				release,
				entrypoint: "entry.ts",
				version: "release-test",
				staticRoot: "public",
				sourceFiles: [
					"src/app.ts",
					"src/query.sql",
					"src/module.wasm",
					"public/index.html",
				],
				usesRivetKit: false,
				maxOutputBytes: 1024 * 1024,
				maxOutputFiles: 32,
				maxFileBytes: 512 * 1024,
			}),
		);

		await execFileAsync(process.execPath, [builder, configPath]);

		const paths = await listFiles(release);
		const wasmPath = paths.find(
			(path) => path.startsWith("modules/module-") && path.endsWith(".wasm"),
		);
		expect(paths).toEqual([
			"agentos-package.json",
			"main.mjs",
			"manifest.json",
			wasmPath,
			"public/index.html",
		]);
		expect(paths).not.toContain("package-lock.json");
		expect(paths.some((path) => path.startsWith("src/"))).toBe(false);
		expect(paths.some((path) => path.startsWith("node_modules/"))).toBe(false);

		const loaded = await import(
			`${pathToFileURL(join(release, "main.mjs")).href}?test=${Date.now()}`
		);
		expect(loaded.default).toEqual({
			greeting: "hello from AgentOS Apps",
			query: "select 'hello from sqlite';\n",
			wasmPath: expect.stringMatching(
				/^\.\/modules\/module-[A-Z0-9]+\.wasm$/,
			),
		});

		const manifest = JSON.parse(
			await readFile(join(release, "manifest.json"), "utf8"),
		);
		expect(manifest.version).toBe(1);
		expect(manifest.mainModule).toBe("main.mjs");
		expect(manifest.modules).toHaveLength(2);
		expect(manifest.assets).toHaveLength(1);
		const main = await readFile(join(release, "main.mjs"));
		expect(manifest.modules[0]).toMatchObject({
			path: "main.mjs",
			size: main.byteLength,
			hash: createHash("sha256").update(main).digest("hex"),
		});
	});

	test.skipIf(!rivetKitTarball)(
		"bundles a real RivetKit application without native runtime packages",
		async () => {
		const root = await mkdtemp(join(tmpdir(), "agentos-apps-rivetkit-builder-"));
		const workspace = join(root, "workspace");
		const release = join(root, "release");
		await mkdir(join(workspace, "src"), { recursive: true });
		await mkdir(join(workspace, "vendor"), { recursive: true });
		await copyFile(rivetKitTarball!, join(workspace, "vendor", "rivetkit.tgz"));
		await writeFile(
			join(workspace, "package.json"),
			JSON.stringify({
				private: true,
				type: "module",
				dependencies: {
					rivetkit: "file:./vendor/rivetkit.tgz",
					"@rivetkit/rivetkit-wasm": "2.3.9",
				},
				overrides: {
					"@rivet-dev/agent-os-core": "npm:empty-npm-package@1.0.0",
					"@rivetkit/engine-cli": "npm:empty-npm-package@1.0.0",
					"@rivetkit/rivetkit-napi": "npm:empty-npm-package@1.0.0",
				},
			}),
		);
		await writeFile(
			join(workspace, "runner.mjs"),
			runnerSource({
				entrypoint: "src/index.mjs",
				release: "rivetkit-test",
				port: 3080,
				maxRequestBytes: 1024 * 1024,
				maxResponseBytes: 1024 * 1024,
				usesRivetKit: true,
			}),
		);
		await writeFile(
			join(workspace, "src", "index.mjs"),
			[
				'import { actor, setup } from "rivetkit";',
				"export const counter = actor({",
				"  state: { count: 0 },",
				"  actions: { increment: (c) => ++c.state.count },",
				"});",
				"export const registry = setup({ use: { counter } });",
				"registry.start();",
				'export default () => new Response("hello");',
			].join("\n"),
		);
		const configPath = join(root, "config.json");
		await writeFile(
			configPath,
			JSON.stringify({
				workspace,
				release,
				entrypoint: "runner.mjs",
				version: "rivetkit-test",
				sourceFiles: ["src/index.mjs"],
				usesRivetKit: true,
				maxOutputBytes: 16 * 1024 * 1024,
				maxOutputFiles: 64,
				maxFileBytes: 8 * 1024 * 1024,
			}),
		);

		await execFileAsync(
			"npm",
			[
				"install",
				"--install-strategy=shallow",
				"--omit=optional",
				"--omit=peer",
				"--legacy-peer-deps",
				"--ignore-scripts",
				"--no-audit",
				"--no-fund",
				"--loglevel=error",
			],
			{ cwd: workspace },
		);
		await execFileAsync(
			process.execPath,
			[builder, configPath],
			{
				cwd: repositoryRoot,
			},
		);

		const paths = await listFiles(release);
		const wasmPath = paths.find(
			(path) =>
				path.startsWith("modules/rivetkit-") && path.endsWith(".wasm"),
		);
		expect(paths).toEqual([
			"agentos-package.json",
			"main.mjs",
			"manifest.json",
			wasmPath,
		]);
		const totalBytes = (
			await Promise.all(
				paths.map(async (path) => (await readFile(join(release, path))).byteLength),
			)
		).reduce((sum, bytes) => sum + bytes, 0);
		expect(totalBytes).toBeLessThan(8 * 1024 * 1024);

		const guest = spawn(process.execPath, [join(release, "main.mjs")], {
			env: {
				...process.env,
				RIVETKIT_RUNTIME: "wasm",
				RIVETKIT_RUNTIME_MODE: "serverless",
			},
			stdio: ["ignore", "pipe", "pipe"],
		});
		try {
			await waitForHttp("http://127.0.0.1:3080/.agentos/ready", guest);
			const response = await fetch("http://127.0.0.1:3080/");
			expect(response.status).toBe(200);
			expect(await response.text()).toBe("hello");
			const metadata = await fetch(
				"http://127.0.0.1:3080/api/rivet/metadata",
				{ headers: { "user-agent": "RivetEngine/test" } },
			);
			expect(metadata.status).toBe(200);
		} finally {
			guest.kill("SIGTERM");
			const exited = await Promise.race([
				new Promise<true>((resolve) =>
					guest.once("exit", () => resolve(true)),
				),
				new Promise<false>((resolve) =>
					setTimeout(() => resolve(false), 1_000),
				),
			]);
			if (!exited && guest.exitCode === null) {
				guest.kill("SIGKILL");
				if (guest.exitCode === null) {
					await new Promise<void>((resolve) =>
						guest.once("exit", () => resolve()),
					);
				}
			}
		}
		},
		20_000,
	);
});

async function listFiles(root: string): Promise<string[]> {
	const paths: string[] = [];
	const walk = async (directory: string) => {
		for (const entry of await readdir(directory, { withFileTypes: true })) {
			const path = join(directory, entry.name);
			if (entry.isDirectory()) {
				await walk(path);
			} else {
				paths.push(path.slice(root.length + 1).replaceAll("\\", "/"));
			}
		}
	};
	await walk(root);
	return paths.sort();
}

async function waitForHttp(
	url: string,
	process: ReturnType<typeof spawn>,
): Promise<void> {
	const deadline = Date.now() + 10_000;
	let stderr = "";
	process.stderr?.on("data", (chunk) => {
		stderr = `${stderr}${chunk}`.slice(-16_384);
	});
	while (Date.now() < deadline) {
		if (process.exitCode !== null) {
			throw new Error(
				`bundled application exited with ${process.exitCode}: ${stderr}`,
			);
		}
		try {
			if ((await fetch(url)).ok) return;
		} catch {
			// The application is still starting.
		}
		await new Promise((resolve) => setTimeout(resolve, 25));
	}
	throw new Error(`bundled application did not become ready: ${stderr}`);
}
