import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({ endpoint: "http://localhost:6420" });

// Schedule a cleanup script every hour
const { id } = await client.vm.getOrCreate("my-agent").cron.schedule({
  schedule: "0 * * * *",
  action: {
    type: "exec",
    command: "rm",
    args: ["-rf", "/tmp/cache/*"],
  },
});
console.log("Cron job ID:", id);
