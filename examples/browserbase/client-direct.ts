import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server-minimal";

const client = createClient<typeof registry>({
	endpoint: "http://localhost:6420",
});
const agent = client.vm.getOrCreate("my-agent");

// Drive `browse` directly through the VM's process API. `browse cloud fetch`
// retrieves a page through the Browserbase cloud and returns it as JSON with the
// page rendered as markdown. `browse` reads its credentials from the command
// environment, which we pass through `exec`.
const env = {
	BROWSERBASE_API_KEY: process.env.BROWSERBASE_API_KEY!,
	BROWSERBASE_PROJECT_ID: process.env.BROWSERBASE_PROJECT_ID!,
};

const { stdout } = await agent.process.exec("browse cloud fetch https://example.com", {
	env,
});

const page = JSON.parse(stdout) as { statusCode: number; content: string };
console.log(`fetched status ${page.statusCode}`);
console.log(page.content);
