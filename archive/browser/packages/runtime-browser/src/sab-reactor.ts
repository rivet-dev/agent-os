// Kernel-worker reactor for the converged async-agent executor
// (AGENTOS-WEB-ASYNC-AGENTS.md §3.3/§3.4/§7). It multiplexes many per-execution
// duplex SAB channels: it drains every execution's up-ring (exec→kernel: syscalls,
// stdout/stderr, exit), services syscalls inline via a host callback, routes output
// into PER-EXECUTION queues, and blocks on a single global GEN signal between
// wakeups. `poll(executionId, ...)` returns that execution's next output, servicing
// everyone's syscalls in the meantime — the property that keeps a synchronous
// AcpCore from deadlocking while it block-waits for one agent's stdout.
//
// Security invariants enforced here (§7):
//  - Identity is CHANNEL-DERIVED: the reactor knows which execution a frame belongs
//    to from which ring it read it on; up-frames carry NO identity bytes.
//  - Hostile frames (bad length / bad kind) KILL that execution — never throw out of
//    the reactor or hang it (one bad agent must not wedge the shared kernel worker).
//  - The TCB never blocks on an untrusted ring: down-ring writes that would block are
//    a protocol kill, not a wait.

import { SabRing, SabRingProtocolError, type SabRingLayout } from "./sab-ring.js";

// Up-channel frame kinds (exec → kernel). First byte of each frame.
export const FRAME_SYSCALL = 1;
export const FRAME_STDOUT = 2;
export const FRAME_STDERR = 3;
export const FRAME_EXIT = 4;
// Down-channel frame kinds (kernel → exec).
export const FRAME_RESULT = 1;
export const FRAME_POISON = 2;

const GEN_INDEX = 0;
/** Bytes for the global control SAB (a single GEN counter the reactor waits on). */
export const REACTOR_CONTROL_BYTES = 1 * Int32Array.BYTES_PER_ELEMENT;
/** Max up-frames drained per execution per pass (fairness — §4/F3). */
const DRAIN_FAIRNESS_CAP = 256;

export type OutputKind = typeof FRAME_STDOUT | typeof FRAME_STDERR | typeof FRAME_EXIT;
export interface OutputFrame {
	kind: OutputKind;
	payload: Uint8Array;
}

/** Services one guest syscall synchronously (the kernel). Returns the result bytes
 * written back to the execution's down-ring. Return `DEFERRED` for an async
 * syscall whose result is not ready synchronously (e.g. a host-callback to the
 * main thread for on-device inference); the execution stays blocked in its syscall
 * shim until the result arrives later via the completion channel (§6 / §3.2.1). */
export const DEFERRED: unique symbol = Symbol("syscall-deferred");
export type ServiceSyscall = (
	executionId: string,
	payload: Uint8Array,
) => Uint8Array | typeof DEFERRED;

interface ExecutionState {
	id: string;
	epoch: number;
	up: SabRing;
	down: SabRing;
	output: OutputFrame[];
	exited: boolean;
	killed: boolean;
}

function encodeFrame(kind: number, payload: Uint8Array): Uint8Array {
	const out = new Uint8Array(1 + payload.byteLength);
	out[0] = kind;
	out.set(payload, 1);
	return out;
}

/** Frame a deferred-syscall completion ({executionId, result}) for the completion
 * channel. The main thread (the host-callback producer) writes these; the reactor
 * drains them and delivers the result to the execution's blocked syscall. */
export function encodeSyscallCompletion(executionId: string, result: Uint8Array): Uint8Array {
	const id = new TextEncoder().encode(executionId);
	const out = new Uint8Array(1 + id.byteLength + result.byteLength);
	out[0] = id.byteLength;
	out.set(id, 1);
	out.set(result, 1 + id.byteLength);
	return out;
}

export class KernelReactor {
	private readonly control: Int32Array;
	private readonly serviceSyscall: ServiceSyscall;
	private readonly now: () => number;
	private readonly executions = new Map<string, ExecutionState>();
	// Optional completion channel: the MAIN thread (an async host-callback producer)
	// writes deferred-syscall results here; the reactor drains them and unblocks the
	// waiting execution. The main thread becomes another reactor producer (§6).
	private readonly completions: SabRing | null;

	constructor(opts: {
		controlSab: SharedArrayBuffer;
		serviceSyscall: ServiceSyscall;
		now: () => number;
		completionSab?: SharedArrayBuffer;
		completionLayout?: SabRingLayout;
	}) {
		this.control = new Int32Array(opts.controlSab, 0, 1);
		this.serviceSyscall = opts.serviceSyscall;
		this.now = opts.now;
		this.completions =
			opts.completionSab && opts.completionLayout
				? new SabRing(opts.completionSab, opts.completionLayout)
				: null;
	}

	/** Register an execution's channel pair. Identity is bound HERE, by the TCB,
	 * before the executor runs (§7/F1, F4, F6). */
	register(executionId: string, channels: { upSab: SharedArrayBuffer; downSab: SharedArrayBuffer; layout: SabRingLayout }): void {
		if (this.executions.has(executionId)) {
			throw new Error(`execution ${executionId} already registered`);
		}
		this.executions.set(executionId, {
			id: executionId,
			epoch: 0,
			up: new SabRing(channels.upSab, channels.layout),
			down: new SabRing(channels.downSab, channels.layout),
			exited: false,
			killed: false,
			output: [],
		});
	}

	isLive(executionId: string): boolean {
		const e = this.executions.get(executionId);
		return !!e && !e.killed;
	}

	/** Drain every live execution's up-ring once (fairness-bounded), servicing
	 * syscalls inline and queuing output per-execution. Returns true if any frame
	 * was processed. A hostile frame kills only the offending execution. */
	drainOnce(): boolean {
		let processed = this.drainCompletions();
		for (const e of this.executions.values()) {
			if (e.killed) continue;
			for (let i = 0; i < DRAIN_FAIRNESS_CAP; i++) {
				let frame: Uint8Array | null;
				try {
					frame = e.up.tryRead();
				} catch (error) {
					if (error instanceof SabRingProtocolError) {
						this.kill(e.id);
						break;
					}
					throw error;
				}
				if (frame === null) break;
				processed = true;
				if (!this.route(e, frame)) break; // route killed the execution
			}
		}
		return processed;
	}

	private route(e: ExecutionState, frame: Uint8Array): boolean {
		const kind = frame[0];
		const payload = frame.subarray(1);
		switch (kind) {
			case FRAME_SYSCALL: {
				const result = this.serviceSyscall(e.id, payload.slice());
				// DEFERRED: an async syscall (e.g. host-callback inference). The
				// execution stays blocked in its shim; the result arrives later on the
				// completion channel and is delivered by drainCompletions.
				if (result === DEFERRED) return true;
				// The TCB must never block on a full down-ring: with one syscall in
				// flight per execution the down-ring cannot be full unless the executor
				// is misbehaving — treat that as a protocol kill, not a wait (§7/F7).
				if (!e.down.tryWrite(encodeFrame(FRAME_RESULT, result))) {
					this.kill(e.id);
					return false;
				}
				this.signal();
				return true;
			}
			case FRAME_STDOUT:
			case FRAME_STDERR: {
				e.output.push({ kind, payload: payload.slice() });
				return true;
			}
			case FRAME_EXIT: {
				e.exited = true;
				e.output.push({ kind: FRAME_EXIT, payload: payload.slice() });
				return true;
			}
			default:
				// Unknown frame kind from an untrusted producer → kill, don't hang.
				this.kill(e.id);
				return false;
		}
	}

	/** Drain the completion channel: deliver each async deferred-syscall result to
	 * the execution's down-ring, unblocking its waiting syscall shim. Returns true if
	 * any completion was delivered. */
	private drainCompletions(): boolean {
		if (!this.completions) return false;
		let processed = false;
		for (let i = 0; i < DRAIN_FAIRNESS_CAP; i++) {
			let frame: Uint8Array | null;
			try {
				frame = this.completions.tryRead();
			} catch {
				break; // a malformed completion frame is a host bug; stop draining
			}
			if (frame === null) break;
			processed = true;
			const idLen = frame[0];
			const executionId = new TextDecoder().decode(frame.subarray(1, 1 + idLen));
			const result = frame.subarray(1 + idLen);
			const e = this.executions.get(executionId);
			if (e && !e.killed) {
				if (e.down.tryWrite(encodeFrame(FRAME_RESULT, result.slice()))) {
					this.signal();
				} else {
					this.kill(e.id);
				}
			}
		}
		return processed;
	}

	/** Dequeue the next queued output (stdout/stderr/exit) for one execution. */
	takeOutput(executionId: string): OutputFrame | null {
		const e = this.executions.get(executionId);
		if (!e) return null;
		return e.output.shift() ?? null;
	}

	private anyRingPending(): boolean {
		if (this.completions?.hasPending()) return true;
		for (const e of this.executions.values()) {
			if (!e.killed && e.up.hasPending()) return true;
		}
		return false;
	}

	/** The blocking reactor step: return `executionId`'s next output, servicing all
	 * executions' syscalls while waiting, until `deadlineMs` (real clock). Returns
	 * null on timeout or if the execution was killed. Legal to block here: the
	 * reactor runs in a Worker. */
	poll(executionId: string, deadlineMs: number): OutputFrame | null {
		for (;;) {
			this.drainOnce();
			const out = this.takeOutput(executionId);
			if (out !== null) return out;
			if (!this.isLive(executionId)) return null;
			const remaining = deadlineMs - this.now();
			if (remaining <= 0) return null;
			// Snapshot GEN AFTER the drain confirmed nothing is pending, then wait
			// only if still empty (§4/F4 — no lost wakeup, no busy-spin).
			const gen = Atomics.load(this.control, GEN_INDEX);
			if (!this.anyRingPending()) {
				Atomics.wait(this.control, GEN_INDEX, gen, remaining);
			}
		}
	}

	/** Producer-side wake: bump GEN + notify. Execution workers call the equivalent
	 * after each up-ring write; the reactor calls it after writing a down result. */
	signal(): void {
		Atomics.add(this.control, GEN_INDEX, 1);
		Atomics.notify(this.control, GEN_INDEX);
	}

	/** Kill an execution: bump its epoch, poison its down-ring so a parked syscall
	 * shim wakes and sees EOF, and stop reading its rings. Never blocks (§7). */
	kill(executionId: string): void {
		const e = this.executions.get(executionId);
		if (!e || e.killed) return;
		e.killed = true;
		e.epoch += 1;
		try {
			e.down.tryWrite(encodeFrame(FRAME_POISON, new Uint8Array(0)));
		} catch {
			// down-ring unusable; nothing more to do — the worker is being torn down.
		}
		this.signal();
	}

	epochOf(executionId: string): number | null {
		return this.executions.get(executionId)?.epoch ?? null;
	}

	unregister(executionId: string): void {
		this.executions.delete(executionId);
	}
}
