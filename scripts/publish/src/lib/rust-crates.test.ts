import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { discoverRustCrates, RUST_CRATES } from "./rust-crates.js";

function withFixture(fn: (root: string) => void) {
	const root = mkdtempSync(join(tmpdir(), "publish-rust-crates-"));
	try {
		fn(root);
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
}

function write(root: string, rel: string, contents: string) {
	const path = join(root, rel);
	mkdirSync(join(path, ".."), { recursive: true });
	writeFileSync(path, contents);
}

function assertBefore(crate: string, dependent: string) {
	const crateIndex = RUST_CRATES.indexOf(crate as (typeof RUST_CRATES)[number]);
	const dependentIndex = RUST_CRATES.indexOf(
		dependent as (typeof RUST_CRATES)[number],
	);

	assert.notEqual(crateIndex, -1, `${crate} is missing from publish order`);
	assert.notEqual(
		dependentIndex,
		-1,
		`${dependent} is missing from publish order`,
	);
	assert(
		crateIndex < dependentIndex,
		`${crate} must publish before ${dependent}`,
	);
}

test("Rust crate publish order satisfies internal dependencies", () => {
	assert.equal(new Set(RUST_CRATES).size, RUST_CRATES.length);
	assert(!RUST_CRATES.includes("agentos-sidecar-browser" as never));
	assert(!RUST_CRATES.includes("agentos-native-sidecar-browser" as never));
	assert(!RUST_CRATES.includes("agentos-sidecar-core" as never));

	assertBefore("agentos-build-support", "agentos-v8-runtime");
	assertBefore("agentos-actor-uds-client", "agentos-native-sidecar");
	assertBefore("agentos-bridge", "agentos-v8-runtime");
	assertBefore("agentos-executor-contract", "agentos-v8-runtime");
	assertBefore("agentos-executor-contract", "agentos-executor-wasm-wasmtime");
	assertBefore("agentos-wasm-common", "agentos-executor-wasm-v8");
	assertBefore("agentos-wasm-common", "agentos-executor-wasm-wasmtime");
	assertBefore("agentos-runtime-tokio", "agentos-kernel");
	assertBefore("agentos-runtime-tokio", "agentos-v8-runtime");
	assertBefore("agentos-runtime-tokio", "agentos-executor-wasm-wasmtime");
	assertBefore("agentos-runtime-tokio", "agentos-native-sidecar");
	assertBefore("agentos-vfs-core", "agentos-vfs");
	assertBefore("agentos-v8-runtime", "agentos-executor-node-v8");
	assertBefore("agentos-v8-runtime", "agentos-executor-python-v8-pyodide");
	assertBefore("agentos-v8-runtime", "agentos-executor-wasm-v8");
	assertBefore("agentos-sidecar-protocol", "agentos-sidecar-client");
	assertBefore("agentos-executor-node-v8", "agentos-native-sidecar");
	assertBefore("agentos-executor-python-v8-pyodide", "agentos-native-sidecar");
	assertBefore("agentos-executor-wasm-v8", "agentos-native-sidecar");
	assertBefore("agentos-executor-wasm-wasmtime", "agentos-native-sidecar");
	assertBefore("agentos-native-sidecar-core", "agentos-native-sidecar");
	assertBefore("agentos-sidecar-client", "agentos-native-sidecar");
	assertBefore("agentos-protocol", "agentos-client");
	assertBefore("agentos-client", "agentos-sidecar");
});

test("browser migration crates stay excluded from real publish discovery", () => {
	const repoRoot = join(import.meta.dirname, "../../../..");
	const crates = discoverRustCrates(repoRoot);
	assert(!crates.includes("agentos-sidecar-browser" as never));
	assert(!crates.includes("agentos-native-sidecar-browser" as never));
});

test("discovers the publishable Rust crate subset from a workspace", () => {
	withFixture((root) => {
		write(
			root,
			"Cargo.toml",
			[
				"[workspace]",
				"members = [",
				'  "crates/agentos-protocol",',
				'  "crates/agentos-sidecar",',
				'  "crates/native-sidecar",',
				'  "crates/client",',
				"]",
				"",
			].join("\n"),
		);
		for (const [member, name] of [
			["crates/agentos-protocol", "agentos-protocol"],
			["crates/agentos-sidecar", "agentos-sidecar"],
			["crates/native-sidecar", "agentos-native-sidecar"],
			["crates/client", "agentos-client"],
		]) {
			write(root, join(member, "Cargo.toml"), `[package]\nname = "${name}"\n`);
		}

		assert.deepEqual(discoverRustCrates(root), [
			"agentos-native-sidecar",
			"agentos-protocol",
			"agentos-client",
			"agentos-sidecar",
		]);
	});
});
