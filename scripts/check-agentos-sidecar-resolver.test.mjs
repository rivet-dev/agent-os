import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";
import vm from "node:vm";

const require = createRequire(import.meta.url);

const source = readFileSync(
	new URL("../packages/sidecar/index.js", import.meta.url),
	"utf8",
);

function resolveFor(platform, arch) {
	const sandbox = {
		require,
		module: { exports: {} },
		exports: {},
		process: { platform, arch, env: {} },
		__dirname: "/tmp/agentos-sidecar-resolver-no-local-binary",
	};
	vm.runInNewContext(source, sandbox, {
		filename: "packages/sidecar/index.js",
	});
	try {
		return sandbox.module.exports.getSidecarPath();
	} catch (error) {
		return error.message;
	}
}

test("sidecar resolver recognizes published platform packages", () => {
	assert.match(resolveFor("linux", "x64"), /agentos-sidecar-linux-x64-gnu/);
	assert.match(resolveFor("linux", "arm64"), /agentos-sidecar-linux-arm64-gnu/);
	assert.match(resolveFor("darwin", "x64"), /agentos-sidecar-darwin-x64/);
	assert.match(resolveFor("darwin", "arm64"), /agentos-sidecar-darwin-arm64/);
});

test("sidecar resolver rejects unsupported platforms", () => {
	assert.match(resolveFor("win32", "x64"), /unsupported platform win32\/x64/);
});
