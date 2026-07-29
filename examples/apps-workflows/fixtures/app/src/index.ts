import { actor, setup } from "rivetkit";
import { workflow } from "rivetkit/workflow";

const job = actor({
	state: {
		id: "",
		status: "queued" as "queued" | "running" | "complete",
	},
	onCreate(c) {
		c.state.id = c.key[0] ?? "";
	},
	actions: {
		inspect: (c) => c.state,
	},
	run: workflow(async (workflowContext) => {
		await workflowContext.step("start", async (c) => {
			c.state.status = "running";
		});
		await workflowContext.sleep("work", 1_000);
		await workflowContext.step("finish", async (c) => {
			c.state.status = "complete";
		});
	}),
});

export const registry = setup({
	use: { job },
});

registry.start();

export default function fetch() {
	return Response.json({
		app: "durable-workflow",
		message: "Use the RivetKit client to create and inspect jobs.",
	});
}
