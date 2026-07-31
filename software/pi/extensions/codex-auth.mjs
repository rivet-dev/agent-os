import { stream as streamOpenAICodexResponses } from "@earendil-works/pi-ai/api/openai-codex-responses";

const CODEX_BASE_URL = "https://chatgpt.com/backend-api";
const SYNTHETIC_TOKEN_ENV = "AGENTOS_CODEX_ACCOUNT_TOKEN";

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

function createCodexProviderConfig(accessToken, accountId) {
	return {
		api: "openai-codex-responses",
		apiKey: `$${SYNTHETIC_TOKEN_ENV}`,
		streamSimple: (model, context, options) =>
			streamOpenAICodexResponses(model, context, {
				...options,
				fetch: createCodexFetch(accessToken, accountId, options?.fetch ?? globalThis.fetch),
				transport: "sse",
			}),
	};
}

export default function codexAuthExtension(pi) {
	const accessToken = process.env.OPENAI_CODEX_ACCESS_TOKEN?.trim();
	const accountId = process.env.OPENAI_CODEX_ACCOUNT_ID?.trim();
	if (!accessToken || !accountId) return;

	process.env[SYNTHETIC_TOKEN_ENV] = createAccountToken(accountId);
	pi.registerProvider("openai-codex", createCodexProviderConfig(accessToken, accountId));
}
