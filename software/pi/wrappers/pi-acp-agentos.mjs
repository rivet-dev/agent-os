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
function forwardInput(line, newline = true) {
	const cwd = acpRequestCwd(line);
	if (cwd) writeFileSync(cwdPath, `${cwd}\n`, { mode: 0o600 });
	child.stdin.write(`${line}${newline ? "\n" : ""}`);
}
process.stdin.on("data", (chunk) => {
	inputBuffer += chunk.toString();
	const lines = inputBuffer.split("\n");
	inputBuffer = lines.pop() ?? "";
	for (const line of lines) forwardInput(line);
});
process.stdin.on("end", () => {
	if (inputBuffer) forwardInput(inputBuffer, false);
	child.stdin.end();
});
const output = createInterface({ input: child.stdout, crlfDelay: Infinity });
output.on("line", (line) => process.stdout.write(`${normalizeAcpResponse(line)}\n`));

for (const signal of ["SIGINT", "SIGTERM"]) {
	process.once(signal, () => child.kill(signal));
}
child.once("error", (error) => {
	process.stderr.write(`${error.stack ?? error}\n`);
	process.exitCode = 1;
});
child.once("exit", (code, signal) => {
	rmSync(cwdPath, { force: true });
	if (signal) process.kill(process.pid, signal);
	else process.exitCode = code ?? 1;
});
