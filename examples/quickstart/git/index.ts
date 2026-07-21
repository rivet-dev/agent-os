// Clone a local repository while its feature branch is the source HEAD.

import { createRequire } from "node:module";
import { dirname, resolve } from "node:path";
import { AgentOs } from "@rivet-dev/agentos";
import git from "@agentos-software/git";

type ExecResult = {
	stdout: string;
	stderr: string;
	exitCode: number;
};

const require = createRequire(import.meta.url);
const MODULE_ACCESS_CWD = resolve(
	dirname(require.resolve("@rivet-dev/agentos")),
	"..",
);
const GIT_QUICKSTART_PERMISSIONS = {
	fs: "allow",
	childProcess: "allow",
	env: "allow",
} as const;

function parseCurrentBranch(output: string): string {
	const branch = output
		.split("\n")
		.map((line) => line.trim())
		.find((line) => line.startsWith("* "))
		?.slice(2)
		.trim();

	if (!branch) {
		throw new Error(`could not determine current branch from:\n${output}`);
	}

	return branch;
}

function parseHeadRef(content: string): string {
	const branch = content.trim().match(/^ref: refs\/heads\/(.+)$/)?.[1];
	if (!branch) {
		throw new Error(`could not determine HEAD ref from:\n${content}`);
	}
	return branch;
}

const vm = await AgentOs.create({
	permissions: GIT_QUICKSTART_PERMISSIONS,
	software: [git],
});

async function run(command: string): Promise<ExecResult> {
	const result = await vm.process.exec(command);
	if (result.exitCode !== 0) {
		throw new Error(
			`command failed: ${command}\n${result.stderr || result.stdout}`,
		);
	}
	return result;
}

await run("git init /tmp/origin");
await vm.filesystem.writeFile("/tmp/origin/README.md", "# demo repo\n");
await run("git -C /tmp/origin add README.md");
await run("git -C /tmp/origin commit -m 'initial commit'");

const defaultBranch = parseCurrentBranch(
	(await run("git -C /tmp/origin branch")).stdout,
);

await run("git -C /tmp/origin checkout -b feature");
await vm.filesystem.writeFile("/tmp/origin/feature.txt", "checked out from feature\n");
await run("git -C /tmp/origin add feature.txt");
await run("git -C /tmp/origin commit -m 'add feature file'");

await run("git clone /tmp/origin /tmp/clone");
console.log("origin default branch:", defaultBranch);
console.log(
	"clone HEAD:",
	parseHeadRef(
		new TextDecoder().decode(await vm.filesystem.readFile("/tmp/clone/.git/HEAD")),
	),
);

const featureFile = await vm.filesystem.readFile("/tmp/clone/feature.txt");
console.log("feature.txt:", new TextDecoder().decode(featureFile).trim());

process.exit(0);
