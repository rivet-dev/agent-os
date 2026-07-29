import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { AgentOs } from "../src/index.js";
import { ALLOW_ALL_VM_PERMISSIONS } from "./helpers/permissions.js";

function normalizeLifecycleError(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}

function isExpectedTeardownError(message: string): boolean {
	const normalized = message.toLowerCase();
	return (
		normalized.includes("unknown sidecar vm") ||
		normalized.includes("already been disposed") ||
		normalized.includes("native sidecar disposed") ||
		normalized.includes("cannot dispatch request on closed native sidecar process")
	);
}

describe("process lifecycle teardown races", () => {
	let vm: AgentOs | undefined;

	beforeEach(async () => {
		vm = await AgentOs.create({ permissions: ALLOW_ALL_VM_PERMISSIONS });
	});

	afterEach(async () => {
		await vm?.dispose();
		vm = undefined;
	});

	test(
		"filesystem calls racing vm.dispose() settle without transport crashes",
		async () => {
			if (!vm) {
				throw new Error("vm should be created before test execution");
			}
			await vm.writeFile("/tmp/hold-open.mjs", "setInterval(() => {}, 1_000);");
			await vm.writeFile("/tmp/seed.txt", "seed");

			vm.spawn("node", ["/tmp/hold-open.mjs"], {
				env: { HOME: "/home/agentos" },
			});

			const operations = Array.from({ length: 12 }, (_, index) =>
				(index % 3 === 0
					? vm.writeFile(`/tmp/race-${index}.txt`, `payload-${index}`)
					: index % 3 === 1
						? vm.readFile("/tmp/seed.txt")
						: vm.stat("/tmp/seed.txt")
				).then(
					(value) => ({ ok: true as const, value }),
					(error) => ({
						ok: false as const,
						message: normalizeLifecycleError(error),
					}),
				),
			);

			const disposePromise = vm.dispose();
			const results = await Promise.all(operations);
			await disposePromise;

			for (const result of results) {
				if (result.ok) {
					continue;
				}
				expect(
					isExpectedTeardownError(result.message),
					`unexpected teardown error: ${result.message}`,
				).toBe(true);
			}

			await expect(vm.readFile("/tmp/seed.txt")).rejects.toSatisfy((error) =>
				isExpectedTeardownError(normalizeLifecycleError(error)),
			);
		},
		30_000,
	);
});
