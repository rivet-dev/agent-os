// M2a: the Agent OS converged kernel, running INSIDE a dedicated worker.
//
// This is the structural inversion the async-agent executor needs (spec §3.1): the
// wasm sidecar (kernel + AcpCore + extensions) instantiates in a Worker, where
// `Atomics.wait` is legal, instead of on the main thread. The main thread becomes an
// async postMessage relay. M2a proves boot + a wire-frame round-trip in the worker;
// guest-syscall re-routing (M2b) and the agent reactor (M3) layer on top here.

interface BootMessage {
	type: "boot";
	id: number;
	moduleUrl: string;
	binaryUrl: string;
}
interface FrameMessage {
	type: "frame";
	id: number;
	frame: Uint8Array;
}
type InboundMessage = BootMessage | FrameMessage;

interface KernelWasmModule {
	default(input?: unknown): Promise<unknown>;
	AgentOsBrowserSidecarWasm: new (hostBridge?: unknown) => {
		readonly sidecarId: string;
		pushFrame(frame: Uint8Array): unknown;
	};
}

let sidecar: InstanceType<KernelWasmModule["AgentOsBrowserSidecarWasm"]> | null = null;

self.onmessage = async (event: MessageEvent<InboundMessage>) => {
	const message = event.data;
	try {
		if (message.type === "boot") {
			const wasm = (await import(/* @vite-ignore */ message.moduleUrl)) as KernelWasmModule;
			await wasm.default(message.binaryUrl);
			sidecar = new wasm.AgentOsBrowserSidecarWasm(null);
			(self as unknown as Worker).postMessage({
				type: "booted",
				id: message.id,
				sidecarId: sidecar.sidecarId,
			});
			return;
		}
		if (message.type === "frame") {
			if (!sidecar) throw new Error("kernel worker: pushFrame before boot");
			const response = sidecar.pushFrame(message.frame);
			if (!(response instanceof Uint8Array)) {
				throw new Error("kernel worker: pushFrame returned no response frame");
			}
			(self as unknown as Worker).postMessage(
				{ type: "response", id: message.id, frame: response },
				// Transfer the response buffer to avoid a copy back to the relay.
				[response.buffer],
			);
			return;
		}
	} catch (error) {
		(self as unknown as Worker).postMessage({
			type: "error",
			id: (message as { id?: number }).id ?? -1,
			message: error instanceof Error ? (error.stack ?? error.message) : String(error),
		});
	}
};
