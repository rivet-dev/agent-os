#!/usr/bin/env node

import { chmodSync, copyFileSync, existsSync, mkdirSync } from "node:fs";
import { resolve as resolvePath } from "node:path";

const source = resolvePath(
	import.meta.dirname,
	"..",
	"..",
	"..",
	"software",
	"codex",
	"wasm",
	"codex",
);
const execSource = resolvePath(
	import.meta.dirname,
	"..",
	"..",
	"..",
	"software",
	"codex",
	"wasm",
	"codex-exec",
);
const dist = resolvePath(import.meta.dirname, "..", "dist");
const target = resolvePath(dist, "codex.wasm");
const execTarget = resolvePath(dist, "codex-exec.wasm");

if (!existsSync(source) || !existsSync(execSource)) {
	throw new Error(
		"Codex WASI artifacts are missing; build the pinned rivet-dev/codex fork first",
	);
}

mkdirSync(dist, { recursive: true });
copyFileSync(source, target);
copyFileSync(execSource, execTarget);
chmodSync(target, 0o755);
chmodSync(execTarget, 0o755);
process.stdout.write(`Staged pinned Codex session-turn WASM from ${source}\n`);
