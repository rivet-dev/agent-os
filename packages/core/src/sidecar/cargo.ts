import { accessSync, constants as fsConstants, existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { homedir } from "node:os";
import path from "node:path";

const CARGO_BINARY_NAME = process.platform === "win32" ? "cargo.exe" : "cargo";

function hasPathSeparator(candidate: string): boolean {
	return candidate.includes("/") || candidate.includes("\\");
}

function isExecutableFile(candidate: string): boolean {
	try {
		if (!statSync(candidate).isFile()) {
			return false;
		}
		accessSync(candidate, fsConstants.X_OK);
		return true;
	} catch {
		return false;
	}
}

function resolveExecutableOnPath(binaryName: string): string | null {
	const pathEntries = (process.env.PATH ?? "")
		.split(path.delimiter)
		.map((entry) => entry.trim())
		.filter(Boolean);

	for (const entry of pathEntries) {
		const candidate = path.join(entry, binaryName);
		if (isExecutableFile(candidate)) {
			return candidate;
		}
	}

	return null;
}

function resolveExecutableCandidate(candidate: string): string | null {
	if (hasPathSeparator(candidate)) {
		return isExecutableFile(candidate) ? candidate : null;
	}

	return resolveExecutableOnPath(candidate);
}

type ToolchainCargo = {
	cargoPath: string;
	rustupHome: string;
};

function getToolchainCargoFromRustupHome(rustupHome: string): ToolchainCargo | null {
	const toolchainsDir = path.join(rustupHome, "toolchains");
	if (!existsSync(toolchainsDir)) {
		return null;
	}

	const settingsPath = path.join(rustupHome, "settings.toml");
	const orderedToolchains: string[] = [];
	if (existsSync(settingsPath)) {
		const defaultToolchain = readFileSync(settingsPath, "utf8")
			.match(/^default_toolchain\s*=\s*"([^"]+)"/m)?.[1]
			?.trim();
		if (defaultToolchain) {
			orderedToolchains.push(defaultToolchain);
		}
	}

	for (const entry of readdirSync(toolchainsDir)) {
		if (!orderedToolchains.includes(entry)) {
			orderedToolchains.push(entry);
		}
	}

	for (const toolchain of orderedToolchains) {
		const cargoPath = path.join(
			toolchainsDir,
			toolchain,
			"bin",
			CARGO_BINARY_NAME,
		);
		if (existsSync(cargoPath)) {
			return { cargoPath, rustupHome };
		}
	}

	return null;
}

function inferRustupHomesFromPath(): string[] {
	const rustupHomes = new Set<string>();
	const pathEntries = (process.env.PATH ?? "")
		.split(path.delimiter)
		.map((entry) => entry.trim())
		.filter(Boolean);

	for (const entry of pathEntries) {
		if (path.basename(entry) !== "bin") {
			continue;
		}

		const parentDir = path.dirname(entry);
		if (existsSync(path.join(parentDir, "toolchains"))) {
			rustupHomes.add(parentDir);
		}

		const siblingRoot = path.dirname(parentDir);
		try {
			for (const sibling of readdirSync(siblingRoot, { withFileTypes: true })) {
				if (!sibling.isDirectory()) {
					continue;
				}
				const siblingPath = path.join(siblingRoot, sibling.name);
				if (existsSync(path.join(siblingPath, "toolchains"))) {
					rustupHomes.add(siblingPath);
				}
			}
		} catch {}
	}

	return [...rustupHomes];
}

function ensureToolchainEnvironment(toolchainCargo: ToolchainCargo): void {
	const toolchainBin = path.dirname(toolchainCargo.cargoPath);
	const currentPathEntries = (process.env.PATH ?? "")
		.split(path.delimiter)
		.filter(Boolean);
	if (!currentPathEntries.includes(toolchainBin)) {
		process.env.PATH = [toolchainBin, ...currentPathEntries].join(path.delimiter);
	}
	if (!process.env.RUSTUP_HOME) {
		process.env.RUSTUP_HOME = toolchainCargo.rustupHome;
	}
	const toolchainName = path.basename(path.dirname(toolchainBin));
	if (!process.env.RUSTUP_TOOLCHAIN) {
		process.env.RUSTUP_TOOLCHAIN = toolchainName;
	}
}

export function findCargoBinary(): string | null {
	const explicitCargo = process.env.CARGO?.trim();
	const rustupHomes = [
		process.env.RUSTUP_HOME?.trim(),
		path.join(homedir(), ".rustup"),
		...inferRustupHomesFromPath(),
	].filter((candidate): candidate is string => Boolean(candidate));
	const toolchainCargoCandidates = rustupHomes
		.map((rustupHome) => getToolchainCargoFromRustupHome(rustupHome))
		.filter((candidate): candidate is ToolchainCargo => Boolean(candidate));
	if (toolchainCargoCandidates.length > 0) {
		ensureToolchainEnvironment(toolchainCargoCandidates[0]);
	}
	const candidates = [
		explicitCargo,
		...toolchainCargoCandidates.map((candidate) => candidate.cargoPath),
		path.join(homedir(), ".cargo", "bin", CARGO_BINARY_NAME),
		CARGO_BINARY_NAME,
	].filter((candidate): candidate is string => Boolean(candidate));

	for (const candidate of candidates) {
		const resolved = resolveExecutableCandidate(candidate);
		if (resolved) {
			return resolved;
		}
	}

	return null;
}

export function resolveCargoBinary(): string {
	return findCargoBinary() ?? CARGO_BINARY_NAME;
}
