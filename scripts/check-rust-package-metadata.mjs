import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, relative, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const defaultRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");

const requiredPackages = [
	{
		name: "agentos-acp-protocol",
		manifestPath: "crates/acp-protocol/Cargo.toml",
		targets: [{ kind: "lib", name: "agentos_acp_protocol" }],
	},
	{
		name: "agentos-sidecar",
		manifestPath: "crates/sidecar/Cargo.toml",
		targets: [{ kind: "bin", name: "agentos-sidecar" }],
	},
	{
		name: "agentos-client",
		manifestPath: "crates/client/Cargo.toml",
		targets: [{ kind: "lib", name: "agentos_client" }],
	},
];

function readCargoMetadata(root) {
	const stdout = execFileSync("cargo", ["metadata", "--format-version", "1", "--no-deps"], {
		cwd: root,
		encoding: "utf8",
	});
	return JSON.parse(stdout);
}

function targetExists(pkg, expected) {
	return pkg.targets.some(
		(target) => target.name === expected.name && target.kind.includes(expected.kind),
	);
}

function formatPath(root, path) {
	return relative(root, path) || ".";
}

function packageByName(metadata, name) {
	return metadata.packages.find((pkg) => pkg.name === name);
}

function validatePackage(root, metadata, expected, errors) {
	const pkg = packageByName(metadata, expected.name);
	if (!pkg) {
		errors.push(`missing Rust package ${expected.name}`);
		return;
	}

	const expectedManifest = resolve(root, expected.manifestPath);
	if (resolve(pkg.manifest_path) !== expectedManifest) {
		errors.push(
			`${expected.name} manifest moved: expected ${expected.manifestPath}, found ${formatPath(root, pkg.manifest_path)}`,
		);
	}

	if (pkg.publish === false) {
		errors.push(`${expected.name} must remain publishable`);
	}

	if (pkg.license !== "Apache-2.0") {
		errors.push(`${expected.name} must use Apache-2.0 license metadata`);
	}
	if (pkg.repository !== "https://github.com/rivet-dev/agent-os") {
		errors.push(`${expected.name} must use the workspace repository metadata`);
	}
	if (typeof pkg.description !== "string" || pkg.description.length === 0) {
		errors.push(`${expected.name} must have a package description`);
	}

	for (const target of expected.targets) {
		if (!targetExists(pkg, target)) {
			errors.push(
				`${expected.name} must expose a ${target.kind} target named ${target.name}`,
			);
		}
	}
}

function validateFlatCrateNames(root, metadata, errors) {
	for (const pkg of metadata.packages) {
		const manifestPath = relative(root, pkg.manifest_path).split("\\").join("/");
		const match = manifestPath.match(/^crates\/([^/]+)\/Cargo\.toml$/);
		if (!match) continue;

		const expectedName = `agentos-${match[1]}`;
		if (pkg.name !== expectedName) {
			errors.push(
				`${manifestPath} package name must be ${expectedName}, found ${pkg.name}`,
			);
		}
	}
}

export function checkRustPackageMetadata(options = {}) {
	const root = resolve(options.root ?? defaultRoot);
	const metadata = options.metadata ?? readCargoMetadata(root);
	const errors = [];

	for (const expected of requiredPackages) {
		validatePackage(root, metadata, expected, errors);
	}
	validateFlatCrateNames(root, metadata, errors);

	return errors;
}

function parseArgs(argv) {
	let root = defaultRoot;
	for (let i = 0; i < argv.length; i++) {
		const arg = argv[i];
		if (arg === "--root") {
			const value = argv[++i];
			if (!value) {
				throw new Error("--root requires a path");
			}
			root = value;
			continue;
		}
		if (arg.startsWith("--root=")) {
			root = arg.slice("--root=".length);
			continue;
		}
		throw new Error(`unknown argument: ${arg}`);
	}
	return { root };
}

export function main(argv = process.argv.slice(2)) {
	const options = parseArgs(argv);
	const root = resolve(options.root);
	if (!existsSync(resolve(root, "Cargo.toml"))) {
		throw new Error(`Cargo.toml not found under ${root}`);
	}
	const errors = checkRustPackageMetadata({ root });
	if (errors.length > 0) {
		for (const error of errors) {
			console.error(error);
		}
		process.exitCode = 1;
		return;
	}

	console.log("Rust package metadata ok");
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
	main();
}
