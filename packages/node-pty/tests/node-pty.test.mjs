import assert from "node:assert/strict";
import test from "node:test";

import { spawn } from "../dist/index.js";

test("exposes the node-pty lifecycle surface", async () => {
	const pty = spawn(
		process.execPath,
		[
			"-e",
			"process.stdin.once('data', d => process.stdout.write(d, () => process.exit(0)))",
		],
		{ cols: 90, rows: 30 },
	);
	assert.equal(pty.cols, 90);
	assert.equal(pty.rows, 30);
	assert.ok(pty.pid > 0);

	let output = "";
	const exited = new Promise((resolve) => pty.onExit(resolve));
	const disposable = pty.onData((data) => {
		output += data;
	});
	pty.write("hello\n");
	const result = await exited;
	disposable.dispose();

	assert.equal(result.exitCode, 0);
	assert.match(output, /hello/);
});

test("validates dimensions and listener bounds", () => {
	assert.throws(() => spawn(process.execPath, [], { cols: 0 }), { code: "EINVAL" });
	process.env.AGENTOS_NODE_PTY_MAX_LISTENERS = "1";
	const pty = spawn(process.execPath, ["-e", "setTimeout(() => {}, 1000)"]);
	pty.onData(() => {});
	assert.throws(() => pty.onData(() => {}), {
		code: "ERR_AGENTOS_NODE_PTY_LISTENER_LIMIT",
	});
	pty.kill();
	delete process.env.AGENTOS_NODE_PTY_MAX_LISTENERS;
});
