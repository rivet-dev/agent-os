// docs:start boot
import { mkdirSync } from "node:fs";
import { resolve } from "node:path";
import pi from "@agentos-software/pi";
import { AgentOs } from "@rivet-dev/agentos-core";

// Embed a VM with the agentOS package. There is no actor runtime or
// client/server split. `AgentOs.create()` returns a handle to the VM.
mkdirSync(".agentos", { recursive: true });
const vm = await AgentOs.create({
	database: {
		type: "sqlite_file",
		path: resolve(".agentos/agentos.sqlite"),
	},
	software: [pi],
});

const result = await vm.process.exec("echo hello");
console.log(result.stdout); // "hello\n"
// docs:end boot

// ── Filesystem ────────────────────────────────────────────────────
async function filesystem() {
	// docs:start filesystem
	await vm.filesystem.writeFile("/home/agentos/hello.txt", "Hello, world!");
	const content = await vm.filesystem.readFile("/home/agentos/hello.txt");
	console.log(new TextDecoder().decode(content));

	await vm.filesystem.mkdir("/home/agentos/src");
	await vm.filesystem.writeFiles([
		{ path: "/home/agentos/src/index.ts", content: "console.log('hi');" },
		{
			path: "/home/agentos/src/utils.ts",
			content: "export const add = (a: number, b: number) => a + b;",
		},
	]);

	const entries = await vm.filesystem.readdirRecursive("/home/agentos");
	for (const entry of entries) {
		console.log(entry.type, entry.path);
	}
	// docs:end filesystem
}

// ── Processes ─────────────────────────────────────────────────────
async function processes() {
	// docs:start processes
	// One-shot execution
	const result = await vm.process.exec("ls -la /home/agentos");
	console.log(result.stdout);

	// Long-running process with portable output and exit subscriptions.
	await vm.filesystem.writeFile(
		"/tmp/server.mjs",
		'import http from "http"; http.createServer((req, res) => res.end("ok")).listen(3000); console.log("listening");',
	);
	const { pid } = await vm.process.spawn("node", ["/tmp/server.mjs"]);

	vm.onProcessOutput(pid, (event) =>
		console.log(event.stream, new TextDecoder().decode(event.data)),
	);
	vm.onProcessExit(pid, (event) => console.log("exited:", event.exitCode));

	// Write to stdin
	await vm.process.writeStdin(pid, "some input\n");

	// Stop or kill
	await vm.process.signal(pid, "SIGTERM");
	// docs:end processes
}

// ── Retained language contexts ───────────────────────────────────
async function contexts() {
	// docs:start contexts
	await vm.createContext("analysis");
	await vm.typescript.execute("const answer: number = 40", {
		contextId: "analysis",
	});
	const result = await vm.javascript.evaluate<number>("answer + 2", {
		contextId: "analysis",
	});
	console.log(result.outcome === "succeeded" ? result.value : result.error);

	await vm.contexts.reset("analysis");
	await vm.contexts.delete("analysis");
	// docs:end contexts
}

// ── Agent sessions ────────────────────────────────────────────────
async function agentSessions() {
	// docs:start sessions
	// openSession() negotiates ACP and durably records the session in SQLite.
	await vm.sessions.open({
		agent: "pi",
		env: { ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY! },
		permissionPolicy: "ask",
	});

	// Native ACP updates and permission records share one session event union.
	vm.onSessionEvent((event) => {
		if (event.type === "permission_request") {
			console.log("Permission:", event.requestId, event.toolCall);
		} else {
			console.log(event.durability, event);
		}
	});

	const result = await vm.sessions.prompt({
		content: [{ type: "text", text: "Write a hello world script" }],
	});
	console.log(result.message?.content ?? []);

	// Unload releases the adapter but preserves SQLite history for restoration.
	await vm.sessions.unload();
	// docs:end sessions
}

// ── Networking ────────────────────────────────────────────────────
async function networking() {
	// docs:start networking
	// Start a server inside the VM
	await vm.filesystem.writeFile(
		"/tmp/app.mjs",
		'import http from "http"; http.createServer((req, res) => res.end("hello")).listen(3000);',
	);
	await vm.process.spawn("node", ["/tmp/app.mjs"]);

	// httpRequest reaches services running in the VM with serializable DTOs.
	const response = await vm.network.httpRequest({ port: 3000, path: "/" });
	console.log(new TextDecoder().decode(response.body));
	// docs:end networking
}

// ── Cron jobs ─────────────────────────────────────────────────────
async function cronJobs() {
	// docs:start cron
	const job = vm.cron.schedule({
		id: "cleanup",
		schedule: "0 * * * *",
		action: { type: "exec", command: "rm", args: ["-rf", "/tmp/cache"] },
	});
	console.log("Scheduled:", job.id);

	// Run an agent session on a schedule
	vm.cron.schedule({
		schedule: "0 9 * * *",
		action: {
			type: "session",
			agentType: "pi",
			prompt: "Review the logs and summarize any errors",
			options: { cwd: "/workspace" },
		},
	});

	vm.onCronEvent((event) => {
		console.log("Cron event:", event.type, event.jobId);
	});

	console.log(vm.cron.list());
	// docs:end cron
}

export { agentSessions, cronJobs, filesystem, networking, processes };
