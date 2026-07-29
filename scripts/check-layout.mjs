import { execFileSync } from "node:child_process";
import {
	existsSync,
	readdirSync,
	readFileSync,
	statSync,
} from "node:fs";
import { join, relative, sep } from "node:path";

const root = process.cwd();
const failures = [];

const fail = (message) => failures.push(message);
const rel = (path) => relative(root, path).split(sep).join("/");
const readJson = (path) => JSON.parse(readFileSync(path, "utf8"));

if (existsSync(join(root, "registry"))) {
	fail("registry/ must not exist; use software/ and toolchain/ instead");
}

const ignoredDirs = new Set([
	".git",
	"node_modules",
	"target",
	"dist",
	"build",
	"vendor",
	".turbo",
]);

const walk = (dir, visit) => {
	for (const entry of readdirSync(dir, { withFileTypes: true })) {
		const path = join(dir, entry.name);
		if (
			entry.isDirectory() &&
			(ignoredDirs.has(entry.name) || rel(path) === ".claude/worktrees")
		) {
			continue;
		}
		if (entry.isDirectory()) {
			walk(path, visit);
		} else {
			visit(path);
		}
	}
};

const allowedTestHomes = [
	/^software\/[^/]+\/test\/.+\.test\.ts$/,
	/^toolchain\/conformance\/.+\.test\.ts$/,
	/^packages\/[^/]+\/tests\/.+\.test\.ts$/,
	/^experiments\/[^/]+\/.+\.test\.ts$/,
	/^scripts\/.+\.test\.ts$/,
];

walk(root, (path) => {
	const normalized = rel(path);
	if (!normalized.endsWith(".test.ts")) return;
	if (!allowedTestHomes.some((pattern) => pattern.test(normalized))) {
		fail(`${normalized} is not in an allowed test home`);
	}
});

const softwareRoot = join(root, "software");
if (existsSync(softwareRoot)) {
	for (const dir of readdirSync(softwareRoot, { withFileTypes: true })) {
		if (!dir.isDirectory()) continue;
		const packageDir = join(softwareRoot, dir.name);
		const manifestPath = join(packageDir, "agentos-package.json");
		if (!existsSync(manifestPath)) continue;
		const manifest = readJson(manifestPath);
		const commands = manifest.commands ?? [];
		if (commands.length > 0 && !existsSync(join(packageDir, "test"))) {
			fail(`software/${dir.name} declares commands but has no test/ directory`);
		}
		if (manifest.registry && !manifest.registry.category) {
			fail(`software/${dir.name} registry block is missing category`);
		}
	}
}

const colocatedCargoTomls = [];
const collectCargoTomls = (dir) => {
	if (!existsSync(dir)) return;
	for (const entry of readdirSync(dir, { withFileTypes: true })) {
		const path = join(dir, entry.name);
		if (entry.isDirectory()) {
			collectCargoTomls(path);
		} else if (entry.name === "Cargo.toml") {
			colocatedCargoTomls.push(path);
		}
	}
};
collectCargoTomls(join(root, "software"));

for (const cargoToml of colocatedCargoTomls) {
	const normalized = rel(cargoToml);
	if (!/\/native\/crates\/[^/]+\/Cargo\.toml$/.test(normalized)) continue;
	const text = readFileSync(cargoToml, "utf8");
	if (!/^\[package\]\nworkspace = "\.\.\/\.\.\/\.\.\/\.\.\/\.\.\/toolchain"/m.test(text)) {
		fail(`${normalized} must explicitly belong to the toolchain workspace`);
	}
}

try {
	const metadata = JSON.parse(
		execFileSync("cargo", ["metadata", "--format-version", "1", "--no-deps"], {
			cwd: root,
			encoding: "utf8",
			stdio: ["ignore", "pipe", "pipe"],
		}),
	);
	for (const pkg of metadata.packages ?? []) {
		const manifestPath = pkg.manifest_path.split(sep).join("/");
		if (manifestPath.includes("/software/")) {
			fail(`main Cargo workspace claims colocated crate ${manifestPath}`);
		}
	}
} catch (error) {
	fail(`cargo metadata failed for root workspace: ${error.message}`);
}

if (failures.length > 0) {
	for (const failure of failures) {
		console.error(`check-layout: ${failure}`);
	}
	process.exit(1);
}

console.log(
	`check-layout: OK (${colocatedCargoTomls.length} colocated Cargo.toml files checked)`,
);
