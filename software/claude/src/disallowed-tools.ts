/**
 * Adapter-neutral launch contract for disallowing built-in agent tools.
 *
 * The sidecar owns no agent-specific tool policy: it forwards the caller's
 * session `env` to the packaged adapter, and this AgentOS-owned launcher
 * translates the neutral `ACP_DISALLOWED_TOOLS` value into the Claude Agent
 * SDK's `disallowedTools` option. Mirrors how `ACP_APPEND_SYSTEM_PROMPT` is
 * translated into each upstream adapter's own prompt flag.
 */

export const DISALLOWED_TOOLS_ENV = "ACP_DISALLOWED_TOOLS";

function invalid(detail: string): Error {
	return new Error(
		`Invalid ${DISALLOWED_TOOLS_ENV}: ${detail}. Provide a comma-separated list of tool names (\`WebFetch,WebSearch\`) or a JSON array of strings (\`["WebFetch","WebSearch"]\`).`,
	);
}

function normalize(names: string[]): string[] {
	const seen = new Set<string>();
	for (const name of names) {
		seen.add(name);
	}
	return [...seen];
}

/**
 * Parse the launch contract value into an ordered, de-duplicated tool list.
 *
 * An unset or blank value keeps the adapter's default behavior. A value that is
 * present but unusable is a hard error rather than a silent empty list, so a
 * mistyped policy fails the session instead of quietly leaving the tool enabled.
 */
export function parseDisallowedTools(raw: string | undefined): string[] {
	const value = raw?.trim();
	if (!value) return [];
	if (value.startsWith("{")) {
		throw invalid("JSON object values are not supported");
	}
	if (value.startsWith("[")) {
		let parsed: unknown;
		try {
			parsed = JSON.parse(value);
		} catch (error) {
			throw invalid(`${(error as Error).message}`);
		}
		if (!Array.isArray(parsed)) throw invalid("JSON value is not an array");
		const names = parsed.map((entry) => {
			if (typeof entry !== "string") {
				throw invalid(`JSON array entry is not a string: ${JSON.stringify(entry)}`);
			}
			const name = entry.trim();
			if (!name) throw invalid("JSON array contains an empty tool name");
			return name;
		});
		return normalize(names);
	}
	const names = value
		.split(",")
		.map((entry) => entry.trim())
		.filter((entry) => entry.length > 0);
	if (names.length === 0) throw invalid("no tool names were found");
	return normalize(names);
}

function asRecord(value: unknown): Record<string, unknown> {
	return value && typeof value === "object" && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: {};
}

/**
 * Merge the launch-contract tools into the session's `_meta.claudeCode.options`
 * without mutating the caller's params. Caller-supplied entries are preserved;
 * the upstream adapter appends its own always-disallowed tools afterwards.
 */
export function withDisallowedTools<T extends Record<string, unknown>>(
	params: T,
	disallowedTools: string[],
): T {
	if (disallowedTools.length === 0) return params;
	const meta = asRecord(params._meta);
	const claudeCode = asRecord(meta.claudeCode);
	const options = asRecord(claudeCode.options);
	const existing = Array.isArray(options.disallowedTools)
		? options.disallowedTools.filter(
				(entry): entry is string => typeof entry === "string",
			)
		: [];
	return {
		...params,
		_meta: {
			...meta,
			claudeCode: {
				...claudeCode,
				options: {
					...options,
					disallowedTools: normalize([...existing, ...disallowedTools]),
				},
			},
		},
	};
}
