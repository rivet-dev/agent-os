#!/usr/bin/env node

// Build the agentos base filesystem in ONE step: snapshot a stock Alpine
// container, apply the agentos transforms, and write the single
// `base-filesystem.json`. Requires Docker. Run this BY HAND when the base needs
// updating — nothing runs it during a build.
//
// There is exactly ONE committed copy: crates/vfs/assets/base-filesystem.json.
// The vfs crate embeds it directly via `include_str!`; the sidecar reads it via
// `vfs::posix::base_filesystem_json()`; the host bakes the env in as a constant
// (packages/core/src/base-filesystem.ts) and reads no JSON. If you change the env
// here, update that constant to match.

import { execFileSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const DEFAULT_IMAGE = process.env.ALPINE_IMAGE ?? "alpine:3.22";

// The ONE committed copy — embedded into the vfs crate via include_str!.
const OUTPUT_PATHS = [
	fileURLToPath(new URL("../../../crates/vfs/assets/base-filesystem.json", import.meta.url)),
];

// --- agentos base identity (the transform target) -----------------------

const BASE_HOSTNAME = "agentos";
const BASE_USER = "agentos";
const BASE_HOME = `/home/${BASE_USER}`;
const BASE_UID = 1000;
const BASE_GID = 1000;

// Non-Alpine directories the agentos base layer always provides on top of the
// captured snapshot. `/workspace` is the default agent working directory (cwd) and
// the conventional mount root; it is kept separate from `$HOME` (/home/agentos),
// which is never a mount.
const EXTRA_DIRECTORIES = [
	{ path: "/workspace", type: "directory", mode: "755", uid: BASE_UID, gid: BASE_GID },
];

const TRANSFORMS = [
	"Normalize HOSTNAME to agentos",
	"Preserve the captured user-level environment and filesystem layout as the agentos base layer",
	"Add the non-Alpine /workspace directory (default agent working directory) owned by the base user",
];

// --- Alpine snapshot capture (Docker) ---------------------------------------

const DEFAULT_ENV_KEYS = [
	"CHARSET",
	"HOME",
	"HOSTNAME",
	"LANG",
	"LC_COLLATE",
	"LOGNAME",
	"PAGER",
	"PATH",
	"SHELL",
	"USER",
];

const TEXT_FILE_PATHS = [
	"/etc/alpine-release",
	"/etc/group",
	"/etc/hostname",
	"/etc/nsswitch.conf",
	"/etc/passwd",
	"/etc/profile",
	"/etc/shadow",
	"/etc/shells",
	"/usr/lib/os-release",
];

const METADATA_ONLY_FILE_PATHS = ["/usr/bin/env"];

const SYMLINK_PATHS = [
	"/etc/os-release",
	"/var/lock",
	"/var/run",
	"/var/spool/cron/crontabs",
];

function addParentDirectories(paths) {
	for (const entry of [...paths]) {
		let current = path.posix.dirname(entry);
		while (current !== "." && current !== "/") {
			paths.add(current);
			current = path.posix.dirname(current);
		}
	}
}

function runDocker(args, options = {}) {
	return execFileSync("docker", args, {
		encoding: "utf-8",
		stdio: ["ignore", "pipe", "pipe"],
		...options,
	});
}

function dockerExec(containerId, args) {
	return runDocker(["exec", containerId, ...args]);
}

function parseEnv(stdout) {
	const result = {};
	for (const line of stdout.split("\n")) {
		if (!line.trim()) {
			continue;
		}
		const separator = line.indexOf("=");
		if (separator === -1) {
			continue;
		}
		result[line.slice(0, separator)] = line.slice(separator + 1);
	}
	return result;
}

function mapFileType(type) {
	switch (type) {
		case "directory":
			return "directory";
		case "regular file":
			return "file";
		case "symbolic link":
			return "symlink";
		default:
			throw new Error(`Unsupported file type: ${type}`);
	}
}

function shouldIncludePath(path) {
	if (path === "/dev" || path.startsWith("/dev/")) {
		return false;
	}
	if (path === "/proc" || path.startsWith("/proc/")) {
		return false;
	}
	if (path.startsWith("/sys/")) {
		return false;
	}
	if (path === "/etc/mtab") {
		return false;
	}
	return true;
}

function collectPaths(containerId) {
	const discovered = dockerExec(containerId, [
		"sh",
		"-lc",
		"find / -maxdepth 2 -type d | sort",
	]);

	const paths = new Set(
		discovered
			.split("\n")
			.map((path) => path.trim())
			.filter(Boolean)
			.filter(shouldIncludePath),
	);

	for (const path of TEXT_FILE_PATHS) {
		paths.add(path);
	}
	for (const path of METADATA_ONLY_FILE_PATHS) {
		paths.add(path);
	}
	for (const path of SYMLINK_PATHS) {
		paths.add(path);
	}
	paths.add("/");
	addParentDirectories(paths);

	return [...paths].sort((a, b) => a.localeCompare(b));
}

function readEntry(containerId, path) {
	const statOutput = dockerExec(containerId, [
		"sh",
		"-lc",
		`stat -c '%F\t%a\t%u\t%g' '${path}'`,
	]).trim();
	const [rawType, mode, uid, gid] = statOutput.split("\t");
	const entry = {
		path,
		type: mapFileType(rawType),
		mode,
		uid: Number(uid),
		gid: Number(gid),
	};

	if (entry.type === "symlink") {
		entry.target = dockerExec(containerId, ["readlink", path]).trim();
	}

	if (entry.type === "file" && TEXT_FILE_PATHS.includes(path)) {
		entry.content = dockerExec(containerId, ["cat", path]);
	}

	return entry;
}

function extractPrompt(profileContent) {
	const match = profileContent.match(/PS1='([^']+)'/);
	if (!match) {
		throw new Error("Unable to extract PS1 from /etc/profile");
	}
	return match[1];
}

function captureAlpineSnapshot() {
	const containerId = runDocker([
		"run",
		"--detach",
		"--rm",
		DEFAULT_IMAGE,
		"sh",
		"-lc",
		"adduser -D agentos >/dev/null 2>&1 && sleep infinity",
	]).trim();

	try {
		const env = parseEnv(dockerExec(containerId, ["sh", "-lc", "su agentos -c env"]));
		const profileContent = dockerExec(containerId, ["cat", "/etc/profile"]);
		const entries = collectPaths(containerId).map((path) => readEntry(containerId, path));
		return {
			env: Object.fromEntries(
				DEFAULT_ENV_KEYS.filter((key) => env[key] !== undefined).map((key) => [key, env[key]]),
			),
			prompt: extractPrompt(profileContent),
			entries,
		};
	} finally {
		try {
			runDocker(["rm", "--force", containerId]);
		} catch {
			// Container may already be gone.
		}
	}
}

// --- transform: raw Alpine snapshot -> agentos base filesystem ----------

function normalizeEntry(entry) {
	if (entry.path === "/etc/hostname" && entry.type === "file") {
		return { ...entry, content: `${BASE_HOSTNAME}\n` };
	}
	return entry;
}

function withExtraDirectories(entries) {
	const existing = new Set(entries.map((entry) => entry.path));
	return [...entries, ...EXTRA_DIRECTORIES.filter((entry) => !existing.has(entry.path))];
}

function buildBaseFilesystem(snapshot) {
	return {
		source: {
			image: DEFAULT_IMAGE,
			builtAt: new Date().toISOString(),
			transforms: TRANSFORMS,
		},
		environment: {
			env: {
				...snapshot.env,
				HOME: BASE_HOME,
				HOSTNAME: BASE_HOSTNAME,
				LOGNAME: BASE_USER,
				USER: BASE_USER,
			},
			prompt: snapshot.prompt,
		},
		filesystem: {
			entries: withExtraDirectories(snapshot.entries.map(normalizeEntry)),
		},
	};
}

function main() {
	const baseFilesystem = buildBaseFilesystem(captureAlpineSnapshot());
	const json = `${JSON.stringify(baseFilesystem, null, 2)}\n`;
	for (const outputPath of OUTPUT_PATHS) {
		mkdirSync(path.dirname(outputPath), { recursive: true });
		writeFileSync(outputPath, json);
		process.stdout.write(`Wrote ${outputPath}\n`);
	}
}

main();
