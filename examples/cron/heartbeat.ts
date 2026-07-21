import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({ endpoint: "http://localhost:6420" });
const handle = client.vm.getOrCreate("my-agent");

// docs:start heartbeat
await handle.cron.schedule({
  schedule: "*/30 * * * *",
  overlap: "skip",
  action: {
    type: "session",
    agentType: "pi",
    prompt: "Check the status of open issues and take any necessary action",
  },
});
// docs:end heartbeat
