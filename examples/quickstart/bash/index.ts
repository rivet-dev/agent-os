import { AgentOs } from "@rivet-dev/agentos-core";

const runtime = await AgentOs.create();

try {
	// No capture: you get the outcome and exit code only.
	await runtime.process.exec("mkdir -p /workspace/demo");

	// Capture "stderr" for diagnostics, "all" for both streams.
	const listing = await runtime.process.exec("ls -la /workspace", {
		output: { capture: "all" },
	});
	console.log(listing.outcome === "succeeded" ? listing.stdout : listing.error);

	// Pipes, redirects, and globs work — it's a real shell.
	const piped = await runtime.process.exec("echo 'hello world' | tr a-z A-Z", {
		output: { capture: "all" },
	});
	console.log(piped.stdout?.trim()); // "HELLO WORLD"

	// execFile sends each arg verbatim — no shell parses it, so no injection.
	const raw = await runtime.process.execFile("echo", ["$(whoami)"], {
		output: { capture: "all" },
	});
	console.log(raw.stdout?.trim()); // "$(whoami)" — printed literally, never run
} finally {
	await runtime.dispose();
}
