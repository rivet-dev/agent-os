import { describe, expect, it, vi } from "vitest";
import type { ConvergedSidecarRequestTransport } from "../../src/converged-sync-bridge-handler.js";
import { ConvergedSyncBridgeHandler } from "../../src/converged-sync-bridge-handler.js";
import { ConvergedSyncBridgeRouter } from "../../src/converged-sync-bridge-router.js";
import { BROWSER_SYNC_BRIDGE_OPERATIONS } from "../../src/sync-bridge.js";
import {
	SYNC_BRIDGE_KIND_JSON,
	SYNC_BRIDGE_KIND_TEXT,
} from "../../src/sync-bridge.js";

function fsTransport(): ConvergedSidecarRequestTransport {
	return {
		sendRequest(payload) {
			if (payload.type === "guest_filesystem_call") {
				return {
					type: "guest_filesystem_result",
					operation: payload.operation,
					path: payload.path,
					content: "from-wasm",
					encoding: "utf8",
				};
			}
			return { type: "rejected", code: "x", message: payload.type };
		},
	};
}

function makeHandler() {
	return new ConvergedSyncBridgeHandler({
		transport: fsTransport(),
		executionId: "exec-1",
	});
}

describe("converged sync bridge router", () => {
	it("routes converged ops to the wasm handler", async () => {
		const legacy = vi.fn();
		const router = new ConvergedSyncBridgeRouter({
			handler: makeHandler(),
			legacy,
		});
		const response = await router.route("fs.readFile", ["/x"]);
		expect(response).toEqual({
			kind: SYNC_BRIDGE_KIND_TEXT,
			value: "from-wasm",
		});
		expect(legacy).not.toHaveBeenCalled();
	});

	it("falls back to the legacy servicer for unconverged ops", async () => {
		const legacy = vi.fn(async () => ({
			kind: SYNC_BRIDGE_KIND_JSON as typeof SYNC_BRIDGE_KIND_JSON,
			value: { resolved: "/m.js" },
		}));
		const router = new ConvergedSyncBridgeRouter({
			handler: makeHandler(),
			legacy,
		});
		const response = await router.route("module.resolve", ["m", "/p"]);
		expect(response).toEqual({
			kind: SYNC_BRIDGE_KIND_JSON,
			value: { resolved: "/m.js" },
		});
		expect(legacy).toHaveBeenCalledWith("module.resolve", ["m", "/p"]);
	});

	it("knows it is not yet fully converged (module/child_process/dgram remain)", () => {
		expect(
			ConvergedSyncBridgeRouter.isFullyConverged(makeHandler(), [
				...BROWSER_SYNC_BRIDGE_OPERATIONS,
			]),
		).toBe(false);
		// fs subset IS fully converged.
		expect(
			ConvergedSyncBridgeRouter.isFullyConverged(makeHandler(), [
				"fs.readFile",
				"fs.writeFile",
				"fs.readDir",
				"fs.stat",
			]),
		).toBe(true);
	});
});
