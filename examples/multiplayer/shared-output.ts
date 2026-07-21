import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({ endpoint: "http://localhost:6420" });
const handle = client.vm.getOrCreate("shared-agent");
const conn = handle.connect();
const { pid } = await handle.process.spawn("node", ["/home/agentos/server.js"]);
const { shellId } = await handle.terminal.open();

// All connected clients see process output
conn.on("processOutput", (data) => {
	if (data.pid !== pid) return;
  const text = new TextDecoder().decode(data.data);
  console.log(`[pid ${data.pid}] ${data.stream}: ${text}`);
});

// All connected clients see shell data
conn.on("shellData", (data) => {
	if (data.shellId !== shellId) return;
  const text = new TextDecoder().decode(data.data);
  process.stdout.write(text);
});
