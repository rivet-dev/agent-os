import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, test } from "vitest";
import { resolvePublishedSidecarBinary } from "../src/binary.js";

const ORIGINAL_AGENTOS_OVERRIDE = process.env.AGENTOS_SIDECAR_BIN;
const ORIGINAL_NATIVE_OVERRIDE = process.env.AGENTOS_NATIVE_SIDECAR_BIN;

afterEach(() => {
	if (ORIGINAL_AGENTOS_OVERRIDE === undefined) {
		delete process.env.AGENTOS_SIDECAR_BIN;
	} else {
		process.env.AGENTOS_SIDECAR_BIN = ORIGINAL_AGENTOS_OVERRIDE;
	}
	if (ORIGINAL_NATIVE_OVERRIDE === undefined) {
		delete process.env.AGENTOS_NATIVE_SIDECAR_BIN;
	} else {
		process.env.AGENTOS_NATIVE_SIDECAR_BIN = ORIGINAL_NATIVE_OVERRIDE;
	}
});

describe("AgentOS runtime sidecar binary resolution", () => {
	test("prefers the native override over the generic override", () => {
		const root = mkdtempSync(join(tmpdir(), "agentos-native-sidecar-bin-"));
		try {
			const nativePath = join(root, "agentos-native-sidecar");
			const genericPath = join(root, "agentos-sidecar");
			writeFileSync(nativePath, "#!/bin/sh\n", { mode: 0o755 });
			writeFileSync(genericPath, "#!/bin/sh\n", { mode: 0o755 });
			process.env.AGENTOS_NATIVE_SIDECAR_BIN = nativePath;
			process.env.AGENTOS_SIDECAR_BIN = genericPath;

			expect(resolvePublishedSidecarBinary()).toBe(nativePath);
		} finally {
			rmSync(root, { recursive: true, force: true });
		}
	});

	test("honors AGENTOS_SIDECAR_BIN when the file exists", () => {
		const root = mkdtempSync(join(tmpdir(), "agentos-native-sidecar-bin-"));
		try {
			delete process.env.AGENTOS_NATIVE_SIDECAR_BIN;
			const binaryPath = join(root, "agentos-native-sidecar");
			writeFileSync(binaryPath, "#!/bin/sh\n", { mode: 0o755 });
			process.env.AGENTOS_SIDECAR_BIN = binaryPath;

			expect(resolvePublishedSidecarBinary()).toBe(binaryPath);
		} finally {
			rmSync(root, { recursive: true, force: true });
		}
	});

	test("rejects a missing AGENTOS_SIDECAR_BIN override", () => {
		delete process.env.AGENTOS_NATIVE_SIDECAR_BIN;
		const binaryPath = join(
			tmpdir(),
			`agentos-native-sidecar-missing-${process.pid}-${Date.now()}`,
		);
		if (existsSync(binaryPath)) {
			rmSync(binaryPath, { force: true });
		}
		process.env.AGENTOS_SIDECAR_BIN = binaryPath;

		expect(() => resolvePublishedSidecarBinary()).toThrow(
			/native sidecar override is set to .* but the file does not exist/,
		);
	});

	test("delegates to the AgentOS resolver package when no override is set", () => {
		delete process.env.AGENTOS_NATIVE_SIDECAR_BIN;
		delete process.env.AGENTOS_SIDECAR_BIN;

		try {
			expect(resolvePublishedSidecarBinary()).toMatch(/agentos-native-sidecar/);
		} catch (error) {
			expect((error as Error).message).toMatch(
				/@rivet-dev\/agentos-runtime-sidecar: platform package .* is not installed/,
			);
		}
	});
});
