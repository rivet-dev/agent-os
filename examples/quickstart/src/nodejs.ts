// Run a Node.js script inside the VM that does filesystem operations.

import { AgentOs } from "@rivet-dev/agent-os-core";
import common from "@rivet-dev/agent-os-common";

const vm = await AgentOs.create({ software: [common] });

await vm.writeFile(
	"/tmp/demo.mjs",
	`
import fs from "fs";
import path from "path";

// Create a directory and write files
fs.mkdirSync("/project/src", { recursive: true });
fs.writeFileSync("/project/src/index.js", 'console.log("hello");');
fs.writeFileSync("/project/README.md", "# My Project");

// Read them back
const files = fs.readdirSync("/project", { recursive: true });
console.log("Files:", files);

const content = fs.readFileSync("/project/src/index.js", "utf8");
console.log("index.js:", content);

const stat = fs.statSync("/project/README.md");
console.log("README size:", stat.size, "bytes");
`,
);

const result = await vm.exec("node /tmp/demo.mjs");
console.log(result.stdout);
console.log("Exit code:", result.exitCode);

await vm.dispose();
