import type { ZodType } from "zod";
import type { HostTool, ToolKit } from "../host-tools.js";
import {
	getZodDescription,
	getZodEnumValues,
	getZodObjectShape,
	unwrapType,
} from "../host-tools-argv.js";
import type {
	AuthenticatedSession,
	CreatedVm,
	NativeSidecarProcessClient,
	SidecarRegisteredToolDefinition,
	SidecarRequestFrame,
	SidecarResponsePayload,
} from "./native-process-client.js";

function validationMessage(error: unknown): string {
	if (
		typeof error === "object" &&
		error !== null &&
		"issues" in error &&
		Array.isArray((error as { issues?: unknown[] }).issues)
	) {
		return (error as { issues: Array<{ message: string; path?: unknown[] }> }).issues
			.map((issue) => {
				const path =
					Array.isArray(issue.path) && issue.path.length > 0
						? ` at "${issue.path.join(".")}"`
						: "";
				return `${issue.message}${path}`;
			})
			.join("; ");
	}
	return error instanceof Error ? error.message : String(error);
}

function zodFieldToJsonSchema(schema: ZodType): unknown {
	const { typeName } = unwrapType(schema);
	const description = getZodDescription(schema);
	const withDescription = (value: Record<string, unknown>) =>
		description ? { ...value, description } : value;

	if (typeName === "ZodString") {
		const enumValues = getZodEnumValues(schema);
		return withDescription(
			enumValues ? { type: "string", enum: enumValues } : { type: "string" },
		);
	}
	if (typeName === "ZodNumber") {
		return withDescription({ type: "number" });
	}
	if (typeName === "ZodBoolean") {
		return withDescription({ type: "boolean" });
	}
	if (typeName === "ZodArray") {
		const shape = ((schema as unknown as { _def?: { type?: ZodType; element?: ZodType } })
			._def ?? {}) as { type?: ZodType; element?: ZodType };
		const itemSchema = shape.element ?? shape.type;
		return withDescription({
			type: "array",
			items: itemSchema ? zodFieldToJsonSchema(itemSchema) : { type: "string" },
		});
	}
	if (typeName === "ZodObject") {
		return zodToJsonSchema(schema);
	}
	return withDescription({ type: "string" });
}

export function zodToJsonSchema(schema: ZodType): unknown {
	const properties: Record<string, unknown> = {};
	const required: string[] = [];

	for (const [fieldName, fieldSchema] of Object.entries(getZodObjectShape(schema))) {
		const { isOptional } = unwrapType(fieldSchema);
		properties[fieldName] = zodFieldToJsonSchema(fieldSchema);
		if (!isOptional) {
			required.push(fieldName);
		}
	}

	return {
		type: "object",
		properties,
		...(required.length > 0 ? { required } : {}),
	};
}

function toolToSidecarDefinition(tool: HostTool): SidecarRegisteredToolDefinition {
	return {
		description: tool.description,
		inputSchema: zodToJsonSchema(tool.inputSchema),
		...(tool.timeout !== undefined ? { timeoutMs: tool.timeout } : {}),
		...(tool.examples && tool.examples.length > 0
			? {
					examples: tool.examples.map((example) => ({
						description: example.description,
						input: example.input,
					})),
				}
			: {}),
	};
}

export async function registerToolkitsOnSidecar(
	client: NativeSidecarProcessClient,
	session: AuthenticatedSession,
	vm: CreatedVm,
	toolKits: ToolKit[],
): Promise<string> {
	if (toolKits.length === 0) {
		client.setSidecarRequestHandler(null);
		return "";
	}

	const toolMap = new Map<string, HostTool>();
	for (const toolKit of toolKits) {
		for (const [toolName, tool] of Object.entries(toolKit.tools)) {
			toolMap.set(`${toolKit.name}:${toolName}`, tool);
		}
	}

	client.setSidecarRequestHandler(async (request: SidecarRequestFrame) =>
		handleToolInvocation(request, toolMap),
	);

	let promptMarkdown = "";
	for (const toolKit of toolKits) {
		const registered = await client.registerToolkit(session, vm, {
			name: toolKit.name,
			description: toolKit.description,
			tools: Object.fromEntries(
				Object.entries(toolKit.tools).map(([toolName, tool]) => [
					toolName,
					toolToSidecarDefinition(tool),
				]),
			),
		});
		promptMarkdown = registered.promptMarkdown;
	}

	return promptMarkdown;
}

async function handleToolInvocation(
	request: SidecarRequestFrame,
	toolMap: ReadonlyMap<string, HostTool>,
): Promise<SidecarResponsePayload> {
	if (request.payload.type !== "tool_invocation") {
		return {
			type: "tool_invocation_result",
			invocation_id: "unknown",
			error: `unsupported sidecar request type: ${request.payload.type}`,
		};
	}
	const payload = request.payload;

	const tool = toolMap.get(payload.tool_key);
	if (!tool) {
		return {
			type: "tool_invocation_result",
			invocation_id: payload.invocation_id,
			error: `Unknown tool "${payload.tool_key}"`,
		};
	}

	const parsed = tool.inputSchema.safeParse(payload.input);
	if (!parsed.success) {
		return {
			type: "tool_invocation_result",
			invocation_id: payload.invocation_id,
			error: validationMessage(parsed.error),
		};
	}

	try {
		const result = await Promise.race([
			Promise.resolve(tool.execute(parsed.data)),
				new Promise<never>((_, reject) =>
					setTimeout(
						() =>
							reject(
								new Error(
									`Tool "${payload.tool_key}" timed out after ${payload.timeout_ms}ms`,
								),
							),
						payload.timeout_ms,
					),
				),
			]);
			return {
				type: "tool_invocation_result",
				invocation_id: payload.invocation_id,
				result,
			};
		} catch (error) {
			return {
				type: "tool_invocation_result",
				invocation_id: payload.invocation_id,
				error: validationMessage(error),
			};
		}
}
