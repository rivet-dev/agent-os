import { type ChildProcessWithoutNullStreams, spawn } from "node:child_process";
import type { Duplex } from "node:stream";

export {
	SidecarProcessError,
	SidecarProcessExited,
} from "./sidecar-errors.js";

import { SidecarProcessError, SidecarProcessExited } from "./sidecar-errors.js";

export interface StdioSidecarProcessSpawnOptions {
	command: string;
	args?: string[];
	cwd?: string;
	combinedStdio?: boolean;
}

export class StdioSidecarProcess {
	readonly child: ChildProcessWithoutNullStreams;
	readonly control: Duplex | null;
	readonly combinedStdio: boolean;
	private readonly stderrChunks: Buffer[] = [];
	private readonly exitListeners = new Set<
		(error: SidecarProcessExited) => void
	>();
	private readonly errorListeners = new Set<
		(error: SidecarProcessError) => void
	>();

	private constructor(
		child: ChildProcessWithoutNullStreams,
		control: Duplex | null,
	) {
		this.child = child;
		this.control = control;
		this.combinedStdio = control === null;
		this.child.stderr.on("data", (chunk: Buffer | string) => {
			this.stderrChunks.push(
				typeof chunk === "string" ? Buffer.from(chunk) : Buffer.from(chunk),
			);
		});
		this.child.on("exit", (code, signal) => {
			const error = new SidecarProcessExited({
				exitCode: code,
				signal,
				stderr: this.stderrText(),
			});
			for (const listener of this.exitListeners) {
				listener(error);
			}
		});
		this.child.on("error", (error) => {
			const normalized =
				error instanceof Error ? error : new Error(String(error));
			const sidecarError = new SidecarProcessError(
				normalized,
				this.stderrText(),
			);
			for (const listener of this.errorListeners) {
				listener(sidecarError);
			}
		});
	}

	static spawn(options: StdioSidecarProcessSpawnOptions): StdioSidecarProcess {
		const combinedStdio = options.combinedStdio === true;
		const child = spawn(options.command, options.args ?? [], {
			cwd: options.cwd,
			env: combinedStdio
				? { ...process.env, AGENTOS_SIDECAR_COMBINED_STDIO: "1" }
				: process.env,
			stdio: combinedStdio
				? ["pipe", "pipe", "pipe"]
				: ["pipe", "pipe", "pipe", "pipe"],
		}) as unknown as ChildProcessWithoutNullStreams;
		try {
			return new StdioSidecarProcess(
				child,
				combinedStdio ? null : requireControlStream(child),
			);
		} catch (error) {
			child.kill("SIGKILL");
			throw error;
		}
	}

	static fromChild(
		child: ChildProcessWithoutNullStreams,
		control?: Duplex | null,
	): StdioSidecarProcess {
		return new StdioSidecarProcess(
			child,
			control === null ? null : (control ?? requireControlStream(child)),
		);
	}

	onExit(handler: (error: SidecarProcessExited) => void): () => void {
		this.exitListeners.add(handler);
		return () => {
			this.exitListeners.delete(handler);
		};
	}

	onError(handler: (error: SidecarProcessError) => void): () => void {
		this.errorListeners.add(handler);
		return () => {
			this.errorListeners.delete(handler);
		};
	}

	stderrText(): string {
		return Buffer.concat(this.stderrChunks).toString("utf8").trim();
	}

	currentExitError(): SidecarProcessExited | null {
		if (this.child.exitCode === null && this.child.signalCode === null) {
			return null;
		}
		return new SidecarProcessExited({
			exitCode: this.child.exitCode,
			signal: this.child.signalCode,
			stderr: this.stderrText(),
		});
	}

	waitForExit(timeoutMs: number): Promise<number | null> {
		return new Promise<number | null>((resolve) => {
			let timer: ReturnType<typeof setTimeout> | null = null;
			const cleanup = () => {
				this.child.off("exit", onExit);
				this.child.off("close", onClose);
				if (timer !== null) {
					clearTimeout(timer);
					timer = null;
				}
			};
			const onExit = (code: number | null) => {
				cleanup();
				resolve(code);
			};
			const onClose = (code: number | null) => {
				cleanup();
				resolve(code);
			};
			if (this.child.exitCode !== null || this.child.signalCode !== null) {
				resolve(this.child.exitCode);
				return;
			}
			this.child.on("exit", onExit);
			this.child.on("close", onClose);
			timer = setTimeout(() => {
				cleanup();
				resolve(null);
			}, timeoutMs);
		});
	}
}

function requireControlStream(child: ChildProcessWithoutNullStreams): Duplex {
	const stream = child.stdio[3];
	if (
		!stream ||
		typeof (stream as Duplex).write !== "function" ||
		typeof (stream as Duplex).on !== "function" ||
		typeof (stream as Duplex).read !== "function"
	) {
		throw new Error("sidecar process did not expose a full-duplex fd 3");
	}
	return stream as Duplex;
}
