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
	assert(!RUST_CRATES.includes("agentos-vm-browser" as never));
	assert(!RUST_CRATES.includes("agentos-sidecar-core" as never));
	assert(!RUST_CRATES.includes("agentos-build-support" as never));
	assert(!RUST_CRATES.includes("agentos-vm-core" as never));

	assertBefore("agentos-rivetkit-ars-client", "agentos-vm");
	assertBefore("agentos-vm-host-interface", "agentos-executor-v8-runtime");
	assertBefore("agentos-executor-contract", "agentos-executor-v8-runtime");
	assertBefore("agentos-executor-contract", "agentos-executor-wasm-wasmtime");
	assertBefore("agentos-resource-accounting", "agentos-driver-tokio");
	assertBefore("agentos-resource-accounting", "agentos-vm-kernel");
	assertBefore("agentos-resource-accounting", "agentos-executor-v8-runtime");
	assertBefore("agentos-resource-accounting", "agentos-executor-wasm-v8");
	assertBefore("agentos-resource-accounting", "agentos-executor-wasm-wasmtime");
	assertBefore("agentos-executor-wasm-abi", "agentos-executor-wasm-v8");
	assertBefore("agentos-executor-wasm-abi", "agentos-executor-wasm-wasmtime");
	assertBefore("agentos-driver-tokio", "agentos-vm-kernel");
	assertBefore("agentos-driver-tokio", "agentos-executor-v8-runtime");
	assertBefore("agentos-driver-tokio", "agentos-executor-wasm-wasmtime");
	assertBefore("agentos-driver-tokio", "agentos-vm");
	assertBefore("agentos-vfs-core", "agentos-vfs-storage");
	assertBefore("agentos-executor-v8-runtime", "agentos-executor-node-v8");
	assertBefore("agentos-executor-v8-runtime", "agentos-executor-python-v8-pyodide");
	assertBefore("agentos-executor-v8-runtime", "agentos-executor-wasm-v8");
	assertBefore("agentos-sidecar-protocol", "agentos-sidecar-client");
	assertBefore("agentos-executor-node-v8", "agentos-vm");
	assertBefore("agentos-executor-python-v8-pyodide", "agentos-vm");
	assertBefore("agentos-executor-wasm-v8", "agentos-vm");
	assertBefore("agentos-executor-wasm-wasmtime", "agentos-vm");
	assertBefore("agentos-sidecar-client", "agentos-vm");
	assertBefore("agentos-acp-protocol", "agentos-client");
	assertBefore("agentos-client", "agentos-sidecar");
});

test("archived browser crates stay excluded from real publish discovery", () => {
	const repoRoot = join(import.meta.dirname, "../../../..");
	const crates = discoverRustCrates(repoRoot);
	assert(!crates.includes("agentos-sidecar-browser" as never));
	assert(!crates.includes("agentos-vm-browser" as never));
});

test("discovers the publishable Rust crate subset from a workspace", () => {
	withFixture((root) => {
		write(
			root,
			"Cargo.toml",
			[
				"[workspace]",
				"members = [",
				'  "crates/acp-protocol",',
				'  "crates/sidecar",',
				'  "crates/vm",',
				'  "crates/client",',
				"]",
				"",
			].join("\n"),
		);
		for (const [member, name] of [
			["crates/acp-protocol", "agentos-acp-protocol"],
			["crates/sidecar", "agentos-sidecar"],
			["crates/vm", "agentos-vm"],
			["crates/client", "agentos-client"],
		]) {
			write(root, join(member, "Cargo.toml"), `[package]\nname = "${name}"\n`);
		}

		assert.deepEqual(discoverRustCrates(root), [
			"agentos-vm",
			"agentos-acp-protocol",
			"agentos-client",
			"agentos-sidecar",
		]);
	});
});
