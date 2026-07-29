/**
 * E2E project-matrix test: run existing fixture projects through the kernel.
 *
 * For each fixture in the repo-owned tests/integration/projects/ directory:
 *   1. Prepare project (npm install, cached by content hash)
 *   2. Run entry via host Node (baseline)
 *   3. Run entry via kernel (NodeFileSystem rooted at project dir, WasmVM + Node)
 *   4. Compare output parity
 *
 * Adapted from the legacy runtime suite to use package imports and
 * repo-local fixtures.
 */

import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import type { Dirent } from "node:fs";
import {
	access,
	cp,
	mkdir,
	readdir,
	readFile,
	rename,
	rm,
	symlink,
	writeFile,
} from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import {
	COMMANDS_DIR,
	createKernel,
	createNodeRuntime,
	createWasmVmRuntime,
	describeIf,
	NodeFileSystem,
} from "@rivet-dev/agentos-vm-test-harness";
import { expect, it } from "vitest";

const execFileAsync = promisify(execFile);
const TEST_TIMEOUT_MS = 55_000;
const COMMAND_TIMEOUT_MS = 45_000;
const ECOSYSTEM_INSTALL_TIMEOUT_MS = 120_000;
const ECOSYSTEM_KERNEL_TIMEOUT_MS = 120_000;
const ECOSYSTEM_CPU_TIME_LIMIT_MS = 300_000;
// A cold full-catalog fixture may consume each bounded phase in sequence:
// dependency install, host parity execution, and kernel execution. Keep the
// aggregate allowance scoped to the opt-in gate and retain teardown margin.
const ECOSYSTEM_TEST_TIMEOUT_MS =
	ECOSYSTEM_INSTALL_TIMEOUT_MS +
	COMMAND_TIMEOUT_MS +
	ECOSYSTEM_KERNEL_TIMEOUT_MS +
	30_000;
const CACHE_READY_MARKER = ".ready";
const WORKTREE_SCHEMA_VERSION = "v3-packed-local-dependencies";
const TRANSIENT_OUTPUT_DIRS = new Set([
	".astro",
	".next",
	"build",
	"coverage",
	"dist",
	"out",
]);

const __dirname = path.dirname(fileURLToPath(import.meta.url));

const WORKSPACE_ROOT = path.resolve(__dirname, "../../../..");
const FIXTURES_ROOT = path.resolve(__dirname, "projects");
const CACHE_ROOT = path.join(__dirname, "../../.cache", "project-matrix");
const REQUIRED_NODE_ECOSYSTEM_FIXTURES = [
	"express-pass",
	"fastify-pass",
	"ws-pass",
	"axios-pass",
	"node-fetch-pass",
	"hono-node-server-pass",
] as const;

// ---------------------------------------------------------------------------
// Types (same schema as project-matrix.test.ts)
// ---------------------------------------------------------------------------

type PackageManager = "pnpm" | "npm" | "bun" | "yarn";
type PassFixtureMetadata = {
	entry: string;
	args?: string[];
	code?: number;
	expectation: "pass";
	packageManager?: PackageManager;
};
type FailFixtureMetadata = {
	entry: string;
	args?: string[];
	expectation: "fail";
	fail: { code: number; stderrIncludes?: string };
	packageManager?: PackageManager;
};
type SkipFixtureMetadata = {
	entry: string;
	args?: string[];
	expectation: "skip";
	reason: string;
	packageManager?: PackageManager;
};
type FixtureMetadata =
	| PassFixtureMetadata
	| FailFixtureMetadata
	| SkipFixtureMetadata;
type FixtureProject = {
	name: string;
	sourceDir: string;
	metadata: FixtureMetadata;
};
type PreparedFixture = {
	cacheHit: boolean;
	cacheKey: string;
	projectDir: string;
};
type WorkingFixtureProject = {
	projectDir: string;
	dispose: () => Promise<void>;
};
type ResultEnvelope = { code: number; stdout: string; stderr: string };

// ---------------------------------------------------------------------------
// Fixture discovery
// ---------------------------------------------------------------------------

async function discoverFixtures(): Promise<FixtureProject[]> {
	let entries: Dirent[];
	try {
		entries = await readdir(FIXTURES_ROOT, { withFileTypes: true });
	} catch {
		// The fixture directory may be omitted from reduced source distributions.
		return [];
	}
	const fixtureDirs = entries
		.filter((e) => e.isDirectory())
		.map((e) => e.name)
		.sort((a, b) => a.localeCompare(b));

	const fixtures: FixtureProject[] = [];
	for (const name of fixtureDirs) {
		const sourceDir = path.join(FIXTURES_ROOT, name);
		const metaPath = path.join(sourceDir, "fixture.json");
		const packageJsonPath = path.join(sourceDir, "package.json");
		if (!(await pathExists(metaPath)) || !(await pathExists(packageJsonPath))) {
			continue;
		}
		const raw = JSON.parse(await readFile(metaPath, "utf8"));
		const metadata = parseMetadata(raw, name);
		fixtures.push({ name, sourceDir, metadata });
	}
	return fixtures;
}

function parseMetadata(
	raw: Record<string, unknown>,
	name: string,
): FixtureMetadata {
	const entry = raw.entry as string;
	const args = Array.isArray(raw.args)
		? raw.args.map((arg) => String(arg))
		: undefined;
	const packageManager = raw.packageManager as PackageManager | undefined;
	if (raw.expectation === "pass")
		return {
			entry,
			...(args && { args }),
			...(typeof raw.code === "number" && { code: raw.code }),
			expectation: "pass",
			...(packageManager && { packageManager }),
		};
	if (raw.expectation === "skip")
		return {
			entry,
			...(args && { args }),
			expectation: "skip",
			reason: raw.reason as string,
			...(packageManager && { packageManager }),
		};
	const fail = raw.fail as { code: number; stderrIncludes?: string };
	return {
		entry,
		...(args && { args }),
		expectation: "fail",
		fail,
		...(packageManager && { packageManager }),
	};
}

// ---------------------------------------------------------------------------
// Fixture preparation
// ---------------------------------------------------------------------------

async function prepareFixtureProject(
	fixture: FixtureProject,
	commandTimeoutMs = COMMAND_TIMEOUT_MS,
): Promise<PreparedFixture> {
	await mkdir(CACHE_ROOT, { recursive: true });
	const cacheKey = await createFixtureCacheKey(fixture);
	const cacheDir = path.join(CACHE_ROOT, `${fixture.name}-${cacheKey}`);
	const readyMarker = path.join(cacheDir, CACHE_READY_MARKER);

	if (
		(await pathExists(readyMarker)) &&
		(await cacheHasRequiredInstallArtifacts(fixture, cacheDir))
	) {
		return { cacheHit: true, cacheKey, projectDir: cacheDir };
	}

	// Reset stale entries
	if (await pathExists(cacheDir)) {
		await rm(cacheDir, { recursive: true, force: true });
	}

	// Stage and install
	const staging = `${cacheDir}.tmp-${process.pid}-${Date.now()}`;
	await rm(staging, { recursive: true, force: true });
	await cp(fixture.sourceDir, staging, {
		recursive: true,
		filter: (src) => !src.split(path.sep).includes("node_modules"),
	});
	const pm = fixture.metadata.packageManager ?? "pnpm";
	let installCmd: { cmd: string; args: string[] };
	if (pm === "npm") {
		installCmd = {
			cmd: "npm",
			args: ["install", "--prefer-offline", "--install-links=true"],
		};
	} else if (pm === "bun") {
		installCmd = { cmd: "bun", args: ["install"] };
	} else if (pm === "yarn") {
		installCmd = await getYarnInstallCmd(staging);
	} else {
		const args = ["install", "--ignore-workspace", "--prefer-offline"];
		if (await pathExists(path.join(staging, "pnpm-lock.yaml"))) {
			args.push("--frozen-lockfile");
		}
		installCmd = { cmd: "pnpm", args };
	}
	await execFileAsync(installCmd.cmd, installCmd.args, {
		cwd: staging,
		timeout: commandTimeoutMs,
		maxBuffer: 10 * 1024 * 1024,
		...(pm === "yarn" && { env: yarnEnv }),
	});
	await writeFile(
		path.join(staging, CACHE_READY_MARKER),
		`${new Date().toISOString()}\n`,
	);

	// Promote
	try {
		await rename(staging, cacheDir);
	} catch (err: unknown) {
		const code =
			err && typeof err === "object" && "code" in err ? String(err.code) : "";
		if (code !== "EEXIST") throw err;
		await rm(staging, { recursive: true, force: true });
		if (!(await pathExists(readyMarker))) {
			throw new Error(`Cache race: missing ready marker at ${cacheDir}`);
		}
	}

	return { cacheHit: false, cacheKey, projectDir: cacheDir };
}

async function createFixtureCacheKey(fixture: FixtureProject): Promise<string> {
	const hash = createHash("sha256");
	const nodeMajor = process.versions.node.split(".")[0] ?? "0";
	const pm = fixture.metadata.packageManager ?? "pnpm";
	const pmVersion =
		pm === "npm"
			? await getNpmVersion()
			: pm === "bun"
				? await getBunVersion()
				: pm === "yarn"
					? await getYarnVersion()
					: await getPnpmVersion();
	hash.update(`node-major:${nodeMajor}\n`);
	hash.update(`pm:${pm}\n`);
	hash.update(`pm-version:${pmVersion}\n`);
	hash.update(`platform:${process.platform}\n`);
	hash.update(`arch:${process.arch}\n`);
	hash.update(`worktree-schema:${WORKTREE_SCHEMA_VERSION}\n`);

	const lockFile =
		pm === "npm"
			? "package-lock.json"
			: pm === "bun"
				? "bun.lock"
				: pm === "yarn"
					? "yarn.lock"
					: "pnpm-lock.yaml";
	for (const [label, filePath] of [
		["workspace-lock", path.join(WORKSPACE_ROOT, "pnpm-lock.yaml")],
		["workspace-package", path.join(WORKSPACE_ROOT, "package.json")],
		["fixture-package", path.join(fixture.sourceDir, "package.json")],
		["fixture-lock", path.join(fixture.sourceDir, lockFile)],
	]) {
		hash.update(`${label}:`);
		try {
			hash.update(await readFile(filePath));
		} catch {
			hash.update("<missing>");
		}
		hash.update("\n");
	}

	const files = await listFiles(fixture.sourceDir);
	for (const rel of files) {
		hash.update(`fixture-file:${rel.split(path.sep).join("/")}\n`);
		hash.update(await readFile(path.join(fixture.sourceDir, rel)));
		hash.update("\n");
	}

	return hash.digest("hex").slice(0, 16);
}

async function cacheHasRequiredInstallArtifacts(
	fixture: FixtureProject,
	cacheDir: string,
): Promise<boolean> {
	if (
		!(await fixtureDeclaresDependencies(fixture)) &&
		!(await fixtureUsesWorkspaces(fixture))
	) {
		return true;
	}
	return pathExists(path.join(cacheDir, "node_modules"));
}

async function fixtureDeclaresDependencies(
	fixture: FixtureProject,
): Promise<boolean> {
	const packageJson = JSON.parse(
		await readFile(path.join(fixture.sourceDir, "package.json"), "utf8"),
	) as Record<string, unknown>;
	return [
		"dependencies",
		"devDependencies",
		"optionalDependencies",
		"peerDependencies",
	].some((key) => {
		const value = packageJson[key];
		return (
			value !== null &&
			typeof value === "object" &&
			Object.keys(value).length > 0
		);
	});
}

async function fixtureUsesWorkspaces(
	fixture: FixtureProject,
): Promise<boolean> {
	const packageJson = JSON.parse(
		await readFile(path.join(fixture.sourceDir, "package.json"), "utf8"),
	) as Record<string, unknown>;
	return packageJson.workspaces !== undefined;
}

async function createWorkingFixtureProject(
	fixture: FixtureProject,
	prepared: PreparedFixture,
	label: string,
): Promise<WorkingFixtureProject> {
	const workingRoot = path.join(CACHE_ROOT, ".worktrees");
	await mkdir(workingRoot, { recursive: true });
	const projectDir = path.join(
		workingRoot,
		`${fixture.name}-${prepared.cacheKey}-${label}-${process.pid}-${Date.now()}`,
	);

	await cp(prepared.projectDir, projectDir, {
		recursive: true,
		filter: (src) => {
			const relative = path.relative(prepared.projectDir, src);
			if (!relative) return true;
			const segments = relative.split(path.sep);
			return !segments.some(
				(segment) =>
					segment === "node_modules" || TRANSIENT_OUTPUT_DIRS.has(segment),
			);
		},
	});

	const installedNodeModulesDir = path.join(
		prepared.projectDir,
		"node_modules",
	);
	if (await pathExists(installedNodeModulesDir)) {
		if (await fixtureUsesWorkspaces(fixture)) {
			// npm workspace links are relative to the project root. Copy this small
			// layout so those links stay inside the isolated working fixture instead
			// of resolving through the external prepared-cache directory.
			await cp(installedNodeModulesDir, path.join(projectDir, "node_modules"), {
				recursive: true,
				verbatimSymlinks: true,
			});
		} else {
			await symlink(
				installedNodeModulesDir,
				path.join(projectDir, "node_modules"),
				"dir",
			);
		}
	}

	return {
		projectDir,
		dispose: () => rm(projectDir, { recursive: true, force: true }),
	};
}

let _pnpmVersionPromise: Promise<string> | undefined;
function getPnpmVersion(): Promise<string> {
	if (!_pnpmVersionPromise) {
		_pnpmVersionPromise = execFileAsync("pnpm", ["--version"], {
			cwd: WORKSPACE_ROOT,
			timeout: COMMAND_TIMEOUT_MS,
		}).then((r) => r.stdout.trim());
	}
	return _pnpmVersionPromise;
}

let _npmVersionPromise: Promise<string> | undefined;
function getNpmVersion(): Promise<string> {
	if (!_npmVersionPromise) {
		_npmVersionPromise = execFileAsync("npm", ["--version"], {
			cwd: WORKSPACE_ROOT,
			timeout: COMMAND_TIMEOUT_MS,
		}).then((r) => r.stdout.trim());
	}
	return _npmVersionPromise;
}

let _bunVersionPromise: Promise<string> | undefined;
function getBunVersion(): Promise<string> {
	if (!_bunVersionPromise) {
		_bunVersionPromise = execFileAsync("bun", ["--version"], {
			cwd: WORKSPACE_ROOT,
			timeout: COMMAND_TIMEOUT_MS,
		}).then((r) => r.stdout.trim());
	}
	return _bunVersionPromise;
}

let _yarnVersionPromise: Promise<string> | undefined;
// Bypass corepack packageManager enforcement so yarn runs in a pnpm workspace.
const yarnEnv = { ...process.env, COREPACK_ENABLE_STRICT: "0" };
function getYarnVersion(): Promise<string> {
	if (!_yarnVersionPromise) {
		_yarnVersionPromise = execFileAsync("yarn", ["--version"], {
			cwd: WORKSPACE_ROOT,
			timeout: COMMAND_TIMEOUT_MS,
			env: yarnEnv,
		}).then((r) => r.stdout.trim());
	}
	return _yarnVersionPromise;
}

async function getYarnInstallCmd(
	projectDir: string,
): Promise<{ cmd: string; args: string[] }> {
	const isBerry = await pathExists(path.join(projectDir, ".yarnrc.yml"));
	return isBerry
		? { cmd: "yarn", args: ["install", "--immutable"] }
		: { cmd: "yarn", args: ["install"] };
}

async function listFiles(root: string): Promise<string[]> {
	const result: string[] = [];
	async function walk(rel: string): Promise<void> {
		const dir = path.join(root, rel);
		const entries = await readdir(dir, { withFileTypes: true });
		for (const e of entries.sort((a, b) => a.name.localeCompare(b.name))) {
			if (e.name === "node_modules") continue;
			const p = rel ? path.join(rel, e.name) : e.name;
			if (e.isDirectory()) await walk(p);
			else if (e.isFile()) result.push(p);
		}
	}
	await walk("");
	return result.sort((a, b) => a.localeCompare(b));
}

// ---------------------------------------------------------------------------
// Host execution (baseline)
// ---------------------------------------------------------------------------

async function runHostExecution(
	projectDir: string,
	entryRel: string,
	args: string[] = [],
): Promise<ResultEnvelope> {
	const entryPath = path.join(projectDir, entryRel);
	return normalizeEnvelope(
		await runCommand(process.execPath, [entryPath, ...args], projectDir),
		projectDir,
	);
}

async function runCommand(
	cmd: string,
	args: string[],
	cwd: string,
): Promise<ResultEnvelope> {
	try {
		const r = await execFileAsync(cmd, args, {
			cwd,
			timeout: COMMAND_TIMEOUT_MS,
			maxBuffer: 10 * 1024 * 1024,
		});
		return { code: 0, stdout: r.stdout, stderr: r.stderr };
	} catch (err: unknown) {
		if (err && typeof err === "object" && "stdout" in err) {
			const e = err as { code?: number; stdout?: string; stderr?: string };
			return {
				code: typeof e.code === "number" ? e.code : 1,
				stdout: typeof e.stdout === "string" ? e.stdout : "",
				stderr: typeof e.stderr === "string" ? e.stderr : "",
			};
		}
		throw err;
	}
}

// ---------------------------------------------------------------------------
// Kernel execution
// ---------------------------------------------------------------------------

async function runKernelExecution(
	projectDir: string,
	entryRel: string,
	args: string[] = [],
	includeWasmRuntime = false,
): Promise<ResultEnvelope> {
	// NodeFileSystem rooted at projectDir. require() resolves from node_modules on disk.
	const vfs = new NodeFileSystem({ root: projectDir });
	const kernel = createKernel({
		filesystem: vfs,
		cwd: "/",
		...(includeWasmRuntime
			? {
					limits: {
						reactor: { operationDeadlineMs: ECOSYSTEM_KERNEL_TIMEOUT_MS },
						jsRuntime: {
							cpuTimeLimitMs: ECOSYSTEM_CPU_TIME_LIMIT_MS,
							importCacheMaterializeTimeoutMs: ECOSYSTEM_KERNEL_TIMEOUT_MS,
						},
					},
				}
			: {}),
	});

	if (includeWasmRuntime) {
		await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));
	}
	await kernel.mount(createNodeRuntime());

	try {
		const vfsEntry = "/" + entryRel.replace(/\\/g, "/");
		// Execute the catalog entry as the top-level Node process. The WASM runtime
		// remains mounted for shell/native command children, but wrapping every entry
		// in `sh -> node` adds a lifecycle layer unrelated to package compatibility.
		const stdoutChunks: Uint8Array[] = [];
		const stderrChunks: Uint8Array[] = [];
		const guestProcess = kernel.spawn("node", [vfsEntry, ...args], {
			cwd: "/",
			...(process.env.AGENTOS_ECOSYSTEM_DIAGNOSTICS === "1"
				? { env: { AGENTOS_CHILD_PROCESS_DIAGNOSTICS: "1" } }
				: {}),
			onStdout: (chunk) => stdoutChunks.push(chunk),
			onStderr: (chunk) => stderrChunks.push(chunk),
		});
		const timeout = setTimeout(
			() => guestProcess.kill(9),
			includeWasmRuntime ? ECOSYSTEM_KERNEL_TIMEOUT_MS : COMMAND_TIMEOUT_MS,
		);
		let result: { exitCode: number; stdout: string; stderr: string };
		try {
			const exitCode = await guestProcess.wait();
			result = {
				exitCode,
				stdout: Buffer.concat(
					stdoutChunks.map((chunk) => Buffer.from(chunk)),
				).toString("utf8"),
				stderr: Buffer.concat(
					stderrChunks.map((chunk) => Buffer.from(chunk)),
				).toString("utf8"),
			};
		} finally {
			clearTimeout(timeout);
		}
		return normalizeEnvelope(
			{ code: result.exitCode, stdout: result.stdout, stderr: result.stderr },
			projectDir,
		);
	} finally {
		await kernel.dispose();
	}
}

// ---------------------------------------------------------------------------
// Output normalization
// ---------------------------------------------------------------------------

function normalizeEnvelope(
	envelope: ResultEnvelope,
	projectDir: string,
): ResultEnvelope {
	return {
		code: envelope.code,
		stdout: normalizeText(envelope.stdout, projectDir),
		stderr: normalizeText(envelope.stderr, projectDir),
	};
}

function normalizeText(value: string, projectDir: string): string {
	const normalized = value.replace(/\r\n/g, "\n");
	const posixDir = projectDir.split(path.sep).join(path.posix.sep);
	return normalizeModuleNotFoundText(
		normalized
			.split(projectDir)
			.join("<project>")
			.split(posixDir)
			.join("<project>"),
	);
}

function normalizeModuleNotFoundText(value: string): string {
	if (!value.includes("Cannot find module")) return value;
	const quoted = value.match(/Cannot find module '([^']+)'/);
	if (quoted) return `Cannot find module '${quoted[1]}'\n`;
	const from = value.match(/Cannot find module:\s*([^\s]+)\s+from\s+/);
	if (from) return `Cannot find module '${from[1]}'\n`;
	return value;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async function pathExists(p: string): Promise<boolean> {
	try {
		await access(p);
		return true;
	} catch {
		return false;
	}
}

async function expectFixtureParity(
	fixture: FixtureProject,
	options?: { includeWasmRuntime?: boolean; installTimeoutMs?: number },
): Promise<void> {
	if (fixture.metadata.expectation === "skip") {
		throw new Error(
			`Skipped fixture ${fixture.name} was executed: ${fixture.metadata.reason}`,
		);
	}
	const prepared = await prepareFixtureProject(
		fixture,
		options?.installTimeoutMs,
	);
	const hostProject = await createWorkingFixtureProject(
		fixture,
		prepared,
		"host",
	);
	const kernelProject = await createWorkingFixtureProject(
		fixture,
		prepared,
		"kernel",
	);
	let host: ResultEnvelope;
	let kernel: ResultEnvelope;

	try {
		host = await runHostExecution(
			hostProject.projectDir,
			fixture.metadata.entry,
			fixture.metadata.args,
		);
		kernel = await runKernelExecution(
			kernelProject.projectDir,
			fixture.metadata.entry,
			fixture.metadata.args,
			options?.includeWasmRuntime,
		);
	} finally {
		await Promise.all([hostProject.dispose(), kernelProject.dispose()]);
	}
	if (process.env.AGENTOS_ECOSYSTEM_DIAGNOSTICS === "1") {
		console.error(
			JSON.stringify({ fixture: fixture.name, host, kernel }, null, 2),
		);
	}

	if (fixture.metadata.expectation === "pass") {
		expect(host.code).toBe(fixture.metadata.code ?? 0);
		expect(kernel).toEqual(host);
		return;
	}

	// Fail fixtures: host succeeds, kernel enforces VM restrictions.
	expect(host.code).toBe(0);
	expect(kernel.code).toBe(fixture.metadata.fail.code);
	if (fixture.metadata.fail.stderrIncludes) {
		expect(kernel.stderr).toContain(fixture.metadata.fail.stderrIncludes);
	}
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const discoveredFixtures = await discoverFixtures();
const packageManagerAvailability = new Map<PackageManager, boolean>();
for (const fixture of discoveredFixtures) {
	const packageManager = fixture.metadata.packageManager ?? "pnpm";
	if (packageManagerAvailability.has(packageManager)) continue;
	try {
		await (packageManager === "npm"
			? getNpmVersion()
			: packageManager === "bun"
				? getBunVersion()
				: packageManager === "yarn"
					? getYarnVersion()
					: getPnpmVersion());
		packageManagerAvailability.set(packageManager, true);
	} catch {
		packageManagerAvailability.set(packageManager, false);
	}
}
const fixturesByName = new Map(
	discoveredFixtures.map((fixture) => [fixture.name, fixture]),
);

describeIf(
	process.env.AGENTOS_ECOSYSTEM_E2E === "1",
	"required Node ecosystem reactor matrix",
	() => {
		it("contains every required fixture", () => {
			expect(
				REQUIRED_NODE_ECOSYSTEM_FIXTURES.filter(
					(name) => !fixturesByName.has(name),
				),
			).toEqual([]);
		});

		for (const name of REQUIRED_NODE_ECOSYSTEM_FIXTURES) {
			it(
				`runs fixture ${name} through the embedded V8 reactor with host-node parity`,
				async () => {
					const fixture = fixturesByName.get(name);
					if (!fixture)
						throw new Error(`Required ecosystem fixture is missing: ${name}`);
					await expectFixtureParity(fixture, {
						// This gate intentionally proves the native Node reactor alone. It
						// must not rely on browser support or prebuilt WASM commands.
						includeWasmRuntime: false,
						installTimeoutMs: ECOSYSTEM_INSTALL_TIMEOUT_MS,
					});
				},
				ECOSYSTEM_TEST_TIMEOUT_MS,
			);
		}
	},
);

describeIf(
	process.env.AGENTOS_ECOSYSTEM_FULL_E2E === "1",
	"full Node ecosystem matrix through kernel",
	() => {
		it("discovers at least one fixture project", () => {
			expect(discoveredFixtures.length).toBeGreaterThan(0);
		});

		for (const fixture of discoveredFixtures) {
			const packageManager = fixture.metadata.packageManager ?? "pnpm";
			const fixtureTest =
				fixture.metadata.expectation === "skip" ||
				packageManagerAvailability.get(packageManager) === false
					? it.skip
					: it;
			fixtureTest(
				`runs fixture ${fixture.name} through kernel with host-node parity`,
				async () => {
					await expectFixtureParity(fixture, {
						includeWasmRuntime: true,
						installTimeoutMs: ECOSYSTEM_INSTALL_TIMEOUT_MS,
					});
				},
				ECOSYSTEM_TEST_TIMEOUT_MS,
			);
		}
	},
);
