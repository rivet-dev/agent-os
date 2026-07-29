import { expect, test } from "@playwright/test";
import { readFileSync } from "node:fs";
import {
	createRuntime,
	debugPendingExec,
	disposeAllRuntimes,
	execRuntime,
	getLastStdioMessage,
	openHarnessPage,
	signalPendingExec,
	terminatePendingExec,
} from "./harness.js";

const cryptoBasicFixture = JSON.parse(
	readFileSync(
		new URL(
			"../../../../tests/fixtures/crypto-basic-conformance.json",
			import.meta.url,
		),
		"utf8",
	),
);

test.beforeEach(async ({ page }) => {
	await openHarnessPage(page);
});

test.afterEach(async ({ page }) => {
	await disposeAllRuntimes(page);
});

test("preserves sync filesystem and module loading parity in a real Chromium worker", async ({
	page,
}) => {
	const { runtimeId, workerUrl, crossOriginIsolated } =
		await createRuntime(page);

	expect(crossOriginIsolated).toBe(true);
	expect(workerUrl).toContain("/agentos-worker.js");

	const filesystemRoundTrip = await execRuntime(
		page,
		runtimeId,
		`
			const fs = require("fs");
			fs.mkdirSync("/workspace");
			fs.writeFileSync("/workspace/hello.txt", "hello");
			fs.writeFileSync("/workspace/helper.js", "module.exports = { value: 42 };");
			const text = fs.readFileSync("/workspace/hello.txt", "utf8");
			const stat = fs.statSync("/workspace/hello.txt");
			console.log(text + ":" + stat.size);
		`,
	);

	expect(filesystemRoundTrip.result.code).toBe(0);
	expect(filesystemRoundTrip.stdio).toContainEqual({
		channel: "stdout",
		message: "hello:5\n",
	});

	const moduleRoundTrip = await execRuntime(
		page,
		runtimeId,
		`
			const fs = require("fs");
			const helper = require("./helper.js");
			console.log(JSON.stringify({
				moduleValue: helper.value,
				fileText: fs.readFileSync("/workspace/hello.txt", "utf8"),
			}));
		`,
		{
			cwd: "/workspace",
			filePath: "/workspace/index.js",
		},
	);

	expect(moduleRoundTrip.result.code).toBe(0);
	expect(JSON.parse(getLastStdioMessage(moduleRoundTrip, "stdout"))).toEqual({
		moduleValue: 42,
		fileText: "hello",
	});
});

test("provides a faithful guest Buffer and path from upstream polyfills", async ({
	page,
}) => {
	const { runtimeId } = await createRuntime(page);
	const result = await execRuntime(
		page,
		runtimeId,
		`
			const assert = (c, m) => { if (!c) throw new Error("FAIL: " + m); };
			// Buffer is a real global (npm packages expect it), not the WASI shim.
			assert(typeof globalThis.Buffer === "function", "global Buffer");
			const buf = Buffer.from("hello", "utf8");
			assert(Buffer.isBuffer(buf), "isBuffer true for Buffer");
			// Fixed semantics: a plain Uint8Array is NOT a Buffer (the old shim said true).
			assert(!Buffer.isBuffer(new Uint8Array([1, 2, 3])), "isBuffer false for Uint8Array");
			assert(buf.toString("base64") === "aGVsbG8=", "toString base64");
			assert(buf.toString("hex") === "68656c6c6f", "toString hex");
			const num = Buffer.alloc(4);
			num.writeUInt32BE(0x01020304, 0);
			assert(num.toString("hex") === "01020304", "writeUInt32BE");
			assert(num.readUInt32BE(0) === 0x01020304, "readUInt32BE");
			assert(Buffer.from("eA==", "base64").toString("utf8") === "x", "base64 decode");
			// path from path-browserify, posix semantics, path === path.posix.
			const path = require("path");
			assert(path.join("/a", "b", "..", "c") === "/a/c", "path.join");
			assert(path.normalize("/a/b/../c") === "/a/c", "path.normalize");
			assert(path.basename("/a/b/c.txt") === "c.txt", "path.basename");
			assert(path.extname("a.b.c") === ".c", "path.extname");
			assert(path.parse("/a/b.txt").name === "b", "path.parse.name");
			assert(path === path.posix, "path === path.posix");
			console.log("buffer-path-ok");
		`,
	);
	expect(result.result.code, result.result.errorMessage ?? "").toBe(0);
	expect(getLastStdioMessage(result, "stdout")).toBe("buffer-path-ok\n");
});

test("surfaces correct POSIX errno numbers on guest fs errors", async ({
	page,
}) => {
	const { runtimeId } = await createRuntime(page);
	const result = await execRuntime(
		page,
		runtimeId,
		`
			const fs = require("fs");
			const grab = (fn) => {
				try { fn(); return null; }
				catch (e) { return { code: e.code, errno: e.errno }; }
			};
			fs.writeFileSync("/exists.txt", "x");
			const out = {
				// kernel error over the wire (was errno -2 already)
				enoent: grab(() => fs.readFileSync("/missing.txt")),
				// fd-table O_EXCL error (EEXIST was errno -1/undefined before H8)
				eexist: grab(() => fs.openSync("/exists.txt", "wx")),
			};
			console.log(JSON.stringify(out));
		`,
	);
	expect(result.result.code, result.result.errorMessage ?? "").toBe(0);
	const out = JSON.parse(getLastStdioMessage(result, "stdout"));
	expect(out.enoent).toMatchObject({ code: "ENOENT", errno: -2 });
	expect(out.eexist).toMatchObject({ code: "EEXIST", errno: -17 });
});

test("honors string open flags and positional writeSync without losing data", async ({
	page,
}) => {
	const { runtimeId } = await createRuntime(page);
	const result = await execRuntime(
		page,
		runtimeId,
		`
			const fs = require("fs");
			const enc = (s) => new TextEncoder().encode(s);
			fs.mkdirSync("/ws");
			// String flag 'w' must create + truncate (previously any string flag
			// silently collapsed to O_RDONLY, so this created nothing).
			const fd = fs.openSync("/ws/seek.txt", "w");
			fs.writeSync(fd, enc("abcd"));
			// Positional overwrite of "cd" -> "XY"; "ab" must survive. The old
			// client-side read-modify-write discarded the whole file whenever the
			// readback failed; the kernel pwrite is atomic and preserves the rest.
			fs.writeSync(fd, enc("XY"), 0, 2, 2);
			fs.closeSync(fd);
			const afterSeek = fs.readFileSync("/ws/seek.txt", "utf8");
			// Append mode must write at EOF regardless of any offset.
			const afd = fs.openSync("/ws/seek.txt", "a");
			fs.writeSync(afd, enc("Z"));
			fs.closeSync(afd);
			const afterAppend = fs.readFileSync("/ws/seek.txt", "utf8");
			console.log(afterSeek + "|" + afterAppend);
		`,
	);

	expect(result.result.code, result.result.errorMessage ?? "").toBe(0);
	expect(getLastStdioMessage(result, "stdout")).toBe("abXY|abXYZ\n");
});

test("persists browser filesystem data across OPFS-backed runtimes", async ({
	page,
}) => {
	const path = `/agentos-opfs-${Date.now()}-${Math.random()
		.toString(16)
		.slice(2)}.txt`;
	const first = await createRuntime(page, { filesystem: "opfs" });

	const write = await execRuntime(
		page,
		first.runtimeId,
		`
			const fs = require("fs");
			fs.writeFileSync(${JSON.stringify(path)}, "opfs-persisted");
		`,
	);
	expect(write.result.code).toBe(0);
	await disposeAllRuntimes(page);

	const second = await createRuntime(page, { filesystem: "opfs" });
	const read = await execRuntime(
		page,
		second.runtimeId,
		`
			const fs = require("fs");
			console.log(fs.readFileSync(${JSON.stringify(path)}, "utf8"));
			fs.unlinkSync(${JSON.stringify(path)});
		`,
	);

	expect(read.result.code).toBe(0);
	expect(getLastStdioMessage(read, "stdout")).toBe("opfs-persisted\n");
});

test("provides a browser WASI module with stdout routed through the runtime", async ({
	page,
}) => {
	const { runtimeId } = await createRuntime(page);
	const wasmBase64 =
		"AGFzbQEAAAABEANgBH9/f38Bf2ABfwBgAAACRgIWd2FzaV9zbmFwc2hvdF9wcmV2aWV3MQhmZF93cml0ZQAAFndhc2lfc25hcHNob3RfcHJldmlldzEJcHJvY19leGl0AAEDAgECBQMBAAEHEwIGbWVtb3J5AgAGX3N0YXJ0AAIKJgEkAEHoB0GACDYCAEHsB0EINgIAQQFB6AdBAUHwBxAAGkEAEAELCw8BAEGACAsId2FzaS1vawo=";

	const result = await execRuntime(
		page,
		runtimeId,
		`
			(async () => {
				const { WASI } = require("node:wasi");
				const bytes = Uint8Array.from(atob(${JSON.stringify(wasmBase64)}), (char) => char.charCodeAt(0));
				const wasi = new WASI({ returnOnExit: true });
				const { instance } = await WebAssembly.instantiate(bytes, {
					wasi_snapshot_preview1: wasi.wasiImport,
				});
				const exitCode = wasi.start(instance);
				console.log("exit:" + exitCode);
			})();
		`,
	);

	expect(result.result.code, result.result.errorMessage ?? "").toBe(0);
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "wasi-ok\n",
	});
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "exit:0\n",
	});
});

test("routes browser WASI stdin through the runtime process stdin", async ({
	page,
}) => {
	const { runtimeId } = await createRuntime(page);
	const wasmBase64 =
		"AGFzbQEAAAABDAJgBH9/f38Bf2AAAAJEAhZ3YXNpX3NuYXBzaG90X3ByZXZpZXcxB2ZkX3JlYWQAABZ3YXNpX3NuYXBzaG90X3ByZXZpZXcxCGZkX3dyaXRlAAADAgEBBQMBAAEHEwIGbWVtb3J5AgAGX3N0YXJ0AAIKRAFCAEHQD0GAEDYCAEHUD0EgNgIAQQBB0A9BAUHkDxAAGkHaD0GAEDYCAEHeD0HkDygCADYCAEEBQdoPQQFB6A8QARoL";

	const result = await execRuntime(
		page,
		runtimeId,
		`
			(async () => {
				const { WASI } = require("node:wasi");
				const bytes = Uint8Array.from(atob(${JSON.stringify(wasmBase64)}), (char) => char.charCodeAt(0));
				const wasi = new WASI({ returnOnExit: true });
				const { instance } = await WebAssembly.instantiate(bytes, {
					wasi_snapshot_preview1: wasi.wasiImport,
				});
				const exitCode = wasi.start(instance);
				console.log("exit:" + exitCode);
			})();
		`,
		{ stdin: "wasi-stdin\n" },
	);

	expect(result.result.code, result.result.errorMessage ?? "").toBe(0);
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "wasi-stdin\n",
	});
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "exit:0\n",
	});
});

test("routes browser WASI file reads through the runtime filesystem", async ({
	page,
}) => {
	const { runtimeId } = await createRuntime(page);
	const wasmBase64 =
		"AGFzbQEAAAABIgVgCX9/f39/fn5/fwF/YAR/f39/AX9gAX8Bf2ABfwBgAAACrAEFFndhc2lfc25hcHNob3RfcHJldmlldzEJcGF0aF9vcGVuAAAWd2FzaV9zbmFwc2hvdF9wcmV2aWV3MQdmZF9yZWFkAAEWd2FzaV9zbmFwc2hvdF9wcmV2aWV3MQhmZF93cml0ZQABFndhc2lfc25hcHNob3RfcHJldmlldzEIZmRfY2xvc2UAAhZ3YXNpX3NuYXBzaG90X3ByZXZpZXcxCXByb2NfZXhpdAADAwIBBAUDAQABBxMCBm1lbW9yeQIABl9zdGFydAAFCmwBagBBA0EAQZQKQQ5BAEICQgBBAEHoBxAAGkHsB0HMCDYCAEHwB0FANgIAQegHKAIAQewHQQFB9AcQARpB+AdBzAg2AgBB/AdB9AcoAgA2AgBBAUH4B0EBQZwJEAIaQegHKAIAEAMaQQAQBAsLFQEAQZQKCw53YXNpLWlucHV0LnR4dA==";

	const result = await execRuntime(
		page,
		runtimeId,
		`
			(async () => {
				const fs = require("fs");
				const { WASI } = require("node:wasi");
				fs.writeFileSync("/wasi-input.txt", "wasi-file-ok\\n");
				const bytes = Uint8Array.from(atob(${JSON.stringify(wasmBase64)}), (char) => char.charCodeAt(0));
				const wasi = new WASI({
					returnOnExit: true,
					preopens: { "/": "/" },
				});
				const { instance } = await WebAssembly.instantiate(bytes, {
					wasi_snapshot_preview1: wasi.wasiImport,
				});
				const exitCode = wasi.start(instance);
				console.log("exit:" + exitCode);
			})();
		`,
	);

	expect(result.result.code, result.result.errorMessage ?? "").toBe(0);
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "wasi-file-ok\n",
	});
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "exit:0\n",
	});
});

test("enforces browser WASI descriptor read rights", async ({ page }) => {
	const { runtimeId } = await createRuntime(page);
	const wasmBase64 =
		"AGFzbQEAAAABGQNgBH9/f38Bf2AJf39/f39+fn9/AX9gAAACZwMWd2FzaV9zbmFwc2hvdF9wcmV2aWV3MQlwYXRoX29wZW4AARZ3YXNpX3NuYXBzaG90X3ByZXZpZXcxB2ZkX3JlYWQAABZ3YXNpX3NuYXBzaG90X3ByZXZpZXcxCGZkX3dyaXRlAAADAgECBQMBAAEHEwIGbWVtb3J5AgAGX3N0YXJ0AAMKbAFqAQJ/QQNBAEHAAEEOQQBCAEIAQQBBCBAAIQEgAUEARwRAAAtBCCgCACEAQRBBgAE2AgBBFEEINgIAIABBEEEBQRgQASEBIAFBzABHBEAAC0EgQeAANgIAQSRBCjYCAEEBQSBBAUEoEAIaCwslAgBBwAALDndhc2ktaW5wdXQudHh0AEHgAAsKcmlnaHRzLW9rCg==";

	const result = await execRuntime(
		page,
		runtimeId,
		`
			(async () => {
				const fs = require("fs");
				const { WASI } = require("node:wasi");
				fs.writeFileSync("/wasi-input.txt", "secret");
				const bytes = Uint8Array.from(atob(${JSON.stringify(wasmBase64)}), (char) => char.charCodeAt(0));
				const wasi = new WASI({
					returnOnExit: true,
					preopens: { "/": "/" },
				});
				const { instance } = await WebAssembly.instantiate(bytes, {
					wasi_snapshot_preview1: wasi.wasiImport,
				});
				const exitCode = wasi.start(instance);
				console.log("exit:" + exitCode);
			})();
		`,
	);

	expect(result.result.code, result.result.errorMessage ?? "").toBe(0);
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "rights-ok\n",
	});
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "exit:0\n",
	});
});

test("maps browser WASI path_open rights violations to ACCES", async ({
	page,
}) => {
	const { runtimeId } = await createRuntime(page);

	const result = await execRuntime(
		page,
		runtimeId,
		`
			(() => {
				const fs = require("fs");
				const { WASI } = require("node:wasi");
				const encoder = new TextEncoder();
				const memory = new WebAssembly.Memory({ initial: 1 });
				const bytes = new Uint8Array(memory.buffer);
				fs.writeFileSync("/rights.txt", "rights");
				const path = encoder.encode("rights.txt");
				bytes.set(path, 1024);
				const wasi = new WASI({
					returnOnExit: true,
					preopens: {
						"/subset": { hostPath: "/", rightsBase: 2n, rightsInheriting: 2n },
						"/no-write": { hostPath: "/", rightsBase: 2n, rightsInheriting: 66n },
					},
				});
				wasi.instance = { exports: { memory } };
				const subsetErrno = wasi.wasiImport.path_open(3, 0, 1024, path.length, 0, 64n, 0n, 0, 2048);
				const writeErrno = wasi.wasiImport.path_open(4, 0, 1024, path.length, 0, 64n, 0n, 0, 2052);
				console.log("subset:" + subsetErrno + " write:" + writeErrno);
			})();
		`,
	);

	expect(result.result.code, result.result.errorMessage ?? "").toBe(0);
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "subset:2 write:2\n",
	});
});

test("maps browser WASI preopen escapes to NOENT", async ({ page }) => {
	const { runtimeId } = await createRuntime(page);

	const result = await execRuntime(
		page,
		runtimeId,
		`
			(() => {
				const { WASI } = require("node:wasi");
				const encoder = new TextEncoder();
				const memory = new WebAssembly.Memory({ initial: 1 });
				const bytes = new Uint8Array(memory.buffer);
				const escapePath = encoder.encode("../../../../etc/passwd");
				bytes.set(escapePath, 1024);
				const wasi = new WASI({
					returnOnExit: true,
					preopens: { "/sandbox": "/sandbox" },
				});
				wasi.instance = { exports: { memory } };
				const errno = wasi.wasiImport.path_open(3, 0, 1024, escapePath.length, 0, 0n, 0n, 0, 2048);
				console.log("escape:" + errno);
			})();
		`,
	);

	expect(result.result.code, result.result.errorMessage ?? "").toBe(0);
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "escape:44\n",
	});
});

test("supports browser WASI fd_seek and fd_tell", async ({ page }) => {
	const { runtimeId } = await createRuntime(page);
	const wasmBase64 =
		"AGFzbQEAAAABJwVgBH9/f38Bf2AJf39/f39+fn9/AX9gBH9+f38Bf2ACf38Bf2AAAAKpAQUWd2FzaV9zbmFwc2hvdF9wcmV2aWV3MQlwYXRoX29wZW4AARZ3YXNpX3NuYXBzaG90X3ByZXZpZXcxB2ZkX3NlZWsAAhZ3YXNpX3NuYXBzaG90X3ByZXZpZXcxB2ZkX3RlbGwAAxZ3YXNpX3NuYXBzaG90X3ByZXZpZXcxB2ZkX3JlYWQAABZ3YXNpX3NuYXBzaG90X3ByZXZpZXcxCGZkX3dyaXRlAAADAgEEBQMBAAEHEwIGbWVtb3J5AgAGX3N0YXJ0AAUKngEBmwEBAn9BA0EAQcAAQQ5BAEICQgBBAEEIEAAhASABQQBHBEAAC0EIKAIAIQAgAEICQQBBEBABQQBHBEAACyAAQRgQAkEARwRAAAtBGCkDAEICUgRAAAtBIEGAATYCAEEkQQI2AgAgAEEgQQFBKBADQQBHBEAAC0EoKAIAQQJHBEAAC0EwQYABNgIAQTRBAjYCAEEBQTBBAUE4EAQaCwsVAQBBwAALDndhc2ktaW5wdXQudHh0";

	const result = await execRuntime(
		page,
		runtimeId,
		`
			(async () => {
				const fs = require("fs");
				const { WASI } = require("node:wasi");
				fs.writeFileSync("/wasi-input.txt", "abcde");
				const bytes = Uint8Array.from(atob(${JSON.stringify(wasmBase64)}), (char) => char.charCodeAt(0));
				const wasi = new WASI({
					returnOnExit: true,
					preopens: { "/": "/" },
				});
				const { instance } = await WebAssembly.instantiate(bytes, {
					wasi_snapshot_preview1: wasi.wasiImport,
				});
				const exitCode = wasi.start(instance);
				console.log("exit:" + exitCode);
			})();
		`,
	);

	expect(result.result.code, result.result.errorMessage ?? "").toBe(0);
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "cd",
	});
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "exit:0\n",
	});
});

test("supports browser WASI fd_pread without moving descriptor offset", async ({
	page,
}) => {
	const { runtimeId } = await createRuntime(page);
	const wasmBase64 =
		"AGFzbQEAAAABKAVgCX9/f39/fn5/fwF/YAV/f39+fwF/YAJ/fwF/YAR/f39/AX9gAAACiQEEFndhc2lfc25hcHNob3RfcHJldmlldzEJcGF0aF9vcGVuAAAWd2FzaV9zbmFwc2hvdF9wcmV2aWV3MQhmZF9wcmVhZAABFndhc2lfc25hcHNob3RfcHJldmlldzEHZmRfdGVsbAACFndhc2lfc25hcHNob3RfcHJldmlldzEIZmRfd3JpdGUAAwMCAQQFAwEAAQcTAgZtZW1vcnkCAAZfc3RhcnQABAqPAQGMAQEBf0EDQQBB0ABBDkEAQgJCAEEAQQgQAEEARwRAAAtBCCgCACEAQSBBgAE2AgBBJEECNgIAIABBIEEBQgFBKBABQQBHBEAAC0EoKAIAQQJHBEAACyAAQTAQAkEARwRAAAtBMCkDAEIAUgRAAAtBwABBgAE2AgBBxABBAjYCAEEBQcAAQQFByAAQAxoLCxUBAEHQAAsOd2FzaS1pbnB1dC50eHQ=";

	const result = await execRuntime(
		page,
		runtimeId,
		`
			(async () => {
				const fs = require("fs");
				const { WASI } = require("node:wasi");
				fs.writeFileSync("/wasi-input.txt", "abcde");
				const bytes = Uint8Array.from(atob(${JSON.stringify(wasmBase64)}), (char) => char.charCodeAt(0));
				const wasi = new WASI({
					returnOnExit: true,
					preopens: { "/": "/" },
				});
				const { instance } = await WebAssembly.instantiate(bytes, {
					wasi_snapshot_preview1: wasi.wasiImport,
				});
				const exitCode = wasi.start(instance);
				console.log("exit:" + exitCode);
			})();
		`,
	);

	expect(result.result.code).toBe(0);
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "bc",
	});
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "exit:0\n",
	});
});

test("routes browser WASI path_open through filesystem permissions", async ({
	page,
}) => {
	const { runtimeId } = await createRuntime(page, { denyFsRead: true });
	const wasmBase64 =
		"AGFzbQEAAAABFgNgCX9/f39/fn5/fwF/YAF/AX9gAAACRgIWd2FzaV9zbmFwc2hvdF9wcmV2aWV3MQlwYXRoX29wZW4AABZ3YXNpX3NuYXBzaG90X3ByZXZpZXcxCGZkX2Nsb3NlAAEDAgECBQMBAAEHEwIGbWVtb3J5AgAGX3N0YXJ0AAIKNgE0AQF/QQNBAEHAAEEKQQBCAkICQQBBCBAAIQAgAEECRwRAAAsgAEEARgRAQQgoAgAQARoLCwsRAQBBwAALCnNlY3JldC50eHQ=";

	const result = await execRuntime(
		page,
		runtimeId,
		`
			(async () => {
				const fs = require("fs");
				const { WASI } = require("node:wasi");
				fs.writeFileSync("/secret.txt", "should-not-read");
				const bytes = Uint8Array.from(atob(${JSON.stringify(wasmBase64)}), (char) => char.charCodeAt(0));
				const wasi = new WASI({
					returnOnExit: true,
					preopens: { "/": "/" },
				});
				const { instance } = await WebAssembly.instantiate(bytes, {
					wasi_snapshot_preview1: wasi.wasiImport,
				});
				const exitCode = wasi.start(instance);
				console.log("exit:" + exitCode);
				const encoder = new TextEncoder();
				const memory = new WebAssembly.Memory({ initial: 1 });
				const bytesView = new Uint8Array(memory.buffer);
				const path = encoder.encode("secret.txt");
				bytesView.set(path, 1024);
				const directWasi = new WASI({
					returnOnExit: true,
					preopens: { "/": "/" },
				});
				directWasi.instance = { exports: { memory } };
				const errno = directWasi.wasiImport.path_open(3, 0, 1024, path.length, 0, 2n, 0n, 0, 2048);
				console.log("errno:" + errno);
			})();
		`,
	);

	expect(result.result.code, result.result.errorMessage ?? "").toBe(0);
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "exit:0\n",
	});
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "errno:2\n",
	});
	expect(result.permissionDecisions.deniedFsReads).toBeGreaterThanOrEqual(2);
});

test("maps browser WASI read-only filesystem errors to ROFS", async ({
	page,
}) => {
	const { runtimeId } = await createRuntime(page);

	const result = await execRuntime(
		page,
		runtimeId,
		`
				(() => {
					const fs = require("fs");
					const { WASI } = require("node:wasi");
					const encoder = new TextEncoder();
					const memory = new WebAssembly.Memory({ initial: 1 });
					const bytes = new Uint8Array(memory.buffer);
					// A read-only preopen makes the kernel-backed filesystem read-only to the
					// guest: creating or opening-for-write under it is ROFS, while opening for
					// read still succeeds. (The host seeds the file through the writable kernel
					// fs; read-only is the WASI preopen attribute the runner enforces.)
					fs.writeFileSync("/readonly-existing.txt", "seed");
					const wasi = new WASI({
						returnOnExit: true,
						preopens: { "/ro": { hostPath: "/", readOnly: true } },
					});
					wasi.instance = { exports: { memory } };
					const createFilename = encoder.encode("readonly-created.txt");
					const existingFilename = encoder.encode("readonly-existing.txt");
					bytes.set(createFilename, 1024);
					bytes.set(existingFilename, 1200);
					const createErrno = wasi.wasiImport.path_open(3, 0, 1024, createFilename.length, 1, 64n, 0n, 0, 2048);
					const writeErrno = wasi.wasiImport.path_open(3, 0, 1200, existingFilename.length, 0, 64n, 0n, 0, 2052);
					const readErrno = wasi.wasiImport.path_open(3, 0, 1200, existingFilename.length, 0, 2n, 0n, 0, 2056);
					console.log("create:" + createErrno + " write:" + writeErrno + " read:" + readErrno);
				})();
		`,
	);

	expect(result.result.code, result.result.errorMessage ?? "").toBe(0);
	expect(result.stdio).toContainEqual({
		channel: "stdout",
			message: "create:69 write:69 read:0\n",
	});
});

test("supports browser WASI read-only preopen specs", async ({ page }) => {
	const { runtimeId } = await createRuntime(page);

	const result = await execRuntime(
		page,
		runtimeId,
		`
			(() => {
				const fs = require("fs");
				const { WASI } = require("node:wasi");
				const encoder = new TextEncoder();
				const decoder = new TextDecoder();
				const memory = new WebAssembly.Memory({ initial: 1 });
				const view = new DataView(memory.buffer);
				const bytes = new Uint8Array(memory.buffer);
				fs.writeFileSync("/readonly-seed.txt", "seed");
				const wasi = new WASI({
					returnOnExit: true,
					preopens: {
						"/ro": { hostPath: "/", readOnly: true },
					},
				});
				wasi.instance = { exports: { memory } };
				const seedPath = encoder.encode("readonly-seed.txt");
				const createPath = encoder.encode("readonly-created.txt");
				const mkdirPath = encoder.encode("readonly-dir");
				bytes.set(seedPath, 1024);
				bytes.set(createPath, 1200);
				bytes.set(mkdirPath, 1400);
				const openReadErrno = wasi.wasiImport.path_open(3, 0, 1024, seedPath.length, 0, 2n, 0n, 0, 2048);
				const fd = view.getUint32(2048, true);
				view.setUint32(3000, 3200, true);
				view.setUint32(3004, 4, true);
				const readErrno = wasi.wasiImport.fd_read(fd, 3000, 1, 3010);
				const readBytes = view.getUint32(3010, true);
				const text = decoder.decode(bytes.subarray(3200, 3200 + readBytes));
				const inheritingWriteReadErrno = wasi.wasiImport.path_open(3, 0, 1024, seedPath.length, 0, 2n, 64n, 0, 2056);
				const inheritingWriteDirectoryErrno = wasi.wasiImport.path_open(3, 0, 1200, 0, 2, 0n, 64n, 0, 2060);
				const resizeOpenErrno = wasi.wasiImport.path_open(3, 0, 1024, seedPath.length, 0, 4194304n, 0n, 0, 2064);
				const resizeFd = view.getUint32(2064, true);
				const resizeErrno = wasi.wasiImport.fd_filestat_set_size(resizeFd, 0n);
				const createErrno = wasi.wasiImport.path_open(3, 0, 1200, createPath.length, 1, 64n, 0n, 0, 2052);
				const mkdirErrno = wasi.wasiImport.path_create_directory(3, 1400, mkdirPath.length);
				console.log("open:" + openReadErrno + " read:" + readErrno + ":" + text + " inherited:" + inheritingWriteReadErrno + ":" + inheritingWriteDirectoryErrno + " resize:" + resizeOpenErrno + ":" + resizeErrno + " create:" + createErrno + " mkdir:" + mkdirErrno);
			})();
		`,
	);

	expect(result.result.code, result.result.errorMessage ?? "").toBe(0);
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "open:0 read:0:seed inherited:0:0 resize:0:69 create:69 mkdir:69\n",
	});
});

test("supports browser WASI fd_readdir and path_filestat_get", async ({
	page,
}) => {
	const { runtimeId } = await createRuntime(page);
	const wasmBase64 =
		"AGFzbQEAAAABHgRgBX9/f39/AX9gBX9/f35/AX9gBH9/f38Bf2AAAAJyAxZ3YXNpX3NuYXBzaG90X3ByZXZpZXcxEXBhdGhfZmlsZXN0YXRfZ2V0AAAWd2FzaV9zbmFwc2hvdF9wcmV2aWV3MQpmZF9yZWFkZGlyAAEWd2FzaV9zbmFwc2hvdF9wcmV2aWV3MQhmZF93cml0ZQACAwIBAwUDAQABBxMCBm1lbW9yeQIABl9zdGFydAADCngBdgEBf0EDQQBB0ABBCUHgABAAQQBHBEAAC0HwAC0AAEEERwRAAAtBgAEpAwBCBVIEQAALQQNBoAFBgAFCAEHIABABQQBHBEAAC0GwASgCACEAIABFBEAAC0HAAEG4ATYCAEHEACAANgIAQQFBwABBAUHMABACGgsLEAEAQdAACwlhbHBoYS50eHQ=";

	const result = await execRuntime(
		page,
		runtimeId,
		`
			(async () => {
				const fs = require("fs");
				const { WASI } = require("node:wasi");
				fs.writeFileSync("/alpha.txt", "hello");
				const bytes = Uint8Array.from(atob(${JSON.stringify(wasmBase64)}), (char) => char.charCodeAt(0));
				const wasi = new WASI({
					returnOnExit: true,
					preopens: { "/": "/" },
				});
				const { instance } = await WebAssembly.instantiate(bytes, {
					wasi_snapshot_preview1: wasi.wasiImport,
				});
				const exitCode = wasi.start(instance);
				console.log("exit:" + exitCode);
			})();
		`,
	);

	expect(result.result.code).toBe(0);
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "alpha.txt",
	});
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "exit:0\n",
	});
});

test("supports browser WASI path_open directory descriptors", async ({
	page,
}) => {
	const { runtimeId } = await createRuntime(page);
	const wasmBase64 =
		"AGFzbQEAAAABIgRgCX9/f39/fn5/fwF/YAV/f39+fwF/YAR/f39/AX9gAAACagMWd2FzaV9zbmFwc2hvdF9wcmV2aWV3MQlwYXRoX29wZW4AABZ3YXNpX3NuYXBzaG90X3ByZXZpZXcxCmZkX3JlYWRkaXIAARZ3YXNpX3NuYXBzaG90X3ByZXZpZXcxCGZkX3dyaXRlAAIDAgEDBQMBAAEHEwIGbWVtb3J5AgAGX3N0YXJ0AAMKbAFqAQJ/QQNBAEHQAEEGQQJCAkICQQBBCBAAQQBHBEAAC0EIKAIAIQAgAEGgAUGAAUIAQcgAEAFBAEcEQAALQbABKAIAIQEgAUUEQAALQcAAQbgBNgIAQcQAIAE2AgBBAUHAAEEBQcwAEAIaCwsNAQBB0AALBm5lc3RlZA==";

	const result = await execRuntime(
		page,
		runtimeId,
		`
			(async () => {
				const fs = require("fs");
				const { WASI } = require("node:wasi");
				fs.mkdirSync("/nested");
				fs.writeFileSync("/nested/child.txt", "child");
				const bytes = Uint8Array.from(atob(${JSON.stringify(wasmBase64)}), (char) => char.charCodeAt(0));
				const wasi = new WASI({
					returnOnExit: true,
					preopens: { "/": "/" },
				});
				const { instance } = await WebAssembly.instantiate(bytes, {
					wasi_snapshot_preview1: wasi.wasiImport,
				});
				const exitCode = wasi.start(instance);
				console.log("exit:" + exitCode);
			})();
		`,
	);

	expect(result.result.code).toBe(0);
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "child.txt",
	});
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "exit:0\n",
	});
});

test("supports browser WASI path operations relative to opened directories", async ({
	page,
}) => {
	const { runtimeId } = await createRuntime(page);
	const wasmBase64 =
		"AGFzbQEAAAABIgRgBH9/f38Bf2AJf39/f39+fn9/AX9gBX9/f39/AX9gAAACkgEEFndhc2lfc25hcHNob3RfcHJldmlldzEJcGF0aF9vcGVuAAEWd2FzaV9zbmFwc2hvdF9wcmV2aWV3MRFwYXRoX2ZpbGVzdGF0X2dldAACFndhc2lfc25hcHNob3RfcHJldmlldzEHZmRfcmVhZAAAFndhc2lfc25hcHNob3RfcHJldmlldzEIZmRfd3JpdGUAAAMCAQMFAwEAAQcTAgZtZW1vcnkCAAZfc3RhcnQABAqoAQGlAQECf0EDQQBBgARBBkECQgJCAkEAQcQBEABBAEcEQA8LQcQBKAIAIQAgAEEAQZAEQQlBgAIQAUEARwRADwsgAEEAQZAEQQlBAEICQgBBAEHIARAAQQBHBEAPC0HIASgCACEBQbgBQcACNgIAQbwBQQU2AgAgAUG4AUEBQcABEAJBAEcEQA8LQbgBQcACNgIAQbwBQQU2AgBBAUG4AUEBQcABEAMaCwscAgBBgAQLBm5lc3RlZABBkAQLCWNoaWxkLnR4dA==";

	const result = await execRuntime(
		page,
		runtimeId,
		`
			(async () => {
				const fs = require("fs");
				const { WASI } = require("node:wasi");
				fs.mkdirSync("/nested");
				fs.writeFileSync("/nested/child.txt", "child");
				const bytes = Uint8Array.from(atob(${JSON.stringify(wasmBase64)}), (char) => char.charCodeAt(0));
				const wasi = new WASI({
					returnOnExit: true,
					preopens: { "/": "/" },
				});
				const { instance } = await WebAssembly.instantiate(bytes, {
					wasi_snapshot_preview1: wasi.wasiImport,
				});
				const exitCode = wasi.start(instance);
				console.log("exit:" + exitCode);
			})();
		`,
	);

	expect(result.result.code).toBe(0);
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "child",
	});
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "exit:0\n",
	});
});

test("supports browser WASI path mutation imports", async ({ page }) => {
	const { runtimeId } = await createRuntime(page);
	const wasmBase64 =
		"AGFzbQEAAAABEwNgA39/fwF/YAR/f39/AX9gAAACqwEEFndhc2lfc25hcHNob3RfcHJldmlldzEVcGF0aF9jcmVhdGVfZGlyZWN0b3J5AAAWd2FzaV9zbmFwc2hvdF9wcmV2aWV3MRBwYXRoX3VubGlua19maWxlAAAWd2FzaV9zbmFwc2hvdF9wcmV2aWV3MRVwYXRoX3JlbW92ZV9kaXJlY3RvcnkAABZ3YXNpX3NuYXBzaG90X3ByZXZpZXcxCGZkX3dyaXRlAAEDAgECBQMBAAEHEwIGbWVtb3J5AgAGX3N0YXJ0AAQKUgFQAEEDQYAEQQQQAEEARwRADwtBA0GQBEEKEAFBAEcEQA8LQQNBoARBCRACQQBHBEAPC0G4AUGwBDYCAEG8AUEINgIAQQFBuAFBAUHAARADGgsLOAQAQYAECwRtYWRlAEGQBAsKdmljdGltLnR4dABBoAQLCWVtcHR5LWRpcgBBsAQLCG11dGF0ZWQK";

	const result = await execRuntime(
		page,
		runtimeId,
		`
			(async () => {
				const fs = require("fs");
				const { WASI } = require("node:wasi");
				fs.writeFileSync("/victim.txt", "remove me");
				fs.mkdirSync("/empty-dir");
				const bytes = Uint8Array.from(atob(${JSON.stringify(wasmBase64)}), (char) => char.charCodeAt(0));
				const wasi = new WASI({
					returnOnExit: true,
					preopens: { "/": "/" },
				});
				const { instance } = await WebAssembly.instantiate(bytes, {
					wasi_snapshot_preview1: wasi.wasiImport,
				});
				const exitCode = wasi.start(instance);
				console.log("exit:" + exitCode);
				console.log(JSON.stringify({
					madeDirectory: fs.statSync("/made").isDirectory(),
					victimExists: fs.existsSync("/victim.txt"),
					emptyDirExists: fs.existsSync("/empty-dir"),
				}));
			})();
		`,
	);

	expect(result.result.code).toBe(0);
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "mutated\n",
	});
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "exit:0\n",
	});
	expect(JSON.parse(getLastStdioMessage(result, "stdout"))).toEqual({
		madeDirectory: true,
		victimExists: false,
		emptyDirExists: false,
	});
});

test("supports browser WASI link, rename, symlink, and readlink imports", async ({
	page,
}) => {
	const { runtimeId } = await createRuntime(page);
	const wasmBase64 =
		"AGFzbQEAAAABKgVgBn9/f39/fwF/YAd/f39/f39/AX9gBX9/f39/AX9gBH9/f38Bf2AAAAK4AQUWd2FzaV9zbmFwc2hvdF9wcmV2aWV3MQlwYXRoX2xpbmsAARZ3YXNpX3NuYXBzaG90X3ByZXZpZXcxC3BhdGhfcmVuYW1lAAAWd2FzaV9zbmFwc2hvdF9wcmV2aWV3MQxwYXRoX3N5bWxpbmsAAhZ3YXNpX3NuYXBzaG90X3ByZXZpZXcxDXBhdGhfcmVhZGxpbmsAABZ3YXNpX3NuYXBzaG90X3ByZXZpZXcxCGZkX3dyaXRlAAMDAgEEBQMBAAEHEwIGbWVtb3J5AgAGX3N0YXJ0AAUKhAEBgQEAQQNBAEGABEEKQQNBkARBCBAAQQBHBEAPC0EDQZAEQQhBA0GgBEEJEAFBAEcEQA8LQaAEQQlBA0GwBEEIEAJBAEcEQA8LQQNBsARBCEHAAkEgQcQBEANBAEcEQA8LQbgBQcACNgIAQbwBQcQBKAIANgIAQQFBuAFBAUHAARAEGgsLPAQAQYAECwpzb3VyY2UudHh0AEGQBAsIaGFyZC50eHQAQaAECwltb3ZlZC50eHQAQbAECwhsaW5rLnR4dA==";

	const result = await execRuntime(
		page,
		runtimeId,
		`
			(async () => {
				const fs = require("fs");
				const { WASI } = require("node:wasi");
				fs.writeFileSync("/source.txt", "linked-content");
				const bytes = Uint8Array.from(atob(${JSON.stringify(wasmBase64)}), (char) => char.charCodeAt(0));
				const wasi = new WASI({
					returnOnExit: true,
					preopens: { "/": "/" },
				});
				const { instance } = await WebAssembly.instantiate(bytes, {
					wasi_snapshot_preview1: wasi.wasiImport,
				});
				const exitCode = wasi.start(instance);
				console.log("exit:" + exitCode);
				console.log(JSON.stringify({
					source: fs.readFileSync("/source.txt", "utf8"),
					moved: fs.readFileSync("/moved.txt", "utf8"),
					linkTarget: fs.readlinkSync("/link.txt"),
					hardExists: fs.existsSync("/hard.txt"),
				}));
			})();
		`,
	);

	expect(result.result.code).toBe(0);
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "moved.txt",
	});
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "exit:0\n",
	});
	expect(JSON.parse(getLastStdioMessage(result, "stdout"))).toEqual({
		source: "linked-content",
		moved: "linked-content",
		linkTarget: "moved.txt",
		hardExists: false,
	});
});

test("supports browser WASI descriptor flag and sync imports", async ({
	page,
}) => {
	const { runtimeId } = await createRuntime(page);
	const wasmBase64 =
		"AGFzbQEAAAABFwRgAn9/AX9gAX8Bf2AEf39/fwF/YAAAApgBBBZ3YXNpX3NuYXBzaG90X3ByZXZpZXcxE2ZkX2Zkc3RhdF9zZXRfZmxhZ3MAABZ3YXNpX3NuYXBzaG90X3ByZXZpZXcxDWZkX2Zkc3RhdF9nZXQAABZ3YXNpX3NuYXBzaG90X3ByZXZpZXcxB2ZkX3N5bmMAARZ3YXNpX3NuYXBzaG90X3ByZXZpZXcxCGZkX3dyaXRlAAIDAgEDBQMBAAEHEwIGbWVtb3J5AgAGX3N0YXJ0AAQKVQFTAEEBQQEQAEEARwRADwtBAUGAAhABQQBHBEAPC0GCAi8BAEEBRwRADwtBARACQQBHBEAPC0G4AUHAAjYCAEG8AUELNgIAQQFBuAFBAUHAARADGgsLEgEAQcACCwtmZC1zeW5jLW9rCg==";

	const result = await execRuntime(
		page,
		runtimeId,
		`
			(async () => {
				const { WASI } = require("node:wasi");
				const bytes = Uint8Array.from(atob(${JSON.stringify(wasmBase64)}), (char) => char.charCodeAt(0));
				const wasi = new WASI({
					returnOnExit: true,
					preopens: { "/": "/" },
				});
				const { instance } = await WebAssembly.instantiate(bytes, {
					wasi_snapshot_preview1: wasi.wasiImport,
				});
				const exitCode = wasi.start(instance);
				console.log("exit:" + exitCode);
			})();
		`,
	);

	expect(result.result.code).toBe(0);
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "fd-sync-ok\n",
	});
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "exit:0\n",
	});
});

test("supports browser WASI pwrite and filestat set size write-through", async ({
	page,
}) => {
	const { runtimeId } = await createRuntime(page);
	const wasmBase64 =
		"AGFzbQEAAAABMAZgBH9/f38Bf2AJf39/f39+fn9/AX9gBX9/f35/AX9gAn9+AX9gBH9+f38Bf2AAAALZAQYWd2FzaV9zbmFwc2hvdF9wcmV2aWV3MQlwYXRoX29wZW4AARZ3YXNpX3NuYXBzaG90X3ByZXZpZXcxCWZkX3B3cml0ZQACFndhc2lfc25hcHNob3RfcHJldmlldzEUZmRfZmlsZXN0YXRfc2V0X3NpemUAAxZ3YXNpX3NuYXBzaG90X3ByZXZpZXcxB2ZkX3NlZWsABBZ3YXNpX3NuYXBzaG90X3ByZXZpZXcxB2ZkX3JlYWQAABZ3YXNpX3NuYXBzaG90X3ByZXZpZXcxCGZkX3dyaXRlAAADAgEFBQMBAAEHEwIGbWVtb3J5AgAGX3N0YXJ0AAYKugEBtwEBAX9BA0EAQYAEQQdBCULCgIACQgBBAEHEARAAQQBHBEAPC0HEASgCACEAQbgBQZAENgIAQbwBQQU2AgAgAEG4AUEBQgBBwAEQAUEARwRADwsgAEIDEAJBAEcEQA8LIABCAEEAQcgBEANBAEcEQA8LQbgBQaAENgIAQbwBQQU2AgAgAEG4AUEBQcABEARBAEcEQA8LQbgBQaAENgIAQbwBQcABKAIANgIAQQFBuAFBAUHAARAFGgsLGQIAQYAECwdvdXQudHh0AEGQBAsFQUJDREU=";

	const result = await execRuntime(
		page,
		runtimeId,
		`
			(async () => {
				const fs = require("fs");
				const { WASI } = require("node:wasi");
				const bytes = Uint8Array.from(atob(${JSON.stringify(wasmBase64)}), (char) => char.charCodeAt(0));
				const wasi = new WASI({
					returnOnExit: true,
					preopens: { "/": "/" },
				});
				const { instance } = await WebAssembly.instantiate(bytes, {
					wasi_snapshot_preview1: wasi.wasiImport,
				});
				const exitCode = wasi.start(instance);
				console.log("exit:" + exitCode);
				console.log("file:" + fs.readFileSync("/out.txt", "utf8"));
			})();
		`,
	);

	expect(result.result.code).toBe(0);
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "ABC",
	});
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "exit:0\n",
	});
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "file:ABC\n",
	});
});

test("supports browser WASI poll_oneoff import", async ({ page }) => {
	const { runtimeId } = await createRuntime(page);
	const wasmBase64 =
		"AGFzbQEAAAABDAJgBH9/f38Bf2AAAAJIAhZ3YXNpX3NuYXBzaG90X3ByZXZpZXcxC3BvbGxfb25lb2ZmAAAWd2FzaV9zbmFwc2hvdF9wcmV2aWV3MQhmZF93cml0ZQAAAwIBAQUDAQABBxMCBm1lbW9yeQIABl9zdGFydAACCngBdgBBuAJBAjoAAEHAAkEBNgIAQYACQYADQQJBxAEQAEEARwRADwtBxAEoAgBBAkcEQA8LQYoDLQAAQQBHBEAPC0GqAy0AAEECRwRADwtBsAMpAwBQBEAPC0G4AUGABDYCAEG8AUEINgIAQQFBuAFBAUHAARABGgsLDwEAQYAECwhwb2xsLW9rCg==";

	const result = await execRuntime(
		page,
		runtimeId,
		`
			(async () => {
				const { WASI } = require("node:wasi");
				const bytes = Uint8Array.from(atob(${JSON.stringify(wasmBase64)}), (char) => char.charCodeAt(0));
				const wasi = new WASI({
					returnOnExit: true,
					preopens: { "/": "/" },
				});
				const { instance } = await WebAssembly.instantiate(bytes, {
					wasi_snapshot_preview1: wasi.wasiImport,
				});
				const exitCode = wasi.start(instance);
				console.log("exit:" + exitCode);
			})();
		`,
	);

	expect(result.result.code).toBe(0);
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "poll-ok\n",
	});
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "exit:0\n",
	});
});

test("reports browser WASI stdin readiness without consuming input", async ({
	page,
}) => {
	const { runtimeId } = await createRuntime(page);

	const result = await execRuntime(
		page,
		runtimeId,
		`
			(() => {
				const { WASI } = require("node:wasi");
				const memory = new WebAssembly.Memory({ initial: 1 });
				const view = new DataView(memory.buffer);
				const bytes = new Uint8Array(memory.buffer);
				const wasi = new WASI({ returnOnExit: true });
				wasi.instance = { exports: { memory } };
				view.setBigUint64(1024, 7n, true);
				view.setUint8(1032, 1);
				view.setUint32(1040, 0, true);
				const pollErrno = wasi.wasiImport.poll_oneoff(1024, 2048, 1, 3000);
				const events = view.getUint32(3000, true);
				const eventType = view.getUint8(2058);
				const eventNbytes = Number(view.getBigUint64(2064, true));
				view.setUint32(3010, 3200, true);
				view.setUint32(3014, 3, true);
				const readErrno = wasi.wasiImport.fd_read(0, 3010, 1, 3020);
				const readBytes = view.getUint32(3020, true);
				const text = new TextDecoder().decode(bytes.subarray(3200, 3200 + readBytes));
				console.log("poll:" + pollErrno + ":" + events + ":" + eventType + ":" + eventNbytes + " read:" + readErrno + ":" + text);
			})();
		`,
		{ stdin: "abc" },
	);

	expect(result.result.code, result.result.errorMessage ?? "").toBe(0);
	expect(result.stdio).toContainEqual({
		channel: "stdout",
		message: "poll:0:1:1:3 read:0:abc\n",
	});
});

test("captures stdio, stdin, exit codes, and runtime errors through the browser harness", async ({
	page,
}) => {
	const { runtimeId } = await createRuntime(page);

	const stdinResult = await execRuntime(
		page,
		runtimeId,
		`
			process.stdin.setEncoding("utf8");
			let stdinText = "";
			process.stdin.on("data", (chunk) => {
				stdinText += chunk;
			});
			process.stdin.on("end", () => {
				console.log("stdin:" + stdinText.trim());
				console.error("stderr:captured");
			});
			process.stdin.resume();
		`,
		{
			stdin: "playwright-input\n",
		},
	);

	expect(stdinResult.crossOriginIsolated).toBe(true);
	expect(stdinResult.result.code).toBe(0);
	expect(stdinResult.stdio).toContainEqual({
		channel: "stdout",
		message: "stdin:playwright-input\n",
	});
	expect(stdinResult.stdio).toContainEqual({
		channel: "stderr",
		message: "stderr:captured\n",
	});

	const consoleFormatResult = await execRuntime(
		page,
		runtimeId,
		`console.log({ answer: 42 }, ["x"], "value:%d", 7);`,
	);
	expect(consoleFormatResult.result.code).toBe(0);
	expect(getLastStdioMessage(consoleFormatResult, "stdout")).toBe(
		"{ answer: 42 } [ 'x' ] value:%d 7\n",
	);

	const printfFormatResult = await execRuntime(
		page,
		runtimeId,
		`console.log("value:%d", 7, { answer: 42 });`,
	);
	expect(printfFormatResult.result.code).toBe(0);
	expect(getLastStdioMessage(printfFormatResult, "stdout")).toBe(
		"value:7 { answer: 42 }\n",
	);

	const exitResult = await execRuntime(page, runtimeId, `process.exit(7);`);
	expect(exitResult.result.code).toBe(7);

	const wasiExitResult = await execRuntime(
		page,
		runtimeId,
		`
			const { WASI } = require("node:wasi");
			const wasi = new WASI();
			wasi.wasiImport.proc_exit(9);
		`,
	);
	expect(wasiExitResult.result.code).toBe(9);

	const errorResult = await execRuntime(
		page,
		runtimeId,
		`throw new Error("browser-runtime-boom");`,
	);
	expect(errorResult.result.code).toBe(1);
	expect(errorResult.result.errorMessage).toContain("browser-runtime-boom");
});

test("exposes configured virtual process and OS identity in the browser worker", async ({
	page,
}) => {
	const { runtimeId } = await createRuntime(page, {
		processConfig: {
			pid: 77,
			ppid: 7,
			uid: 501,
			gid: 20,
			platform: "linux",
			arch: "x64",
		},
		osConfig: {
			user: "runner",
			uid: 501,
			gid: 20,
			homedir: "/home/runner",
			shell: "/bin/bash",
			hostname: "browser-vm",
			tmpdir: "/browser-tmp",
			type: "BrowserLinux",
			release: "9.9.9-browser",
			version: "Browser agentos build",
			machine: "browser64",
		},
	});

	const result = await execRuntime(
		page,
		runtimeId,
		`
			const os = require("node:os");
			console.log(JSON.stringify({
				pid: process.pid,
				ppid: process.ppid,
				uid: process.getuid(),
				gid: process.getgid(),
				euid: process.geteuid(),
				egid: process.getegid(),
				groups: process.getgroups(),
				platform: process.platform,
				arch: process.arch,
				osPlatform: os.platform(),
				osArch: os.arch(),
				osHostname: os.hostname(),
				osTmpdir: os.tmpdir(),
				osType: os.type(),
				osRelease: os.release(),
				osVersion: os.version(),
				osMachine: os.machine(),
				userInfo: os.userInfo(),
			}));
		`,
	);

	expect(result.result.code).toBe(0);
	expect(JSON.parse(getLastStdioMessage(result, "stdout"))).toEqual({
		pid: 77,
		ppid: 7,
		uid: 501,
		gid: 20,
		euid: 501,
		egid: 20,
		groups: [20],
		platform: "linux",
		arch: "x64",
		osPlatform: "linux",
		osArch: "x64",
		osHostname: "browser-vm",
		osTmpdir: "/browser-tmp",
		osType: "BrowserLinux",
		osRelease: "9.9.9-browser",
		osVersion: "Browser agentos build",
		osMachine: "browser64",
		userInfo: {
			username: "runner",
			uid: 501,
			gid: 20,
			shell: "/bin/bash",
			homedir: "/home/runner",
		},
	});
});

test("routes browser child_process through the driver command executor", async ({
	page,
}) => {
	const { runtimeId } = await createRuntime(page, {
		commandExecutor: "echo",
	});

	const result = await execRuntime(
		page,
		runtimeId,
		`
			(async () => {
				const childProcess = require("child_process");
				const sync = childProcess.spawnSync("echo", ["sync-value"], {
					encoding: "utf8",
					cwd: "/workspace",
					env: { MODE: "sync" },
				});
				const child = childProcess.spawn("wait-signal", ["async-value"], {
					cwd: "/workspace",
					env: { MODE: "async" },
				});
				let stdout = "";
				child.stdout.on("data", (chunk) => {
					stdout += chunk.toString();
				});
				let invalidSignalCode = null;
				try {
					child.kill("SIGBOGUS");
				} catch (error) {
					invalidSignalCode = error && error.code;
				}
				child.kill("SIGUSR1");
				const [asyncCode, asyncSignal] = await new Promise((resolve, reject) => {
					child.on("error", reject);
					child.on("exit", (code, signal) => resolve([code, signal]));
				});
				console.log(JSON.stringify({
					syncStatus: sync.status,
					syncStdout: sync.stdout.trim(),
					asyncCode,
					asyncSignal,
					asyncStdout: stdout.trim(),
					invalidSignalCode,
				}));
			})();
		`,
	);

	expect(result.result.code).toBe(0);
	expect(JSON.parse(getLastStdioMessage(result, "stdout"))).toEqual({
		syncStatus: 0,
		syncStdout: "sync-value",
		asyncCode: null,
		asyncSignal: "SIGUSR1",
		asyncStdout: "signal:10",
		invalidSignalCode: "ERR_UNKNOWN_SIGNAL",
	});

	const deniedRuntime = await createRuntime(page, {
		commandExecutor: "echo",
		denyChildProcess: true,
	});
	const denied = await execRuntime(
		page,
		deniedRuntime.runtimeId,
		`
			const { spawnSync } = require("child_process");
			const result = spawnSync("echo", ["denied"], { encoding: "utf8" });
			console.log(JSON.stringify({
				status: result.status,
				code: result.error && result.error.code,
				message: result.error && result.error.message,
			}));
		`,
	);

	expect(denied.result.code).toBe(0);
	expect(JSON.parse(getLastStdioMessage(denied, "stdout"))).toMatchObject({
		status: 1,
		code: "EACCES",
	});
});

test("resolves browser loopback DNS through the guest network bridge", async ({
	page,
}) => {
	const { runtimeId } = await createRuntime(page, { useDefaultNetwork: true });
	const result = await execRuntime(
		page,
		runtimeId,
		`
			(async () => {
				const dns = require("dns").promises;
				const localhost = await dns.lookup("localhost");
				const literal = await dns.lookup("127.0.0.1");
				let externalCode = null;
				try {
					await dns.lookup("example.com");
				} catch (error) {
					externalCode = error && error.code;
				}
				console.log(JSON.stringify({ localhost, literal, externalCode }));
			})();
		`,
	);

	expect(result.result.code).toBe(0);
	expect(JSON.parse(getLastStdioMessage(result, "stdout"))).toEqual({
		localhost: { address: "127.0.0.1", family: 4 },
		literal: { address: "127.0.0.1", family: 4 },
		externalCode: "ENOSYS",
	});
});

test("routes browser dgram loopback through the guest network bridge", async ({
	page,
}) => {
	const { runtimeId } = await createRuntime(page, { useDefaultNetwork: true });
	const result = await execRuntime(
		page,
		runtimeId,
		`
			(async () => {
				const dgram = require("node:dgram");
				const server = dgram.createSocket("udp4");
				const client = dgram.createSocket("udp4");
				const received = new Promise((resolve, reject) => {
					server.on("error", reject);
					client.on("error", reject);
					server.on("message", (message, remote) => {
						resolve({
							message: message.toString("utf8"),
							remoteAddress: remote.address,
							remoteFamily: remote.family,
							remoteSize: remote.size,
						});
					});
				});
				await new Promise((resolve) => server.bind(0, "127.0.0.1", resolve));
				const port = server.address().port;
				client.send(new TextEncoder().encode("udp-ok"), port, "127.0.0.1");
				const payload = await Promise.race([
					received,
					new Promise((_, reject) => setTimeout(() => reject(new Error("udp timeout")), 1000)),
				]);
				server.close();
				client.close();
				console.log(JSON.stringify(payload));
			})();
		`,
	);

	expect(result.result.code).toBe(0);
	expect(JSON.parse(getLastStdioMessage(result, "stdout"))).toEqual({
		message: "udp-ok",
		remoteAddress: "127.0.0.1",
		remoteFamily: "IPv4",
		remoteSize: 6,
	});
});

test("denies browser dgram bind through the applied network policy", async ({
	page,
}) => {
	const { runtimeId } = await createRuntime(page, {
		useDefaultNetwork: true,
		denyNetwork: true,
	});
	const result = await execRuntime(
		page,
		runtimeId,
		`
			(async () => {
				const dgram = require("node:dgram");
				const socket = dgram.createSocket("udp4");
				const deniedPromise = Promise.race([
					new Promise((resolve) => {
						socket.on("error", (error) => {
							resolve({ code: error.code, message: error.message });
						});
					}),
					new Promise((resolve) => setTimeout(() => resolve({ code: "timeout" }), 1000)),
				]);
				socket.bind(0, "127.0.0.1");
				const denied = await deniedPromise;
				console.log(JSON.stringify(denied));
			})();
		`,
	);

	expect(result.result.code).toBe(0);
	expect(JSON.parse(getLastStdioMessage(result, "stdout"))).toMatchObject({
		code: "EACCES",
	});
});

test("denies browser dgram send through the applied network policy", async ({
	page,
}) => {
	const { runtimeId } = await createRuntime(page, {
		useDefaultNetwork: true,
		denyNetworkPort: 9,
	});
	const result = await execRuntime(
		page,
		runtimeId,
		`
			(async () => {
				const dgram = require("node:dgram");
				const socket = dgram.createSocket("udp4");
				const denied = await new Promise((resolve) => {
					socket.send(new TextEncoder().encode("blocked"), 9, "127.0.0.1", (error) => {
						resolve(error ? { code: error.code, message: error.message } : { code: null });
					});
				});
				socket.close();
				console.log(JSON.stringify(denied));
			})();
		`,
	);

	expect(result.result.code).toBe(0);
	expect(JSON.parse(getLastStdioMessage(result, "stdout"))).toMatchObject({
		code: "EACCES",
	});
});

test("tracks browser process signal handlers through the sync bridge", async ({
	page,
}) => {
	const { runtimeId } = await createRuntime(page);

	const direct = await debugPendingExec(
		page,
		runtimeId,
		`
			(async () => {
				_processSignalState.applySyncPromise(void 0, [15, "user", "[]", 0]);
				await new Promise(() => undefined);
			})();
		`,
		250,
	);

	expect(direct.outcome).toBe("rejected");
	expect(direct.debug.signalHandlers).toEqual([
		{
			executionId: "exec-1",
			handlers: [
				{
					signal: 15,
					action: "user",
					mask: [],
					flags: 0,
				},
			],
		},
	]);

	const registeredRuntime = await createRuntime(page);
	const registered = await debugPendingExec(
		page,
		registeredRuntime.runtimeId,
		`
			(async () => {
				const handler = () => {};
				process.on("SIGTERM", handler);
				await new Promise(() => undefined);
			})();
		`,
		250,
	);

	expect(registered.outcome).toBe("rejected");
	expect(registered.debug.signalHandlers).toEqual([
		{
			executionId: "exec-1",
			handlers: [
				{
					signal: 15,
					action: "user",
					mask: [],
					flags: 0,
				},
			],
		},
	]);

	const removedRuntime = await createRuntime(page);
	const removed = await debugPendingExec(
		page,
		removedRuntime.runtimeId,
		`
			(async () => {
				const handler = () => {};
				process.on("SIGTERM", handler);
				process.off("SIGTERM", handler);
				await new Promise(() => undefined);
			})();
		`,
		250,
	);

	expect(removed.outcome).toBe("rejected");
	expect(removed.debug.signalHandlers).toEqual([]);
});

test("provides browser node:crypto digest, hmac, pbkdf2, scrypt, and random APIs", async ({
	page,
}) => {
	const { runtimeId } = await createRuntime(page);

	const result = await execRuntime(
		page,
		runtimeId,
		`
			(async () => {
				const fixture = ${JSON.stringify(cryptoBasicFixture)};
				const crypto = require("node:crypto");
				const pbkdf2Hex = await new Promise((resolve, reject) => {
					crypto.pbkdf2(fixture.password, fixture.salt, fixture.iterations, fixture.keyLength, "sha256", (error, value) => {
						if (error) reject(error);
						else resolve(value.toString("hex"));
					});
				});
				const pbkdf2Sha384Hex = await new Promise((resolve, reject) => {
					crypto.pbkdf2(fixture.password, fixture.salt, fixture.iterations, fixture.keyLength, "sha384", (error, value) => {
						if (error) reject(error);
						else resolve(value.toString("hex"));
					});
				});
				const scryptHex = await new Promise((resolve, reject) => {
					crypto.scrypt(fixture.password, fixture.salt, fixture.keyLength, fixture.scrypt, (error, value) => {
						if (error) reject(error);
						else resolve(value.toString("hex"));
					});
				});
				const bytesFromHex = (hex) => Uint8Array.from(hex.match(/../g).map((byte) => parseInt(byte, 16)));
				const bytesToHex = (bytes) => Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
				const aesCbcKey = bytesFromHex(fixture.aesCbc.keyHex);
				const aesCbcIv = bytesFromHex(fixture.aesCbc.ivHex);
				const aesGcmKey = bytesFromHex(fixture.aesGcm.keyHex);
				const aesGcmIv = bytesFromHex(fixture.aesGcm.ivHex);
				const aesGcmAad = new TextEncoder().encode(fixture.aesGcm.aad);
				const secretKey = crypto.createSecretKey(new TextEncoder().encode(fixture.hmacKey));
				const hmacWithSecretKey = crypto.createHmac("sha256", secretKey).update(fixture.message).digest("hex");
				const generatedHmacKey = crypto.generateKeySync("hmac", { length: 256 });
				const generatedAesKey = crypto.generateKeySync("aes", { length: 256 });
				const modPow = (base, exponent, modulus) => {
					let result = 1n;
					let cursor = base % modulus;
					let remaining = exponent;
					while (remaining > 0n) {
						if ((remaining & 1n) === 1n) result = (result * cursor) % modulus;
						cursor = (cursor * cursor) % modulus;
						remaining >>= 1n;
					}
					return result;
				};
				const isPrime = (value) => {
					if (value < 2n) return false;
					for (const prime of [2n, 3n, 5n, 7n, 11n, 13n, 17n, 19n, 23n, 29n, 31n, 37n]) {
						if (value === prime) return true;
						if (value % prime === 0n) return false;
					}
					let d = value - 1n;
					let s = 0;
					while ((d & 1n) === 0n) {
						d >>= 1n;
						s += 1;
					}
					for (const base of [2n, 3n, 5n, 7n, 11n, 13n, 17n, 19n, 23n, 29n, 31n, 37n]) {
						if (base >= value - 2n) continue;
						let x = modPow(base, d, value);
						if (x === 1n || x === value - 1n) continue;
						let witness = false;
						for (let r = 1; r < s; r += 1) {
							x = (x * x) % value;
							if (x === value - 1n) {
								witness = true;
								break;
							}
						}
						if (!witness) return false;
					}
					return true;
				};
				const generatedPrime = crypto.generatePrimeSync(fixture.expected.primes.bits, { bigint: true });
				const generatedSafePrime = crypto.generatePrimeSync(fixture.expected.primes.safeBits, { bigint: true, safe: true });
				const generatedPrimeBuffer = crypto.generatePrimeSync(fixture.expected.primes.bufferBits);
				const groupAlice = crypto.getDiffieHellman("modp14");
				const groupBob = crypto.getDiffieHellman("modp14");
				groupAlice.generateKeys();
				groupBob.generateKeys();
				const groupSecretA = groupAlice.computeSecret(groupBob.getPublicKey());
				const groupSecretB = groupBob.computeSecret(groupAlice.getPublicKey());
				const fixtureDhAlice = crypto.createDiffieHellman(
					Uint8Array.from(fixture.dh.primeHex.match(/../g).map((byte) => parseInt(byte, 16))),
					Uint8Array.from(fixture.dh.generatorHex.match(/../g).map((byte) => parseInt(byte, 16))),
				);
				const fixtureDhBob = crypto.createDiffieHellman(
					Uint8Array.from(fixture.dh.primeHex.match(/../g).map((byte) => parseInt(byte, 16))),
					Uint8Array.from(fixture.dh.generatorHex.match(/../g).map((byte) => parseInt(byte, 16))),
				);
				fixtureDhAlice.setPrivateKey(fixture.dh.privateAHex, "hex");
				fixtureDhAlice.setPublicKey(fixture.dh.publicAHex, "hex");
				fixtureDhBob.setPrivateKey(fixture.dh.privateBHex, "hex");
				fixtureDhBob.setPublicKey(fixture.dh.publicBHex, "hex");
				const fixtureDhSecretA = fixtureDhAlice.computeSecret(fixture.dh.publicBHex, "hex", "hex");
				const fixtureDhSecretB = fixtureDhBob.computeSecret(fixture.dh.publicAHex, "hex", "hex");
				const x25519Alice = crypto.generateKeyPairSync("x25519");
				const x25519Bob = crypto.generateKeyPairSync("x25519");
				const x25519SecretA = crypto.diffieHellman({
					privateKey: x25519Alice.privateKey,
					publicKey: x25519Bob.publicKey,
				});
				const x25519SecretB = crypto.diffieHellman({
					privateKey: x25519Bob.privateKey,
					publicKey: x25519Alice.publicKey,
				});
				const ecdhAlice = crypto.createECDH("prime256v1");
				const ecdhBob = crypto.createECDH("prime256v1");
				ecdhAlice.generateKeys();
				ecdhBob.generateKeys();
				const ecdhSecretA = ecdhAlice.computeSecret(ecdhBob.getPublicKey());
				const ecdhSecretB = ecdhBob.computeSecret(ecdhAlice.getPublicKey());
				const fixtureEcdhAlice = crypto.createECDH(fixture.ecdh.curve);
				const fixtureEcdhBob = crypto.createECDH(fixture.ecdh.curve);
				fixtureEcdhAlice.setPrivateKey(fixture.ecdh.privateAHex, "hex");
				fixtureEcdhBob.setPrivateKey(fixture.ecdh.privateBHex, "hex");
				const fixtureEcdhSecretA = fixtureEcdhAlice.computeSecret(fixture.ecdh.publicBHex, "hex", "hex");
				const fixtureEcdhSecretB = fixtureEcdhBob.computeSecret(fixture.ecdh.publicAHex, "hex", "hex");
				const cipher = crypto.createCipheriv(fixture.aesCbc.algorithm, aesCbcKey, aesCbcIv);
				const aes256CbcCiphertext = cipher.update(fixture.aesCbc.plaintext, "utf8", "hex") + cipher.final("hex");
				const decipher = crypto.createDecipheriv(fixture.aesCbc.algorithm, aesCbcKey, aesCbcIv);
				const aes256CbcPlaintext = decipher.update(aes256CbcCiphertext, "hex", "utf8") + decipher.final("utf8");
				const gcmCipher = crypto.createCipheriv(fixture.aesGcm.algorithm, aesGcmKey, aesGcmIv, {
					authTagLength: fixture.aesGcm.authTagLength,
				});
				gcmCipher.setAAD(aesGcmAad);
				const aes256GcmCiphertext = gcmCipher.update(fixture.aesGcm.plaintext, "utf8", "hex") + gcmCipher.final("hex");
				const aes256GcmAuthTag = gcmCipher.getAuthTag().toString("hex");
				const gcmDecipher = crypto.createDecipheriv(fixture.aesGcm.algorithm, aesGcmKey, aesGcmIv, {
					authTagLength: fixture.aesGcm.authTagLength,
				});
				gcmDecipher.setAAD(aesGcmAad);
				gcmDecipher.setAuthTag(bytesFromHex(aes256GcmAuthTag));
				const aes256GcmPlaintext = gcmDecipher.update(aes256GcmCiphertext, "hex", "utf8") + gcmDecipher.final("utf8");
				const subtleKey = await crypto.subtle.importKey("raw", aesGcmKey, { name: "AES-GCM" }, false, ["encrypt", "decrypt"]);
				const subtleAlgorithm = {
					name: "AES-GCM",
					iv: aesGcmIv,
					additionalData: aesGcmAad,
					tagLength: fixture.aesGcm.authTagLength * 8,
				};
				const aes256GcmWebCryptoBytes = new Uint8Array(await crypto.subtle.encrypt(
					subtleAlgorithm,
					subtleKey,
					new TextEncoder().encode(fixture.aesGcm.plaintext),
				));
				const aes256GcmWebCryptoCiphertext = bytesToHex(aes256GcmWebCryptoBytes);
				const aes256GcmWebCryptoPlaintext = new TextDecoder().decode(await crypto.subtle.decrypt(
					subtleAlgorithm,
					subtleKey,
					aes256GcmWebCryptoBytes,
				));
				const generatedCipher = crypto.createCipheriv("aes-256-cbc", generatedAesKey, aesCbcIv);
				const generatedCiphertext = generatedCipher.update("key-object", "utf8", "hex") + generatedCipher.final("hex");
				const generatedDecipher = crypto.createDecipheriv("aes-256-cbc", generatedAesKey, aesCbcIv);
				const generatedPlaintext = generatedDecipher.update(generatedCiphertext, "hex", "utf8") + generatedDecipher.final("utf8");
				const random = crypto.randomBytes(16);
				const fillTarget = new Uint8Array(8);
				crypto.randomFillSync(fillTarget);
				let invalidAesCode = null;
				try {
					crypto.generateKeySync("aes", { length: 129 });
				} catch (error) {
					invalidAesCode = error && error.code;
				}
				console.log(JSON.stringify({
					hashes: crypto.getHashes(),
					md5: crypto.createHash("md5").update(fixture.message).digest("hex"),
					sha224: crypto.createHash("sha224").update(fixture.message).digest("hex"),
					sha256: crypto.createHash("sha256").update(fixture.message).digest("hex"),
					sha384: crypto.createHash("sha384").update(fixture.message).digest("hex"),
					hmacSha256: crypto.createHmac("sha256", fixture.hmacKey).update(fixture.message).digest("hex"),
					hmacWithSecretKey,
					hmacSha384: crypto.createHmac("sha384", fixture.hmacKey).update(fixture.message).digest("hex"),
					pbkdf2SyncHex: crypto.pbkdf2Sync(fixture.password, fixture.salt, fixture.iterations, fixture.keyLength, "sha256").toString("hex"),
					pbkdf2Sha384Hex,
					pbkdf2Hex,
					scryptSyncHex: crypto.scryptSync(fixture.password, fixture.salt, fixture.keyLength, fixture.scrypt).toString("hex"),
					scryptHex,
					aes256CbcCiphertext,
					aes256CbcPlaintext,
					aes256GcmCiphertext,
					aes256GcmAuthTag,
					aes256GcmPlaintext,
					aes256GcmWebCryptoCiphertext,
					aes256GcmWebCryptoPlaintext,
					generatedPlaintext,
					secretKeyType: secretKey.type,
					secretKeySize: secretKey.symmetricKeySize,
					secretKeyExportHex: secretKey.export().toString("hex"),
					generatedHmacKeyType: generatedHmacKey.type,
					generatedHmacKeySize: generatedHmacKey.symmetricKeySize,
					generatedAesKeyType: generatedAesKey.type,
					generatedAesKeySize: generatedAesKey.symmetricKeySize,
					generatedPrimeType: typeof generatedPrime,
					generatedPrimeBits: generatedPrime.toString(2).length,
					generatedPrimeLooksPrime: isPrime(generatedPrime),
					generatedSafePrimeBits: generatedSafePrime.toString(2).length,
					generatedSafePrimeLooksSafe: isPrime(generatedSafePrime) && isPrime((generatedSafePrime - 1n) / 2n),
					generatedPrimeBufferBits: fixture.expected.primes.bufferBits,
					generatedPrimeBufferByteLength: generatedPrimeBuffer.byteLength,
					groupVerifyError: groupAlice.verifyError,
					groupSecretMatches: groupSecretA.equals(groupSecretB),
					groupSecretLength: groupSecretA.length,
					groupPrimeLength: groupAlice.getPrime().length,
					groupGeneratorHex: groupAlice.getGenerator("hex"),
					fixtureDhPublicAHex: fixtureDhAlice.getPublicKey("hex").slice(-fixture.dh.publicAHex.length),
					fixtureDhPublicBHex: fixtureDhBob.getPublicKey("hex").slice(-fixture.dh.publicBHex.length),
					fixtureDhSecretAHex: fixtureDhSecretA,
					fixtureDhSecretBHex: fixtureDhSecretB,
					x25519KeyTypes: [
						x25519Alice.privateKey.type,
						x25519Alice.privateKey.asymmetricKeyType,
						x25519Alice.publicKey.type,
						x25519Alice.publicKey.asymmetricKeyType,
					],
					x25519SecretMatches: x25519SecretA.equals(x25519SecretB),
					x25519SecretLength: x25519SecretA.length,
					ecdhSecretMatches: ecdhSecretA.equals(ecdhSecretB),
					ecdhSecretLength: ecdhSecretA.length,
					ecdhPublicKeyLength: ecdhAlice.getPublicKey().length,
					ecdhCompressedPublicKeyLength: ecdhAlice.getPublicKey(undefined, "compressed").length,
					fixtureEcdhPublicAHex: fixtureEcdhAlice.getPublicKey("hex"),
					fixtureEcdhPublicBHex: fixtureEcdhBob.getPublicKey("hex"),
					fixtureEcdhSecretAHex: fixtureEcdhSecretA,
					fixtureEcdhSecretBHex: fixtureEcdhSecretB,
					invalidAesCode,
					randomLength: random.length,
					fillChanged: Array.from(fillTarget).some((byte) => byte !== 0),
					ciphers: crypto.getCiphers(),
					curves: crypto.getCurves(),
					uuidLooksValid: /^[0-9a-f-]{36}$/.test(crypto.randomUUID()),
				}));
			})();
		`,
	);

	expect(result.result.code, result.result.errorMessage ?? "").toBe(0);
	const expectedDhSecret = cryptoBasicFixture.dh.secretHex.padStart(
		cryptoBasicFixture.dh.primeHex.length,
		"0",
	);
	expect(JSON.parse(getLastStdioMessage(result, "stdout"))).toEqual({
		hashes: cryptoBasicFixture.expected.hashes,
		md5: cryptoBasicFixture.expected.md5,
		sha224: cryptoBasicFixture.expected.sha224,
		sha256: cryptoBasicFixture.expected.sha256,
		sha384: cryptoBasicFixture.expected.sha384,
		hmacSha256: cryptoBasicFixture.expected.hmacSha256,
		hmacWithSecretKey: cryptoBasicFixture.expected.hmacSha256,
		hmacSha384: cryptoBasicFixture.expected.hmacSha384,
		pbkdf2SyncHex: cryptoBasicFixture.expected.pbkdf2Sha256,
		pbkdf2Sha384Hex: cryptoBasicFixture.expected.pbkdf2Sha384,
		pbkdf2Hex: cryptoBasicFixture.expected.pbkdf2Sha256,
		scryptSyncHex: cryptoBasicFixture.expected.scrypt,
		scryptHex: cryptoBasicFixture.expected.scrypt,
		aes256CbcCiphertext: cryptoBasicFixture.expected.aes256CbcCiphertext,
		aes256CbcPlaintext: cryptoBasicFixture.aesCbc.plaintext,
		aes256GcmCiphertext: cryptoBasicFixture.expected.aes256GcmCiphertext,
		aes256GcmAuthTag: cryptoBasicFixture.expected.aes256GcmAuthTag,
		aes256GcmPlaintext: cryptoBasicFixture.aesGcm.plaintext,
		aes256GcmWebCryptoCiphertext: cryptoBasicFixture.expected.aes256GcmWebCryptoCiphertext,
		aes256GcmWebCryptoPlaintext: cryptoBasicFixture.aesGcm.plaintext,
		generatedPlaintext: "key-object",
		secretKeyType: "secret",
		secretKeySize: cryptoBasicFixture.hmacKey.length,
		secretKeyExportHex: "7368617265642d736563726574",
		generatedHmacKeyType: "secret",
		generatedHmacKeySize: 32,
		generatedAesKeyType: "secret",
		generatedAesKeySize: 32,
		generatedPrimeType: "bigint",
		generatedPrimeBits: cryptoBasicFixture.expected.primes.bits,
		generatedPrimeLooksPrime: true,
		generatedSafePrimeBits: cryptoBasicFixture.expected.primes.safeBits,
		generatedSafePrimeLooksSafe: true,
		generatedPrimeBufferBits: cryptoBasicFixture.expected.primes.bufferBits,
		generatedPrimeBufferByteLength: cryptoBasicFixture.expected.primes.bufferByteLength,
		groupVerifyError: 0,
		groupSecretMatches: true,
		groupSecretLength: 256,
		groupPrimeLength: 256,
		groupGeneratorHex: "02",
		fixtureDhPublicAHex: cryptoBasicFixture.dh.publicAHex,
		fixtureDhPublicBHex: cryptoBasicFixture.dh.publicBHex,
		fixtureDhSecretAHex: expectedDhSecret,
		fixtureDhSecretBHex: expectedDhSecret,
		x25519KeyTypes: ["private", "x25519", "public", "x25519"],
		x25519SecretMatches: true,
		x25519SecretLength: 32,
		ecdhSecretMatches: true,
		ecdhSecretLength: 32,
		ecdhPublicKeyLength: 65,
		ecdhCompressedPublicKeyLength: 33,
		fixtureEcdhPublicAHex: cryptoBasicFixture.ecdh.publicAHex,
		fixtureEcdhPublicBHex: cryptoBasicFixture.ecdh.publicBHex,
		fixtureEcdhSecretAHex: cryptoBasicFixture.ecdh.secretHex,
		fixtureEcdhSecretBHex: cryptoBasicFixture.ecdh.secretHex,
		invalidAesCode: "ERR_INVALID_ARG_VALUE",
		randomLength: 16,
		fillChanged: true,
		ciphers: cryptoBasicFixture.expected.ciphers,
		curves: cryptoBasicFixture.expected.curves,
		uuidLooksValid: true,
	});
});

test("provides browser node:crypto RSA sign and verify parity", async ({
	page,
}) => {
	const { runtimeId } = await createRuntime(page);

	const result = await execRuntime(
		page,
		runtimeId,
		`
			(async () => {
			const crypto = require("node:crypto");
			const rsaFixture = ${JSON.stringify(cryptoBasicFixture.rsa)};
			const privateKey = rsaFixture.privatePem;
			const publicKey = rsaFixture.publicPem;
			const message = rsaFixture.message;
			const expectedSignature = rsaFixture.sha256SignatureHex;
			const privateKeyObject = crypto.createPrivateKey(privateKey);
			const publicKeyObject = crypto.createPublicKey(publicKey);
			const signature = crypto.createSign("sha256").update(message).sign(privateKeyObject, "hex");
			const verified = crypto.createVerify("sha256").update(message).verify(publicKeyObject, signature, "hex");
			const expectedVerified = crypto.createVerify("sha256").update(message).verify(publicKeyObject, expectedSignature, "hex");
			const tampered = crypto.createVerify("sha256").update(message + "!").verify(publicKeyObject, signature, "hex");
			const oneShotSignature = crypto.sign("sha256", new TextEncoder().encode(message), privateKeyObject);
			const oneShotVerified = crypto.verify("sha256", new TextEncoder().encode(message), publicKeyObject, oneShotSignature);
			const oneShotTampered = crypto.verify("sha256", new TextEncoder().encode(message + "!"), publicKeyObject, oneShotSignature);
			const encodeText = (value) => new TextEncoder().encode(value);
			const generatedPair = crypto.generateKeyPairSync("rsa", { modulusLength: 1024 });
			const generatedPrivatePem = generatedPair.privateKey.export({ format: "pem", type: "pkcs8" });
			const generatedPublicPem = generatedPair.publicKey.export({ format: "pem", type: "spki" });
			const generatedSignature = crypto.sign("sha256", encodeText("generated-rsa"), generatedPair.privateKey);
			const generatedVerified = crypto.verify("sha256", encodeText("generated-rsa"), generatedPair.publicKey, generatedSignature);
			const encodedPair = crypto.generateKeyPairSync("rsa", {
				modulusLength: 1024,
				publicKeyEncoding: { format: "pem", type: "spki" },
				privateKeyEncoding: { format: "pem", type: "pkcs8" },
			});
			const generatedAsyncPair = await new Promise((resolve, reject) => {
				crypto.generateKeyPair("rsa", { modulusLength: 1024 }, (error, publicKeyValue, privateKeyValue) => {
					if (error) reject(error);
					else resolve({
						publicType: publicKeyValue.type,
						privateType: privateKeyValue.type,
						publicAsymmetricKeyType: publicKeyValue.asymmetricKeyType,
						privateAsymmetricKeyType: privateKeyValue.asymmetricKeyType,
					});
				});
			});
			const oaepCiphertext = crypto.publicEncrypt(publicKeyObject, encodeText("agentos-rsa-oaep"));
			const oaepPlaintext = crypto.privateDecrypt(privateKeyObject, oaepCiphertext).toString("utf8");
			const pkcs1Ciphertext = crypto.publicEncrypt(
				{ key: publicKeyObject, padding: crypto.constants.RSA_PKCS1_PADDING },
				encodeText("agentos-rsa-pkcs1"),
			);
			const pkcs1Plaintext = crypto.privateDecrypt(
				{ key: privateKeyObject, padding: crypto.constants.RSA_PKCS1_PADDING },
				pkcs1Ciphertext,
			).toString("utf8");
			console.log(JSON.stringify({
				signature,
				verified,
				expectedVerified,
				tampered,
				oneShotSignature: oneShotSignature.toString("hex"),
				oneShotVerified,
				oneShotTampered,
				generatedKeyTypes: [
					generatedPair.publicKey.type,
					generatedPair.publicKey.asymmetricKeyType,
					generatedPair.privateKey.type,
					generatedPair.privateKey.asymmetricKeyType,
				],
				generatedPrivatePemHeader: generatedPrivatePem.split("\\n")[0],
				generatedPublicPemHeader: generatedPublicPem.split("\\n")[0],
				generatedVerified,
				encodedPublicKeyKind: typeof encodedPair.publicKey,
				encodedPrivateKeyKind: typeof encodedPair.privateKey,
				encodedPublicKeyHeader: encodedPair.publicKey.split("\\n")[0],
				encodedPrivateKeyHeader: encodedPair.privateKey.split("\\n")[0],
				generatedAsyncPair,
				oaepPlaintext,
				pkcs1Plaintext,
				keyTypes: [privateKeyObject.type, publicKeyObject.type],
				constants: [crypto.constants.RSA_PKCS1_PADDING, crypto.constants.RSA_PKCS1_OAEP_PADDING],
			}));
			})();
		`,
	);

	expect(result.result.code, result.result.errorMessage ?? "").toBe(0);
	expect(JSON.parse(getLastStdioMessage(result, "stdout"))).toEqual({
		signature: cryptoBasicFixture.rsa.sha256SignatureHex,
		verified: true,
		expectedVerified: true,
		tampered: false,
		oneShotSignature: cryptoBasicFixture.rsa.sha256SignatureHex,
		oneShotVerified: true,
		oneShotTampered: false,
		generatedKeyTypes: ["public", "rsa", "private", "rsa"],
		generatedPrivatePemHeader: "-----BEGIN PRIVATE KEY-----",
		generatedPublicPemHeader: "-----BEGIN PUBLIC KEY-----",
		generatedVerified: true,
		encodedPublicKeyKind: "string",
		encodedPrivateKeyKind: "string",
		encodedPublicKeyHeader: "-----BEGIN PUBLIC KEY-----",
		encodedPrivateKeyHeader: "-----BEGIN PRIVATE KEY-----",
		generatedAsyncPair: {
			publicType: "public",
			privateType: "private",
			publicAsymmetricKeyType: "rsa",
			privateAsymmetricKeyType: "rsa",
		},
		oaepPlaintext: "agentos-rsa-oaep",
		pkcs1Plaintext: "agentos-rsa-pkcs1",
		keyTypes: ["private", "public"],
		constants: [1, 4],
	});
});

test("fails loud on RSA-PSS signatures instead of silently downgrading to PKCS1", async ({
	page,
}) => {
	const { runtimeId } = await createRuntime(page);
	const result = await execRuntime(
		page,
		runtimeId,
		`
			const crypto = require("node:crypto");
			const rsaFixture = ${JSON.stringify(cryptoBasicFixture.rsa)};
			const privateKeyObject = crypto.createPrivateKey(rsaFixture.privatePem);
			const publicKeyObject = crypto.createPublicKey(rsaFixture.publicPem);
			const grab = (fn) => { try { fn(); return null; } catch (e) { return e.code ?? e.message; } };
			// The browser backend only does PKCS#1 v1.5. A PSS request must throw a
			// clear unsupported error, never silently produce a PKCS1 signature.
			const signPss = grab(() =>
				crypto.createSign("sha256").update(rsaFixture.message).sign({
					key: privateKeyObject,
					padding: crypto.constants.RSA_PKCS1_PSS_PADDING ?? 6,
				}, "hex"),
			);
			const verifyPss = grab(() =>
				crypto.createVerify("sha256").update(rsaFixture.message).verify({
					key: publicKeyObject,
					padding: crypto.constants.RSA_PKCS1_PSS_PADDING ?? 6,
				}, "00", "hex"),
			);
			// PKCS1 (the supported padding) still works.
			const pkcs1Ok = typeof crypto.createSign("sha256").update(rsaFixture.message).sign({
				key: privateKeyObject,
				padding: crypto.constants.RSA_PKCS1_PADDING,
			}, "hex") === "string";
			console.log(JSON.stringify({ signPss, verifyPss, pkcs1Ok }));
		`,
	);
	expect(result.result.code, result.result.errorMessage ?? "").toBe(0);
	expect(JSON.parse(getLastStdioMessage(result, "stdout"))).toEqual({
		signPss: "ERR_UNSUPPORTED_BROWSER_CRYPTO",
		verifyPss: "ERR_UNSUPPORTED_BROWSER_CRYPTO",
		pkcs1Ok: true,
	});
});

test("applies frozen time by default and restores live timing when disabled", async ({
	page,
}) => {
	const { runtimeId } = await createRuntime(page);

	const frozen = await execRuntime(
		page,
		runtimeId,
		`
			console.log(JSON.stringify({
				firstDate: Date.now(),
				secondDate: Date.now(),
				firstPerformance: performance.now(),
				secondPerformance: performance.now(),
				frozenDate: new Date().getTime(),
				sharedType: typeof SharedArrayBuffer,
			}));
		`,
	);

	const frozenValues = JSON.parse(getLastStdioMessage(frozen, "stdout")) as {
		firstDate: number;
		secondDate: number;
		firstPerformance: number;
		secondPerformance: number;
		frozenDate: number;
		sharedType: string;
	};
	expect(frozen.result.code).toBe(0);
	expect(frozenValues.firstDate).toBe(frozenValues.secondDate);
	expect(frozenValues.frozenDate).toBe(frozenValues.firstDate);
	expect(frozenValues.firstPerformance).toBe(0);
	expect(frozenValues.secondPerformance).toBe(0);
	expect(frozenValues.sharedType).toBe("undefined");

	const restored = await execRuntime(
		page,
		runtimeId,
		`
			(async () => {
				const startDate = Date.now();
				const startPerformance = performance.now();
				await new Promise((resolve) => setTimeout(resolve, 25));
				const endDate = Date.now();
				const endPerformance = performance.now();
				console.log(JSON.stringify({
					startDate,
					endDate,
					startPerformance,
					endPerformance,
					sharedType: typeof SharedArrayBuffer,
				}));
			})();
		`,
		{
			timingMitigation: "off",
		},
	);

	const restoredValues = JSON.parse(
		getLastStdioMessage(restored, "stdout"),
	) as {
		startDate: number;
		endDate: number;
		startPerformance: number;
		endPerformance: number;
		sharedType: string;
	};
	expect(restored.result.code).toBe(0);
	expect(restoredValues.endDate).toBeGreaterThan(restoredValues.startDate);
	expect(restoredValues.endPerformance).toBeGreaterThan(
		restoredValues.startPerformance,
	);
	expect(restoredValues.sharedType).not.toBe("undefined");
});

test("rejects forged guest control traffic and keeps the runtime usable", async ({
	page,
}) => {
	const { runtimeId } = await createRuntime(page);

	const forgedMessageAttempt = await execRuntime(
		page,
		runtimeId,
		`
			(async () => {
				const rawPostMessageType = typeof _realPostMessage;
				await self.onmessage({
					data: {
						id: 999,
						type: "dispose",
					},
				});
				console.log(JSON.stringify({
					rawPostMessageType,
					onmessageType: typeof self.onmessage,
					stillRunning: true,
				}));
			})();
		`,
	);

	expect(forgedMessageAttempt.result.code).toBe(0);
	expect(
		JSON.parse(getLastStdioMessage(forgedMessageAttempt, "stdout")),
	).toEqual({
		rawPostMessageType: "undefined",
		onmessageType: "function",
		stillRunning: true,
	});

	const followUp = await execRuntime(
		page,
		runtimeId,
		`console.log("second-pass");`,
	);
	expect(followUp.result.code).toBe(0);
	expect(getLastStdioMessage(followUp, "stdout")).toBe("second-pass\n");
});

test("hard termination rejects pending work and clears sync bridge state", async ({
	page,
}) => {
	const { runtimeId } = await createRuntime(page);

	const warmup = await execRuntime(page, runtimeId, `console.log("warmup");`);
	expect(warmup.result.code).toBe(0);
	expect(getLastStdioMessage(warmup, "stdout")).toBe("warmup\n");

	const terminated = await terminatePendingExec(
		page,
		runtimeId,
		`
			(async () => {
				await new Promise(() => undefined);
			})();
		`,
	);

	expect(terminated.outcome).toBe("rejected");
	expect(terminated.errorMessage).toContain("disposed");
	expect(terminated.debug.disposed).toBe(true);
	expect(terminated.debug.pendingCount).toBe(0);
	expect(terminated.debug.signalState).toEqual([0, 0, 0, 0]);
	expect(terminated.debug.workerOnmessage).toBe("null");
	expect(terminated.debug.workerOnerror).toBe("null");
});

test("signals resolve pending browser execution with default signal exit codes", async ({
	page,
}) => {
	const { runtimeId } = await createRuntime(page);

	for (const [signal, exitCode] of [
		[15, 143],
		[9, 137],
	]) {
		const terminated = await signalPendingExec(
			page,
			runtimeId,
			`
				(async () => {
					await new Promise(() => undefined);
				})();
			`,
			signal,
		);

		expect(terminated.signaled).toBe(true);
		expect(terminated.outcome).toBe("resolved");
		expect(terminated.resultCode).toBe(exitCode);
		expect(terminated.errorMessage).toBeNull();
		expect(terminated.debug.disposed).toBe(false);
		expect(terminated.debug.pendingCount).toBe(0);
	}
});
