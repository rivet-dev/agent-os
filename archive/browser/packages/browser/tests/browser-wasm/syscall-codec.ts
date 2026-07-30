// Encoding for the agent↔kernel syscall channel carried over the SAB up-ring
// (AGENTOS-WEB-ASYNC-AGENTS.md §3.2). The payload is `{operation, args}` JSON, but
// some net.* args are binary (net.write `data` must reach the kernel as a real
// Uint8Array — the converged net bridge rejects anything else). Plain JSON.stringify
// turns a Uint8Array into `{"0":..}`, so we tag binary values as `{$u8: <base64>}`
// on the way out and revive them on the way in. Backward-compatible: an args list
// with no binary values encodes/decodes identically to plain JSON, so the existing
// string-only agents are unaffected.

const U8_TAG = "$u8";

function toBase64(bytes: Uint8Array): string {
	let binary = "";
	for (let i = 0; i < bytes.length; i += 1) binary += String.fromCharCode(bytes[i]);
	return btoa(binary);
}

function fromBase64(base64: string): Uint8Array {
	const binary = atob(base64);
	const out = new Uint8Array(binary.length);
	for (let i = 0; i < binary.length; i += 1) out[i] = binary.charCodeAt(i);
	return out;
}

/** Encode a syscall request, tagging any Uint8Array (or typed-array view) args so they
 * survive the JSON channel and arrive at the kernel as real Uint8Arrays. */
export function encodeSyscall(operation: string, args: unknown[]): Uint8Array {
	const json = JSON.stringify({ operation, args }, (_key, value) => {
		if (value instanceof Uint8Array) return { [U8_TAG]: toBase64(value) };
		if (ArrayBuffer.isView(value)) {
			const view = value as ArrayBufferView;
			return { [U8_TAG]: toBase64(new Uint8Array(view.buffer, view.byteOffset, view.byteLength)) };
		}
		return value;
	});
	return new TextEncoder().encode(json);
}

/** Decode a syscall request, reviving `{$u8}` tags back into Uint8Arrays. */
export function decodeSyscall(bytes: Uint8Array): { operation: string; args: unknown[] } {
	const parsed = JSON.parse(new TextDecoder().decode(bytes), (_key, value) => {
		if (value && typeof value === "object" && typeof (value as Record<string, unknown>)[U8_TAG] === "string") {
			return fromBase64((value as Record<string, string>)[U8_TAG]);
		}
		return value;
	}) as { operation?: string; args?: unknown[] };
	return { operation: String(parsed.operation ?? ""), args: parsed.args ?? [] };
}
