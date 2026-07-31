#!/usr/bin/env node

import { spawn } from "node:child_process";
import { createInterface } from "node:readline";

import { normalizeAcpResponse } from "./acp-errors.mjs";

const upstreamEntrypoint = new URL("./pi-acp/index.js", import.meta.url).pathname;
const child = spawn(process.execPath, [upstreamEntrypoint, ...process.argv.slice(2)], {
	env: process.env,
	stdio: ["pipe", "pipe", "inherit"],
});

process.stdin.on("data", (chunk) => child.stdin.write(chunk));
process.stdin.on("end", () => child.stdin.end());
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
	if (signal) process.kill(process.pid, signal);
	else process.exitCode = code ?? 1;
});
