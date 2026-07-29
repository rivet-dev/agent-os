// Nightly: requires a non-core registry command.
/**
 * Package-owned integration tests for the unzip command.
 */

import { afterEach, expect, it } from "vitest";
import {
	C_BUILD_DIR,
	COMMANDS_DIR,
	createInMemoryFileSystem,
	createKernel,
	createWasmVmRuntime,
	describeIf,
	hasCWasmBinaries,
	type Kernel,
} from "@rivet-dev/agentos-test-harness";

interface HostileEntry {
	name: string;
	method: number;
	compressedSize: number;
	uncompressedSize: number;
	localOffset: number;
}

/** Builds a ZIP whose EOCD cd-size field is corrupt so unzip's raw central-directory fallback parser is exercised. */
function buildFallbackArchive(
	prefix: Uint8Array,
	entries: HostileEntry[],
): Uint8Array {
	const enc = new TextEncoder();
	const cdParts: Uint8Array[] = [];
	for (const entry of entries) {
		const nameBytes = enc.encode(entry.name);
		const cd = new Uint8Array(46 + nameBytes.length);
		const dv = new DataView(cd.buffer);
		dv.setUint32(0, 0x02014b50, true);
		dv.setUint16(4, 20, true);
		dv.setUint16(6, 20, true);
		dv.setUint16(10, entry.method, true);
		dv.setUint32(20, entry.compressedSize, true);
		dv.setUint32(24, entry.uncompressedSize, true);
		dv.setUint16(28, nameBytes.length, true);
		dv.setUint32(42, entry.localOffset, true);
		cd.set(nameBytes, 46);
		cdParts.push(cd);
	}
	const cdOffset = prefix.length;
	const cdLen = cdParts.reduce((n, part) => n + part.length, 0);
	const eocd = new Uint8Array(22);
	const dv = new DataView(eocd.buffer);
	dv.setUint32(0, 0x06054b50, true);
	dv.setUint16(8, entries.length, true);
	dv.setUint16(10, entries.length, true);
	dv.setUint32(12, 0xffffffff, true);
	dv.setUint32(16, cdOffset, true);
	const out = new Uint8Array(prefix.length + cdLen + 22);
	out.set(prefix, 0);
	let offset = cdOffset;
	for (const part of cdParts) {
		out.set(part, offset);
		offset += part.length;
	}
	out.set(eocd, offset);
	return out;
}

describeIf(
	hasCWasmBinaries("zip", "unzip"),
	"unzip command",
	{ timeout: 10_000 },
	() => {
		let kernel: Kernel;

		afterEach(async () => {
			await kernel?.dispose();
		});

		it("extracts a zip archive into a target directory", async () => {
			const vfs = createInMemoryFileSystem();
			await vfs.writeFile("/hello.txt", "Hello, World!\n");

			kernel = createKernel({ filesystem: vfs });
			await kernel.mount(
				createWasmVmRuntime({ commandDirs: [C_BUILD_DIR, COMMANDS_DIR] }),
			);

			const zipResult = await kernel.exec("zip /archive.zip /hello.txt");
			expect(zipResult.exitCode, zipResult.stderr).toBe(0);

			const unzipResult = await kernel.exec("unzip -d /extracted /archive.zip");
			expect(unzipResult.exitCode, unzipResult.stderr).toBe(0);
			expect(await vfs.readTextFile("/extracted/hello.txt")).toBe(
				"Hello, World!\n",
			);
		});

		it("lists archive contents with sizes", async () => {
			const vfs = createInMemoryFileSystem();
			await vfs.writeFile("/data.txt", "some data content\n");

			kernel = createKernel({ filesystem: vfs });
			await kernel.mount(
				createWasmVmRuntime({ commandDirs: [C_BUILD_DIR, COMMANDS_DIR] }),
			);

			const zipResult = await kernel.exec("zip /list-test.zip /data.txt");
			expect(zipResult.exitCode, zipResult.stderr).toBe(0);

			const listResult = await kernel.exec("unzip -l /list-test.zip");
			expect(listResult.exitCode, listResult.stderr).toBe(0);
			expect(listResult.stdout).toContain("data.txt");
			expect(listResult.stdout).toContain("18");
			expect(listResult.stdout).toMatch(/1 file/);
		});

		it("extracts binary file contents exactly", async () => {
			const vfs = createInMemoryFileSystem();
			const content = new Uint8Array(256);
			for (let i = 0; i < 256; i++) content[i] = i;
			await vfs.writeFile("/binary.bin", content);

			kernel = createKernel({ filesystem: vfs });
			await kernel.mount(
				createWasmVmRuntime({ commandDirs: [C_BUILD_DIR, COMMANDS_DIR] }),
			);

			const zipResult = await kernel.exec("zip /roundtrip.zip /binary.bin");
			expect(zipResult.exitCode, zipResult.stderr).toBe(0);

			const unzipResult = await kernel.exec("unzip -d /rt-out /roundtrip.zip");
			expect(unzipResult.exitCode, unzipResult.stderr).toBe(0);

			const extracted = await vfs.readFile("/rt-out/binary.bin");
			expect(extracted.length).toBe(256);
			for (let i = 0; i < 256; i++) {
				expect(extracted[i]).toBe(i);
			}
		});

		it("rejects an entry with a wrapping local offset", async () => {
			const vfs = createInMemoryFileSystem();
			const bytes = buildFallbackArchive(new Uint8Array(0), [
				{
					name: "evil.txt",
					method: 0,
					compressedSize: 4,
					uncompressedSize: 4,
					localOffset: 0xfffffff0,
				},
			]);
			await vfs.writeFile("/evil.zip", bytes);

			kernel = createKernel({ filesystem: vfs });
			await kernel.mount(
				createWasmVmRuntime({ commandDirs: [C_BUILD_DIR, COMMANDS_DIR] }),
			);

			const result = await kernel.exec("unzip -d /out /evil.zip");
			expect(result.exitCode, result.stderr).not.toBe(0);
			expect(result.stderr).toMatch(/error/);
			expect(await vfs.exists("/out/evil.txt")).toBe(false);
		});

		it("rejects an entry whose normalized name is empty", async () => {
			const vfs = createInMemoryFileSystem();
			const bytes = buildFallbackArchive(new Uint8Array(0), [
				{
					name: "/",
					method: 0,
					compressedSize: 0,
					uncompressedSize: 0,
					localOffset: 0,
				},
			]);
			await vfs.writeFile("/empty-name.zip", bytes);

			kernel = createKernel({ filesystem: vfs });
			await kernel.mount(
				createWasmVmRuntime({ commandDirs: [C_BUILD_DIR, COMMANDS_DIR] }),
			);

			const result = await kernel.exec("unzip /empty-name.zip");
			expect(result.exitCode, result.stderr).not.toBe(0);
			expect(result.stderr).toMatch(/error/);
		});

		it("rejects hostile uncompressed sizes before extracting", async () => {
			const vfs = createInMemoryFileSystem();
			const prefix = new Uint8Array(31);
			const pdv = new DataView(prefix.buffer);
			pdv.setUint32(0, 0x04034b50, true);
			pdv.setUint16(4, 20, true);
			pdv.setUint16(26, 0, true);
			pdv.setUint16(28, 0, true);
			prefix[30] = 0x41;
			const bytes = buildFallbackArchive(prefix, [
				{
					name: "big.bin",
					method: 0,
					compressedSize: 1,
					uncompressedSize: 0xffffffff,
					localOffset: 0,
				},
			]);
			await vfs.writeFile("/big.zip", bytes);

			kernel = createKernel({ filesystem: vfs });
			await kernel.mount(
				createWasmVmRuntime({ commandDirs: [C_BUILD_DIR, COMMANDS_DIR] }),
			);

			const result = await kernel.exec("unzip -d /cap-out /big.zip");
			expect(result.exitCode, result.stderr).not.toBe(0);
			expect(result.stderr).toMatch(/error/);
			expect(await vfs.exists("/cap-out/big.bin")).toBe(false);
		});
	},
);
