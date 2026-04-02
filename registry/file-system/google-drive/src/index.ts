import type {
	MountConfigJsonObject,
	NativeMountPluginDescriptor,
} from "@rivet-dev/agent-os";

export type GoogleDriveCredentials = MountConfigJsonObject & {
	clientEmail: string;
	privateKey: string;
};

export interface GoogleDriveFsOptions {
	credentials: GoogleDriveCredentials;
	folderId: string;
	keyPrefix?: string;
	chunkSize?: number;
	inlineThreshold?: number;
}

export type GoogleDriveMountPluginConfig = MountConfigJsonObject & {
	credentials: GoogleDriveCredentials;
	folderId: string;
	keyPrefix?: string;
	chunkSize?: number;
	inlineThreshold?: number;
};

/**
 * Create a declarative Google Drive native mount descriptor.
 *
 * This keeps the package on the public mount-helper surface while routing
 * first-party Google Drive-backed filesystems through the native
 * `google_drive` plugin instead of a TypeScript runtime package.
 */
export function createGoogleDriveBackend(
	options: GoogleDriveFsOptions,
): NativeMountPluginDescriptor<GoogleDriveMountPluginConfig> {
	return {
		id: "google_drive",
		config: {
			credentials: options.credentials,
			folderId: options.folderId,
			...(options.keyPrefix ? { keyPrefix: options.keyPrefix } : {}),
			...(options.chunkSize != null ? { chunkSize: options.chunkSize } : {}),
			...(options.inlineThreshold != null
				? { inlineThreshold: options.inlineThreshold }
				: {}),
		},
	};
}
