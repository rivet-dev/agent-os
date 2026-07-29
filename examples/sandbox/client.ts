import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({
	endpoint: "http://localhost:6420",
});
const vm = client.vm.getOrCreate("my-agent");

// Write code via the filesystem. The /home/agentos/sandbox mount maps to the sandbox root.
await vm.filesystem.writeFile("/home/agentos/sandbox/app/index.ts", 'console.log("hello")');

// Run it inside the sandbox through the generated binding command.
// The VM path above maps to /app/index.ts at the sandbox root.
const result = await vm.process.exec(
	"agentos-sandbox run-command --command node --args /app/index.ts",
);
console.log(result.stdout); // "hello\n"

const install = await vm.process.exec(
	"agentos-sandbox run-command --command npm --args install --args --prefix --args /app",
);
console.log(install.exitCode, install.stdout);

// Spawn a long-running process and stream its output. Connect to the VM,
// then subscribe to `processOutput` events for the spawned pid.
const { pid } = await vm.process.spawn("npm", ["run", "dev", "--prefix", "/home/agentos/sandbox/app"]);
const conn = vm.connect();
 conn.on("processOutput", (payload) => {
 	if (payload.pid !== pid) return;
	console.log(payload.stream, new TextDecoder().decode(payload.data));
});
