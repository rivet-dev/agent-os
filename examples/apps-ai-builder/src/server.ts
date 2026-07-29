import { readFile } from "node:fs/promises";
import { anthropic } from "@ai-sdk/anthropic";
import { serve } from "@hono/node-server";
import {
	AgentOSAppsError,
	appsRouter,
	deployApp,
} from "@rivet-dev/agentos-apps";
import { generateText } from "ai";
import { Hono } from "hono";
import { registry } from "./actors.js";

const editablePaths = [
	"package.json",
	"tsconfig.json",
	"src/index.ts",
] as const;
const maxRepairs = 3;
const maxFileBytes = 64 * 1024;

registry.start();

async function loadSeed(): Promise<Record<string, string>> {
	const files: Record<string, string> = {};
	for (const path of editablePaths) {
		files[path] = await readFile(
			new URL(`../fixtures/app/${path}`, import.meta.url),
			"utf8",
		);
	}
	return files;
}

function parseFiles(text: string): Record<string, string> {
	const json = text.match(/```json\s*([\s\S]*?)```/)?.[1] ?? text;
	const value = JSON.parse(json) as { files?: Record<string, unknown> };
	if (!value.files || typeof value.files !== "object") {
		throw new TypeError("model response must contain a files object");
	}
	const files: Record<string, string> = {};
	for (const path of editablePaths) {
		const content = value.files[path];
		if (typeof content !== "string") {
			throw new TypeError(`model response is missing ${path}`);
		}
		if (Buffer.byteLength(content) > maxFileBytes) {
			throw new RangeError(`${path} exceeds ${maxFileBytes} bytes`);
		}
		files[path] = content;
	}
	return files;
}

async function revise(
	prompt: string,
	files: Record<string, string>,
	diagnostics?: string,
): Promise<Record<string, string>> {
	const result = await generateText({
		model: anthropic(process.env.AI_MODEL ?? "claude-sonnet-4-5"),
		maxOutputTokens: 8_000,
		prompt: [
			'Return JSON only as {"files":{"path":"content"}}.',
			`You may edit only: ${editablePaths.join(", ")}.`,
			"The app must compile, start a RivetKit registry, and export a Web fetch handler.",
			`User request: ${prompt}`,
			diagnostics ? `Previous build diagnostics:\n${diagnostics}` : "",
			`Current files:\n${JSON.stringify(files)}`,
		]
			.filter(Boolean)
			.join("\n\n"),
	});
	return parseFiles(result.text);
}

async function generateApp(appId: string, prompt: string) {
	let files = await revise(prompt, await loadSeed());
	for (let attempt = 0; attempt <= maxRepairs; attempt += 1) {
		try {
			return await deployApp({
				appId,
				files,
			});
		} catch (error) {
			const appsError =
				error instanceof AgentOSAppsError ||
				(typeof error === "object" &&
					error !== null &&
					"code" in error &&
					typeof error.code === "string" &&
					error.code.startsWith("agentos_apps_"));
			if (!appsError || attempt === maxRepairs) {
				throw error;
			}
			const details = error as {
				code: string;
				message?: string;
				metadata?: unknown;
			};
			const diagnostics = JSON.stringify({
				code: details.code,
				message: details.message ?? String(error),
				metadata: details.metadata,
			}).slice(0, 16 * 1024);
			files = await revise(prompt, files, diagnostics);
		}
	}
	throw new Error("unreachable");
}

const server = new Hono();
// An agent or any other part of the system can call this route. A generic
// deployment endpoint could accept multipart files; this example generates the
// files from a prompt instead.
server.post("/deploy/:name", async (context) => {
	const body = await context.req.json<{ prompt?: unknown }>();
	if (typeof body.prompt !== "string" || body.prompt.length > 4_000) {
		return context.json(
			{ error: "prompt must be at most 4,000 characters" },
			400,
		);
	}
	return context.json(
		await generateApp(context.req.param("name"), body.prompt),
	);
});
server.route("/apps", appsRouter);

serve({
	fetch: server.fetch,
	port: 3000,
});
