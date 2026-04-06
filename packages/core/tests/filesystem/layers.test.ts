import { describe, expect, test } from "vitest";
import { createInMemoryLayerStore } from "../../src/index.js";

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
			})
		).toThrow("no longer valid");
	});
});
