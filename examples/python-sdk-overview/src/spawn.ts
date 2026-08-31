import { AgentOs } from "@rivet-dev/agentos-core";

const runtime = await AgentOs.create({ permissions: { network: "allow" } });

try {
	const server = await runtime.python.spawnModule("http.server", {
		args: ["8000"],
		output: { retainEvents: true },
	});

	const response = await runtime.network.httpRequest({ port: 8000, path: "/" });
	console.log(response.status); // 200

	await runtime.process.readOutput(server.pid);
	await runtime.process.signal(server.pid, "SIGINT");
	await runtime.process.wait(server.pid);
} finally {
	await runtime.dispose();
}
