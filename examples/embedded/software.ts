import git from "@agentos-software/git";
import { AgentOs } from "@rivet-dev/agentos-core";

const vm = await AgentOs.create({ software: [git] });

try {
	const result = await vm.process.exec("git --version", {
		output: { capture: "all" },
	});
	console.log(result.stdout);
} finally {
	await vm.dispose();
}
