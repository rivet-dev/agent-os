import { describe, expect, it } from "vitest";
import {
	SabRing,
	SabRingProtocolError,
	sabRingByteLength,
	sabRingMaxFrameBytes,
} from "../../src/sab-ring.js";

const LAYOUT = { slotCount: 4, slotBytes: 64 };

function newRing(layout = LAYOUT): { ring: SabRing; sab: SharedArrayBuffer } {
	const sab = new SharedArrayBuffer(sabRingByteLength(layout));
	return { ring: new SabRing(sab, layout), sab };
}

function frame(text: string): Uint8Array {
	return new TextEncoder().encode(text);
}

function read(ring: SabRing): string | null {
	const out = ring.tryRead();
	return out === null ? null : new TextDecoder().decode(out);
}

describe("SabRing layout", () => {
	it("sizes the backing buffer for header + slots", () => {
		expect(sabRingByteLength({ slotCount: 4, slotBytes: 64 })).toBe(16 + 4 * 64);
	});
	it("reports max frame as slot minus the length prefix", () => {
		expect(sabRingMaxFrameBytes(64)).toBe(60);
	});
	it("rejects non-power-of-two slot counts and tiny slots", () => {
		const sab = new SharedArrayBuffer(4096);
		expect(() => new SabRing(sab, { slotCount: 3, slotBytes: 64 })).toThrow();
		expect(() => new SabRing(sab, { slotCount: 4, slotBytes: 4 })).toThrow();
	});
});

describe("SabRing SPSC round-trip", () => {
	it("returns null when empty", () => {
		const { ring } = newRing();
		expect(ring.tryRead()).toBeNull();
		expect(ring.hasPending()).toBe(false);
	});

	it("round-trips a single frame", () => {
		const { ring } = newRing();
		expect(ring.tryWrite(frame("hello"))).toBe(true);
		expect(ring.hasPending()).toBe(true);
		expect(read(ring)).toBe("hello");
		expect(ring.tryRead()).toBeNull();
	});

	it("preserves FIFO order", () => {
		const { ring } = newRing();
		ring.tryWrite(frame("a"));
		ring.tryWrite(frame("b"));
		ring.tryWrite(frame("c"));
		expect(read(ring)).toBe("a");
		expect(read(ring)).toBe("b");
		expect(read(ring)).toBe("c");
	});

	it("applies backpressure when full (tryWrite=false, never throws)", () => {
		const { ring } = newRing(); // slotCount 4
		expect(ring.tryWrite(frame("1"))).toBe(true);
		expect(ring.tryWrite(frame("2"))).toBe(true);
		expect(ring.tryWrite(frame("3"))).toBe(true);
		expect(ring.tryWrite(frame("4"))).toBe(true);
		expect(ring.tryWrite(frame("5"))).toBe(false); // full
		expect(read(ring)).toBe("1"); // drain one frees a slot
		expect(ring.tryWrite(frame("5"))).toBe(true);
	});

	it("wraps around correctly over many frames", () => {
		const { ring } = newRing();
		for (let i = 0; i < 100; i++) {
			expect(ring.tryWrite(frame(`f${i}`))).toBe(true);
			expect(read(ring)).toBe(`f${i}`);
		}
	});

	it("carries an empty frame", () => {
		const { ring } = newRing();
		expect(ring.tryWrite(new Uint8Array(0))).toBe(true);
		const out = ring.tryRead();
		expect(out).not.toBeNull();
		expect(out!.byteLength).toBe(0);
	});

	it("throws locally when a frame exceeds the slot", () => {
		const { ring } = newRing();
		expect(() => ring.tryWrite(new Uint8Array(61))).toThrow(); // maxFrame is 60
	});
});

describe("SabRing hostile-input validation (read side = TCB reading untrusted bytes)", () => {
	it("throws SabRingProtocolError on an out-of-bounds length prefix", () => {
		const { ring, sab } = newRing();
		// Simulate a malicious producer: write a huge length prefix into slot 0 and
		// advance the tail, WITHOUT going through tryWrite's write-side guard.
		const control = new Int32Array(sab, 0, 4);
		const bytes = new Uint8Array(sab, 16);
		bytes[0] = 0xff;
		bytes[1] = 0xff;
		bytes[2] = 0xff;
		bytes[3] = 0x7f; // length = 0x7fffffff
		Atomics.store(control, 1, 1); // tail = 1 (one frame "available")
		expect(() => ring.tryRead()).toThrow(SabRingProtocolError);
	});

	it("does not read past the slot for a length just over the max", () => {
		const { ring, sab } = newRing();
		const control = new Int32Array(sab, 0, 4);
		const bytes = new Uint8Array(sab, 16);
		const bad = 61; // maxFrame is 60
		bytes[0] = bad & 0xff;
		Atomics.store(control, 1, 1);
		expect(() => ring.tryRead()).toThrow(SabRingProtocolError);
	});
});
