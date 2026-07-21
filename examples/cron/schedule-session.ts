import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({ endpoint: "http://localhost:6420" });

// Run an agent every day at 9 AM to check for issues
await client.vm.getOrCreate("my-agent").cron.schedule({
  schedule: "0 9 * * *",
  action: {
    type: "session",
    agentType: "pi",
    prompt: "Review the logs in /workspace/logs/ and summarize any errors",
    cwd: "/workspace",
  },
});
