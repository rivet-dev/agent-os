import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({ endpoint: "http://localhost:6420" });
const conn = client.vm.getOrCreate("my-agent").connect();
const { pid } = await conn.process.spawn("node", ["/home/agentos/server.js"]);

conn.on("processOutput", (data) => {
	if (data.pid !== pid) return;
  // data.pid: number
  // data.stream: "stdout" | "stderr"
  // data.data: Uint8Array
  const text = new TextDecoder().decode(data.data);
  console.log(`[${data.pid}] ${data.stream}: ${text}`);
});

conn.on("processExit", (data) => {
	if (data.pid !== pid) return;
  // data.pid: number
  // data.exitCode: number
  console.log(`Process ${data.pid} exited with code ${data.exitCode}`);
});
