import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({
	endpoint: "http://localhost:6420",
});

// Stand-in for your own per-tenant credential store.
declare function lookupTenantApiKey(tenantId: string): Promise<string>;

// Give each tenant an isolated VM keyed by their tenant id, then inject that
// tenant's API key from your database at session creation. Keys stay on the
// server and never reach the client.
async function startTenantSession(tenantId: string) {
	const anthropicApiKey = await lookupTenantApiKey(tenantId);

	await client.vm.getOrCreate(tenantId).sessions.open({
		agent: "pi",
		env: { ANTHROPIC_API_KEY: anthropicApiKey },
	});
}

await startTenantSession("tenant-123");
