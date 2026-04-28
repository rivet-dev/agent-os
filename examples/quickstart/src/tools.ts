// Host toolkits: define tools that execute on the host and are callable
// from inside the VM via the tools RPC server.
//
// Each toolkit becomes a set of tools accessible at AGENTOS_TOOLS_PORT.
// Node scripts inside the VM can call the server directly with fetch.

import { AgentOs, hostTool, toolKit } from "@rivet-dev/agent-os-core";
import { z } from "zod";

const weatherToolkit = toolKit({
	name: "weather",
	description: "Look up weather information for cities.",
	tools: {
		get: hostTool({
			description: "Get the current weather for a city.",
			inputSchema: z.object({
				city: z.string().describe("City name (e.g. 'London')."),
			}),
			execute: async ({ city }) => ({
				city,
				temperature: 18,
				conditions: "partly cloudy",
				humidity: 65,
			}),
			examples: [
				{ description: "Get London weather", input: { city: "London" } },
			],
		}),
	},
});

const calcToolkit = toolKit({
	name: "calc",
	description: "Simple calculator operations.",
	tools: {
		add: hostTool({
			description: "Add two numbers.",
			inputSchema: z.object({ a: z.number(), b: z.number() }),
			execute: ({ a, b }) => ({ result: a + b }),
		}),
	},
});

const vm = await AgentOs.create({
	toolKits: [weatherToolkit, calcToolkit],
	permissions: {
		fs: "allow",
		network: "allow",
		childProcess: "allow",
		env: "allow",
		tool: "allow",
	},
});

async function readToolsPort(): Promise<string> {
	let stdout = "";
	let stderr = "";
	await vm.writeFile(
		"/tmp/read-tools-port.cjs",
		'process.stdout.write(process.env.AGENTOS_TOOLS_PORT||"")',
	);
	const proc = vm.spawn("node", ["/tmp/read-tools-port.cjs"], {
		onStdout: (data) => {
			stdout += new TextDecoder().decode(data);
		},
		onStderr: (data) => {
			stderr += new TextDecoder().decode(data);
		},
	});
	const exitCode = await vm.waitProcess(proc.pid);
	if (exitCode !== 0) {
		throw new Error(`Failed to read AGENTOS_TOOLS_PORT: ${stderr.trim()}`);
	}
	const port = stdout.trim();
	if (!port) {
		throw new Error("AGENTOS_TOOLS_PORT is not set inside the VM");
	}
	return port;
}

const port = await readToolsPort();
console.log("Tools RPC port:", port);

// Helper: call a tool via the RPC server using a Node script inside the VM
async function callTool(
	toolkit: string,
	tool: string,
	input: Record<string, unknown>,
): Promise<unknown> {
	const outFile = `/tmp/${toolkit}-${tool}-out.json`;
	let stderr = "";
	const source = [
		'import{writeFileSync as w}from"node:fs";',
		`const r=await fetch("http://127.0.0.1:${port}/call",{method:"POST",headers:{"Content-Type":"application/json"},body:${JSON.stringify(
			JSON.stringify({ toolkit, tool, input }),
		)}});`,
		`w(${JSON.stringify(outFile)},await r.text());`,
	].join("");
	await vm.writeFile("/tmp/tool-call.mjs", source);
	const proc = vm.spawn("node", ["/tmp/tool-call.mjs"], {
		onStderr: (data) => {
			stderr += new TextDecoder().decode(data);
		},
	});
	const exitCode = await vm.waitProcess(proc.pid);
	if (exitCode !== 0) {
		throw new Error(
			`Tool call process exited with code ${exitCode}: ${stderr.trim()}`,
		);
	}
	const data = await vm.readFile(outFile);
	return JSON.parse(new TextDecoder().decode(data));
}

// Call the weather tool
const weather = await callTool("weather", "get", { city: "London" });
console.log("Weather:", JSON.stringify(weather));

// Call the calculator tool
const sum = await callTool("calc", "add", { a: 10, b: 32 });
console.log("Sum:", JSON.stringify(sum));

await vm.dispose();
