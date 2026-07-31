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

function bindingResult(value) {
	const parsed = typeof value === "string" ? JSON.parse(value) : value;
	return parsed?.result ?? parsed;
}

function parseBoundStart(value) {
	const response = bindingResult(value);
	if (
		!Number.isInteger(response?.status)
		|| typeof response?.requestId !== "string"
		|| !response?.headers
		|| typeof response.headers !== "object"
	) throw new Error("Codex request binding returned invalid start metadata");
	return response;
}

function parseBoundChunk(value) {
	const chunk = bindingResult(value);
	if (typeof chunk?.done !== "boolean" || typeof chunk?.chunkBase64 !== "string") {
		throw new Error("Codex request binding returned an invalid stream chunk");
	}
	return chunk;
}

export function createBoundCodexFetch(invokeBinding, upstreamFetch = globalThis.fetch) {
	return async (input, init) => {
		const url = input instanceof Request ? input.url : String(input);
		if (url !== CODEX_BASE_URL && !url.startsWith(`${CODEX_BASE_URL}/`)) return upstreamFetch(input, init);
		const headers = new Headers(init?.headers ?? (input instanceof Request ? input.headers : undefined));
		headers.delete("authorization");
		headers.delete("chatgpt-account-id");
		const response = parseBoundStart(await invokeBinding("start", {
			target: url,
			method: init?.method ?? (input instanceof Request ? input.method : "GET"),
			headers: Object.fromEntries(headers),
			bodyBase64: (await requestBody(input, init)).toString("base64"),
		}));
		let closed = false;
		const cancel = async () => {
			if (closed) return;
			closed = true;
			await invokeBinding("cancel", { requestId: response.requestId }).catch(() => undefined);
		};
		const stream = new ReadableStream({
			start(controller) {
				if (!init?.signal) return;
				if (init.signal.aborted) {
					void cancel();
					controller.error(init.signal.reason);
					return;
				}
				init.signal.addEventListener("abort", () => {
					void cancel();
					controller.error(init.signal.reason);
				}, { once: true });
			},
			async pull(controller) {
				if (closed) return;
				try {
					const chunk = parseBoundChunk(await invokeBinding("read", { requestId: response.requestId }));
					if (closed) return;
					if (chunk.chunkBase64) controller.enqueue(Buffer.from(chunk.chunkBase64, "base64"));
					if (chunk.done) {
						closed = true;
						controller.close();
					}
				} catch (error) {
					closed = true;
					controller.error(error);
				}
			},
			cancel,
		});
		return new Response(stream, {
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

async function invokeBoundCommand(command, input) {
	if (command === "start") {
		const path = `/tmp/agentos-codex-request-${process.pid}-${randomUUID()}.json`;
		try {
			await writeFile(path, JSON.stringify(input), { mode: 0o600 });
			const { stdout } = await execFileAsync(CREDENTIAL_COMMAND, ["start", "--json-file", path], {
				timeout: 120_000,
				maxBuffer: 1024 * 1024,
			});
			return bindingResult(stdout);
		} finally {
			await rm(path, { force: true });
		}
	}
	const { stdout } = await execFileAsync(
		CREDENTIAL_COMMAND,
		[command, "--request-id", input.requestId],
		{ timeout: command === "read" ? 10 * 60_000 : 10_000, maxBuffer: 8 * 1024 * 1024 },
	);
	return bindingResult(stdout);
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
	const bound = commandExists(CREDENTIAL_COMMAND);
	const staticCredential = !bound
		&& process.env.AGENTOS_CODEX_ALLOW_ENV_AUTH === "1"
		&& accessToken
		&& accountId
		? { accessToken, accountId }
		: null;
	if (!staticCredential && !bound) return;

	process.env[SYNTHETIC_TOKEN_ENV] = createAccountToken(staticCredential?.accountId ?? "agentos-bound-account");
	pi.registerProvider(
		"openai-codex",
		createCodexProviderConfig(
			staticCredential
				? (upstreamFetch) => createDynamicCodexFetch(async () => staticCredential, upstreamFetch)
				: (upstreamFetch) => createBoundCodexFetch(invokeBoundCommand, upstreamFetch),
		),
	);
}
