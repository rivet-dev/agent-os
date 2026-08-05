export const SIDECAR_PROTOCOL_SCHEMA = {
	name: "agentos-sidecar",
	version: 8,
} as const;

export type LiveProtocolSchema = typeof SIDECAR_PROTOCOL_SCHEMA;

export type ProtocolSchemaLike = {
	name: string;
	version: number;
};

export function validateSidecarProtocolSchema(
	schema: ProtocolSchemaLike,
): LiveProtocolSchema {
	if (
		schema.name !== SIDECAR_PROTOCOL_SCHEMA.name ||
		schema.version !== SIDECAR_PROTOCOL_SCHEMA.version
	) {
		throw new Error(
			`unsupported sidecar protocol schema ${schema.name}@${schema.version}`,
		);
	}
	return SIDECAR_PROTOCOL_SCHEMA;
}
