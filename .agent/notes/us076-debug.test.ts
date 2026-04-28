import { test } from "vitest";
import { AgentOs } from "../../packages/core/src/agent-os.ts";
import { REGISTRY_SOFTWARE } from "../../packages/core/tests/helpers/registry-commands.ts";

test("debug", async () => {
	const vm = await AgentOs.create({ software: REGISTRY_SOFTWARE });
	try {
		await vm.writeFile("/tmp/test.js", 'console.log("node-output");');
		console.log("NODE_RESULT", JSON.stringify(await vm.exec("node /tmp/test.js")));
		console.log("XU_RESULT", JSON.stringify(await vm.exec("xu hello-agent-os")));
	} finally {
		await vm.dispose();
	}
}, 120_000);
