#!/usr/bin/env node

import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const DEFAULT_INPUT = fileURLToPath(
	new URL("../fixtures/alpine-defaults.json", import.meta.url),
);
const DEFAULT_OUTPUT = fileURLToPath(
	new URL("../fixtures/base-filesystem.json", import.meta.url),
);

const BASE_HOSTNAME = "agent-os";
const BASE_USER = "user";
const BASE_HOME = `/home/${BASE_USER}`;

function readJson(pathname) {
	return JSON.parse(readFileSync(pathname, "utf-8"));
}

function normalizeEntry(entry) {
	if (entry.path === "/etc/hostname" && entry.type === "file") {
		return {
			...entry,
			content: `${BASE_HOSTNAME}\n`,
		};
	}

	return entry;
}

function buildBaseFilesystem(snapshot, inputPath) {
	return {
		source: {
			snapshotPath: path.basename(inputPath),
			image: snapshot.image,
			snapshotCreatedAt: snapshot.createdAt,
			builtAt: new Date().toISOString(),
			transforms: [
				"Normalize HOSTNAME to agent-os",
				"Preserve the captured user-level environment and filesystem layout as the AgentOs base layer",
			],
		},
		environment: {
			env: {
				...snapshot.environment.env,
				HOME: BASE_HOME,
				HOSTNAME: BASE_HOSTNAME,
				LOGNAME: BASE_USER,
				USER: BASE_USER,
			},
			prompt: snapshot.environment.prompt,
		},
		filesystem: {
			entries: snapshot.filesystem.entries.map(normalizeEntry),
		},
	};
}

function main() {
	const [inputPath = DEFAULT_INPUT, outputPath = DEFAULT_OUTPUT] = process.argv.slice(2);
	const snapshot = readJson(inputPath);
	const baseFilesystem = buildBaseFilesystem(snapshot, inputPath);

	mkdirSync(path.dirname(outputPath), { recursive: true });
	writeFileSync(outputPath, `${JSON.stringify(baseFilesystem, null, 2)}\n`);
	process.stdout.write(`Wrote ${outputPath}\n`);
}

main();
