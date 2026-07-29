import { createServer, type IncomingMessage } from "node:http";

export const ONE_PIXEL_PNG_BASE64 =
	"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9ZQmcAAAAASUVORK5CYII=";

export const ONE_PIXEL_PNG_BYTES = new Uint8Array(
	Buffer.from(ONE_PIXEL_PNG_BASE64, "base64"),
);

export function requestContainsExactPng(request: unknown): boolean {
	if (Array.isArray(request)) return request.some(requestContainsExactPng);
	if (!request || typeof request !== "object") return false;
	const record = request as Record<string, unknown>;
	const source =
		record.source && typeof record.source === "object"
			? (record.source as Record<string, unknown>)
			: null;
	if (
		record.type === "image" &&
		((record.data === ONE_PIXEL_PNG_BASE64 &&
			record.mimeType === "image/png") ||
			(source?.type === "base64" &&
				source.media_type === "image/png" &&
				source.data === ONE_PIXEL_PNG_BASE64))
	) {
		return true;
	}
	return Object.entries(record).some(
		([key, value]) => key !== "rawOutput" && requestContainsExactPng(value),
	);
}

export function requestContainsExactPngToolResult(request: unknown): boolean {
	if (Array.isArray(request)) {
		return request.some(requestContainsExactPngToolResult);
	}
	if (!request || typeof request !== "object") return false;
	const record = request as Record<string, unknown>;
	if (record.type === "tool_result" && requestContainsExactPng(record)) {
		return true;
	}
	return Object.values(record).some(requestContainsExactPngToolResult);
}

async function readBody(request: IncomingMessage): Promise<Buffer> {
	const chunks: Buffer[] = [];
	for await (const chunk of request) {
		chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
	}
	return Buffer.concat(chunks);
}

export async function startRawRequestCapture(upstreamUrl: string): Promise<{
	url: string;
	requests: unknown[];
	stop: () => Promise<void>;
}> {
	const requests: unknown[] = [];
	const server = createServer(async (request, response) => {
		try {
			const body = await readBody(request);
			if (body.length > 0) requests.push(JSON.parse(body.toString("utf8")));

			const headers = new Headers();
			for (const [name, value] of Object.entries(request.headers)) {
				if (
					value === undefined ||
					name === "host" ||
					name === "content-length" ||
					name === "connection"
				) {
					continue;
				}
				for (const item of Array.isArray(value) ? value : [value]) {
					headers.append(name, item);
				}
			}
			const upstream = await fetch(new URL(request.url ?? "/", upstreamUrl), {
				method: request.method,
				headers,
				body: body.length > 0 ? body : undefined,
			});
			response.statusCode = upstream.status;
			for (const [name, value] of upstream.headers) {
				if (
					name !== "content-length" &&
					name !== "transfer-encoding" &&
					name !== "content-encoding"
				) {
					response.setHeader(name, value);
				}
			}
			response.end(Buffer.from(await upstream.arrayBuffer()));
		} catch (error) {
			response.statusCode = 502;
			response.end(error instanceof Error ? error.message : String(error));
		}
	});
	await new Promise<void>((resolve) => {
		server.listen(0, "127.0.0.1", resolve);
	});
	server.unref();
	const address = server.address();
	if (!address || typeof address === "string") {
		throw new Error("raw request capture did not expose a TCP port");
	}
	return {
		url: `http://127.0.0.1:${address.port}`,
		requests,
		stop: async () => {
			server.closeAllConnections?.();
			await new Promise<void>((resolve, reject) => {
				server.close((error) => (error ? reject(error) : resolve()));
			});
		},
	};
}
