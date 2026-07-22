import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({
	endpoint: "http://localhost:6420",
});
const agent = client.vm.getOrCreate("my-agent");

const { pid } = await agent.process.spawn("node", ["/home/agentos/server.js"]);

const processStatus = (process: { state: "running" | "exited" }) =>
	process.state;

// List all processes tracked by the VM
const processes = await agent.process.list();
for (const p of processes) {
	console.log(p.pid, p.command ?? "", processStatus(p));
}

// Inspect a specific process by pid
const info = await agent.process.get(pid);
console.log(processStatus(info));

// Graceful stop (SIGTERM)
await agent.process.signal(pid, "SIGTERM");

// Force kill (SIGKILL)
await agent.process.kill(pid);
