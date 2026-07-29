import { agentOS, setup } from "@rivet-dev/agentos";
import { vercelWorldActors } from "@rivet-dev/vercel-world/registry";

const vm = agentOS();

export const registry = setup({
	use: { ...vercelWorldActors, vm },
});
