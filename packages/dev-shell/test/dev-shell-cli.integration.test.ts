import { spawn } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";
import { resolveWorkspacePaths } from "../src/shared.ts";

interface CommandResult {
	exitCode: number;
	stdout: string;
	stderr: string;
}

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const workspaceRoot = path.resolve(__dirname, "..", "..", "..");
const justfilePath = path.join(workspaceRoot, "justfile");
const fallbackRecipe =
	'pnpm --filter @rivet-dev/agent-os-dev-shell dev-shell -- "$@"';
resolveWorkspacePaths(__dirname);

function resolveExecutable(binaryName: string): string | undefined {
	const pathValue = process.env.PATH;
	if (!pathValue) {
		return undefined;
	}

	const candidateNames =
		process.platform === "win32"
			? [binaryName, `${binaryName}.exe`, `${binaryName}.cmd`]
			: [binaryName];

	for (const entry of pathValue.split(path.delimiter)) {
		if (!entry) {
			continue;
		}

		for (const candidateName of candidateNames) {
			const candidatePath = path.join(entry, candidateName);
			if (existsSync(candidatePath)) {
				return candidatePath;
			}
		}
	}

	return undefined;
}

function createDevShellWrapperProcess(args: string[]) {
	const justBinary = resolveExecutable("just");
	if (justBinary) {
		return spawn(justBinary, ["--justfile", justfilePath, "dev-shell", ...args], {
			cwd: workspaceRoot,
			env: process.env,
			stdio: ["ignore", "pipe", "pipe"],
		});
	}

	const justfileContents = readFileSync(justfilePath, "utf8");
	if (!justfileContents.includes(fallbackRecipe)) {
		throw new Error(
			"just is not installed and the dev-shell justfile recipe no longer matches the pnpm fallback command",
		);
	}

	const separatorIndex = args.indexOf("--");
	const forwardedArgs =
		separatorIndex === -1
			? args
			: [...args.slice(0, separatorIndex), ...args.slice(separatorIndex + 1)];
	return spawn(
		"pnpm",
		[
			"--filter",
			"@rivet-dev/agent-os-dev-shell",
			"dev-shell",
			"--",
			...forwardedArgs,
		],
		{
			cwd: workspaceRoot,
			env: process.env,
			stdio: ["ignore", "pipe", "pipe"],
		},
	);
}

function stripJustPreamble(output: string): string {
	return output
		.split("\n")
		.filter(
			(line) =>
				line.length > 0 &&
				!line.startsWith(
					"pnpm --filter @rivet-dev/agent-os-dev-shell dev-shell --",
				) &&
				!line.startsWith("> @rivet-dev/agent-os-dev-shell@ dev-shell ") &&
				!line.startsWith("> pnpm exec tsx src/shell.ts ") &&
				!line.startsWith("> tsx src/shell.ts ") &&
				!line.startsWith(
					"> node ../../node_modules/tsx/dist/cli.mjs src/shell.ts ",
				),
		)
		.join("\n")
		.trim();
}

function runJustDevShell(
	args: string[],
	timeoutMs = 30_000,
): Promise<CommandResult> {
	return new Promise((resolve, reject) => {
		const child = createDevShellWrapperProcess(args);

		const stdoutChunks: Buffer[] = [];
		const stderrChunks: Buffer[] = [];
		const timer = setTimeout(() => {
			child.kill("SIGKILL");
			reject(new Error(`Timed out running: just dev-shell ${args.join(" ")}`));
		}, timeoutMs);

		child.stdout.on("data", (chunk: Buffer) => stdoutChunks.push(chunk));
		child.stderr.on("data", (chunk: Buffer) => stderrChunks.push(chunk));
		child.on("error", (error) => {
			clearTimeout(timer);
			reject(error);
		});
		child.on("close", (code) => {
			clearTimeout(timer);
			resolve({
				exitCode: code ?? 1,
				stdout: Buffer.concat(stdoutChunks).toString("utf8"),
				stderr: Buffer.concat(stderrChunks).toString("utf8"),
			});
		});
	});
}

describe("dev-shell justfile wrapper", { timeout: 60_000 }, () => {
	let workDir: string | undefined;

	afterEach(async () => {
		if (workDir) {
			await rm(workDir, { recursive: true, force: true });
			workDir = undefined;
		}
	});

	it("runs the default work dir through the just wrapper", async () => {
		const result = await runJustDevShell([
			"--",
			"node",
			"-e",
			"process.stdout.write(process.cwd())",
		]);
		expect(result.exitCode).toBe(0);
		expect(result.stderr).toContain("agent-os dev shell");
		expect(result.stderr).toContain("loaded commands:");
		expect(stripJustPreamble(result.stdout)).toBe(
			path.resolve(workspaceRoot, "packages", "dev-shell"),
		);
	});

	it("passes --work-dir through the just wrapper", async () => {
		workDir = await mkdtemp(path.join(tmpdir(), "agent-os-dev-shell-just-"));
		const result = await runJustDevShell([
			"--work-dir",
			workDir,
			"--",
			"node",
			"-e",
			"process.stdout.write(process.cwd())",
		]);
		expect(result.exitCode).toBe(0);
		expect(result.stderr).toContain(`work dir: ${workDir}`);
		expect(stripJustPreamble(result.stdout)).toBe(workDir);
	});

	it("runs startup commands through the just wrapper", async () => {
		const result = await runJustDevShell([
			"--",
			"node",
			"-e",
			"console.log('JUST_DEV_SHELL_NODE_OK')",
		]);
		expect(result.exitCode).toBe(0);
		expect(stripJustPreamble(result.stdout)).toContain(
			"JUST_DEV_SHELL_NODE_OK",
		);
	});

	it("runs scripted shell commands through the just wrapper", async () => {
		workDir = await mkdtemp(path.join(tmpdir(), "agent-os-dev-shell-shell-"));
		const shellWorkDir = workDir;
		const result = await runJustDevShell([
			"--work-dir",
			shellWorkDir,
			"--",
			"bash",
			"-lc",
			`echo cli-shell-ok && pwd`,
		]);
		expect(result.exitCode).toBe(0);
		const stdout = stripJustPreamble(result.stdout);
		expect(stdout).toContain("cli-shell-ok");
		expect(stdout).toContain(shellWorkDir);
	});

	it("runs pi through the just wrapper", async () => {
		const result = await runJustDevShell(["--", "pi", "--help"], 45_000);
		expect(result.exitCode).toBe(0);
		expect(`${result.stdout}\n${result.stderr}`).toMatch(/pi|usage|Usage/);
	});
});
