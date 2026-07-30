import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, test } from "vitest";

// Regression guard for #11 and #59.
//
// Both issues were caused by the removed legacy V8 runtime.js
// (a stdout/socket-path handshake plus a TDZ bug) together with a host-side
// `node:v8.deserialize()` decode of `run()` results. That host-side V8
// serialization made result decoding sensitive to the host Node version /
// V8 engine build (#11) and crashed on constrained hosts such as Lambda
// cold-starts (#59).
//
// The current architecture runs guest code in a process-isolated sidecar
// binary and decodes `run()` results as plain JSON read back over the VFS
// (`packages/core/src/node-runtime.ts`), with NO host-side `node:v8`. These
// tests lock in that root-cause fix at the source level (so they run in a
// plain Node-host CI without booting a sidecar) and verify the structured
// JSON-over-VFS decode behaves correctly for nested values.

const nodeRuntimeSource = readFileSync(
	fileURLToPath(new URL("../src/node-runtime.ts", import.meta.url)),
	"utf8",
);

describe("node-runtime run() result transport (#11, #59)", () => {
	test("the host transport never uses node:v8 deserialize", () => {
		// The whole point of the fix: no host-side V8 serialization in the
		// run()/decode path, which is what made #11 host-engine-sensitive and
		// crashed Lambda cold-starts in #59.
		expect(nodeRuntimeSource).not.toContain("node:v8");
		expect(nodeRuntimeSource).not.toContain("v8.deserialize");
		expect(nodeRuntimeSource).not.toContain("deserialize(");
	});

	test("run() decodes structured results as JSON read back over the VFS", () => {
		// The decode must go through a VFS result file plus JSON.parse over a
		// TextDecoder, not a host-engine-specific binary deserializer.
		expect(nodeRuntimeSource).toContain("this.kernel.readFile(resultPath)");
		expect(nodeRuntimeSource).toMatch(
			/JSON\.parse\(\s*new TextDecoder\(\)\.decode\(bytes\)/,
		);
		// And the guest writes its return value as JSON to that same VFS file.
		expect(nodeRuntimeSource).toContain(
			"JSON.stringify(value === undefined ? null : value)",
		);
	});

	test("structured nested values round-trip through the JSON-over-VFS path", () => {
		// Model the exact host/guest decode used by run(): the guest writes
		// JSON.stringify(value) into a VFS file; the host reads the bytes back
		// and decodes them with JSON.parse(new TextDecoder().decode(bytes)).
		// A nested object with numbers must survive intact, with no dependency
		// on host V8 structured-clone serialization.
		const value = {
			ok: true,
			count: 3,
			ratio: 1.5,
			nested: { items: [1, 2, 3], label: "x", deep: { n: 42 } },
		};

		// Guest side: __return() writes JSON.stringify(value) to the result file.
		const guestBytes = new TextEncoder().encode(JSON.stringify(value));

		// Host side: node-runtime reads the file bytes and JSON.parses them.
		const decoded = JSON.parse(new TextDecoder().decode(guestBytes));

		expect(decoded).toEqual(value);
		expect(decoded.nested.items).toEqual([1, 2, 3]);
		expect(decoded.nested.deep.n).toBe(42);
	});
});
