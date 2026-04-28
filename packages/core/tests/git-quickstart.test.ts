import common from "@rivet-dev/agent-os-common";
import git from "@rivet-dev/agent-os-git";
import { resolve } from "node:path";
import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { AgentOs } from "../src/index.js";
import {
	commandPackageSkipReason,
	registrySkipReason,
	withFallbackCommandDir,
} from "./helpers/registry-commands.js";

type ExecResult = {
	stdout: string;
	stderr: string;
	exitCode: number;
};

const GIT_QUICKSTART_PERMISSIONS = {
	fs: "allow",
	childProcess: "allow",
	env: "allow",
} as const;

const COMMON_SOFTWARE = common.map(withFallbackCommandDir);
const GIT_PACKAGE = {
	...withFallbackCommandDir(git),
	commands: git.commands.filter((command) => command.name === "git"),
};
const gitQuickstartArtifactReason =
	registrySkipReason || commandPackageSkipReason(GIT_PACKAGE);
const MODULE_ACCESS_CWD = resolve(import.meta.dirname, "..");

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
	const headRef = content.trim().match(/^ref: refs\/heads\/(.+)$/)?.[1];
	if (!headRef) {
		throw new Error(`could not determine HEAD ref from:\n${content}`);
	}
	return headRef;
}

describe("git quickstart integration", () => {
	if (gitQuickstartArtifactReason) {
		test("requires registry git command artifacts", () => {
			expect(gitQuickstartArtifactReason).toBe(false);
		});
		return;
	}

		let vm: AgentOs;

		beforeEach(async () => {
			vm = await AgentOs.create({
				moduleAccessCwd: MODULE_ACCESS_CWD,
				permissions: GIT_QUICKSTART_PERMISSIONS,
				software: [COMMON_SOFTWARE, GIT_PACKAGE],
			});
		});

		afterEach(async () => {
			await vm.dispose();
		});

		async function run(command: string): Promise<ExecResult> {
			const result = await vm.exec(command);
			if (result.exitCode !== 0) {
				throw new Error(
					`command failed: ${command}\n${result.stderr || result.stdout}`,
				);
			}
			return result;
		}

		test(
			"covers the quickstart local origin -> clone -> checkout flow",
			async () => {
				await run("git init /tmp/origin");
				await vm.writeFile("/tmp/origin/README.md", "# demo repo\n");
				await run("git -C /tmp/origin add README.md");
				await run("git -C /tmp/origin commit -m 'initial commit'");

				const defaultBranch = parseCurrentBranch(
					(await run("git -C /tmp/origin branch")).stdout,
				);

				await run("git -C /tmp/origin checkout -b feature");
				await vm.writeFile("/tmp/origin/feature.txt", "checked out from feature\n");
				await run("git -C /tmp/origin add feature.txt");
				await run("git -C /tmp/origin commit -m 'add feature file'");

				await run("git clone /tmp/origin /tmp/clone");

				const cloneHead = new TextDecoder().decode(
					await vm.readFile("/tmp/clone/.git/HEAD"),
				);
				expect(parseHeadRef(cloneHead)).toBe("feature");
				expect(defaultBranch).not.toBe("feature");

				const featureFile = await vm.readFile("/tmp/clone/feature.txt");
				expect(new TextDecoder().decode(featureFile)).toBe(
					"checked out from feature\n",
				);

				const readme = await vm.readFile("/tmp/clone/README.md");
				expect(new TextDecoder().decode(readme)).toBe("# demo repo\n");
			},
			120_000,
		);
});
