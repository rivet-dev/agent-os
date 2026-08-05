import type { Readable, Writable } from "node:stream";
import { PendingResponseRegistry } from "./correlation.js";
import {
	type FrameTransport,
	StdioFrameTransport,
} from "./frame-stream.js";

export type ClassifiedFrame<TResponseFrame, TEventFrame, TSidecarRequestFrame> =
	| {
			kind: "response";
			requestId: number;
			frame: TResponseFrame;
	  }
	| {
			kind: "event";
			frame: TEventFrame;
	  }
	| {
			kind: "sidecarRequest";
			frame: TSidecarRequestFrame;
	  };

export interface FrameRpcTransportOptions<
	TReadFrame,
	TWriteFrame,
	TResponseFrame,
	TEventFrame,
	TSidecarRequestFrame,
> {
	frameTransport?: FrameTransport<TReadFrame, TWriteFrame>;
	stdin?: Writable;
	stdout?: Readable;
	encodeFrame: (frame: TWriteFrame) => Uint8Array;
	decodeFrame: (payload: Uint8Array) => TReadFrame;
	classifyFrame: (
		frame: TReadFrame,
	) => ClassifiedFrame<TResponseFrame, TEventFrame, TSidecarRequestFrame>;
}

export class FrameRpcTransport<
	TReadFrame,
	TWriteFrame,
	TResponseFrame,
	TEventFrame,
	TSidecarRequestFrame,
> {
	private readonly frameTransport: FrameTransport<TReadFrame, TWriteFrame>;
	private readonly pendingResponses =
		new PendingResponseRegistry<TResponseFrame>();
	private readonly eventListeners = new Set<(event: TEventFrame) => void>();
	private readonly sidecarRequestListeners = new Set<
		(request: TSidecarRequestFrame) => void
	>();
	private readonly frameActivityListeners = new Set<() => void>();

	constructor(
		options: FrameRpcTransportOptions<
			TReadFrame,
			TWriteFrame,
			TResponseFrame,
			TEventFrame,
			TSidecarRequestFrame
		>,
	) {
		if (options.frameTransport) {
			this.frameTransport = options.frameTransport;
		} else {
			if (!options.stdin || !options.stdout) {
				throw new Error(
					"FrameRpcTransport requires either frameTransport or stdin/stdout streams",
				);
			}
			this.frameTransport = new StdioFrameTransport<TReadFrame, TWriteFrame>({
				stdin: options.stdin,
				stdout: options.stdout,
				encodeFrame: options.encodeFrame,
				decodeFrame: options.decodeFrame,
			});
		}
		this.frameTransport.onFrame((frame) => {
			this.dispatchFrame(options.classifyFrame(frame));
		});
	}

	onEvent(handler: (event: TEventFrame) => void): () => void {
		this.eventListeners.add(handler);
		return () => {
			this.eventListeners.delete(handler);
		};
	}

	/**
	 * Observe every classified inbound frame (response, event, or sidecar
	 * request) before it is routed. This is the transport's liveness signal:
	 * the silence watchdog resets on each invocation, so ANY inbound traffic —
	 * not just heartbeats — proves the sidecar is alive.
	 */
	onFrameActivity(handler: () => void): () => void {
		this.frameActivityListeners.add(handler);
		return () => {
			this.frameActivityListeners.delete(handler);
		};
	}

	onSidecarRequest(handler: (request: TSidecarRequestFrame) => void): () => void {
		this.sidecarRequestListeners.add(handler);
		return () => {
			this.sidecarRequestListeners.delete(handler);
		};
	}

	onError(handler: (error: Error) => void): () => void {
		return this.frameTransport.onError(handler);
	}

	onEnd(handler: () => void): () => void {
		return this.frameTransport.onEnd(handler);
	}

	async sendFrame(
		requestId: number,
		frame: TWriteFrame,
	): Promise<TResponseFrame> {
		const response = this.pendingResponses.waitForResponse(requestId);
		void this.writeFrame(frame).catch((error) => {
			this.pendingResponses.reject(
				requestId,
				error instanceof Error ? error : new Error(String(error)),
			);
		});
		return await response;
	}

	async writeFrame(frame: TWriteFrame): Promise<void> {
		await this.frameTransport.writeFrame(frame);
	}

	rejectAll(error: Error): void {
		this.pendingResponses.rejectAll(error);
	}

	dispose(): void {
		this.frameTransport.dispose();
		this.pendingResponses.rejectAll(new Error("frame rpc transport disposed"));
		this.eventListeners.clear();
		this.sidecarRequestListeners.clear();
		this.frameActivityListeners.clear();
	}

	private dispatchFrame(
		classified: ClassifiedFrame<
			TResponseFrame,
			TEventFrame,
			TSidecarRequestFrame
		>,
	): void {
		for (const listener of this.frameActivityListeners) {
			listener();
		}
		switch (classified.kind) {
			case "response":
				this.pendingResponses.resolve(
					classified.requestId,
					classified.frame,
				);
				return;
			case "event":
				for (const listener of this.eventListeners) {
					listener(classified.frame);
				}
				return;
			case "sidecarRequest":
				for (const listener of this.sidecarRequestListeners) {
					listener(classified.frame);
				}
				return;
		}
	}
}
