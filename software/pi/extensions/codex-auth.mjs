export const codexProviderConfig = {
	apiKey: "$OPENAI_CODEX_ACCESS_TOKEN",
	headers: {
		"chatgpt-account-id": "$OPENAI_CODEX_ACCOUNT_ID",
	},
};

export default function codexAuthExtension(pi) {
	pi.registerProvider("openai-codex", codexProviderConfig);
}
