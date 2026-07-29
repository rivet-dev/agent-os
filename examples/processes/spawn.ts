import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({ endpoint: "http://localhost:6420" });
const agent = client.vm.getOrCreate("my-agent");
const conn = agent.connect();

// Spawn a dev server
const { pid } = await agent.process.spawn("node", ["/home/agentos/server.js"]);

// Subscribe to process output
conn.on("processOutput", (data) => {
	if (data.pid !== pid) return;
  const text = new TextDecoder().decode(data.data);
  console.log(`[pid ${data.pid}] ${data.stream}: ${text}`);
});

conn.on("processExit", (data) => {
	if (data.pid !== pid) return;
  console.log(`[pid ${data.pid}] exited with code ${data.exitCode}`);
});

console.log("Started process:", pid);
