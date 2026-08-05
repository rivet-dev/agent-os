// Gate: Agent OS product versions stay pinned in the committed tree.
//
// Release jobs override these versions transiently in CI. The committed state
// must keep Agent OS-owned package.json files and the Rust workspace package at
// 0.0.1.
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const EXPECTED_VERSION = "0.0.1";
const defaultRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");

function parseArgs(argv) {
	const options = { root: defaultRoot };
	for (let i = 0; i < argv.length; i++) {
		const arg = argv[i];
		if (arg === "--root") {
			options.root = argv[++i];
			continue;
		}
		if (arg.startsWith("--root=")) {
			options.root = arg.slice("--root=".length);
			continue;
		}
		throw new Error(`unknown argument: ${arg}`);
	}
	return { root: resolve(options.root) };
}

function toRel(root, path) {
	return relative(root, path).split(sep).join("/");
}

function isExcluded(relPath) {
	if (relPath === "node_modules" || relPath.startsWith("node_modules/")) return true;
	if (relPath === ".claude" || relPath.startsWith(".claude/")) return true;
	if (relPath === "scripts/publish" || relPath.startsWith("scripts/publish/")) return true;
	return relPath
		.split("/")
		.some(
			(part) =>
				part === ".astro" ||
				part === ".cache" ||
				part === ".codex-build" ||
				part === ".eve" ||
				part === ".output" ||
				part === ".turbo" ||
				part === "dist" ||
				part === "fixtures" ||
				part === "node_modules" ||
				part === "target" ||
				part === "vendor" ||
				part === "tests",
		);
}

function isIncludedPackageJson(relPath) {
	return (
		(relPath.startsWith("packages/") ||
			relPath.startsWith("examples/") ||
			relPath === "website/package.json") &&
		relPath.endsWith("/package.json")
	);
}

function walkPackageJsons(root, dir, files) {
	for (const entry of readdirSync(dir, { withFileTypes: true })) {
		const path = resolve(dir, entry.name);
		const relPath = toRel(root, path);
		if (entry.isDirectory()) {
			if (isExcluded(relPath)) continue;
			walkPackageJsons(root, path, files);
			continue;
		}
		if (!entry.isFile() || entry.name !== "package.json") continue;
		if (isExcluded(relPath) || !isIncludedPackageJson(relPath)) continue;
		files.push({ path, relPath });
	}
}

function readPackageVersion(path, relPath, failures) {
	let manifest;
	try {
		manifest = JSON.parse(readFileSync(path, "utf8"));
	} catch (err) {
		failures.push(`${relPath} could not be parsed: ${err.message}`);
		return { hasVersion: false };
	}
	if (manifest.private) return { hasVersion: false };
	if (!Object.hasOwn(manifest, "version")) return { hasVersion: false };
	return { hasVersion: true, version: manifest.version };
}

function readWorkspacePackageVersion(root, failures) {
	const cargoPath = join(root, "Cargo.toml");
	if (!existsSync(cargoPath)) {
		failures.push("Cargo.toml is missing");
		return null;
	}

	let inWorkspacePackage = false;
	for (const line of readFileSync(cargoPath, "utf8").split("\n")) {
		const header = line.match(/^\[([^\]]+)\]\s*$/);
		if (header) {
			inWorkspacePackage = header[1] === "workspace.package";
			continue;
		}
		if (!inWorkspacePackage) continue;
		const version = line.match(/^\s*version\s*=\s*"([^"]+)"/)?.[1];
		if (version) return version;
	}

	failures.push("Cargo.toml [workspace.package] is missing version");
	return null;
}

// Internal agent-os crate deps in [workspace.dependencies] carry an explicit
// `version` requirement (path = "crates/..."). It MUST match the workspace
// package version, or cargo fails to resolve (the crate is 0.0.1 but a sibling
// requires the old version).
function checkWorkspaceCrateDeps(root, failures) {
	const cargoPath = join(root, "Cargo.toml");
	if (!existsSync(cargoPath)) return;
	let inDeps = false;
	for (const line of readFileSync(cargoPath, "utf8").split("\n")) {
		const header = line.match(/^\[([^\]]+)\]\s*$/);
		if (header) {
			inDeps = header[1] === "workspace.dependencies";
			continue;
		}
		if (!inDeps) continue;
		if (!/path\s*=\s*"crates\//.test(line)) continue;
		const name = line.match(/^\s*([A-Za-z0-9_-]+)\s*=/)?.[1];
		const version = line.match(/version\s*=\s*"([^"]+)"/)?.[1];
		if (version && version !== EXPECTED_VERSION) {
			failures.push(
				`Cargo.toml [workspace.dependencies] ${name} version is "${version}"`,
			);
		}
	}
}

function auditFixedVersions(options = {}) {
	const root = resolve(options.root ?? defaultRoot);
	const failures = [];
	const packageJsons = [];

	if (!existsSync(root)) {
		return { root, ok: false, packageCount: 0, failures: [`${root} does not exist`] };
	}

	for (const relRoot of ["packages", "examples", "website"]) {
		const scanRoot = join(root, relRoot);
		if (existsSync(scanRoot)) walkPackageJsons(root, scanRoot, packageJsons);
	}
	packageJsons.sort((a, b) => a.relPath.localeCompare(b.relPath));

	let packageCount = 0;
	for (const { path, relPath } of packageJsons) {
		const result = readPackageVersion(path, relPath, failures);
		if (!result.hasVersion) continue;
		packageCount++;
		if (result.version !== EXPECTED_VERSION) {
			failures.push(`${relPath} version is "${result.version}"`);
		}
	}

	const cargoVersion = readWorkspacePackageVersion(root, failures);
	if (cargoVersion !== null && cargoVersion !== EXPECTED_VERSION) {
		failures.push(`Cargo.toml [workspace.package] version is "${cargoVersion}"`);
	}
	checkWorkspaceCrateDeps(root, failures);

	return { root, ok: failures.length === 0, packageCount, failures };
}

export function main(argv = process.argv.slice(2)) {
	const options = parseArgs(argv);
	const result = auditFixedVersions(options);
	if (result.ok) {
		process.stdout.write(
			`verify-fixed-versions: OK (${result.packageCount} package.json + Cargo.toml pinned to ${EXPECTED_VERSION})\n`,
		);
		return 0;
	}
	for (const failure of result.failures) {
		process.stderr.write(`verify-fixed-versions: ${failure}\n`);
	}
	return 1;
}

if (import.meta.url === `file://${process.argv[1]}`) {
	process.exitCode = main();
}
