import { describe, expect, it } from "vitest";
import { SabRing, sabRingByteLength, type SabRingLayout } from "../../src/sab-ring.js";
import {
	FRAME_EXIT,
	FRAME_POISON,
	FRAME_RESULT,
	FRAME_STDOUT,
	FRAME_SYSCALL,
	KernelReactor,
	REACTOR_CONTROL_BYTES,
	type ServiceSyscall,
} from "../../src/sab-reactor.js";

const LAYOUT: SabRingLayout = { slotCount: 8, slotBytes: 128 };

function encodeFrame(kind: number, payload: Uint8Array): Uint8Array {
	const out = new Uint8Array(1 + payload.byteLength);
	out[0] = kind;
	out.set(payload, 1);
	return out;
}
const text = (s: string) => new TextEncoder().encode(s);
const decode = (b: Uint8Array) => new TextDecoder().decode(b);

interface Harness {
	reactor: KernelReactor;
	clock: { ms: number };
	add(id: string): { up: SabRing; down: SabRing };
	syscalls: Array<{ id: string; payload: string }>;
}

function harness(serviceSyscall?: ServiceSyscall): Harness {
	const controlSab = new SharedArrayBuffer(REACTOR_CONTROL_BYTES);
	const clock = { ms: 0 };
	const syscalls: Array<{ id: string; payload: string }> = [];
	const reactor = new KernelReactor({
		controlSab,
		now: () => clock.ms,
		serviceSyscall:
			serviceSyscall ??
			((id, payload) => {
				syscalls.push({ id, payload: decode(payload) });
				return text(`ok:${decode(payload)}`);
			}),
	});
	return {
		reactor,
		clock,
		syscalls,
		add(id: string) {
			const upSab = new SharedArrayBuffer(sabRingByteLength(LAYOUT));
			const downSab = new SharedArrayBuffer(sabRingByteLength(LAYOUT));
			reactor.register(id, { upSab, downSab, layout: LAYOUT });
			// Test-side producer view of up (write) and consumer view of down (read).
			return { up: new SabRing(upSab, LAYOUT), down: new SabRing(downSab, LAYOUT) };
		},
	};
}

describe("KernelReactor routing", () => {
	it("services a syscall inline and writes the result to the down-ring", () => {
		const h = harness();
		const { up, down } = h.add("e1");
		up.tryWrite(encodeFrame(FRAME_SYSCALL, text("ping")));
		h.reactor.drainOnce();
		expect(h.syscalls).toEqual([{ id: "e1", payload: "ping" }]);
		const result = down.tryRead();
		expect(result).not.toBeNull();
		expect(result![0]).toBe(FRAME_RESULT);
		expect(decode(result!.subarray(1))).toBe("ok:ping");
	});

	it("queues stdout per-execution and never leaks across executions (channel-derived identity)", () => {
		const h = harness();
		const a = h.add("a");
		const b = h.add("b");
		a.up.tryWrite(encodeFrame(FRAME_STDOUT, text("from-a")));
		b.up.tryWrite(encodeFrame(FRAME_STDOUT, text("from-b")));
		h.reactor.drainOnce();
		const outA = h.reactor.takeOutput("a");
		const outB = h.reactor.takeOutput("b");
		expect(outA && decode(outA.payload)).toBe("from-a");
		expect(outB && decode(outB.payload)).toBe("from-b");
		// a's queue is now empty — it never saw b's frame.
		expect(h.reactor.takeOutput("a")).toBeNull();
	});

	it("routes exit but keeps the execution live (exit != killed)", () => {
		const h = harness();
		const { up } = h.add("e1");
		up.tryWrite(encodeFrame(FRAME_EXIT, new Uint8Array([0])));
		h.reactor.drainOnce();
		const out = h.reactor.takeOutput("e1");
		expect(out?.kind).toBe(FRAME_EXIT);
		expect(h.reactor.isLive("e1")).toBe(true);
	});
});

describe("KernelReactor hostile-input resilience (one bad agent must not wedge the kernel)", () => {
	it("kills the execution on an unknown frame kind, leaving others live", () => {
		const h = harness();
		const a = h.add("a");
		const b = h.add("b");
		a.up.tryWrite(encodeFrame(99, text("garbage")));
		b.up.tryWrite(encodeFrame(FRAME_STDOUT, text("ok")));
		h.reactor.drainOnce();
		expect(h.reactor.isLive("a")).toBe(false);
		expect(h.reactor.isLive("b")).toBe(true);
		expect(decode(h.reactor.takeOutput("b")!.payload)).toBe("ok");
	});

	it("kills the execution on a hostile frame length, never reading OOB or throwing out of the reactor", () => {
		const h = harness();
		const { up } = h.add("bad");
		h.add("good");
		// Simulate a malicious producer: write a huge length prefix into slot 0 and
		// advance the tail directly, bypassing the write-side guard.
		const buf = up as unknown as { bytes: Uint8Array; control: Int32Array };
		buf.bytes[0] = 0xff;
		buf.bytes[1] = 0xff;
		buf.bytes[2] = 0xff;
		buf.bytes[3] = 0x7f;
		Atomics.store(buf.control, 1, 1);
		expect(() => h.reactor.drainOnce()).not.toThrow();
		expect(h.reactor.isLive("bad")).toBe(false);
		expect(h.reactor.isLive("good")).toBe(true);
	});
});

describe("KernelReactor poll + teardown", () => {
	it("returns queued output immediately without waiting", () => {
		const h = harness();
		const { up } = h.add("e1");
		up.tryWrite(encodeFrame(FRAME_STDOUT, text("hi")));
		const out = h.reactor.poll("e1", 10_000);
		expect(out && decode(out.payload)).toBe("hi");
	});

	it("returns null on deadline (timeout) when nothing is queued", () => {
		const h = harness();
		h.add("e1");
		h.clock.ms = 100;
		expect(h.reactor.poll("e1", 50)).toBeNull(); // deadline already passed
	});

	it("kill poisons the down-ring and bumps the epoch", () => {
		const h = harness();
		const { down } = h.add("e1");
		expect(h.reactor.epochOf("e1")).toBe(0);
		h.reactor.kill("e1");
		expect(h.reactor.isLive("e1")).toBe(false);
		expect(h.reactor.epochOf("e1")).toBe(1);
		const poison = down.tryRead();
		expect(poison?.[0]).toBe(FRAME_POISON);
	});

	it("poll returns null once an execution is killed", () => {
		const h = harness();
		h.add("e1");
		h.reactor.kill("e1");
		expect(h.reactor.poll("e1", 10_000)).toBeNull();
	});
});
