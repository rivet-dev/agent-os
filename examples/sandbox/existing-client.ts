import { AgentOs } from "@rivet-dev/agentos-core";
import { SandboxAgent } from "sandbox-agent";
import { docker } from "sandbox-agent/docker";

const sandbox = await SandboxAgent.start({ sandbox: docker() });

const vm = await AgentOs.create({
	sandbox: {
		client: sandbox,
		dispose: () => sandbox.destroySandbox(),
	},
});

await vm.dispose();
