import { agentOS, setup } from "@rivet-dev/agentos";
import myAgent from "./my-agent.ts";

const vm = agentOS({ software: [myAgent] });
// openSession() launches or restores the agent through its acpEntrypoint:
//   await vm.sessions.open({ agent: "my-agent" });

export const registry = setup({ use: { vm } });
registry.start();
