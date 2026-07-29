import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({ endpoint: "http://localhost:6420" });
const agent = client.vm.getOrCreate("my-agent");

// One-shot execution
const result = await agent.process.exec("echo hello && ls /home/agentos");
console.log("stdout:", result.stdout);
console.log("exit code:", result.exitCode);

// Spawn a long-running process
const conn = agent.connect();
const { pid } = await agent.process.spawn("node", ["server.js"]);
conn.on("processOutput", (data) => {
	if (data.pid !== pid) return;
  console.log(`[pid ${data.pid}]`, new TextDecoder().decode(data.data));
});

console.log("Process ID:", pid);
