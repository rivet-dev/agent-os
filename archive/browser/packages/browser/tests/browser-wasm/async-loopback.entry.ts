// M4 groundwork gate: loopback TCP through the async-agent executor
// (AGENTOS-WEB-ASYNC-AGENTS.md §6). An ASYNC agent answers a session/prompt by driving
// a full loopback handshake (listen/connect/accept/write/read on 127.0.0.1) through
// mid-turn `net.*` syscalls — the kernel socket table, the single network-policy
// enforcement point. Proves net.* routes through the same pushFrame path the echo
// agent proved for fs.*, which the in-sandbox HTTP proxy guest is then built on. No
// inference session needed (the agent makes no host.inference call).

import { Buffer as BufferPolyfill } from "buffer";
(globalThis as unknown as { Buffer?: unknown }).Buffer ??= BufferPolyfill;

import { KernelWorkerRelay, runSessionPromptGate } from "./async-harness.js";

(globalThis as unknown as { __asyncLoopback: unknown }).__asyncLoopback = {
	async run() {
		const relay = new KernelWorkerRelay("/async-kernel.worker.js");
		return runSessionPromptGate(relay, {
			agentType: "async-loopback",
			adapterEntrypoint: "/bin/async-loopback-agent",
			promptText: "go",
		});
	},
};

const status = document.getElementById("status");
if (status) status.textContent = "ready";
