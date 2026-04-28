import { describe, expect, test } from "vitest";
import { z } from "zod";
import {
	HostToolSchemaConversionError,
	zodToJsonSchema,
} from "../src/host-tools-zod.js";
import {
	MAX_TOOL_DESCRIPTION_LENGTH,
	hostTool,
	toolKit,
	validateToolkits,
} from "../src/index.js";

describe("host tool description limits", () => {
	test("accepts toolkit and tool descriptions at the exported limit", () => {
		const description = "a".repeat(MAX_TOOL_DESCRIPTION_LENGTH);

		expect(() =>
			validateToolkits([
				toolKit({
					name: "browser",
					description,
					tools: {
						screenshot: hostTool({
							description,
							inputSchema: z.object({ url: z.string() }),
							execute: () => ({ ok: true }),
						}),
					},
				}),
			]),
		).not.toThrow();
	});

	test("rejects toolkit descriptions longer than the exported limit", () => {
		expect(() =>
			validateToolkits([
				toolKit({
					name: "browser",
					description: "a".repeat(MAX_TOOL_DESCRIPTION_LENGTH + 1),
					tools: {
						screenshot: hostTool({
							description: "Take a screenshot",
							inputSchema: z.object({ url: z.string() }),
							execute: () => ({ ok: true }),
						}),
					},
				}),
			]),
		).toThrow(
			`Toolkit "browser" description is ${MAX_TOOL_DESCRIPTION_LENGTH + 1} characters, max is ${MAX_TOOL_DESCRIPTION_LENGTH}`,
		);
	});

	test("rejects tool descriptions longer than the exported limit", () => {
		expect(() =>
			validateToolkits([
				toolKit({
					name: "browser",
					description: "Browser automation",
					tools: {
						screenshot: hostTool({
							description: "a".repeat(MAX_TOOL_DESCRIPTION_LENGTH + 1),
							inputSchema: z.object({ url: z.string() }),
							execute: () => ({ ok: true }),
						}),
					},
				}),
			]),
		).toThrow(
			`Tool "browser/screenshot" description is ${MAX_TOOL_DESCRIPTION_LENGTH + 1} characters, max is ${MAX_TOOL_DESCRIPTION_LENGTH}`,
		);
	});

	test("fails loudly when a host tool input schema uses an unsupported discriminated union", () => {
		const tool = hostTool({
			description: "Inspect a variant payload",
			inputSchema: z.object({
				payload: z.discriminatedUnion("kind", [
					z.object({ kind: z.literal("text"), value: z.string() }),
					z.object({ kind: z.literal("code"), status: z.number() }),
				]),
			}),
			execute: () => ({ ok: true }),
		});

		try {
			zodToJsonSchema(tool.inputSchema);
			throw new Error("Expected unsupported host tool schema to fail");
		} catch (error) {
			expect(error).toBeInstanceOf(HostToolSchemaConversionError);
			expect(error).toMatchObject({
				path: "$.payload",
				zodType: "discriminatedUnion",
			});
		}
	});
});
