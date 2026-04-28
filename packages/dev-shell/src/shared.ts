import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import path from "node:path";

export interface WorkspacePaths {
	workspaceRoot: string;
	hostProjectRoot: string;
	wasmCommandsDir: string;
	wasmCommandDirs: string[];
	realProviderEnvFile: string;
}

export function findWorkspaceRoot(startDir: string): string {
	let current = path.resolve(startDir);

	while (true) {
		if (existsSync(path.join(current, "pnpm-workspace.yaml"))) {
			return current;
		}

		const parent = path.dirname(current);
		if (parent === current) {
			throw new Error(`Could not locate pnpm-workspace.yaml from ${startDir}`);
		}
		current = parent;
	}
}

export function resolveWorkspacePaths(startDir: string): WorkspacePaths {
	const workspaceRoot = findWorkspaceRoot(startDir);
	const builtWasmCommandsDir = path.join(
		workspaceRoot,
		"registry",
		"native",
		"target",
		"wasm32-wasip1",
		"release",
		"commands",
	);
	const packagedCoreutilsWasmDir = path.join(
		workspaceRoot,
		"registry",
		"software",
		"coreutils",
		"wasm",
	);
	const wasmCommandDirs = [
		builtWasmCommandsDir,
		packagedCoreutilsWasmDir,
	].filter((commandDir, index, allDirs) => {
		return existsSync(commandDir) && allDirs.indexOf(commandDir) === index;
	});
	return {
		workspaceRoot,
		// Dev-shell used to live in a nested runtime repo. In this monorepo,
		// the workspace root itself is the host-visible project root.
		hostProjectRoot: workspaceRoot,
		wasmCommandsDir: wasmCommandDirs[0] ?? packagedCoreutilsWasmDir,
		wasmCommandDirs,
		realProviderEnvFile: path.join(homedir(), "misc", "env.txt"),
	};
}

export function stripWrappingQuotes(value: string): string {
	if (
		(value.startsWith('"') && value.endsWith('"')) ||
		(value.startsWith("'") && value.endsWith("'"))
	) {
		return value.slice(1, -1);
	}
	return value;
}

export function parseEnvFile(filePath: string): Record<string, string> {
	const parsed: Record<string, string> = {};
	const contents = readFileSync(filePath, "utf8");

	for (const rawLine of contents.split(/\r?\n/)) {
		const line = rawLine.trim();
		if (!line || line.startsWith("#")) continue;

		const withoutExport = line.startsWith("export ")
			? line.slice("export ".length).trim()
			: line;
		const separator = withoutExport.indexOf("=");
		if (separator <= 0) continue;

		const key = withoutExport.slice(0, separator).trim();
		const rawValue = withoutExport.slice(separator + 1).trim();
		if (!key) continue;

		parsed[key] = stripWrappingQuotes(rawValue);
	}

	return parsed;
}

export function collectShellEnv(envFilePath?: string): Record<string, string> {
	const shellEnv: Record<string, string> = {};

	for (const [key, value] of Object.entries(process.env)) {
		if (typeof value === "string") {
			shellEnv[key] = value;
		}
	}

	const sourcePath = envFilePath ?? path.join(homedir(), "misc", "env.txt");
	if (existsSync(sourcePath)) {
		for (const [key, value] of Object.entries(parseEnvFile(sourcePath))) {
			if (!(key in shellEnv)) {
				shellEnv[key] = value;
			}
		}
	}

	if (!shellEnv.TERM) shellEnv.TERM = "xterm-256color";
	if (!shellEnv.COLORTERM) shellEnv.COLORTERM = "truecolor";

	return shellEnv;
}
