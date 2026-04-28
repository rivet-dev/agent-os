import assert from "node:assert/strict";
import { once } from "node:events";
import {
	chmodSync,
	mkdtempSync,
	readFileSync,
	rmSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { spawnCodexExecChild } from "../dist/adapter.js";

function writeFixtureExecutable(dir) {
	const fixturePath = join(dir, "fake-codex-exec.mjs");
	writeFileSync(
		fixturePath,
		[
			"#!/usr/bin/env node",
			"import { writeFileSync } from 'node:fs';",
			"import { join } from 'node:path';",
			"writeFileSync(join(process.cwd(), 'child-env.json'), JSON.stringify(process.env, null, 2));",
			"process.stdout.write(JSON.stringify({ type: 'done', stop_reason: 'end_turn', assistant_text: '', history: [] }) + '\\n');",
		].join("\n"),
	);
	chmodSync(fixturePath, 0o755);
	return fixturePath;
}

async function captureChildEnv(env) {
	const cwd = mkdtempSync(join(tmpdir(), "codex-adapter-env-"));
	try {
		const execCommand = writeFixtureExecutable(cwd);
		const child = spawnCodexExecChild({ cwd, env, execCommand });
		const [code, signal] = await once(child, "close");
		assert.equal(code, 0);
		assert.equal(signal, null);
		return JSON.parse(readFileSync(join(cwd, "child-env.json"), "utf8"));
	} finally {
		rmSync(cwd, { force: true, recursive: true });
	}
}

test("spawnCodexExecChild strips AGENT_OS and NODE_SYNC_RPC env keys", async () => {
	const childEnv = await captureChildEnv({
		AGENT_OS_KEEP_STDIN_OPEN: "1",
		AGENT_OS_SECRET: "hidden",
		HOME: "/tmp/codex-home",
		NODE_SYNC_RPC_TOKEN: "sync-rpc-secret",
		OPENAI_API_KEY: "sk-test",
		PATH: process.env.PATH ?? "",
		TERM: "xterm-256color",
		VISIBLE_MARKER: "should-not-pass",
		XDG_CONFIG_HOME: "/tmp/codex-config",
	});

	assert.equal(childEnv.OPENAI_API_KEY, "sk-test");
	assert.equal(childEnv.HOME, "/tmp/codex-home");
	assert.equal(childEnv.TERM, "xterm-256color");
	assert.equal(childEnv.XDG_CONFIG_HOME, "/tmp/codex-config");
	assert.ok(!("AGENT_OS_KEEP_STDIN_OPEN" in childEnv));
	assert.ok(!("AGENT_OS_SECRET" in childEnv));
	assert.ok(!("NODE_SYNC_RPC_TOKEN" in childEnv));
	assert.ok(!("VISIBLE_MARKER" in childEnv));
});

test("spawnCodexExecChild strips loader injection env vars", async () => {
	const childEnv = await captureChildEnv({
		DYLD_INSERT_LIBRARIES: "/tmp/libinject.dylib",
		HOME: "/tmp/codex-home",
		LD_PRELOAD: "/tmp/libinject.so",
		NODE_OPTIONS: "--require /tmp/evil.js",
		OPENAI_BASE_URL: "https://example.invalid/v1",
		PATH: process.env.PATH ?? "",
	});

	assert.equal(childEnv.OPENAI_BASE_URL, "https://example.invalid/v1");
	assert.ok(!("DYLD_INSERT_LIBRARIES" in childEnv));
	assert.ok(!("LD_PRELOAD" in childEnv));
	assert.ok(!("NODE_OPTIONS" in childEnv));
});
