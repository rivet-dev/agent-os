#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const DEFAULT_IMAGE = process.env.ALPINE_IMAGE ?? "alpine:3.22";
const DEFAULT_OUTPUT = fileURLToPath(
	new URL("../fixtures/alpine-defaults.json", import.meta.url),
);

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

const METADATA_ONLY_FILE_PATHS = [
	"/usr/bin/env",
];

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

		const key = line.slice(0, separator);
		const value = line.slice(separator + 1);
		result[key] = value;
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

function main() {
	const outputPath = process.argv[2] ?? DEFAULT_OUTPUT;
		const containerId = runDocker([
		"run",
		"--detach",
		"--rm",
		DEFAULT_IMAGE,
		"sh",
		"-lc",
		"adduser -D user >/dev/null 2>&1 && sleep infinity",
	]).trim();

	try {
		const rawEnv = parseEnv(
			dockerExec(containerId, ["sh", "-lc", "su user -c env"]),
		);
		const env = Object.fromEntries(
			DEFAULT_ENV_KEYS
				.filter((key) => rawEnv[key] !== undefined)
				.map((key) => [key, rawEnv[key]]),
		);

		const profileContent = dockerExec(containerId, ["cat", "/etc/profile"]);
		const entries = collectPaths(containerId).map((path) =>
			readEntry(containerId, path),
		);

		const snapshot = {
			image: DEFAULT_IMAGE,
			createdAt: new Date().toISOString(),
			environment: {
				env,
				prompt: extractPrompt(profileContent),
			},
			filesystem: {
				entries,
			},
		};

		mkdirSync(path.dirname(outputPath), { recursive: true });
		writeFileSync(outputPath, `${JSON.stringify(snapshot, null, 2)}\n`);
		process.stdout.write(`Wrote ${outputPath}\n`);
	} finally {
		try {
			runDocker(["rm", "--force", containerId]);
		} catch {
			// Container may already be gone.
		}
	}
}

main();
