import { mkdirSync } from "node:fs";
import { resolve } from "node:path";
import { AgentOs, type AgentOsOptions } from "@rivet-dev/agentos-core";

mkdirSync(".agentos", { recursive: true });
const options = {
	database: {
		type: "sqlite_file",
		path: resolve(".agentos/agentos.sqlite"),
	},
} satisfies AgentOsOptions;

let vm = await AgentOs.create(options);
await vm.filesystem.writeFile("/home/agentos/result.txt", "persistent");
await vm.dispose();

vm = await AgentOs.create(options);
const result = await vm.filesystem.readFile("/home/agentos/result.txt");
console.log(new TextDecoder().decode(result));
await vm.dispose();
