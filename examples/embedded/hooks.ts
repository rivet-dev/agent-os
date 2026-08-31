import pi from "@agentos-software/pi";
import { AgentOs } from "@rivet-dev/agentos-core";

// In the embedded API, ACP updates and permission records share one durable event union.
const vm = await AgentOs.create({ software: [pi] });
await vm.sessions.open({ agent: "pi", permissionPolicy: "ask" });

// Runs for every event on this session.
vm.onSessionEvent((event) => {
	if (event.type === "permission_request") {
		console.log("Permission request:", event.requestId, event.toolCall);
	} else {
		console.log("Session update:", event.durability, event);
	}
});
