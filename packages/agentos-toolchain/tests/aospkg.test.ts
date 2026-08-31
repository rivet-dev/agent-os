import { execFileSync } from "node:child_process";
import {
	chmodSync,
	mkdirSync,
	mkdtempSync,
	readFileSync,
	rmSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, test } from "vitest";
import {
	decodeAospkgManifest,
	packAospkgFromTarBytes,
	verifyAospkgCommands,
} from "../src/aospkg.js";

const dirs: string[] = [];

afterEach(() => {
	for (const dir of dirs.splice(0))
		rmSync(dir, { recursive: true, force: true });
});

function sourceTar(mode = 0o755): Buffer {
	const dir = mkdtempSync(join(tmpdir(), "agentos-aospkg-runtime-"));
	dirs.push(dir);
	mkdirSync(join(dir, "bin"));
	writeFileSync(join(dir, "bin", "agent"), "#!/usr/bin/env node\n");
	chmodSync(join(dir, "bin", "agent"), mode);
	writeFileSync(
		join(dir, "agentos-package.json"),
		JSON.stringify({
			name: "runtime-fixture",
			version: "1.0.0",
			agent: {
				acpEntrypoint: "agent",
			},
		}),
	);
	const tar = join(dir, "source.tar");
	execFileSync("tar", ["-cf", tar, "-C", dir, "agentos-package.json", "bin"]);
	return readFileSync(tar);
}

describe("package manifest", () => {
	test("packs and decodes v1 agent metadata", () => {
		const { bytes } = packAospkgFromTarBytes(sourceTar());
		const manifest = decodeAospkgManifest(bytes);
		expect(bytes.readUInt16LE(16)).toBe(1);
		expect(manifest.agent?.acpEntrypoint).toBe("agent");
	});

	test("decodes the manifest without requiring later container chunks", () => {
		const { bytes } = packAospkgFromTarBytes(sourceTar());
		const manifestEnd = 16 + bytes.readUInt32LE(8);
		expect(
			decodeAospkgManifest(bytes.subarray(0, manifestEnd)).agent?.acpEntrypoint,
		).toBe("agent");
	});

	test("verifies the complete executable command contract", () => {
		const { bytes } = packAospkgFromTarBytes(sourceTar());
		expect(verifyAospkgCommands(bytes, ["agent"]).commands).toHaveLength(1);
		expect(() => verifyAospkgCommands(bytes, ["agent", "missing"])).toThrow(
			/expected=agent,missing actual=agent/,
		);
	});

	test("rejects packed command targets without an executable mode bit", () => {
		const { bytes } = packAospkgFromTarBytes(sourceTar(0o644));
		expect(() => verifyAospkgCommands(bytes, ["agent"])).toThrow(
			/non-executable entry \/bin\/agent/,
		);
	});
});
