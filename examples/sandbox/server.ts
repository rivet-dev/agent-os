import { agentOS, setup } from "@rivet-dev/agentos";
import { docker } from "@rivet-dev/agentos-sandbox";

const vm = agentOS({
	sandbox: {
		provider: docker(),
		mountPath: "/home/agentos/sandbox",
	},
});

export const registry = setup({ use: { vm } });

registry.start();
