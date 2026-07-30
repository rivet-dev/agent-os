import { expect, test } from "@playwright/test";

// R2 gate: process stdio is bound to a kernel PTY slave. The host receives the
// master fd, reads the guest's stdout from the master, writes terminal input to
// the master, and the guest receives it on process.stdin through the real line
// discipline.

test("guest stdio binds to a kernel PTY slave and host drives the master", async ({
	page,
}) => {
	await page.goto("/pty-stdio.html");
	await expect(page.locator("#status")).toHaveText("ready", { timeout: 20_000 });

	const result = await page.evaluate(() => window.__ptyStdio!.run());

	if (result.error) throw new Error(result.error);
	expect(result.exitCode).toBe(0);
	expect(result.masterFd).toBeGreaterThan(0);
	expect(result.slaveFd).toBeGreaterThan(0);
	expect(result.ready).toEqual({
		stdinTTY: true,
		stdoutTTY: true,
		stderrTTY: true,
		columns: 100,
		rows: 31,
	});
	expect(result.stdoutResized).toEqual({
		count: 1,
		columns: 100,
		rows: 31,
	});
	expect(result.resized).toEqual({
		count: 1,
		columns: 132,
		rows: 43,
	});
	expect(result.output).toContain("GOT:terminal-input");
});

declare global {
	interface Window {
		__ptyStdio?: {
			run(): Promise<{
				exitCode: number;
				masterFd?: number;
				slaveFd?: number;
				ready?: {
					stdinTTY?: boolean;
					stdoutTTY?: boolean;
					stderrTTY?: boolean;
				columns?: number;
				rows?: number;
			};
			resized?: {
				count?: number;
				columns?: number;
				rows?: number;
			};
			stdoutResized?: {
				count?: number;
				columns?: number;
				rows?: number;
			};
			output: string;
			error?: string;
		}>;
		};
	}
}
