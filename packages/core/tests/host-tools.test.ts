import { describe, expect, test } from "vitest";
import { z } from "zod";
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
});
