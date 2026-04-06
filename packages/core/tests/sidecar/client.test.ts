import { describe, expect, it } from "vitest";
import {
	AgentOsSidecarClient,
	type AgentOsSidecarSessionBootstrap,
	type AgentOsSidecarVmBootstrap,
} from "../../src/sidecar/client.js";

describe("AgentOsSidecarClient", () => {
	it("tracks sidecar session and VM lifecycle through a mock transport", async () => {
		const calls: Array<
			| { type: "session"; bootstrap: AgentOsSidecarSessionBootstrap }
			| { type: "vm"; bootstrap: AgentOsSidecarVmBootstrap }
			| { type: "dispose-vm"; vmId: string }
			| { type: "dispose-session" }
		> = [];
		let tick = 100;
		let nextId = 0;
		const client = new AgentOsSidecarClient({
			createId: () => `id-${++nextId}`,
			now: () => ++tick,
			async createSessionTransport(bootstrap) {
				calls.push({ type: "session", bootstrap });
				return {
					async createVm(vmBootstrap) {
						calls.push({ type: "vm", bootstrap: vmBootstrap });
					},
					async disposeVm(vmId) {
						calls.push({ type: "dispose-vm", vmId });
					},
					async dispose() {
						calls.push({ type: "dispose-session" });
					},
				};
			},
		});

		const session = await client.createSession({
			placement: { kind: "shared", pool: "default" },
			metadata: { owner: "core-test" },
		});
		expect(session.describe()).toMatchObject({
			sessionId: "id-1",
			state: "ready",
			placement: { kind: "shared", pool: "default" },
			metadata: { owner: "core-test" },
			vmIds: [],
		});

		const vm = await session.createVm({
			metadata: { runtime: "javascript" },
		});
		expect(vm.describe()).toMatchObject({
			vmId: "id-2",
			sessionId: "id-1",
			state: "ready",
			metadata: { runtime: "javascript" },
		});
		expect(session.listVms()).toEqual([vm.describe()]);
		expect(client.listSessions()).toEqual([session.describe()]);

		await vm.dispose();
		expect(vm.describe()).toMatchObject({
			vmId: "id-2",
			state: "disposed",
		});

		await session.dispose();
		expect(session.describe()).toMatchObject({
			sessionId: "id-1",
			state: "disposed",
		});

		expect(calls).toEqual([
			{
				type: "session",
				bootstrap: {
					sessionId: "id-1",
					placement: { kind: "shared", pool: "default" },
					metadata: { owner: "core-test" },
					signal: undefined,
				},
			},
			{
				type: "vm",
				bootstrap: {
					vmId: "id-2",
					sessionId: "id-1",
					metadata: { runtime: "javascript" },
				},
			},
			{
				type: "dispose-vm",
				vmId: "id-2",
			},
			{
				type: "dispose-session",
			},
		]);
	});

	it("disposes every tracked session when the client is torn down", async () => {
		const disposedSessions: string[] = [];
		const disposedVms: string[] = [];
		let nextId = 0;
		const client = new AgentOsSidecarClient({
			createId: () => `id-${++nextId}`,
			async createSessionTransport(bootstrap) {
				return {
					async createVm(vmBootstrap) {
						disposedSessions.push(`create:${bootstrap.sessionId}:${vmBootstrap.vmId}`);
					},
					async disposeVm(vmId) {
						disposedVms.push(vmId);
					},
					async dispose() {
						disposedSessions.push(`dispose:${bootstrap.sessionId}`);
					},
				};
			},
		});

		const first = await client.createSession();
		const second = await client.createSession({
			placement: { kind: "explicit", sidecarId: "shared-sidecar-2" },
		});
		const vm = await second.createVm();

		await client.dispose();

		expect(first.describe().state).toBe("disposed");
		expect(second.describe().state).toBe("disposed");
		expect(vm.describe().state).toBe("disposed");
		expect(disposedVms).toEqual(["id-3"]);
		expect(disposedSessions).toEqual([
			"create:id-2:id-3",
			"dispose:id-1",
			"dispose:id-2",
		]);
	});
});
