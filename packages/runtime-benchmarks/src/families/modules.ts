import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import type { BenchmarkOp } from "../lib/layers.js";
import type { BenchVm } from "../lib/vm.js";

const JS_RUNTIME_UNSUPPORTED = "module resolution is a JS-runtime surface";
const MODULES_PER_ITERATION = 100;
const MODULE_SAMPLE_CAP = {
	maxIterations: 5,
	maxWarmup: 1,
};
const REPO_ROOT = fileURLToPath(new URL("../../../..", import.meta.url));
const ZOD_PACKAGE_HOST_DIR = join(
	REPO_ROOT,
	"node_modules/.pnpm/zod@4.3.6/node_modules/zod",
);
const ZOD_PACKAGE_GUEST_DIR = "/mnt/bench-zod";
const ZOD_TRANSITIVE_MODULE_FILE_COUNT = 76;

function require100SmallSetup(): string {
	return `async () => {
  const fs = await import("node:fs");
  const path = await import("node:path");
  const { createRequire } = await import("node:module");
  const root = "/tmp/fuzz-perf-modules-require-" + process.pid;
  fs.rmSync(root, { recursive: true, force: true });
  fs.mkdirSync(root, { recursive: true });
  const total = Number(process.env.BENCH_WARMUP || 5) + Number(process.env.BENCH_ITERATIONS || 20);
  for (let i = 0; i < total; i++) {
    const dir = path.join(root, String(i));
    fs.mkdirSync(dir, { recursive: true });
    for (let file = 0; file < ${MODULES_PER_ITERATION}; file++) {
      const value = i * ${MODULES_PER_ITERATION} + file;
      fs.writeFileSync(path.join(dir, "mod-" + file + ".cjs"), "module.exports = " + value + ";\\n");
    }
  }
  globalThis.__modulesBenchRequireRoot = root;
  globalThis.__modulesBenchRequire = createRequire(import.meta.url);
  globalThis.__modulesBenchPathJoin = path.join;
}`;
}

function require100SmallProgram(): string {
	return `async (i) => {
  const root = globalThis.__modulesBenchRequireRoot;
  const require = globalThis.__modulesBenchRequire;
  const pathJoin = globalThis.__modulesBenchPathJoin;
  let sum = 0;
  for (let file = 0; file < ${MODULES_PER_ITERATION}; file++) {
    sum += require(pathJoin(root, String(i), "mod-" + file + ".cjs"));
  }
  const expected = ${MODULES_PER_ITERATION} * (i * ${MODULES_PER_ITERATION}) + (${MODULES_PER_ITERATION} * (${MODULES_PER_ITERATION} - 1)) / 2;
  if (sum !== expected) throw new Error("bad require sum " + sum + " expected " + expected);
}`;
}

function import100SmallEsmSetup(): string {
	return `async () => {
  const fs = await import("node:fs");
  const path = await import("node:path");
  const { pathToFileURL } = await import("node:url");
  const root = "/tmp/fuzz-perf-modules-import-" + process.pid;
  fs.rmSync(root, { recursive: true, force: true });
  fs.mkdirSync(root, { recursive: true });
  const total = Number(process.env.BENCH_WARMUP || 5) + Number(process.env.BENCH_ITERATIONS || 20);
  for (let i = 0; i < total; i++) {
    const dir = path.join(root, String(i));
    fs.mkdirSync(dir, { recursive: true });
    for (let file = 0; file < ${MODULES_PER_ITERATION}; file++) {
      const value = i * ${MODULES_PER_ITERATION} + file;
      fs.writeFileSync(path.join(dir, "mod-" + file + ".mjs"), "export const value = " + value + ";\\n");
    }
  }
  globalThis.__modulesBenchImportRoot = root;
  globalThis.__modulesBenchImportPathJoin = path.join;
  globalThis.__modulesBenchPathToFileURL = pathToFileURL;
}`;
}

function import100SmallEsmProgram(): string {
	return `async (i) => {
  const root = globalThis.__modulesBenchImportRoot;
  const pathJoin = globalThis.__modulesBenchImportPathJoin;
  const pathToFileURL = globalThis.__modulesBenchPathToFileURL;
  let sum = 0;
  for (let file = 0; file < ${MODULES_PER_ITERATION}; file++) {
    const filePath = pathJoin(root, String(i), "mod-" + file + ".mjs");
    const mod = await import(pathToFileURL(filePath).href);
    sum += mod.value;
  }
  const expected = ${MODULES_PER_ITERATION} * (i * ${MODULES_PER_ITERATION}) + (${MODULES_PER_ITERATION} * (${MODULES_PER_ITERATION} - 1)) / 2;
  if (sum !== expected) throw new Error("bad import sum " + sum + " expected " + expected);
}`;
}

function npmPackageImportRunnerSource(): string {
	return `
const entryUrl = process.env.BENCH_NPM_ENTRY_URL;
const iteration = process.env.BENCH_NPM_ITERATION;
if (!entryUrl || !iteration) throw new Error("missing npm import bench env");
const mod = await import(entryUrl + "?bench=" + iteration);
if (!mod.z || typeof mod.z.object !== "function") {
  throw new Error("zod export check failed");
}
`;
}

function runHostNpmPackageImport(iters: number, warmup: number): number[] {
	const entryUrl = pathToFileURL(join(ZOD_PACKAGE_HOST_DIR, "index.js")).href;
	const samples: number[] = [];
	const dir = mkdtempSync(join(tmpdir(), "agentos-modules-npm-"));
	const runner = join(dir, "import-zod.mjs");
	try {
		writeFileSync(runner, npmPackageImportRunnerSource());
		for (let i = 0; i < warmup + iters; i++) {
			const start = process.hrtime.bigint();
			execFileSync("node", [runner], {
				stdio: "pipe",
				env: {
					...process.env,
					BENCH_NPM_ENTRY_URL: entryUrl,
					BENCH_NPM_ITERATION: String(i),
				},
				maxBuffer: 128 * 1024 * 1024,
			});
			const ms = Number(process.hrtime.bigint() - start) / 1e6;
			if (i >= warmup) samples.push(ms);
		}
	} finally {
		rmSync(dir, { recursive: true, force: true });
	}
	return samples;
}

async function runGuestNpmPackageImport(
	vm: BenchVm,
	iters: number,
	warmup: number,
): Promise<number[]> {
	const entryUrl = `file://${ZOD_PACKAGE_GUEST_DIR}/index.js`;
	const runner = "/tmp/fuzz-perf-modules-import-zod.mjs";
	await vm.writeFile(runner, npmPackageImportRunnerSource());
	const samples: number[] = [];
	for (let i = 0; i < warmup + iters; i++) {
		const start = process.hrtime.bigint();
		const result = await vm.spawnNodeCapture(runner, {
			BENCH_NPM_ENTRY_URL: entryUrl,
			BENCH_NPM_ITERATION: String(i),
		});
		const ms = Number(process.hrtime.bigint() - start) / 1e6;
		if (result.exitCode !== 0) {
			throw new Error(`guest zod import exited ${result.exitCode}\n${result.stderr}`);
		}
		if (i >= warmup) samples.push(ms);
	}
	return samples;
}

function ensureZodPackage(): void {
	if (!existsSync(join(ZOD_PACKAGE_HOST_DIR, "index.js"))) {
		throw new Error(`zod benchmark package not found at ${ZOD_PACKAGE_HOST_DIR}`);
	}
}

export const modulesFamily: BenchmarkOp[] = [
	{
		family: "modules",
		name: "require_100_small",
		...MODULE_SAMPLE_CAP,
		nativeUnsupportedReason: JS_RUNTIME_UNSUPPORTED,
		wasmUnsupportedReason: JS_RUNTIME_UNSUPPORTED,
		fileLine: "crates/execution/src/node_import_cache.rs:4750",
		reproducer: "stage 100 unique tiny CJS files per iteration, require them, and verify exported sum",
		setup: require100SmallSetup(),
		program: require100SmallProgram(),
	},
	{
		family: "modules",
		name: "import_100_small_esm",
		...MODULE_SAMPLE_CAP,
		nativeUnsupportedReason: JS_RUNTIME_UNSUPPORTED,
		wasmUnsupportedReason: JS_RUNTIME_UNSUPPORTED,
		fileLine: "crates/execution/src/node_import_cache.rs:4750",
		reproducer: "stage 100 unique tiny ESM files per iteration, dynamic-import them, and verify exported sum",
		setup: import100SmallEsmSetup(),
		program: import100SmallEsmProgram(),
	},
	{
		family: "modules",
		name: "import_npm_package",
		...MODULE_SAMPLE_CAP,
		nativeUnsupportedReason: JS_RUNTIME_UNSUPPORTED,
		wasmUnsupportedReason: JS_RUNTIME_UNSUPPORTED,
		fileLine: "crates/execution/src/node_import_cache.rs:4750",
		reproducer: `dynamic-import zod@4.3.6 from a read-only mounted package tree (${ZOD_TRANSITIVE_MODULE_FILE_COUNT} transitive ESM files) and verify z.object`,
		runNode: runHostNpmPackageImport,
		prepareVm: async () => {
			ensureZodPackage();
			return {
				options: {
					mounts: [
						{
							guestPath: ZOD_PACKAGE_GUEST_DIR,
							hostPath: ZOD_PACKAGE_HOST_DIR,
							readOnly: true,
						},
					],
				},
			};
		},
		runGuest: (vm, iters, warmup) => runGuestNpmPackageImport(vm, iters, warmup),
	},
	{
		family: "modules",
		name: "import_fresh_file",
		...MODULE_SAMPLE_CAP,
		nativeUnsupportedReason: JS_RUNTIME_UNSUPPORTED,
		wasmUnsupportedReason: JS_RUNTIME_UNSUPPORTED,
		fileLine: "crates/execution/src/javascript.rs:3939",
		reproducer: "write a unique /tmp .mjs file, dynamic-import it, and verify the exported value",
		program: `async (i) => {
  const fs = await import("node:fs");
  const value = "fresh-" + process.pid + "-" + i;
  const path = "/tmp/fuzz-perf-import-" + process.pid + "-" + i + ".mjs";
  fs.writeFileSync(path, "export const value = " + JSON.stringify(value) + ";\\n");
  const mod = await import("file://" + path);
  if (mod.value !== value) throw new Error("bad fresh import");
  fs.unlinkSync(path);
}`,
	},
];
