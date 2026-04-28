// Sandbox extension: mount a Docker sandbox filesystem and run commands.
//
// Requires Docker. Starts a sandbox-agent container, mounts its filesystem
// at /sandbox, and registers the sandbox toolkit for running commands.

import { AgentOs } from "@rivet-dev/agent-os-core";
import common from "@rivet-dev/agent-os-common";
import {
	createSandboxFs,
	createSandboxToolkit,
} from "@rivet-dev/agent-os-sandbox";

const SANDBOX_QUICKSTART_PERMISSIONS = {
	fs: "allow",
	network: "allow",
	childProcess: "allow",
	env: "allow",
	tool: "allow",
} as const;
const skipDocker = process.env.SKIP_DOCKER === "1";

async function readToolsPort(vm: AgentOs): Promise<string> {
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

async function callTool(
	vm: AgentOs,
	port: string,
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
	return JSON.parse(new TextDecoder().decode(await vm.readFile(outFile)));
}

if (skipDocker) {
	console.log("Skipping sandbox quickstart because SKIP_DOCKER=1.");
	process.exit(0);
}

const [{ SandboxAgent }, { docker }] = await Promise.all([
	import("sandbox-agent"),
	import("sandbox-agent/docker"),
]);

// Start a Docker-backed sandbox.
const sandbox = await SandboxAgent.start({
	sandbox: docker(),
});

// Mount the sandbox filesystem at /sandbox and register the toolkit.
const vm = await AgentOs.create({
	permissions: SANDBOX_QUICKSTART_PERMISSIONS,
	software: [common],
	mounts: [
		{
			path: "/sandbox",
			plugin: createSandboxFs({ client: sandbox }),
		},
	],
	toolKits: [createSandboxToolkit({ client: sandbox })],
});

// Write and read a file through the mounted sandbox filesystem.
await vm.writeFile("/sandbox/hello.txt", "Hello from agentOS!");
const content = await vm.readFile("/sandbox/hello.txt");
console.log("Read from sandbox mount:", new TextDecoder().decode(content));

const port = await readToolsPort(vm);
console.log("Tools RPC port:", port);

const runCommandResult = await callTool(vm, port, "sandbox", "run-command", {
	command: "echo",
	args: ["hello from Docker sandbox"],
});
console.log("Sandbox command:", JSON.stringify(runCommandResult));

const processList = await callTool(vm, port, "sandbox", "list-processes", {});
console.log("Sandbox processes:", JSON.stringify(processList));

await vm.dispose();
await sandbox.dispose();
