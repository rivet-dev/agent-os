import { createAppsActors } from "./actors.js";

export { deployApp } from "./deploy.js";
export { AgentOSAppsError } from "./errors.js";
export { appsRouter } from "./router.js";
export type {
	AppScaling,
	DeployAppInput,
	Deployment,
} from "./types.js";

export function setupApps() {
	return {
		appsActors: createAppsActors(),
	};
}
