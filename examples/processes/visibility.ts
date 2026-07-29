import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({ endpoint: "http://localhost:6420" });
const agent = client.vm.getOrCreate("my-agent");

const processStatus = (process: { running: boolean; exitCode?: number | null }) =>
  process.running ? "running" : `exited ${process.exitCode ?? ""}`.trim();

// All processes spawned in the VM
const all = await agent.process.list();
for (const p of all) {
  console.log(p.pid, p.command, p.args.join(" "), processStatus(p));
}

// Inspect a single process by pid
const first = all[0];
if (first) {
  const info = await agent.process.get(first.pid);
  console.log(info.pid, info.command, "status:", processStatus(info));
}
