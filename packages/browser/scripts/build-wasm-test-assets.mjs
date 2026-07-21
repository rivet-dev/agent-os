#!/usr/bin/env node
// Builds the assets the Chromium wasm tests (tests/browser-wasm/) need:
//   1. the agentos-sidecar-browser wasm for the web target (.cache/agentos-sidecar-wasm-web)
//   2. the ACP wire-codec bundle (tests/browser-wasm/acp-codec.bundle.js) via esbuild,
//      resolving @rivet-dev/agentos-runtime-core + agent-os's generated ACP encoders + a Buffer polyfill.
//
// Run before `playwright test --config=playwright.wasm.config.ts` (the config's
// webServer invokes this).

import { spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, readdirSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);

const here = path.dirname(fileURLToPath(import.meta.url));
const packageRoot = path.resolve(here, "..");
const repoRoot = path.resolve(packageRoot, "..", "..");

function run(cmd, args) {
	const result = spawnSync(cmd, args, { stdio: "inherit" });
	if (result.status !== 0) {
		throw new Error(`${cmd} ${args.join(" ")} failed (${result.status})`);
	}
}

function pnpmModuleDir(prefix, pkg) {
	const base = path.join(repoRoot, "node_modules", ".pnpm");
	const match = readdirSync(base)
		.filter((name) => name.startsWith(prefix))
		.sort()
		.pop();
	if (!match) throw new Error(`no ${prefix}* under ${base}`);
	return path.join(base, match, "node_modules", pkg);
}

// 1. wasm web build (idempotent; wasm-pack is incremental).
run("node", [path.join(here, "build-sidecar-wasm.mjs"), "--target", "web"]);

// 2. esbuild bundle of the ACP codec entry (esbuild + buffer come from the pnpm store).
const esbuildBin = path.join(pnpmModuleDir("esbuild@0.25", "esbuild"), "bin", "esbuild");
const bufferDir = pnpmModuleDir("buffer@6", "buffer");
const entry = path.join(packageRoot, "tests", "browser-wasm", "acp-codec.entry.ts");
const outfile = path.join(packageRoot, "tests", "browser-wasm", "acp-codec.bundle.js");

if (!existsSync(esbuildBin)) throw new Error(`esbuild not found at ${esbuildBin}`);
run(esbuildBin, [
	entry,
	"--bundle",
	"--format=esm",
	"--platform=browser",
	`--alias:buffer=${bufferDir}`,
	`--outfile=${outfile}`,
]);

// 3. Converged runtime harness assets: bundle @rivet-dev/agentos-runtime-browser's worker and
// the converged-runtime harness so a real guest can run in Chromium with its
// fs.* syscalls routed through the converged sync-bridge to the agentos wasm
// kernel (createAgentOsConvergedSidecar).
const browserTestsDir = path.join(packageRoot, "tests", "browser-wasm");
const workerEntry = require.resolve("@rivet-dev/agentos-runtime-browser/internal/worker");
const agentOsBrowserRoot = path.resolve(path.dirname(workerEntry), "..");
const agentOsRepoRoot = path.resolve(agentOsBrowserRoot, "..", "..");
const agentOsCommandsDir = path.join(
	agentOsRepoRoot,
	"registry",
	"native",
	"target",
	"wasm32-wasip1",
	"release",
	"commands",
);
const repoNativeCommandsDir = path.join(
	repoRoot,
	"registry",
	"native",
	"target",
	"wasm32-wasip1",
	"release",
	"commands",
);
const runtimeCoreCommandsDir = path.join(repoRoot, "packages", "runtime-core", "commands");
const coreutilsCommandsDir = path.join(repoRoot, "registry", "software", "coreutils", "bin");
const browserCommandsDir = path.join(browserTestsDir, "commands");

function copyCommandsFrom(commandsDir) {
	mkdirSync(browserCommandsDir, { recursive: true });
	for (const entry of readdirSync(commandsDir)) {
		copyFileSync(path.join(commandsDir, entry), path.join(browserCommandsDir, entry));
	}
	console.log(`copied real wasm commands from ${commandsDir}`);
}

if (existsSync(repoNativeCommandsDir)) {
	copyCommandsFrom(repoNativeCommandsDir);
} else if (existsSync(agentOsCommandsDir)) {
	copyCommandsFrom(agentOsCommandsDir);
} else if (existsSync(runtimeCoreCommandsDir)) {
	copyCommandsFrom(runtimeCoreCommandsDir);
} else if (existsSync(coreutilsCommandsDir)) {
	copyCommandsFrom(coreutilsCommandsDir);
} else {
	console.log(
		`skipping real wasm command copy; missing ${repoNativeCommandsDir}, ${agentOsCommandsDir}, ${runtimeCoreCommandsDir}, and ${coreutilsCommandsDir}`,
	);
}
run(esbuildBin, [
	workerEntry,
	"--bundle",
	"--format=esm",
	"--platform=browser",
	"--target=es2022",
	`--alias:buffer=${bufferDir}`,
	`--outfile=${path.join(browserTestsDir, "agentos-worker.js")}`,
]);
run(esbuildBin, [
	path.join(browserTestsDir, "converged-runtime-harness.entry.ts"),
	"--bundle",
	"--format=esm",
	"--platform=browser",
	"--target=es2022",
	`--alias:buffer=${bufferDir}`,
	`--outfile=${path.join(browserTestsDir, "converged-runtime-harness.bundle.js")}`,
]);

// 5. M2a kernel-in-worker: the kernel worker entry + the main-thread relay harness.
run(esbuildBin, [
	path.join(browserTestsDir, "agentos-kernel.worker.ts"),
	"--bundle",
	"--format=esm",
	"--platform=browser",
	"--target=es2022",
	`--outfile=${path.join(browserTestsDir, "agentos-kernel.worker.js")}`,
]);
run(esbuildBin, [
	path.join(browserTestsDir, "kernel-worker.entry.ts"),
	"--bundle",
	"--format=esm",
	"--platform=browser",
	"--target=es2022",
	`--alias:buffer=${bufferDir}`,
	`--outfile=${path.join(browserTestsDir, "kernel-worker.bundle.js")}`,
]);

// 6. M3 async-agent executor: the kernel worker (reactor + drive loop), the async
// agent execution worker, and the gate harness.
// 7. M4 async-inference: the inference agent worker + the chrome-llm host-callback
// gate (shares the kernel worker + harness with M3).
for (const [src, out] of [
	["async-kernel.worker.ts", "async-kernel.worker.js"],
	["async-echo-agent.worker.ts", "async-echo-agent.worker.js"],
	["async-agent.entry.ts", "async-agent.bundle.js"],
	["async-infer-agent.worker.ts", "async-infer-agent.worker.js"],
	["async-infer.entry.ts", "async-infer.bundle.js"],
	["async-loopback-agent.worker.ts", "async-loopback-agent.worker.js"],
	["async-loopback.entry.ts", "async-loopback.bundle.js"],
	["async-proxy-agent.worker.ts", "async-proxy-agent.worker.js"],
	["async-proxy.entry.ts", "async-proxy.bundle.js"],
	["pty-loopback-agent.worker.ts", "pty-loopback-agent.worker.js"],
	["pty-loopback.entry.ts", "pty-loopback.bundle.js"],
	["pty-stdio.entry.ts", "pty-stdio.bundle.js"],
	["browser-real-shell.entry.ts", "browser-real-shell.bundle.js"],
	["real-terminal.entry.ts", "real-terminal.bundle.js"],
	["real-language-model.entry.ts", "real-language-model.bundle.js"],
	["pi-tui.entry.ts", "pi-tui.bundle.js"],
	["agent-demo.entry.ts", "agent-demo.bundle.js"],
	["pi-boot.entry.ts", "pi-boot.bundle.js"],
	["pi-prompt.entry.ts", "pi-prompt.bundle.js"],
	["pi-demo.entry.ts", "pi-demo.bundle.js"],
]) {
	run(esbuildBin, [
		path.join(browserTestsDir, src),
		"--bundle",
		"--format=esm",
		"--platform=browser",
		"--target=es2022",
		`--alias:buffer=${bufferDir}`,
		`--outfile=${path.join(browserTestsDir, out)}`,
	]);
}

// 8. M4a pi-boot gate: bundle the REAL pi ACP adapter (CJS, node:fs/path kept external
// so they route through the kernel; node:stream/module are supplied by the executor's
// polyfills). Guarded: only when the adapter dist + @mariozechner/pi-agent-core are
// present (a local pi build); the pi-boot spec is test.fixme until the executor gains a
// persistent-execution mode, so its absence in CI is harmless.
const piBrowserEntry = path.join(repoRoot, "registry", "agent", "pi", "adapter-browser-entry.mjs");
const piAdapterDist = path.join(repoRoot, "registry", "agent", "pi", "dist", "adapter.js");
const piCliDist = path.join(repoRoot, "registry", "agent", "pi", "node_modules", "@mariozechner", "pi-coding-agent", "dist", "cli.js");
const piCliPackageJson = path.join(repoRoot, "registry", "agent", "pi", "node_modules", "@mariozechner", "pi-coding-agent", "package.json");
const piCliThemeDir = path.join(repoRoot, "registry", "agent", "pi", "node_modules", "@mariozechner", "pi-coding-agent", "dist", "modes", "interactive", "theme");
const piUndiciFetchShim = path.join(browserTestsDir, "pi-undici-fetch-shim.cjs");
const piAgentCore = path.join(repoRoot, "registry", "agent", "pi", "node_modules", "@mariozechner", "pi-agent-core", "dist", "index.js");
if (existsSync(piAdapterDist) && existsSync(piAgentCore)) {
	// (a) The M4a boot bundle: the adapter alone (CJS; SDK loaded lazily at session/new).
	// node:fs/path kept external = kernel-backed; node:stream/module via the executor
	// polyfills. This is what the pi-boot gate runs (reaches ACP initialize).
	run(esbuildBin, [
		piAdapterDist,
		"--bundle",
		"--platform=node",
		"--format=cjs",
		"--external:node:*",
		`--outfile=${path.join(browserTestsDir, "pi-adapter.bundle.js")}`,
	]);
	// (b) The M4b full-SDK bundle: the browser entry (adapter + SDK submodules
	// statically published through the native __PI_SDK_RUNTIME__ contract) as ONE
	// self-contained .cjs. The `.cjs`
	// extension tells the executor it is CommonJS so it does NOT apply the ESM import
	// transform (which trips on the bundle's dynamic import()). The SDK still needs a few
	// more node-builtin shims before it reaches session/prompt; tracked as M4b/pi-4.
	run(esbuildBin, [
		piBrowserEntry,
		"--bundle",
		"--platform=node",
		"--format=cjs",
		"--external:node:*",
		'--define:import.meta.url="file:///root/pi/adapter.cjs"',
		`--outfile=${path.join(browserTestsDir, "pi-adapter.bundle.cjs")}`,
	]);
	if (existsSync(piCliDist)) {
		if (existsSync(piCliPackageJson)) {
			copyFileSync(piCliPackageJson, path.join(browserTestsDir, "pi-package.json"));
		}
		if (existsSync(piCliThemeDir)) {
			for (const entry of readdirSync(piCliThemeDir)) {
				if (entry.endsWith(".json")) {
					copyFileSync(
						path.join(piCliThemeDir, entry),
						path.join(browserTestsDir, `pi-theme-${entry}`),
					);
				}
			}
		}
		run(esbuildBin, [
			piCliDist,
			"--bundle",
			"--platform=node",
			"--format=cjs",
			"--external:node:*",
			`--alias:undici=${piUndiciFetchShim}`,
			'--define:import.meta.url="file:///root/pi-cli.cjs"',
			`--outfile=${path.join(browserTestsDir, "pi-cli.bundle.cjs")}`,
		]);
	}
	console.log("pi adapter bundles built (boot .js + full-SDK .cjs)");
} else {
	console.log("skipping pi adapter bundle (pi dist / pi-agent-core not present)");
}

console.log("agentos wasm test assets built");
