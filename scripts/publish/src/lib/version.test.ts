import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { DEFAULT_SIDECAR_PLATFORMS } from "./packages.js";
import { bumpCargoVersions, bumpPackageJsons } from "./version.js";

async function writeJson(root: string, rel: string, value: unknown) {
	const path = join(root, rel);
	await mkdir(join(path, ".."), { recursive: true });
	await writeFile(path, `${JSON.stringify(value, null, "\t")}\n`);
}

test("bumpCargoVersions bumps [workspace.package] and AgentOS path deps", async () => {
	const repoRoot = await mkdtemp(join(tmpdir(), "agentos-version-test-"));
	try {
		await writeFile(
			join(repoRoot, "Cargo.toml"),
			`[workspace.package]
version = "0.2.0"

[workspace.dependencies]
agentos-protocol = { path = "crates/agentos-protocol", version = "0.2.0-rc.3" }
agentos-kernel = { path = "crates/kernel", version = "0.2.0-rc.3" }
serde = "1"
`,
		);
		await mkdir(join(repoRoot, "crates", "excluded-core"), { recursive: true });
		await writeFile(
			join(repoRoot, "crates", "excluded-core", "Cargo.toml"),
			`[package]
name = "agentos-excluded-core"
version = "0.2.0"

[dependencies]
agentos-protocol = { path = "../agentos-protocol", version = "0.2.0" }
`,
		);

		await bumpCargoVersions(repoRoot, "0.3.0");

		const cargoToml = await readFile(join(repoRoot, "Cargo.toml"), "utf8");
		// a6 workspace version bumped...
		assert.match(cargoToml, /\[workspace\.package\]\nversion = "0\.3\.0"/);
		// ...AgentOS-owned crate deps (path = "crates/...") bumped...
		assert.match(
			cargoToml,
			/agentos-protocol = \{ path = "crates\/agentos-protocol", version = "0\.3\.0" \}/,
		);
		assert.match(
			cargoToml,
			/agentos-kernel = \{ path = "crates\/kernel", version = "0\.3\.0" \}/,
		);
		assert.match(cargoToml, /serde = "1"/);
		const excludedCargoToml = await readFile(
			join(repoRoot, "crates", "excluded-core", "Cargo.toml"),
			"utf8",
		);
		assert.match(excludedCargoToml, /version = "0\.3\.0"/);
		assert.match(
			excludedCargoToml,
			/agentos-protocol = \{ path = "\.\.\/agentos-protocol", version = "0\.3\.0" \}/,
		);
	} finally {
		await rm(repoRoot, { recursive: true, force: true });
	}
});

test("bumpPackageJsons injects sidecar platform optional dependencies", async () => {
	const repoRoot = await mkdtemp(join(tmpdir(), "agentos-version-test-"));
	try {
		await writeJson(repoRoot, "package.json", {
			name: "agentos-workspace",
			private: true,
			packageManager: "pnpm@10.13.1",
		});
		await writeFile(
			join(repoRoot, "pnpm-workspace.yaml"),
			[
				"packages:",
				"  - packages/*",
				"  - packages/sidecar-binary/npm/*",
				"  - packages/runtime-sidecar/npm/*",
				"",
			].join("\n"),
		);
		for (const [rel, name] of [
			["packages/agentos", "@rivet-dev/agentos"],
			["packages/core", "@rivet-dev/agentos-core"],
			["packages/sidecar-binary", "@rivet-dev/agentos-sidecar"],
			["packages/runtime-sidecar", "@rivet-dev/agentos-runtime-sidecar"],
			...DEFAULT_SIDECAR_PLATFORMS.map((platform) => [
				`packages/sidecar-binary/npm/${platform}`,
				`@rivet-dev/agentos-sidecar-${platform}`,
			]),
			...DEFAULT_SIDECAR_PLATFORMS.map((platform) => [
				`packages/runtime-sidecar/npm/${platform}`,
				`@rivet-dev/agentos-runtime-sidecar-${platform}`,
			]),
		]) {
			await writeJson(repoRoot, join(rel, "package.json"), {
				name,
				version: "0.0.0",
			});
		}

		await bumpPackageJsons(repoRoot, "0.3.0");

		const sidecarManifest = JSON.parse(
			await readFile(
				join(repoRoot, "packages/sidecar-binary/package.json"),
				"utf8",
			),
		);
		assert.deepEqual(
			sidecarManifest.optionalDependencies,
			Object.fromEntries(
				DEFAULT_SIDECAR_PLATFORMS.map((platform) => [
					`@rivet-dev/agentos-sidecar-${platform}`,
					"0.3.0",
				]).sort(),
			),
		);

		const runtimeSidecarManifest = JSON.parse(
			await readFile(
				join(repoRoot, "packages/runtime-sidecar/package.json"),
				"utf8",
			),
		);
		assert.deepEqual(
			runtimeSidecarManifest.optionalDependencies,
			Object.fromEntries(
				DEFAULT_SIDECAR_PLATFORMS.map((platform) => [
					`@rivet-dev/agentos-runtime-sidecar-${platform}`,
					"0.3.0",
				]).sort(),
			),
		);

	} finally {
		await rm(repoRoot, { recursive: true, force: true });
	}
});

test("bumpPackageJsons pins lockstep and independent AgentOS Apps runtimes", async () => {
	const repoRoot = await mkdtemp(join(tmpdir(), "agentos-version-test-"));
	try {
		await writeJson(repoRoot, "package.json", {
			name: "agentos-workspace",
			private: true,
			packageManager: "pnpm@10.13.1",
		});
		await writeFile(
			join(repoRoot, "pnpm-workspace.yaml"),
			["packages:", "  - packages/*", "  - software/*", ""].join("\n"),
		);
		await writeJson(repoRoot, "packages/apps/package.json", {
			name: "@rivet-dev/agentos-apps",
			version: "0.0.1",
			dependencies: {
				"@agentos-software/apps-builder": "workspace:*",
				"@agentos-software/sh": "workspace:*",
				"@agentos-software/tar": "workspace:*",
			},
		});
		for (const name of [
			"@agentos-software/apps-builder",
			"@agentos-software/sh",
			"@agentos-software/tar",
		]) {
			await writeJson(
				repoRoot,
				`software/${name.split("/")[1]}/package.json`,
				{ name, version: "0.0.1" },
			);
		}

		await bumpPackageJsons(repoRoot, "0.0.0-preview.abc1234", {
			resolveNpmLatestVersion: async (name) => {
				assert.equal(name, "@agentos-software/tar");
				return "0.3.5";
			},
		});

		const appsManifest = JSON.parse(
			await readFile(
				join(repoRoot, "packages/apps/package.json"),
				"utf8",
			),
		);
		assert.deepEqual(appsManifest.dependencies, {
			"@agentos-software/apps-builder": "0.0.0-preview.abc1234",
			"@agentos-software/sh": "0.0.0-preview.abc1234",
			"@agentos-software/tar": "0.3.5",
		});
	} finally {
		await rm(repoRoot, { recursive: true, force: true });
	}
});
