import type { Deployment } from "@rivet-dev/agentos-apps";
import { createClient } from "rivetkit/client";
import type { registry as appRegistry } from "../fixtures/app/src/index.js";

const response = await fetch("http://localhost:3000/deploy/sqlite-notes", {
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
	const notes = client.notes.getOrCreate(["shared"]);
	await notes.add("Hello from the RivetKit client");
	console.log(await notes.list());
} finally {
	await client.dispose();
}
