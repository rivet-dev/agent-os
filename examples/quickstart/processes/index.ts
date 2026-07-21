// Execute commands and manage processes inside the VM.

import { AgentOs } from "@rivet-dev/agentos";

const vm = await AgentOs.create();

// Run shell commands with exec()
const result = await vm.process.exec("echo 'hello from shell'");
console.log("exec stdout:", result.stdout.trim());
console.log("exec exit code:", result.exitCode);

// Shell pipeline
const piped = await vm.process.exec("echo hello | tr a-z A-Z");
console.log("piped:", piped.stdout.trim());

// grep
await vm.filesystem.writeFile(
	"/tmp/data.txt",
	"apple\nbanana\ncherry\napricot\n",
);
const grepped = await vm.process.exec("grep ap /tmp/data.txt");
console.log("grep:", grepped.stdout.trim());

// sed
const sedResult = await vm.process.exec(
	"echo 'hello world' | sed 's/world/agentOS/'",
);
console.log("sed:", sedResult.stdout.trim());

// Spawn a Node.js script and wait for it to complete
await vm.filesystem.writeFile(
	"/tmp/counter.mjs",
	`
let i = 0;
const interval = setInterval(() => {
  console.log("tick " + i++);
  if (i >= 3) { clearInterval(interval); }
}, 100);
`,
);

const proc = await vm.process.spawn("node", ["/tmp/counter.mjs"]);
vm.onProcessOutput(proc.pid, (event) => {
	if (event.stream === "stdout") {
		process.stdout.write(
			`[process ${proc.pid}] ${new TextDecoder().decode(event.data)}`,
		);
	}
});
console.log("Spawned process:", proc.pid);

// Wait for it to finish
const exitCode = await vm.process.wait(proc.pid);
console.log("Process exited with code:", exitCode);

// List all processes
console.log("Processes:", await vm.process.list());

await vm.dispose();
