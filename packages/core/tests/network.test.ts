import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { AgentOs } from "../src/index.js";

const LISTENER_SCRIPT = `
const net = require("net");
const server = net.createServer(() => {});
server.listen(0, "127.0.0.1", () => {
  console.log("LISTENING:" + server.address().port);
});
setTimeout(() => {}, 30000);
`;

describe("networking", () => {
	let vm: AgentOs;

	beforeEach(async () => {
		vm = await AgentOs.create();
	});

	afterEach(async () => {
		await vm.dispose();
	});

	test("starts a guest TCP listener", async () => {
		await vm.writeFile("/tmp/network.js", LISTENER_SCRIPT);

		let resolvePort!: (value: number) => void;
		const portPromise = new Promise<number>((resolve) => {
			resolvePort = resolve;
		});

		const { pid } = vm.spawn("node", ["/tmp/network.js"], {
			onStdout: (data: Uint8Array) => {
				const text = new TextDecoder().decode(data);
				const match = text.match(/LISTENING:(\d+)/);
				if (match) {
					resolvePort(Number(match[1]));
				}
			},
		});

		const port = await Promise.race([
			portPromise,
			new Promise<number>((_, reject) =>
				setTimeout(
					() => reject(new Error("timed out waiting for guest listener")),
					5_000,
				),
			),
		]);
		expect(port).toBeGreaterThan(0);

		vm.killProcess(pid);
	});
});
