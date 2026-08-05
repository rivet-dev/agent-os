import { resolvePublishedSidecarBinary } from "./binary.js";
import type { LiveSidecarEventSelector } from "./event-buffer.js";
import type { LiveOwnershipScope } from "./ownership.js";
import { SidecarProcessExited, StdioSidecarProcess } from "./process.js";
import { SidecarProtocolClient } from "./protocol-client.js";
import type {
	LiveEventFrame,
	LiveResponseFrame,
	LiveSidecarRequestHandler,
	ProtocolFramePayloadCodec,
} from "./protocol-frames.js";
import type { LiveRequestPayload } from "./request-payloads.js";
import type { SidecarProcessTransport } from "./sidecar-client.js";
import { registerSidecarProcessSpawnFactory } from "./sidecar-process.js";

export const DEFAULT_SIDECAR_EVENT_BUFFER_CAPACITY = 4_096;
export const DEFAULT_SIDECAR_GRACEFUL_EXIT_MS = 5_000;
export const DEFAULT_SIDECAR_FORCE_EXIT_MS = 2_000;

export interface StdioSidecarProtocolClientSpawnOptions {
	cwd?: string;
	command?: string;
	args?: string[];
	eventBufferCapacity?: number;
	gracefulExitMs?: number;
	forceExitMs?: number;
	disposedErrorMessage?: string;
	payloadCodec?: ProtocolFramePayloadCodec;
	/**
	 * Override the silence watchdog window (default 30s). Tests only — the
	 * window is a fixed protocol constant paired with the sidecar's 10s
	 * heartbeat cadence, not an operator tunable.
	 */
	silenceTimeoutMs?: number;
}

type ResolvedStdioSidecarProtocolClientOptions = Required<
	Pick<
		StdioSidecarProtocolClientSpawnOptions,
		| "eventBufferCapacity"
		| "gracefulExitMs"
		| "forceExitMs"
		| "disposedErrorMessage"
		| "payloadCodec"
	>
> &
	Pick<StdioSidecarProtocolClientSpawnOptions, "silenceTimeoutMs">;

export class StdioSidecarProtocolClient implements SidecarProcessTransport {
	readonly child: StdioSidecarProcess["child"];
	private readonly sidecarProcess: StdioSidecarProcess;
	private readonly protocolClient: SidecarProtocolClient;
	private readonly gracefulExitMs: number;
	private readonly forceExitMs: number;
	private readonly disposedErrorMessage: string;

	private constructor(
		sidecarProcess: StdioSidecarProcess,
		options: ResolvedStdioSidecarProtocolClientOptions,
	) {
		this.sidecarProcess = sidecarProcess;
		this.child = sidecarProcess.child;
		this.gracefulExitMs = options.gracefulExitMs;
		this.forceExitMs = options.forceExitMs;
		this.disposedErrorMessage = options.disposedErrorMessage;
		const transportOptions = this.sidecarProcess.combinedStdio
			? {
					stdin: this.child.stdin,
					stdout: this.child.stdout,
					combinedStdio: true as const,
				}
			: {
					stdin: this.child.stdin,
					stdout: this.child.stdout,
					control: this.sidecarProcess.control!,
				};
		this.protocolClient = new SidecarProtocolClient({
			...transportOptions,
			eventBufferCapacity: options.eventBufferCapacity,
			payloadCodec: options.payloadCodec,
			silenceTimeoutMs: options.silenceTimeoutMs,
			// A silent sidecar is dead or wedged; reap the process so it cannot
			// linger as a zombie holding VM resources. The watchdog then rejects
			// all in-flight requests with `SidecarSilenceTimeout`.
			onSilenceExpired: () => {
				try {
					this.child.kill("SIGKILL");
				} catch {
					// The child may have exited between the check and the kill.
				}
			},
			stderrText: () => this.sidecarProcess.stderrText(),
			streamEndedError: () =>
				this.sidecarProcess.currentExitError() ??
				new SidecarProcessExited({
					exitCode: this.child.exitCode,
					signal: this.child.signalCode,
					stderr: this.sidecarProcess.stderrText(),
				}),
			frameError: (error) => this.sidecarProcess.currentExitError() ?? error,
		});
		this.sidecarProcess.onExit((error) => {
			this.failPermanently(error);
		});
		this.sidecarProcess.onError((error) => {
			this.failPermanently(error);
		});
	}

	static spawn(
		options: StdioSidecarProtocolClientSpawnOptions = {},
	): StdioSidecarProtocolClient {
		const combinedStdio =
			typeof (
				globalThis as typeof globalThis & {
					_childProcessSpawnStart?: unknown;
				}
			)._childProcessSpawnStart !== "undefined";
		return new StdioSidecarProtocolClient(
			StdioSidecarProcess.spawn({
				command: options.command ?? resolvePublishedSidecarBinary(),
				args: options.args ?? [],
				cwd: options.cwd,
				combinedStdio,
			}),
			{
				silenceTimeoutMs: options.silenceTimeoutMs,
				eventBufferCapacity:
					options.eventBufferCapacity ?? DEFAULT_SIDECAR_EVENT_BUFFER_CAPACITY,
				gracefulExitMs:
					options.gracefulExitMs ?? DEFAULT_SIDECAR_GRACEFUL_EXIT_MS,
				forceExitMs: options.forceExitMs ?? DEFAULT_SIDECAR_FORCE_EXIT_MS,
				disposedErrorMessage:
					options.disposedErrorMessage ?? "sidecar client disposed",
				payloadCodec: options.payloadCodec ?? "bare",
			},
		);
	}

	setSidecarRequestHandler(handler: LiveSidecarRequestHandler | null): void {
		this.protocolClient.setSidecarRequestHandler(handler);
	}

	onEvent(handler: (event: LiveEventFrame) => void): () => void {
		return this.protocolClient.onEvent(handler);
	}

	async sendRequest(input: {
		ownership: LiveOwnershipScope;
		payload: LiveRequestPayload;
	}): Promise<LiveResponseFrame> {
		return await this.protocolClient.sendRequest(input);
	}

	async waitForEvent(
		matcher: LiveSidecarEventSelector | ((event: LiveEventFrame) => boolean),
		timeoutMs?: number,
		options?: {
			signal?: AbortSignal;
		},
	): Promise<LiveEventFrame> {
		return await this.protocolClient.waitForEvent(matcher, timeoutMs, options);
	}

	async dispose(): Promise<void> {
		let shutdownError: Error | null = null;
		try {
			await this.protocolClient.shutdown(this.disposedErrorMessage);
		} catch (error) {
			shutdownError = error instanceof Error ? error : new Error(String(error));
		}
		this.protocolClient.failPermanently(new Error(this.disposedErrorMessage));

		if (!this.child.stdin.destroyed) {
			try {
				this.child.stdin.end();
			} catch {
				// Stdin may already be closing. The child exit watcher will catch up.
			}
		}
		if (this.sidecarProcess.control && !this.sidecarProcess.control.destroyed) {
			try {
				this.sidecarProcess.control.end();
			} catch {
				// The control socket may already be closing with the child.
			}
		}

		const exitCode = await this.sidecarProcess.waitForExit(this.gracefulExitMs);
		if (exitCode === null) {
			try {
				this.child.kill("SIGKILL");
			} catch {
				// The child may have exited between the timeout and the kill attempt.
			}
			await this.sidecarProcess.waitForExit(this.forceExitMs);
		}

		this.protocolClient.dispose();
		try {
			this.child.stdin.destroy();
		} catch {
			// Best effort. The child is gone so the descriptor will close on its own.
		}
		try {
			this.child.stdout.destroy();
		} catch {
			// Best effort. The child is gone so the descriptor will close on its own.
		}
		try {
			this.child.stderr.destroy();
		} catch {
			// Best effort. The child is gone so the descriptor will close on its own.
		}
		if (this.sidecarProcess.control) {
			try {
				this.sidecarProcess.control.destroy();
			} catch {
				// Best effort. The child is gone so the descriptor will close on its own.
			}
		}

		if (exitCode !== null && exitCode !== 0 && this.child.signalCode === null) {
			throw new Error(
				`sidecar exited with code ${exitCode}\nstderr:\n${this.sidecarProcess.stderrText()}`,
			);
		}
		if (shutdownError && this.child.exitCode !== 0) {
			throw shutdownError;
		}
	}

	failPermanently(error: Error): void {
		this.protocolClient.failPermanently(error, {
			replaceExisting: (current, next) =>
				current instanceof SidecarProcessExited &&
				current.exitCode === null &&
				current.signal === null &&
				next instanceof SidecarProcessExited &&
				(next.exitCode !== null || next.signal !== null),
		});
	}
}

registerSidecarProcessSpawnFactory((options) =>
	StdioSidecarProtocolClient.spawn(options),
);
