import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({ endpoint: "http://localhost:6420" });
const agent = client.vm.getOrCreate("my-agent");
const conn = agent.connect();

// Spawn an interactive shell process
const { pid } = await agent.process.spawn("sh", []);

// Stream this process's output as it is produced
conn.on("processOutput", (data) => {
	if (data.pid !== pid) return;
  const text = new TextDecoder().decode(data.data);
  process.stdout.write(text);
});

// Drive it by writing commands to stdin
await agent.process.writeStdin(pid, "ls -la /home/agentos\n");

// Close stdin to let the shell exit, then wait for it
await agent.process.closeStdin(pid);
await agent.process.wait(pid);
