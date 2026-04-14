import { describe, expect, test } from "vitest";
import type {
	SidecarRequestPayload,
	SidecarResponsePayload,
} from "../src/sidecar/native-process-client.js";

describe("HTTP request transform types", () => {
	test("transform_http_request payload shape matches expected contract", () => {
		const request: Extract<
			SidecarRequestPayload,
			{ type: "transform_http_request" }
		> = {
			type: "transform_http_request",
			request_id: "http-42",
			url: "https://api.example.com/v1/data",
			method: "POST",
			headers: {
				authorization: ["Bearer __CREDENTIAL_REF_abc__"],
				"content-type": ["application/json"],
			},
			body: '{"key":"value"}',
		};

		expect(request.type).toBe("transform_http_request");
		expect(request.url).toBe("https://api.example.com/v1/data");
		expect(request.method).toBe("POST");
		expect(request.headers.authorization).toEqual([
			"Bearer __CREDENTIAL_REF_abc__",
		]);
		expect(request.body).toBe('{"key":"value"}');
	});

	test("transform_http_result response can return modified headers only", () => {
		const response: Extract<
			SidecarResponsePayload,
			{ type: "transform_http_result" }
		> = {
			type: "transform_http_result",
			request_id: "http-42",
			headers: {
				authorization: ["Bearer real-secret-token"],
				"content-type": ["application/json"],
			},
		};

		expect(response.url).toBeUndefined();
		expect(response.method).toBeUndefined();
		expect(response.body).toBeUndefined();
		expect(response.error).toBeUndefined();
		expect(response.headers?.authorization).toEqual([
			"Bearer real-secret-token",
		]);
	});

	test("transform_http_result response can return an error", () => {
		const response: Extract<
			SidecarResponsePayload,
			{ type: "transform_http_result" }
		> = {
			type: "transform_http_result",
			request_id: "http-fail",
			error: "credential resolver unavailable",
		};

		expect(response.error).toBe("credential resolver unavailable");
		expect(response.url).toBeUndefined();
		expect(response.headers).toBeUndefined();
	});

	test("transform_http_result response can return all fields", () => {
		const response: Extract<
			SidecarResponsePayload,
			{ type: "transform_http_result" }
		> = {
			type: "transform_http_result",
			request_id: "http-99",
			url: "https://proxy.internal.com/v1/charges",
			method: "PUT",
			headers: {
				authorization: ["Bearer sk_live_xxx"],
				"x-proxy-target": ["api.stripe.com"],
			},
			body: '{"amount":100}',
		};

		expect(response.url).toBe("https://proxy.internal.com/v1/charges");
		expect(response.method).toBe("PUT");
		expect(response.body).toBe('{"amount":100}');
		expect(response.headers?.["x-proxy-target"]).toEqual(["api.stripe.com"]);
	});

	test("SidecarRequestHandler dispatch matches transform_http_request to transform_http_result", () => {
		const request: SidecarRequestPayload = {
			type: "transform_http_request",
			request_id: "http-1",
			url: "https://example.com",
			method: "GET",
			headers: {},
		};

		const matchesResult = (
			req: SidecarRequestPayload,
			res: SidecarResponsePayload,
		): boolean => {
			if (req.type === "transform_http_request") {
				return res.type === "transform_http_result";
			}
			return false;
		};

		const response: SidecarResponsePayload = {
			type: "transform_http_result",
			request_id: "http-1",
		};

		expect(matchesResult(request, response)).toBe(true);
		expect(
			matchesResult(request, {
				type: "tool_invocation_result",
				invocation_id: "x",
			}),
		).toBe(false);
	});
});
