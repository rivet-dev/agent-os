import { strict as assert } from "node:assert";
import { createServer } from "node:http";
import { describe, it } from "node:test";
import { readLoadConfig, runLoadTest } from "./load.js";

describe("AgentOS Apps load driver", () => {
	it("rejects an impossible success-rate gate", () => {
		assert.throws(
			() => readLoadConfig({ LOAD_TEST_MIN_SUCCESS_RATE: "1.1" }),
			/LOAD_TEST_MIN_SUCCESS_RATE must be a number between 0 and 1/,
		);
	});

	it("records cold and warm latency with a hard request bound", async () => {
		let requestCount = 0;
		const server = createServer((_request, response) => {
			requestCount += 1;
			response.setHeader(
				"x-agentos-app-cold-start",
				requestCount % 2 === 1 ? "1" : "0",
			);
			response.setHeader("x-agentos-app-replica", `replica-${requestCount % 2}`);
			response.setHeader("x-agentos-app-replica-count", "2");
			response.setHeader("x-agentos-app-queue-delay-ms", "3");
			response.end("hello");
		});
		await new Promise<void>((resolve) =>
			server.listen(0, "127.0.0.1", resolve),
		);

		try {
			const address = server.address();
			assert(address && typeof address !== "string");
			const result = await runLoadTest({
				...readLoadConfig({}),
				target: `http://127.0.0.1:${address.port}`,
				concurrency: 2,
				durationSeconds: 1,
				maxRequests: 4,
			});

			assert.equal(result.completed, 4);
			assert.equal(result.successRate, 1);
			assert.equal(result.coldStarts, 2);
			assert.equal(result.warmRequests, 2);
			assert.equal(result.unclassifiedRequests, 0);
			assert.equal(result.warmHitRate, 0.5);
			assert.equal(result.replicaHeaderCoverage, 1);
			assert.equal(result.maximumReplicaCount, 2);
			assert.equal(result.stoppedBy, "request-limit");
			assert(result.coldLatencyMs.p50 > 0);
			assert(result.warmLatencyMs.p50 > 0);
		} finally {
			await new Promise<void>((resolve, reject) =>
				server.close((error) => (error ? reject(error) : resolve())),
			);
		}
	});

	it("fails a request instead of buffering an oversized response", async () => {
		const result = await runLoadTest(
			{
				...readLoadConfig({}),
				target: "http://load.test",
				concurrency: 1,
				durationSeconds: 1,
				maxRequests: 1,
				maxResponseBytes: 4,
			},
			async () => new Response("too large"),
		);

		assert.equal(result.completed, 1);
		assert.equal(result.successRate, 0);
		assert.deepEqual(result.statuses, { ResponseBodyLimitError: 1 });
	});
});
