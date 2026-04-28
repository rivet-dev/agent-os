import { existsSync } from "node:fs";
import { chmod, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";
import { createDevShellKernel } from "../src/index.ts";
import { resolveWorkspacePaths } from "../src/shared.ts";

const paths = resolveWorkspacePaths(
	path.dirname(fileURLToPath(import.meta.url)),
);
const DEV_SHELL_TMP_ROOT_PREFIX = `agent-os-dev-shell-${process.pid}-`;

async function listDevShellTempRoots(): Promise<string[]> {
	return (await readdir(tmpdir(), { withFileTypes: true }))
		.filter(
			(entry) =>
				entry.isDirectory() &&
				entry.name.startsWith(DEV_SHELL_TMP_ROOT_PREFIX),
		)
		.map((entry) => path.join(tmpdir(), entry.name))
		.sort();
}

async function runKernelCommand(
	shell: Awaited<ReturnType<typeof createDevShellKernel>>,
	command: string,
	args: string[],
	timeoutMs = 20_000,
): Promise<{ exitCode: number; stdout: string; stderr: string }> {
	let stdout = "";
	let stderr = "";
	const flushOutputCallbacks = async () => {
		let previousSnapshot = "";
		for (let attempt = 0; attempt < 3; attempt++) {
			const snapshot = `${stdout.length}:${stderr.length}`;
			if (snapshot === previousSnapshot) {
				return;
			}
			previousSnapshot = snapshot;
			await new Promise((resolve) => setTimeout(resolve, 0));
		}
	};

	return Promise.race([
		(async () => {
			const proc = shell.kernel.spawn(command, args, {
				cwd: shell.workDir,
				env: shell.env,
				onStdout: (chunk) => {
					stdout += Buffer.from(chunk).toString("utf8");
				},
				onStderr: (chunk) => {
					stderr += Buffer.from(chunk).toString("utf8");
				},
			});
			const exitCode = await proc.wait();
			await flushOutputCallbacks();
			return { exitCode, stdout, stderr };
		})(),
		new Promise<never>((_, reject) =>
			setTimeout(
				() =>
					reject(new Error(`Timed out running: ${command} ${args.join(" ")}`)),
				timeoutMs,
			),
		),
	]);
}

describe("dev-shell integration", { timeout: 60_000 }, () => {
		let shell: Awaited<ReturnType<typeof createDevShellKernel>> | undefined;
		let workDir: string | undefined;
		let hostOnlyDir: string | undefined;

		afterEach(async () => {
			await shell?.dispose();
			shell = undefined;
			if (hostOnlyDir) {
				await rm(hostOnlyDir, { recursive: true, force: true });
				hostOnlyDir = undefined;
			}
			if (workDir) {
				await rm(workDir, { recursive: true, force: true });
				workDir = undefined;
			}
		});

		it("boots the sandbox-native dev-shell surface and runs node, pi, and the Wasm shell", async () => {
			workDir = await mkdtemp(path.join(tmpdir(), "agent-os-dev-shell-"));
			await writeFile(path.join(workDir, "note.txt"), "dev-shell\n");

			shell = await createDevShellKernel({ workDir });

			expect(shell.loadedCommands).toEqual(
				expect.arrayContaining(["bash", "node", "npm", "npx", "pi", "sh"]),
			);
			expect(shell.loadedCommands).not.toEqual(
				expect.arrayContaining(["python", "python3", "pip"]),
			);

			const nodeResult = await runKernelCommand(shell, "node", [
				"-e",
				"console.log(process.version)",
			]);
			expect(nodeResult.exitCode).toBe(0);
			expect(nodeResult.stdout).toMatch(/v\d+\.\d+\.\d+/);

			const shellResult = await runKernelCommand(shell, "bash", [
				"-ic",
				"echo shell-ok",
			]);
			expect(shellResult.exitCode).toBe(0);
			expect(shellResult.stdout).toContain("shell-ok");

			const piResult = await runKernelCommand(shell, "pi", ["--help"], 30_000);
			expect(piResult.exitCode).toBe(0);
			expect(`${piResult.stdout}\n${piResult.stderr}`).toMatch(
				/pi|usage|Usage/,
			);
		});

		it("resolves file listings through the Wasm shell", async () => {
			workDir = await mkdtemp(path.join(tmpdir(), "agent-os-dev-shell-pty-"));
			await writeFile(path.join(workDir, "note.txt"), "pty-dev-shell\n");
			shell = await createDevShellKernel({ workDir });

			const shellResult = await runKernelCommand(shell, "bash", [
				"-ic",
				"ls /bin",
			]);

			expect(shellResult.exitCode).toBe(0);
			expect(shellResult.stdout).toContain("npm");
			expect(shellResult.stdout).toContain("npx");
		});

		it("does not read or execute host-only paths outside the mounted VM roots", async () => {
			workDir = await mkdtemp(path.join(tmpdir(), "agent-os-dev-shell-isolated-"));
			hostOnlyDir = await mkdtemp("/var/tmp/agent-os-dev-shell-host-only-");
			const hostOnlyFile = path.join(hostOnlyDir, "secret.txt");
			const hostOnlyCommand = path.join(hostOnlyDir, "host-only-command.sh");

			await writeFile(hostOnlyFile, "host-only secret\n");
			await writeFile(
				hostOnlyCommand,
				"#!/bin/sh\nprintf 'host-only command should stay hidden\\n'\n",
			);
			await chmod(hostOnlyCommand, 0o755);

			shell = await createDevShellKernel({ workDir });

			const readResult = await runKernelCommand(shell, "cat", [hostOnlyFile]);
			expect(readResult.exitCode).not.toBe(0);
			expect(`${readResult.stdout}\n${readResult.stderr}`).not.toContain(
				"host-only secret",
			);

			const execResult = await runKernelCommand(shell, hostOnlyCommand, []);
			expect(execResult.exitCode).not.toBe(0);
			expect(`${execResult.stdout}\n${execResult.stderr}`).not.toContain(
				"host-only command should stay hidden",
			);
		});

		it("keeps dev-shell writes in the VM shadow root instead of mutating the host work dir", async () => {
			workDir = await mkdtemp(path.join(tmpdir(), "agent-os-dev-shell-shadow-"));
			const guestFilePath = path.join(workDir, "note.txt");
			await writeFile(guestFilePath, "host-note\n");

			shell = await createDevShellKernel({ workDir });
			await shell.kernel.writeFile(guestFilePath, "vm-note\n");

			const guestReadback = new TextDecoder().decode(
				await shell.kernel.readFile(guestFilePath),
			);
			expect(guestReadback).toBe("vm-note\n");
			await expect(readFile(guestFilePath, "utf8")).resolves.toBe("host-note\n");

			const catResult = await runKernelCommand(shell, "cat", [guestFilePath]);
			expect(catResult.exitCode).toBe(0);
			expect(catResult.stdout).toContain("vm-note");
		});

		it("mounts /tmp on isolated per-session host temp dirs and removes them on dispose", async () => {
			const workDirA = await mkdtemp(path.join(tmpdir(), "agent-os-dev-shell-a-"));
			const workDirB = await mkdtemp(path.join(tmpdir(), "agent-os-dev-shell-b-"));
			const tempRootsBefore = await listDevShellTempRoots();
			let shellA: Awaited<ReturnType<typeof createDevShellKernel>> | undefined;
			let shellB: Awaited<ReturnType<typeof createDevShellKernel>> | undefined;
			let sessionARoot: string | undefined;
			let sessionBRoot: string | undefined;

			try {
				shellA = await createDevShellKernel({ workDir: workDirA });
				shellB = await createDevShellKernel({ workDir: workDirB });

				await shellA.kernel.writeFile("/tmp/session-a.txt", "session-a\n");
				await shellB.kernel.writeFile("/tmp/session-b.txt", "session-b\n");

				await expect(shellA.kernel.exists("/tmp/session-b.txt")).resolves.toBe(
					false,
				);
				await expect(shellB.kernel.exists("/tmp/session-a.txt")).resolves.toBe(
					false,
				);

				const createdRoots = (await listDevShellTempRoots()).filter(
					(root) => !tempRootsBefore.includes(root),
				);
				expect(createdRoots).toHaveLength(2);

				for (const root of createdRoots) {
					expect(path.basename(root)).toMatch(
						new RegExp(`^${DEV_SHELL_TMP_ROOT_PREFIX}`),
					);
					expect(existsSync(path.join(root, "tmp"))).toBe(true);
				}

				const tempRootContents = await Promise.all(
					createdRoots.map(async (root) => ({
						root,
						entries: await readdir(path.join(root, "tmp")),
					})),
				);
				sessionARoot = tempRootContents.find((root) =>
					root.entries.includes("session-a.txt"),
				)?.root;
				sessionBRoot = tempRootContents.find((root) =>
					root.entries.includes("session-b.txt"),
				)?.root;
				expect(sessionARoot).toBeDefined();
				expect(sessionBRoot).toBeDefined();
				expect(sessionARoot).not.toBe(sessionBRoot);
			} finally {
				await shellA?.dispose();
				await shellB?.dispose();
				await rm(workDirA, { recursive: true, force: true });
				await rm(workDirB, { recursive: true, force: true });
			}

			expect(sessionARoot && existsSync(sessionARoot)).toBe(false);
			expect(sessionBRoot && existsSync(sessionBRoot)).toBe(false);
		});
});

describe("dev-shell debug logger", { timeout: 60_000 }, () => {
	let shell: Awaited<ReturnType<typeof createDevShellKernel>> | undefined;
	let workDir: string | undefined;
	let logDir: string | undefined;

	afterEach(async () => {
		await shell?.dispose();
		shell = undefined;
		if (workDir) {
			await rm(workDir, { recursive: true, force: true });
			workDir = undefined;
		}
		if (logDir) {
			await rm(logDir, { recursive: true, force: true });
			logDir = undefined;
		}
	});

	it("writes structured debug logs to the requested file and keeps stdout/stderr clean", async () => {
		workDir = await mkdtemp(path.join(tmpdir(), "agent-os-debug-log-"));
		logDir = await mkdtemp(path.join(tmpdir(), "agent-os-debug-log-out-"));
		const logPath = path.join(logDir, "debug.ndjson");

		// Capture process stdout/stderr to detect any contamination.
		const origStdoutWrite = process.stdout.write.bind(process.stdout);
		const origStderrWrite = process.stderr.write.bind(process.stderr);
		const stdoutCapture: string[] = [];
		const stderrCapture: string[] = [];
		process.stdout.write = ((chunk: unknown, ...rest: unknown[]) => {
			if (typeof chunk === "string") stdoutCapture.push(chunk);
			else if (Buffer.isBuffer(chunk))
				stdoutCapture.push(chunk.toString("utf8"));
			return (origStdoutWrite as Function)(chunk, ...rest);
		}) as typeof process.stdout.write;
		process.stderr.write = ((chunk: unknown, ...rest: unknown[]) => {
			if (typeof chunk === "string") stderrCapture.push(chunk);
			else if (Buffer.isBuffer(chunk))
				stderrCapture.push(chunk.toString("utf8"));
			return (origStderrWrite as Function)(chunk, ...rest);
		}) as typeof process.stderr.write;

		try {
			shell = await createDevShellKernel({
				workDir,
				mountWasm: false,
				debugLogPath: logPath,
			});

			// Run a quick command to exercise the kernel.
			const proc = shell.kernel.spawn(
				"node",
				["-e", "console.log('debug-log-test')"],
				{
					cwd: shell.workDir,
					env: shell.env,
				},
			);
			await proc.wait();

			await shell.dispose();
			shell = undefined;
		} finally {
			process.stdout.write = origStdoutWrite;
			process.stderr.write = origStderrWrite;
		}

		// The log file must exist and contain structured JSON lines.
		expect(existsSync(logPath)).toBe(true);
		const logContent = await readFile(logPath, "utf8");
		const lines = logContent.trim().split("\n").filter(Boolean);
		expect(lines.length).toBeGreaterThanOrEqual(1);

		// Every line must be valid JSON with a timestamp.
		for (const line of lines) {
			const record = JSON.parse(line);
			expect(record).toHaveProperty("time");
		}

		// At least one record should reference session init.
		const initRecord = lines.find((line) =>
			line.includes("dev-shell session init"),
		);
		expect(initRecord).toBeDefined();

		// Stdout/stderr must not contain any pino JSON records.
		const combinedOutput = [...stdoutCapture, ...stderrCapture].join("");
		for (const line of lines) {
			expect(combinedOutput).not.toContain(line);
		}
	});

	it("emits kernel diagnostic records for spawn, process exit, and PTY operations", async () => {
		workDir = await mkdtemp(path.join(tmpdir(), "agent-os-debug-diag-"));
		logDir = await mkdtemp(path.join(tmpdir(), "agent-os-debug-diag-out-"));
		const logPath = path.join(logDir, "debug.ndjson");

		shell = await createDevShellKernel({
			workDir,
			mountWasm: false,
			debugLogPath: logPath,
		});

		// Spawn a command to exercise kernel spawn/exit logging
		const proc = shell.kernel.spawn(
			"node",
			["-e", "console.log('diag-test')"],
			{
				cwd: shell.workDir,
				env: shell.env,
			},
		);
		await proc.wait();

		await shell.dispose();
		shell = undefined;

		const logContent = await readFile(logPath, "utf8");
		const lines = logContent.trim().split("\n").filter(Boolean);
		const records = lines.map((l) => JSON.parse(l));

		// Must contain spawn and exit diagnostics from the kernel
		const spawnRecord = records.find(
			(r: Record<string, unknown>) =>
				r.msg === "process spawned" &&
				(r as Record<string, unknown>).command === "node",
		);
		expect(spawnRecord).toBeDefined();
		expect(spawnRecord).toHaveProperty("pid");
		expect(spawnRecord).toHaveProperty("driver");

		const exitRecord = records.find(
			(r: Record<string, unknown>) =>
				r.msg === "process exited" &&
				(r as Record<string, unknown>).command === "node",
		);
		expect(exitRecord).toBeDefined();
		expect(exitRecord).toHaveProperty("exitCode", 0);

		// Must contain driver mount diagnostics
		const mountRecord = records.find(
			(r: Record<string, unknown>) => r.msg === "runtime driver mounted",
		);
		expect(mountRecord).toBeDefined();

		// Every record must have a timestamp
		for (const record of records) {
			expect(record).toHaveProperty("time");
		}
	});

	it("redacts secret keys in log records", async () => {
		workDir = await mkdtemp(path.join(tmpdir(), "agent-os-debug-log-redact-"));
		logDir = await mkdtemp(
			path.join(tmpdir(), "agent-os-debug-log-redact-out-"),
		);
		const logPath = path.join(logDir, "debug.ndjson");

		shell = await createDevShellKernel({
			workDir,
			mountWasm: false,
			debugLogPath: logPath,
		});

		// Log a record that includes a sensitive key.
		shell.logger.info(
			{
				env: { ANTHROPIC_API_KEY: "sk-ant-secret-value", SAFE_VAR: "visible" },
			},
			"env snapshot",
		);

		await shell.dispose();
		shell = undefined;

		const logContent = await readFile(logPath, "utf8");
		expect(logContent).not.toContain("sk-ant-secret-value");
		expect(logContent).toContain("[REDACTED]");
		expect(logContent).toContain("visible");
	});
});
