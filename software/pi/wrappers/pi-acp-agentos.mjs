#!/usr/bin/env node

import { spawn } from "node:child_process";
import { rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { createInterface } from "node:readline";

import { acpRequestCwd, normalizeAcpResponse } from "./acp-errors.mjs";

const upstreamEntrypoint = new URL("./pi-acp/index.js", import.meta.url).pathname;
const cwdPath = resolve(tmpdir(), `agentos-pi-cwd-${process.pid}`);
const child = spawn(process.execPath, [upstreamEntrypoint, ...process.argv.slice(2)], {
	env: { ...process.env, AGENTOS_PI_CWD_FILE: cwdPath },
	stdio: ["pipe", "pipe", "inherit"],
});

let inputBuffer = "";
let sessionCwd = "";
let finished = false;
function writeInput(value) {
	if (child.stdin.write(value) || typeof process.stdin.pause !== "function") return;
	process.stdin.pause();
	child.stdin.once("drain", () => process.stdin.resume?.());
}
function forwardInput(line, newline = true) {
	const cwd = acpRequestCwd(line);
	if (cwd && !sessionCwd) {
		sessionCwd = cwd;
		writeFileSync(cwdPath, `${cwd}\n`, { mode: 0o600 });
	} else if (cwd && cwd !== sessionCwd) {
		throw new Error(`ACP adapter cannot change cwd from ${sessionCwd} to ${cwd}`);
	}
	writeInput(`${line}${newline ? "\n" : ""}`);
}
process.stdin.on("data", (chunk) => {
	if (sessionCwd) {
		writeInput(chunk);
		return;
	}
	inputBuffer += chunk.toString();
	for (;;) {
		const newline = inputBuffer.indexOf("\n");
		if (newline < 0) return;
		const line = inputBuffer.slice(0, newline);
		inputBuffer = inputBuffer.slice(newline + 1);
		forwardInput(line);
		if (!sessionCwd) continue;
		if (inputBuffer) writeInput(inputBuffer);
		inputBuffer = "";
		return;
	}
});
process.stdin.on("end", () => {
	if (inputBuffer) forwardInput(inputBuffer, false);
	child.stdin.end();
});
const output = createInterface({ input: child.stdout, crlfDelay: Infinity });
output.on("line", (line) => process.stdout.write(`${normalizeAcpResponse(line)}\n`));

for (const signal of ["SIGINT", "SIGTERM"]) {
	process.once(signal, () => {
		child.kill(signal);
		setTimeout(() => child.kill("SIGKILL"), 2_000).unref?.();
	});
}
child.once("error", (error) => {
	process.stderr.write(`${error.stack ?? error}\n`);
	finish(1);
});
child.once("close", (code, signal) => {
	finish(code ?? 1, signal);
});

function finish(code, signal) {
	if (finished) return;
	finished = true;
	process.stdin.removeAllListeners?.("data");
	process.stdin.removeAllListeners?.("end");
	output.close();
	rmSync(cwdPath, { force: true });
	if (signal) process.kill(process.pid, signal);
	else process.exit(code);
}
