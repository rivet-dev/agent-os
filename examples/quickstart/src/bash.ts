// Run shell commands inside the VM.

import { AgentOs } from "@rivet-dev/agent-os-core";
import common from "@rivet-dev/agent-os-common";

const vm = await AgentOs.create({ software: [common] });

// Simple commands
const echo = await vm.exec("echo 'Hello from the shell!'");
console.log(echo.stdout);

// Pipes
const piped = await vm.exec("echo 'hello world' | tr a-z A-Z");
console.log("Piped:", piped.stdout.trim());

// File manipulation
await vm.exec("echo 'line 1' > /tmp/test.txt");
await vm.exec("echo 'line 2' >> /tmp/test.txt");
await vm.exec("echo 'line 3' >> /tmp/test.txt");

const cat = await vm.exec("cat /tmp/test.txt");
console.log("File contents:");
console.log(cat.stdout);

// grep
const grep = await vm.exec("grep '2' /tmp/test.txt");
console.log("Grep result:", grep.stdout.trim());

console.log("Exit code:", grep.exitCode);

await vm.dispose();
