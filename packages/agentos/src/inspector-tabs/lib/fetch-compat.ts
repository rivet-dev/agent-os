// Chrome treats a request with a `ReadableStream` body as a streaming upload,
// which it only permits over HTTP/2 — a cleartext `http://` engine is
// HTTP/1.1, so the request dies as ERR_ALPN_NEGOTIATION_FAILED before it is
// sent and the server never logs anything. rivetkit's browser client routes
// every action through a `Request` whose `.body` is a stream, so all inspector
// actions fail against a plain-http engine. Buffer such bodies back into a
// normal upload; over HTTPS this is a no-op in behavior.
//
// Action payloads are small (arguments only — file contents move as base64 in
// the JSON body), so buffering is bounded by the caller's own argument size.

const STREAM_BODY_LIMIT_BYTES = 64 * 1024 * 1024;

async function bufferStream(
	stream: ReadableStream<Uint8Array>,
): Promise<ArrayBuffer> {
	const reader = stream.getReader();
	const chunks: Uint8Array[] = [];
	let total = 0;
	try {
		for (;;) {
			const { done, value } = await reader.read();
			if (done) break;
			if (!value) continue;
			total += value.length;
			if (total > STREAM_BODY_LIMIT_BYTES) {
				throw new Error(
					`inspector request body exceeds ${STREAM_BODY_LIMIT_BYTES} bytes; raise STREAM_BODY_LIMIT_BYTES in lib/fetch-compat.ts`,
				);
			}
			chunks.push(value);
		}
	} finally {
		reader.releaseLock();
	}
	const out = new Uint8Array(new ArrayBuffer(total));
	let offset = 0;
	for (const chunk of chunks) {
		out.set(chunk, offset);
		offset += chunk.length;
	}
	return out.buffer;
}

let installed = false;

/** Idempotent; safe to call before any client is constructed. */
export function installStreamBodyFetchCompat(): void {
	if (installed || typeof globalThis.fetch !== "function") return;
	installed = true;
	const original = globalThis.fetch.bind(globalThis);

	globalThis.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
		const body = init?.body;
		if (!(body instanceof ReadableStream)) return original(input, init);
		const buffered = await bufferStream(body);
		// `duplex` only exists to permit a stream body; carrying it over would
		// re-trigger the same negotiation.
		const { duplex: _duplex, ...rest } = init as RequestInit & {
			duplex?: string;
		};
		return original(input, { ...rest, body: buffered });
	};
}
