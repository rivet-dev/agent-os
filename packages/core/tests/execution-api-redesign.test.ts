import { afterAll, beforeAll, describe, expect, test } from "vitest";
import { AgentOs } from "../src/index.js";

describe("execution API redesign", () => {
	let vm: AgentOs;

	beforeAll(async () => {
		vm = await AgentOs.create({ defaultSoftware: false });
	}, 30_000);

	afterAll(async () => {
		await vm.dispose();
	});

	test("uses explicit language-pinned contexts shared by JavaScript and TypeScript", async () => {
		await vm.createContext("analysis");
		await expect(vm.createContext("analysis")).rejects.toMatchObject({
			detail: { code: "context_conflict" },
		});
		await expect(
			vm.javascript.execute("globalThis.missing = true", {
				contextId: "missing",
			}),
		).rejects.toMatchObject({ detail: { code: "context_not_found" } });

		await vm.typescript.execute("globalThis.answer = 40 as number", {
			contextId: "analysis",
		});
		const result = await vm.javascript.evaluate<number>(
			"globalThis.answer + 2",
			{ contextId: "analysis" },
		);
		expect(result).toMatchObject({ outcome: "succeeded", value: 42 });
		await expect(
			vm.python.evaluate("42", { contextId: "analysis" }),
		).rejects.toMatchObject({
			detail: { code: "context_language_mismatch" },
		});

		expect(await vm.contexts.get("analysis")).toMatchObject({
			contextId: "analysis",
			language: "javascript",
		});
		expect(await vm.contexts.list()).toEqual([
			expect.objectContaining({ contextId: "analysis" }),
		]);
		await vm.contexts.reset("analysis");
		await vm.contexts.delete("analysis");
		await expect(vm.contexts.get("analysis")).rejects.toMatchObject({
			detail: { code: "context_not_found" },
		});
	}, 30_000);

	test("runs every Secure Exec resident-runner source unchanged", async () => {
		const contextId = "secure-exec-resident-cases";
		await vm.createContext(contextId);
		try {
			const cases = [
				["1 + 1", ""],
				["globalThis.x = 1", ""],
				['console.log("A")', "A\n"],
				['process.stdout.write("B")', "B"],
				["export const y = 1;", ""],
			] as const;

			for (const [source, expectedStdout] of cases) {
				const result = await vm.javascript.execute(source, {
					contextId,
					timeoutMs: 10_000,
					output: { capture: "all" },
				});
				expect(result, source).toMatchObject({
					outcome: "succeeded",
					exitCode: 0,
					stdout: expectedStdout,
				});
			}

			expect(
				await vm.javascript.evaluate("globalThis.x", { contextId }),
			).toMatchObject({ outcome: "succeeded", value: 1 });
		} finally {
			await vm.contexts.delete(contextId);
		}
	}, 30_000);

	test("returns a PID for spawned language work and controls it through process", async () => {
		const process = await vm.javascript.spawn("console.log('spawned')", {
			output: { retainEvents: true },
		});
		expect(process.pid).toBeGreaterThan(0);
		expect(await vm.process.get(process.pid)).toMatchObject({
			pid: process.pid,
			language: "javascript",
		});

		const exit = await vm.process.wait(process.pid);
		expect(exit).toMatchObject({
			pid: process.pid,
			outcome: "exited",
			exitCode: 0,
		});
		const output = await vm.process.readOutput(process.pid);
		expect(
			output.events.some((event) =>
				new TextDecoder().decode(event.chunk).includes("spawned"),
			),
		).toBe(true);
	}, 30_000);
});
