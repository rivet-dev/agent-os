#!/usr/bin/env node

import {
	copyFileSync,
	existsSync,
	mkdirSync,
	mkdtempSync,
	readFileSync,
	readdirSync,
	rmSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const runtimeCoreDir = join(repositoryRoot, "packages/runtime-core");
const runtimeSidecarDir = join(repositoryRoot, "packages/runtime-sidecar");

let platformPackageDir;
let sidecarBin;
let threadFixture;
for (let index = 2; index < process.argv.length; index += 2) {
	const flag = process.argv[index];
	const value = process.argv[index + 1];
	if (!value) usage(`missing value for ${flag}`);
	if (flag === "--platform-package") platformPackageDir = resolve(value);
	else if (flag === "--sidecar-bin") sidecarBin = resolve(value);
	else if (flag === "--thread-fixture") threadFixture = resolve(value);
	else usage(`unknown option ${flag}`);
}

if (Boolean(platformPackageDir) === Boolean(sidecarBin)) {
	usage("provide exactly one of --platform-package or --sidecar-bin");
}
if (sidecarBin && !existsSync(sidecarBin)) {
	throw new Error(`sidecar binary does not exist: ${sidecarBin}`);
}
if (platformPackageDir && !existsSync(join(platformPackageDir, "agentos-native-sidecar"))) {
	throw new Error(
		`platform package is missing agentos-native-sidecar: ${platformPackageDir}`,
	);
}
if (!threadFixture || !existsSync(threadFixture)) {
	usage("--thread-fixture must name the generated pthread conformance module");
}

const scratch = mkdtempSync(join(tmpdir(), "agentos-packed-wasm-"));
try {
	const packedPackages = [pack(runtimeSidecarDir)];
	if (platformPackageDir) packedPackages.push(pack(platformPackageDir));
	packedPackages.push(pack(runtimeCoreDir));

	const installDir = join(scratch, "install");
	mkdirSync(installDir, { recursive: true });
	const localPackages = Object.fromEntries(
		packedPackages.map(({ name, tarball }) => [name, `file:${tarball}`]),
	);
	writeFileSync(
		join(installDir, "package.json"),
		`${JSON.stringify(
			{
				private: true,
				type: "module",
				dependencies: localPackages,
				pnpm: { overrides: localPackages },
			},
			null,
			2,
		)}\n`,
	);
	run(
		"pnpm",
		[
			"install",
			"--ignore-scripts",
			"--config.optional=false",
			"--no-frozen-lockfile",
		],
		installDir,
	);

	const runnerPath = join(installDir, "smoke.mjs");
	copyFileSync(threadFixture, join(installDir, "pthread-conformance.wasm"));
	writeFileSync(
		runnerPath,
		`import { fileURLToPath } from "node:url";
import { NodeRuntime } from "@rivet-dev/agentos-runtime-core";
import { createInMemoryFileSystem } from "@rivet-dev/agentos-runtime-core/test-runtime";

const backends = ["v8", "wasmtime", "wasmtime-threads"];
for (const backend of backends) {
  const runtime = await NodeRuntime.create({
    filesystem: createInMemoryFileSystem(),
    wasmBackend: backend,
    wasmCommandDirs: [fileURLToPath(new URL(".", import.meta.url))],
    permissions: {
      fs: "allow",
      network: "allow",
      childProcess: "allow",
      process: "allow",
      env: "allow",
    },
  });
  try {
    const result = await runtime.execCommand("sh", [
      "-c",
      \`printf 'packed-\${backend}\\n' | tr '[:lower:]' '[:upper:]'\`,
    ]);
    const expected = \`PACKED-\${backend.toUpperCase()}\\n\`;
    if (result.exitCode !== 0 || result.stdout !== expected) {
      throw new Error(
        \`\${backend} packaged smoke failed: exit=\${result.exitCode} stdout=\${JSON.stringify(result.stdout)} stderr=\${JSON.stringify(result.stderr)}\`,
      );
    }
    if (backend === "wasmtime-threads") {
      const threaded = await runtime.execCommand("pthread-conformance.wasm", []);
      if (threaded.exitCode !== 0 || !threaded.stdout.includes("pthread-ok")) {
        throw new Error(
          \`packaged pthread smoke failed: exit=\${threaded.exitCode} stdout=\${JSON.stringify(threaded.stdout)} stderr=\${JSON.stringify(threaded.stderr)}\`,
        );
      }
    }
    process.stdout.write(\`packaged backend smoke passed: \${backend}\\n\`);
  } finally {
    await runtime.dispose();
  }
}
`,
	);

	const runnerEnv = { ...process.env };
	delete runnerEnv.AGENTOS_SIDECAR_BIN;
	delete runnerEnv.AGENTOS_WASMTIME_WORKER_PATH;
	if (sidecarBin) {
		runnerEnv.AGENTOS_SIDECAR_BIN = sidecarBin;
		runnerEnv.AGENTOS_WASMTIME_WORKER_PATH = sidecarBin;
	}
	run(process.execPath, [runnerPath], installDir, { env: runnerEnv });
} finally {
	rmSync(scratch, { recursive: true, force: true });
}

function pack(packageDir) {
	const manifest = JSON.parse(
		readFileSync(join(packageDir, "package.json"), "utf8"),
	);
	if (typeof manifest.name !== "string" || manifest.name.length === 0) {
		throw new Error(`package has no valid name: ${packageDir}`);
	}
	const before = new Set(readdirSync(scratch));
	run("pnpm", ["pack", "--pack-destination", scratch], packageDir);
	const created = readdirSync(scratch).filter(
		(entry) => entry.endsWith(".tgz") && !before.has(entry),
	);
	if (created.length !== 1) {
		throw new Error(
			`expected one tarball from ${packageDir}, found ${created.join(", ") || "none"}`,
		);
	}
	return { name: manifest.name, tarball: join(scratch, created[0]) };
}

function run(command, args, cwd, options = {}) {
	if (options.createCwd) mkdirSync(cwd, { recursive: true });
	const result = spawnSync(command, args, {
		cwd,
		env: options.env ?? process.env,
		stdio: "inherit",
	});
	if (result.error) throw result.error;
	if (result.status !== 0) {
		throw new Error(`${command} exited with status ${result.status}`);
	}
}

function usage(message) {
	throw new Error(
		`${message}\nusage: smoke-packed-wasm-backends.mjs (--platform-package <dir> | --sidecar-bin <path>) --thread-fixture <path>`,
	);
}
