import assert from "node:assert/strict";
import test from "node:test";
import { resolve } from "node:path";
import { checkRustPackageMetadata } from "./check-rust-package-metadata.mjs";

const root = resolve(import.meta.dirname, "..");

function pkg(name, manifestPath, targets, overrides = {}) {
	return {
		name,
		manifest_path: resolve(root, manifestPath),
		publish: null,
		license: "Apache-2.0",
		repository: "https://github.com/rivet-dev/agent-os",
		description: `${name} description`,
		targets,
		...overrides,
	};
}

const validMetadata = {
	packages: [
		pkg("agentos-protocol", "crates/agentos-protocol/Cargo.toml", [
			{ kind: ["lib"], name: "agentos_protocol" },
		]),
		pkg("agentos-sidecar", "crates/agentos-sidecar/Cargo.toml", [
			{ kind: ["lib"], name: "agentos_sidecar_wrapper" },
			{ kind: ["bin"], name: "agentos-sidecar" },
		]),
		pkg("agentos-client", "crates/client/Cargo.toml", [
			{ kind: ["lib"], name: "agentos_client" },
		]),
	],
};

test("accepts expected Rust package metadata", () => {
	assert.deepEqual(checkRustPackageMetadata({ root, metadata: validMetadata }), []);
});

test("rejects noncanonical agentos-client lib target names", () => {
	const metadata = structuredClone(validMetadata);
	const client = metadata.packages.find((item) => item.name === "agentos-client");
	client.targets[0].name = "agent_os_client";

	assert.deepEqual(checkRustPackageMetadata({ root, metadata }), [
		"agentos-client must expose a lib target named agentos_client",
	]);
});

test("rejects non-publishable required Rust packages", () => {
	const metadata = structuredClone(validMetadata);
	const client = metadata.packages.find((item) => item.name === "agentos-client");
	client.publish = false;

	assert.deepEqual(checkRustPackageMetadata({ root, metadata }), [
		"agentos-client must remain publishable",
	]);
});
