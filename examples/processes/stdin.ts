import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({ endpoint: "http://localhost:6420" });
const agent = client.vm.getOrCreate("my-agent");

const { pid } = await agent.process.spawn("cat", []);

// Write to stdin
await agent.process.writeStdin(pid, "hello from stdin\n");

// Close stdin when done
await agent.process.closeStdin(pid);

// Wait for the process to exit
const exitCode = await agent.process.wait(pid);
console.log("exit code:", exitCode);
