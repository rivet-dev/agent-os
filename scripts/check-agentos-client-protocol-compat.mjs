import {
	existsSync,
	readdirSync,
	readFileSync,
	statSync,
} from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const defaultRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");

const allowedCompatFiles = new Set([]);

const compatPattern = /\bagentos_client::protocol\b/g;
const legacyWireConstantPattern =
	/\bagentos_client::protocol::DEFAULT_MAX_FRAME_BYTES\b/g;
const staleCompatibilityDocPattern =
	/\blive transport still uses the compatibility protocol surface\b/g;
const sidecarCompatPattern = /\bagentos_sidecar::protocol\b/g;

function isDir(path) {
	return existsSync(path) && statSync(path).isDirectory();
}

function collectRustFiles(root, dir = root) {
	if (!isDir(dir)) {
		return [];
	}
	const files = [];
	for (const entry of readdirSync(dir, { withFileTypes: true })) {
		const path = join(dir, entry.name);
		if (entry.isDirectory()) {
			files.push(...collectRustFiles(root, path));
			continue;
		}
		if (entry.isFile() && entry.name.endsWith(".rs")) {
			files.push(path);
		}
	}
	return files;
}

function lineAndColumn(source, index) {
	const prefix = source.slice(0, index);
	const lines = prefix.split("\n");
	return {
		line: lines.length,
		column: lines.at(-1).length + 1,
	};
}

function formatPath(root, path) {
	return relative(root, path).replaceAll("\\", "/");
}

function reportSidecarProtocolUse(errors, source, rel, index) {
	const location = lineAndColumn(source, index);
	errors.push(
		`${rel}:${location.line}:${location.column} imports the agentos sidecar compatibility protocol surface; use agentos_sidecar::wire for generated wire types`,
	);
}

export function checkAgentOsClientProtocolCompat(options = {}) {
	const root = resolve(options.root ?? defaultRoot);
	const clientRoots = [
		join(root, "crates/client/src"),
		join(root, "crates/client/tests"),
	];
	const agentOsSidecarRoots = [
		join(root, "crates/agentos-sidecar/src"),
		join(root, "crates/agentos-sidecar/tests"),
	];
	const errors = [];
	for (const filePath of clientRoots.flatMap((scanRoot) =>
		collectRustFiles(root, scanRoot),
	)) {
		const source = readFileSync(filePath, "utf8");
		const rel = formatPath(root, filePath);
		legacyWireConstantPattern.lastIndex = 0;
		for (const match of source.matchAll(legacyWireConstantPattern)) {
			const location = lineAndColumn(source, match.index ?? 0);
			errors.push(
				`${rel}:${location.line}:${location.column} reads the default frame limit through the compatibility protocol surface; use agentos_client::wire::DEFAULT_MAX_FRAME_BYTES`,
			);
		}
		staleCompatibilityDocPattern.lastIndex = 0;
		for (const match of source.matchAll(staleCompatibilityDocPattern)) {
			const location = lineAndColumn(source, match.index ?? 0);
			errors.push(
				`${rel}:${location.line}:${location.column} documents stale generated-wire migration state; describe agentos_client::wire as the active transport surface`,
			);
		}
		compatPattern.lastIndex = 0;
		for (const match of source.matchAll(compatPattern)) {
			if (!allowedCompatFiles.has(rel)) {
				const location = lineAndColumn(source, match.index ?? 0);
				errors.push(
					`${rel}:${location.line}:${location.column} imports the live protocol compatibility surface; use agentos_client::wire for generated wire types or add this file to the migration inventory with justification`,
				);
			}
		}
	}

	for (const filePath of agentOsSidecarRoots.flatMap((scanRoot) =>
		collectRustFiles(root, scanRoot),
	)) {
		const source = readFileSync(filePath, "utf8");
		const rel = formatPath(root, filePath);
		sidecarCompatPattern.lastIndex = 0;
		for (const match of source.matchAll(sidecarCompatPattern)) {
			reportSidecarProtocolUse(errors, source, rel, match.index ?? 0);
		}
	}

	const sidecarPath = join(root, "crates/client/src/sidecar.rs");
	if (existsSync(sidecarPath)) {
		const sidecarSource = readFileSync(sidecarPath, "utf8");
		if (!sidecarSource.includes("use agentos_sidecar_client::wire;")) {
			errors.push("crates/client/src/sidecar.rs must import agentos_sidecar_client::wire");
		}
		if (!sidecarSource.includes("protocol_version: wire::PROTOCOL_VERSION")) {
			errors.push(
				"crates/client/src/sidecar.rs authenticate request must use wire::PROTOCOL_VERSION",
			);
		}
	}

	return errors;
}

function parseArgs(argv) {
	let root = defaultRoot;
	for (let i = 0; i < argv.length; i++) {
		const arg = argv[i];
		if (arg === "--root") {
			root = argv[++i];
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
	const { root } = parseArgs(argv);
	const errors = checkAgentOsClientProtocolCompat({ root });
	for (const error of errors) {
		console.error(error);
	}
	if (errors.length > 0) {
		process.exitCode = 1;
		return;
	}
	console.log("agent-os protocol compatibility inventory is contained");
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
	main();
}
