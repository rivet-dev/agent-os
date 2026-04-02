import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
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
const paths = resolveWorkspacePaths(__dirname);
const hasWasmBinaries = existsSync(path.join(paths.wasmCommandsDir, "bash"));

function stripJustPreamble(output: string): string {
	return output
		.split("\n")
		.filter((line) =>
			line.length > 0 &&
			!line.startsWith("pnpm --filter @rivet-dev/agent-os-dev-shell dev-shell --") &&
			!line.startsWith("> @rivet-dev/agent-os-dev-shell@ dev-shell ") &&
			!line.startsWith("> tsx src/shell.ts ")
		)
		.join("\n")
		.trim();
}

function runJustDevShell(args: string[], timeoutMs = 30_000): Promise<CommandResult> {
	return new Promise((resolve, reject) => {
		const child = spawn("just", ["dev-shell", ...args], {
			cwd: workspaceRoot,
			env: process.env,
			stdio: ["ignore", "pipe", "pipe"],
		});

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
		expect(stripJustPreamble(result.stdout)).toBe(path.resolve(workspaceRoot, "packages", "dev-shell"));
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
		expect(stripJustPreamble(result.stdout)).toContain("JUST_DEV_SHELL_NODE_OK");
	});

	it.skipIf(!hasWasmBinaries)("runs Wasm shell builtins and coreutils through the just wrapper", async () => {
		workDir = await mkdtemp(path.join(tmpdir(), "agent-os-dev-shell-just-wasm-"));
		const result = await runJustDevShell([
			"--work-dir",
			workDir,
			"--",
			"sh",
			"-lc",
			`printf 'cli-note\\n' > ${JSON.stringify(path.join(workDir, "note.txt"))} && pwd && printenv PATH && command -v ls && ls ${JSON.stringify(workDir)}`,
		]);
		expect(result.exitCode).toBe(0);
		const stdout = stripJustPreamble(result.stdout);
		expect(stdout).toContain(workDir);
		expect(stdout).toContain("/bin");
		expect(stdout).toContain("/bin/ls");
		expect(stdout).toContain("note.txt");
	});

	it("runs pi through the just wrapper", async () => {
		const result = await runJustDevShell(["--", "pi", "--help"], 45_000);
		expect(result.exitCode).toBe(0);
		expect(`${result.stdout}\n${result.stderr}`).toMatch(/pi|usage|Usage/);
	});
});
