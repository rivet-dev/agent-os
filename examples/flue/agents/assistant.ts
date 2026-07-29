import { createAgent } from "@flue/runtime";
import { agentOSSandbox } from "@rivet-dev/agentos-flue";
import { registry } from "../actors.js";

export default createAgent(() => ({
	model: "anthropic/claude-sonnet-5",
	sandbox: agentOSSandbox({ actor: "vm", registry }),
}));
