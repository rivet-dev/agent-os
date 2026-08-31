import { AgentOs } from "@rivet-dev/agentos-core";

// The runtime exposes its underlying VM for advanced shell and WASM workflows.
const runtime = await AgentOs.create({
	permissions: { network: "allow" },
});
try {
	const result = await runtime.process.execFile(
		"sh",
		["-c", "printf 'hello from a WASM-backed AgentOS command\\n'"],
		{ output: { capture: "all" } },
	);
	console.log(result.stdout?.trim());
} finally {
	await runtime.dispose();
}
