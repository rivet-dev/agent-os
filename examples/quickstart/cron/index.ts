// Cron scheduling: schedule recurring commands inside the VM.

import { AgentOs } from "@rivet-dev/agentos";

const vm = await AgentOs.create();

// Schedule a command to run every second (for demo purposes)
const job = vm.cron.schedule({
	schedule: "* * * * * *",
	action: { type: "exec", command: "echo", args: ["cron tick"] },
});
console.log("Scheduled cron job:", job.id);

// List all scheduled jobs
const jobs = vm.cron.list();
console.log("Active cron jobs:", jobs);

// Wait a few seconds to let the cron fire
console.log("Waiting 3 seconds for cron ticks...");
await new Promise((r) => setTimeout(r, 3000));

// Cancel the job
vm.cron.cancel(job.id);
console.log("Cancelled cron job:", job.id);

// Verify it's gone
const remaining = vm.cron.list();
console.log("Remaining cron jobs:", remaining.length);

await vm.dispose();
