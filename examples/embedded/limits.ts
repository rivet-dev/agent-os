import { AgentOs } from "@rivet-dev/agentos-core";

// The same `limits` object the actor takes. `onLimitWarning` is an embedded
// create option rather than a broadcast event, so it fires only in this process.
const vm = await AgentOs.create({
	limits: {
		resources: { maxProcesses: 64, maxFilesystemBytes: 256 * 1024 * 1024 },
		jsRuntime: { v8HeapLimitMb: 128, cpuTimeLimitMs: 30_000 },
	},
	onLimitWarning: (warning) => {
		console.warn("near limit:", warning);
	},
});
await vm.dispose();
