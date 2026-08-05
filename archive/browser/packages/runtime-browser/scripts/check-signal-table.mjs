#!/usr/bin/env node
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const packageRoot = path.resolve(scriptDir, "..");
const repoRoot = path.resolve(packageRoot, "..", "..");

const rustSource = readFileSync(
	path.join(repoRoot, "crates", "native-sidecar-core", "src", "signals.rs"),
	"utf8",
);
const runtimeSource = readFileSync(
	path.join(packageRoot, "src", "runtime.ts"),
	"utf8",
);
const runtimeDriverSource = readFileSync(
	path.join(packageRoot, "src", "runtime-driver.ts"),
	"utf8",
);
const signalSource = readFileSync(
	path.join(packageRoot, "src", "signals.ts"),
	"utf8",
);

const rustSignals = parseRustSignalTable(rustSource);
const browserSignalTables = [
	{
		label: "browser signal module",
		table: parseBrowserSignalModule(signalSource),
	},
];
rustSignals.delete("0");

const errors = [];
for (const { label, table } of browserSignalTables) {
	for (const [name, value] of sortedEntries(rustSignals)) {
		if (table.get(name) !== value) {
			errors.push(
				`${name} is ${table.get(name) ?? "missing"} in ${label} but ${value} in native-sidecar-core`,
			);
		}
	}
	for (const [name] of sortedEntries(table)) {
		if (!rustSignals.has(name)) {
			errors.push(`${name} exists in ${label} but not in native-sidecar-core`);
		}
	}
}

if (!signalSource.includes("return signal > 0 ? 128 + signal : null;")) {
	errors.push(
		"browser defaultSignalExitCode must remain the shared POSIX 128 + signal rule",
	);
}
if (
	!runtimeSource.includes('import { PROCESS_SIGNAL_NUMBERS } from "./signals.js";') ||
	!runtimeSource.includes("JSON.stringify(PROCESS_SIGNAL_NUMBERS)")
) {
	errors.push(
		"runtime child_process polyfill must consume PROCESS_SIGNAL_NUMBERS from src/signals.ts",
	);
}
if (/const signalNumbers = \{[\s\S]*?\n\s*};/.test(runtimeSource)) {
	errors.push("runtime child_process polyfill must not declare a local signal table");
}
if (
	!runtimeDriverSource.includes("parseProcessSignalStateArgs") ||
	!runtimeDriverSource.includes("applyProcessSignalStateUpdate")
) {
	errors.push(
		"runtime driver must consume process signal-state helpers from src/signals.ts",
	);
}
if (/function parseProcessSignalStateArgs/.test(runtimeDriverSource)) {
	errors.push("runtime driver must not declare a local signal-state parser");
}
if (/function applyProcessSignalStateUpdate/.test(runtimeDriverSource)) {
	errors.push("runtime driver must not declare a local signal-state updater");
}

if (errors.length > 0) {
	console.error("Browser signal table drift detected:");
	for (const error of errors) {
		console.error(`  - ${error}`);
	}
	process.exit(1);
}

function parseRustSignalTable(source) {
	const match = source.match(/pub fn signal_number_from_name[\s\S]*?match signal \{([\s\S]*?)\n\s*}/);
	if (!match) {
		throw new Error("Unable to find native-sidecar-core signal_number_from_name table");
	}
	const table = new Map();
	for (const entry of match[1].matchAll(/((?:"[^"]+"\s*(?:\|\s*)?)+)\s*=>\s*Some\((\d+)\)/g)) {
		const value = Number(entry[2]);
		for (const name of entry[1].matchAll(/"([^"]+)"/g)) {
			table.set(name[1], value);
		}
	}
	return table;
}

function parseBrowserSignalModule(source) {
	const match = source.match(/const PROCESS_SIGNAL_NUMBERS: Record<string, number> = \{([\s\S]*?)\n};/);
	if (!match) {
		throw new Error("Unable to find browser PROCESS_SIGNAL_NUMBERS table");
	}
	const table = new Map();
	for (const entry of match[1].matchAll(/\bSIG([A-Z0-9]+):\s*(\d+),/g)) {
		table.set(entry[1], Number(entry[2]));
	}
	return table;
}

function sortedEntries(map) {
	return [...map.entries()].sort(([left], [right]) => left.localeCompare(right));
}
