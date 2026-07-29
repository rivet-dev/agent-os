// Nightly: requires the non-core Git command package.
import { existsSync, statSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import common from "@agentos-software/common";
import git from "@agentos-software/git";
import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { AgentOs } from "../src/index.js";
import { moduleAccessMounts } from "./helpers/node-modules-mount.js";

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

const COMMON_SOFTWARE = common;
const GIT_PACKAGE = requireBuiltPackage(git, "git");
const MODULE_ACCESS_CWD = resolve(import.meta.dirname, "..");
const GIT_CONFIG = [
	"-c safe.directory=*",
	"-c init.defaultBranch=main",
	"-c user.name=agentos",
	"-c user.email=agentos@example.invalid",
].join(" ");

function requireBuiltPackage<T extends { packagePath: string }>(
	pkg: T,
	name: string,
): T {
	const packageDir = pkg.packagePath.endsWith(".aospkg")
		? join(dirname(pkg.packagePath), "package")
		: pkg.packagePath;
	const built =
		existsSync(pkg.packagePath) &&
		statSync(pkg.packagePath).size > 16 &&
		existsSync(join(packageDir, "agentos-package.json")) &&
		existsSync(join(packageDir, "bin", name));
	if (!built) {
		throw new Error(
			`software package ${name} is NOT BUILT (no valid ${pkg.packagePath}).`,
		);
	}
	return pkg;
}

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

function gitCommand(args: string): string {
	return `git ${GIT_CONFIG} ${args}`;
}

describe("git quickstart integration", () => {
	let vm: AgentOs;

	beforeEach(async () => {
		vm = await AgentOs.create({
			mounts: moduleAccessMounts(MODULE_ACCESS_CWD),
			permissions: GIT_QUICKSTART_PERMISSIONS,
			software: [COMMON_SOFTWARE, GIT_PACKAGE],
		});
	});

	afterEach(async () => {
		await vm?.dispose();
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

	test("covers the quickstart local origin -> clone -> checkout flow", async () => {
		await run(gitCommand("init /tmp/origin"));
		await vm.writeFile("/tmp/origin/README.md", "# demo repo\n");
		await run(gitCommand("-C /tmp/origin add README.md"));
		await run(gitCommand("-C /tmp/origin commit -m 'initial commit'"));

		const defaultBranch = parseCurrentBranch(
			(await run(gitCommand("-C /tmp/origin branch"))).stdout,
		);

		await run(gitCommand("-C /tmp/origin checkout -b feature"));
		await vm.writeFile("/tmp/origin/feature.txt", "checked out from feature\n");
		await run(gitCommand("-C /tmp/origin add feature.txt"));
		await run(gitCommand("-C /tmp/origin commit -m 'add feature file'"));

		await run(gitCommand("clone /tmp/origin /tmp/clone"));

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

		const currentBranch = (
			await run(gitCommand("-C /tmp/clone branch --show-current"))
		).stdout.trim();
		expect(currentBranch).toBe("feature");

		const log = await run(gitCommand("-C /tmp/clone log --oneline --all"));
		expect(log.stdout).toContain("add feature file");
		expect(log.stdout).toContain("initial commit");

		await vm.writeFile("/tmp/clone/README.md", "# changed demo repo\n");
		const diff = await run(gitCommand("-C /tmp/clone diff -- README.md"));
		expect(diff.stdout).toContain("-# demo repo");
		expect(diff.stdout).toContain("+# changed demo repo");
	}, 120_000);
});
