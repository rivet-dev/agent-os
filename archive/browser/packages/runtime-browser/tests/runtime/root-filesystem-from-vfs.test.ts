import { describe, expect, it } from "vitest";
import { createInMemoryFileSystem } from "../../src/runtime.js";
import {
	collectRootFilesystemEntries,
	rootFilesystemConfigFromVfs,
} from "../../src/root-filesystem-from-vfs.js";

async function seeded() {
	const fs = createInMemoryFileSystem();
	await fs.mkdir("/app", { recursive: true });
	await fs.writeFile("/app/index.js", "console.log('hi')");
	await fs.mkdir("/app/data", { recursive: true });
	await fs.writeFile("/app/data/blob.bin", new Uint8Array([0, 1, 2, 253]));
	return fs;
}

describe("root filesystem from vfs", () => {
	it("snapshots files and directories into bootstrap entries", async () => {
		const entries = await collectRootFilesystemEntries(await seeded());
		const byPath = new Map(entries.map((entry) => [entry.path, entry]));

		expect(byPath.get("/app")).toMatchObject({ kind: "directory" });
		expect(byPath.get("/app/data")).toMatchObject({ kind: "directory" });
		expect(byPath.get("/app/index.js")).toMatchObject({
			kind: "file",
			content: "console.log('hi')",
			encoding: "utf8",
		});
		// Non-utf8 content falls back to base64.
		const blob = byPath.get("/app/data/blob.bin");
		expect(blob?.kind).toBe("file");
		expect(blob?.encoding).toBe("base64");
		expect(blob?.content).toBe("AAEC/Q==");
	});

	it("produces an ephemeral writable RootFilesystemConfig", async () => {
		const config = await rootFilesystemConfigFromVfs(await seeded());
		expect(config.mode).toBe("ephemeral");
		expect(config.disableDefaultBaseLayer).toBe(false);
		expect(config.lowers).toEqual([]);
		expect(config.bootstrapEntries.length).toBeGreaterThan(0);
	});

	it("skips kernel pseudo-filesystems", async () => {
		const fs = createInMemoryFileSystem();
		await fs.mkdir("/dev", { recursive: true });
		await fs.writeFile("/dev/fake", "x");
		await fs.mkdir("/app", { recursive: true });
		const entries = await collectRootFilesystemEntries(fs);
		expect(entries.some((entry) => entry.path.startsWith("/dev"))).toBe(false);
		expect(entries.some((entry) => entry.path === "/app")).toBe(true);
	});
});
