// Cross-thread syscall round-trip producer (node:worker_threads). Mirrors
// SabExecutionEndpoint's wire protocol exactly (kept in sync by the consumer using
// the REAL SabRing/KernelReactor): make a blocking SYSCALL (write SYSCALL frame +
// GEN signal, then Atomics.wait the down-channel for the RESULT), then write the
// result back as STDOUT. Proves the duplex syscall + stdout path across real OS
// threads with real Atomics — the agent execution model's core.

import { parentPort, workerData } from "node:worker_threads";

const FRAME_SYSCALL = 1;
const FRAME_STDOUT = 2;
const FRAME_RESULT = 1;
const HEADER_BYTES = 16;
const { upSab, downSab, genSab, slotCount, slotBytes } = workerData;

const upCtl = new Int32Array(upSab, 0, 4);
const upBytes = new Uint8Array(upSab, HEADER_BYTES);
const downCtl = new Int32Array(downSab, 0, 4);
const downBytes = new Uint8Array(downSab, HEADER_BYTES);
const gen = new Int32Array(genSab, 0, 1);

function signal() {
	Atomics.add(gen, 0, 1);
	Atomics.notify(gen, 0);
}

function writeUp(frame) {
	const head = Atomics.load(upCtl, 0);
	const tail = Atomics.load(upCtl, 1);
	if (tail - head >= slotCount) throw new Error("up full");
	const slot = (tail % slotCount) * slotBytes;
	upBytes[slot] = frame.byteLength & 0xff;
	upBytes[slot + 1] = (frame.byteLength >>> 8) & 0xff;
	upBytes[slot + 2] = (frame.byteLength >>> 16) & 0xff;
	upBytes[slot + 3] = (frame.byteLength >>> 24) & 0xff;
	upBytes.set(frame, slot + 4);
	Atomics.store(upCtl, 1, tail + 1);
	signal();
}

function readDown() {
	const tail = Atomics.load(downCtl, 1);
	const head = Atomics.load(downCtl, 0);
	if (head === tail) return null;
	const slot = (head % slotCount) * slotBytes;
	const len =
		(downBytes[slot] |
			(downBytes[slot + 1] << 8) |
			(downBytes[slot + 2] << 16) |
			(downBytes[slot + 3] << 24)) >>>
		0;
	const out = new Uint8Array(len);
	out.set(downBytes.subarray(slot + 4, slot + 4 + len));
	Atomics.store(downCtl, 0, head + 1);
	return out;
}

function syscall(payload) {
	const frame = new Uint8Array(1 + payload.byteLength);
	frame[0] = FRAME_SYSCALL;
	frame.set(payload, 1);
	writeUp(frame);
	for (;;) {
		const down = readDown();
		if (down && down[0] === FRAME_RESULT) return down.subarray(1);
		Atomics.wait(gen, 0, Atomics.load(gen, 0), 1000);
	}
}

const enc = new TextEncoder();
const result = syscall(enc.encode("ping")); // → kernel services → "echo:ping"
const stdout = new Uint8Array(1 + result.byteLength);
stdout[0] = FRAME_STDOUT;
stdout.set(result, 1);
writeUp(stdout);
parentPort.postMessage({ done: true });
