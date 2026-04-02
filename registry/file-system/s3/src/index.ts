import type {
	MountConfigJsonObject,
	NativeMountPluginDescriptor,
} from "@rivet-dev/agent-os";

export type S3Credentials = MountConfigJsonObject & {
	accessKeyId: string;
	secretAccessKey: string;
};

export interface S3FsOptions {
	bucket: string;
	prefix?: string;
	region?: string;
	credentials?: S3Credentials;
	endpoint?: string;
	chunkSize?: number;
	inlineThreshold?: number;
}

export type S3MountPluginConfig = MountConfigJsonObject & {
	bucket: string;
	prefix?: string;
	region?: string;
	credentials?: S3Credentials;
	endpoint?: string;
	chunkSize?: number;
	inlineThreshold?: number;
};

/**
 * Create a declarative S3 mount plugin descriptor.
 *
 * This keeps the legacy helper name while routing first-party S3-backed mounts
 * through the native `s3` plugin instead of a TypeScript runtime package.
 */
export function createS3Backend(
	options: S3FsOptions,
): NativeMountPluginDescriptor<S3MountPluginConfig> {
	return {
		id: "s3",
		config: {
			bucket: options.bucket,
			...(options.prefix ? { prefix: options.prefix } : {}),
			...(options.region ? { region: options.region } : {}),
			...(options.credentials ? { credentials: options.credentials } : {}),
			...(options.endpoint ? { endpoint: options.endpoint } : {}),
			...(options.chunkSize != null ? { chunkSize: options.chunkSize } : {}),
			...(options.inlineThreshold != null
				? { inlineThreshold: options.inlineThreshold }
				: {}),
		},
	};
}
