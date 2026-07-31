import { execFile } from "node:child_process";
import { accessSync, constants } from "node:fs";
import { rm, writeFile } from "node:fs/promises";
import { delimiter, resolve } from "node:path";
import { promisify } from "node:util";
import { randomUUID } from "node:crypto";

import { stream as streamOpenAICodexResponses } from "@earendil-works/pi-ai/api/openai-codex-responses";

const CODEX_BASE_URL = "https://chatgpt.com/backend-api";
const SYNTHETIC_TOKEN_ENV = "AGENTOS_CODEX_ACCOUNT_TOKEN";
const CREDENTIAL_COMMAND = "agentos-codex-auth";
const execFileAsync = promisify(execFile);

export function createAccountToken(accountId) {
	const payload = Buffer.from(
		JSON.stringify({
			"https://api.openai.com/auth": { chatgpt_account_id: accountId },
		}),
	).toString("base64url");
	return `e30.${payload}.agentos`;
}

export function createCodexFetch(accessToken, accountId, upstreamFetch = globalThis.fetch) {
	return (input, init) => {
		const url = input instanceof Request ? input.url : String(input);
		if (url !== CODEX_BASE_URL && !url.startsWith(`${CODEX_BASE_URL}/`)) return upstreamFetch(input, init);

		const headers = new Headers(init?.headers ?? (input instanceof Request ? input.headers : undefined));
		headers.set("authorization", `Bearer ${accessToken}`);
		headers.set("chatgpt-account-id", accountId);
		return upstreamFetch(input, { ...init, headers });
	};
}

export function parseCodexCredential(value) {
	const parsed = typeof value === "string" ? JSON.parse(value) : value;
	const credential = parsed?.result ?? parsed;
	const accessToken = credential?.accessToken?.trim();
	const accountId = credential?.accountId?.trim();
	if (accessToken && accountId) return { accessToken, accountId };
	throw new Error("Codex credential binding returned an invalid credential");
}

export function createDynamicCodexFetch(resolveCredential, upstreamFetch = globalThis.fetch) {
	return async (input, init) => {
		const url = input instanceof Request ? input.url : String(input);
		if (url !== CODEX_BASE_URL && !url.startsWith(`${CODEX_BASE_URL}/`)) return upstreamFetch(input, init);

		const { accessToken, accountId } = parseCodexCredential(await resolveCredential());
		return createCodexFetch(accessToken, accountId, upstreamFetch)(input, init);
	};
}

async function requestBody(input, init) {
	const body = init?.body;
	if (typeof body === "string") return Buffer.from(body);
	if (body instanceof URLSearchParams) return Buffer.from(body.toString());
	if (body instanceof ArrayBuffer) return Buffer.from(body);
	if (ArrayBuffer.isView(body)) return Buffer.from(body.buffer, body.byteOffset, body.byteLength);
	if (body instanceof Blob) return Buffer.from(await body.arrayBuffer());
	if (body) return Buffer.from(await new Response(body).arrayBuffer());
	if (input instanceof Request && input.body) return Buffer.from(await input.clone().arrayBuffer());
	return Buffer.alloc(0);
}

function parseBoundResponse(value) {
	const parsed = typeof value === "string" ? JSON.parse(value) : value;
	const response = parsed?.result ?? parsed;
	if (
		!Number.isInteger(response?.status)
		|| typeof response?.bodyBase64 !== "string"
		|| !response?.headers
		|| typeof response.headers !== "object"
	) throw new Error("Codex request binding returned an invalid response");
	return response;
}

export function createBoundCodexFetch(invokeBinding, upstreamFetch = globalThis.fetch) {
	return async (input, init) => {
		const url = input instanceof Request ? input.url : String(input);
		if (url !== CODEX_BASE_URL && !url.startsWith(`${CODEX_BASE_URL}/`)) return upstreamFetch(input, init);
		const headers = new Headers(init?.headers ?? (input instanceof Request ? input.headers : undefined));
		headers.delete("authorization");
		headers.delete("chatgpt-account-id");
		const response = parseBoundResponse(await invokeBinding({
			target: url,
			method: init?.method ?? (input instanceof Request ? input.method : "GET"),
			headers: Object.fromEntries(headers),
			bodyBase64: (await requestBody(input, init)).toString("base64"),
		}));
		return new Response(Buffer.from(response.bodyBase64, "base64"), {
			status: response.status,
			statusText: typeof response.statusText === "string" ? response.statusText : "",
			headers: response.headers,
		});
	};
}

function commandExists(command) {
	for (const directory of (process.env.PATH ?? "").split(delimiter)) {
		if (!directory) continue;
		try {
			accessSync(resolve(directory, command), constants.X_OK);
			return true;
		} catch {}
	}
	return false;
}

async function invokeBoundRequest(request) {
	const path = `/tmp/agentos-codex-request-${process.pid}-${randomUUID()}.json`;
	try {
		await writeFile(path, JSON.stringify(request), { mode: 0o600 });
		const { stdout } = await execFileAsync(CREDENTIAL_COMMAND, ["request", "--json-file", path], {
			timeout: 120_000,
			maxBuffer: 64 * 1024 * 1024,
		});
		return parseBoundResponse(stdout);
	} finally {
		await rm(path, { force: true });
	}
}

function createCodexProviderConfig(createFetch) {
	return {
		api: "openai-codex-responses",
		apiKey: `$${SYNTHETIC_TOKEN_ENV}`,
		streamSimple: (model, context, options) =>
			streamOpenAICodexResponses(model, context, {
				...options,
				fetch: createFetch(options?.fetch ?? globalThis.fetch),
				transport: "sse",
			}),
	};
}

export default function codexAuthExtension(pi) {
	const accessToken = process.env.OPENAI_CODEX_ACCESS_TOKEN?.trim();
	const accountId = process.env.OPENAI_CODEX_ACCOUNT_ID?.trim();
	const staticCredential = accessToken && accountId ? { accessToken, accountId } : null;
	if (!staticCredential && !commandExists(CREDENTIAL_COMMAND)) return;

	process.env[SYNTHETIC_TOKEN_ENV] = createAccountToken(staticCredential?.accountId ?? "agentos-bound-account");
	pi.registerProvider(
		"openai-codex",
		createCodexProviderConfig(
			staticCredential
				? (upstreamFetch) => createDynamicCodexFetch(async () => staticCredential, upstreamFetch)
				: (upstreamFetch) => createBoundCodexFetch(invokeBoundRequest, upstreamFetch),
		),
	);
}
