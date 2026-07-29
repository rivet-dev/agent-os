import {
	existsSync,
	lstatSync,
	mkdirSync,
	mkdtempSync,
	readdirSync,
	readFileSync,
	rmSync,
	symlinkSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import {
	copyWasmCommands,
	requiredPackageArtifactNames,
	requiredSoftwareCommandNames,
} from "../scripts/copy-wasm-commands.mjs";

const roots: string[] = [];

function tempRoot(): string {
	const root = mkdtempSync(join(tmpdir(), "agentos-copy-commands-"));
	roots.push(root);
	return root;
}

function writeManifest(
	softwareRoot: string,
	packageName: string,
	manifest: Record<string, unknown>,
): void {
	const packageDir = join(softwareRoot, packageName);
	mkdirSync(packageDir, { recursive: true });
	writeFileSync(
		join(packageDir, "agentos-package.json"),
		`${JSON.stringify(manifest)}\n`,
	);
}

function fixture() {
	const root = tempRoot();
	const sourceDir = join(root, "source");
	const destDir = join(root, "dest");
	const softwareRoot = join(root, "software");
	mkdirSync(sourceDir, { recursive: true });
	mkdirSync(destDir, { recursive: true });
	mkdirSync(softwareRoot, { recursive: true });
	writeManifest(softwareRoot, "default-tools", {
		commands: ["alpha"],
		aliases: { "alpha-alias": "alpha" },
		stubs: ["legacy"],
	});
	for (const [packageName, command] of [
		["codex-cli", "codex"],
		["duckdb", "duckdb"],
		["vim", "vim"],
	]) {
		writeManifest(softwareRoot, packageName, { commands: [command] });
	}
	return { root, sourceDir, destDir, softwareRoot };
}

afterEach(() => {
	for (const root of roots.splice(0)) {
		rmSync(root, { recursive: true, force: true });
	}
});

describe("copy WASM commands", () => {
	it("derives commands, aliases, and stubs while excluding optional builds", () => {
		const { softwareRoot } = fixture();
		expect(requiredSoftwareCommandNames(softwareRoot)).toEqual([
			"alpha",
			"alpha-alias",
			"legacy",
		]);
	});

	it("derives a required command contract for selected packages", () => {
		const { softwareRoot } = fixture();
		expect(
			requiredPackageArtifactNames(softwareRoot, ["default-tools"]),
		).toEqual(["_stubs", "alpha", "alpha-alias", "legacy"]);
	});

	it("validates a selected package without requiring the full registry", () => {
		const { sourceDir, destDir, softwareRoot } = fixture();
		for (const name of ["_stubs", "alpha", "alpha-alias", "legacy"]) {
			writeFileSync(join(sourceDir, name), name);
		}
		writeFileSync(join(sourceDir, "unselected-extra"), "extra");

		copyWasmCommands({
			sourceDir,
			destDir,
			softwareRoot,
			requireCommands: true,
			requiredPackageNames: ["default-tools"],
			log: () => {},
		});

		expect(readdirSync(destDir).sort()).toEqual([
			"_stubs",
			"alpha",
			"alpha-alias",
			"legacy",
		]);
	});

	it("fails required preflight without erasing previously vendored commands", () => {
		const { sourceDir, destDir, softwareRoot } = fixture();
		writeFileSync(join(sourceDir, "alpha"), "alpha");
		writeFileSync(join(destDir, "known-good"), "preserve me");

		expect(() =>
			copyWasmCommands({
				sourceDir,
				destDir,
				softwareRoot,
				requireCommands: true,
				log: () => {},
			}),
		).toThrow(/missing required default WASM commands.*alpha-alias, legacy/);
		expect(readFileSync(join(destDir, "known-good"), "utf8")).toBe(
			"preserve me",
		);
	});

	it("copies optional extras with exact basenames and dereferences aliases", () => {
		const { sourceDir, destDir, softwareRoot } = fixture();
		for (const name of ["alpha", "alpha-alias", "legacy", "codex"]) {
			writeFileSync(join(sourceDir, name), name);
		}
		writeFileSync(join(sourceDir, "extra-real"), "extra");
		symlinkSync("extra-real", join(sourceDir, "extra-alias"));

		copyWasmCommands({
			sourceDir,
			destDir,
			softwareRoot,
			requireCommands: true,
			log: () => {},
		});

		expect(readdirSync(destDir).sort()).toEqual(readdirSync(sourceDir).sort());
		expect(lstatSync(join(destDir, "extra-alias")).isFile()).toBe(true);
		expect(lstatSync(join(destDir, "extra-alias")).isSymbolicLink()).toBe(
			false,
		);
		expect(readFileSync(join(destDir, "extra-alias"), "utf8")).toBe("extra");
	});

	it("rejects an incomplete already-vendored artifact when source is absent", () => {
		const { root, destDir, softwareRoot } = fixture();
		const sourceDir = join(root, "missing-source");
		writeFileSync(join(destDir, "alpha"), "alpha");

		expect(() =>
			copyWasmCommands({
				sourceDir,
				destDir,
				softwareRoot,
				requireCommands: true,
				log: () => {},
			}),
		).toThrow(/missing required default WASM commands.*alpha-alias, legacy/);
		expect(existsSync(join(destDir, "alpha"))).toBe(true);
	});

	it("rejects a selected package artifact without its internal stub source", () => {
		const { root, destDir, softwareRoot } = fixture();
		const sourceDir = join(root, "missing-source");
		for (const name of ["alpha", "alpha-alias", "legacy"]) {
			writeFileSync(join(destDir, name), name);
		}

		expect(() =>
			copyWasmCommands({
				sourceDir,
				destDir,
				softwareRoot,
				requireCommands: true,
				requiredPackageNames: ["default-tools"],
				log: () => {},
			}),
		).toThrow(/missing required default WASM commands.*_stubs/);
	});
});
