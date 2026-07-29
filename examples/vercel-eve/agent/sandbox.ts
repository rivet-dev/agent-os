import { agentOSBackend } from "@rivet-dev/agentos-eve";
import { defineSandbox } from "eve/sandbox";
import { registry } from "../actors";

export default defineSandbox({
	backend: agentOSBackend({ actor: "vm", registry }),
});
