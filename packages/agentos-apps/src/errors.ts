export class AgentOSAppsError extends Error {
	readonly code: string;
	readonly metadata?: Record<string, unknown>;

	constructor(
		code: string,
		message: string,
		metadata?: Record<string, unknown>,
	) {
		super(message);
		this.name = "AgentOSAppsError";
		this.code = code;
		this.metadata = metadata;
	}
}
