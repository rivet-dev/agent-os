import { agentOS, setup } from "@rivet-dev/agentos";
import pi from "@agentos-software/pi";
import { actor } from "rivetkit";
import { workflow } from "rivetkit/workflow";

const vm = agentOS({ software: [pi] });

// Each created actor is one durable workflow run. The run input lives in state,
// so no application queue is needed; AgentOS serializes prompts per session.
const bugFixer = actor({
	state: {
		repo: "",
		issue: "",
		status: "running" as "running" | "complete",
	},
	onCreate: (c, input: { repo: string; issue: string }) => {
		c.state.repo = input.repo;
		c.state.issue = input.issue;
	},
	run: workflow(async (ctx) => {
		await ctx.step("clone-repo", (step) =>
			step
				.client<typeof registry>()
				.vm.getOrCreate("bug-fixer")
				.process.exec(`git clone ${step.state.repo} /home/agentos/repo`),
		);

		await ctx.step("fix-bug", async (step) => {
			const agent = step
				.client<typeof registry>()
				.vm.getOrCreate("bug-fixer");
			await agent.sessions.open({
				agent: "pi",
				env: { ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY! },
			});
			await agent.sessions.prompt({
				content: [
					{ type: "text", text: `Fix the bug in issue: ${step.state.issue}` },
				],
			});
		});

		await ctx.step("run-tests", (step) =>
			step
				.client<typeof registry>()
				.vm.getOrCreate("bug-fixer")
				.process.exec("cd /home/agentos/repo && npm test"),
		);
		await ctx.step("complete", async (step) => {
			step.state.status = "complete";
		});
	}),
});

export const registry = setup({ use: { vm, bugFixer } });
registry.start();
