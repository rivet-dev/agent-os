import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({ endpoint: "http://localhost:6420" });
const handle = client.vm.getOrCreate("my-agent");

// List all cron jobs
const jobs = await handle.cron.list();
for (const job of jobs) {
  console.log(job.id, job.schedule);
}

// Cancel a specific job
await handle.cron.cancel(jobs[0].id);
