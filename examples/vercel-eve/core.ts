import { AgentOs } from "@rivet-dev/agentos-core";
import { agentOSCoreBackend } from "@rivet-dev/agentos-eve";
import { defineSandbox } from "eve/sandbox";

export default defineSandbox({
	backend: agentOSCoreBackend({
		create: ({ sessionKey }) =>
			AgentOs.create({
				mounts: [
					{
						path: "/workspace",
						plugin: {
							id: "host_dir",
							config: {
								hostPath: `/var/lib/eve/${encodeURIComponent(sessionKey)}`,
							},
						},
						readOnly: false,
					},
				],
			}),
	}),
});
