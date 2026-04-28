import type { ZodType } from "zod";

const OPTIONAL_WRAPPER_TYPES = new Set(["default", "optional"]);
const TRANSPARENT_WRAPPER_TYPES = new Set([
	...OPTIONAL_WRAPPER_TYPES,
	"branded",
	"catch",
	"readonly",
]);

const UNSUPPORTED_TYPES = new Set([
	"bigint",
	"date",
	"effects",
	"intersection",
	"pipeline",
	"pipe",
	"record",
	"tuple",
]);

type JsonObject = Record<string, unknown>;

export class HostToolSchemaConversionError extends Error {
	readonly path: string;
	readonly zodType: string;

	constructor(path: string, zodType: string, details?: string) {
		super(
			[
				`Unsupported Zod schema at ${path}: ${zodType}`,
				details ? `(${details})` : undefined,
			]
				.filter(Boolean)
				.join(" "),
		);
		this.name = "HostToolSchemaConversionError";
		this.path = path;
		this.zodType = zodType;
	}
}

function getSchemaDef(schema: unknown): JsonObject {
	return ((
		schema as {
			_def?: JsonObject;
			def?: JsonObject;
		}
	)._def ??
		(schema as { def?: JsonObject }).def ??
		{}) as JsonObject;
}

function getInnerSchema(schema: ZodType): ZodType | undefined {
	const def = getSchemaDef(schema);
	return (def.innerType ?? def.schema ?? def.type ?? def.in) as
		| ZodType
		| undefined;
}

function normalizeTypeName(schema: ZodType): string {
	const def = getSchemaDef(schema);
	const constructorName = String(
		(schema as { constructor?: { name?: string } }).constructor?.name ?? "",
	)
		.replace(/^Zod/, "")
		.toLowerCase();

	if (constructorName === "discriminatedunion" || "discriminator" in def) {
		return "discriminatedunion";
	}

	const rawTypeName = String(
		def.typeName ?? def.type ?? (schema as { type?: string }).type ?? constructorName,
	);

	return rawTypeName.replace(/^Zod/, "").toLowerCase();
}

function displayTypeName(typeName: string): string {
	switch (typeName) {
		case "discriminatedunion":
			return "discriminatedUnion";
		case "nativeenum":
			return "nativeEnum";
		default:
			return typeName;
	}
}

function getDescription(schema: ZodType): string | undefined {
	const def = getSchemaDef(schema);
	if (typeof def.description === "string") {
		return def.description;
	}
	const instanceDescription = (schema as { description?: unknown }).description;
	return typeof instanceDescription === "string"
		? instanceDescription
		: undefined;
}

function joinPath(parent: string, segment: string): string {
	return parent === "$" ? `$.${segment}` : `${parent}.${segment}`;
}

function metadataProducesRefs(schema: ZodType): boolean {
	const meta = (schema as { meta?: () => unknown }).meta?.();
	return (
		typeof meta === "object" &&
		meta !== null &&
		("id" in meta || "$ref" in meta || "$defs" in meta)
	);
}

function getChecks(schema: ZodType): unknown[] {
	const checks = getSchemaDef(schema).checks;
	return Array.isArray(checks) ? checks : [];
}

function isCustomRefinement(check: unknown): boolean {
	const checkDef = getSchemaDef(check);
	const nestedDef = getSchemaDef((check as { _zod?: { def?: JsonObject } })._zod);
	return (
		String(checkDef.check ?? nestedDef.check ?? "").toLowerCase() === "custom"
	);
}

function validateChecks(schema: ZodType, path: string, typeName: string) {
	if (getChecks(schema).some(isCustomRefinement)) {
		throw new HostToolSchemaConversionError(
			path,
			displayTypeName(typeName),
			"custom refinements cannot be represented faithfully in JSON Schema",
		);
	}
}

function validateSchema(schema: ZodType, path: string) {
	const typeName = normalizeTypeName(schema);

	if (metadataProducesRefs(schema)) {
		throw new HostToolSchemaConversionError(
			path,
			displayTypeName(typeName),
			"metadata that emits $ref/$defs is not supported",
		);
	}

	if (UNSUPPORTED_TYPES.has(typeName)) {
		throw new HostToolSchemaConversionError(path, displayTypeName(typeName));
	}

	if (typeName === "discriminatedunion") {
		throw new HostToolSchemaConversionError(path, displayTypeName(typeName));
	}

	if (TRANSPARENT_WRAPPER_TYPES.has(typeName)) {
		const inner = getInnerSchema(schema);
		if (!inner) {
			throw new HostToolSchemaConversionError(
				path,
				displayTypeName(typeName),
				"wrapper schema is missing its inner schema",
			);
		}
		validateSchema(inner, path);
		return;
	}

	if (typeName === "nullable") {
		const inner = getInnerSchema(schema);
		if (!inner) {
			throw new HostToolSchemaConversionError(
				path,
				displayTypeName(typeName),
				"nullable schema is missing its inner schema",
			);
		}
		validateSchema(inner, path);
		return;
	}

	validateChecks(schema, path, typeName);

	if (typeName === "object") {
		const rawShape = getSchemaDef(schema).shape;
		const shape =
			typeof rawShape === "function"
				? (rawShape as () => Record<string, ZodType>)()
				: ((rawShape ?? {}) as Record<string, ZodType>);

		for (const [fieldName, fieldSchema] of Object.entries(shape)) {
			validateSchema(fieldSchema, joinPath(path, fieldName));
		}
		return;
	}

	if (typeName === "array") {
		const itemSchema = getSchemaDef(schema).element as ZodType | undefined;
		if (!itemSchema) {
			throw new HostToolSchemaConversionError(
				path,
				displayTypeName(typeName),
				"array schema is missing its item schema",
			);
		}
		validateSchema(itemSchema, `${path}[]`);
		return;
	}

	if (typeName === "union") {
		const options = getSchemaDef(schema).options;
		if (!Array.isArray(options) || options.length === 0) {
			throw new HostToolSchemaConversionError(
				path,
				displayTypeName(typeName),
				"union schema is missing its options",
			);
		}
		for (let index = 0; index < options.length; index += 1) {
			validateSchema(options[index] as ZodType, `${path}<union:${index}>`);
		}
		return;
	}

	if (typeName === "literal") {
		const values = getSchemaDef(schema).values;
		const literalValues = Array.isArray(values) ? values : [];
		const [literalValue] = literalValues;
		if (
			literalValues.length !== 1 ||
			!("string number boolean".split(" ").includes(typeof literalValue) || literalValue === null)
		) {
			throw new HostToolSchemaConversionError(
				path,
				displayTypeName(typeName),
				"literal values must be JSON primitives",
			);
		}
		return;
	}

	if (
		typeName === "string" ||
		typeName === "number" ||
		typeName === "boolean" ||
		typeName === "enum" ||
		typeName === "nativeenum"
	) {
		return;
	}

	throw new HostToolSchemaConversionError(path, displayTypeName(typeName));
}

function sanitizeJsonSchema(value: unknown): unknown {
	if (Array.isArray(value)) {
		return value.map((item) => sanitizeJsonSchema(item));
	}

	if (!value || typeof value !== "object") {
		return value;
	}

	const record = value as JsonObject;
	const sanitized: JsonObject = {};

	for (const [key, entry] of Object.entries(record)) {
		if (key === "$schema") {
			continue;
		}
		if (key === "additionalProperties" && entry === false && record.type === "object") {
			continue;
		}
		sanitized[key] = sanitizeJsonSchema(entry);
	}

	return sanitized;
}

function findUnsupportedGeneratedKeyword(
	value: unknown,
	path: string,
): { keyword: "$ref" | "$defs"; path: string } | null {
	if (Array.isArray(value)) {
		for (let index = 0; index < value.length; index += 1) {
			const match = findUnsupportedGeneratedKeyword(value[index], `${path}[${index}]`);
			if (match) {
				return match;
			}
		}
		return null;
	}

	if (!value || typeof value !== "object") {
		return null;
	}

	for (const [key, entry] of Object.entries(value as JsonObject)) {
		if (key === "$ref" || key === "$defs") {
			return { keyword: key, path };
		}
		const match = findUnsupportedGeneratedKeyword(entry, joinPath(path, key));
		if (match) {
			return match;
		}
	}

	return null;
}

export function zodToJsonSchema(schema: ZodType): unknown {
	validateSchema(schema, "$");

	const jsonSchema = (
		schema as ZodType & { toJSONSchema?: () => unknown }
	).toJSONSchema?.();
	if (!jsonSchema) {
		throw new HostToolSchemaConversionError(
			"$",
			displayTypeName(normalizeTypeName(schema)),
			"schema does not expose toJSONSchema()",
		);
	}

	const unsupportedKeyword = findUnsupportedGeneratedKeyword(jsonSchema, "$");
	if (unsupportedKeyword) {
		throw new HostToolSchemaConversionError(
			"$",
			displayTypeName(normalizeTypeName(schema)),
			`${unsupportedKeyword.keyword} emitted at ${unsupportedKeyword.path} is not supported`,
		);
	}

	const sanitized = sanitizeJsonSchema(jsonSchema) as JsonObject;
	const description = getDescription(schema);
	return description && typeof sanitized === "object" && sanitized !== null
		? { ...sanitized, description }
		: sanitized;
}
