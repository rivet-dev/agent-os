// T2 gate: kernel PTY through the converged browser path (AGENTOS-WEB-PTY-TERMINAL.md).
// An async agent answers a session/prompt by driving a full pseudo-terminal loopback
// (open / raw-mode tcsetattr / write master / read slave / write slave / read master /
// close) through mid-turn `pty.*` syscalls — the kernel's PtyManager and line
// discipline, exercised over the same pushFrame path net.*/fs.* use. Proves the new
// guest_pty dispatcher + converged-pty-bridge wiring works end to end in real Chromium.

import { Buffer as BufferPolyfill } from "buffer";
(globalThis as unknown as { Buffer?: unknown }).Buffer ??= BufferPolyfill;

import { KernelWorkerRelay, runSessionPromptGate } from "./async-harness.js";

(globalThis as unknown as { __ptyLoopback: unknown }).__ptyLoopback = {
	async run() {
		const relay = new KernelWorkerRelay("/async-kernel.worker.js");
		return runSessionPromptGate(relay, {
			agentType: "pty-loopback",
			adapterEntrypoint: "/bin/pty-loopback-agent",
			promptText: "go",
		});
	},
};

const status = document.getElementById("status");
if (status) status.textContent = "ready";
