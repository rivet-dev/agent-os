export const movedMessage =
	'Dynamic Apps moved to @rivet-dev/dynamic-apps. Install it and import from "@rivet-dev/dynamic-apps".';

throw new Error(movedMessage);

/** @deprecated Install and import from `@rivet-dev/dynamic-apps`. */
export class AgentOSAppsError extends Error {}

/** @deprecated Install and import from `@rivet-dev/dynamic-apps`. */
export function setupApps(): never {
	throw new Error(movedMessage);
}

/** @deprecated Install and import from `@rivet-dev/dynamic-apps`. */
export function deployApp(_input: DeployAppInput): never {
	throw new Error(movedMessage);
}

/** @deprecated Install and import from `@rivet-dev/dynamic-apps`. */
export const appsRouter: never = undefined as never;

/** @deprecated Install and import from `@rivet-dev/dynamic-apps`. */
export interface AppScaling {
	minReplicas?: number;
	maxReplicas?: number;
	targetConcurrency?: number;
}

/** @deprecated Install and import from `@rivet-dev/dynamic-apps`. */
export interface DeployAppInput {
	appId: string;
	source?: URL;
	files?: Record<string, string | Uint8Array>;
	createNamespace?: boolean;
	regions?: string[];
	scaling?: AppScaling;
}

/** @deprecated Install and import from `@rivet-dev/dynamic-apps`. */
export interface Deployment {
	appId: string;
	release: string;
	namespace: string;
	pool: string;
	regions: string[];
}
