import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({ endpoint: "http://localhost:6420" });
const agent = client.vm.getOrCreate("my-agent");

// Fetch from the VM service started above.
const response = await agent.network.httpRequest({ port: 3000, path: "/" });
console.log("Status:", response.status);
console.log("Body:", new TextDecoder().decode(response.body));
