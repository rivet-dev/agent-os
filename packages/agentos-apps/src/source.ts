import { lstat, readdir, readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { AgentOSAppsError } from "./errors.js";
import { normalizeAppPath } from "./runtime.js";
import type { DeployAppInput } from "./types.js";

const DEFAULT_MAX_FILES = 2_000;
const DEFAULT_MAX_SOURCE_BYTES = 4 * 1024 * 1024;
const DEFAULT_MAX_FILE_BYTES = 2 * 1024 * 1024;
const IGNORED_DIRECTORIES = new Set([".git", ".agentos", "node_modules"]);
const APP_ID_PATTERN = /^[a-z0-9](?:[a-z0-9-]{0,62})$/;

export function validateAppId(appId: string): void {
	if (!APP_ID_PATTERN.test(appId)) {
		throw new AgentOSAppsError(
			"agentos_apps_invalid_app_id",
			"appId must be 1-63 lowercase letters, digits, or hyphens, beginning with a letter or digit",
			{ appId },
		);
	}
}

function enforceFileBounds(
	path: string,
	content: Uint8Array,
	state: { files: number; bytes: number },
): void {
	state.files += 1;
	state.bytes += content.byteLength;
	if (state.files > DEFAULT_MAX_FILES) {
		throw new AgentOSAppsError(
			"agentos_apps_file_limit",
			`application contains more than maxFiles ${DEFAULT_MAX_FILES}; reduce the source tree`,
			{ limit: DEFAULT_MAX_FILES },
		);
	}
	if (content.byteLength > DEFAULT_MAX_FILE_BYTES) {
		throw new AgentOSAppsError(
			"agentos_apps_file_size_limit",
			`${path} is ${content.byteLength} bytes, exceeding maxFileBytes ${DEFAULT_MAX_FILE_BYTES}; reduce the file size`,
			{ path, observed: content.byteLength, limit: DEFAULT_MAX_FILE_BYTES },
		);
	}
	if (state.bytes > DEFAULT_MAX_SOURCE_BYTES) {
		throw new AgentOSAppsError(
			"agentos_apps_source_limit",
			`application source exceeds maxSourceBytes ${DEFAULT_MAX_SOURCE_BYTES}; reduce the source tree`,
			{ observed: state.bytes, limit: DEFAULT_MAX_SOURCE_BYTES },
		);
	}
}

async function loadDirectory(source: URL): Promise<Record<string, Uint8Array>> {
	if (source.protocol !== "file:") {
		throw new AgentOSAppsError(
			"agentos_apps_invalid_source",
			"deployApp source must be a file: directory URL",
			{ protocol: source.protocol },
		);
	}
	const root = fileURLToPath(source);
	const rootStat = await lstat(root);
	if (!rootStat.isDirectory() || rootStat.isSymbolicLink()) {
		throw new AgentOSAppsError(
			"agentos_apps_invalid_source",
			"deployApp source must reference a real directory, not a symlink",
		);
	}
	const output: Record<string, Uint8Array> = {};
	const bounds = { files: 0, bytes: 0 };

	const visit = async (directory: string, prefix: string): Promise<void> => {
		const entries = await readdir(directory, { withFileTypes: true });
		entries.sort((a, b) => a.name.localeCompare(b.name));
		for (const entry of entries) {
			const relative = prefix ? `${prefix}/${entry.name}` : entry.name;
			const path = `${directory}/${entry.name}`;
			const entryStat = await lstat(path);
			if (entryStat.isSymbolicLink()) {
				throw new AgentOSAppsError(
					"agentos_apps_source_symlink",
					`application source contains unsupported symlink ${relative}`,
					{ path: relative },
				);
			}
			if (entryStat.isDirectory()) {
				if (!IGNORED_DIRECTORIES.has(entry.name)) {
					await visit(path, relative);
				}
				continue;
			}
			if (!entryStat.isFile()) {
				throw new AgentOSAppsError(
					"agentos_apps_source_file_type",
					`application source contains unsupported file type ${relative}`,
					{ path: relative },
				);
			}
			const normalized = normalizeAppPath(relative);
			const content = new Uint8Array(await readFile(path));
			enforceFileBounds(normalized, content, bounds);
			output[normalized] = content;
		}
	};

	await visit(root, "");
	return output;
}

export async function prepareSource(
	input: DeployAppInput,
): Promise<Record<string, Uint8Array>> {
	validateAppId(input.appId);
	if ("source" in input && input.source) return loadDirectory(input.source);

	const output: Record<string, Uint8Array> = {};
	const bounds = { files: 0, bytes: 0 };
	for (const [path, value] of Object.entries(input.files).sort(([a], [b]) =>
		a.localeCompare(b),
	)) {
		const normalized = normalizeAppPath(path);
		const content =
			typeof value === "string" ? new TextEncoder().encode(value) : value;
		enforceFileBounds(normalized, content, bounds);
		output[normalized] = new Uint8Array(content);
	}
	return output;
}
