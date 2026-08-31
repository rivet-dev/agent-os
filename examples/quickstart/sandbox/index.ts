// Sandbox extension: mount a Docker sandbox filesystem and run commands.
//
// Requires Docker. Starts a sandbox-agent container, mounts its filesystem
// at /mnt/sandbox, and registers sandbox bindings for running commands.

import { AgentOs } from "@rivet-dev/agentos-core";
import { docker } from "@rivet-dev/agentos-sandbox";

const SANDBOX_QUICKSTART_PERMISSIONS = {
	fs: "allow",
	network: "allow",
	childProcess: "allow",
	env: "allow",
	binding: "allow",
} as const;
const skipDocker = process.env.SKIP_DOCKER === "1";
const SANDBOX_MOUNT = "/mnt/sandbox";

if (skipDocker) {
	console.log("Skipping sandbox quickstart because SKIP_DOCKER=1.");
	process.exit(0);
}

// Start a Docker-backed sandbox, mount its filesystem, and register its bindings.
const vm = await AgentOs.create({
	permissions: SANDBOX_QUICKSTART_PERMISSIONS,
	sandbox: { provider: docker() },
});

try {
	// Write and read a file through the mounted sandbox filesystem.
	await vm.filesystem.writeFile(
		`${SANDBOX_MOUNT}/hello.txt`,
		"Hello from agentOS!",
	);
	const content = await vm.filesystem.readFile(`${SANDBOX_MOUNT}/hello.txt`);
	console.log("Read from sandbox mount:", new TextDecoder().decode(content));

	const runCommandResult = await vm.process.exec(
		"agentos-sandbox run-command --command echo --args 'hello from Docker sandbox'",
	);
	console.log("Sandbox command:", runCommandResult.stdout.trim());

	const processList = await vm.process.exec("agentos-sandbox list-processes");
	console.log("Sandbox processes:", processList.stdout.trim());

	const ANTHROPIC_API_KEY = process.env.ANTHROPIC_API_KEY;
	if (ANTHROPIC_API_KEY) {
		await vm.sessions.open({
			agent: "pi",
			cwd: SANDBOX_MOUNT,
			env: { ANTHROPIC_API_KEY },
		});
		const result = await vm.sessions.prompt({
			content: [
				{
					type: "text",
					text: "Create a C source file named fib.c in the current directory that prints Fibonacci numbers.",
				},
			],
		});
		console.log("Agent:", result.message?.content ?? []);
		if (!(await vm.filesystem.exists(`${SANDBOX_MOUNT}/fib.c`))) {
			throw new Error(`Expected the agent to create ${SANDBOX_MOUNT}/fib.c`);
		}
		console.log(`Verified ${SANDBOX_MOUNT}/fib.c exists.`);
		await vm.sessions.delete();
	} else {
		console.log("Skipping agent prompt because ANTHROPIC_API_KEY is not set.");
	}
} finally {
	await vm.dispose();
}
