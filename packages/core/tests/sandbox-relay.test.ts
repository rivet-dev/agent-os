import { describe, expect, test, vi } from "vitest";
import type { AgentOsSandboxClient } from "../src/sandbox.js";
import {
	createSandboxRelay,
	type SandboxRelay,
	type SandboxRelayClientController,
} from "../src/sandbox-relay.js";

function staticController(
	getClient: () => AgentOsSandboxClient,
): SandboxRelayClientController {
	return {
		withClient: (operation) => operation(getClient()),
	};
}

function authenticatedFetch(
	relay: SandboxRelay,
	path: string,
	init: RequestInit = {},
): Promise<Response> {
	const headers = new Headers(init.headers);
	headers.set("authorization", `Bearer ${relay.token}`);
	return fetch(`${relay.baseUrl}${path}`, {
		...init,
		headers,
		redirect: "manual",
	});
}

describe("sandbox filesystem relay", () => {
	test("requires its bearer token and exposes only native filesystem routes", async () => {
		const request = vi.fn(async () => new Response("upstream"));
		const relay = await createSandboxRelay({
			controller: staticController(
				() => ({ request }) as unknown as AgentOsSandboxClient,
			),
		});
		try {
			const unauthenticated = await fetch(
				`${relay.baseUrl}/v1/fs/stat?path=%2F`,
			);
			expect(unauthenticated.status).toBe(401);
			expect(await unauthenticated.json()).toEqual(
				expect.objectContaining({
					title: "Unauthorized",
					status: 401,
				}),
			);

			const wrongToken = await fetch(`${relay.baseUrl}/v1/fs/stat?path=%2F`, {
				headers: { authorization: "Bearer wrong-token" },
			});
			expect(wrongToken.status).toBe(401);

			const unknownRoute = await authenticatedFetch(relay, "/v1/processes");
			expect(unknownRoute.status).toBe(404);
			expect(await unknownRoute.json()).toEqual(
				expect.objectContaining({
					title: "Not Found",
					status: 404,
				}),
			);
			expect(request).not.toHaveBeenCalled();
		} finally {
			await relay.dispose();
		}
	});

	test("resolves upstream authentication and custom headers for every client generation", async () => {
		const seen: Array<{
			path: string;
			redirect: RequestRedirect | undefined;
			headers: Headers;
		}> = [];
		const makeClient = (
			token: string,
			defaultHeaders: Record<string, string>,
		): AgentOsSandboxClient =>
			({
				token,
				defaultHeaders,
				request: async (path: string, init?: RequestInit) => {
					seen.push({
						path,
						redirect: init?.redirect,
						headers: new Headers(init?.headers),
					});
					return new Response(null, {
						status: 307,
						headers: { location: "/v1/fs/stat?path=%2Fredirected" },
					});
				},
			}) as unknown as AgentOsSandboxClient;

		let client = makeClient("first-token", {
			"x-generation": "first",
			"x-retired-header": "first-only",
		});
		const relay = await createSandboxRelay({
			controller: staticController(() => client),
		});
		try {
			const first = await authenticatedFetch(
				relay,
				"/v1/fs/stat?path=%2Ffirst",
				{ headers: { range: "bytes=0-3" } },
			);
			expect(first.status).toBe(307);
			expect(first.headers.get("location")).toBe(
				"/v1/fs/stat?path=%2Fredirected",
			);

			client = makeClient("second-token", { "x-generation": "second" });
			const second = await authenticatedFetch(
				relay,
				"/v1/fs/stat?path=%2Fsecond",
			);
			expect(second.status).toBe(307);

			expect(seen).toHaveLength(2);
			expect(seen[0]?.path).toBe("/v1/fs/stat?path=%2Ffirst");
			expect(seen[0]?.redirect).toBe("manual");
			expect(seen[0]?.headers.get("authorization")).toBe("Bearer first-token");
			expect(seen[0]?.headers.get("x-generation")).toBe("first");
			expect(seen[0]?.headers.get("x-retired-header")).toBe("first-only");
			expect(seen[0]?.headers.get("range")).toBe("bytes=0-3");
			expect(seen[1]?.path).toBe("/v1/fs/stat?path=%2Fsecond");
			expect(seen[1]?.headers.get("authorization")).toBe("Bearer second-token");
			expect(seen[1]?.headers.get("x-generation")).toBe("second");
			expect(seen[1]?.headers.get("x-retired-header")).toBeNull();
		} finally {
			await relay.dispose();
		}
	});

	test("bounds concurrent requests and streams the accepted response", async () => {
		let markEntered!: () => void;
		const entered = new Promise<void>((resolve) => {
			markEntered = resolve;
		});
		let releaseResponse!: (response: Response) => void;
		const request = vi.fn(async () => {
			markEntered();
			return await new Promise<Response>((resolve) => {
				releaseResponse = resolve;
			});
		});
		const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
		const relay = await createSandboxRelay({
			controller: staticController(
				() => ({ request }) as unknown as AgentOsSandboxClient,
			),
			maxConcurrentRequests: 1,
		});
		try {
			const accepted = authenticatedFetch(
				relay,
				"/v1/fs/stat?path=%2Faccepted",
			);
			await entered;

			const rejected = await authenticatedFetch(
				relay,
				"/v1/fs/stat?path=%2Frejected",
			);
			expect(rejected.status).toBe(429);
			expect(await rejected.json()).toEqual(
				expect.objectContaining({
					title: "Sandbox relay capacity exceeded",
					detail: expect.stringContaining("sandbox.maxRelayRequests=1"),
				}),
			);

			const encoder = new TextEncoder();
			const stream = new ReadableStream<Uint8Array>({
				start(controller) {
					controller.enqueue(encoder.encode("first-"));
					setTimeout(() => {
						controller.enqueue(encoder.encode("second"));
						controller.close();
					}, 5);
				},
			});
			releaseResponse(
				new Response(stream, {
					headers: { "content-type": "application/octet-stream" },
				}),
			);
			const acceptedResponse = await accepted;
			expect(acceptedResponse.status).toBe(200);
			expect(await acceptedResponse.text()).toBe("first-second");
			expect(request).toHaveBeenCalledTimes(1);
			expect(warn).toHaveBeenCalledWith(
				"agentOS sandbox relay near sandbox.maxRelayRequests: 1/1",
			);
		} finally {
			warn.mockRestore();
			await relay.dispose();
		}
	});

	test("streams request bodies to the active client", async () => {
		let upstreamBody = "";
		const request = vi.fn(async (_path: string, init?: RequestInit) => {
			upstreamBody = await new Response(init?.body as never).text();
			return new Response(`received:${upstreamBody}`);
		});
		const relay = await createSandboxRelay({
			controller: staticController(
				() => ({ request }) as unknown as AgentOsSandboxClient,
			),
		});
		try {
			const response = await authenticatedFetch(
				relay,
				"/v1/fs/file?path=%2Fupload.txt",
				{
					method: "PUT",
					body: "streamed-upload",
				},
			);
			expect(response.status).toBe(200);
			expect(await response.text()).toBe("received:streamed-upload");
			expect(upstreamBody).toBe("streamed-upload");
		} finally {
			await relay.dispose();
		}
	});
});
