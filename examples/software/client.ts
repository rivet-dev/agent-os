import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({
	endpoint: "http://localhost:6420",
});
const agent = client.vm.getOrCreate("my-agent");

// `rg` (ripgrep) and `jq` are now available inside the VM. Find files containing
// "TODO" and pretty-print the matching paths as JSON.
const result = await agent.process.exec("rg --files-with-matches TODO /home/agentos | jq -R .");
console.log(result.stdout);
