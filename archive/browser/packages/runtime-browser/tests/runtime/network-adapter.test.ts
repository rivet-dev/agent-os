import { describe, expect, it } from "vitest";
import { createBrowserDriver, createBrowserNetworkAdapter } from "../../src/driver.js";
import type { NetworkAdapter } from "../../src/runtime.js";

describe("browser network adapter DNS", () => {
	it("resolves deterministic loopback names without host DNS", async () => {
		const adapter = createBrowserNetworkAdapter();

		await expect(adapter.dnsLookup("localhost")).resolves.toEqual({
			address: "127.0.0.1",
			family: 4,
		});
		await expect(adapter.dnsLookup("ip6-localhost")).resolves.toEqual({
			address: "::1",
			family: 6,
		});
	});

	it("returns IP literals directly and fails unsupported host DNS loudly", async () => {
		const adapter = createBrowserNetworkAdapter();

		await expect(adapter.dnsLookup("127.0.0.1")).resolves.toEqual({
			address: "127.0.0.1",
			family: 4,
		});
		await expect(adapter.dnsLookup("::1")).resolves.toEqual({
			address: "::1",
			family: 6,
		});
		await expect(adapter.dnsLookup("example.com")).resolves.toMatchObject({
			code: "ENOSYS",
		});
	});
});

describe("browser driver network adapter injection", () => {
	it("uses a caller-supplied network adapter", async () => {
		const adapter: NetworkAdapter = {
			async fetch(url) {
				return {
					ok: true,
					status: 200,
					statusText: "OK",
					headers: { "content-type": "text/plain" },
					body: `custom:${url}`,
					url,
					redirected: false,
				};
			},
			async dnsLookup(hostname) {
				return { address: hostname, family: 4 };
			},
			async httpRequest(url) {
				return {
					status: 200,
					statusText: "OK",
					headers: { "content-type": "text/plain" },
					body: `custom-http:${url}`,
					url,
				};
			},
		};
		const system = await createBrowserDriver({
			filesystem: "memory",
			networkAdapter: adapter,
		});

		await expect(system.network!.fetch("https://model.local/test")).resolves.toMatchObject({
			body: "custom:https://model.local/test",
		});
	});

	it("permission-wraps a caller-supplied network adapter", async () => {
		const adapter: NetworkAdapter = {
			async fetch(url) {
				return {
					ok: true,
					status: 200,
					statusText: "OK",
					headers: {},
					body: url,
					url,
					redirected: false,
				};
			},
			async dnsLookup(hostname) {
				return { address: hostname, family: 4 };
			},
			async httpRequest(url) {
				return { status: 200, statusText: "OK", headers: {}, body: url, url };
			},
		};
		const system = await createBrowserDriver({
			filesystem: "memory",
			networkAdapter: adapter,
			permissions: {
				network: ({ url }) => !url?.includes("blocked.local"),
			},
		});

		await expect(system.network!.fetch("https://allowed.local/test")).resolves.toMatchObject({
			body: "https://allowed.local/test",
		});
		await expect(system.network!.fetch("https://blocked.local/test")).rejects.toThrow(
			"EACCES",
		);
	});
});
