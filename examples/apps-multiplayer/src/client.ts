import type { Deployment } from "@rivet-dev/agentos-apps";
import { createClient } from "rivetkit/client";
import type { registry as appRegistry } from "../fixtures/app/src/index.js";

const response = await fetch("http://localhost:3000/deploy/multiplayer-room", {
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
	const room = client.room.getOrCreate(["lobby"]);
	await room.join("alice");
	console.log(await room.move("alice", 4, 8));
} finally {
	await client.dispose();
}
