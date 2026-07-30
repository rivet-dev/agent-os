// M4b gate harness: drive a COMPLETE pi turn (initialize → session/new → session/prompt
// → a model answer) in the browser converged executor and expose the result for the
// Playwright spec. See pi-runner.ts for the shared driver.

import { Buffer as BufferPolyfill } from "buffer";
(globalThis as unknown as { Buffer?: unknown }).Buffer ??= BufferPolyfill;

import { runPiTurn } from "./pi-runner.js";

(globalThis as unknown as { __piPrompt: unknown }).__piPrompt = {
	run: () => runPiTurn({ prompt: "Say PONG" }),
};

const status = document.getElementById("status");
if (status) status.textContent = "ready";
