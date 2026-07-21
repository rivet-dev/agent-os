import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({ endpoint: "http://localhost:6420" });

const result = await client.vm
	.getOrCreate("my-agent")
	.process.exec("echo hello && ls /home/agentos");
console.log("stdout:", result.stdout);
console.log("stderr:", result.stderr);
console.log("exit code:", result.exitCode);
