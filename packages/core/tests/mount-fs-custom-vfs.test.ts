import { afterEach, describe, expect, test } from "vitest";
import {
	createInMemoryFileSystem,
	createKernel,
	type Kernel,
	type VirtualFileSystem,
} from "../src/test-runtime.js";

const VFS_METHODS = [
	"readFile",
	"readTextFile",
	"readDir",
	"readDirWithTypes",
	"writeFile",
	"createDir",
	"mkdir",
	"exists",
	"stat",
	"removeFile",
	"removeDir",
	"rename",
	"realpath",
	"symlink",
	"readlink",
	"lstat",
	"link",
	"chmod",
	"chown",
	"utimes",
	"truncate",
	"pread",
	"pwrite",
] as const;

function createRecordingFilesystem(): {
	fs: VirtualFileSystem;
	calls: string[];
} {
	const base = createInMemoryFileSystem();
	const calls: string[] = [];
	const delegates = base as unknown as Record<
		(typeof VFS_METHODS)[number],
		(...args: unknown[]) => unknown
	>;
	const fs = Object.fromEntries(
		VFS_METHODS.map((method) => [
			method,
			(...args: unknown[]) => {
				calls.push(`${method}:${String(args[0])}`);
				return delegates[method].apply(base, args);
			},
		]),
	) as unknown as VirtualFileSystem;

	return { fs, calls };
}

describe("Kernel.mountFs custom JS VFS", () => {
	let kernel: Kernel | undefined;

	afterEach(async () => {
		await kernel?.dispose();
		kernel = undefined;
	});

	test(
		"routes runtime reads and writes through a plain JS VFS object",
		async () => {
			const mounted = createRecordingFilesystem();
			kernel = createKernel({ filesystem: createInMemoryFileSystem() });

			kernel.mountFs("/mnt/custom", mounted.fs);
			await kernel.writeFile("/mnt/custom/note.txt", "from custom vfs");

			expect(
				new TextDecoder().decode(await kernel.readFile("/mnt/custom/note.txt")),
			).toBe("from custom vfs");
			expect(mounted.calls).toContain("writeFile:/note.txt");
			expect(mounted.calls).toContain("readFile:/note.txt");

			kernel.unmountFs("/mnt/custom");
			await expect(kernel.readFile("/mnt/custom/note.txt")).rejects.toThrow();
		},
		120_000,
	);

	test("routes positioned writes through a mounted JS VFS", async () => {
		const mounted = createRecordingFilesystem();
		kernel = createKernel({ filesystem: createInMemoryFileSystem() });

		kernel.mountFs("/mnt/custom", mounted.fs);
		await kernel.writeFile("/mnt/custom/db.bin", "abcde");
		await kernel.pwrite(
			"/mnt/custom/db.bin",
			2,
			new TextEncoder().encode("XYZ"),
		);

		expect(
			new TextDecoder().decode(await kernel.readFile("/mnt/custom/db.bin")),
		).toBe("abXYZ");
		expect(mounted.calls).toContain("pwrite:/db.bin");
	});
});
