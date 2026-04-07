import {
	type LocalCompatMount,
	serializeMountConfigForSidecar as serializeMountConfig,
} from "./sidecar/rpc-client.js";

export type { LocalCompatMount };

export const serializeMountConfigForSidecar = serializeMountConfig;
