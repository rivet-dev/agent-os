import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, test } from "vitest";
import { NodeRuntime } from "../src/index.js";
import { createInMemoryFileSystem } from "../src/test-runtime.js";

describe("NodeRuntime execCommand output capture", () => {
	test(
		"captures complete stdout when a fast process exits immediately",
		async () => {
			const commandDir = await mkdtemp(
				join(tmpdir(), "agentos-node-output-commands-"),
			);
			// This case exercises the V8-backed `node` command, but NodeRuntime
			// also requires a shell runtime descriptor. Keep the test independent
			// of generated registry artifacts by supplying a valid no-op `_start`.
			await writeFile(
				join(commandDir, "sh"),
				Buffer.from(
					"0061736d0100000001040160000003020100070a01065f737461727400000a040102000b",
					"hex",
				),
			);
			let runtime: NodeRuntime | undefined;
			try {
				runtime = await NodeRuntime.create({
					filesystem: createInMemoryFileSystem(),
					commandsDir: commandDir,
				});
				const expected = "x".repeat(64 * 1024);
				const script = [
					'const fs = require("node:fs");',
					"const chunk = Buffer.alloc(4096, 120);",
					"for (let i = 0; i < 16; i += 1) fs.writeSync(1, chunk);",
					"process.exit(0);",
				].join(" ");

				for (let i = 0; i < 10; i += 1) {
					const result = await runtime.execCommand("node", ["-e", script]);

					expect(result.exitCode).toBe(0);
					expect(result.stdout).toBe(expected);
					expect(result.stderr).toBe("");
				}
			} finally {
				await runtime?.dispose();
				await rm(commandDir, { force: true, recursive: true });
			}
		},
		120_000,
	);
});
