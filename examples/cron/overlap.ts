import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({ endpoint: "http://localhost:6420" });

// Queue overlapping executions
await client.vm.getOrCreate("my-agent").cron.schedule({
  schedule: "*/5 * * * *",
  overlap: "queue",
  action: {
    type: "session",
    agentType: "pi",
    prompt: "Process the next batch of tasks",
  },
});
