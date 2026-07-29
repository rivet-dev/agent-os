import { createServer } from "node:http";
import { type AddressInfo, connect } from "node:net";
import { afterEach, describe, expect, test } from "vitest";
import {
	registerGuestEngineProxy,
	unregisterGuestEngineProxy,
} from "../src/engine-proxy.js";

const owners: string[] = [];
const servers: Array<ReturnType<typeof createServer>> = [];

afterEach(async () => {
	for (const owner of owners.splice(0)) unregisterGuestEngineProxy(owner);
	await Promise.all(
		servers.splice(0).map(
			(server) =>
				new Promise<void>((resolve, reject) => {
					server.close((error) => (error ? reject(error) : resolve()));
				}),
		),
	);
});

describe("guest Engine capability proxy", () => {
	test("hides credentials and restricts a guest to its namespace and pool", async () => {
		const upstreamRequests: Array<{
			url: string;
			authorization?: string;
			body: string;
		}> = [];
		const upstream = createServer(async (request, response) => {
			let body = "";
			for await (const chunk of request) body += chunk;
			upstreamRequests.push({
				url: request.url ?? "",
				authorization: request.headers.authorization,
				body,
			});
			response.setHeader("content-type", "application/json");
			if (request.url?.startsWith("/metadata")) {
				response.end(
					JSON.stringify({
						clientEndpoint: "http://engine.internal",
						clientNamespace: "wrong",
						clientToken: "host-secret",
					}),
				);
				return;
			}
			if (request.url?.includes("actor_ids=other-actor")) {
				response.end(
					JSON.stringify({
						actors: [
							{
								actor_id: "other-actor",
								runner_name_selector: "other-pool",
							},
						],
					}),
				);
				return;
			}
			const requestUrl = new URL(request.url ?? "/", "http://upstream");
			if (
				requestUrl.pathname === "/actors" &&
				requestUrl.searchParams.get("name") === "notes"
			) {
				response.end(
					JSON.stringify({
						actors: [
							{
								actor_id: "app-actor",
								runner_name_selector: "app-pool",
							},
						],
					}),
				);
				return;
			}
			response.end(JSON.stringify({ actors: [] }));
		});
		let upgradeUrl = "";
		let upgradeAuthorization: string | undefined;
		let upgradeProtocols: string | undefined;
		upstream.on("upgrade", (request, socket) => {
			upgradeUrl = request.url ?? "";
			upgradeAuthorization = request.headers.authorization;
			upgradeProtocols = request.headers["sec-websocket-protocol"];
			socket.end(
				"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Protocol: rivet_token.host-secret\r\n\r\n",
			);
		});
		servers.push(upstream);
		await new Promise<void>((resolve, reject) => {
			upstream.once("error", reject);
			upstream.listen(0, "127.0.0.1", resolve);
		});
		const upstreamPort = (upstream.address() as AddressInfo).port;
		const owner = "replica:test";
		owners.push(owner);
		const registration = await registerGuestEngineProxy({
			owner,
			upstreamEndpoint: `http://127.0.0.1:${upstreamPort}`,
			upstreamToken: "host-secret",
			namespace: "app-namespace",
			pool: "app-pool",
			maxRequestBytes: 1024,
			maxResponseBytes: 4096,
		});

		const metadataResponse = await fetch(`${registration.endpoint}/metadata`, {
			headers: { authorization: "Bearer guest-controlled" },
		});
		expect(metadataResponse.status).toBe(200);
		expect(await metadataResponse.json()).toEqual({
			clientEndpoint: registration.endpoint,
			clientNamespace: "app-namespace",
		});
		expect(upstreamRequests[0]).toMatchObject({
			authorization: "Bearer host-secret",
		});
		expect(
			new URL(
				upstreamRequests[0]?.url ?? "",
				"http://upstream",
			).searchParams.get("namespace"),
		).toBe("app-namespace");

		const createResponse = await fetch(`${registration.endpoint}/actors`, {
			method: "POST",
			headers: { "content-type": "application/json" },
			body: JSON.stringify({
				name: "notes",
				runner_name_selector: "attacker-controlled",
			}),
		});
		expect(createResponse.status).toBe(200);
		expect(JSON.parse(upstreamRequests[1]?.body ?? "{}")).toMatchObject({
			name: "notes",
			runner_name_selector: "app-pool",
		});

		expect((await fetch(`${registration.endpoint}/namespaces`)).status).toBe(
			403,
		);
		expect((await fetch(`${registration.endpoint}/actors`)).status).toBe(403);
		expect(upstreamRequests).toHaveLength(2);

		const gatewayResponse = await fetch(
			`${registration.endpoint}/gateway/notes/action/list?rvt-method=getOrCreate&rvt-namespace=evil&rvt-pool=other-pool&rvt-runner=evil`,
			{ headers: { authorization: "Bearer guest-controlled" } },
		);
		expect(gatewayResponse.status).toBe(200);
		expect(upstreamRequests[2]?.authorization).toBeUndefined();
		const gatewayUrl = new URL(
			upstreamRequests[2]?.url ?? "",
			"http://upstream",
		);
		expect(gatewayUrl.searchParams.get("rvt-namespace")).toBe("app-namespace");
		expect(gatewayUrl.searchParams.get("rvt-pool")).toBe("app-pool");
		expect(gatewayUrl.searchParams.get("rvt-runner")).toBe("app-pool");
		const getResponse = await fetch(
			`${registration.endpoint}/gateway/notes/action/list?rvt-method=get&rvt-key=a%2Fb,,c%5Cd`,
		);
		expect(getResponse.status).toBe(200);
		const lookupUrl = new URL(
			upstreamRequests.at(-2)?.url ?? "",
			"http://upstream",
		);
		expect(lookupUrl.searchParams.get("key")).toBe("a\\/b/\\0/c\\\\d");
		const resolvedGatewayUrl = new URL(
			upstreamRequests.at(-1)?.url ?? "",
			"http://upstream",
		);
		expect(resolvedGatewayUrl.pathname).toBe("/gateway/app-actor/action/list");
		expect(
			Array.from(resolvedGatewayUrl.searchParams.keys()).some((name) =>
				name.startsWith("rvt-"),
			),
		).toBe(false);
		const directResponse = await fetch(
			`${registration.endpoint}/gateway/app-actor/action/list?rvt-namespace=evil&user-query=preserved`,
		);
		expect(directResponse.status).toBe(200);
		const directGatewayUrl = new URL(
			upstreamRequests.at(-1)?.url ?? "",
			"http://upstream",
		);
		expect(directGatewayUrl.pathname).toBe("/gateway/app-actor/action/list");
		expect(directGatewayUrl.searchParams.get("user-query")).toBe("preserved");
		expect(
			Array.from(directGatewayUrl.searchParams.keys()).some((name) =>
				name.startsWith("rvt-"),
			),
		).toBe(false);
		expect(
			(await fetch(`${registration.endpoint}/gateway/other-actor/action/list`))
				.status,
		).toBe(403);
		expect(upstreamRequests.at(-1)?.authorization).toBe("Bearer host-secret");

		const proxyUrl = new URL(registration.endpoint);
		const upgradeResponse = await new Promise<string>((resolve, reject) => {
			const socket = connect(Number(proxyUrl.port), proxyUrl.hostname);
			let response = "";
			socket.setEncoding("utf8");
			socket.on("connect", () => {
				socket.write(
					[
						`GET ${proxyUrl.pathname}/envoys/connect?protocol_version=8&namespace=evil&pool_name=evil HTTP/1.1`,
						`Host: ${proxyUrl.host}`,
						"Connection: Upgrade",
						"Upgrade: websocket",
						"Sec-WebSocket-Key: dGVzdC1rZXk=",
						"Sec-WebSocket-Version: 13",
						"",
						"",
					].join("\r\n"),
				);
			});
			socket.on("data", (chunk) => {
				response += chunk;
			});
			socket.on("end", () => resolve(response));
			socket.on("error", reject);
		});
		expect(upgradeResponse).toContain("101 Switching Protocols");
		expect(upgradeResponse).not.toContain("rivet_token.host-secret");
		const upgradedUrl = new URL(upgradeUrl, "http://upstream");
		expect(upgradedUrl.searchParams.get("namespace")).toBe("app-namespace");
		expect(upgradedUrl.searchParams.get("pool_name")).toBe("app-pool");
		expect(upgradeAuthorization).toBeUndefined();
		expect(upgradeProtocols).toContain("rivet_token.host-secret");

		unregisterGuestEngineProxy(owner);
		owners.splice(owners.indexOf(owner), 1);
		expect((await fetch(`${registration.endpoint}/metadata`)).status).toBe(401);
	});
});
