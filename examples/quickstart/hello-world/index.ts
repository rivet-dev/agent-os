// Minimal agentOS example: create a VM, write a file, read it back.

import { AgentOs } from "@rivet-dev/agentos";

const vm = await AgentOs.create();

await vm.filesystem.writeFile("/hello.txt", "Hello from agentOS!");
const content = await vm.filesystem.readFile("/hello.txt");
console.log(new TextDecoder().decode(content));

await vm.dispose();
