// Execution-worker side of the converged async-agent SAB channel (the counterpart
// to KernelReactor; see AGENTOS-WEB-ASYNC-AGENTS.md §3.2). An agent/guest execution
// worker uses this to: write stdout/stderr/exit to the kernel (non-blocking SAB
// writes + a GEN signal), and make a synchronous kernel syscall (write a SYSCALL
// frame, then BLOCK on the down-channel for the result). The blocking syscall shim
// is only legal in a Worker (`Atomics.wait` throws on the main thread) — which is
// exactly the model: the execution worker blocks while the kernel worker services.
//
// stdin is delivered to the agent via postMessage (event-loop friendly), NOT this
// SAB — so an async agent can `await` the LLM while still receiving stdin (§3.2's
// stdio split). This endpoint carries only exec→kernel output + syscalls and
// kernel→exec syscall results.

import { SabRing, type SabRingLayout } from "./sab-ring.js";

const FRAME_SYSCALL = 1;
const FRAME_STDOUT = 2;
const FRAME_STDERR = 3;
const FRAME_EXIT = 4;
const FRAME_RESULT = 1;
const FRAME_POISON = 2;
const GEN_INDEX = 0;
const DEFAULT_SYSCALL_TIMEOUT_MS = 30_000;

/** Thrown when the kernel poisoned this execution's channel (it was killed/torn
 * down) while a syscall shim was blocked — the agent should exit. */
export class ExecutionKilledError extends Error {
	constructor() {
		super("execution killed by the kernel");
		this.name = "ExecutionKilledError";
	}
}

export class SabExecutionEndpoint {
	private readonly up: SabRing; // producer: exec → kernel
	private readonly down: SabRing; // consumer: kernel → exec
	private readonly control: Int32Array; // global GEN

	constructor(opts: {
		upSab: SharedArrayBuffer;
		downSab: SharedArrayBuffer;
		controlSab: SharedArrayBuffer;
		layout: SabRingLayout;
	}) {
		this.up = new SabRing(opts.upSab, opts.layout);
		this.down = new SabRing(opts.downSab, opts.layout);
		this.control = new Int32Array(opts.controlSab, 0, 1);
	}

	private signal(): void {
		Atomics.add(this.control, GEN_INDEX, 1);
		Atomics.notify(this.control, GEN_INDEX);
	}

	/** Write a framed message to the up-channel + wake the kernel reactor. Blocks
	 * (bounded back-off) only if the ring is full — the kernel drains continuously. */
	private writeUp(kind: number, payload: Uint8Array): void {
		const frame = new Uint8Array(1 + payload.byteLength);
		frame[0] = kind;
		frame.set(payload, 1);
		while (!this.up.tryWrite(frame)) {
			Atomics.wait(this.control, GEN_INDEX, Atomics.load(this.control, GEN_INDEX), 1);
		}
		this.signal();
	}

	writeStdout(bytes: Uint8Array): void {
		this.writeUp(FRAME_STDOUT, bytes);
	}
	writeStderr(bytes: Uint8Array): void {
		this.writeUp(FRAME_STDERR, bytes);
	}
	exit(code = 0): void {
		this.writeUp(FRAME_EXIT, new Uint8Array([code & 0xff, (code >>> 8) & 0xff, (code >>> 16) & 0xff, (code >>> 24) & 0xff]));
	}

	/** Synchronous kernel syscall (Worker-only): write the request, then block on the
	 * down-channel until the kernel writes the result. This is the guest model's
	 * blocking shim — the agent only blocks here (inside a sync syscall), never while
	 * awaiting the LLM (§3.2). */
	syscall(payload: Uint8Array, timeoutMs = DEFAULT_SYSCALL_TIMEOUT_MS): Uint8Array {
		this.writeUp(FRAME_SYSCALL, payload);
		const deadline = Date.now() + timeoutMs;
		for (;;) {
			const frame = this.down.tryRead();
			if (frame !== null) {
				if (frame[0] === FRAME_POISON) throw new ExecutionKilledError();
				if (frame[0] === FRAME_RESULT) return frame.subarray(1);
				// unknown down frame: ignore and keep waiting
			}
			const remaining = deadline - Date.now();
			if (remaining <= 0) throw new Error("kernel syscall timed out");
			Atomics.wait(this.control, GEN_INDEX, Atomics.load(this.control, GEN_INDEX), remaining);
		}
	}
}
