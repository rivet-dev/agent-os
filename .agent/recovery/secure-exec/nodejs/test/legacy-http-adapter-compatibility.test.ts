import { describe, expect, it } from "vitest";
import { deserialize } from "node:v8";
import { SocketTable, type PermissionDecision } from "@secure-exec/core";
import { HOST_BRIDGE_GLOBAL_KEYS } from "../src/bridge-contract.ts";
import {
	buildNetworkBridgeHandlers,
	resolveHttpServerResponse,
} from "../src/bridge-handlers.ts";
import { createDefaultNetworkAdapter } from "../src/default-network-adapter.ts";
import { createNodeHostNetworkAdapter } from "../src/host-network-adapter.ts";
import { createBudgetState } from "../src/isolate-bootstrap.ts";

const allowAll = (): PermissionDecision => ({ allow: true });

class TrackingSocketTable extends SocketTable {
	connectCalls: Array<{ host: string; port: number }> = [];

	override async connect(socketId: number, addr: { host: string; port: number }): Promise<void> {
		this.connectCalls.push({ host: addr.host, port: addr.port });
		return await super.connect(socketId, addr);
	}
}

describe("legacy default-network adapter compatibility", () => {
	it("lets the legacy adapter reach a kernel-backed listener", async () => {
		const adapter = createDefaultNetworkAdapter();
		const socketTable = new SocketTable({
			hostAdapter: createNodeHostNetworkAdapter(),
			networkCheck: allowAll,
		});

		const result = buildNetworkBridgeHandlers({
			networkAdapter: adapter,
			budgetState: createBudgetState(),
			isolateJsonPayloadLimitBytes: 1024 * 1024,
			activeHttpServerIds: new Set(),
			activeHttpServerClosers: new Map(),
			pendingHttpServerStarts: { count: 0 },
			activeHttpClientRequests: { count: 0 },
			sendStreamEvent(eventType, payload) {
				if (eventType !== "http_request") return;
				const event = deserialize(Buffer.from(payload)) as {
					requestId: number;
					serverId: number;
				};
				resolveHttpServerResponse({
					requestId: event.requestId,
					serverId: event.serverId,
					responseJson: JSON.stringify({
						status: 200,
						headers: [["content-type", "text/plain"]],
						body: "bridge-ok",
						bodyEncoding: "utf8",
					}),
				});
			},
			socketTable,
			pid: 1,
		});

		const listenRaw = result.handlers[HOST_BRIDGE_GLOBAL_KEYS.networkHttpServerListenRaw];
		const closeRaw = result.handlers[HOST_BRIDGE_GLOBAL_KEYS.networkHttpServerCloseRaw];
		const listenResult = await Promise.resolve(
			listenRaw(JSON.stringify({ serverId: 1, hostname: "127.0.0.1", port: 0 })),
		);
		const { address } = JSON.parse(String(listenResult)) as {
			address: { address: string; port: number } | null;
		};

		if (!address) {
			throw new Error("expected kernel listener address");
		}

		try {
			const httpResponse = await Promise.race([
				adapter.httpRequest(`http://127.0.0.1:${address.port}/`, { method: "GET" }),
				new Promise<never>((_, reject) =>
					setTimeout(() => reject(new Error("httpRequest timed out")), 1000),
				),
			]);

			expect(httpResponse.status).toBe(200);
			expect(httpResponse.body).toBe("bridge-ok");

			const fetchResponse = await Promise.race([
				adapter.fetch(`http://127.0.0.1:${address.port}/`, { method: "GET" }),
				new Promise<never>((_, reject) =>
					setTimeout(() => reject(new Error("fetch timed out")), 1000),
				),
			]);

			expect(fetchResponse.status).toBe(200);
			expect(fetchResponse.body).toBe("bridge-ok");
		} finally {
			await Promise.resolve(closeRaw(1));
			await result.dispose();
		}
	});

	it("keeps loopback fetch/httpRequest routing on kernel sockets even when a legacy adapter is injected", async () => {
		const adapter = createDefaultNetworkAdapter() as ReturnType<typeof createDefaultNetworkAdapter> & {
			fetch: typeof createDefaultNetworkAdapter extends (...args: any[]) => infer T
				? T["fetch"]
				: never;
			httpRequest: typeof createDefaultNetworkAdapter extends (...args: any[]) => infer T
				? T["httpRequest"]
				: never;
		};
		adapter.fetch = async () => {
			throw new Error("legacy fetch adapter path used");
		};
		adapter.httpRequest = async () => {
			throw new Error("legacy httpRequest adapter path used");
		};

		const socketTable = new TrackingSocketTable({
			hostAdapter: createNodeHostNetworkAdapter(),
			networkCheck: allowAll,
		});

		const result = buildNetworkBridgeHandlers({
			networkAdapter: adapter,
			budgetState: createBudgetState(),
			isolateJsonPayloadLimitBytes: 1024 * 1024,
			activeHttpServerIds: new Set(),
			activeHttpServerClosers: new Map(),
			pendingHttpServerStarts: { count: 0 },
			activeHttpClientRequests: { count: 0 },
			sendStreamEvent(eventType, payload) {
				if (eventType !== "http_request") return;
				const event = deserialize(Buffer.from(payload)) as {
					requestId: number;
					serverId: number;
				};
				resolveHttpServerResponse({
					requestId: event.requestId,
					serverId: event.serverId,
					responseJson: JSON.stringify({
						status: 200,
						headers: [["content-type", "text/plain"]],
						body: "kernel-routed",
						bodyEncoding: "utf8",
					}),
				});
			},
			socketTable,
			pid: 1,
		});

		const listenRaw = result.handlers[HOST_BRIDGE_GLOBAL_KEYS.networkHttpServerListenRaw];
		const fetchRaw = result.handlers[HOST_BRIDGE_GLOBAL_KEYS.networkFetchRaw];
		const httpRequestRaw = result.handlers[HOST_BRIDGE_GLOBAL_KEYS.networkHttpRequestRaw];
		const closeRaw = result.handlers[HOST_BRIDGE_GLOBAL_KEYS.networkHttpServerCloseRaw];
		const listenResult = await Promise.resolve(
			listenRaw(JSON.stringify({ serverId: 2, hostname: "127.0.0.1", port: 0 })),
		);
		const { address } = JSON.parse(String(listenResult)) as {
			address: { address: string; port: number } | null;
		};

		if (!address) {
			throw new Error("expected kernel listener address");
		}

		try {
			const url = `http://127.0.0.1:${address.port}/kernel-client`;

			const fetchResponse = JSON.parse(String(await Promise.resolve(
				fetchRaw(url, JSON.stringify({ method: "GET", headers: {}, body: null })),
			))) as {
				status: number;
				body: string;
			};
			expect(fetchResponse.status).toBe(200);
			expect(fetchResponse.body).toBe("kernel-routed");

			const httpResponse = JSON.parse(String(await Promise.resolve(
				httpRequestRaw(url, JSON.stringify({ method: "GET", headers: {}, body: null })),
			))) as {
				status: number;
				body: string;
			};
			expect(httpResponse.status).toBe(200);
			expect(httpResponse.body).toBe("kernel-routed");
			expect(socketTable.connectCalls).toEqual(
				expect.arrayContaining([
					{ host: "127.0.0.1", port: address.port },
				]),
			);
		} finally {
			await Promise.resolve(closeRaw(2));
			await result.dispose();
		}
	});
});
