#!/usr/bin/env node

import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import { constants as osConstants } from "node:os";

const MAX_CAPTURE_BYTES = 64 * 1024;
const MAX_OUTPUT_COUNT = 0xffff_ffff;
const TERMINATION_GRACE_MS = 500;
const FORWARDED_ENV_KEYS = new Set([
	"ACP_APPEND_SYSTEM_PROMPT",
	"AI_GATEWAY_API_KEY",
	"VERCEL_OIDC_TOKEN",
	"VERCEL_TOKEN",
]);
const WORKSPACE_ENV_KEYS = new Set([
	"HOME",
	"LANG",
	"LC_ALL",
	"LC_CTYPE",
	"LOGNAME",
	"PATH",
	"SHELL",
	"TEMP",
	"TERM",
	"TMP",
	"TMPDIR",
	"TZ",
	"USER",
]);

type FxRuntime = {
	write(data: string | Uint8Array): void;
	closeStdin(): void;
	abort(): void;
	setLineHandler(handler: (message: unknown) => void): void;
	exited: Promise<number>;
};

type WorkspaceExecInput = {
	command: string;
	cwd: string;
	signal: AbortSignal;
	timeoutMs: number;
	outputLimitBytes: number;
};

type FxSdk = {
	createFxAcpRuntime(options: Record<string, unknown>): Promise<FxRuntime>;
};

type WorkspaceExecResult = {
	exitCode: number;
	stdout: string;
	stderr: string;
	stdoutTotalBytes: number;
	stderrTotalBytes: number;
};

function wasmEnvironment(): Record<string, string> {
	return Object.fromEntries(
		Object.entries(process.env).filter(
			(entry): entry is [string, string] =>
				entry[1] !== undefined &&
				(entry[0].startsWith("FX_") || FORWARDED_ENV_KEYS.has(entry[0])),
		),
	);
}

function workspaceEnvironment(): NodeJS.ProcessEnv {
	return Object.fromEntries(
		Object.entries(process.env).filter(
			(entry): entry is [string, string] =>
				entry[1] !== undefined && WORKSPACE_ENV_KEYS.has(entry[0]),
		),
	);
}

function appendBounded(
	chunks: Buffer[],
	chunk: Buffer,
	captured: number,
	limit: number,
): number {
	const remaining = limit - captured;
	if (remaining <= 0) return captured;
	const accepted = chunk.subarray(0, remaining);
	chunks.push(accepted);
	return captured + accepted.length;
}

function addOutputBytes(total: number, chunk: Buffer): number {
	return Math.min(MAX_OUTPUT_COUNT, total + chunk.length);
}

function capturedOutput(
	chunks: Buffer[],
	totalBytes: number,
): { text: string; totalBytes: number } {
	const text = Buffer.concat(chunks).toString("utf8");
	return {
		text,
		totalBytes: Math.max(totalBytes, Buffer.byteLength(text)),
	};
}

function signalExitCode(signal: NodeJS.Signals | null): number {
	if (!signal) return 1;
	const number = osConstants.signals[signal];
	return number === undefined ? 128 : 128 + number;
}

function killWorkspaceProcessGroup(
	child: ReturnType<typeof spawn>,
	signal: NodeJS.Signals,
): boolean {
	if (child.pid === undefined || child.pid <= 0) return child.kill(signal);
	try {
		return process.kill(-child.pid, signal);
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code === "ESRCH") return false;
		reportWorkspaceError(error);
		return child.kill(signal);
	}
}

function reportWorkspaceError(error: unknown): void {
	const detail = error instanceof Error ? (error.stack ?? error.message) : String(error);
	process.stderr.write(`agentos-fx-acp: workspace command failed: ${detail}\n`);
}

async function runWorkspaceCommand({
	command,
	cwd,
	signal,
	outputLimitBytes,
}: WorkspaceExecInput): Promise<WorkspaceExecResult> {
	if (!Number.isSafeInteger(outputLimitBytes) || outputLimitBytes < 0) {
		throw new TypeError("workspace output limit must be a non-negative safe integer");
	}
	const captureLimit = Math.min(MAX_CAPTURE_BYTES, outputLimitBytes);
	return new Promise((resolve, reject) => {
		const child = spawn(command, {
			cwd,
			detached: true,
			env: workspaceEnvironment(),
			shell: true,
			stdio: ["ignore", "pipe", "pipe"],
		});
		const stdout: Buffer[] = [];
		const stderr: Buffer[] = [];
		let stdoutBytes = 0;
		let stderrBytes = 0;
		let stdoutTotalBytes = 0;
		let stderrTotalBytes = 0;
		let settled = false;
		let forceKillTimer: NodeJS.Timeout | undefined;

		const cleanup = () => signal.removeEventListener("abort", abort);
		const clearForceKill = () => {
			if (forceKillTimer !== undefined) clearTimeout(forceKillTimer);
			forceKillTimer = undefined;
		};
		const fail = (error: unknown) => {
			if (settled) return;
			settled = true;
			cleanup();
			reject(error);
		};
		const finishSpawnError = (error: NodeJS.ErrnoException) => {
			if (settled) return;
			settled = true;
			cleanup();
			clearForceKill();
			reportWorkspaceError(error);
			const stderr = error.message;
			resolve({
				exitCode: error.code === "EACCES" ? 126 : 127,
				stdout: "",
				stderr,
				stdoutTotalBytes: 0,
				stderrTotalBytes: Buffer.byteLength(stderr),
			});
		};
		const abort = () => {
			if (
				child.exitCode === null &&
				child.signalCode === null &&
				killWorkspaceProcessGroup(child, "SIGTERM")
			) {
				forceKillTimer = setTimeout(() => {
					forceKillTimer = undefined;
					if (
						child.exitCode === null &&
						child.signalCode === null &&
						!killWorkspaceProcessGroup(child, "SIGKILL")
					) {
						process.stderr.write(
							"agentos-fx-acp: workspace command resisted SIGTERM and SIGKILL\n",
						);
					}
				}, TERMINATION_GRACE_MS);
				forceKillTimer.unref();
			} else if (child.exitCode === null && child.signalCode === null) {
				process.stderr.write("agentos-fx-acp: workspace command was not running when cancellation arrived\n");
			}
			fail(signal.reason ?? new DOMException("workspace command aborted", "AbortError"));
		};

		child.stdout.on("data", (chunk: Buffer | string) => {
			const buffer = Buffer.from(chunk);
			stdoutTotalBytes = addOutputBytes(stdoutTotalBytes, buffer);
			stdoutBytes = appendBounded(stdout, buffer, stdoutBytes, captureLimit);
		});
		child.stderr.on("data", (chunk: Buffer | string) => {
			const buffer = Buffer.from(chunk);
			stderrTotalBytes = addOutputBytes(stderrTotalBytes, buffer);
			stderrBytes = appendBounded(stderr, buffer, stderrBytes, captureLimit);
		});
		child.once("error", finishSpawnError);
		child.once("close", (code, closeSignal) => {
			clearForceKill();
			if (settled) return;
			settled = true;
			cleanup();
			const stdoutResult = capturedOutput(stdout, stdoutTotalBytes);
			const stderrResult = capturedOutput(stderr, stderrTotalBytes);
			resolve({
				exitCode: code ?? signalExitCode(closeSignal),
				stdout: stdoutResult.text,
				stderr: stderrResult.text,
				stdoutTotalBytes: stdoutResult.totalBytes,
				stderrTotalBytes: stderrResult.totalBytes,
			});
		});

		if (signal.aborted) abort();
		else signal.addEventListener("abort", abort, { once: true });
	});
}

async function execWorkspace(input: WorkspaceExecInput): Promise<WorkspaceExecResult> {
	try {
		return await runWorkspaceCommand(input);
	} catch (error) {
		if (error instanceof DOMException && (error.name === "AbortError" || error.name === "TimeoutError")) {
			throw error;
		}
		reportWorkspaceError(error);
		return {
			exitCode: 127,
			stdout: "",
			stderr: error instanceof Error ? error.message : String(error),
			stdoutTotalBytes: 0,
			stderrTotalBytes: Buffer.byteLength(
				error instanceof Error ? error.message : String(error),
			),
		};
	}
}

async function main(): Promise<void> {
	// Generated from a pinned and patched upstream FX source tree during build.
	// @ts-expect-error The generated SDK intentionally has no handwritten declaration file.
	const sdk = (await import("./fx-sdk.js")) as FxSdk;
	const cwd = process.cwd();
	const home = process.env.HOME?.startsWith("/") ? process.env.HOME : "/home/agentos";
	const wasm = readFileSync(new URL("./fx-core.wasm", import.meta.url));
	const runtime = await sdk.createFxAcpRuntime({
		wasm,
		env: wasmEnvironment(),
		fetch: globalThis.fetch.bind(globalThis),
		workspace: {
			info: {
				version: 1,
				root: cwd,
				cwd,
				home,
				gitAvailable: false,
				ephemeral: true,
			},
			permission: "allow-sandboxed",
			exec: execWorkspace,
		},
	});

	runtime.setLineHandler((message) => {
		process.stdout.write(`${JSON.stringify(message)}\n`);
	});
	process.stdin.on("data", (chunk: Buffer | string) => runtime.write(Buffer.from(chunk)));
	process.stdin.once("end", () => runtime.closeStdin());
	process.stdin.once("error", (error) => {
		process.stderr.write(`agentos-fx-acp: stdin failed: ${error.stack ?? error.message}\n`);
		runtime.abort();
	});

	const exitCode = await runtime.exited;
	if (exitCode !== 0) process.exitCode = exitCode;
}

main().catch((error: unknown) => {
	const detail = error instanceof Error ? (error.stack ?? error.message) : String(error);
	process.stderr.write(`agentos-fx-acp: ${detail}\n`);
	process.exitCode = 1;
});
