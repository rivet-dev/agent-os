import { describe, expect, test } from "vitest";
import { AgentOs } from "../src/index.js";

describe("AgentOs sidecar placement", () => {
	test("reuses shared sidecar handles and defaults AgentOs.create() to the shared pool", async () => {
		const shared = await AgentOs.getSharedSidecar();
		const sameShared = await AgentOs.getSharedSidecar();
		const otherPool = await AgentOs.getSharedSidecar({ pool: "integration" });
		expect(shared).toBe(sameShared);
		expect(otherPool).not.toBe(shared);

		const vm = await AgentOs.create();
		try {
			expect(vm.sidecar).toBe(shared);
			expect(vm.sidecar.describe()).toMatchObject({
				placement: { kind: "shared", pool: "default" },
				state: "ready",
				activeVmCount: 1,
			});
			expect("kernel" in vm).toBe(false);
			expect((vm as Record<string, unknown>).kernel).toBeUndefined();
		} finally {
			await vm.dispose();
			await otherPool.dispose();
			await shared.dispose();
		}
	});

	test("accepts explicit sidecar handle injection", async () => {
		const sidecar = await AgentOs.createSidecar({
			sidecarId: "agent-os-explicit-test-sidecar",
		});
		const vm = await AgentOs.create({
			sidecar: {
				kind: "explicit",
				handle: sidecar,
			},
		});

		try {
			expect(vm.sidecar).toBe(sidecar);
			expect(sidecar.describe()).toMatchObject({
				sidecarId: "agent-os-explicit-test-sidecar",
				placement: {
					kind: "explicit",
					sidecarId: "agent-os-explicit-test-sidecar",
				},
				state: "ready",
				activeVmCount: 1,
			});

			await vm.writeFile("/tmp/placement-check.txt", "ok");
			expect(new TextDecoder().decode(await vm.readFile("/tmp/placement-check.txt"))).toBe(
				"ok",
			);
		} finally {
			await vm.dispose();
			await sidecar.dispose();
		}

		expect(sidecar.describe().state).toBe("disposed");
	});

	test("can target a non-default shared sidecar pool through AgentOsOptions", async () => {
		const sidecar = await AgentOs.getSharedSidecar({ pool: "batch" });
		const vm = await AgentOs.create({
			sidecar: {
				kind: "shared",
				pool: "batch",
			},
		});

		try {
			expect(vm.sidecar).toBe(sidecar);
			expect(vm.sidecar.describe().placement).toEqual({
				kind: "shared",
				pool: "batch",
			});
		} finally {
			await vm.dispose();
			await sidecar.dispose();
		}
	});
});
