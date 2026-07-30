// Single-producer / single-consumer (SPSC) framed ring buffer over a
// SharedArrayBuffer, for the converged kernel-in-worker reactor (see
// AGENTOS-WEB-ASYNC-AGENTS.md §4). One ring carries length-prefixed frames in one
// direction between exactly one writer and one reader; an execution's duplex
// channel is a pair (up: exec→kernel, down: kernel→exec).
//
// Design choices that make this safe to read from the TCB against an UNTRUSTED
// producer:
//  - SPSC: the producer owns `tail`, the consumer owns `head` — no CAS, no
//    multi-writer races (the single-producer invariant is the caller's, §4/F6).
//  - Slot ring (fixed-size slots), not a byte ring: no wrap-straddle bookkeeping,
//    and every frame length is trivially bounded by the slot size.
//  - Publish order (§4/F5): write bytes → `Atomics.store(tail)` (release). Read
//    order: `Atomics.load(tail)` (acquire) → read bytes. `tail` publishes the frame.
//  - Copy-then-validate (§4/F3): the consumer copies the frame into a fresh
//    (kernel-private) array in one bounded read and validates the length against the
//    slot capacity; it never re-reads shared bytes after the bounds check (no TOCTOU).
//
// The cross-ring wakeup signal (`GEN`) lives in a separate control SAB owned by the
// reactor; this module only moves frames. Producers bump `GEN` + `Atomics.notify`
// AFTER a successful `tryWrite` (the reactor blocks on `GEN`, §3.3).

const HEAD_INDEX = 0;
const TAIL_INDEX = 1;
// 4 Int32 header slots (head, tail, 2 reserved) keep the data region 16-byte aligned.
const HEADER_I32 = 4;
const HEADER_BYTES = HEADER_I32 * Int32Array.BYTES_PER_ELEMENT;
const LEN_PREFIX_BYTES = Int32Array.BYTES_PER_ELEMENT;

export interface SabRingLayout {
	slotCount: number;
	slotBytes: number;
}

/** Total SharedArrayBuffer byte size required to back a ring of this layout. */
export function sabRingByteLength(layout: SabRingLayout): number {
	return HEADER_BYTES + layout.slotCount * layout.slotBytes;
}

/** Max payload bytes a single frame may carry for a given slot size. */
export function sabRingMaxFrameBytes(slotBytes: number): number {
	return slotBytes - LEN_PREFIX_BYTES;
}

/**
 * SPSC framed ring over a SharedArrayBuffer. Construct one instance per endpoint
 * (the producer in its worker, the consumer in the kernel worker) over the SAME
 * SharedArrayBuffer. `slotCount`/`slotBytes` MUST match on both ends.
 */
export class SabRing {
	private readonly control: Int32Array;
	private readonly bytes: Uint8Array;
	private readonly slotCount: number;
	private readonly slotBytes: number;
	private readonly maxFrameBytes: number;

	constructor(sab: SharedArrayBuffer, layout: SabRingLayout) {
		if (layout.slotCount <= 0 || (layout.slotCount & (layout.slotCount - 1)) !== 0) {
			throw new Error("SabRing slotCount must be a positive power of two");
		}
		if (layout.slotBytes <= LEN_PREFIX_BYTES) {
			throw new Error("SabRing slotBytes must exceed the length prefix");
		}
		if (sab.byteLength < sabRingByteLength(layout)) {
			throw new Error("SabRing SharedArrayBuffer too small for layout");
		}
		this.control = new Int32Array(sab, 0, HEADER_I32);
		this.bytes = new Uint8Array(sab, HEADER_BYTES, layout.slotCount * layout.slotBytes);
		this.slotCount = layout.slotCount;
		this.slotBytes = layout.slotBytes;
		this.maxFrameBytes = sabRingMaxFrameBytes(layout.slotBytes);
	}

	get capacityFrames(): number {
		return this.slotCount;
	}

	get maxFrame(): number {
		return this.maxFrameBytes;
	}

	/** Producer side: enqueue one frame. Returns false if the ring is full
	 * (backpressure) — the UNTRUSTED producer may then block/retry; the TCB
	 * consumer must never block on a full ring (§4/F7). Throws only on a local
	 * programming error (frame too large for the slot). */
	tryWrite(frame: Uint8Array): boolean {
		if (frame.byteLength > this.maxFrameBytes) {
			throw new Error(
				`SabRing frame ${frame.byteLength} exceeds slot capacity ${this.maxFrameBytes}`,
			);
		}
		const head = Atomics.load(this.control, HEAD_INDEX);
		const tail = Atomics.load(this.control, TAIL_INDEX);
		if (tail - head >= this.slotCount) return false; // full
		const slot = (tail % this.slotCount) * this.slotBytes;
		// length prefix (little-endian) then payload, then publish via tail (release).
		this.bytes[slot] = frame.byteLength & 0xff;
		this.bytes[slot + 1] = (frame.byteLength >>> 8) & 0xff;
		this.bytes[slot + 2] = (frame.byteLength >>> 16) & 0xff;
		this.bytes[slot + 3] = (frame.byteLength >>> 24) & 0xff;
		this.bytes.set(frame, slot + LEN_PREFIX_BYTES);
		Atomics.store(this.control, TAIL_INDEX, tail + 1);
		return true;
	}

	/** Consumer side: dequeue one frame as a fresh kernel-private copy, or null if
	 * empty. Validates the length as HOSTILE input (§4/F3): a length outside
	 * [0, maxFrame] throws (the caller must kill that execution, §7), never reads OOB.
	 * Copy-then-validate: we snapshot the length, bound-check it, then copy exactly
	 * that many bytes — no re-read of shared memory after the check. */
	tryRead(): Uint8Array | null {
		const tail = Atomics.load(this.control, TAIL_INDEX); // acquire
		const head = Atomics.load(this.control, HEAD_INDEX);
		if (head === tail) return null; // empty
		const slot = (head % this.slotCount) * this.slotBytes;
		const len =
			(this.bytes[slot] |
				(this.bytes[slot + 1] << 8) |
				(this.bytes[slot + 2] << 16) |
				(this.bytes[slot + 3] << 24)) >>>
			0;
		if (len > this.maxFrameBytes) {
			throw new SabRingProtocolError(
				`frame length ${len} exceeds slot capacity ${this.maxFrameBytes}`,
			);
		}
		const out = new Uint8Array(len);
		out.set(this.bytes.subarray(slot + LEN_PREFIX_BYTES, slot + LEN_PREFIX_BYTES + len));
		Atomics.store(this.control, HEAD_INDEX, head + 1); // free the slot
		return out;
	}

	/** True if at least one frame is queued (consumer view). */
	hasPending(): boolean {
		return Atomics.load(this.control, HEAD_INDEX) !== Atomics.load(this.control, TAIL_INDEX);
	}
}

/** Thrown when a frame read from shared memory violates the protocol (hostile
 * producer). The reactor turns this into "kill that execution", never a hang (§7). */
export class SabRingProtocolError extends Error {
	constructor(message: string) {
		super(`SAB ring protocol violation: ${message}`);
		this.name = "SabRingProtocolError";
	}
}
