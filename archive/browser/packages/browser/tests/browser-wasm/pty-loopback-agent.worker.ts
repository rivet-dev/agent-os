// A PTY-loopback probe agent (AGENTOS-WEB-PTY-TERMINAL.md T2). On session/prompt it
// drives a full pseudo-terminal loopback through the kernel PtyManager entirely via
// mid-turn `pty.*` syscalls: open a master/slave pair, switch the line discipline to
// raw mode, write to the MASTER, read the bytes back from the SLAVE (input line
// discipline), write a reply to the SLAVE, and read it back on the MASTER (output line
// discipline). This is the PTY analogue of the async-loopback TCP gate, and proves the
// converged `pty.*` sync-bridge ops route through the wasm sidecar's guest_pty
// dispatcher into the kernel's real PTY (the same pushFrame path net.*/fs.* use).

import { SabExecutionEndpoint, type SabRingLayout } from "@rivet-dev/agentos-runtime-browser";
import { encodeSyscall } from "./syscall-codec.js";

interface InitMessage {
	type: "init";
	upSab: SharedArrayBuffer;
	downSab: SharedArrayBuffer;
	controlSab: SharedArrayBuffer;
	layout: SabRingLayout;
}
interface StdinMessage {
	type: "stdin";
	chunk: Uint8Array;
}

let endpoint: SabExecutionEndpoint | null = null;
let buffer = "";
const decoder = new TextDecoder();
const encoder = new TextEncoder();

/** Blocking kernel syscall (the SAB shim). Returns the ConvergedSyncResponse value
 * ({masterFd,slaveFd} / {data:base64} / {written} / {}), or throws on error. */
function syscall(operation: string, arg: unknown): Record<string, unknown> {
	const raw = endpoint!.syscall(encodeSyscall(operation, [arg]));
	const response = JSON.parse(decoder.decode(raw)) as {
		kind?: number;
		value?: Record<string, unknown>;
		error?: string;
	};
	if (response.error) throw new Error(`${operation}: ${response.error}`);
	return response.value ?? {};
}

function decodeBase64(base64: string): Uint8Array {
	const binary = atob(base64);
	const out = new Uint8Array(binary.length);
	for (let i = 0; i < binary.length; i += 1) out[i] = binary.charCodeAt(i);
	return out;
}

/** Read from a pty fd, retrying the short-poll until bytes arrive (the kernel clamps
 * each blocking read to a small ceiling so it never parks the sidecar). */
function readPty(fd: number): string {
	for (let attempt = 0; attempt < 200; attempt += 1) {
		const result = syscall("pty.read", { fd });
		if (typeof result.data === "string") {
			return decoder.decode(decodeBase64(result.data));
		}
	}
	throw new Error(`pty.read(${fd}) timed out`);
}

/** Full pseudo-terminal loopback through the kernel line discipline. */
function runPtyLoopback(message: string): string {
	const pair = syscall("pty.open", {});
	const masterFd = pair.masterFd as number;
	const slaveFd = pair.slaveFd as number;
	// Raw mode: bytes pass through 1:1, no canonical line buffering, no echo back to
	// the master, no NL translation — a clean loopback of exactly what we write.
	syscall("pty.tcsetattr", {
		fd: slaveFd,
		icanon: false,
		echo: false,
		opost: false,
		isig: false,
	});
	// Host → master → (input line discipline) → slave input buffer.
	syscall("pty.write", { fd: masterFd, data: encoder.encode(message) });
	const fromSlave = readPty(slaveFd);
	// Guest → slave → (output line discipline) → master output buffer.
	syscall("pty.write", { fd: slaveFd, data: encoder.encode(`ECHO:${fromSlave}`) });
	const fromMaster = readPty(masterFd);
	syscall("pty.close", { fd: slaveFd });
	syscall("pty.close", { fd: masterFd });
	return fromMaster;
}

async function handleLine(line: string): Promise<void> {
	const request = JSON.parse(line) as {
		id: number;
		method: string;
		params?: { protocolVersion?: number };
	};
	await Promise.resolve();
	const { id, method, params } = request;
	let body: Record<string, unknown>;
	switch (method) {
		case "initialize":
			body = {
				result: {
					protocolVersion: params?.protocolVersion ?? 1,
					agentInfo: { name: "pty-loopback", version: "0.0.0" },
					agentCapabilities: {},
				},
			};
			break;
		case "session/new":
			body = { result: { sessionId: "pty-loopback-session" } };
			break;
		case "session/prompt": {
			let content: string;
			try {
				content = runPtyLoopback("ping-pty");
			} catch (error) {
				content = `ERR:${error instanceof Error ? error.message : String(error)}`;
			}
			body = { result: { stopReason: "end_turn", content } };
			break;
		}
		default:
			body = { error: { code: -32601, message: `method not found: ${method}` } };
	}
	endpoint!.writeStdout(
		encoder.encode(`${JSON.stringify({ jsonrpc: "2.0", id, ...body })}\n`),
	);
}

self.onmessage = (event: MessageEvent<InitMessage | StdinMessage>) => {
	const message = event.data;
	if (message.type === "init") {
		endpoint = new SabExecutionEndpoint({
			upSab: message.upSab,
			downSab: message.downSab,
			controlSab: message.controlSab,
			layout: message.layout,
		});
		return;
	}
	if (message.type === "stdin" && endpoint) {
		buffer += decoder.decode(message.chunk);
		let newline = buffer.indexOf("\n");
		while (newline >= 0) {
			const line = buffer.slice(0, newline).trim();
			buffer = buffer.slice(newline + 1);
			if (line) void handleLine(line);
			newline = buffer.indexOf("\n");
		}
	}
};
