import { afterEach, describe, expect, test } from "vitest";
import { AgentOs } from "../src/index.js";

const textDecoder = new TextDecoder();

async function runSpawnedProcess(
	vm: AgentOs,
	command: string,
	args: string[],
): Promise<{ exitCode: number; stdout: string; stderr: string }> {
	const stdoutChunks: string[] = [];
	const stderrChunks: string[] = [];
	const { pid } = vm.spawn(command, args, {
		onStdout: (chunk) => {
			stdoutChunks.push(textDecoder.decode(chunk));
		},
		onStderr: (chunk) => {
			stderrChunks.push(textDecoder.decode(chunk));
		},
	});

	return {
		exitCode: await vm.waitProcess(pid),
		stdout: stdoutChunks.join(""),
		stderr: stderrChunks.join(""),
	};
}

describe("guest http.request transport", () => {
	let vm: AgentOs;

	afterEach(async () => {
		await vm?.dispose();
	});

	test("reaches a guest net listener through the kernel socket path", async () => {
		vm = await AgentOs.create({
			permissions: {
				fs: "allow",
				network: "allow",
				childProcess: "allow",
			},
		});

		const script = [
			'const http = require("node:http");',
			'const net = require("node:net");',
			'const body = JSON.stringify({ ok: true, path: "/transport-check" });',
			"const server = net.createServer((socket) => {",
			'  let buffered = "";',
			'  socket.setEncoding("utf8");',
			'  socket.on("data", (chunk) => {',
			"    buffered += chunk;",
			'    if (!buffered.includes("\\r\\n\\r\\n")) return;',
			'    socket.end([',
			'      "HTTP/1.1 200 OK",',
			'      "Content-Type: application/json",',
			'      `Content-Length: ${Buffer.byteLength(body)}`,',
			'      "Connection: close",',
			'      "",',
			"      body,",
			'    ].join("\\r\\n"));',
			"  });",
			"});",
			'server.listen(0, "127.0.0.1", () => {',
			"  const address = server.address();",
			'  if (!address || typeof address === "string") {',
			'    console.error("missing tcp address");',
			"    process.exit(1);",
			"    return;",
			"  }",
			'  const req = http.get(`http://127.0.0.1:${address.port}/transport-check`, (res) => {',
			'    let responseBody = "";',
			'    res.setEncoding("utf8");',
			'    res.on("data", (chunk) => {',
			"      responseBody += chunk;",
			"    });",
			'    res.on("end", () => {',
			"      console.log(JSON.stringify({ statusCode: res.statusCode, body: responseBody }));",
			'      server.close(() => process.exit(0));',
			"    });",
			"  });",
			'  req.on("error", (error) => {',
			'    console.error(error?.stack ?? String(error));',
			'    server.close(() => process.exit(1));',
			"  });",
			"});",
		].join("\n");

		const result = await runSpawnedProcess(vm, "node", ["-e", script]);

		expect(result.exitCode).toBe(0);
		expect(result.stderr).toBe("");
		expect(JSON.parse(result.stdout.trim())).toEqual({
			statusCode: 200,
			body: JSON.stringify({
				ok: true,
				path: "/transport-check",
			}),
		});
	});
});
