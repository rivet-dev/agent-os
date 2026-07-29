import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({ endpoint: "http://localhost:6420" });
const handle = client.vm.getOrCreate("my-agent");

// docs:start subscribe
const conn = handle.connect();
conn.on("cronEvent", (event) => {
  console.log("Cron event:", event);
});
// docs:end subscribe

await handle.cron.schedule({
  schedule: "*/1 * * * *",
  action: { type: "exec", command: "echo", args: ["heartbeat"] },
});
