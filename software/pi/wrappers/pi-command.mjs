#!/usr/bin/env node

import { spawn } from "node:child_process";

if (process.argv.length === 3 && process.argv[2] === "--version") {
	process.stdout.write("0.83.0\n");
	process.exit(0);
}

const upstreamEntrypoint = new URL(
	"../node_modules/@earendil-works/pi-coding-agent/dist/cli.js",
	import.meta.url,
).pathname;
const child = spawn(process.execPath, [upstreamEntrypoint, ...process.argv.slice(2)], {
	env: process.env,
	stdio: "inherit",
});
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
