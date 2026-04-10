import { describe, expect, test } from "vitest";
import {
	createInMemoryLayerStore,
	createSnapshotExport,
} from "../src/index.js";

describe("layer store", () => {
	test("sealed writable layers become reusable read-only snapshots", async () => {
		const store = createInMemoryLayerStore();
		const upper = await store.createWritableLayer();
		const overlay = store.createOverlayFilesystem({
			upper,
			lowers: [],
		});

		await overlay.mkdir("/data", { recursive: true });
		await overlay.writeFile("/data/note.txt", "hello from layer");

		const snapshot = await store.sealLayer(upper);
		const reopened = await store.openSnapshotLayer(snapshot.layerId);
		const readOnlyOverlay = store.createOverlayFilesystem({
			mode: "read-only",
			lowers: [reopened],
		});

		expect(await readOnlyOverlay.readTextFile("/data/note.txt")).toBe(
			"hello from layer",
		);
		expect(() =>
			store.createOverlayFilesystem({
				upper,
				lowers: [],
			}),
		).toThrow("no longer valid");
	});

	test("imported snapshots compose with a sealed upper snapshot across multiple lowers", async () => {
		const store = createInMemoryLayerStore();
		const lower = await store.importSnapshot(
			createSnapshotExport([
				{
					path: "/",
					type: "directory",
					mode: "0755",
					uid: 0,
					gid: 0,
				},
				{
					path: "/workspace",
					type: "directory",
					mode: "0755",
					uid: 0,
					gid: 0,
				},
				{
					path: "/workspace/shared.txt",
					type: "file",
					mode: "0644",
					uid: 0,
					gid: 0,
					content: "lower",
				},
				{
					path: "/workspace/lower-only.txt",
					type: "file",
					mode: "0644",
					uid: 0,
					gid: 0,
					content: "lower-only",
				},
			]),
		);
		const higher = await store.importSnapshot(
			createSnapshotExport([
				{
					path: "/",
					type: "directory",
					mode: "0755",
					uid: 0,
					gid: 0,
				},
				{
					path: "/workspace",
					type: "directory",
					mode: "0755",
					uid: 0,
					gid: 0,
				},
				{
					path: "/workspace/shared.txt",
					type: "file",
					mode: "0644",
					uid: 0,
					gid: 0,
					content: "higher",
				},
				{
					path: "/workspace/higher-only.txt",
					type: "file",
					mode: "0644",
					uid: 0,
					gid: 0,
					content: "higher-only",
				},
			]),
		);

		const upper = await store.createWritableLayer();
		const writableOverlay = store.createOverlayFilesystem({
			upper,
			lowers: [higher, lower],
		});

		await writableOverlay.writeFile("/workspace/shared.txt", "upper");
		await writableOverlay.writeFile("/workspace/upper-only.txt", "upper-only");

		const sealedUpper = await store.sealLayer(upper);
		const reopenedUpper = await store.openSnapshotLayer(sealedUpper.layerId);
		const readOnlyOverlay = store.createOverlayFilesystem({
			mode: "read-only",
			lowers: [reopenedUpper, higher, lower],
		});

		expect(await readOnlyOverlay.readTextFile("/workspace/shared.txt")).toBe(
			"upper",
		);
		expect(
			await readOnlyOverlay.readTextFile("/workspace/higher-only.txt"),
		).toBe("higher-only");
		expect(await readOnlyOverlay.readTextFile("/workspace/lower-only.txt")).toBe(
			"lower-only",
		);
		expect(await readOnlyOverlay.readTextFile("/workspace/upper-only.txt")).toBe(
			"upper-only",
		);
	});
});
