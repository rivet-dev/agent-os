import { AgentOs } from "@rivet-dev/agentos";

const vm = await AgentOs.create({ defaultSoftware: false });
await vm.filesystem.mount({
	path: "/home/agentos/scratch",
	plugin: { id: "memory", config: {} },
});
await vm.filesystem.writeFile("/home/agentos/scratch/hello.txt", "hello");
