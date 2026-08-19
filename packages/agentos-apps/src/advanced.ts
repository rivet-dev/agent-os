export const movedMessage =
	'Dynamic Apps moved to @rivet-dev/dynamic-apps. Install it and import from "@rivet-dev/dynamic-apps".';

throw new Error(movedMessage);

/** @deprecated Install and import from `@rivet-dev/dynamic-apps`. */
export interface AgentOSAppsRoutingClient {
	agentOSAppsApp: {
		getOrCreate(key?: string | string[]): {
			fetch(request: Request): Promise<Response>;
		};
	};
}

/** @deprecated Install and import from `@rivet-dev/dynamic-apps`. */
export interface CreateAppsRouterOptions {
	client?: AgentOSAppsRoutingClient;
}

/** @deprecated Install and import from `@rivet-dev/dynamic-apps`. */
export function createAppsRouter(_options?: CreateAppsRouterOptions): never {
	throw new Error(movedMessage);
}
