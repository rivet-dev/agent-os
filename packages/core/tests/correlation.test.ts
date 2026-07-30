import { describe, expect, test } from "vitest";
import { PendingResponseRegistry } from "../src/correlation.js";

describe("pending response registry", () => {
	test("resolves a registered response by request id", async () => {
		const registry = new PendingResponseRegistry<string>();
		const response = registry.waitForResponse(7);

		expect(registry.resolve(7, "ok")).toBe(true);
		await expect(response).resolves.toBe("ok");
		expect(registry.resolve(7, "late")).toBe(false);
	});

	test("rejects a registered response by request id", async () => {
		const registry = new PendingResponseRegistry<string>();
		const response = registry.waitForResponse(9);
		const error = new Error("write failed");

		expect(registry.reject(9, error)).toBe(true);
		await expect(response).rejects.toThrow("write failed");
		expect(registry.reject(9, error)).toBe(false);
	});

	test("pending responses have no deadline of their own", async () => {
		// A response is bounded by the transport silence watchdog (via
		// `rejectAll`), never by a per-request timer: a legitimately long
		// request must be able to outlive any fixed per-request window.
		const registry = new PendingResponseRegistry<string>();
		const response = registry.waitForResponse(11);

		await new Promise((resolve) => setTimeout(resolve, 50));
		expect(registry.resolve(11, "slow but fine")).toBe(true);
		await expect(response).resolves.toBe("slow but fine");
	});

	test("rejects duplicate request ids", () => {
		const registry = new PendingResponseRegistry<string>();
		const pending = registry.waitForResponse(13);
		void pending.catch(() => undefined);

		expect(() => registry.waitForResponse(13)).toThrow(
			"response waiter already registered for request 13",
		);

		registry.rejectAll(new Error("cleanup"));
	});

	test("rejects all pending responses", async () => {
		const registry = new PendingResponseRegistry<string>();
		const first = registry.waitForResponse(1);
		const second = registry.waitForResponse(2);

		registry.rejectAll(new Error("transport closed"));

		await expect(first).rejects.toThrow("transport closed");
		await expect(second).rejects.toThrow("transport closed");
		expect(registry.resolve(1, "late")).toBe(false);
		expect(registry.resolve(2, "late")).toBe(false);
	});
});
