import { execFile } from "node:child_process";
import { accessSync, constants } from "node:fs";
import { delimiter, resolve } from "node:path";
import { promisify } from "node:util";

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

	const proxyUrl = credential?.proxyUrl?.trim();
	const proxyToken = credential?.proxyToken?.trim();
	if (proxyUrl && proxyToken && accountId) {
		const parsedUrl = new URL(proxyUrl);
		if (parsedUrl.protocol === "http:" || parsedUrl.protocol === "https:") {
			return { proxyUrl: parsedUrl.toString(), proxyToken, accountId };
		}
	}
	throw new Error("Codex credential binding returned an invalid credential");
}

export function createDynamicCodexFetch(resolveCredential, upstreamFetch = globalThis.fetch) {
	return async (input, init) => {
		const url = input instanceof Request ? input.url : String(input);
		if (url !== CODEX_BASE_URL && !url.startsWith(`${CODEX_BASE_URL}/`)) return upstreamFetch(input, init);

		const credential = parseCodexCredential(await resolveCredential());
		if (credential.accessToken) {
			return createCodexFetch(credential.accessToken, credential.accountId, upstreamFetch)(input, init);
		}

		const proxyUrl = new URL(credential.proxyUrl);
		proxyUrl.searchParams.set("target", url);
		const headers = new Headers(init?.headers ?? (input instanceof Request ? input.headers : undefined));
		headers.set("authorization", `Bearer ${credential.proxyToken}`);
		headers.delete("chatgpt-account-id");
		return upstreamFetch(proxyUrl, {
			...(input instanceof Request
				? { method: input.method, body: input.body }
				: {}),
			...init,
			headers,
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

async function loadBoundCredential() {
	const { stdout } = await execFileAsync(CREDENTIAL_COMMAND, ["get"], {
		timeout: 10_000,
		maxBuffer: 1024 * 1024,
	});
	return parseCodexCredential(stdout);
}

function createCodexProviderConfig(resolveCredential) {
	return {
		api: "openai-codex-responses",
		apiKey: `$${SYNTHETIC_TOKEN_ENV}`,
		streamSimple: (model, context, options) =>
			streamOpenAICodexResponses(model, context, {
				...options,
				fetch: createDynamicCodexFetch(resolveCredential, options?.fetch ?? globalThis.fetch),
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
		createCodexProviderConfig(staticCredential ? async () => staticCredential : loadBoundCredential),
	);
}
