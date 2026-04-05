import { anthropic } from "@ai-sdk/anthropic";
import { createTypeScriptTools } from "@secure-exec/typescript";
import { generateText, stepCountIs, tool } from "ai";
import {
	allowAll,
	createNodeDriver,
	createNodeRuntimeDriverFactory,
	NodeRuntime,
} from "secure-exec";
import { z } from "zod";

const systemDriver = createNodeDriver({
	moduleAccess: {
		cwd: process.cwd(),
	},
	permissions: allowAll,
});
const runtimeDriverFactory = createNodeRuntimeDriverFactory();

const runtime = new NodeRuntime({
	systemDriver,
	runtimeDriverFactory,
	memoryLimit: 64,
	cpuTimeLimitMs: 5000,
});
const ts = createTypeScriptTools({
	systemDriver,
	runtimeDriverFactory,
	memoryLimit: 256,
	cpuTimeLimitMs: 5000,
});

try {
	const { text } = await generateText({
		model: anthropic("claude-sonnet-4-6"),
		prompt:
			"Write TypeScript that calculates the first 20 fibonacci numbers. Assign the result to module.exports.",
		stopWhen: stepCountIs(5),
		tools: {
			execute_typescript: tool({
				description:
					"Type-check TypeScript in a sandbox, compile it, then run the emitted JavaScript in a sandbox. Return diagnostics when validation fails.",
				inputSchema: z.object({ code: z.string() }),
				execute: async ({ code }) => {
					const typecheck = await ts.typecheckSource({
						sourceText: code,
						filePath: "/root/generated.ts",
						compilerOptions: {
							module: "commonjs",
							target: "es2022",
						},
					});

					if (!typecheck.success) {
						return {
							ok: false,
							stage: "typecheck",
							diagnostics: typecheck.diagnostics,
						};
					}

					const compiled = await ts.compileSource({
						sourceText: code,
						filePath: "/root/generated.ts",
						compilerOptions: {
							module: "commonjs",
							target: "es2022",
						},
					});

					if (!compiled.success || !compiled.outputText) {
						return {
							ok: false,
							stage: "compile",
							diagnostics: compiled.diagnostics,
						};
					}

					const execution = await runtime.run<Record<string, unknown>>(
						compiled.outputText,
						"/root/generated.js",
					);

					if (execution.code !== 0) {
						return {
							ok: false,
							stage: "run",
							errorMessage:
								execution.errorMessage ??
								`Sandbox exited with code ${execution.code}`,
						};
					}

					return {
						ok: true,
						stage: "run",
						exports: execution.exports,
					};
				},
			}),
		},
	});

	console.log(text);
} finally {
	runtime.dispose();
}
