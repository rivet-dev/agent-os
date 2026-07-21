import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({ endpoint: "http://localhost:6420" });
const agent = client.vm.getOrCreate("my-agent");

// Schedule a command every hour
await agent.cron.schedule({
  schedule: "0 * * * *",
  action: { type: "exec", command: "rm", args: ["-rf", "/tmp/cache/*"] },
});

// Schedule an agent session daily at 9 AM
await agent.cron.schedule({
  schedule: "0 9 * * *",
  action: {
    type: "session",
    agentType: "pi",
    prompt: "Review the codebase for security issues and write a report to /home/agentos/audit.md",
  },
});
