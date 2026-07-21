/**
 * Networking example.
 *
 * Network access for guest code is governed by the VM permission policy. This
 * example shows both sides of the gate:
 *
 *   1. With network "allow", the guest starts a loopback HTTP server inside the
 *      VM and fetches it - the request and response stay entirely within the
 *      kernel socket table (hermetic, no real host network).
 *   2. With an explicit network deny policy, the same fetch is blocked.
 *   3. Host loopback is separate from VM loopback: even with network "allow",
 *      a guest can reach a host loopback service only when the host port is in
 *      loopbackExemptPorts.
 *
 * Run with:
 *   AGENTOS_SIDECAR_BIN=../../../target/debug/agentos-sidecar \
 *     npx tsx src/index.ts
 */

import { createServer as createHttpServer } from "node:http";
import type { AddressInfo } from "node:net";
// docs:start vm-network
import { AgentOs } from "@rivet-dev/agentos";

// Guest program: start a loopback HTTP server, then fetch it. Both the listen
// and the fetch go through the kernel socket table.
const GUEST = `
import http from "node:http";

const server = http.createServer((_req, res) => {
	res.writeHead(200, { "content-type": "text/plain" });
	res.end("network-ok");
});

await new Promise((resolve, reject) => {
	server.once("error", reject);
	server.listen(0, "127.0.0.1", resolve);
});

const { port } = server.address();
const response = await fetch("http://127.0.0.1:" + port + "/");
const body = await response.text();
console.log("status:", response.status);
console.log("body:", body);

await new Promise((resolve) => server.close(resolve));
`;

// 1. Network allowed. This matches the high-level API default, but spelling it
// out makes the example's policy intent clear.
const allowed = await AgentOs.create({
	permissions: { network: "allow" },
});
try {
	const result = await allowed.javascript.execute(GUEST, {
		output: { capture: "all" },
	});
	console.log("[network allowed] exitCode:", result.exitCode);
	console.log(
		"[network allowed] stdout:",
		JSON.stringify(result.stdout?.trim()),
	);
	console.log(
		"[network allowed] stderr:",
		JSON.stringify(result.stderr?.trim()),
	);
} finally {
	await allowed.dispose();
}
// docs:end vm-network

// 2. Network denied explicitly. Supplying a partial policy denies omitted
// scopes too, so name the runtime capabilities needed to launch the program.
const denied = await AgentOs.create({
	permissions: {
		fs: "allow",
		network: "deny",
		childProcess: "allow",
		process: "allow",
		env: "allow",
	},
});
try {
	const result = await denied.javascript.execute(GUEST, {
		output: { capture: "all" },
	});
	console.log("[network denied] exitCode:", result.exitCode);
	console.log(
		"[network denied] stderr:",
		JSON.stringify(result.stderr?.trim().split("\n")[0]),
	);
} finally {
	await denied.dispose();
}

// 3. Host loopback access: network "allow" is not enough to reach real host
// loopback. The host must explicitly exempt the host port too.
const hostServer = createHttpServer((req, res) => {
	res.writeHead(200, { "content-type": "text/plain" });
	res.end(`host-ok:${req.url}`);
});
await new Promise<void>((resolve) => {
	hostServer.listen(0, "127.0.0.1", resolve);
});
const hostPort = (hostServer.address() as AddressInfo).port;

const HOST_FETCH_GUEST = `
try {
	const response = await fetch("http://127.0.0.1:${hostPort}/from-guest");
	console.log(response.status + ":" + await response.text());
} catch (error) {
	console.log(error.cause?.code || error.code || error.name);
	process.exit(2);
}
`;

try {
	const blockedHostLoopback = await AgentOs.create({
		permissions: { network: "allow" },
	});
	try {
		const result = await blockedHostLoopback.javascript.execute(
			HOST_FETCH_GUEST,
			{ output: { capture: "all" } },
		);
		console.log("[host loopback blocked] exitCode:", result.exitCode);
		console.log(
			"[host loopback blocked] stdout:",
			JSON.stringify(result.stdout?.trim()),
		);
	} finally {
		await blockedHostLoopback.dispose();
	}

	const allowedHostLoopback = await AgentOs.create({
		permissions: { network: "allow" },
		loopbackExemptPorts: [hostPort],
	});
	try {
		const result = await allowedHostLoopback.javascript.execute(
			HOST_FETCH_GUEST,
			{ output: { capture: "all" } },
		);
		console.log("[host loopback allowed] exitCode:", result.exitCode);
		console.log(
			"[host loopback allowed] stdout:",
			JSON.stringify(result.stdout?.trim()),
		);
	} finally {
		await allowedHostLoopback.dispose();
	}
} finally {
	await new Promise<void>((resolve) => hostServer.close(() => resolve()));
}
