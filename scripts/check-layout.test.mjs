import assert from "node:assert/strict";
import { chmodSync, mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import test from "node:test";

const script = join(dirname(fileURLToPath(import.meta.url)), "check-layout.mjs");

test("allows experiment tests and ignores nested Claude worktrees", () => {
	const root = mkdtempSync(join(tmpdir(), "agentos-layout-"));
	try {
		const nestedTest = join(
			root,
			".claude/worktrees/other/packages/core/tests/legacy.test.ts",
		);
		mkdirSync(dirname(nestedTest), { recursive: true });
		writeFileSync(nestedTest, "export {};\n");
		const experimentTest = join(root, "experiments/gigacode/gate.test.ts");
		mkdirSync(dirname(experimentTest), { recursive: true });
		writeFileSync(experimentTest, "export {};\n");

		const bin = join(root, "bin");
		mkdirSync(bin);
		const cargo = join(bin, "cargo");
		writeFileSync(cargo, '#!/bin/sh\nprintf \'{"packages":[]}\'\n');
		chmodSync(cargo, 0o755);

		const result = spawnSync(process.execPath, [script], {
			cwd: root,
			encoding: "utf8",
			env: { ...process.env, PATH: `${bin}:${process.env.PATH ?? ""}` },
		});
		assert.equal(result.status, 0, result.stderr || result.stdout);
		assert.match(result.stdout, /check-layout: OK/);
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
});

test("ignores generated Codex sources and allows experiment-local tests", () => {
	const root = mkdtempSync(join(tmpdir(), "agentos-layout-"));
	try {
		for (const testPath of [
			"toolchain/.codex-build/checkout/sdk/typescript/tests/generated.test.ts",
			"experiments/gigacode/gigacode.e2e.test.ts",
		]) {
			const path = join(root, testPath);
			mkdirSync(dirname(path), { recursive: true });
			writeFileSync(path, "export {};\n");
		}

		const bin = join(root, "bin");
		mkdirSync(bin);
		const cargo = join(bin, "cargo");
		writeFileSync(cargo, '#!/bin/sh\nprintf \'{"packages":[]}\'\n');
		chmodSync(cargo, 0o755);

		const result = spawnSync(process.execPath, [script], {
			cwd: root,
			encoding: "utf8",
			env: { ...process.env, PATH: `${bin}:${process.env.PATH ?? ""}` },
		});
		assert.equal(result.status, 0, result.stderr || result.stdout);
		assert.match(result.stdout, /check-layout: OK/);
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
});
