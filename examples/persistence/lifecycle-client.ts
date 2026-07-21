import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({
	endpoint: "http://localhost:6420",
});

const conn = client.vm.getOrCreate("my-agent").connect();

conn.on("vmBooted", () => {
	console.log("VM is ready");
});

conn.on("vmShutdown", (payload) => {
	console.log("VM shutdown reason:", payload.reason);
	// reason: "sleep" | "destroy" | "error"
});

// The VM starts lazily on the first AgentOS action after wake, after the
// lifecycle subscriptions above are active.
await conn.filesystem.exists("/");
