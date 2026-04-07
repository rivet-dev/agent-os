import type { ZodType } from "zod";

const OPTIONAL_WRAPPER_TYPES = new Set(["default", "optional"]);
const TRANSPARENT_WRAPPER_TYPES = new Set([
	...OPTIONAL_WRAPPER_TYPES,
	"branded",
	"catch",
	"effects",
	"pipe",
	"pipeline",
	"readonly",
]);

function getSchemaDef(schema: ZodType): Record<string, unknown> {
	return (((schema as unknown) as {
		_def?: Record<string, unknown>;
		def?: Record<string, unknown>;
	})
		._def ??
		((schema as unknown) as { def?: Record<string, unknown> }).def ??
		{}) as Record<string, unknown>;
}

function unwrapSchema(schema: ZodType): {
	schema: ZodType;
	typeName: string;
	isOptional: boolean;
} {
	let current = schema;
	let isOptional = false;

	while (true) {
		const def = getSchemaDef(current);
		const typeName = String(
			def.typeName ?? def.type ?? (current as { type?: string }).type ?? "",
		)
			.replace(/^Zod/, "")
			.toLowerCase();

		if (!TRANSPARENT_WRAPPER_TYPES.has(typeName)) {
			return { schema: current, typeName, isOptional };
		}

		if (OPTIONAL_WRAPPER_TYPES.has(typeName)) {
			isOptional = true;
		}

		const inner = (def.innerType ?? def.schema ?? def.type ?? def.in) as
			| ZodType
			| undefined;
		if (!inner) {
			return { schema: current, typeName, isOptional };
		}
		current = inner;
	}
}

function getDescription(schema: ZodType): string | undefined {
	const def = getSchemaDef(schema);
	if (typeof def.description === "string") {
		return def.description;
	}
	const instanceDescription = (schema as unknown as { description?: unknown })
		.description;
	return typeof instanceDescription === "string" ? instanceDescription : undefined;
}

function withDescription(schema: ZodType, value: Record<string, unknown>) {
	const description = getDescription(schema);
	return description ? { ...value, description } : value;
}

export function zodToJsonSchema(schema: ZodType): unknown {
	const { schema: unwrapped, typeName } = unwrapSchema(schema);
	const def = getSchemaDef(unwrapped);

	if (typeName === "object") {
		const rawShape = def.shape;
		const shape =
			typeof rawShape === "function"
				? (rawShape as () => Record<string, ZodType>)()
				: ((rawShape ?? {}) as Record<string, ZodType>);
		const properties: Record<string, unknown> = {};
		const required: string[] = [];

		for (const [fieldName, fieldSchema] of Object.entries(shape)) {
			const { isOptional } = unwrapSchema(fieldSchema);
			properties[fieldName] = zodToJsonSchema(fieldSchema);
			if (!isOptional) {
				required.push(fieldName);
			}
		}

		return withDescription(schema, {
			type: "object",
			properties,
			...(required.length > 0 ? { required } : {}),
		});
	}

	if (typeName === "array") {
		const itemSchema = (def.element ?? def.type) as ZodType | undefined;
		return withDescription(schema, {
			type: "array",
			items: itemSchema ? zodToJsonSchema(itemSchema) : { type: "string" },
		});
	}

	if (typeName === "string") {
		const enumValues = Array.isArray(def.values)
			? def.values
			: Array.isArray(def.entries)
				? def.entries
				: undefined;
		return withDescription(
			schema,
			enumValues ? { type: "string", enum: enumValues } : { type: "string" },
		);
	}

	if (typeName === "enum" || typeName === "nativeenum") {
		const enumValues =
			Array.isArray(def.values)
				? def.values
				: Object.values((def.entries ?? {}) as Record<string, string>);
		return withDescription(schema, { type: "string", enum: enumValues });
	}

	if (typeName === "number") {
		return withDescription(schema, { type: "number" });
	}

	if (typeName === "boolean") {
		return withDescription(schema, { type: "boolean" });
	}

	return withDescription(schema, { type: "string" });
}
