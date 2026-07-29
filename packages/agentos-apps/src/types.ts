export interface AppScaling {
	minReplicas?: number;
	maxReplicas?: number;
	targetConcurrency?: number;
}

interface DeployAppBase {
	/** Stable URL-safe identifier used for routing and namespace isolation. */
	appId: string;
	/**
	 * Create a stable Rivet namespace for this app. By default, deployments use
	 * the namespace already configured for the ordinary Rivet connection.
	 */
	createNamespace?: boolean;
	regions?: string[];
	scaling?: AppScaling;
}

export type DeployAppInput =
	| (DeployAppBase & {
			/** Local application directory. */
			source: URL;
			files?: never;
	  })
	| (DeployAppBase & {
			/** Complete generated application tree. */
			files: Record<string, string | Uint8Array>;
			source?: never;
	  });

export interface Deployment {
	appId: string;
	release: string;
	namespace: string;
	pool: string;
	regions: string[];
}

export interface AppReleaseInfo {
	release: string;
	artifactHash: string;
	artifactBytes: number;
	createdAt: number;
	regions: string[];
	scaling: Required<AppScaling>;
	status: "building" | "ready" | "failed";
	error?: string;
}

export interface PreparedDeployAppInput {
	appId: string;
	files: Record<string, Uint8Array>;
	regions?: string[];
	scaling?: AppScaling;
	namespace: string;
	runtime: {
		endpoint: string;
		pool: string;
	};
}
