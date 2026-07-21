import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({ endpoint: "http://localhost:6420" });
const agent = client.vm.getOrCreate("my-agent");

// Fetch from a service running inside the VM
const response = await agent.network.httpRequest({ port: 3000, path: "/api/health" });
console.log("Status:", response.status);

// Create a preview path (port forwarding through the actor), valid for 1 hour
const preview = await agent.createPreviewUrl(3000, 3600);
console.log("Preview path:", preview.path);
console.log("Expires at:", new Date(preview.expiresAt));
