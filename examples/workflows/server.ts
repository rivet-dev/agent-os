// docs:start basic
import { agentOS, setup } from "@rivet-dev/agentos";
import pi from "@agentos-software/pi";
import { actor } from "rivetkit";
import { type WorkflowStepContextOf, workflow } from "rivetkit/workflow";

const vm = agentOS({ software: [pi] });

interface BugFixInput {
	repo: string;
	issue: string;
}

// Each actor instance is one durable workflow run. Its creation input is stored
// in actor state, so no queue is needed to trigger or order agent prompts.
const bugFixer = actor({
	state: {
		repo: "",
		issue: "",
		status: "running" as "running" | "complete",
		exitCode: null as number | null,
	},
	onCreate: (c, input: BugFixInput) => {
		c.state.repo = input.repo;
		c.state.issue = input.issue;
	},
	run: workflow(async (ctx) => {
		await ctx.step("clone-repo", (step) => cloneRepo(step, step.state.repo));
		await ctx.step("fix-bug", (step) =>
			fixBugWithAgent(step, step.state.issue),
		);
		const exitCode = await ctx.step("run-tests", (step) => runTests(step));
		await ctx.step("record-result", async (step) => {
			step.state.exitCode = exitCode;
			step.state.status = "complete";
		});
	}),
	actions: {
		getState: (c) => c.state,
	},
});

async function cloneRepo(
	step: WorkflowStepContextOf<typeof bugFixer>,
	repo: string,
): Promise<void> {
	const agent = step.client<typeof registry>().vm.getOrCreate("bug-fixer");
	await agent.process.exec(`git clone ${repo} /home/agentos/repo`);
}

async function fixBugWithAgent(
	step: WorkflowStepContextOf<typeof bugFixer>,
	issue: string,
): Promise<void> {
	const agent = step.client<typeof registry>().vm.getOrCreate("bug-fixer");
	await agent.sessions.open({
		agent: "pi",
		env: { ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY! },
	});
	await agent.sessions.prompt({
		content: [{ type: "text", text: `Fix the bug described in issue: ${issue}` }],
	});
}

async function runTests(
	step: WorkflowStepContextOf<typeof bugFixer>,
): Promise<number> {
	const agent = step.client<typeof registry>().vm.getOrCreate("bug-fixer");
	const tests = await agent.process.exec("cd /home/agentos/repo && npm test");
	return tests.exitCode;
}
// docs:end basic

// docs:start chaining
interface CodeReviewInput {
	filePath: string;
}

const codeReviewer = actor({
	state: {
		filePath: "",
		status: "running" as "running" | "complete",
	},
	onCreate: (c, input: CodeReviewInput) => {
		c.state.filePath = input.filePath;
	},
	run: workflow(async (ctx) => {
		await ctx.step("review", (step) =>
			reviewCode(step, step.state.filePath),
		);
		const review = await ctx.step("read-review", (step) => readReview(step));
		await ctx.step("fix", (step) => applyReview(step, review));
		await ctx.step("record-review", async (step) => {
			step.state.status = "complete";
		});
	}),
	actions: {
		getState: (c) => c.state,
	},
});

async function reviewCode(
	step: WorkflowStepContextOf<typeof codeReviewer>,
	filePath: string,
): Promise<void> {
	const agent = step.client<typeof registry>().vm.getOrCreate("reviewer");
	await agent.sessions.open({
		agent: "pi",
		env: { ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY! },
	});
	await agent.sessions.prompt({
		content: [
			{
				type: "text",
				text: `Review ${filePath} and write findings to /home/agentos/review.md`,
			},
		],
	});
}

async function readReview(
	step: WorkflowStepContextOf<typeof codeReviewer>,
): Promise<string> {
	const agent = step.client<typeof registry>().vm.getOrCreate("reviewer");
	return new TextDecoder().decode(
		await agent.filesystem.readFile("/home/agentos/review.md"),
	);
}

async function applyReview(
	step: WorkflowStepContextOf<typeof codeReviewer>,
	review: string,
): Promise<void> {
	const agent = step.client<typeof registry>().vm.getOrCreate("reviewer");
	await agent.sessions.prompt({
		content: [
			{ type: "text", text: `Apply the following review feedback:\n\n${review}` },
		],
	});
}

export const registry = setup({ use: { vm, bugFixer, codeReviewer } });
registry.start();
// docs:end chaining
