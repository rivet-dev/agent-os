import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({ endpoint: "http://localhost:6420" });
const agent = client.vm.getOrCreate("my-agent");

// Write into the VFS (creates parent dirs). Accepts string | Uint8Array.
await agent.filesystem.writeFile("/home/agentos/out.txt", "hi");

// Read back to the host as raw bytes.
const bytes = await agent.filesystem.readFile("/home/agentos/out.txt");
console.log(new TextDecoder().decode(bytes));
