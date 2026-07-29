import { describe, expect, test } from "vitest";
import { ForegroundPriorityGate } from "./foreground-priority-gate";
import {
	catalogFromConfigOptions,
	cleanGeneratedSessionTitle,
	isDefaultSessionTitle,
	reconcileSessionStreamDelta,
	type PendingStreamChunk,
} from "./gigacode";

function deferred() {
	let resolve!: () => void;
	const promise = new Promise<void>((complete) => {
		resolve = complete;
	});
	return { promise, resolve };
}

describe("ForegroundPriorityGate", () => {
	test("never overlaps foreground with probes and favors queued foreground", async () => {
		const gate = new ForegroundPriorityGate();
		const firstProbeStarted = deferred();
		const releaseFirstProbe = deferred();
		const foregroundStarted = deferred();
		const releaseForeground = deferred();
		const secondProbeStarted = deferred();
		const order: string[] = [];

		const firstProbe = gate.background(async () => {
			order.push("probe-1");
			firstProbeStarted.resolve();
			await releaseFirstProbe.promise;
		});
		await firstProbeStarted.promise;

		const foreground = gate.foreground(async () => {
			order.push("foreground");
			foregroundStarted.resolve();
			await releaseForeground.promise;
		});
		const secondProbe = gate.background(async () => {
			order.push("probe-2");
			secondProbeStarted.resolve();
		});

		expect(order).toEqual(["probe-1"]);
		releaseFirstProbe.resolve();
		await foregroundStarted.promise;
		expect(order).toEqual(["probe-1", "foreground"]);

		releaseForeground.resolve();
		await secondProbeStarted.promise;
		await Promise.all([firstProbe, foreground, secondProbe]);
		expect(order).toEqual(["probe-1", "foreground", "probe-2"]);
	});
});

describe("ACP model catalog", () => {
	test("collapses variant-qualified model choices into per-model variants", () => {
		const catalog = catalogFromConfigOptions([
			{
				id: "model",
				category: "model",
				type: "select",
				currentValue: "provider/alpha",
				options: [
					{ value: "provider/alpha", name: "Provider/Alpha" },
					{ value: "provider/alpha/low", name: "Provider/Alpha (Low)" },
					{ value: "provider/alpha/high", name: "Provider/Alpha (High)" },
					{ value: "provider/beta", name: "Provider/Beta" },
					{ value: "provider/beta/high", name: "Provider/Beta (High)" },
				],
			},
		]);

		expect(catalog?.models.map((model) => model.id)).toEqual([
			"provider/alpha",
			"provider/beta",
		]);
		expect(Object.keys(catalog?.models[0]?.variants ?? {})).toEqual([
			"low",
			"high",
		]);
		expect(Object.keys(catalog?.models[1]?.variants ?? {})).toEqual(["high"]);
		expect(catalog?.models[0]?.variants?.high).toMatchObject({
			configId: "model",
			value: "provider/alpha/high",
		});
	});
});

describe("ACP stream reconciliation", () => {
	test("does not append the durable aggregate after live ephemeral chunks", () => {
		const pending: PendingStreamChunk[] = [];
		expect(
			reconcileSessionStreamDelta(
				pending,
				{ durability: "ephemeral", afterSequence: 4 },
				{ messageId: "message-1" },
				"text",
				"hello ",
			),
		).toBe("hello ");
		expect(
			reconcileSessionStreamDelta(
				pending,
				{ durability: "ephemeral", afterSequence: 4 },
				{ messageId: "message-1" },
				"text",
				"world",
			),
		).toBe("world");
		expect(
			reconcileSessionStreamDelta(
				pending,
				{ durability: "durable", sequence: 5 },
				{ messageId: "message-1" },
				"text",
				"hello world",
			),
		).toBe("");
		expect(pending).toEqual([]);
	});

	test("emits only a durable suffix not seen live", () => {
		const pending: PendingStreamChunk[] = [
			{
				type: "text",
				messageId: "message-1",
				afterSequence: 8,
				text: "partial",
			},
		];
		expect(
			reconcileSessionStreamDelta(
				pending,
				{ durability: "durable", sequence: 9 },
				{ messageId: "message-1" },
				"text",
				"partial completion",
			),
		).toBe(" completion");
	});
});

describe("session title compatibility", () => {
	test("recognizes only native OpenCode placeholder titles", () => {
		expect(isDefaultSessionTitle("New session - 2026-07-20T12:34:56.789Z")).toBe(
			true,
		);
		expect(
			isDefaultSessionTitle("Child session - 2026-07-20T12:34:56.789Z"),
		).toBe(true);
		expect(isDefaultSessionTitle("New session - custom")).toBe(false);
		expect(isDefaultSessionTitle("Explicit user title")).toBe(false);
	});

	test("cleans title-agent output like native OpenCode", () => {
		expect(
			cleanGeneratedSessionTitle("<think>ignore this</think>\n  Fix ACP cancellation  \nextra"),
		).toBe("Fix ACP cancellation");
		expect(cleanGeneratedSessionTitle("\n \n")).toBeUndefined();
		expect(cleanGeneratedSessionTitle("x".repeat(110))).toBe(
			`${"x".repeat(97)}...`,
		);
	});
});
