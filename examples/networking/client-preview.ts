import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({ endpoint: "http://localhost:6420" });
const agent = client.vm.getOrCreate("my-agent");

// Start a web app in the VM
await agent.process.spawn("node", ["/home/agentos/app.js"]);

// Create a preview URL for port 3000, valid for 1 hour
const preview = await agent.createPreviewUrl(3000, 3600);
console.log("Preview path:", preview.path);
console.log("Token:", preview.token);
console.log("Expires at:", new Date(preview.expiresAt));

// Create a preview URL with a shorter expiration
const shortPreview = await agent.createPreviewUrl(3000, 300); // 5 minutes
console.log("Short-lived preview:", shortPreview.path);
