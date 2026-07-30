import path from "node:path";
import { fileURLToPath } from "node:url";
import { Worker } from "node:worker_threads";
import { describe, expect, it } from "vitest";
import { sabRingByteLength, type SabRingLayout } from "../../src/sab-ring.js";
import { SabExecutionEndpoint } from "../../src/sab-execution-endpoint.js";
import {
	FRAME_STDOUT,
	KernelReactor,
	REACTOR_CONTROL_BYTES,
} from "../../src/sab-reactor.js";

const LAYOUT: SabRingLayout = { slotCount: 8, slotBytes: 64 };
const text = (s: string) => new TextEncoder().encode(s);
const decode = (b: Uint8Array) => new TextDecoder().decode(b);

function channels() {
	const upSab = new SharedArrayBuffer(sabRingByteLength(LAYOUT));
	const downSab = new SharedArrayBuffer(sabRingByteLength(LAYOUT));
	const controlSab = new SharedArrayBuffer(REACTOR_CONTROL_BYTES);
	return { upSab, downSab, controlSab };
}

describe("SabExecutionEndpoint non-blocking writes (single-thread, against the real reactor)", () => {
	it("writes stdout the reactor routes to this execution", () => {
		const { upSab, downSab, controlSab } = channels();
		const endpoint = new SabExecutionEndpoint({ upSab, downSab, controlSab, layout: LAYOUT });
		const reactor = new KernelReactor({
			controlSab,
			now: () => Date.now(),
			serviceSyscall: () => new Uint8Array(0),
		});
		reactor.register("e", { upSab, downSab, layout: LAYOUT });

		endpoint.writeStdout(text("hello-from-agent"));
		endpoint.exit(0);
		reactor.drainOnce();

		const out = reactor.takeOutput("e");
		expect(out?.kind).toBe(FRAME_STDOUT);
		expect(decode(out!.payload)).toBe("hello-from-agent");
		const exit = reactor.takeOutput("e");
		expect(exit?.kind).toBe(4 /* FRAME_EXIT */);
	});
});

describe("SabExecutionEndpoint blocking syscall (cross-thread, real Atomics)", () => {
	it(
		"makes a blocking syscall serviced by the reactor on another thread, then writes the result as stdout",
		async () => {
			const { upSab, downSab, controlSab } = channels();
			const reactor = new KernelReactor({
				controlSab,
				now: () => Date.now(),
				// Echo the syscall payload back as the result.
				serviceSyscall: (_id, payload) => text(`echo:${decode(payload)}`),
			});
			reactor.register("e", { upSab, downSab, layout: LAYOUT });

			const here = path.dirname(fileURLToPath(import.meta.url));
			const worker = new Worker(
				path.join(here, "fixtures", "sab-syscall-roundtrip.worker.mjs"),
				{
					workerData: {
						upSab,
						downSab,
						genSab: controlSab,
						slotCount: LAYOUT.slotCount,
						slotBytes: LAYOUT.slotBytes,
					},
				},
			);
			const done = new Promise<void>((resolve, reject) => {
				worker.on("error", reject);
				worker.on("exit", (c) => (c === 0 ? resolve() : reject(new Error(`exit ${c}`))));
			});

			// poll services the worker's blocking syscall (writing the RESULT to the
			// down-channel) and then returns the stdout the worker writes afterward.
			const out = reactor.poll("e", Date.now() + 10_000);
			await worker.terminate();
			await done.catch(() => {});

			expect(out).not.toBeNull();
			expect(out!.kind).toBe(FRAME_STDOUT);
			expect(decode(out!.payload)).toBe("echo:ping");
		},
		20_000,
	);
});
