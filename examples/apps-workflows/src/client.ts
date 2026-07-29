import type { Deployment } from "@rivet-dev/agentos-apps";
import { createClient } from "rivetkit/client";
import type { registry as appRegistry } from "../fixtures/app/src/index.js";

const response = await fetch("http://localhost:3000/deploy/durable-workflow", {
	method: "POST",
});
if (!response.ok) {
	throw new Error(`deployment failed: ${response.status} ${await response.text()}`);
}
const deployment = (await response.json()) as Deployment;

const client = createClient<typeof appRegistry>({
	namespace: deployment.namespace,
	poolName: deployment.pool,
});

try {
	const job = client.job.getOrCreate(["example-job"]);
	console.log(await job.inspect());
} finally {
	await client.dispose();
}
