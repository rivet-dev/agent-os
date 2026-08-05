import assert from "node:assert/strict";
import { describe, test } from "node:test";
import type { MatrixBaseline, MatrixBaselineRow } from "../src/baseline.js";
import { compareMatrixBaseline } from "../src/compare-baseline.js";

const OPTIONS = {
	threshold: 2,
	tinyBaselineFloorMs: 0.5,
	tinyAllowedRegressionMs: 1,
};

function row(p50Ms: number): MatrixBaselineRow {
	return {
		key: "fs/fs_read_small",
		family: "fs",
		op: "fs_read_small",
		lanes: { guest: { p50Ms } },
		tax: {},
	};
}

function compare(baselineP50Ms: number, currentP50Ms: number) {
	const baseline = {
		rows: [row(baselineP50Ms)],
	} as MatrixBaseline;
	return compareMatrixBaseline(
		[row(currentP50Ms)],
		baseline,
		[{ key: "fs/fs_read_small", lane: "guest" }],
		OPTIONS,
	)[0];
}

describe("compareMatrixBaseline", () => {
	test("ignores sub-millisecond absolute noise for tiny baselines", () => {
		const result = compare(0.35, 1.25);
		assert.equal(result.ratio, 3.57);
		assert.equal(result.deltaMs, 0.9);
		assert.equal(result.status, "ignored");
	});

	test("fails a material regression from a tiny baseline", () => {
		const result = compare(0.35, 1.4);
		assert.equal(result.ratio, 4);
		assert.equal(result.deltaMs, 1.05);
		assert.equal(result.status, "fail");
	});

	test("keeps ratio-only enforcement for normal baselines", () => {
		const result = compare(5, 10.1);
		assert.equal(result.ratio, 2.02);
		assert.equal(result.status, "fail");
	});
});
