import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import {
	EXCLUDED,
	LOCKSTEP_SOFTWARE_PACKAGES,
	assertDiscoverySanity,
	buildMetaPlatformMap,
	discoverPackages,
} from "./packages.js";

const repoRoot = resolve(import.meta.dirname, "../../../..");

function withFixture(fn: (root: string) => void) {
	const root = mkdtempSync(join(tmpdir(), "publish-packages-"));
	try {
		fn(root);
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
}

function writeJson(root: string, rel: string, value: unknown) {
	const path = join(root, rel);
	mkdirSync(join(path, ".."), { recursive: true });
	writeFileSync(path, `${JSON.stringify(value, null, "\t")}\n`);
}

test("discovers Agent OS sidecar resolver packages", () => {
	const packages = discoverPackages(repoRoot);
	const names = packages.map((pkg) => pkg.name);

	const hasAgentOsPackages = names.some((name) =>
		name.startsWith("@rivet-dev/agentos-"),
	);
	if (hasAgentOsPackages) {
		assert(names.includes("@rivet-dev/agentos-sidecar-linux-x64-gnu"));
		assert(names.includes("@rivet-dev/agentos-sidecar"));
		assert(
			names.indexOf("@rivet-dev/agentos-sidecar-linux-x64-gnu") <
				names.indexOf("@rivet-dev/agentos-sidecar"),
		);
	}

	assert(names.includes("@rivet-dev/agentos-runtime-sidecar-linux-x64-gnu"));
	assert(names.includes("@rivet-dev/agentos-runtime-sidecar"));
	assert(
		names.indexOf("@rivet-dev/agentos-runtime-sidecar-linux-x64-gnu") <
			names.indexOf("@rivet-dev/agentos-runtime-sidecar"),
	);
});

test("builds platform map for the agent-os sidecar meta package", () => {
	const packages = discoverPackages(repoRoot);
	const names = packages.map((pkg) => pkg.name);
	const metaMap = buildMetaPlatformMap(packages);

	if (names.includes("@rivet-dev/agentos-sidecar")) {
		assert.deepEqual(metaMap.get("@rivet-dev/agentos-sidecar"), [
			"@rivet-dev/agentos-sidecar-darwin-arm64",
			"@rivet-dev/agentos-sidecar-darwin-x64",
			"@rivet-dev/agentos-sidecar-linux-arm64-gnu",
			"@rivet-dev/agentos-sidecar-linux-x64-gnu",
		]);
		assert.deepEqual(metaMap.get("@rivet-dev/agentos-runtime-sidecar"), [
			"@rivet-dev/agentos-runtime-sidecar-darwin-arm64",
			"@rivet-dev/agentos-runtime-sidecar-darwin-x64",
			"@rivet-dev/agentos-runtime-sidecar-linux-arm64-gnu",
			"@rivet-dev/agentos-runtime-sidecar-linux-x64-gnu",
		]);
	}
});

test("sanity check passes for the agent-os workspace", () => {
	const packages = discoverPackages(repoRoot);
	const names = new Set(packages.map((pkg) => pkg.name));

	assert.doesNotThrow(() => assertDiscoverySanity(packages));
	assert(names.has("@rivet-dev/agentos"));
});

test("publishes only new AgentOS Apps software packages in lockstep", () => {
	const names = discoverPackages(repoRoot).map((pkg) => pkg.name);

	assert(LOCKSTEP_SOFTWARE_PACKAGES.has("@agentos-software/apps-builder"));
	assert(LOCKSTEP_SOFTWARE_PACKAGES.has("@agentos-software/sh"));
	assert(!LOCKSTEP_SOFTWARE_PACKAGES.has("@agentos-software/tar"));
	assert(names.includes("@rivet-dev/agentos-apps"));
	assert(names.includes("@agentos-software/apps-builder"));
	assert(names.includes("@agentos-software/sh"));
	assert(!names.includes("@agentos-software/tar"));
});

test("browser migration packages stay explicitly excluded from publication", () => {
	assert(EXCLUDED.has("@rivet-dev/agentos-browser"));
	assert(EXCLUDED.has("@rivet-dev/agentos-runtime-browser"));
});
