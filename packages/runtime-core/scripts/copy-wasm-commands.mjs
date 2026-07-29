#!/usr/bin/env node

/**
 * Vendor the WASM command binaries into `@rivet-dev/agentos-runtime-core` so
 * they ship inside the published tarball. Source aliases are dereferenced
 * because npm does not preserve this command tree's symlinks.
 */

import {
	copyFileSync,
	existsSync,
	lstatSync,
	mkdirSync,
	readFileSync,
	readdirSync,
	realpathSync,
	rmSync,
} from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const PACKAGE_ROOT = fileURLToPath(new URL("..", import.meta.url));
const REPO_ROOT = fileURLToPath(new URL("../../..", import.meta.url));

const SOURCE_DIR = path.join(
	REPO_ROOT,
	"toolchain/target/wasm32-wasip1/release/commands",
);
const DEST_DIR = path.join(PACKAGE_ROOT, "commands");
const SOFTWARE_ROOT = path.join(REPO_ROOT, "software");

// These packages are intentionally outside `make -C toolchain commands`:
// codex is built from its separately pinned upstream checkout, while duckdb
// and vim are explicit heavy builds. If any are present they are still copied;
// they are simply not prerequisites for `--require`.
const OPTIONAL_COMMAND_PACKAGES = new Set(["codex-cli", "duckdb", "vim"]);

function commandNames(manifest, manifestPath) {
	const names = [
		...(manifest.commands ?? []),
		...Object.keys(manifest.aliases ?? {}),
		...(manifest.stubs ?? []),
	];
	for (const name of names) {
		if (typeof name !== "string" || name.length === 0 || name.includes("/")) {
			throw new Error(
				`invalid command name ${JSON.stringify(name)} in ${manifestPath}`,
			);
		}
	}
	return names;
}

/** Derive the default command contract from the software package manifests. */
export function requiredSoftwareCommandNames(softwareRoot = SOFTWARE_ROOT) {
	const required = new Set();
	for (const entry of readdirSync(softwareRoot, { withFileTypes: true })) {
		if (!entry.isDirectory() || OPTIONAL_COMMAND_PACKAGES.has(entry.name)) {
			continue;
		}
		const manifestPath = path.join(
			softwareRoot,
			entry.name,
			"agentos-package.json",
		);
		if (!existsSync(manifestPath)) {
			continue;
		}
		const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
		for (const name of commandNames(manifest, manifestPath)) {
			required.add(name);
		}
	}
	return [...required].sort();
}

export function requiredPackageArtifactNames(
	softwareRoot = SOFTWARE_ROOT,
	packageNames = [],
) {
	const required = new Set();
	for (const packageName of packageNames) {
		const manifestPath = path.join(
			softwareRoot,
			packageName,
			"agentos-package.json",
		);
		if (!existsSync(manifestPath)) {
			throw new Error(`software package manifest not found: ${manifestPath}`);
		}
		const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
		for (const name of commandNames(manifest, manifestPath)) {
			required.add(name);
		}
		if ((manifest.stubs ?? []).length > 0) {
			required.add("_stubs");
		}
	}
	return [...required].sort();
}

function commandFiles(dir, { requireRealFiles = false } = {}) {
	const files = [];
	for (const name of readdirSync(dir).sort()) {
		if (name.startsWith(".")) {
			continue;
		}
		const entryPath = path.join(dir, name);
		const stat = lstatSync(entryPath);
		if (requireRealFiles) {
			if (stat.isSymbolicLink() || !stat.isFile()) {
				throw new Error(`vendored command is not a real file: ${entryPath}`);
			}
			files.push({ name, sourcePath: entryPath });
			continue;
		}

		if (!stat.isFile() && !stat.isSymbolicLink()) {
			throw new Error(`command source is not a file or symlink: ${entryPath}`);
		}
		const sourcePath = stat.isSymbolicLink()
			? realpathSync(entryPath)
			: entryPath;
		if (!lstatSync(sourcePath).isFile()) {
			throw new Error(`command source does not resolve to a file: ${entryPath}`);
		}
		files.push({ name, sourcePath });
	}
	return files;
}

function assertRequiredCommands(files, required, location) {
	const available = new Set(files.map(({ name }) => name));
	const missing = required.filter((name) => !available.has(name));
	if (missing.length > 0) {
		throw new Error(
			`missing required default WASM commands from ${location}: ${missing.join(", ")}`,
		);
	}
}

function assertSameBasenames(sourceFiles, destFiles) {
	const sourceNames = sourceFiles.map(({ name }) => name);
	const destNames = destFiles.map(({ name }) => name);
	if (
		sourceNames.length !== destNames.length ||
		sourceNames.some((name, index) => name !== destNames[index])
	) {
		throw new Error(
			`copied WASM command basenames differ: source=${sourceNames.join(",")} ` +
				`destination=${destNames.join(",")}`,
		);
	}
}

function destIsPopulated(destDir) {
	try {
		return readdirSync(destDir).some((entry) => !entry.startsWith("."));
	} catch {
		return false;
	}
}

export function copyWasmCommands({
	sourceDir = SOURCE_DIR,
	destDir = DEST_DIR,
	softwareRoot = SOFTWARE_ROOT,
	requireCommands = false,
	requiredPackageNames = [],
	log = console.log,
	warn = console.warn,
} = {}) {
	const required = requireCommands
		? requiredPackageNames.length > 0
			? requiredPackageArtifactNames(softwareRoot, requiredPackageNames)
			: requiredSoftwareCommandNames(softwareRoot)
		: [];

	if (!existsSync(sourceDir)) {
		// CI may download a previously validated artifact directly into the
		// package. In require mode, validate that fallback instead of accepting a
		// merely non-empty directory.
		if (destIsPopulated(destDir)) {
			if (requireCommands) {
				const destFiles = commandFiles(destDir, { requireRealFiles: true });
				assertRequiredCommands(destFiles, required, destDir);
			}
			log(
				`Using already-vendored commands at ${path.relative(REPO_ROOT, destDir)}; ` +
					`in-repo build output ${path.relative(REPO_ROOT, sourceDir)} is absent.`,
			);
			return;
		}

		const message =
			`WASM commands not found at ${sourceDir} and none vendored at ${destDir}. ` +
			"Build them with `make -C toolchain commands` (or drop a prebuilt " +
			"commands artifact into the package) before packing so they ship in the tarball.";
		if (requireCommands) {
			throw new Error(message);
		}
		warn(`warning: ${message} Skipping copy.`);
		return;
	}

	// Complete every fallible source/manifest check before clearing a previously
	// valid vendored directory. A partial build must not erase known-good output.
	const availableSourceFiles = commandFiles(sourceDir);
	if (requireCommands) {
		assertRequiredCommands(availableSourceFiles, required, sourceDir);
	}
	const requiredNames = new Set(required);
	const sourceFiles =
		requiredPackageNames.length > 0
			? availableSourceFiles.filter(({ name }) => requiredNames.has(name))
			: availableSourceFiles;

	rmSync(destDir, { recursive: true, force: true });
	mkdirSync(destDir, { recursive: true });

	for (const { name, sourcePath } of sourceFiles) {
		copyFileSync(sourcePath, path.join(destDir, name));
	}

	// The published tree must contain every source basename exactly once and no
	// symlinks. This also catches future non-flat or unsupported command entries.
	const destFiles = commandFiles(destDir, { requireRealFiles: true });
	assertSameBasenames(sourceFiles, destFiles);

	log(
		`Copied ${destFiles.length} WASM command binaries to ${path.relative(REPO_ROOT, destDir)}`,
	);
}

function main() {
	try {
		const requireCoreutils = process.argv.includes("--require-coreutils");
		copyWasmCommands({
			requireCommands:
				requireCoreutils || process.argv.includes("--require"),
			requiredPackageNames: requireCoreutils ? ["coreutils"] : [],
		});
	} catch (error) {
		console.error(`error: ${error instanceof Error ? error.message : String(error)}`);
		process.exitCode = 1;
	}
}

if (
	process.argv[1] !== undefined &&
	path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
	main();
}
