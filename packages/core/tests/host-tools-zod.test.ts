import { describe, expect, test } from "vitest";
import { z } from "zod";
import { zodToJsonSchema } from "../src/host-tools-zod.js";

describe("zodToJsonSchema", () => {
	test("converts objects with required, optional, enum, and descriptions", () => {
		const schema = z.object({
			url: z.string().describe("Target URL"),
			fullPage: z.boolean().optional(),
			format: z.enum(["png", "jpg"]).describe("Image format"),
			width: z.number().optional(),
		});

		expect(zodToJsonSchema(schema)).toEqual({
			type: "object",
			properties: {
				url: { type: "string", description: "Target URL" },
				fullPage: { type: "boolean" },
				format: {
					type: "string",
					enum: ["png", "jpg"],
					description: "Image format",
				},
				width: { type: "number" },
			},
			required: ["url", "format"],
		});
	});

	test("converts nested objects and arrays recursively", () => {
		const schema = z.object({
			tags: z.array(z.string()),
			options: z.object({
				retries: z.number().optional(),
				headers: z.array(z.string()).optional(),
			}),
		});

		expect(zodToJsonSchema(schema)).toEqual({
			type: "object",
			properties: {
				tags: {
					type: "array",
					items: { type: "string" },
				},
				options: {
					type: "object",
					properties: {
						retries: { type: "number" },
						headers: {
							type: "array",
							items: { type: "string" },
						},
					},
				},
			},
			required: ["tags", "options"],
		});
	});
});
