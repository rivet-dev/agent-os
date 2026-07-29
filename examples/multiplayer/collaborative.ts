import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

// Driver client
const driver = createClient<typeof registry>({
	endpoint: "http://localhost:6420",
});
const driverAgent = driver.vm.getOrCreate("shared-agent");

await driverAgent.sessions.open({
	agent: "pi",
	env: { ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY! },
});

// Observer client (different user, same actor)
const observer = createClient<typeof registry>({
	endpoint: "http://localhost:6420",
});

const observerConn = observer.vm.getOrCreate("shared-agent").connect();
observerConn.on("sessionEvent", (event) => {
	console.log("[observer]", event);
});

// Driver sends a prompt. Observer sees the streaming response.
await driverAgent.sessions.prompt({
	content: [{ type: "text", text: "Refactor the auth module" }],
});
