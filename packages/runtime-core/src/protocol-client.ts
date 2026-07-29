import type { Readable, Writable } from "node:stream";
import {
	type LiveSidecarEventSelector,
	normalizeSidecarEventMatcher,
	SidecarEventBuffer,
	SidecarEventBufferOverflow,
	sidecarEventWaitAbortError,
} from "./event-buffer.js";
import { FrameRpcTransport } from "./frame-rpc.js";
import {
	type FrameTransport,
	SplitLaneFrameTransport,
	StdioFrameTransport,
} from "./frame-stream.js";
import type { LiveOwnershipScope } from "./ownership.js";
import {
	classifySidecarWrittenProtocolFrame,
	decodeProtocolFramePayload,
	encodeProtocolFramePayload,
	HostProtocolFrameFactory,
	type LiveControlFrame,
	type LiveEventFrame,
	type LiveProtocolFrame,
	type LiveRequestFrame,
	type LiveResponseFrame,
	type LiveSidecarRequestFrame,
	type LiveSidecarRequestHandler,
	type LiveSidecarResponseFrame,
	type ProtocolFramePayloadCodec,
	resolveSidecarRequestFramePayload,
} from "./protocol-frames.js";
import type { LiveRequestPayload } from "./request-payloads.js";
import {
	SidecarRejectedError,
	SidecarSilenceTimeout,
} from "./sidecar-errors.js";

/**
 * How long the host tolerates TOTAL inbound silence (no responses, events,
 * sidecar requests, or heartbeats) before declaring the sidecar dead. The
 * sidecar heartbeats every 10s from a dedicated thread, so this allows two
 * missed beats plus margin; it bounds "sidecar is dead or wedged", never "this
 * request is slow" — individual requests have no deadline of their own.
 */
const DEFAULT_SIDECAR_SILENCE_TIMEOUT_MS = 30_000;

export interface SidecarProtocolClientOptions {
	/** A combined transport remains supported for embedders and focused tests. */
	frameTransport?: FrameTransport<
		LiveResponseFrame | LiveEventFrame | LiveSidecarRequestFrame,
		LiveProtocolFrame
	>;
	ordinaryFrameTransport?: FrameTransport<LiveEventFrame, LiveRequestFrame>;
	controlFrameTransport?: FrameTransport<
		LiveResponseFrame | LiveSidecarRequestFrame | LiveEventFrame,
		LiveSidecarResponseFrame | LiveControlFrame
	>;
	stdin?: Writable;
	stdout?: Readable;
	control?: Readable & Writable;
	/** Use one bidirectional stdin/stdout transport when the host cannot inherit fd 3. */
	combinedStdio?: boolean;
	eventBufferCapacity: number;
	payloadCodec?: ProtocolFramePayloadCodec;
	stderrText?: () => string;
	frameError?: (error: Error) => Error;
	streamEndedError?: () => Error;
	/** Override the silence watchdog window. Tests only; production uses the default. */
	silenceTimeoutMs?: number;
	/**
	 * Runs when the silence watchdog fires, before pending work is rejected.
	 * The stdio layer uses it to SIGKILL the sidecar child.
	 */
	onSilenceExpired?: () => void;
}

export class SidecarProtocolClient {
	private readonly eventBuffer: SidecarEventBuffer<LiveEventFrame>;
	private readonly eventListeners = new Set<(event: LiveEventFrame) => void>();
	private readonly silenceTimeoutMs: number;
	private silenceTimer: ReturnType<typeof setInterval> | null = null;
	private lastInboundAtMs = 0;
	private readonly payloadCodec: ProtocolFramePayloadCodec;
	private readonly stderrText: () => string;
	private readonly hostFrameFactory = new HostProtocolFrameFactory();
	private readonly frameTransport: FrameRpcTransport<
		LiveResponseFrame | LiveEventFrame | LiveSidecarRequestFrame,
		LiveRequestFrame | LiveSidecarResponseFrame | LiveControlFrame,
		LiveResponseFrame,
		LiveEventFrame,
		LiveSidecarRequestFrame
	>;
	private closedError: Error | null = null;
	private readonly eventWaiters = new Set<{
		matches: (event: LiveEventFrame) => boolean;
		resolve: (event: LiveEventFrame) => void;
		reject: (error: Error) => void;
		timer: ReturnType<typeof setTimeout> | null;
	}>();
	private sidecarRequestHandler: LiveSidecarRequestHandler | null = null;

	constructor(options: SidecarProtocolClientOptions) {
		this.silenceTimeoutMs =
			options.silenceTimeoutMs ?? DEFAULT_SIDECAR_SILENCE_TIMEOUT_MS;
		this.eventBuffer = new SidecarEventBuffer(options.eventBufferCapacity);
		this.payloadCodec = options.payloadCodec ?? "bare";
		this.stderrText = options.stderrText ?? (() => "");
		this.frameTransport = new FrameRpcTransport<
			LiveResponseFrame | LiveEventFrame | LiveSidecarRequestFrame,
			LiveRequestFrame | LiveSidecarResponseFrame | LiveControlFrame,
			LiveResponseFrame,
			LiveEventFrame,
			LiveSidecarRequestFrame
		>({
			frameTransport: resolveProtocolFrameTransport(options, this.payloadCodec),
			encodeFrame: (frame) =>
				encodeProtocolFramePayload(frame, this.payloadCodec),
			decodeFrame: (payload) =>
				decodeProtocolFramePayload(payload, this.payloadCodec),
			classifyFrame: classifySidecarWrittenProtocolFrame,
		});
		this.frameTransport.onEvent((event) => {
			this.dispatchEvent(event);
		});
		this.frameTransport.onSidecarRequest((request) => {
			void this.dispatchSidecarRequest(request);
		});
		this.frameTransport.onError((error) => {
			this.failPermanently(options.frameError?.(error) ?? error);
		});
		this.frameTransport.onEnd(() => {
			this.failPermanently(
				options.streamEndedError?.() ??
					new Error("sidecar protocol stream ended"),
			);
		});
		this.frameTransport.onFrameActivity(() => {
			this.lastInboundAtMs = performance.now();
		});
		this.startSilenceWatchdog(options.onSilenceExpired);
	}

	/**
	 * Arm the silence watchdog: ANY inbound frame resets the clock (see the
	 * `onFrameActivity` tap above), and the sidecar heartbeats every 10s even
	 * while busy, so sustained silence for the full window means the process
	 * is dead or wedged — not slow. The check interval is unref'd so an idle
	 * host process can still exit naturally.
	 */
	private startSilenceWatchdog(onExpired?: () => void): void {
		this.lastInboundAtMs = performance.now();
		const checkIntervalMs = Math.max(
			Math.min(this.silenceTimeoutMs / 4, 1_000),
			10,
		);
		this.silenceTimer = setInterval(() => {
			const silenceMs = performance.now() - this.lastInboundAtMs;
			if (silenceMs < this.silenceTimeoutMs) {
				return;
			}
			this.stopSilenceWatchdog();
			const error = new SidecarSilenceTimeout({
				silenceMs,
				stderr: this.stderrText(),
			});
			try {
				onExpired?.();
			} finally {
				this.failPermanently(error);
			}
		}, checkIntervalMs);
		this.silenceTimer.unref?.();
	}

	private stopSilenceWatchdog(): void {
		if (this.silenceTimer !== null) {
			clearInterval(this.silenceTimer);
			this.silenceTimer = null;
		}
	}

	setSidecarRequestHandler(handler: LiveSidecarRequestHandler | null): void {
		this.sidecarRequestHandler = handler;
	}

	onEvent(handler: (event: LiveEventFrame) => void): () => void {
		this.eventListeners.add(handler);
		return () => {
			this.eventListeners.delete(handler);
		};
	}

	async sendRequest(input: {
		ownership: LiveOwnershipScope;
		payload: LiveRequestPayload;
	}): Promise<LiveResponseFrame> {
		if (this.closedError) {
			throw this.closedError;
		}

		const request = this.hostFrameFactory.createRequestFrame(input);
		// No per-request deadline: only the caller knows whether an operation is
		// legitimately long (a whole agent turn is one request). A dead or
		// wedged sidecar rejects this via the silence watchdog instead.
		const response = await this.frameTransport.sendFrame(
			request.request_id,
			request,
		);

		if (response.payload.type === "rejected") {
			throw new SidecarRejectedError(request.request_id, response.payload);
		}
		return response;
	}

	async shutdown(reason: string): Promise<void> {
		if (this.closedError) {
			throw this.closedError;
		}
		await this.frameTransport.writeFrame(
			this.hostFrameFactory.createControlFrame({ type: "shutdown", reason }),
		);
	}

	async waitForEvent(
		matcher: LiveSidecarEventSelector | ((event: LiveEventFrame) => boolean),
		timeoutMs?: number,
		options?: {
			signal?: AbortSignal;
		},
	): Promise<LiveEventFrame> {
		if (this.closedError instanceof SidecarEventBufferOverflow) {
			throw this.closedError;
		}
		const normalizedMatcher =
			normalizeSidecarEventMatcher<LiveEventFrame>(matcher);
		const bufferedEvent = this.eventBuffer.take(normalizedMatcher);
		if (bufferedEvent) {
			return bufferedEvent;
		}
		if (this.closedError) {
			throw this.closedError;
		}
		if (options?.signal?.aborted) {
			throw sidecarEventWaitAbortError(options.signal.reason);
		}

		return await new Promise<LiveEventFrame>((resolve, reject) => {
			let abortListener: (() => void) | null = null;
			const waiter = {
				matches: normalizedMatcher.matches,
				resolve: (event: LiveEventFrame) => {
					if (waiter.timer !== null) {
						clearTimeout(waiter.timer);
					}
					if (abortListener) {
						options?.signal?.removeEventListener("abort", abortListener);
						abortListener = null;
					}
					this.eventWaiters.delete(waiter);
					resolve(event);
				},
				reject: (error: Error) => {
					if (waiter.timer !== null) {
						clearTimeout(waiter.timer);
					}
					if (abortListener) {
						options?.signal?.removeEventListener("abort", abortListener);
						abortListener = null;
					}
					this.eventWaiters.delete(waiter);
					reject(error);
				},
				timer:
					timeoutMs === undefined
						? null
						: setTimeout(() => {
								this.eventWaiters.delete(waiter);
								reject(
									new Error(
										`timed out waiting for sidecar event\nstderr:\n${this.stderrText()}`,
									),
								);
							}, timeoutMs),
			};
			if (options?.signal) {
				abortListener = () => {
					waiter.reject(sidecarEventWaitAbortError(options.signal?.reason));
				};
				options.signal.addEventListener("abort", abortListener, { once: true });
			}
			this.eventWaiters.add(waiter);
		});
	}

	failPermanently(
		error: Error,
		options?: {
			replaceExisting?: (current: Error, next: Error) => boolean;
		},
	): void {
		if (this.closedError) {
			if (!options?.replaceExisting?.(this.closedError, error)) {
				return;
			}
		}
		this.closedError = error;
		this.stopSilenceWatchdog();
		this.rejectPending(error);
	}

	dispose(): void {
		this.stopSilenceWatchdog();
		this.frameTransport.dispose();
	}

	private async writeFrame(frame: LiveSidecarResponseFrame): Promise<void> {
		await this.frameTransport.writeFrame(frame);
	}

	private async dispatchSidecarRequest(
		request: LiveSidecarRequestFrame,
	): Promise<void> {
		// A callback can arrive immediately before the response that acknowledges
		// the command which created it. Let the inbound decoder finish that turn
		// before writing a callback response; otherwise a very fast host callback
		// can complete and reap the guest process while its execute request is
		// still being admitted, leaving the host waiting for an exit it missed.
		await new Promise<void>((resolve) => {
			const scheduleImmediate = (
				globalThis as typeof globalThis & {
					setImmediate?: (callback: () => void) => unknown;
				}
			).setImmediate;
			if (scheduleImmediate) {
				scheduleImmediate(resolve);
			} else {
				setTimeout(resolve, 0);
			}
		});
		const payload = await resolveSidecarRequestFramePayload(
			request,
			this.sidecarRequestHandler,
		);

		try {
			await this.writeFrame(
				this.hostFrameFactory.createSidecarResponseFrame({
					request,
					payload,
				}),
			);
		} catch (error) {
			const normalized =
				error instanceof Error ? error : new Error(String(error));
			this.failPermanently(normalized);
		}
	}

	private dispatchEvent(event: LiveEventFrame): void {
		// Transport-level liveness beats from the sidecar. Their arrival already
		// reset the silence watchdog at the frame layer; they carry no meaning
		// for consumers and must never reach the bounded event buffer, where a
		// long-idle VM would accumulate one every 10s until overflow.
		if (
			event.payload.type === "structured" &&
			event.payload.name === "heartbeat"
		) {
			return;
		}
		for (const listener of this.eventListeners) {
			try {
				listener(event);
			} catch {
				// Event listeners are best-effort observers and must not break framing.
			}
		}
		for (const waiter of this.eventWaiters) {
			if (!waiter.matches(event)) {
				continue;
			}
			waiter.resolve(event);
			return;
		}
		this.bufferEvent(event);
	}

	private bufferEvent(event: LiveEventFrame): void {
		const overflow = this.eventBuffer.buffer(event);
		if (overflow) {
			this.failPermanently(overflow);
		}
	}

	private rejectPending(error: Error): void {
		this.frameTransport.rejectAll(error);
		for (const waiter of this.eventWaiters) {
			waiter.reject(error);
		}
		this.eventWaiters.clear();
	}
}

function resolveProtocolFrameTransport(
	options: SidecarProtocolClientOptions,
	payloadCodec: ProtocolFramePayloadCodec,
): FrameTransport<
	LiveResponseFrame | LiveEventFrame | LiveSidecarRequestFrame,
	LiveRequestFrame | LiveSidecarResponseFrame | LiveControlFrame
> {
	const hasSplitTransport =
		options.ordinaryFrameTransport !== undefined ||
		options.controlFrameTransport !== undefined;
	const hasStdio =
		options.stdin !== undefined ||
		options.stdout !== undefined ||
		options.control !== undefined;
	if (options.combinedStdio) {
		if (
			options.frameTransport ||
			hasSplitTransport ||
			options.control !== undefined ||
			!options.stdin ||
			!options.stdout
		) {
			throw new Error(
				"SidecarProtocolClient combinedStdio requires only stdin and stdout streams",
			);
		}
		return new StdioFrameTransport({
			stdin: options.stdin,
			stdout: options.stdout,
			encodeFrame: (frame) => encodeProtocolFramePayload(frame, payloadCodec),
			decodeFrame: (payload) =>
				decodeProtocolFramePayload(payload, payloadCodec) as
					| LiveResponseFrame
					| LiveEventFrame
					| LiveSidecarRequestFrame,
		});
	}
	if (options.frameTransport) {
		if (hasSplitTransport || hasStdio) {
			throw new Error(
				"SidecarProtocolClient frameTransport cannot be combined with split-lane transports or streams",
			);
		}
		return options.frameTransport;
	}

	let ordinary = options.ordinaryFrameTransport;
	let control = options.controlFrameTransport;
	if (hasSplitTransport) {
		if (!ordinary || !control || hasStdio) {
			throw new Error(
				"SidecarProtocolClient requires both ordinaryFrameTransport and controlFrameTransport, without stdio streams",
			);
		}
	} else {
		if (!options.stdin || !options.stdout || !options.control) {
			throw new Error(
				"SidecarProtocolClient requires a combined frameTransport or stdin/stdout/control streams",
			);
		}
		ordinary = new StdioFrameTransport<LiveEventFrame, LiveRequestFrame>({
			stdin: options.stdin,
			stdout: options.stdout,
			encodeFrame: (frame) => encodeProtocolFramePayload(frame, payloadCodec),
			decodeFrame: (payload) => {
				const frame = decodeProtocolFramePayload(payload, payloadCodec);
				validateOrdinaryInboundFrame(frame);
				return frame;
			},
		});
		control = new StdioFrameTransport<
			LiveResponseFrame | LiveSidecarRequestFrame | LiveEventFrame,
			LiveSidecarResponseFrame | LiveControlFrame
		>({
			stdin: options.control,
			stdout: options.control,
			encodeFrame: (frame) => encodeProtocolFramePayload(frame, payloadCodec),
			decodeFrame: (payload) => {
				const frame = decodeProtocolFramePayload(payload, payloadCodec);
				validateControlInboundFrame(frame);
				return frame;
			},
		});
	}

	return new SplitLaneFrameTransport({
		ordinary,
		control,
		validateOrdinaryFrame: validateOrdinaryInboundFrame,
		validateControlFrame: validateControlInboundFrame,
		selectWriteLane: (frame) => {
			switch (frame.frame_type) {
				case "request":
					return "ordinary";
				case "sidecar_response":
				case "control":
					return "control";
				default: {
					const invalidFrame = frame as { frame_type: string };
					throw new Error(
						`host frame ${invalidFrame.frame_type} is not valid on either writable protocol lane`,
					);
				}
			}
		},
	});
}

function validateOrdinaryInboundFrame(frame: {
	frame_type: string;
	payload?: unknown;
}): asserts frame is LiveEventFrame {
	if (frame.frame_type !== "event") {
		throw new Error(
			`sidecar frame ${frame.frame_type} is not valid on the ordinary protocol lane`,
		);
	}
	if (isHeartbeatFrame(frame as LiveEventFrame)) {
		throw new Error(
			"sidecar heartbeat event is not valid on the ordinary protocol lane",
		);
	}
}

function validateControlInboundFrame(frame: {
	frame_type: string;
	payload?: unknown;
}): asserts frame is
	| LiveResponseFrame
	| LiveSidecarRequestFrame
	| LiveEventFrame {
	if (
		frame.frame_type === "response" ||
		frame.frame_type === "sidecar_request"
	) {
		return;
	}
	if (
		frame.frame_type === "event" &&
		isHeartbeatFrame(frame as LiveEventFrame)
	) {
		return;
	}
	throw new Error(
		`sidecar frame ${frame.frame_type} is not valid on the control protocol lane`,
	);
}

function isHeartbeatFrame(frame: LiveEventFrame): boolean {
	return (
		frame.payload.type === "structured" && frame.payload.name === "heartbeat"
	);
}
