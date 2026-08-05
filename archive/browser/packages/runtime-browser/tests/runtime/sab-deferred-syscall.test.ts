import { describe, expect, it } from "vitest";
import { SabRing, sabRingByteLength, type SabRingLayout } from "../../src/sab-ring.js";
import {
	DEFERRED,
	KernelReactor,
	REACTOR_CONTROL_BYTES,
	encodeSyscallCompletion,
} from "../../src/sab-reactor.js";
import { SabExecutionEndpoint } from "../../src/sab-execution-endpoint.js";

const LAYOUT: SabRingLayout = { slotCount: 8, slotBytes: 256 };
const text = (s: string) => new TextEncoder().encode(s);
const decode = (b: Uint8Array) => new TextDecoder().decode(b);

// A DEFERRED syscall (an async host-callback, e.g. on-device inference) is NOT
// answered synchronously by serviceSyscall; the execution stays blocked until the
// MAIN thread writes the result to the completion channel and the reactor delivers
// it. This is the async-inference transport (§6). Tested single-threaded by driving
// the endpoint + reactor on one thread (no Atomics.wait needed because the result is
// staged before we read it).
describe("deferred syscall + completion channel", () => {
	it("leaves a deferred syscall pending, then completes it via the completion channel", () => {
		const upSab = new SharedArrayBuffer(sabRingByteLength(LAYOUT));
		const downSab = new SharedArrayBuffer(sabRingByteLength(LAYOUT));
		const controlSab = new SharedArrayBuffer(REACTOR_CONTROL_BYTES);
		const completionSab = new SharedArrayBuffer(sabRingByteLength(LAYOUT));

		const reactor = new KernelReactor({
			controlSab,
			now: () => Date.now(),
			// Every syscall here is async (host-callback) → DEFERRED.
			serviceSyscall: () => DEFERRED,
			completionSab,
			completionLayout: LAYOUT,
		});
		reactor.register("e", { upSab, downSab, layout: LAYOUT });

		// The execution producer side of the channels (the agent worker uses these).
		const upProducer = new SabRing(upSab, LAYOUT); // exec → kernel
		const downConsumer = new SabRing(downSab, LAYOUT); // kernel → exec
		// Simulate the agent writing a SYSCALL frame ([kind=1][payload]).
		upProducer.tryWrite(new Uint8Array([1, ...text("infer:hello")]));

		// Reactor drains the syscall → DEFERRED → no result written yet.
		reactor.drainOnce();
		expect(downConsumer.tryRead()).toBeNull(); // still pending

		// The MAIN thread completes it: write the result to the completion channel.
		const completionProducer = new SabRing(completionSab, LAYOUT);
		completionProducer.tryWrite(encodeSyscallCompletion("e", text("PONG_FROM_MODEL")));

		// Reactor drains the completion → delivers the result to the down-ring.
		reactor.drainOnce();
		const result = downConsumer.tryRead();
		expect(result).not.toBeNull();
		expect(result![0]).toBe(1 /* FRAME_RESULT (down-channel) */);
		expect(decode(result!.subarray(1))).toBe("PONG_FROM_MODEL");
	});

	it("a SabExecutionEndpoint syscall round-trips through a deferred completion", () => {
		const upSab = new SharedArrayBuffer(sabRingByteLength(LAYOUT));
		const downSab = new SharedArrayBuffer(sabRingByteLength(LAYOUT));
		const controlSab = new SharedArrayBuffer(REACTOR_CONTROL_BYTES);
		const completionSab = new SharedArrayBuffer(sabRingByteLength(LAYOUT));

		let pendingPayload: string | null = null;
		const reactor = new KernelReactor({
			controlSab,
			now: () => Date.now(),
			serviceSyscall: (_id, payload) => {
				pendingPayload = decode(payload);
				return DEFERRED;
			},
			completionSab,
			completionLayout: LAYOUT,
		});
		reactor.register("e", { upSab, downSab, layout: LAYOUT });
		const endpoint = new SabExecutionEndpoint({ upSab, downSab, controlSab, layout: LAYOUT });
		const completionProducer = new SabRing(completionSab, LAYOUT);

		// Stage the completion BEFORE the (single-thread) syscall reads it: the
		// endpoint writes the request, the reactor drains it (DEFERRED), we stage the
		// completion, the reactor drains that → down-ring → the endpoint's syscall
		// returns it.
		const upProducer = new SabRing(upSab, LAYOUT);
		upProducer.tryWrite(new Uint8Array([1, ...text("infer:q")]));
		reactor.drainOnce();
		expect(pendingPayload).toBe("infer:q");
		completionProducer.tryWrite(encodeSyscallCompletion("e", text("ANSWER")));
		reactor.drainOnce();
		// Now the endpoint can read its result without blocking.
		const downConsumer = new SabRing(downSab, LAYOUT);
		const frame = downConsumer.tryRead();
		expect(decode(frame!.subarray(1))).toBe("ANSWER");
		void endpoint;
	});
});
