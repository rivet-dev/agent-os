import { describe, expect, it } from "vitest";
import {
	buildHttpRequest,
	buildHttpResponse,
	handleProxyRequest,
	readHttpRequest,
	readHttpResponse,
} from "../../src/openai-proxy.js";

const text = (s: string) => new TextEncoder().encode(s);
// A readChunk that yields the whole buffer once then EOF (the message is already
// complete in `initial`, so these helpers shouldn't need it).
const noMore = () => null;

describe("HTTP framing", () => {
	it("round-trips a request through build + read", () => {
		const bytes = buildHttpRequest("POST", "/v1/chat/completions", '{"hi":1}');
		const req = readHttpRequest(bytes, noMore);
		expect(req).not.toBeNull();
		expect(req!.method).toBe("POST");
		expect(req!.path).toBe("/v1/chat/completions");
		expect(req!.headers.get("content-type")).toBe("application/json");
		expect(req!.body).toBe('{"hi":1}');
	});

	it("round-trips a response through build + read", () => {
		const bytes = buildHttpResponse(200, '{"ok":true}');
		const res = readHttpResponse(bytes, noMore);
		expect(res!.status).toBe(200);
		expect(res!.body).toBe('{"ok":true}');
	});

	it("reassembles a body that arrives in chunks (partial reads)", () => {
		const full = buildHttpRequest("POST", "/v1/messages", '{"a":"bcdef"}');
		// Deliver: first 10 bytes in `initial`, the rest in two readChunk calls.
		const initial = full.subarray(0, 10);
		const rest = [full.subarray(10, 25), full.subarray(25)];
		let i = 0;
		const req = readHttpRequest(initial, () => (i < rest.length ? rest[i++] : null));
		expect(req!.method).toBe("POST");
		expect(req!.body).toBe('{"a":"bcdef"}');
	});

	it("returns null when headers never complete (EOF before CRLFCRLF)", () => {
		expect(readHttpRequest(text("POST /x HTTP/1.1\r\nHost: a"), noMore)).toBeNull();
	});
});

describe("handleProxyRequest", () => {
	it("forwards a chat-completions POST body to infer and wraps the reply as 200", async () => {
		const reqBytes = buildHttpRequest("POST", "/v1/chat/completions", '{"messages":[]}');
		const req = readHttpRequest(reqBytes, noMore)!;
		let seen = "";
		const out = await handleProxyRequest(req, (body) => {
			seen = body;
			return '{"choices":[{"message":{"content":"hi"}}]}';
		});
		const res = readHttpResponse(out, noMore)!;
		expect(seen).toBe('{"messages":[]}');
		expect(res.status).toBe(200);
		expect(JSON.parse(res.body).choices[0].message.content).toBe("hi");
	});

	it("404s a non-chat path without calling infer", async () => {
		const req = readHttpRequest(buildHttpRequest("GET", "/health", ""), noMore)!;
		let called = false;
		const out = await handleProxyRequest(req, () => ((called = true), "x"));
		expect(readHttpResponse(out, noMore)!.status).toBe(404);
		expect(called).toBe(false);
	});
});
