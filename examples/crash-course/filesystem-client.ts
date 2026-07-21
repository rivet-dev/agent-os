import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({ endpoint: "http://localhost:6420" });
const agent = client.vm.getOrCreate("my-agent");

// Write a file
await agent.filesystem.writeFile("/home/agentos/config.json", JSON.stringify({ key: "value" }));

// Read a file
const content = await agent.filesystem.readFile("/home/agentos/config.json");
console.log(new TextDecoder().decode(content));

// List directory contents recursively
const files = await agent.filesystem.readdirRecursive("/home/agentos");
for (const entry of files) {
  console.log(entry.type, entry.path);
}
