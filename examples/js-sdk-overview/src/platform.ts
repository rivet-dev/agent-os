import { AgentOs } from "@rivet-dev/agentos";

const runtime = await AgentOs.create({ permissions: { network: "allow" } });

try {
	// Filesystem: node:fs is the VM's persistent filesystem.
	const files = await runtime.javascript.execute(
		`
		import fs from "node:fs/promises";

		await fs.writeFile("/workspace/data.txt", "hello");
		console.log(await fs.readFile("/workspace/data.txt", "utf8"));
		`,
		{ output: { capture: "all" } },
	);
	console.log(files.stdout); // "hello\n"

	// Process trees: node:child_process spawns real guest processes.
	const processes = await runtime.javascript.execute(
		`
		import { execFileSync } from "node:child_process";

		console.log(execFileSync("ls", ["-la", "/workspace"], { encoding: "utf8" }));
		`,
		{ output: { capture: "all" } },
	);
	console.log(processes.stdout); // Directory listing for /workspace

	// Networking: sockets and fetch go through the VM network policy.
	const status = await runtime.javascript.evaluate<number>(
		`(async () => (await fetch("https://example.com")).status)()`,
	);
	console.log(status.outcome === "succeeded" ? status.value : status.error); // 200
} finally {
	await runtime.dispose();
}
