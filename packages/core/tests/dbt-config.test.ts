import { existsSync, readdirSync } from "node:fs";
import { resolve } from "node:path";
import { allowAll } from "@secure-exec/core";
import { describe, expect, it } from "vitest";
import { AgentOs } from "../src/index.js";

// These tests cover the AgentOs `python.dbt` opt-in surface.
//
// We intentionally do NOT spin up a full kernel here — the L7 smoke test
// covers that. These tests lock down option resolution + the failure mode
// when the wheels package is empty.

const wheelsHostDir = resolve(
	import.meta.dirname,
	"../../../registry/software/python-wheels/wheels",
);

function wheelCount(): number {
	if (!existsSync(wheelsHostDir)) return 0;
	return readdirSync(wheelsHostDir).filter((f) => f.endsWith(".whl")).length;
}

describe("AgentOs.create — python.dbt option", () => {
	it("does nothing when python.dbt is omitted", async () => {
		const aos = await AgentOs.create({
			permissions: allowAll,
		});
		expect(aos).toBeDefined();
		await aos.dispose();
	});

	it("does nothing when python.dbt is explicitly false", async () => {
		const aos = await AgentOs.create({
			permissions: allowAll,
			python: { dbt: false },
		});
		expect(aos).toBeDefined();
		await aos.dispose();
	});

	it.skipIf(wheelCount() > 0)(
		"throws a clear error when wheels package is empty",
		async () => {
			await expect(
				AgentOs.create({
					permissions: allowAll,
					python: { dbt: true },
				}),
			).rejects.toThrow(/python\.dbt is enabled but/);
		},
	);

	it.skipIf(wheelCount() === 0)(
		"throws a clear error when wheels package isn't installed",
		async () => {
			// When wheels are present we can still validate the missing-package
			// branch by pointing at a name that won't resolve.
			await expect(
				AgentOs.create({
					permissions: allowAll,
					python: {
						dbt: { wheelsPackage: "@rivet-dev/agent-os-not-a-real-package" },
					},
				}),
			).rejects.toThrow(/wheel package "@rivet-dev\/agent-os-not-a-real-package"/);
		},
	);
});
