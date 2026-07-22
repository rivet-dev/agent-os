import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const scriptPath = join(dirname(fileURLToPath(import.meta.url)), "verify-fixed-versions.mjs");

function runGate(root) {
	const args = [scriptPath];
	if (root) args.push(`--root=${root}`);
	return execFileSync(process.execPath, args, { stdio: "pipe" });
}

function gateExitCode(root) {
	try {
		runGate(root);
		return 0;
	} catch (err) {
		return err.status;
	}
}

test("passes on the current tree", () => {
	runGate();
});

test("ignores generated package caches", () => {
	const root = mkdtempSync(join(tmpdir(), "fixed-versions-"));
	try {
		writeFileSync(join(root, "Cargo.toml"), '[workspace.package]\nversion = "0.0.1"\n');
		for (const generatedDir of [".cache", ".eve", ".output", "dist", "node_modules"]) {
			const packageDir = join(root, "packages", "runtime", generatedDir, "fixture");
			mkdirSync(packageDir, { recursive: true });
			writeFileSync(
				join(packageDir, "package.json"),
				JSON.stringify({ name: "third-party-fixture", version: "9.9.9" }),
			);
		}
		const nestedOutput = join(
			root,
			"examples",
			"app",
			".output",
			"server",
			"node_modules",
			"dependency",
		);
		mkdirSync(nestedOutput, { recursive: true });
		writeFileSync(
			join(nestedOutput, "package.json"),
			JSON.stringify({ name: "third-party-output", version: "9.9.9" }),
		);
		runGate(root);
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
});

test("fails when a product package drifts off 0.0.1", () => {
	const root = mkdtempSync(join(tmpdir(), "fixed-versions-"));
	try {
		writeFileSync(join(root, "Cargo.toml"), '[workspace.package]\nversion = "0.0.1"\n');
		mkdirSync(join(root, "packages", "core"), { recursive: true });
		writeFileSync(
			join(root, "packages", "core", "package.json"),
			JSON.stringify({ name: "@rivet-dev/agentos-core", version: "0.2.0-rc.3" }),
		);
		const exitCode = gateExitCode(root);
		if (exitCode !== 1) {
			throw new Error(`expected gate to exit 1 on a drifted version, got ${exitCode}`);
		}
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
});

test("fails when Cargo.toml [workspace.package] drifts off 0.0.1", () => {
	const root = mkdtempSync(join(tmpdir(), "fixed-versions-"));
	try {
		writeFileSync(join(root, "Cargo.toml"), '[workspace.package]\nversion = "0.2.0-rc.3"\n');
		const exitCode = gateExitCode(root);
		if (exitCode !== 1) {
			throw new Error(`expected gate to exit 1 on a drifted Cargo version, got ${exitCode}`);
		}
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
});

test("fails when an internal crate dep requirement drifts off 0.0.1", () => {
	const root = mkdtempSync(join(tmpdir(), "fixed-versions-"));
	try {
		writeFileSync(
			join(root, "Cargo.toml"),
			'[workspace.package]\nversion = "0.0.1"\n\n[workspace.dependencies]\n' +
				'agentos-protocol = { path = "crates/agentos-protocol", version = "0.2.0-rc.3" }\n' +
				'agentos-kernel = { path = "crates/kernel", version = "0.3.4-rc.1" }\n',
		);
		const exitCode = gateExitCode(root);
		if (exitCode !== 1) {
			throw new Error(`expected gate to exit 1 on a drifted crate dep, got ${exitCode}`);
		}
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
});
